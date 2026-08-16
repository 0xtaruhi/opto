// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::PostmapCandidate;
use crate::{ImplementationDb, TargetCellRef, TargetPinDirection};
use hashbrown::{HashMap, HashSet};
use opto_ir::mapped::{
    CellId, CellSpec, ConnectionRef, ConnectionSignal, MappedNetlist, NetId, PinId, RegionDelta,
};
use smallvec::SmallVec;

const WINDOW_INPUT_CAP: usize = 12;
const CELL_INPUT_CAP: usize = 6;
const STRUCTURAL_CELL_AREA_CAP: f64 = 4.0;
const WINDOW_CELL_CAP: usize = 24;

mod search;

use search::wire_replacement_for;

mod drivers;
pub(super) use drivers::DriverIndex;
use drivers::{
    Window, cell_output_pin, collect_window, debug_mfs, evaluate_cell, filled,
    sorted_candidate_nets, word_count,
};

#[derive(Clone, Copy)]
pub(super) struct OptimizationContext<'a> {
    pub(super) mapped: &'a MappedNetlist,
    pub(super) implementations: &'a ImplementationDb,
    pub(super) functions: &'a HashMap<String, CellFunction>,
    pub(super) resynthesis: &'a ResynthesisCells,
    pub(super) drivers: &'a DriverIndex,
    pub(super) boundary: &'a HashSet<NetId>,
    pub(super) diagnostics: bool,
}

#[derive(Debug)]
pub(super) struct CellFunction {
    inputs: Vec<String>,
    output: String,
    truth_bits: u64,
    input_count: usize,
    library_index: u32,
    area: f64,
    delay: f64,
    transition: f64,
}

#[derive(Debug)]
pub(super) struct ResynthesisCells {
    inverter: Option<ResynthesisCell>,
    by_input_count: [Vec<ResynthesisCell>; CELL_INPUT_CAP + 1],
}

#[derive(Debug)]
struct ResynthesisCell {
    name: String,
    index: u32,
    area: f64,
    delay: f64,
    transition: f64,
    truth: u64,
    pins: Vec<String>,
    output: String,
}

impl ResynthesisCell {
    fn precedes(&self, other: &Self) -> bool {
        self.area
            .total_cmp(&other.area)
            .then_with(|| self.delay.total_cmp(&other.delay))
            .then_with(|| self.transition.total_cmp(&other.transition))
            .then_with(|| self.name.cmp(&other.name))
            .is_lt()
    }
}

fn candidate_allowed(cell: &ResynthesisCell, current_area: f64) -> bool {
    // This is a deterministic search bound, not an acceptance objective. The
    // closure transaction decides whether a larger cell is justified.
    cell.area < current_area * STRUCTURAL_CELL_AREA_CAP
}

pub(super) fn cell_functions(
    library: &opto_library::TargetCellSet,
) -> HashMap<String, CellFunction> {
    let mut functions = HashMap::new();
    for (index, cell) in library.synthesis_cells() {
        if let Some(function) = single_output_function(
            cell,
            u32::try_from(index).expect("target-cell arena is bounded by compact cell IDs"),
        ) {
            functions.insert(cell.name().to_string(), function);
        }
    }
    functions
}

pub(super) fn resynthesis_cells(functions: &HashMap<String, CellFunction>) -> ResynthesisCells {
    let mut inverter: Option<ResynthesisCell> = None;
    let mut best_by_input = std::array::from_fn::<_, { CELL_INPUT_CAP + 1 }, _>(|_| {
        HashMap::<u64, ResynthesisCell>::new()
    });
    for (name, function) in functions {
        if function.input_count == 1 && function.truth_bits & 0b11 == 0b01 {
            let candidate = resynthesis_cell(name, function, function.truth_bits & 0b11);
            if inverter
                .as_ref()
                .is_none_or(|current| candidate.precedes(current))
            {
                inverter = Some(candidate);
            }
        }
        if (2..=CELL_INPUT_CAP).contains(&function.input_count) {
            let truth = function.truth_bits & truth_mask(function.input_count);
            let entry = best_by_input[function.input_count].entry(truth);
            let candidate = resynthesis_cell(name, function, truth);
            match entry {
                hashbrown::hash_map::Entry::Occupied(mut occupied) => {
                    if candidate.precedes(occupied.get()) {
                        occupied.insert(candidate);
                    }
                }
                hashbrown::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(candidate);
                }
            }
        }
    }
    let by_input_count = best_by_input.map(|by_truth| {
        let mut cells = by_truth
            .into_iter()
            .map(|(_, cell)| cell)
            .collect::<Vec<_>>();
        cells.sort_by(|left, right| {
            left.truth
                .cmp(&right.truth)
                .then_with(|| left.name.cmp(&right.name))
        });
        cells
    });
    ResynthesisCells {
        inverter,
        by_input_count,
    }
}

fn truth_mask(input_count: usize) -> u64 {
    let bits = 1usize << input_count;
    if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

fn resynthesis_cell(name: &str, function: &CellFunction, truth: u64) -> ResynthesisCell {
    ResynthesisCell {
        name: name.to_string(),
        index: function.library_index,
        area: function.area,
        delay: function.delay,
        transition: function.transition,
        truth,
        pins: function.inputs.clone(),
        output: function.output.clone(),
    }
}

fn single_output_function(cell: TargetCellRef<'_>, library_index: u32) -> Option<CellFunction> {
    if cell.sequential().next().is_some() {
        return None;
    }
    let inputs = cell
        .pins()
        .filter(|pin| pin.direction() == TargetPinDirection::Input)
        .collect::<Vec<_>>();
    if inputs.len() > CELL_INPUT_CAP {
        return None;
    }
    let mut outputs = cell
        .pins()
        .filter(|pin| pin.direction() == TargetPinDirection::Output && pin.three_state().is_none());
    let output = outputs.next()?;
    if outputs.next().is_some() {
        return None;
    }
    let input_names = inputs.iter().map(|pin| pin.name()).collect::<Vec<_>>();
    let truth_bits = output.function()?.truth_table_bits(&input_names)?;
    let area = cell
        .area()
        .filter(|area| area.is_finite() && *area >= 0.0)?;
    let delay = output
        .timing_arcs()
        .filter_map(opto_library::TargetTimingArcRef::default_delay)
        .max_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY);
    let transition = output
        .timing_arcs()
        .filter_map(opto_library::TargetTimingArcRef::default_transition)
        .max_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY);
    Some(CellFunction {
        inputs: inputs.iter().map(|pin| pin.name().to_string()).collect(),
        output: output.name().to_string(),
        truth_bits,
        input_count: inputs.len(),
        library_index,
        area,
        delay,
        transition,
    })
}

pub(super) fn optimization_boundary_nets(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
) -> Result<HashSet<NetId>, crate::SynthError> {
    let mut boundary = mapped
        .immutable_boundary_nets()
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    for net in mapped.net_ids() {
        if boundary.contains(&net) {
            continue;
        }
        let Some(pins) = mapped.pins_on_net(net) else {
            continue;
        };
        let mut first = None;
        for cell in pins.filter_map(|pin| mapped.pin_owner(pin)) {
            if let Some(first) = first
                && !implementations.cells_share_owner(first, cell)?
            {
                boundary.insert(net);
                break;
            }
            first = Some(cell);
        }
    }
    Ok(boundary)
}

pub(super) fn optimization_candidate(
    context: OptimizationContext<'_>,
    cell: CellId,
) -> Option<PostmapCandidate> {
    optimization_candidate_reading(context, cell, &mut Vec::new())
}

/// Derives one candidate and records the reads needed to invalidate a miss.
pub(super) fn optimization_candidate_reading(
    context: OptimizationContext<'_>,
    cell: CellId,
    read: &mut Vec<CellId>,
) -> Option<PostmapCandidate> {
    read.clear();
    dead_cell_candidate(context.mapped, context.functions, context.boundary, cell)
        .or_else(|| wire_replacement_for(context, cell, read))
}

const ODC_DEPTH: usize = 5;
const ODC_CONE_CAP: usize = 32;

fn observability_care(
    mapped: &MappedNetlist,
    functions: &HashMap<String, CellFunction>,
    boundary: &HashSet<NetId>,
    window: &Window,
    out_net: NetId,
    output_pin: PinId,
    input_count: usize,
) -> Option<(Vec<u64>, Vec<CellId>)> {
    let mut cone_cells = Vec::new();
    let mut cone_nets = vec![out_net];
    let mut frontier = vec![out_net];
    for _ in 0..ODC_DEPTH {
        let mut next = Vec::new();
        for &net in &frontier {
            for pin in mapped.pins_on_net(net)? {
                if pin == output_pin {
                    continue;
                }
                let Some(owner) = mapped.pin_owner(pin) else {
                    continue;
                };
                if cone_cells.contains(&owner) || cone_cells.len() >= ODC_CONE_CAP {
                    continue;
                }
                let Some(owner_function) =
                    mapped.cell_type(owner).and_then(|name| functions.get(name))
                else {
                    continue;
                };
                let Some((_, owner_out)) = cell_output_pin(mapped, owner, owner_function) else {
                    continue;
                };
                if boundary.contains(&owner_out) {
                    continue;
                }
                let ready = mapped.connections(owner).is_some_and(|connections| {
                    connections
                        .iter()
                        .all(|connection| match connection.signal {
                            ConnectionSignal::Net(input_net) => {
                                window.bits.contains_key(&input_net)
                                    || cone_nets.contains(&input_net)
                                    || input_net == owner_out
                            }
                            ConnectionSignal::Constant(_) => true,
                        })
                });
                if !ready {
                    continue;
                }
                cone_cells.push(owner);
                cone_nets.push(owner_out);
                next.push(owner_out);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    let words = word_count(input_count);
    let mut low_bits = window.bits.clone();
    low_bits.insert(out_net, filled(false, input_count));
    let mut high_bits = window.bits.clone();
    high_bits.insert(out_net, filled(true, input_count));
    for &cone_cell in &cone_cells {
        let function = functions.get(mapped.cell_type(cone_cell)?)?;
        let (_, cone_out) = cell_output_pin(mapped, cone_cell, function)?;
        let low = evaluate_cell(mapped, cone_cell, function, &low_bits, None, input_count)?;
        let high = evaluate_cell(mapped, cone_cell, function, &high_bits, None, input_count)?;
        low_bits.insert(cone_out, low);
        high_bits.insert(cone_out, high);
    }

    let mut care = vec![0u64; words];
    for &net in &cone_nets {
        let observed = if boundary.contains(&net) {
            true
        } else {
            let mut observed = false;
            for pin in mapped.pins_on_net(net)? {
                if pin == output_pin {
                    continue;
                }
                let Some(owner) = mapped.pin_owner(pin) else {
                    observed = true;
                    break;
                };
                if mapped
                    .connections(owner)
                    .and_then(|connections| {
                        connections
                            .iter()
                            .find_map(|connection| match connection.signal {
                                ConnectionSignal::Net(candidate) if candidate == net => {
                                    mapped.pin_name(connection)
                                }
                                _ => None,
                            })
                    })
                    .is_none()
                {
                    observed = true;
                    break;
                }
                if !cone_cells.contains(&owner) {
                    observed = true;
                    break;
                }
            }
            observed
        };
        if !observed {
            continue;
        }
        let (Some(low), Some(high)) = (low_bits.get(&net), high_bits.get(&net)) else {
            return None;
        };
        for (care, (low, high)) in care.iter_mut().zip(low.iter().zip(high.iter())) {
            *care |= low ^ high;
        }
    }
    Some((care, cone_cells))
}

fn driver_mffc(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    functions: &HashMap<String, CellFunction>,
    boundary: &HashSet<NetId>,
    root: CellId,
    keep_nets: &[NetId],
) -> Vec<(CellId, NetId, f64)> {
    driver_mffc_reading(
        mapped,
        implementations,
        functions,
        boundary,
        root,
        keep_nets,
        &mut Vec::new(),
    )
}

/// Grows the cone and records every cell that influenced the decision.
fn driver_mffc_reading(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    functions: &HashMap<String, CellFunction>,
    boundary: &HashSet<NetId>,
    root: CellId,
    keep_nets: &[NetId],
    inspected: &mut Vec<CellId>,
) -> Vec<(CellId, NetId, f64)> {
    inspected.push(root);
    let mut dying_cells = vec![root];
    let mut dying = Vec::new();
    let mut changed = true;
    // Bound both the transaction snapshot and the formal proof cone.
    while changed && dying.len() < 8 {
        changed = false;
        let mut inputs = Vec::new();
        for &cell in &dying_cells {
            if let Some(connections) = mapped.connections(cell) {
                for connection in connections {
                    if let ConnectionSignal::Net(net) = connection.signal {
                        inputs.push(net);
                    }
                }
            }
        }
        for net in inputs {
            if boundary.contains(&net)
                || keep_nets.contains(&net)
                || dying.iter().any(|&(_, dead, _)| dead == net)
            {
                continue;
            }
            if let Some(pins) = mapped.pins_on_net(net) {
                inspected.extend(pins.filter_map(|pin| mapped.pin_owner(pin)));
            }
            let Some(driver) = mapped.pins_on_net(net).and_then(|mut pins| {
                pins.find_map(|pin| {
                    let owner = mapped.pin_owner(pin)?;
                    let function = functions.get(mapped.cell_type(owner)?)?;
                    let (output_pin, output_net) = cell_output_pin(mapped, owner, function)?;
                    (output_pin == pin && output_net == net).then_some((owner, function.area))
                })
            }) else {
                continue;
            };
            if dying_cells.contains(&driver.0) {
                continue;
            }
            if !implementations
                .cells_share_owner(root, driver.0)
                .unwrap_or(false)
            {
                continue;
            }
            let Some(pins) = mapped.pins_on_net(net) else {
                continue;
            };
            let all_dying = pins
                .filter_map(|pin| mapped.pin_owner(pin))
                .all(|owner| owner == driver.0 || dying_cells.contains(&owner));
            if !all_dying {
                continue;
            }
            dying_cells.push(driver.0);
            dying.push((driver.0, net, driver.1));
            changed = true;
        }
    }
    dying
}

pub(super) fn dead_cell_candidate(
    mapped: &MappedNetlist,
    functions: &HashMap<String, CellFunction>,
    boundary: &HashSet<NetId>,
    cell: CellId,
) -> Option<PostmapCandidate> {
    if !mapped.is_live_cell(cell) {
        return None;
    }
    let function = mapped
        .cell_type(cell)
        .and_then(|name| functions.get(name))?;
    let (output_pin, out_net) = cell_output_pin(mapped, cell, function)?;
    if boundary.contains(&out_net) {
        return None;
    }
    if mapped.pins_on_net(out_net)?.any(|pin| pin != output_pin) {
        return None;
    }
    let nets = super::mapped_cell_nets(mapped, [cell]).ok()?;
    let snapshot = mapped.snapshot_region([cell], nets).ok()?;
    let mut delta = RegionDelta::new(snapshot);
    delta.remove_cell(cell).ok()?;
    delta.remove_net(out_net).ok()?;
    Some(PostmapCandidate::new(delta))
}

/// Builds one transaction that removes structurally dead cells to a fixpoint.
pub(super) fn dead_cell_removal(
    mapped: &MappedNetlist,
    functions: &HashMap<String, CellFunction>,
    boundary: &HashSet<NetId>,
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
    // A cell this pass cannot interpret is never removed, and every net it
    // touches counts as read: an unknown output pin must not look like a dead
    // net just because nothing else drives it.
    let mut outputs = HashMap::new();
    for cell in mapped.cell_ids() {
        let output = mapped
            .cell_type(cell)
            .and_then(|name| functions.get(name))
            .and_then(|function| cell_output_pin(mapped, cell, function));
        if let Some((pin, net)) = output {
            outputs.insert(cell, (pin, net));
        }
    }
    let mut readers: HashMap<NetId, usize> = HashMap::new();
    let mut drivers: HashMap<NetId, CellId> = HashMap::new();
    for cell in mapped.cell_ids() {
        let output_pin = outputs.get(&cell).map(|&(pin, _)| pin);
        if let Some(&(_, net)) = outputs.get(&cell) {
            drivers.insert(net, cell);
        }
        let Some(pins) = mapped.pin_ids(cell) else {
            continue;
        };
        for pin in pins {
            if Some(pin) == output_pin {
                continue;
            }
            let Some(connection) = mapped.connection(pin) else {
                continue;
            };
            if let ConnectionSignal::Net(net) = connection.signal {
                *readers.entry(net).or_default() += 1;
            }
        }
    }

    let removable = |cell: CellId, readers: &HashMap<NetId, usize>| {
        outputs.get(&cell).is_some_and(|&(_, net)| {
            !boundary.contains(&net) && readers.get(&net).copied().unwrap_or(0) == 0
        })
    };
    let mut dead = Vec::new();
    let mut pending = mapped
        .cell_ids()
        .filter(|&cell| removable(cell, &readers))
        .collect::<Vec<_>>();
    let mut removed = HashSet::new();
    while let Some(cell) = pending.pop() {
        if !removed.insert(cell) {
            continue;
        }
        dead.push(cell);
        let output_pin = outputs.get(&cell).map(|&(pin, _)| pin);
        let Some(pins) = mapped.pin_ids(cell) else {
            continue;
        };
        for pin in pins {
            if Some(pin) == output_pin {
                continue;
            }
            let Some(connection) = mapped.connection(pin) else {
                continue;
            };
            let ConnectionSignal::Net(net) = connection.signal else {
                continue;
            };
            let Some(count) = readers.get_mut(&net) else {
                continue;
            };
            *count -= 1;
            if *count == 0
                && let Some(&driver) = drivers.get(&net)
                && !removed.contains(&driver)
                && removable(driver, &readers)
            {
                pending.push(driver);
            }
        }
    }
    if dead.is_empty() {
        return Ok(None);
    }
    // Cell order is the stable arena order, so the delta is identical across
    // worker counts even though the worklist pops in discovery order.
    dead.sort_unstable();
    let nets = super::mapped_cell_nets(mapped, dead.iter().copied())?;
    let snapshot = mapped
        .snapshot_region(dead.clone(), nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    for cell in dead {
        delta.remove_cell(cell).map_err(crate::SynthError::from)?;
        if let Some(&(_, net)) = outputs.get(&cell) {
            delta.remove_net(net).map_err(crate::SynthError::from)?;
        }
    }
    Ok(Some(PostmapCandidate::new(delta)))
}

#[derive(Clone, Copy)]
struct DriverReplacement<'a> {
    cell: CellId,
    out_net: NetId,
    cell_name: &'a str,
    library_index: u32,
    output_pin: &'a str,
    inputs: &'a [(&'a str, NetId)],
    dying: &'a [(CellId, NetId, f64)],
}

fn replace_driver_candidate(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    replacement: DriverReplacement<'_>,
) -> Result<PostmapCandidate, crate::SynthError> {
    let DriverReplacement {
        cell,
        out_net,
        cell_name,
        library_index,
        output_pin,
        inputs,
        dying,
    } = replacement;
    let protected_nets = inputs.iter().map(|&(_, net)| net).collect::<Vec<_>>();
    let dying = closed_dying_cone(mapped, cell, &protected_nets, dying);
    let sources = std::iter::once(cell)
        .chain(dying.iter().map(|&(cell, _, _)| cell))
        .collect::<Vec<_>>();
    if sources.iter().any(|&source| {
        !implementations
            .cells_share_owner(cell, source)
            .unwrap_or(false)
    }) {
        return Err(crate::SynthError::invariant(
            "MFS replacement crossed an implementation-owner boundary",
        ));
    }
    let cells = std::iter::once(cell)
        .chain(dying.iter().map(|&(dying_cell, _, _)| dying_cell))
        .collect::<Vec<_>>();
    let mut nets = super::mapped_cell_nets(mapped, cells.iter().copied())?;
    nets.extend(std::iter::once(out_net));
    nets.extend(inputs.iter().map(|&(_, net)| net));
    nets.extend(dying.iter().map(|&(_, net, _)| net));
    let snapshot = mapped
        .snapshot_region(cells, nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    let instance = format!("mfs_{}", out_net.index());
    let mut spec = CellSpec::new(&instance, cell_name, Some(library_index));
    for &(pin, net) in inputs {
        spec = spec.connect(pin, None, ConnectionRef::Net(net));
    }
    spec = spec.connect(output_pin, None, ConnectionRef::Net(out_net));
    let added = delta.add_cell(spec).map_err(crate::SynthError::from)?;
    delta.remove_cell(cell).map_err(crate::SynthError::from)?;
    for &(dying_cell, dying_net, _) in &dying {
        delta
            .remove_cell(dying_cell)
            .map_err(crate::SynthError::from)?;
        delta
            .remove_net(dying_net)
            .map_err(crate::SynthError::from)?;
    }
    PostmapCandidate::new(delta).record_added_cell(
        added,
        sources.iter().copied(),
        sources.iter().copied(),
    )
}

#[derive(Clone, Copy)]
struct RewireReplacement<'a> {
    cell: CellId,
    out_net: NetId,
    consumers: &'a [PinId],
    target: ConnectionRef,
    target_net: Option<NetId>,
    dying: &'a [(CellId, NetId, f64)],
}

fn rewire_candidate(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    replacement: RewireReplacement<'_>,
) -> Result<PostmapCandidate, crate::SynthError> {
    let RewireReplacement {
        cell,
        out_net,
        consumers,
        target,
        target_net,
        dying,
    } = replacement;
    let protected_nets = target_net.into_iter().collect::<Vec<_>>();
    let dying = closed_dying_cone(mapped, cell, &protected_nets, dying);
    if dying.iter().any(|&(dying, _, _)| {
        !implementations
            .cells_share_owner(cell, dying)
            .unwrap_or(false)
    }) {
        return Err(crate::SynthError::invariant(
            "MFS rewire crossed an implementation-owner boundary",
        ));
    }
    let mut cells = vec![cell];
    for &pin in consumers {
        if let Some(owner) = mapped.pin_owner(pin)
            && !cells.contains(&owner)
        {
            cells.push(owner);
        }
    }
    for &(dying_cell, _, _) in &dying {
        if !cells.contains(&dying_cell) {
            cells.push(dying_cell);
        }
    }
    let mut nets = super::mapped_cell_nets(mapped, cells.iter().copied())?;
    nets.extend(std::iter::once(out_net));
    nets.extend(target_net);
    nets.extend(dying.iter().map(|&(_, net, _)| net));
    let snapshot = mapped
        .snapshot_region(cells, nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    for &pin in consumers {
        delta
            .reconnect_pin(pin, target)
            .map_err(crate::SynthError::from)?;
    }
    delta.remove_cell(cell).map_err(crate::SynthError::from)?;
    delta.remove_net(out_net).map_err(crate::SynthError::from)?;
    for &(dying_cell, dying_net, _) in &dying {
        delta
            .remove_cell(dying_cell)
            .map_err(crate::SynthError::from)?;
        delta
            .remove_net(dying_net)
            .map_err(crate::SynthError::from)?;
    }
    Ok(PostmapCandidate::new(delta))
}

fn closed_dying_cone(
    mapped: &MappedNetlist,
    root: CellId,
    protected_nets: &[NetId],
    candidates: &[(CellId, NetId, f64)],
) -> Vec<(CellId, NetId, f64)> {
    let mut retained = vec![true; candidates.len()];
    // Removing one candidate can expose an external consumer of another.
    // Compute the greatest closed subset by monotonically deleting candidates
    // until every retained net is consumed exclusively inside the removal set.
    loop {
        let removed_cells = std::iter::once(root)
            .chain(
                candidates
                    .iter()
                    .zip(&retained)
                    .filter_map(|(&(cell, _, _), &retain)| retain.then_some(cell)),
            )
            .collect::<HashSet<_>>();
        let mut changed = false;
        for (index, &(_, net, _)) in candidates.iter().enumerate() {
            if !retained[index] {
                continue;
            }
            let closed = !protected_nets.contains(&net)
                && mapped.pins_on_net(net).is_some_and(|pins| {
                    pins.filter_map(|pin| mapped.pin_owner(pin))
                        .all(|owner| removed_cells.contains(&owner))
                });
            if !closed {
                retained[index] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    candidates
        .iter()
        .zip(retained)
        .filter_map(|(&candidate, retain)| retain.then_some(candidate))
        .collect()
}

#[cfg(test)]
mod tests;
