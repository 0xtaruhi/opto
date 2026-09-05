// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::PostmapCandidate;
use crate::{ImplementationDb, RegionAnchorId};
use opto_ir::mapped::{
    CellId, CellSpec, ConnectionRef, ConnectionSignal, MappedNetlist, NetId, PinId, RegionDelta,
    TempNetId,
};
use opto_library::{
    TargetCellRef, TargetCellSet, TargetPinDirection, TargetPinRef, TargetTimingType, TimingEdge,
    normalized_cell_area,
};
use opto_timing::{DesignRuleKind, DesignRuleViolation};
use std::collections::{BTreeMap, BTreeSet};

mod tree;
use tree::{
    balance_sink_groups, branching_factor_candidates, estimated_buffer_path_delay,
    maximum_legal_factor, tree_shape,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CriticalFanout {
    pub(super) net: NetId,
    pub(super) clone_branch: Vec<PinId>,
    pub(super) sinks: Vec<PinId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FanoutTreeStrategy {
    pub(super) buffer_index: usize,
    pub(super) branching_factor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FanoutTreePlan {
    pub(super) net: NetId,
    pub(super) leaf_groups: Vec<Vec<PinId>>,
    pub(super) strategy: FanoutTreeStrategy,
    pub(super) namespace: u64,
    pub(super) ordinal: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BufferBranchPlan {
    pub(super) net: NetId,
    pub(super) sinks: Vec<PinId>,
    pub(super) buffer_index: usize,
    pub(super) instance_name: String,
    pub(super) net_name: String,
}

impl FanoutTreePlan {
    pub(super) fn sink_count(&self) -> usize {
        self.leaf_groups.iter().map(Vec::len).sum()
    }
}

pub(super) struct FanoutTreeSelection {
    pub(super) strategy: FanoutTreeStrategy,
    pub(super) leaf_groups: Vec<Vec<PinId>>,
}

pub(super) fn critical_fanouts(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    cells: impl IntoIterator<Item = CellId>,
    critical_nets: impl IntoIterator<Item = NetId>,
) -> Result<Vec<CriticalFanout>, crate::SynthError> {
    let critical_nets = critical_nets.into_iter().collect::<BTreeSet<_>>();
    let mut critical_by_net = BTreeMap::<NetId, BTreeSet<PinId>>::new();
    for cell in cells {
        for pin in cell_input_pins(mapped, library, cell)? {
            let Some(ConnectionSignal::Net(net)) =
                mapped.connection(pin).map(|connection| connection.signal)
            else {
                continue;
            };
            if !critical_nets.contains(&net) {
                continue;
            }
            critical_by_net.entry(net).or_default().insert(pin);
        }
    }

    let mut fanouts = Vec::new();
    for (net, critical) in critical_by_net {
        let mut sinks = net_sink_pins(mapped, library, net)?
            .into_iter()
            .map(|(pin, _)| pin)
            .collect::<Vec<_>>();
        sinks.sort_unstable();
        if sinks.len() < 2 {
            continue;
        }
        let mut clone_branch = sinks
            .iter()
            .filter_map(|&pin| critical.contains(&pin).then_some(pin))
            .collect::<Vec<_>>();
        if clone_branch.len() == sinks.len() {
            clone_branch.truncate(sinks.len().div_ceil(2));
        }
        if !clone_branch.is_empty() {
            fanouts.push(CriticalFanout {
                net,
                clone_branch,
                sinks,
            });
        }
    }
    fanouts.sort_by(|left, right| {
        right
            .sinks
            .len()
            .cmp(&left.sinks.len())
            .then_with(|| left.net.cmp(&right.net))
    });
    Ok(fanouts)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded buffer counts scale an approximate physical area estimate"
)]
pub(super) fn select_fanout_tree_strategy(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    scenarios: &opto_timing::ScenarioSet,
    buffer_candidates: &[usize],
    sinks: &[PinId],
    net_states: &[crate::closure::mmmc::MmmcNetState],
) -> Result<Option<FanoutTreeSelection>, crate::SynthError> {
    let sink_count = sinks.len();
    if sink_count < 3 {
        return Ok(None);
    }
    let timing_views = fanout_timing_views(mapped, scenarios, sinks, net_states)?;
    let mut best_score = None::<(f64, f64, usize, &str, usize)>;
    let mut best = None;
    for &buffer_index in buffer_candidates {
        let buffer = library.get(buffer_index).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "unknown fanout-tree buffer library index {buffer_index}"
            ))
        })?;
        let descriptor = buffer_descriptor(buffer, buffer_index)?;
        let views = buffer_timing_views(
            &timing_views,
            buffer.name(),
            descriptor.input.name(),
            descriptor.output.name(),
        )?;
        if views.is_empty() {
            continue;
        }
        let Some(unbuffered_wire_delay) = estimated_unbuffered_wire_delay(&views) else {
            continue;
        };
        let characterized_factor = views
            .iter()
            .filter_map(|view| {
                view.output
                    .timing_arcs()
                    .filter(|arc| arc.timing_type() == TargetTimingType::Combinational)
                    .filter_map(|arc| {
                        arc.delay_model().and_then(
                            opto_library::ArcDelayModel::maximum_characterized_output_load,
                        )
                    })
                    .min_by(f64::total_cmp)
                    .map(|maximum_load| {
                        maximum_legal_factor(view, maximum_load, sink_count.saturating_sub(1))
                    })
            })
            .min();
        let maximum_factor = match characterized_factor {
            Some(factor) => factor,
            None => sink_count.saturating_sub(1),
        }
        .min(sink_count.saturating_sub(1));
        if maximum_factor < 2 {
            continue;
        }
        for branching_factor in branching_factor_candidates(sink_count, maximum_factor)? {
            let leaf_groups = balance_sink_groups(sinks, &views, branching_factor)?;
            let (levels, buffers) = tree_shape(sink_count, branching_factor)?;
            let Some(path_delay) =
                estimated_buffer_path_delay(&views, sinks, &leaf_groups, branching_factor, levels)
            else {
                continue;
            };
            let area = normalized_cell_area(buffer.area()) * buffers as f64;
            if !path_delay.is_finite() || !area.is_finite() || path_delay >= unbuffered_wire_delay {
                continue;
            }
            let score = (path_delay, area, buffers, buffer.name(), branching_factor);
            if best_score.as_ref().is_none_or(|current| {
                score
                    .0
                    .total_cmp(&current.0)
                    .then_with(|| score.1.total_cmp(&current.1))
                    .then_with(|| score.2.cmp(&current.2))
                    .then_with(|| score.3.cmp(current.3))
                    .then_with(|| score.4.cmp(&current.4))
                    .is_lt()
            }) {
                best_score = Some(score);
                best = Some(FanoutTreeSelection {
                    strategy: FanoutTreeStrategy {
                        buffer_index,
                        branching_factor,
                    },
                    leaf_groups: leaf_groups
                        .into_iter()
                        .map(|group| group.into_iter().map(|index| sinks[index]).collect())
                        .collect(),
                });
            }
        }
    }
    Ok(best)
}

fn estimated_unbuffered_wire_delay(views: &[BufferTimingView<'_, '_, '_>]) -> Option<f64> {
    if views.is_empty() {
        return None;
    }
    let mut worst = None::<f64>;
    for view in views {
        let Some(state) = view.net_state else {
            continue;
        };
        let wire = view.wire_load?;
        let sink_capacitance = view
            .sink_loads
            .iter()
            .map(|load| load.capacitance)
            .max_by(f64::total_cmp)?;
        let delay = view.wire_tree.sink_delay(
            view.units.normalize_resistance(state.wire_resistance),
            wire.capacitance_at(state.wire_fanout),
            state.wire_fanout,
            state.capacitance,
            sink_capacitance,
        );
        worst = Some(worst.map_or(delay, |current| current.max(delay)));
    }
    worst
}

struct FanoutTimingView<'library, 'state> {
    scenario: &'library str,
    cells_by_name: BTreeMap<&'library str, TargetCellRef<'library>>,
    wire_load: Option<&'library opto_library::WireLoadModel>,
    wire_tree: opto_library::WireLoadTree,
    units: opto_library::TimingLibraryUnits,
    net_state: Option<&'state opto_timing::NetTimingState>,
    sink_loads: Box<[ElectricalLoad]>,
}

struct BufferTimingView<'library, 'loads, 'state> {
    input: TargetPinRef<'library>,
    output: TargetPinRef<'library>,
    wire_load: Option<&'library opto_library::WireLoadModel>,
    wire_tree: opto_library::WireLoadTree,
    units: opto_library::TimingLibraryUnits,
    net_state: Option<&'state opto_timing::NetTimingState>,
    sink_loads: &'loads [ElectricalLoad],
}

#[derive(Debug, Clone, Copy, Default)]
struct ElectricalLoad {
    capacitance: f64,
    fanout: f64,
    /// Receiver count, independent of the abstract fanout load.
    receivers: f64,
    /// Largest individual receiver load for a worst-branch estimate.
    max_sink_capacitance: f64,
}

/// One mapped sink pin resolved against the netlist that owns it.
struct SinkPin<'a> {
    cell_name: &'a str,
    pin_name: &'a str,
    library_pin: usize,
}

/// Builds one view per MMMC corner, each indexed by cell name so repeated
/// buffer and sink lookups are not linear scans of the timing library.
fn fanout_timing_views<'library, 'state>(
    mapped: &MappedNetlist,
    scenarios: &'library opto_timing::ScenarioSet,
    sinks: &[PinId],
    net_states: &'state [crate::closure::mmmc::MmmcNetState],
) -> Result<Vec<FanoutTimingView<'library, 'state>>, crate::SynthError> {
    let expected_views = scenarios.analysis_views().len();
    if net_states.len() != expected_views {
        return Err(crate::SynthError::invariant(
            "MMMC fanout net states do not align with scenario views",
        ));
    }
    let resolved = sinks
        .iter()
        .map(|&pin| resolve_sink_pin(mapped, pin))
        .collect::<Result<Vec<_>, _>>()?;
    if net_states
        .windows(2)
        .any(|pair| pair[0].view >= pair[1].view)
    {
        return Err(crate::SynthError::invariant(
            "MMMC fanout net states are not in unique canonical view order",
        ));
    }
    let mut views = Vec::with_capacity(expected_views);
    for (view, scenario, delay_type) in scenarios.analysis_views() {
        let state = net_states
            .binary_search_by_key(&view, |state| state.view)
            .map_err(|_| {
                crate::SynthError::invariant("MMMC fanout net states omit an analysis view")
            })?;
        let library = match delay_type {
            opto_timing::DelayType::Max => scenario.late_library(),
            opto_timing::DelayType::Min => scenario.early_library(),
        };
        let cells_by_name = library
            .cells
            .iter()
            .map(|cell| (cell.name(), cell))
            .collect::<BTreeMap<_, _>>();
        let sink_loads = resolved
            .iter()
            .map(|sink| sink_electrical_load(&cells_by_name, sink, scenario.name()))
            .collect::<Result<Vec<_>, _>>()?;
        views.push(FanoutTimingView {
            scenario: scenario.name(),
            cells_by_name,
            wire_load: library.wire_load_model.as_ref(),
            wire_tree: library.wire_load_tree,
            units: library.units,
            net_state: net_states[state].state.as_ref(),
            sink_loads: sink_loads.into_boxed_slice(),
        });
    }
    Ok(views)
}

fn buffer_timing_views<'library, 'loads, 'state>(
    timing_views: &'loads [FanoutTimingView<'library, 'state>],
    cell_name: &str,
    input_name: &str,
    output_name: &str,
) -> Result<Vec<BufferTimingView<'library, 'loads, 'state>>, crate::SynthError> {
    let mut views = Vec::new();
    for timing_view in timing_views {
        let buffer = timing_view.cells_by_name.get(cell_name).copied();
        let pins = buffer.and_then(|cell| {
            let input = cell.pins().find(|pin| pin.name() == input_name)?;
            let output = cell.pins().find(|pin| pin.name() == output_name)?;
            Some((input, output))
        });
        let (input, output) = pins.ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "scenario '{}' timing library does not characterize fanout-tree buffer \
                 '{cell_name}' pins '{input_name}' and '{output_name}'",
                timing_view.scenario
            ))
        })?;
        views.push(BufferTimingView {
            input,
            output,
            wire_load: timing_view.wire_load,
            wire_tree: timing_view.wire_tree,
            units: timing_view.units,
            net_state: timing_view.net_state,
            sink_loads: &timing_view.sink_loads,
        });
    }
    Ok(views)
}

/// Resolves a sink pin to the cell type and library pin the netlist bound it to.
fn resolve_sink_pin(mapped: &MappedNetlist, pin: PinId) -> Result<SinkPin<'_>, crate::SynthError> {
    let resolved = mapped
        .pin_owner(pin)
        .and_then(|owner| mapped.cell_type(owner))
        .zip(mapped.connection(pin))
        .and_then(|(cell_name, connection)| {
            Some(SinkPin {
                cell_name,
                pin_name: mapped.pin_name(connection)?,
                library_pin: connection.library_pin? as usize,
            })
        });
    resolved.ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "fanout-tree sink pin {pin:?} is not a live library-bound receiver"
        ))
    })
}

/// Reads a sink's input capacitance and fanout load from one timing view.
fn sink_electrical_load(
    cells_by_name: &BTreeMap<&str, TargetCellRef<'_>>,
    sink: &SinkPin<'_>,
    scenario: &str,
) -> Result<ElectricalLoad, crate::SynthError> {
    let target = cells_by_name
        .get(sink.cell_name)
        .and_then(|cell| cell.pins().nth(sink.library_pin))
        .filter(|target| {
            target.name() == sink.pin_name
                && matches!(
                    target.direction(),
                    TargetPinDirection::Input | TargetPinDirection::Inout
                )
        });
    let target = target.ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "scenario '{scenario}' timing library does not characterize receiver pin '{}/{}'",
            sink.cell_name, sink.pin_name
        ))
    })?;
    Ok(ElectricalLoad {
        capacitance: target.design_input_capacitance(),
        fanout: target.design_fanout_load(),
        receivers: 1.0,
        max_sink_capacitance: target.design_input_capacitance(),
    })
}

pub(super) fn fanout_tree_buffer_count(
    sink_count: usize,
    strategy: FanoutTreeStrategy,
) -> Result<usize, crate::SynthError> {
    tree_shape(sink_count, strategy.branching_factor).map(|(_, buffers)| buffers)
}

pub(super) fn buffer_branches(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    violation: &DesignRuleViolation,
) -> Result<Vec<Vec<PinId>>, crate::SynthError> {
    let Some(net) = violation.mapped_net else {
        return Ok(Vec::new());
    };
    let mut sinks = net_sink_pins(mapped, library, net)?;
    Ok(match violation.kind {
        DesignRuleKind::MaxFanout => {
            sinks.sort_by(|left, right| {
                right
                    .1
                    .total_cmp(&left.1)
                    .then_with(|| left.0.cmp(&right.0))
            });
            let mut branch = Vec::new();
            let mut branch_load = 0.0;
            for (pin, load) in sinks {
                if branch_load + load > violation.limit {
                    continue;
                }
                branch_load += load;
                branch.push(pin);
            }
            if branch.is_empty() {
                return Ok(Vec::new());
            }
            vec![branch]
        }
        DesignRuleKind::MaxTransition | DesignRuleKind::MaxCapacitance => {
            let mut ranked = sinks
                .into_iter()
                .map(|(pin, fanout)| {
                    let capacitance = library_pin(mapped, library, pin)?
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "electrical repair sink has no target-library pin identity",
                            )
                        })?
                        .design_input_capacitance();
                    Ok((pin, capacitance, fanout))
                })
                .collect::<Result<Vec<_>, crate::SynthError>>()?;
            ranked.sort_by(|left, right| {
                right
                    .1
                    .total_cmp(&left.1)
                    .then_with(|| right.2.total_cmp(&left.2))
                    .then_with(|| left.0.cmp(&right.0))
            });
            match ranked.first() {
                Some(&(pin, _, _)) => vec![vec![pin]],
                None => Vec::new(),
            }
        }
    })
}

pub(super) fn net_sink_pins(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    net: NetId,
) -> Result<Vec<(PinId, f64)>, crate::SynthError> {
    let Some(pins) = mapped.pins_on_net(net) else {
        return Ok(Vec::new());
    };
    let mut sinks = Vec::new();
    for pin in pins.collect::<Vec<_>>() {
        let Some(target) = library_pin(mapped, library, pin)? else {
            continue;
        };
        if matches!(
            target.direction(),
            TargetPinDirection::Input | TargetPinDirection::Inout
        ) {
            sinks.push((pin, target.design_fanout_load()));
        }
    }
    Ok(sinks)
}

pub(super) fn cell_input_pins(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    cell: CellId,
) -> Result<Vec<PinId>, crate::SynthError> {
    let Some(pins) = mapped.pin_ids(cell) else {
        return Ok(Vec::new());
    };
    let mut inputs = Vec::new();
    for pin in pins.collect::<Vec<_>>() {
        let connected_to_net = mapped
            .connection(pin)
            .is_some_and(|connection| matches!(connection.signal, ConnectionSignal::Net(_)));
        if !connected_to_net {
            continue;
        }
        let Some(target) = library_pin(mapped, library, pin)? else {
            continue;
        };
        if matches!(
            target.direction(),
            TargetPinDirection::Input | TargetPinDirection::Inout
        ) {
            inputs.push(pin);
        }
    }
    Ok(inputs)
}

pub(super) fn library_pin<'a>(
    mapped: &MappedNetlist,
    library: &'a TargetCellSet,
    pin: PinId,
) -> Result<Option<TargetPinRef<'a>>, crate::SynthError> {
    let cell = mapped.pin_owner(pin).ok_or_else(|| {
        crate::SynthError::invariant(format!("mapped pin {pin:?} has no live owner"))
    })?;
    let record = mapped
        .cell(cell)
        .ok_or_else(|| crate::SynthError::invariant(format!("mapped cell {cell:?} disappeared")))?;
    let Some(cell_index) = record.library_cell else {
        return Ok(None);
    };
    let target = library.get(cell_index as usize).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "mapped cell {cell:?} references unknown library cell {cell_index}"
        ))
    })?;
    let connection = mapped
        .connection(pin)
        .ok_or_else(|| crate::SynthError::invariant(format!("mapped pin {pin:?} disappeared")))?;
    let Some(pin_index) = connection.library_pin else {
        return Ok(None);
    };
    let target_pin = target.pins().nth(pin_index as usize).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "mapped pin {pin:?} references unknown library pin {pin_index} of '{}'",
            target.name()
        ))
    })?;
    Ok(Some(target_pin))
}

#[derive(Clone, Copy)]
struct BufferDescriptor<'a> {
    input_index: u16,
    input: TargetPinRef<'a>,
    output_index: u16,
    output: TargetPinRef<'a>,
    library_cell: u32,
}

fn buffer_descriptor(
    buffer: TargetCellRef<'_>,
    buffer_index: usize,
) -> Result<BufferDescriptor<'_>, crate::SynthError> {
    let (input_index, input) = buffer
        .pins()
        .enumerate()
        .find(|(_, pin)| pin.direction() == TargetPinDirection::Input)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!("buffer '{}' has no input", buffer.name()))
        })?;
    let (output_index, output) = buffer
        .pins()
        .enumerate()
        .find(|(_, pin)| pin.direction() == TargetPinDirection::Output)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!("buffer '{}' has no output", buffer.name()))
        })?;
    Ok(BufferDescriptor {
        input_index: u16::try_from(input_index)
            .map_err(|_| crate::SynthError::capacity("buffer input pin index exceeds capacity"))?,
        input,
        output_index: u16::try_from(output_index)
            .map_err(|_| crate::SynthError::capacity("buffer output pin index exceeds capacity"))?,
        output,
        library_cell: u32::try_from(buffer_index)
            .map_err(|_| crate::SynthError::capacity("buffer library index exceeds capacity"))?,
    })
}

struct FanoutTreeNode {
    output: TempNetId,
    input: Option<ConnectionRef>,
    leaf_sinks: Vec<PinId>,
}

fn net_driver_cells(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    library: &TargetCellSet,
    net: NetId,
) -> Result<Option<Box<[CellId]>>, crate::SynthError> {
    // An empty driver set denotes global topology. `None` is reserved for a
    // normal buffering candidate whose mapped drivers disagree on ownership.
    let mut drivers = Vec::new();
    let mut endpoint = None::<Option<RegionAnchorId>>;
    for pin in mapped.pins_on_net(net).into_iter().flatten() {
        if library_pin(mapped, library, pin)?
            .is_some_and(|pin| pin.direction() == TargetPinDirection::Output)
            && let Some(cell) = mapped.pin_owner(pin)
        {
            let candidate = implementations.ownership_endpoint(cell)?;
            if endpoint.is_some_and(|endpoint| endpoint != candidate) {
                return Ok(None);
            }
            endpoint = Some(candidate);
            drivers.push(cell);
        }
    }
    drivers.sort_unstable();
    drivers.dedup();
    Ok(Some(drivers.into_boxed_slice()))
}

pub(super) fn group_sink_pins_by_owner(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    sinks: impl IntoIterator<Item = PinId>,
) -> Result<Vec<(CellId, Vec<PinId>)>, crate::SynthError> {
    let mut groups = BTreeMap::<Option<RegionAnchorId>, (CellId, Vec<PinId>)>::new();
    for pin in sinks {
        let cell = mapped.pin_owner(pin).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "boundary sink pin {pin:?} has no live mapped owner"
            ))
        })?;
        let endpoint = implementations.ownership_endpoint(cell)?;
        let group = groups.entry(endpoint).or_insert_with(|| (cell, Vec::new()));
        group.0 = group.0.min(cell);
        group.1.push(pin);
    }
    Ok(groups
        .into_iter()
        .map(|(_, (cell, mut pins))| {
            pins.sort_unstable();
            (cell, pins)
        })
        .collect())
}

pub(super) fn fanout_forest_delta(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    library: &TargetCellSet,
    plans: &[FanoutTreePlan],
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
    if plans.is_empty() {
        return Err(crate::SynthError::invariant(
            "fanout forest requires at least one tree",
        ));
    }
    let mut prepared = Vec::with_capacity(plans.len());
    let mut seen_source_nets = BTreeSet::new();
    let mut sink_cells = Vec::new();
    let mut source_nets = Vec::with_capacity(plans.len());
    for plan in plans {
        if plan.strategy.branching_factor < 2
            || plan.sink_count() <= plan.strategy.branching_factor
            || plan.leaf_groups.is_empty()
            || plan
                .leaf_groups
                .iter()
                .any(|group| group.is_empty() || group.len() > plan.strategy.branching_factor)
        {
            return Err(crate::SynthError::invariant(
                "fanout-tree plan has an invalid buffered hierarchy",
            ));
        }
        if !seen_source_nets.insert(plan.net) {
            return Err(crate::SynthError::invariant(
                "fanout forest contains duplicate source nets",
            ));
        }
        let buffer = library.get(plan.strategy.buffer_index).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "unknown fanout-tree buffer library index {}",
                plan.strategy.buffer_index
            ))
        })?;
        let descriptor = buffer_descriptor(buffer, plan.strategy.buffer_index)?;
        let mut unique_sinks = BTreeSet::new();
        let mut plan_sink_cells = Vec::new();
        for &pin in plan.leaf_groups.iter().flatten() {
            if !unique_sinks.insert(pin) {
                return Err(crate::SynthError::invariant(
                    "fanout-tree plan assigns one sink to multiple leaf groups",
                ));
            }
            let cell = mapped.pin_owner(pin).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "fanout-tree sink pin {pin:?} has no live owner"
                ))
            })?;
            let ConnectionSignal::Net(net) = mapped
                .connection(pin)
                .ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "fanout-tree sink pin {pin:?} disappeared"
                    ))
                })?
                .signal
            else {
                return Err(crate::SynthError::invariant(format!(
                    "fanout-tree sink pin {pin:?} is not connected to a net"
                )));
            };
            if net != plan.net {
                return Err(crate::SynthError::invariant(
                    "fanout-tree plan sinks do not share its source net",
                ));
            }
            plan_sink_cells.push(cell);
        }
        let Some(drivers) = net_driver_cells(mapped, implementations, library, plan.net)? else {
            continue;
        };
        source_nets.push(plan.net);
        sink_cells.extend(plan_sink_cells);
        prepared.push((plan, drivers, buffer, descriptor));
    }
    if prepared.is_empty() {
        return Ok(None);
    }
    sink_cells.sort_unstable();
    sink_cells.dedup();
    source_nets.sort_unstable();
    source_nets.dedup();
    let snapshot = mapped
        .snapshot_region(sink_cells, source_nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    let mut added_cells = Vec::new();
    for (plan, drivers, buffer, descriptor) in prepared {
        let groups = group_sink_pins_by_owner(
            mapped,
            implementations,
            plan.leaf_groups.iter().flatten().copied(),
        )?;
        for (segment, (sink, pins)) in groups.into_iter().enumerate() {
            let leaf_groups = pins
                .chunks(plan.strategy.branching_factor)
                .map(<[PinId]>::to_vec)
                .collect::<Vec<_>>();
            added_cells.extend(
                append_fanout_tree(&mut delta, plan, segment, &leaf_groups, buffer, descriptor)?
                    .into_iter()
                    .map(|added| (added, drivers.clone(), sink)),
            );
        }
    }
    let mut candidate = PostmapCandidate::new(delta);
    for (added, drivers, sink) in added_cells {
        candidate = candidate.record_repair_segment(implementations, added, &drivers, sink)?;
    }
    Ok(Some(candidate))
}

pub(super) fn buffer_branch_forest_delta(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    library: &TargetCellSet,
    plans: &[BufferBranchPlan],
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
    if plans.is_empty() {
        return Err(crate::SynthError::invariant(
            "electrical buffer forest requires at least one branch",
        ));
    }
    let mut prepared = Vec::with_capacity(plans.len());
    let mut seen_source_nets = BTreeSet::new();
    let mut seen_sink_pins = BTreeSet::new();
    let mut source_nets = BTreeSet::new();
    let mut sink_cells = Vec::new();
    for plan in plans {
        if !seen_source_nets.insert(plan.net) {
            return Err(crate::SynthError::invariant(
                "electrical buffer forest contains multiple branches for one source net",
            ));
        }
        if plan.sinks.is_empty() {
            return Err(crate::SynthError::invariant(
                "electrical buffer branch has no sinks",
            ));
        }
        let buffer = library.get(plan.buffer_index).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "unknown electrical buffer library index {}",
                plan.buffer_index
            ))
        })?;
        let descriptor = buffer_descriptor(buffer, plan.buffer_index)?;
        let mut plan_sink_cells = Vec::new();
        for &pin in &plan.sinks {
            if !seen_sink_pins.insert(pin) {
                return Err(crate::SynthError::invariant(
                    "electrical buffer forest assigns one sink to multiple branches",
                ));
            }
            let cell = mapped.pin_owner(pin).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "electrical buffer sink pin {pin:?} has no live owner"
                ))
            })?;
            let connection = mapped.connection(pin).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "electrical buffer sink pin {pin:?} disappeared"
                ))
            })?;
            if connection.signal != ConnectionSignal::Net(plan.net) {
                return Err(crate::SynthError::invariant(
                    "electrical buffer branch sinks do not share its source net",
                ));
            }
            plan_sink_cells.push(cell);
        }
        let Some(drivers) = net_driver_cells(mapped, implementations, library, plan.net)? else {
            continue;
        };
        source_nets.insert(plan.net);
        sink_cells.extend(plan_sink_cells);
        prepared.push((plan, drivers, buffer, descriptor));
    }
    if prepared.is_empty() {
        return Ok(None);
    }
    sink_cells.sort_unstable();
    sink_cells.dedup();
    let snapshot = mapped
        .snapshot_region(sink_cells, source_nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    let mut added_cells = Vec::with_capacity(prepared.len());
    for (plan, drivers, buffer, descriptor) in prepared {
        let groups = group_sink_pins_by_owner(mapped, implementations, plan.sinks.iter().copied())?;
        let multiple = groups.len() > 1;
        for (segment, (sink, pins)) in groups.into_iter().enumerate() {
            let suffix = if multiple {
                format!("_{segment}")
            } else {
                String::new()
            };
            let new_net = delta
                .add_net(Some(format!("{}{suffix}", plan.net_name)))
                .map_err(crate::SynthError::from)?;
            let added = delta
                .add_cell(
                    CellSpec::new(
                        format!("{}{suffix}", plan.instance_name),
                        buffer.name(),
                        Some(descriptor.library_cell),
                    )
                    .connect(
                        descriptor.input.name(),
                        Some(descriptor.input_index),
                        ConnectionRef::Net(plan.net),
                    )
                    .connect(
                        descriptor.output.name(),
                        Some(descriptor.output_index),
                        ConnectionRef::NewNet(new_net),
                    ),
                )
                .map_err(crate::SynthError::from)?;
            for pin in pins {
                delta
                    .reconnect_pin(pin, ConnectionRef::NewNet(new_net))
                    .map_err(crate::SynthError::from)?;
            }
            added_cells.push((added, drivers.clone(), sink));
        }
    }
    let mut candidate = PostmapCandidate::new(delta);
    for (added, drivers, sink) in added_cells {
        candidate = candidate.record_repair_segment(implementations, added, &drivers, sink)?;
    }
    Ok(Some(candidate))
}

fn append_fanout_tree(
    delta: &mut RegionDelta,
    plan: &FanoutTreePlan,
    segment: usize,
    leaf_groups: &[Vec<PinId>],
    buffer: TargetCellRef<'_>,
    descriptor: BufferDescriptor<'_>,
) -> Result<Vec<opto_ir::mapped::TempCellId>, crate::SynthError> {
    let mut nodes = Vec::<FanoutTreeNode>::new();
    let mut current = Vec::new();
    for group in leaf_groups {
        let index = nodes.len();
        let output = delta
            .add_net(Some(format!(
                "_buffer_tree_{}_{}_{}_{index}",
                plan.namespace, plan.ordinal, segment
            )))
            .map_err(crate::SynthError::from)?;
        nodes.push(FanoutTreeNode {
            output,
            input: None,
            leaf_sinks: group.clone(),
        });
        current.push(index);
    }
    while current.len() > plan.strategy.branching_factor {
        let mut parents = Vec::new();
        for children in current.chunks(plan.strategy.branching_factor) {
            let index = nodes.len();
            let output = delta
                .add_net(Some(format!(
                    "_buffer_tree_{}_{}_{}_{index}",
                    plan.namespace, plan.ordinal, segment
                )))
                .map_err(crate::SynthError::from)?;
            for &child in children {
                nodes[child].input = Some(ConnectionRef::NewNet(output));
            }
            nodes.push(FanoutTreeNode {
                output,
                input: None,
                leaf_sinks: Vec::new(),
            });
            parents.push(index);
        }
        current = parents;
    }
    for root in current {
        nodes[root].input = Some(ConnectionRef::Net(plan.net));
    }

    let mut added_cells = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        let input = node.input.ok_or_else(|| {
            crate::SynthError::invariant("fanout-tree node has no parent connection")
        })?;
        let added = delta
            .add_cell(
                CellSpec::new(
                    format!(
                        "U_buffer_tree_{}_{}_{}_{index}",
                        plan.namespace, plan.ordinal, segment
                    ),
                    buffer.name(),
                    Some(descriptor.library_cell),
                )
                .connect(descriptor.input.name(), Some(descriptor.input_index), input)
                .connect(
                    descriptor.output.name(),
                    Some(descriptor.output_index),
                    ConnectionRef::NewNet(node.output),
                ),
            )
            .map_err(crate::SynthError::from)?;
        added_cells.push(added);
        for &pin in &node.leaf_sinks {
            delta
                .reconnect_pin(pin, ConnectionRef::NewNet(node.output))
                .map_err(crate::SynthError::from)?;
        }
    }
    Ok(added_cells)
}

#[cfg(test)]
mod planning_tests {
    use super::*;

    #[test]
    fn branching_search_scales_with_tree_depth_not_sink_count() {
        let sink_count = 1_000_000;
        let maximum_factor = sink_count - 1;
        let candidates = branching_factor_candidates(sink_count, maximum_factor).unwrap();
        let maximum_levels = tree_shape(sink_count, 2).unwrap().0;
        let minimum_levels = tree_shape(sink_count, maximum_factor).unwrap().0;

        assert_eq!(candidates.first(), Some(&2));
        assert_eq!(candidates.last(), Some(&maximum_factor));
        assert!(candidates.len() <= 2 * (maximum_levels - minimum_levels + 1));
        assert!(candidates.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
