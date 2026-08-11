// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::CandidateDisposition;
use super::candidates::PostmapCellCatalog;
use super::{TimingOptimizationSession, mfs, sizing};
use crate::OptimizationPhase;
use opto_runtime::{ExecutionContext, Task, TaskKey};

const RESYNTHESIS_PLAN_TASK_DOMAIN: u32 = 0x4d46_5352;

/// Runs timing-driven multi-function resynthesis over deterministic scheduling
/// chunks.
///
/// Each planning sweep reads one committed mapped generation in parallel.
/// Candidate evaluation and commits remain ordered, and accepted edits refresh
/// only the affected driver index entries before the next sweep.
pub(super) fn optimize(
    session: &mut TimingOptimizationSession<'_>,
    catalog: &PostmapCellCatalog,
    runtime: &ExecutionContext,
    enabled: bool,
) -> Result<(), crate::SynthError> {
    if !enabled || session.timing_met() {
        return Ok(());
    }
    let functions = catalog.mfs_functions();
    let resynthesis = catalog.mfs_resynthesis(mfs::ResynthesisObjective::Timing);
    let optimization_boundary =
        mfs::optimization_boundary_nets(session.mapped, session.implementations)?;
    let mut drivers = mfs::DriverIndex::build(session.mapped, functions);
    loop {
        let cells = sizing::mapped_cells_for_timing_instances(
            session.timing.critical_instances()?,
            session.mapped,
        )?;
        let eligible = cells.into_iter().collect::<std::collections::HashSet<_>>();
        let tasks = super::region::scoped_work(
            session.mapped,
            &eligible,
            super::region::scheduling_cell_budget(eligible.len(), runtime.parallelism()),
        )?
        .into_iter()
        .map(|work| {
            Task::new(
                TaskKey::new(RESYNTHESIS_PLAN_TASK_DOMAIN, work.task_ordinal()),
                work,
            )
        })
        .collect::<Vec<_>>();
        let candidates = {
            let mapped = &*session.mapped;
            let implementations = &*session.implementations;
            let diagnostics = session.diagnostics().mfs;
            let context = mfs::OptimizationContext {
                mapped,
                implementations,
                functions,
                resynthesis,
                drivers: &drivers,
                boundary: &optimization_boundary,
                diagnostics,
            };
            runtime.map_ordered(tasks, |work| {
                Ok::<_, crate::SynthError>(
                    work.cells
                        .into_iter()
                        .map(|cell| mfs::optimization_candidate(context, cell))
                        .collect::<Vec<_>>(),
                )
            })?
        };
        let mut changed = false;
        for candidate in candidates.into_iter().flatten().flatten() {
            if session.qor_budget_exhausted() {
                break;
            }
            let affected_nets = candidate.delta.snapshot().net_ids().collect::<Vec<_>>();
            if session.evaluate_qor(candidate, OptimizationPhase::BooleanResynthesis)?
                == CandidateDisposition::Accepted(())
            {
                drivers.refresh(session.mapped, functions, affected_nets);
                changed = true;
            }
        }
        if !changed || session.qor_budget_exhausted() {
            return Ok(());
        }
    }
}
