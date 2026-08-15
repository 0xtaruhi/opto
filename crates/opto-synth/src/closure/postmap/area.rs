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

    let optimization_boundary =
        super::mfs::optimization_boundary_nets(session.mapped, session.implementations)?;
    remove_dead_cells(
        &mut session,
        catalog,
        &optimization_boundary,
        &mut cleanup_dirty,
    )?;
    remove_constant_registers(
        &mut session,
        options,
        runtime,
        &optimization_boundary,
        &mut cleanup_dirty,
    )?;

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
    resynthesize_dirty_cells(
        &mut session,
        ResynthesisScope {
            catalog,
            runtime,
            trace,
            diagnostics,
            optimization_boundary: &optimization_boundary,
        },
        cleanup_dirty,
        phase_started,
    )?;
    Ok(session.finish())
}

/// Everything one resynthesis sweep reads but never edits.
#[derive(Clone, Copy)]
struct ResynthesisScope<'a> {
    catalog: &'a super::candidates::PostmapCellCatalog,
    runtime: &'a opto_runtime::ExecutionContext,
    trace: crate::api::diagnostics::SynthTrace,
    diagnostics: crate::SynthesisDiagnostics,
    optimization_boundary: &'a hashbrown::HashSet<opto_ir::mapped::NetId>,
}

/// Rewrites mapped cells whose context moved, until nothing is left to rewrite.
fn resynthesize_dirty_cells(
    session: &mut AreaOptimizationSession<'_>,
    scope: ResynthesisScope<'_>,
    cleanup_dirty: std::collections::HashSet<opto_ir::mapped::CellId>,
    phase_started: std::time::Instant,
) -> Result<(), crate::SynthError> {
    let ResynthesisScope {
        catalog,
        runtime,
        trace,
        diagnostics,
        optimization_boundary,
    } = scope;
    let functions = catalog.mfs_functions();
    let resynthesis = catalog.mfs_resynthesis(super::mfs::ResynthesisObjective::Area);
    let evaluation_budget = super::session::default_evaluation_budget(session.mapped.cell_count());
    let mut drivers = super::mfs::DriverIndex::build(session.mapped, functions);
    let mut evaluations = 0usize;
    // Mapped resynthesis seeds from a measured dirty cone, never from the whole
    // netlist. A region-owned cell was selected by cover under the same care set
    // and the same library, with exact-area recovery already applied, so
    // re-deriving it here repeats a decision that has not changed. The seeds are
    // the cells whose context did move: the ones this closure has already edited
    // above, and the retained non-region instances that cover never costed.
    let mut seeds = cleanup_dirty;
    for cell in session.mapped.cell_ids() {
        if !matches!(
            session.implementations.cell_ownership(cell)?,
            crate::MappedCellOwnership::Region(_)
        ) {
            seeds.insert(cell);
        }
    }
    let mut dirty = std::collections::HashSet::new();
    if !seeds.is_empty() {
        super::mfs::extend_candidate_invalidation(
            session.mapped,
            functions,
            &drivers,
            optimization_boundary,
            seeds,
            &mut dirty,
        );
    }
    // Candidate generation is read-only and parallel within a sweep. Selection
    // and commit are ordered afterward, so every task observes one mapped
    // generation and no accepted edit depends on worker completion order.
    //
    // A candidate held over from the previous round is derived from a mapped
    // generation no committed edit has reached, so it is scheduled in its
    // frontier position but not derived again. Scheduling over the whole
    // frontier keeps the selection order independent of which candidates were
    // carried and which were derived.
    let mut retained = std::collections::HashMap::<opto_ir::mapped::CellId, _>::new();
    // A search that found nothing depends only on the cells it read, which the
    // invalidation walk deliberately over-approximates. Remembering the read set
    // keeps the few cells with an expensive search from being asked the same
    // question every round.
    let mut barren = std::collections::HashMap::<opto_ir::mapped::CellId, Vec<_>>::new();
    loop {
        if evaluations >= evaluation_budget {
            break;
        }
        let frontier = dirty
            .iter()
            .copied()
            .chain(retained.keys().copied())
            .collect::<std::collections::HashSet<_>>();
        let work = super::region::scoped_work(
            session.mapped,
            &frontier,
            super::region::scheduling_cell_budget(frontier.len(), runtime.parallelism()),
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
            let context = super::mfs::OptimizationContext {
                mapped,
                implementations,
                functions,
                resynthesis,
                drivers,
                boundary: optimization_boundary,
                diagnostics: diagnostics.mfs,
            };
            let dirty = &dirty;
            runtime.map_ordered(tasks, move |work| {
                Ok::<_, crate::SynthError>(
                    work.cells
                        .into_iter()
                        .map(|cell| {
                            let mut read = Vec::new();
                            let candidate = dirty.contains(&cell).then(|| {
                                super::mfs::optimization_candidate_reading(context, cell, &mut read)
                            });
                            (cell, candidate.flatten(), read)
                        })
                        .collect::<Vec<_>>(),
                )
            })?
        };
        crate::api::diagnostics::trace!(
            trace,
            "postmap.area.mfs_generate",
            "scoped_cells={cell_count} derived={} wall={:?}",
            dirty.len(),
            sweep_started.elapsed()
        );
        let apply_started = std::time::Instant::now();
        let mut touched = std::collections::HashSet::new();
        let mut next_dirty = std::collections::HashSet::new();
        let pool = candidates
            .into_iter()
            .flatten()
            .map(|(cell, candidate, read)| {
                if candidate.is_none() && !read.is_empty() {
                    barren.insert(cell, read);
                }
                let candidate = candidate.or_else(|| retained.remove(&cell));
                (cell, candidate)
            })
            .collect::<Vec<_>>();
        let batch = select_non_conflicting(pool);
        retained.clear();
        retained.extend(batch.deferred);
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
        let touched_cells = touched.clone();
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
        if touched.is_empty() && next_dirty.is_empty() && retained.is_empty() {
            break;
        }
        super::mfs::extend_candidate_invalidation(
            session.mapped,
            functions,
            &drivers,
            optimization_boundary,
            touched,
            &mut next_dirty,
        );
        // A committed edit reached these cells, so any candidate carried for
        // them describes a netlist that no longer exists. Derive them again.
        retained.retain(|cell, _| !next_dirty.contains(cell));
        barren.retain(|_, read| !read.iter().any(|cell| touched_cells.contains(cell)));
        next_dirty.retain(|cell| !barren.contains_key(cell));
        dirty = next_dirty;
    }

    crate::api::diagnostics::trace!(
        trace,
        "postmap.area.mfs",
        "cells={} wall={:?}",
        session.mapped.cell_count(),
        phase_started.elapsed()
    );
    Ok(())
}

/// Removes every cell whose output no design object reads, and records the cells
/// the removals reached.
///
/// This runs before the rest of the closure so later phases do not evaluate,
/// time, or resynthesize logic that is already unobservable. Buffering, cloning,
/// and constant-register removal can each strand a driver, so the sweep repeats
/// until a scan finds nothing.
fn remove_dead_cells(
    session: &mut AreaOptimizationSession<'_>,
    catalog: &super::candidates::PostmapCellCatalog,
    optimization_boundary: &hashbrown::HashSet<opto_ir::mapped::NetId>,
    cleanup_dirty: &mut std::collections::HashSet<opto_ir::mapped::CellId>,
) -> Result<(), crate::SynthError> {
    let functions = catalog.mfs_functions();
    loop {
        let Some(candidate) =
            super::mfs::dead_cell_removal(session.mapped, functions, optimization_boundary)?
        else {
            return Ok(());
        };
        let CandidateDisposition::Accepted(edit) =
            session.evaluate(candidate, OptimizationPhase::RegisterOptimization)?
        else {
            return Ok(());
        };
        extend_cleanup_frontier(session.mapped, &edit, cleanup_dirty);
    }
}

/// Removes every register whose reachable value is one constant, and records the
/// cells the removals reached so later phases can rescope from them.
///
/// One round proves and commits the whole independent batch. A committed round
/// can expose the next register, so rounds repeat until a scan of the reached
/// cells proves nothing.
fn remove_constant_registers(
    session: &mut AreaOptimizationSession<'_>,
    options: &crate::SynthesisOptions,
    runtime: &opto_runtime::ExecutionContext,
    optimization_boundary: &hashbrown::HashSet<opto_ir::mapped::NetId>,
    cleanup_dirty: &mut std::collections::HashSet<opto_ir::mapped::CellId>,
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
        let CandidateDisposition::Accepted(edit) =
            session.evaluate(candidate, OptimizationPhase::RegisterOptimization)?
        else {
            return Ok(());
        };
        let mut reached = std::collections::HashSet::new();
        extend_cleanup_frontier(session.mapped, &edit, &mut reached);
        cleanup_dirty.extend(reached.iter().copied());
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
