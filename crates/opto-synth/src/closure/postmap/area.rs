// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::{CandidateDisposition, PostmapCandidate};
use super::candidates::sizing_regions;
use super::objective::{PhysicalObjective, mapped_physical_objective};
use super::session::{AcceptedCandidate, CandidateEvaluation, ClosureBaseline, evaluate_candidate};
use super::sizing::sizing_delta;
use super::{PostmapOutcome, PostmapRequest};
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
        library: &options.target_cells,
        scenarios,
        physical,
        replacements: 0,
        observer,
        connectivity,
    };
    let phase_started = std::time::Instant::now();

    let optimization_boundary =
        super::mfs::optimization_boundary_nets(session.mapped, session.implementations)?;
    remove_dead_cells(&mut session, catalog, &optimization_boundary)?;
    remove_constant_registers(&mut session, options, runtime, &optimization_boundary)?;

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
    }
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
                CandidateDisposition::Accepted(_) | CandidateDisposition::Stale => break,
                CandidateDisposition::Rejected => {}
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
            if let CandidateDisposition::Accepted(edit) =
                session.evaluate(candidate, OptimizationPhase::BooleanResynthesis)?
            {
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
        let CandidateDisposition::Accepted(_) =
            session.evaluate(candidate, OptimizationPhase::RegisterOptimization)?
        else {
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
        let CandidateDisposition::Accepted(edit) =
            session.evaluate(candidate, OptimizationPhase::RegisterOptimization)?
        else {
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

    /// Evaluates a candidate and returns the accepted edit frontier.
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
