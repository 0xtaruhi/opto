// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::{CandidateDisposition, PostmapCandidate};
use super::power::MmmcPower;
use crate::closure::mapped_timing::MappedTimingTransaction;
use crate::closure::mmmc::{
    MmmcMetrics, MmmcTiming, aggregate_data_arrivals, aggregate_timing_owners,
};
use crate::closure::objective::{
    PhysicalObjective, compare_closure, mapped_physical_objective, physical_objective_after_edit,
};
use crate::{
    ImplementationDb, OptimizationPhase, SynthesisOptions, SynthesisProgress,
    api::types::SynthesisPolicy,
};
use opto_ir::mapped::MappedNetlist;
use opto_runtime::ExecutionContext;
use opto_timing::{DesignRuleSummary, DesignRuleViolation, ScenarioSet, TimingQualitySummary};
use std::sync::Arc;

pub(super) struct TimingOptimizationRequest<'a> {
    pub(super) mapped: &'a mut MappedNetlist,
    pub(super) implementations: &'a mut ImplementationDb,
    pub(super) timing: MmmcTiming,
    pub(super) options: &'a SynthesisOptions,
    pub(super) scenarios: &'a ScenarioSet,
    pub(super) runtime: &'a ExecutionContext,
    pub(super) power_evaluator: Arc<dyn crate::SynthesisPowerEvaluator>,
    pub(super) connectivity: &'a crate::mapping::materialize::FrozenObservableConnectivity,
    pub(super) diagnostics: crate::SynthesisDiagnostics,
    pub(super) observer: &'a mut dyn FnMut(SynthesisProgress),
}

pub(super) struct TimingOptimizationSession<'a> {
    pub(super) mapped: &'a mut MappedNetlist,
    pub(super) implementations: &'a mut ImplementationDb,
    pub(super) timing: MmmcTiming,
    pub(super) options: &'a SynthesisOptions,
    connectivity: &'a crate::mapping::materialize::FrozenObservableConnectivity,
    state: TimingOptimizationState,
    observer: &'a mut dyn FnMut(SynthesisProgress),
}

impl<'a> TimingOptimizationSession<'a> {
    pub(super) fn start(request: TimingOptimizationRequest<'a>) -> Result<Self, crate::SynthError> {
        let TimingOptimizationRequest {
            mapped,
            implementations,
            timing,
            options,
            scenarios,
            runtime,
            power_evaluator,
            connectivity,
            diagnostics,
            observer,
        } = request;
        let mut timing = timing;
        let initial_metrics = timing.metrics()?;
        let power = MmmcPower::new(&timing, scenarios, runtime, power_evaluator)?;
        let mut physical = mapped_physical_objective(mapped, &options.target_cells, scenarios)?;
        physical.dynamic = power.committed().dynamic_watts();
        let qor_budget = default_evaluation_budget(mapped.cell_count());
        Ok(Self {
            mapped,
            implementations,
            timing,
            options,
            connectivity,
            state: TimingOptimizationState {
                analysis: initial_metrics.analysis,
                design_rule_summary: initial_metrics.design_rule_summary,
                design_rules: initial_metrics.design_rules,
                physical,
                power,
                replacements: 0,
                evaluations: 0,
                qor_evaluations: 0,
                candidates: 0,
                qor_budget,
                qor_limit: qor_budget,
                rejected: 0,
                stale: 0,
                diagnostics,
                scenarios: scenarios.clone(),
            },
            observer,
        })
    }

    pub(super) fn qor_budget_exhausted(&self) -> bool {
        self.qor_remaining() == 0
    }

    pub(super) fn qor_remaining(&self) -> usize {
        self.state
            .qor_limit
            .saturating_sub(self.state.qor_evaluations)
    }

    /// Lends part of the remaining global budget to one search frontier.
    /// Unspent evaluations return to the enclosing search even on an error;
    /// nested frontiers cannot spend another phase's reserved evaluations.
    pub(super) fn with_qor_allowance<T>(
        &mut self,
        allowance: usize,
        search: impl FnOnce(&mut Self) -> Result<T, crate::SynthError>,
    ) -> Result<T, crate::SynthError> {
        let previous = self.state.qor_limit;
        self.state.qor_limit = previous.min(self.state.qor_evaluations.saturating_add(allowance));
        let result = search(self);
        self.state.qor_limit = previous;
        result
    }

    pub(super) fn timing_met(&self) -> bool {
        self.state.analysis.wns().is_none_or(|slack| slack >= 0.0)
    }

    pub(super) fn has_design_rule_violations(&self) -> bool {
        !self.state.design_rules.is_empty()
    }

    pub(super) fn design_rules(&self) -> &[DesignRuleViolation] {
        &self.state.design_rules
    }

    /// Returns this session's timing-diagnostics sink.
    pub(super) fn trace(&self) -> crate::api::diagnostics::SynthTrace {
        crate::api::diagnostics::SynthTrace::timing(self.state.diagnostics)
    }

    pub(super) fn scenarios(&self) -> &ScenarioSet {
        &self.state.scenarios
    }

    pub(super) fn analysis(&self) -> &TimingQualitySummary {
        &self.state.analysis
    }

    pub(super) fn changed(&self) -> bool {
        self.state.replacements != 0
    }

    #[cfg(test)]
    pub(super) fn replacements(&self) -> usize {
        self.state.replacements
    }

    pub(super) fn report_completion(&self, elapsed: std::time::Duration) {
        crate::api::diagnostics::trace!(
            self.trace(),
            "postmap.timing.finish",
            "wall={elapsed:?} sta_evaluations={} qor_evaluations={}/{} candidates={} \
             accepted={} rejected={} stale={} wns={:?} tns={:.6}",
            self.state.evaluations,
            self.state.qor_evaluations,
            self.state.qor_budget,
            self.state.candidates,
            self.state.replacements,
            self.state.rejected,
            self.state.stale,
            self.state.analysis.wns(),
            self.state.analysis.tns(),
        );
    }

    /// Seals the run and hands the shared incremental timing owner back to the
    /// synthesis pipeline for final reporting.
    pub(super) fn finish(self) -> super::PostmapOutcome {
        let changed = self.changed();
        #[cfg(test)]
        let replacements = self.replacements();
        super::PostmapOutcome {
            timing: Some(self.timing),
            changed,
            #[cfg(test)]
            replacements,
        }
    }

    pub(super) fn evaluate_topology(
        &mut self,
        candidate: PostmapCandidate,
        phase: OptimizationPhase,
    ) -> Result<CandidateDisposition<()>, crate::SynthError> {
        self.evaluate(candidate, phase, EvaluationClass::Topology)
    }

    pub(super) fn evaluate_qor(
        &mut self,
        candidate: PostmapCandidate,
        phase: OptimizationPhase,
    ) -> Result<CandidateDisposition<()>, crate::SynthError> {
        self.evaluate(candidate, phase, EvaluationClass::Qor)
    }

    fn evaluate(
        &mut self,
        candidate: PostmapCandidate,
        phase: OptimizationPhase,
        class: EvaluationClass,
    ) -> Result<CandidateDisposition<()>, crate::SynthError> {
        if class == EvaluationClass::Qor {
            if self.qor_budget_exhausted() {
                return Err(crate::SynthError::invariant(
                    "QoR candidate evaluation exceeded its deterministic budget",
                ));
            }
            self.state.qor_evaluations =
                self.state.qor_evaluations.checked_add(1).ok_or_else(|| {
                    crate::SynthError::invariant("QoR STA evaluation count overflow")
                })?;
        }
        self.state.evaluations =
            self.state.evaluations.checked_add(1).ok_or_else(|| {
                crate::SynthError::invariant("timing STA evaluation count overflow")
            })?;
        self.state.candidates = self
            .state
            .candidates
            .checked_add(1)
            .ok_or_else(|| crate::SynthError::invariant("timing candidate count overflow"))?;
        let disposition = evaluate_candidate(
            CandidateEvaluation {
                mapped: self.mapped,
                implementations: self.implementations,
                timing: Some(&mut self.timing),
                power: Some(&mut self.state.power),
                library: &self.options.target_cells,
                scenarios: &self.state.scenarios,
                physical: self.state.physical,
                closure: Some(ClosureBaseline {
                    analysis: &self.state.analysis,
                    design_rule_summary: self.state.design_rule_summary,
                }),
                operation: "post-map timing transaction",
                connectivity: self.connectivity,
            },
            candidate,
        )?;
        let accepted = match disposition {
            CandidateDisposition::Accepted(accepted) => accepted,
            CandidateDisposition::Rejected => {
                self.state.rejected += 1;
                self.trace_progress(phase);
                return Ok(CandidateDisposition::Rejected);
            }
            CandidateDisposition::Stale => {
                self.state.stale += 1;
                self.trace_progress(phase);
                return Ok(CandidateDisposition::Stale);
            }
        };

        let metrics = accepted.timing.ok_or_else(|| {
            crate::SynthError::invariant("accepted timing candidate reported no timing closure")
        })?;
        self.state.analysis = metrics.analysis;
        self.state.design_rule_summary = metrics.design_rule_summary;
        self.state.design_rules = metrics.design_rules;
        self.state.physical = accepted.physical;
        self.state.replacements =
            self.state.replacements.checked_add(1).ok_or_else(|| {
                crate::SynthError::invariant("post-map replacement count overflow")
            })?;
        self.timing.compact_every_view()?;
        self.trace_progress(phase);
        Ok(CandidateDisposition::Accepted(()))
    }

    pub(super) fn publish_progress(&mut self, phase: OptimizationPhase) {
        (self.observer)(SynthesisProgress::timing_candidate(
            phase,
            self.state.physical.area,
            self.mapped.cell_count(),
            &self.state.analysis,
            self.state.evaluations,
        ));
    }

    fn trace_progress(&self, phase: OptimizationPhase) {
        crate::api::diagnostics::trace!(
            self.trace().and(self.state.evaluations.is_power_of_two()),
            "postmap.timing.progress",
            "stage={} sta_evaluations={} candidates={} accepted={} rejected={} stale={} \
             wns={:?} tns={:.6}",
            phase.stage().as_str(),
            self.state.evaluations,
            self.state.candidates,
            self.state.replacements,
            self.state.rejected,
            self.state.stale,
            self.state.analysis.wns(),
            self.state.analysis.tns(),
        );
    }
}

pub(super) struct TimingOptimizationPolicy {
    repeated_passes: bool,
}

impl TimingOptimizationPolicy {
    pub(super) fn new(policy: SynthesisPolicy) -> Self {
        Self {
            repeated_passes: policy.repeated_timing_passes,
        }
    }

    pub(super) fn allows_pass(&self, completed_passes: usize) -> bool {
        self.repeated_passes || completed_passes == 0
    }
}

struct TimingOptimizationState {
    analysis: TimingQualitySummary,
    design_rule_summary: DesignRuleSummary,
    design_rules: Vec<DesignRuleViolation>,
    physical: PhysicalObjective,
    power: MmmcPower,
    replacements: usize,
    evaluations: usize,
    qor_evaluations: usize,
    candidates: usize,
    qor_budget: usize,
    qor_limit: usize,
    rejected: usize,
    stale: usize,
    diagnostics: crate::SynthesisDiagnostics,
    scenarios: ScenarioSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvaluationClass {
    Topology,
    Qor,
}

/// The timing state a candidate must improve on to be accepted.
///
/// Present when constraints are measurable. Its absence selects the purely
/// physical acceptance test and skips re-aggregating timing altogether.
pub(super) struct ClosureBaseline<'a> {
    pub(super) analysis: &'a TimingQualitySummary,
    pub(super) design_rule_summary: DesignRuleSummary,
}

/// Everything one candidate is evaluated against.
pub(super) struct CandidateEvaluation<'a> {
    pub(super) mapped: &'a mut MappedNetlist,
    pub(super) implementations: &'a mut ImplementationDb,
    /// Incremental timing owners. Absent when no scenario has timing arcs.
    pub(super) timing: Option<&'a mut MmmcTiming>,
    pub(super) power: Option<&'a mut MmmcPower>,
    pub(super) library: &'a opto_library::TargetCellSet,
    pub(super) scenarios: &'a ScenarioSet,
    pub(super) physical: PhysicalObjective,
    pub(super) closure: Option<ClosureBaseline<'a>>,
    /// Stable operation name used when a rollback also fails.
    pub(super) operation: &'static str,
    pub(super) connectivity: &'a crate::mapping::materialize::FrozenObservableConnectivity,
}

/// The measurements a committed candidate produced.
pub(super) struct AcceptedCandidate {
    /// Re-aggregated timing. Present exactly when a closure baseline was given.
    pub(super) timing: Option<MmmcMetrics>,
    pub(super) physical: PhysicalObjective,
    pub(super) affected_cells: Vec<opto_ir::mapped::CellId>,
    pub(super) affected_nets: Vec<opto_ir::mapped::NetId>,
}

/// Runs the post-map candidate transaction: begin, re-time, re-power,
/// re-measure, then commit or roll back on the configured acceptance test.
///
/// This is the only place that mutates a mapped netlist speculatively. Every
/// pass uses the same transaction and acceptance path; available constraints
/// add closure measurements to the objective instead of selecting another
/// optimizer.
pub(super) fn evaluate_candidate(
    request: CandidateEvaluation<'_>,
    candidate: PostmapCandidate,
) -> Result<CandidateDisposition<AcceptedCandidate>, crate::SynthError> {
    let CandidateEvaluation {
        mapped,
        implementations,
        timing,
        power,
        library,
        scenarios,
        physical: baseline,
        closure,
        operation,
        connectivity,
    } = request;
    let PostmapCandidate {
        delta,
        implementation,
        guard: _,
    } = candidate;
    let mut no_owners = [];
    let (owners, policies) = match timing {
        Some(timing) => {
            let (owners, views) = timing.owners_and_views();
            (owners, Some(views))
        }
        None => (no_owners.as_mut_slice(), None),
    };
    let baseline_arrivals = policies.map(|views| aggregate_data_arrivals(owners, views));
    let Some(mut transaction) = MappedTimingTransaction::begin_optimization(mapped, owners, delta)?
    else {
        return Ok(CandidateDisposition::Stale);
    };
    let preserves_connectivity = match connectivity.preserves_affected(
        transaction.mapped(),
        library,
        transaction.mapped_edit().affected_nets(),
    ) {
        Ok(preserves) => preserves,
        Err(error) => return transaction.abort(error, operation),
    };
    if !preserves_connectivity {
        transaction.rollback()?;
        return Ok(CandidateDisposition::Rejected);
    }
    let timing_metrics = match &closure {
        Some(_) => {
            let Some(views) = policies else {
                return transaction.abort(
                    crate::SynthError::invariant(
                        "timing-closure candidate evaluation has no timing model",
                    ),
                    operation,
                );
            };
            match aggregate_timing_owners(transaction.timing_mut(), views) {
                Ok(metrics) => Some(metrics),
                Err(error) => return transaction.abort(error, operation),
            }
        }
        None => None,
    };
    let power_proposal = match power.as_ref() {
        Some(power) => match power.evaluate(transaction.timing_mut()) {
            Ok(proposal) => proposal,
            Err(error) => return transaction.abort(error, operation),
        },
        None => super::power::PowerProposal::unmeasured(),
    };
    let mut physical = match physical_objective_after_edit(
        transaction.mapped(),
        transaction.mapped_edit(),
        baseline,
        library,
        scenarios,
    ) {
        Ok(physical) => physical,
        Err(error) => return transaction.abort(error, operation),
    };
    physical.dynamic = power_proposal.dynamic_watts();
    let primary_order = match (&closure, &timing_metrics) {
        (Some(closure), Some(metrics)) => compare_closure(
            &metrics.analysis,
            metrics.design_rule_summary,
            physical,
            closure.analysis,
            closure.design_rule_summary,
            baseline,
        ),
        _ => crate::closure::objective::compare_physical(physical, baseline),
    };
    let accepted = primary_order
        .then_with(|| match (baseline_arrivals, policies) {
            (Some(before), Some(views)) => {
                let after = aggregate_data_arrivals(transaction.timing_mut(), views);
                after
                    .0
                    .total_cmp(&before.0)
                    .then_with(|| after.1.total_cmp(&before.1))
            }
            _ => std::cmp::Ordering::Equal,
        })
        .is_lt();
    if !accepted {
        transaction.rollback()?;
        return Ok(CandidateDisposition::Rejected);
    }
    let affected_cells = transaction
        .mapped_edit()
        .affected_cells()
        .filter(|&cell| transaction.mapped().is_live_cell(cell))
        .collect::<Vec<_>>();
    let affected_nets = transaction
        .mapped_edit()
        .affected_nets()
        .collect::<Vec<_>>();
    transaction.commit_with(operation, |mapped, edit| {
        let prepared = implementations.prepare_region_edit(mapped, edit, &implementation)?;
        implementations.commit_region_edit(prepared)
    })?;
    if let Some(power) = power {
        power.commit(power_proposal);
    }
    Ok(CandidateDisposition::Accepted(AcceptedCandidate {
        timing: timing_metrics,
        physical,
        affected_cells,
        affected_nets,
    }))
}

pub(super) fn default_evaluation_budget(cells: usize) -> usize {
    const MINIMUM_PIPELINE_EVALUATIONS: usize = 8;
    ceiling_sqrt(cells).max(MINIMUM_PIPELINE_EVALUATIONS)
}

fn ceiling_sqrt(value: usize) -> usize {
    let floor = value.isqrt();
    floor + usize::from(floor * floor < value)
}

#[cfg(test)]
mod tests {
    use super::default_evaluation_budget;

    #[test]
    fn evaluation_budget_is_a_fixed_sublinear_bound() {
        assert_eq!(default_evaluation_budget(10_000), 100);
        assert_eq!(default_evaluation_budget(9), 8);
    }
}
