// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::{CandidateDisposition, PostmapCandidate};
use super::candidates::sizing_regions;
use super::forest::{self, EvaluationPolicy, ForestSession};
use super::session::{AcceptedCandidate, CandidateEvaluation, ClosureBaseline, evaluate_candidate};
use super::sizing::sizing_forest_delta;
use super::{PostmapOutcome, PostmapRequest};
use crate::closure::objective::{PhysicalObjective, mapped_physical_objective};
use crate::{ImplementationDb, OptimizationPhase, SynthesisProgress};
use opto_ir::mapped::MappedNetlist;

const RESYNTHESIS_PLAN_TASK_DOMAIN: u32 = 0x4d46_5352;

pub(super) fn optimize(
    request: PostmapRequest<'_>,
    config: crate::SynthesisConfig,
    observer: &mut dyn FnMut(SynthesisProgress),
) -> Result<PostmapOutcome, crate::SynthError> {
    let diagnostics = config.diagnostics;
    let PostmapRequest {
        mapped,
        implementations,
        options,
        catalog,
        runtime,
        policy,
        scenarios,
        timing,
        fanout_load_profile: _,
        power_evaluator,
        connectivity,
    } = request;
    let mut timing = timing;
    let closure = timing
        .as_mut()
        .map(super::MmmcTiming::metrics)
        .transpose()?;
    let power = timing
        .as_ref()
        .map(|timing| super::MmmcPower::new(timing, scenarios, runtime, power_evaluator.clone()))
        .transpose()?;
    let mut physical = mapped_physical_objective(mapped, &options.target_cells, scenarios)?;
    physical.dynamic = power
        .as_ref()
        .and_then(|power| power.committed().dynamic_watts());
    let trace = crate::api::diagnostics::SynthTrace::timing(diagnostics);
    let mut session = AreaOptimizationSession {
        mapped,
        implementations,
        timing,
        closure,
        power,
        options,
        scenarios,
        physical,
        evaluations: 0,
        replacements: 0,
        observer,
        connectivity,
    };
    let phase_started = std::time::Instant::now();

    let optimization_boundary =
        super::mfs::optimization_boundary_nets(session.mapped, session.implementations)?;
    remove_dead_cells(&mut session, catalog, &optimization_boundary)?;
    session.publish_progress(OptimizationPhase::RegisterOptimization);
    remove_constant_registers(&mut session, options, runtime, &optimization_boundary)?;
    session.publish_progress(OptimizationPhase::RegisterOptimization);

    crate::api::diagnostics::trace!(
        trace,
        "postmap.area.dedupe",
        "cells={} wall={:?}",
        session.mapped.cell_count(),
        phase_started.elapsed()
    );
    if policy.resynthesis {
        resynthesize(
            &mut session,
            catalog,
            runtime,
            &optimization_boundary,
            diagnostics.mfs,
        )?;
        session.publish_progress(OptimizationPhase::BooleanResynthesis);
    }
    let phase_started = std::time::Instant::now();
    let generations = 1 + usize::from(policy.repeated_timing_passes);
    for _ in 0..generations {
        let cells = session.mapped.cell_ids().collect::<Vec<_>>();
        let regions = sizing_regions(
            runtime,
            cells.into_iter().rev(),
            session.mapped,
            options,
            catalog,
            true,
            None,
        )?;
        let choices = regions
            .into_iter()
            .filter_map(|region| {
                region
                    .tradeoff_candidates
                    .first()
                    .copied()
                    .map(|candidate| (region.cell, candidate))
            })
            .collect::<Vec<_>>();
        if !forest::evaluate(
            &choices,
            OptimizationPhase::TradeoffSizing,
            EvaluationPolicy::Complete,
            &mut session,
            |mapped, _, options, choices| {
                sizing_forest_delta(mapped, &options.target_cells, choices).map(Some)
            },
        )? {
            break;
        }
    }

    crate::api::diagnostics::trace!(
        trace,
        "postmap.area.sizing",
        "cells={} wall={:?}",
        session.mapped.cell_count(),
        phase_started.elapsed()
    );
    Ok(session.finish())
}

fn resynthesize(
    session: &mut AreaOptimizationSession<'_>,
    catalog: &super::candidates::PostmapCellCatalog,
    runtime: &opto_runtime::ExecutionContext,
    optimization_boundary: &hashbrown::HashSet<opto_ir::mapped::NetId>,
    diagnostics: bool,
) -> Result<(), crate::SynthError> {
    let functions = catalog.mfs_functions();
    let resynthesis = catalog.mfs_resynthesis();
    let evaluation_budget = super::session::default_evaluation_budget(session.mapped.cell_count());
    let mut evaluations = 0usize;
    let mut drivers = super::mfs::DriverIndex::build(session.mapped, functions);
    loop {
        let eligible = session
            .mapped
            .cell_ids()
            .collect::<std::collections::HashSet<_>>();
        let tasks = super::region::scoped_work(
            session.mapped,
            &eligible,
            super::region::scheduling_cell_budget(eligible.len(), runtime.parallelism()),
        )?
        .into_iter()
        .map(|work| {
            opto_runtime::Task::new(
                opto_runtime::TaskKey::new(RESYNTHESIS_PLAN_TASK_DOMAIN, work.task_ordinal()),
                work,
            )
        })
        .collect::<Vec<_>>();
        let candidates = {
            let context = super::mfs::OptimizationContext {
                mapped: session.mapped,
                implementations: session.implementations,
                functions,
                resynthesis,
                drivers: &drivers,
                boundary: optimization_boundary,
                diagnostics,
            };
            runtime.map_ordered(tasks, |work| {
                Ok::<_, crate::SynthError>(
                    work.cells
                        .into_iter()
                        .map(|cell| super::mfs::optimization_candidate(context, cell))
                        .collect::<Vec<_>>(),
                )
            })?
        };
        let mut changed = false;
        for candidate in candidates.into_iter().flatten().flatten() {
            if evaluations >= evaluation_budget {
                break;
            }
            evaluations = increment_count(evaluations, "post-map resynthesis evaluation")?;
            if let CandidateDisposition::Accepted(edit) = session.evaluate(candidate)? {
                drivers.refresh(
                    session.mapped,
                    functions,
                    edit.affected_nets.iter().copied(),
                );
                changed = true;
            }
        }
        if !changed || evaluations >= evaluation_budget {
            return Ok(());
        }
    }
}

/// Removes unobserved cells to a fixpoint and records the affected frontier.
fn remove_dead_cells(
    session: &mut AreaOptimizationSession<'_>,
    catalog: &super::candidates::PostmapCellCatalog,
    optimization_boundary: &hashbrown::HashSet<opto_ir::mapped::NetId>,
) -> Result<(), crate::SynthError> {
    let functions = catalog.mfs_functions();
    loop {
        let Some(candidate) =
            super::mfs::dead_cell_removal(session.mapped, functions, optimization_boundary)?
        else {
            return Ok(());
        };
        let CandidateDisposition::Accepted(_) = session.evaluate(candidate)? else {
            return Ok(());
        };
    }
}

/// Removes proved-constant register batches to a scoped fixpoint.
fn remove_constant_registers(
    session: &mut AreaOptimizationSession<'_>,
    options: &crate::SynthesisOptions,
    runtime: &opto_runtime::ExecutionContext,
    optimization_boundary: &hashbrown::HashSet<opto_ir::mapped::NetId>,
) -> Result<(), crate::SynthError> {
    let mut scope = None;
    loop {
        let registers = super::registers::constant_register_candidates(
            session.mapped,
            &options.target_cells,
            optimization_boundary,
            scope.as_ref(),
            runtime,
        )?;
        let Some(candidate) =
            super::registers::constant_register_removal(session.mapped, &registers)?
        else {
            return Ok(());
        };
        let CandidateDisposition::Accepted(edit) = session.evaluate(candidate)? else {
            return Ok(());
        };
        let mut reached = std::collections::HashSet::new();
        extend_cleanup_frontier(session.mapped, &edit, &mut reached);
        scope = Some(reached);
    }
}

fn extend_cleanup_frontier(
    mapped: &MappedNetlist,
    edit: &AcceptedCandidate,
    dirty: &mut std::collections::HashSet<opto_ir::mapped::CellId>,
) {
    dirty.extend(edit.affected_cells.iter().copied());
    for &net in &edit.affected_nets {
        if let Some(pins) = mapped.pins_on_net(net) {
            dirty.extend(pins.filter_map(|pin| mapped.pin_owner(pin)));
        }
    }
}

/// Owns post-map state and the running closure objective.
pub(super) struct AreaOptimizationSession<'a> {
    mapped: &'a mut MappedNetlist,
    implementations: &'a mut ImplementationDb,
    timing: Option<super::MmmcTiming>,
    closure: Option<crate::closure::mmmc::MmmcMetrics>,
    power: Option<super::MmmcPower>,
    options: &'a crate::SynthesisOptions,
    scenarios: &'a opto_timing::ScenarioSet,
    physical: PhysicalObjective,
    evaluations: usize,
    replacements: usize,
    observer: &'a mut dyn FnMut(SynthesisProgress),
    connectivity: &'a crate::mapping::materialize::FrozenObservableConnectivity,
}

impl AreaOptimizationSession<'_> {
    /// Seals the run and returns the timing owner to the synthesis pipeline.
    fn finish(self) -> PostmapOutcome {
        PostmapOutcome {
            timing: self.timing,
            changed: self.replacements != 0,
            #[cfg(test)]
            replacements: self.replacements,
        }
    }

    /// Evaluates a candidate and returns the accepted edit frontier.
    fn evaluate(
        &mut self,
        candidate: PostmapCandidate,
    ) -> Result<CandidateDisposition<AcceptedCandidate>, crate::SynthError> {
        self.evaluations = increment_count(self.evaluations, "post-map cleanup evaluation")?;
        let mut disposition = evaluate_candidate(
            CandidateEvaluation {
                mapped: self.mapped,
                implementations: self.implementations,
                timing: self.timing.as_mut(),
                power: self.power.as_mut(),
                library: &self.options.target_cells,
                scenarios: self.scenarios,
                physical: self.physical,
                closure: self.closure.as_ref().map(|closure| ClosureBaseline {
                    analysis: &closure.analysis,
                    design_rule_summary: closure.design_rule_summary,
                }),
                operation: "post-map cleanup transaction",
                connectivity: self.connectivity,
            },
            candidate,
        )?;
        if let CandidateDisposition::Accepted(accepted) = &mut disposition {
            self.physical = accepted.physical;
            if let Some(timing) = accepted.timing.take() {
                self.closure = Some(timing);
            }
            self.replacements = increment_count(self.replacements, "post-map replacement")?;
        }
        Ok(disposition)
    }

    fn publish_progress(&mut self, phase: OptimizationPhase) {
        match &self.closure {
            Some(closure) => (self.observer)(SynthesisProgress::timing_candidate(
                phase,
                self.physical.area,
                self.physical.cells,
                &closure.analysis,
                self.evaluations,
            )),
            None => (self.observer)(SynthesisProgress::candidate(
                phase,
                self.physical.area,
                self.physical.cells,
            )),
        }
    }
}

impl ForestSession for AreaOptimizationSession<'_> {
    fn mapped(&self) -> &MappedNetlist {
        self.mapped
    }

    fn implementations(&self) -> &ImplementationDb {
        self.implementations
    }

    fn options(&self) -> &crate::SynthesisOptions {
        self.options
    }

    fn qor_budget_exhausted(&self) -> bool {
        false
    }

    fn evaluate_forest_candidate(
        &mut self,
        candidate: PostmapCandidate,
        _phase: OptimizationPhase,
        policy: EvaluationPolicy,
    ) -> Result<CandidateDisposition<()>, crate::SynthError> {
        if policy != EvaluationPolicy::Complete {
            return Err(crate::SynthError::invariant(
                "cleanup forest unexpectedly requested a QoR-budgeted evaluation",
            ));
        }
        self.evaluate(candidate)
            .map(|disposition| match disposition {
                CandidateDisposition::Accepted(_) => CandidateDisposition::Accepted(()),
                CandidateDisposition::Rejected => CandidateDisposition::Rejected,
                CandidateDisposition::Stale => CandidateDisposition::Stale,
            })
    }

    fn publish_forest_progress(&mut self, phase: OptimizationPhase) {
        self.publish_progress(phase);
    }
}

fn increment_count(count: usize, what: &str) -> Result<usize, crate::SynthError> {
    count
        .checked_add(1)
        .ok_or_else(|| crate::SynthError::invariant(format!("{what} count overflow")))
}
