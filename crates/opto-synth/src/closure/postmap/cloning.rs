// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::buffering::{self, library_pin, net_sink_pins};
use super::candidate::PostmapCandidate;
use super::forest::{self, EvaluationPolicy, RejectionPolicy};
use super::{TimingOptimizationSession, sizing};
use crate::{ImplementationDb, OptimizationPhase};
use opto_ir::mapped::{
    CellId, CellSpec, ConnectionRef, ConnectionSignal, MappedNetlist, NetId, PinId, RegionDelta,
};
use opto_library::{TargetCellSet, TargetPinDirection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CloneBranchPlan {
    pub(super) net: NetId,
    pub(super) branch: Vec<PinId>,
    pub(super) instance_name: String,
    pub(super) net_name: String,
}

/// Clones the residual critical branches left after global HFNS and
/// electrical legalization.
///
/// Planning is deterministic and produces at most one branch per source net.
/// The complete forest is evaluated atomically; deterministic bisection is
/// used only when the aggregate transaction does not improve closure.
pub(super) fn optimize(
    session: &mut TimingOptimizationSession<'_>,
    enabled: bool,
) -> Result<(), crate::SynthError> {
    if !enabled || session.qor_budget_exhausted() {
        return Ok(());
    }

    let mut branches = std::collections::BTreeMap::new();
    if enabled {
        for fanout in timing_critical_fanouts(session)? {
            branches.insert(fanout.net, fanout.clone_branch);
        }
    }
    let plans = branches
        .into_iter()
        .enumerate()
        .map(|(ordinal, (net, branch))| CloneBranchPlan {
            net,
            branch,
            instance_name: format!("U_clone_{ordinal}"),
            net_name: format!("_clone_net_{ordinal}"),
        })
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return Ok(());
    }
    let trace = session.trace();
    if trace.is_enabled() {
        let sinks = plans.iter().try_fold(0usize, |total, plan| {
            total.checked_add(plan.branch.len()).ok_or_else(|| {
                crate::SynthError::capacity("residual clone-forest sink count exceeds capacity")
            })
        })?;
        crate::api::diagnostics::trace!(
            trace,
            "postmap.clone_forest",
            "branches={} sinks={sinks}",
            plans.len()
        );
    }
    forest::evaluate(
        &plans,
        OptimizationPhase::CriticalFanoutCloning,
        RejectionPolicy::Bisect,
        EvaluationPolicy::QorBudgeted,
        session,
        |mapped, implementations, options, plans| {
            clone_driver_forest_delta(mapped, implementations, &options.target_cells, plans)
        },
    )?;
    Ok(())
}

fn timing_critical_fanouts(
    session: &mut TimingOptimizationSession<'_>,
) -> Result<Vec<buffering::CriticalFanout>, crate::SynthError> {
    let frontier = session.timing.critical_frontier()?;
    let cells = sizing::mapped_cells_for_timing_instances(frontier.instances, session.mapped)?;
    buffering::critical_fanouts(
        session.mapped,
        &session.options.target_cells,
        cells,
        frontier.mapped_nets,
    )
}

/// Clone the driver of `net`, moving the `branch` sinks onto the clone's
/// output while every other connection of the driver is duplicated verbatim.
/// Returns `None` when the net has no qualifying driver: cloning is limited
/// to a unique, single-output, non-tristate, purely combinational cell, and
/// the original driver must keep at least one sink.
#[cfg(test)]
pub(super) fn clone_driver_delta(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    net: NetId,
    branch: &[PinId],
    instance_name: &str,
    net_name: &str,
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
    let implementations = ImplementationDb::empty(mapped.cell_slot_count());
    clone_driver_forest_delta(
        mapped,
        &implementations,
        library,
        &[CloneBranchPlan {
            net,
            branch: branch.to_vec(),
            instance_name: instance_name.to_string(),
            net_name: net_name.to_string(),
        }],
    )
}

pub(super) fn clone_driver_forest_delta(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    library: &TargetCellSet,
    plans: &[CloneBranchPlan],
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
    let mut segmented = Vec::new();
    let mut source_nets = std::collections::BTreeSet::new();
    for plan in plans {
        if !source_nets.insert(plan.net) {
            return Err(crate::SynthError::invariant(
                "clone forest contains duplicate source nets",
            ));
        }
        let groups = buffering::group_sink_pins_by_owner(
            mapped,
            implementations,
            plan.branch.iter().copied(),
        )?;
        let multiple = groups.len() > 1;
        segmented.extend(
            groups
                .into_iter()
                .enumerate()
                .map(|(segment, (sink, branch))| {
                    let suffix = if multiple {
                        format!("_{segment}")
                    } else {
                        String::new()
                    };
                    (
                        CloneBranchPlan {
                            net: plan.net,
                            branch,
                            instance_name: format!("{}{suffix}", plan.instance_name),
                            net_name: format!("{}{suffix}", plan.net_name),
                        },
                        sink,
                    )
                }),
        );
    }
    let mut prepared = Vec::new();
    let mut cells = Vec::new();
    let mut nets = Vec::new();
    for (plan, sink) in &segmented {
        let Some(clone) = prepare_clone(mapped, library, plan)? else {
            continue;
        };
        cells.push(clone.driver.cell);
        for &pin in &plan.branch {
            cells.push(mapped.pin_owner(pin).ok_or_else(|| {
                crate::SynthError::invariant(format!("clone branch pin {pin:?} has no live owner"))
            })?);
        }
        nets.extend(clone.driver_pins.iter().filter_map(|&pin| {
            match mapped.connection(pin).map(|connection| connection.signal) {
                Some(ConnectionSignal::Net(net)) => Some(net),
                _ => None,
            }
        }));
        nets.push(plan.net);
        prepared.push((clone, *sink));
    }
    if prepared.is_empty() {
        return Ok(None);
    }
    cells.sort_unstable();
    cells.dedup();
    nets.sort_unstable();
    nets.dedup();
    let snapshot = mapped
        .snapshot_region(cells, nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    let mut added_cells = Vec::with_capacity(prepared.len());
    for (clone, sink) in prepared {
        let new_net = delta
            .add_net(Some(clone.plan.net_name.clone()))
            .map_err(crate::SynthError::from)?;
        let mut spec = CellSpec::new(
            &clone.plan.instance_name,
            &clone.cell_type,
            Some(clone.library_cell),
        );
        for &pin in &clone.driver_pins {
            let connection = mapped.connection(pin).ok_or_else(|| {
                crate::SynthError::invariant(format!("clone driver pin {pin:?} disappeared"))
            })?;
            let pin_name = mapped.pin_name(connection).ok_or_else(|| {
                crate::SynthError::invariant(format!("clone driver pin {pin:?} has no name"))
            })?;
            let reference = if pin == clone.driver.output {
                ConnectionRef::NewNet(new_net)
            } else {
                match connection.signal {
                    ConnectionSignal::Net(input) => ConnectionRef::Net(input),
                    ConnectionSignal::Constant(value) => ConnectionRef::Constant(value),
                }
            };
            spec = spec.connect(pin_name, connection.library_pin, reference);
        }
        let added = delta.add_cell(spec).map_err(crate::SynthError::from)?;
        for &pin in &clone.plan.branch {
            delta
                .reconnect_pin(pin, ConnectionRef::NewNet(new_net))
                .map_err(crate::SynthError::from)?;
        }
        added_cells.push((added, clone.driver.cell, sink));
    }
    let mut candidate = PostmapCandidate::new(delta);
    for (added, source, sink) in added_cells {
        candidate = candidate.record_repair_segment(implementations, added, &[source], sink)?;
    }
    Ok(Some(candidate))
}

struct PreparedClone<'a> {
    plan: &'a CloneBranchPlan,
    driver: QualifiedDriver,
    cell_type: String,
    library_cell: u32,
    driver_pins: Vec<PinId>,
}

fn prepare_clone<'a>(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    plan: &'a CloneBranchPlan,
) -> Result<Option<PreparedClone<'a>>, crate::SynthError> {
    if plan.branch.is_empty() {
        return Ok(None);
    }
    let Some(driver) = qualified_driver(mapped, library, plan.net)? else {
        return Ok(None);
    };
    if plan.branch.len() >= net_sink_pins(mapped, library, plan.net)?.len() {
        return Ok(None);
    }
    for &pin in &plan.branch {
        if mapped.connection(pin).map(|connection| connection.signal)
            != Some(ConnectionSignal::Net(plan.net))
        {
            return Err(crate::SynthError::invariant(format!(
                "clone branch pin {pin:?} is not a sink of the split net"
            )));
        }
    }
    let cell_type = mapped
        .cell_type(driver.cell)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!("clone driver {:?} has no cell type", driver.cell))
        })?
        .to_string();
    let library_cell = mapped
        .cell(driver.cell)
        .and_then(|cell| cell.library_cell)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "clone driver {:?} lost its library cell",
                driver.cell
            ))
        })?;
    let driver_pins = mapped
        .pin_ids(driver.cell)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!("clone driver {:?} disappeared", driver.cell))
        })?
        .collect::<Vec<_>>();
    Ok(Some(PreparedClone {
        plan,
        driver,
        cell_type,
        library_cell,
        driver_pins,
    }))
}

#[derive(Clone, Copy)]
struct QualifiedDriver {
    cell: CellId,
    output: PinId,
}

fn qualified_driver(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    net: NetId,
) -> Result<Option<QualifiedDriver>, crate::SynthError> {
    if mapped
        .constant_drivers()
        .iter()
        .any(|&(driven, _)| driven == net)
    {
        return Ok(None);
    }
    let Some(pins) = mapped.pins_on_net(net) else {
        return Ok(None);
    };
    let mut output = None;
    for pin in pins.collect::<Vec<_>>() {
        let Some(target) = library_pin(mapped, library, pin)? else {
            // A pin without a library binding has an unknown direction, so
            // the driver structure of this net cannot be established.
            return Ok(None);
        };
        match target.direction() {
            TargetPinDirection::Output => {
                if output.is_some() {
                    return Ok(None);
                }
                output = Some(pin);
            }
            TargetPinDirection::Inout | TargetPinDirection::Internal => return Ok(None),
            TargetPinDirection::Input => {}
        }
    }
    let Some(output) = output else {
        return Ok(None);
    };
    let cell = mapped.pin_owner(output).ok_or_else(|| {
        crate::SynthError::invariant(format!("driver pin {output:?} has no live owner"))
    })?;
    let Some(index) = mapped.cell(cell).and_then(|record| record.library_cell) else {
        return Ok(None);
    };
    let target = library.get(index as usize).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "mapped cell {cell:?} references unknown library cell {index}"
        ))
    })?;
    if !target.is_synthesis_eligible() || target.sequential().next().is_some() {
        return Ok(None);
    }
    let mut outputs = 0usize;
    for pin in target.pins() {
        if pin.three_state().is_some() {
            return Ok(None);
        }
        match pin.direction() {
            TargetPinDirection::Output => outputs += 1,
            TargetPinDirection::Inout | TargetPinDirection::Internal => return Ok(None),
            TargetPinDirection::Input => {}
        }
    }
    if outputs != 1 {
        return Ok(None);
    }
    Ok(Some(QualifiedDriver { cell, output }))
}
