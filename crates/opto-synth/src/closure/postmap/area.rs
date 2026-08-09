// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::{CandidateDisposition, PostmapCandidate, select_non_conflicting};
use super::candidates::sizing_regions;
use super::objective::{PhysicalObjective, mapped_physical_objective};
use super::session::{AcceptedCandidate, CandidateEvaluation, ClosureBaseline, evaluate_candidate};
use super::sizing::sizing_delta;
use super::{PostmapOutcome, PostmapRequest};
use crate::{ImplementationDb, OptimizationPhase, SynthesisProgress};
use opto_ir::mapped::MappedNetlist;

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
        library: &options.target_cells,
        scenarios,
        physical,
        replacements: 0,
        observer,
        connectivity,
    };
    let mut cleanup_dirty = std::collections::HashSet::new();
    let phase_started = std::time::Instant::now();

    let optimization_boundary = super::mfs::optimization_boundary_nets(session.mapped);
    let mut merged = true;
    while merged {
        merged = false;
        let candidates = super::registers::constant_register_candidates(
            session.mapped,
            &options.target_cells,
            &optimization_boundary,
        )?;
        for candidate in candidates {
            if let CandidateDisposition::Accepted(edit) =
                session.evaluate(candidate, OptimizationPhase::RegisterOptimization)?
            {
                extend_cleanup_frontier(session.mapped, &edit, &mut cleanup_dirty);
                merged = true;
            }
        }
    }

    crate::api::diagnostics::trace!(
        trace,
        "postmap.area.dedupe",
        "cells={} wall={:?}",
        session.mapped.cell_count(),
        phase_started.elapsed()
    );
    let phase_started = std::time::Instant::now();
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
    for sizing in regions {
        for candidate_index in sizing.tradeoff_candidates {
            let target = options.target_cells.get(candidate_index).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "sizing candidate references unknown library index {candidate_index}"
                ))
            })?;
            let candidate =
                sizing_delta(session.mapped, sizing.cell, target.name(), candidate_index)?;
            match session.evaluate(candidate, OptimizationPhase::TradeoffSizing)? {
                CandidateDisposition::Accepted(edit) => {
                    extend_cleanup_frontier(session.mapped, &edit, &mut cleanup_dirty);
                    break;
                }
                CandidateDisposition::Rejected => {}
                CandidateDisposition::Stale => break,
            }
        }
    }

    crate::api::diagnostics::trace!(
        trace,
        "postmap.area.sizing",
        "cells={} wall={:?}",
        session.mapped.cell_count(),
        phase_started.elapsed()
    );
    let phase_started = std::time::Instant::now();
    if !policy.area_resynthesis {
        return Ok(session.finish());
    }
    let functions = catalog.mfs_functions();
    let resynthesis = catalog.mfs_resynthesis(super::mfs::ResynthesisObjective::Area);
    let evaluation_budget = super::session::default_evaluation_budget(session.mapped.cell_count());
    let mut drivers = super::mfs::DriverIndex::build(session.mapped, functions);
    let mut evaluations = 0usize;
    let mut dirty = if cleanup_dirty.is_empty() {
        session.mapped.cell_ids().collect()
    } else {
        let mut dirty = std::collections::HashSet::new();
        super::mfs::extend_candidate_invalidation(
            session.mapped,
            functions,
            &drivers,
            &optimization_boundary,
            cleanup_dirty,
            &mut dirty,
        );
        dirty
    };
    // Candidate generation is read-only and parallel within a sweep. Selection
    // and commit are ordered afterward, so every task observes one mapped
    // generation and no accepted edit depends on worker completion order.
    loop {
        if evaluations >= evaluation_budget {
            break;
        }
        let work = super::region::scoped_work(
            session.mapped,
            &dirty,
            super::region::scheduling_cell_budget(dirty.len(), runtime.parallelism()),
        )?;
        let cell_count = work.iter().map(|work| work.cells.len()).sum::<usize>();
        if work.is_empty() {
            break;
        }
        let tasks = work
            .into_iter()
            .map(|work| {
                opto_runtime::Task::new(opto_runtime::TaskKey::new(6, work.task_ordinal()), work)
            })
            .collect::<Vec<_>>();
        let sweep_started = std::time::Instant::now();
        let candidates = {
            let mapped = &*session.mapped;
            let implementations = &*session.implementations;
            let drivers = &drivers;
            let optimization_boundary = &optimization_boundary;
            let context = super::mfs::OptimizationContext {
                mapped,
                implementations,
                functions,
                resynthesis,
                drivers,
                boundary: optimization_boundary,
                diagnostics: diagnostics.mfs,
            };
            runtime.map_ordered(tasks, move |work| {
                Ok::<_, crate::SynthError>(
                    work.cells
                        .into_iter()
                        .map(|cell| (cell, super::mfs::optimization_candidate(context, cell)))
                        .collect::<Vec<_>>(),
                )
            })?
        };
        crate::api::diagnostics::trace!(
            trace,
            "postmap.area.mfs_generate",
            "scoped_cells={cell_count} cells={} wall={:?} par={}",
            session.mapped.cell_count(),
            sweep_started.elapsed(),
            runtime.parallelism()
        );
        let apply_started = std::time::Instant::now();
        let mut touched = std::collections::HashSet::new();
        let mut next_dirty = std::collections::HashSet::new();
        let batch = select_non_conflicting(candidates.into_iter().flatten());
        next_dirty.extend(batch.deferred);
        for (cell, candidate) in batch.selected {
            if evaluations >= evaluation_budget {
                break;
            }
            evaluations = increment_count(evaluations, "post-map area evaluation")?;
            match session.evaluate(candidate, OptimizationPhase::BooleanResynthesis)? {
                CandidateDisposition::Accepted(edit) => {
                    drivers.refresh(
                        session.mapped,
                        functions,
                        edit.affected_nets.iter().copied(),
                    );
                    touched.extend(edit.affected_cells);
                    for net in edit.affected_nets {
                        if let Some(pins) = session.mapped.pins_on_net(net) {
                            touched.extend(pins.filter_map(|pin| session.mapped.pin_owner(pin)));
                        }
                    }
                }
                CandidateDisposition::Rejected => {}
                CandidateDisposition::Stale => {
                    next_dirty.insert(cell);
                }
            }
        }
        crate::api::diagnostics::trace!(
            trace,
            "postmap.area.mfs_apply",
            "cells={} wall={:?} touched={}",
            session.mapped.cell_count(),
            apply_started.elapsed(),
            touched.len()
        );
        if evaluations >= evaluation_budget {
            break;
        }
        if touched.is_empty() && next_dirty.is_empty() {
            break;
        }
        super::mfs::extend_candidate_invalidation(
            session.mapped,
            functions,
            &drivers,
            &optimization_boundary,
            touched,
            &mut next_dirty,
        );
        dirty = next_dirty;
    }

    crate::api::diagnostics::trace!(
        trace,
        "postmap.area.mfs",
        "cells={} wall={:?}",
        session.mapped.cell_count(),
        phase_started.elapsed()
    );
    Ok(session.finish())
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

/// Owns the mapped netlist and the running objective for common post-map
/// cleanup. When timing exists, the same candidate path also preserves or
/// improves constraint closure.
///
/// This mirrors [`super::session::TimingOptimizationSession`]: passes ask the
/// session to evaluate a candidate and never assemble the transaction inputs or
/// publish progress themselves.
struct AreaOptimizationSession<'a> {
    mapped: &'a mut MappedNetlist,
    implementations: &'a mut ImplementationDb,
    timing: Option<super::MmmcTiming>,
    closure: Option<crate::closure::mmmc::MmmcMetrics>,
    power: Option<super::MmmcPower>,
    library: &'a opto_library::TargetCellSet,
    scenarios: &'a opto_timing::ScenarioSet,
    physical: PhysicalObjective,
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

    /// Evaluates one candidate and, when it is accepted, publishes progress and
    /// returns the nets and cells its edit touched.
    fn evaluate(
        &mut self,
        candidate: PostmapCandidate,
        phase: OptimizationPhase,
    ) -> Result<CandidateDisposition<AcceptedCandidate>, crate::SynthError> {
        let mut disposition = evaluate_candidate(
            CandidateEvaluation {
                mapped: self.mapped,
                implementations: self.implementations,
                timing: self.timing.as_mut(),
                power: self.power.as_mut(),
                library: self.library,
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
            (self.observer)(SynthesisProgress::candidate(
                phase,
                self.physical.area,
                self.physical.cells,
            ));
        }
        Ok(disposition)
    }
}

fn increment_count(count: usize, what: &str) -> Result<usize, crate::SynthError> {
    count
        .checked_add(1)
        .ok_or_else(|| crate::SynthError::invariant(format!("{what} count overflow")))
}
