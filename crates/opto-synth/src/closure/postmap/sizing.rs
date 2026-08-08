// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::PostmapCandidate;
use super::candidates::{self, PostmapCellCatalog, sizing_regions};
use super::forest::{self, EvaluationPolicy, RejectionPolicy};
use super::{TimingOptimizationPolicy, TimingOptimizationSession};
use crate::OptimizationPhase;
use opto_ir::mapped::{CellId, ConnectionRef, ConnectionSignal, MappedNetlist, PinId, RegionDelta};
use opto_library::TargetCellSet;
use opto_runtime::ExecutionContext;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SizingFrontier {
    WorstPath,
    AllViolations,
}

impl SizingFrontier {
    pub(super) const fn next(self) -> Option<Self> {
        match self {
            Self::WorstPath => Some(Self::AllViolations),
            Self::AllViolations => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizingCandidateKind {
    Monotonic,
    Tradeoff,
}

impl SizingCandidateKind {
    const fn phase(self) -> OptimizationPhase {
        match self {
            Self::Monotonic => OptimizationPhase::MonotonicSizing,
            Self::Tradeoff => OptimizationPhase::TradeoffSizing,
        }
    }
}

/// Runs bounded, frontier-based cell sizing after topology optimization.
///
/// Candidate discovery is parallel by region. One deterministic replacement
/// per region is then materialized as a single forest and evaluated with one
/// exact STA transaction.
pub(super) fn optimize(
    session: &mut TimingOptimizationSession<'_>,
    catalog: &PostmapCellCatalog,
    runtime: &ExecutionContext,
    policy: &TimingOptimizationPolicy,
) -> Result<(), crate::SynthError> {
    let mut passes = 0usize;
    let mut frontier = SizingFrontier::WorstPath;
    while policy.allows_pass(passes) {
        passes += 1;
        if session.qor_budget_exhausted() {
            break;
        }
        let timing_met = session.timing_met();
        let area_recovery = timing_met && !session.has_design_rule_violations();
        let cells = if timing_met || session.has_design_rule_violations() {
            session.mapped.cell_ids().collect::<Vec<_>>()
        } else {
            match frontier {
                SizingFrontier::WorstPath => mapped_cells_for_timing_instances(
                    session.timing.critical_instances()?,
                    session.mapped,
                )?,
                SizingFrontier::AllViolations => mapped_cells_for_timing_instances(
                    session.timing.instances_with_slack_at_most_all(0.0)?,
                    session.mapped,
                )?,
            }
        };
        let regions = sizing_regions(
            runtime,
            cells.into_iter().rev(),
            session.mapped,
            session.options,
            catalog,
            area_recovery,
            Some(&session.timing),
        )?;
        let mut accepted = false;
        for kind in [
            SizingCandidateKind::Monotonic,
            SizingCandidateKind::Tradeoff,
        ] {
            if evaluate_forest(&regions, kind, session)? {
                accepted = true;
                break;
            }
        }
        if accepted {
            frontier = SizingFrontier::WorstPath;
            continue;
        }
        if !timing_met && let Some(next) = frontier.next() {
            frontier = next;
            continue;
        }
        break;
    }
    Ok(())
}

pub(super) fn mapped_cells_for_timing_instances(
    instances: impl IntoIterator<Item = opto_timing::TimingInstanceId>,
    mapped: &MappedNetlist,
) -> Result<Vec<CellId>, crate::SynthError> {
    let mut cells = instances
        .into_iter()
        .map(|instance| {
            CellId::from_index(instance.raw() as usize).map_err(crate::SynthError::Mapped)
        })
        .collect::<Result<Vec<_>, _>>()?;
    cells.retain(|&cell| mapped.is_live_cell(cell));
    Ok(cells)
}

pub(super) fn evaluate_pin_swaps(
    session: &mut TimingOptimizationSession<'_>,
    catalog: &PostmapCellCatalog,
) -> Result<(), crate::SynthError> {
    if session.qor_budget_exhausted() {
        return Ok(());
    }
    let cells = if !session.timing_met() {
        mapped_cells_for_timing_instances(session.timing.critical_instances()?, session.mapped)?
    } else if session.has_design_rule_violations() {
        session.mapped.cell_ids().collect()
    } else {
        Vec::new()
    };
    let mut plans = Vec::new();
    for cell_id in cells.into_iter().collect::<BTreeSet<_>>().into_iter().rev() {
        let mapped_cell = session.mapped.cell(cell_id).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "pin swap references non-live mapped cell {cell_id:?}"
            ))
        })?;
        let Some(cell_index) = mapped_cell.library_cell.map(|index| index as usize) else {
            continue;
        };
        let cell = session
            .options
            .target_cells
            .get(cell_index)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "pin swap references unknown library index {cell_index}"
                ))
            })?;
        let Some(&(first, second)) = catalog.pin_swaps(cell_index).first() else {
            continue;
        };
        let first_pin = cell
            .pins()
            .nth(first as usize)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "pin-swap catalog references missing pin index {first} in cell '{}'",
                    cell.name()
                ))
            })?
            .name();
        let second_pin = cell
            .pins()
            .nth(second as usize)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "pin-swap catalog references missing pin index {second} in cell '{}'",
                    cell.name()
                ))
            })?
            .name();
        plans.push(pin_swap_plan(
            session.mapped,
            cell_id,
            first_pin,
            second_pin,
        )?);
    }
    if plans.is_empty() {
        return Ok(());
    }
    crate::api::diagnostics::trace!(
        session.trace(),
        "postmap.pin_swap_forest",
        "cells={}",
        plans.len()
    );
    forest::evaluate(
        &plans,
        OptimizationPhase::PinSwap,
        RejectionPolicy::KeepWhole,
        EvaluationPolicy::QorBudgeted,
        session,
        |mapped, _, _, plans| pin_swap_forest_delta(mapped, plans).map(Some),
    )?;
    Ok(())
}

fn evaluate_forest(
    regions: &[candidates::SizingRegion],
    kind: SizingCandidateKind,
    session: &mut TimingOptimizationSession<'_>,
) -> Result<bool, crate::SynthError> {
    let choices = regions
        .iter()
        .filter_map(|region| {
            let candidates = match kind {
                SizingCandidateKind::Monotonic => &region.monotonic_candidates,
                SizingCandidateKind::Tradeoff => &region.tradeoff_candidates,
            };
            candidates
                .first()
                .copied()
                .map(|candidate| (region.cell, candidate))
        })
        .collect::<Vec<_>>();
    forest::evaluate(
        &choices,
        kind.phase(),
        RejectionPolicy::KeepWhole,
        EvaluationPolicy::QorBudgeted,
        session,
        |mapped, _, options, choices| {
            sizing_forest_delta(mapped, &options.target_cells, choices).map(Some)
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PinSwapPlan {
    pub(super) cell: CellId,
    first: PinId,
    second: PinId,
}

pub(super) fn pin_swap_plan(
    mapped: &MappedNetlist,
    cell: CellId,
    first_pin: &str,
    second_pin: &str,
) -> Result<PinSwapPlan, crate::SynthError> {
    let first = mapped_pin(mapped, cell, first_pin)?;
    let second = mapped_pin(mapped, cell, second_pin)?;
    Ok(PinSwapPlan {
        cell,
        first,
        second,
    })
}

pub(super) fn pin_swap_forest_delta(
    mapped: &MappedNetlist,
    plans: &[PinSwapPlan],
) -> Result<PostmapCandidate, crate::SynthError> {
    if plans.is_empty() {
        return Err(crate::SynthError::invariant(
            "pin-swap forest requires at least one cell",
        ));
    }
    let mut cells = Vec::with_capacity(plans.len());
    let mut unique_cells = BTreeSet::new();
    let mut swaps = Vec::with_capacity(plans.len());
    let mut nets = Vec::new();
    for &plan in plans {
        if !unique_cells.insert(plan.cell) {
            return Err(crate::SynthError::invariant(
                "pin-swap forest contains duplicate cells",
            ));
        }
        if plan.first == plan.second
            || mapped.pin_owner(plan.first) != Some(plan.cell)
            || mapped.pin_owner(plan.second) != Some(plan.cell)
        {
            return Err(crate::SynthError::invariant(
                "pin-swap plan does not reference two distinct pins of its cell",
            ));
        }
        let first_signal = mapped
            .connection(plan.first)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!("mapped pin {:?} disappeared", plan.first))
            })?
            .signal;
        let second_signal = mapped
            .connection(plan.second)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!("mapped pin {:?} disappeared", plan.second))
            })?
            .signal;
        nets.extend(
            [first_signal, second_signal]
                .into_iter()
                .filter_map(|signal| match signal {
                    ConnectionSignal::Net(net) => Some(net),
                    ConnectionSignal::Constant(_) => None,
                }),
        );
        cells.push(plan.cell);
        swaps.push((plan, first_signal, second_signal));
    }
    nets.sort_unstable();
    nets.dedup();
    let snapshot = mapped
        .snapshot_region(cells, nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    for (plan, first_signal, second_signal) in swaps {
        delta
            .reconnect_pin(plan.first, connection_ref(second_signal))
            .map_err(crate::SynthError::from)?;
        delta
            .reconnect_pin(plan.second, connection_ref(first_signal))
            .map_err(crate::SynthError::from)?;
    }
    Ok(PostmapCandidate::new(delta))
}

pub(super) fn sizing_delta(
    mapped: &MappedNetlist,
    cell: CellId,
    cell_type: &str,
    library_cell: usize,
) -> Result<PostmapCandidate, crate::SynthError> {
    let nets = mapped
        .connections(cell)
        .ok_or_else(|| crate::SynthError::invariant(format!("sizing cell {cell:?} disappeared")))?
        .iter()
        .filter_map(|connection| match connection.signal {
            ConnectionSignal::Net(net) => Some(net),
            ConnectionSignal::Constant(_) => None,
        });
    let snapshot = mapped
        .snapshot_region([cell], nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    delta
        .replace_cell(
            cell,
            cell_type,
            Some(
                u32::try_from(library_cell).map_err(|_| {
                    crate::SynthError::capacity("library cell index exceeds capacity")
                })?,
            ),
        )
        .map_err(crate::SynthError::from)?;
    Ok(PostmapCandidate::new(delta))
}

pub(super) fn sizing_forest_delta(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    choices: &[(CellId, usize)],
) -> Result<PostmapCandidate, crate::SynthError> {
    if choices.is_empty() {
        return Err(crate::SynthError::invariant(
            "sizing forest requires at least one replacement",
        ));
    }
    let mut cells = Vec::with_capacity(choices.len());
    let mut unique_cells = BTreeSet::new();
    let mut nets = Vec::new();
    for &(cell, candidate_index) in choices {
        if !unique_cells.insert(cell) {
            return Err(crate::SynthError::invariant(
                "sizing forest contains duplicate cells",
            ));
        }
        library.get(candidate_index).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "sizing candidate references unknown library index {candidate_index}"
            ))
        })?;
        cells.push(cell);
        nets.extend(
            mapped
                .connections(cell)
                .ok_or_else(|| {
                    crate::SynthError::invariant(format!("sizing cell {cell:?} disappeared"))
                })?
                .iter()
                .filter_map(|connection| match connection.signal {
                    ConnectionSignal::Net(net) => Some(net),
                    ConnectionSignal::Constant(_) => None,
                }),
        );
    }
    nets.sort_unstable();
    nets.dedup();
    let snapshot = mapped
        .snapshot_region(cells.iter().copied(), nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    for &(cell, candidate_index) in choices {
        let candidate = library.get(candidate_index).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "sizing candidate references unknown library index {candidate_index}"
            ))
        })?;
        delta
            .replace_cell(
                cell,
                candidate.name(),
                Some(u32::try_from(candidate_index).map_err(|_| {
                    crate::SynthError::capacity("library cell index exceeds capacity")
                })?),
            )
            .map_err(crate::SynthError::from)?;
    }
    Ok(PostmapCandidate::new(delta))
}

fn mapped_pin(
    mapped: &MappedNetlist,
    cell: CellId,
    name: &str,
) -> Result<PinId, crate::SynthError> {
    mapped
        .pin_ids(cell)
        .ok_or_else(|| crate::SynthError::invariant(format!("mapped cell {cell:?} disappeared")))?
        .find(|pin| {
            mapped
                .connection(*pin)
                .and_then(|connection| mapped.pin_name(connection))
                == Some(name)
        })
        .ok_or_else(|| {
            crate::SynthError::invariant(format!("mapped cell {cell:?} has no pin '{name}'"))
        })
}

fn connection_ref(signal: ConnectionSignal) -> ConnectionRef {
    match signal {
        ConnectionSignal::Net(net) => ConnectionRef::Net(net),
        ConnectionSignal::Constant(value) => ConnectionRef::Constant(value),
    }
}
