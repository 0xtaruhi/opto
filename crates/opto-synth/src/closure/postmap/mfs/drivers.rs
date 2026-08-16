// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Driver index and resynthesis window search for post-map MFS.

use super::{
    CELL_INPUT_CAP, CellFunction, CellId, ConnectionSignal, HashMap, HashSet, MappedNetlist, NetId,
    PinId, SmallVec, WINDOW_CELL_CAP, WINDOW_INPUT_CAP,
};

pub(super) struct Window {
    pub(super) inputs: SmallVec<[NetId; WINDOW_INPUT_CAP]>,
    pub(super) bits: HashMap<NetId, Vec<u64>>,
    pub(super) cells: SmallVec<[CellId; WINDOW_CELL_CAP]>,
}

#[derive(Debug)]
pub(in crate::closure::postmap) struct DriverIndex {
    by_net: Vec<Option<CellId>>,
}

impl DriverIndex {
    pub(in crate::closure::postmap) fn build(
        mapped: &MappedNetlist,
        functions: &HashMap<String, CellFunction>,
    ) -> Self {
        let mut by_net = Vec::new();
        for cell in mapped.cell_ids() {
            let Some(function) = mapped.cell_type(cell).and_then(|name| functions.get(name)) else {
                continue;
            };
            let Some((_, net)) = cell_output_pin(mapped, cell, function) else {
                continue;
            };
            if by_net.len() <= net.index() {
                by_net.resize(net.index() + 1, None);
            }
            by_net[net.index()] = Some(cell);
        }
        Self { by_net }
    }

    pub(super) fn driver(&self, mapped: &MappedNetlist, net: NetId) -> Option<CellId> {
        self.by_net
            .get(net.index())
            .copied()
            .flatten()
            .filter(|&cell| mapped.is_live_cell(cell))
    }

    pub(in crate::closure::postmap) fn refresh(
        &mut self,
        mapped: &MappedNetlist,
        functions: &HashMap<String, CellFunction>,
        nets: impl IntoIterator<Item = NetId>,
    ) {
        for net in nets {
            if self.by_net.len() <= net.index() {
                self.by_net.resize(net.index() + 1, None);
            }
            self.by_net[net.index()] = output_driver(mapped, functions, net);
        }
    }
}

pub(super) fn sorted_candidate_nets<'a>(
    bits: &'a HashMap<NetId, Vec<u64>>,
    tainted: &HashSet<NetId>,
) -> Vec<(NetId, &'a [u64])> {
    let mut nets = bits
        .iter()
        .filter(|(net, _)| !tainted.contains(*net))
        .map(|(&net, bits)| (net, bits.as_slice()))
        .collect::<Vec<_>>();
    nets.sort_by_key(|(net, _)| net.index());
    nets
}

pub(super) fn word_count(input_count: usize) -> usize {
    (1usize << input_count).div_ceil(64)
}

pub(super) fn variable_bits(index: usize, input_count: usize) -> Vec<u64> {
    let assignments = 1usize << input_count;
    let mut bits = vec![0u64; word_count(input_count)];
    for assignment in 0..assignments {
        if assignment & (1 << index) != 0 {
            bits[assignment / 64] |= 1 << (assignment % 64);
        }
    }
    bits
}

pub(super) fn filled(value: bool, input_count: usize) -> Vec<u64> {
    let assignments = 1usize << input_count;
    let mut bits = vec![0u64; word_count(input_count)];
    if value {
        for assignment in 0..assignments {
            bits[assignment / 64] |= 1 << (assignment % 64);
        }
    }
    bits
}

pub(super) fn cell_output_pin(
    mapped: &MappedNetlist,
    cell: CellId,
    function: &CellFunction,
) -> Option<(PinId, NetId)> {
    for pin in mapped.pin_ids(cell)? {
        let connection = mapped.connection(pin)?;
        let name = mapped.pin_name(connection)?;
        if !function.inputs.iter().any(|input| input == name)
            && let ConnectionSignal::Net(net) = connection.signal
        {
            return Some((pin, net));
        }
    }
    None
}

pub(super) fn evaluate_cell(
    mapped: &MappedNetlist,
    cell: CellId,
    function: &CellFunction,
    bits: &HashMap<NetId, Vec<u64>>,
    forced: Option<(NetId, &[u64])>,
    input_count: usize,
) -> Option<Vec<u64>> {
    let words = word_count(input_count);
    let mut input_bits: Vec<Vec<u64>> = vec![vec![0u64; words]; CELL_INPUT_CAP];
    let connections = mapped.connections(cell)?;
    for connection in connections {
        let name = mapped.pin_name(connection)?;
        let Some(position) = function.inputs.iter().position(|input| input == name) else {
            continue;
        };
        input_bits[position] = match connection.signal {
            ConnectionSignal::Net(net) => match forced {
                Some((forced_net, forced_bits)) if forced_net == net => forced_bits.to_vec(),
                _ => bits.get(&net)?.clone(),
            },
            ConnectionSignal::Constant(value) => filled(value, input_count),
        };
    }
    let mut output = vec![0u64; words];
    for assignment in 0..1usize << input_count {
        let word = assignment / 64;
        let bit = 1u64 << (assignment % 64);
        let mut cell_assignment = 0usize;
        for (position, input) in input_bits.iter().enumerate().take(function.input_count) {
            if input[word] & bit != 0 {
                cell_assignment |= 1 << position;
            }
        }
        if function.truth_bits & (1u64 << cell_assignment) != 0 {
            output[word] |= bit;
        }
    }
    Some(output)
}

pub(super) fn collect_window(
    mapped: &MappedNetlist,
    functions: &HashMap<String, CellFunction>,
    drivers: &DriverIndex,
    roots: &[NetId],
) -> Option<Window> {
    let mut inputs = SmallVec::new();
    let mut cells = SmallVec::new();
    let mut states = HashMap::<NetId, bool>::new();
    let mut stack = roots
        .iter()
        .rev()
        .map(|&root| (root, false))
        .collect::<Vec<_>>();
    // Use an explicit post-order DFS: the first visit schedules fanins, while
    // the expanded visit appends the driver after its inputs. Nets beyond the
    // bounded cell cone become formal window inputs instead of making search
    // complexity depend on the complete fanin graph.
    while let Some((net, expanded)) = stack.pop() {
        if states.contains_key(&net) && !expanded {
            continue;
        }
        let driver = drivers.driver(mapped, net);
        let function = driver
            .and_then(|cell| mapped.cell_type(cell))
            .and_then(|name| functions.get(name));
        let (Some(driver), Some(function)) = (driver, function) else {
            states.insert(net, true);
            if !inputs.contains(&net) {
                if inputs.len() == WINDOW_INPUT_CAP {
                    return None;
                }
                inputs.push(net);
            }
            continue;
        };
        if expanded {
            states.insert(net, true);
            if cells.len() >= WINDOW_CELL_CAP {
                if !inputs.contains(&net) {
                    if inputs.len() == WINDOW_INPUT_CAP {
                        return None;
                    }
                    inputs.push(net);
                }
            } else {
                cells.push(driver);
            }
            continue;
        }
        if cells.len() >= WINDOW_CELL_CAP {
            states.insert(net, true);
            if !inputs.contains(&net) {
                if inputs.len() == WINDOW_INPUT_CAP {
                    return None;
                }
                inputs.push(net);
            }
            continue;
        }
        states.insert(net, false);
        stack.push((net, true));
        for connection in mapped.connections(driver)? {
            let name = mapped.pin_name(connection)?;
            if !function.inputs.iter().any(|input| input == name) {
                continue;
            }
            if let ConnectionSignal::Net(input) = connection.signal
                && !states.contains_key(&input)
            {
                stack.push((input, false));
            }
        }
    }
    let input_count = inputs.len();
    let mut bits = HashMap::new();
    for (index, &input) in inputs.iter().enumerate() {
        bits.insert(input, variable_bits(index, input_count));
    }
    for &cell in &cells {
        let function = functions.get(mapped.cell_type(cell)?)?;
        let (_, output) = cell_output_pin(mapped, cell, function)?;
        let value = evaluate_cell(mapped, cell, function, &bits, None, input_count)?;
        bits.insert(output, value);
    }
    Some(Window {
        inputs,
        bits,
        cells,
    })
}

pub(super) fn output_driver(
    mapped: &MappedNetlist,
    functions: &HashMap<String, CellFunction>,
    net: NetId,
) -> Option<CellId> {
    mapped.pins_on_net(net)?.find_map(|pin| {
        let owner = mapped.pin_owner(pin)?;
        let function = functions.get(mapped.cell_type(owner)?)?;
        let (output, output_net) = cell_output_pin(mapped, owner, function)?;
        (output == pin && output_net == net).then_some(owner)
    })
}

pub(super) fn debug_mfs(kind: &str, enabled: bool) {
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::new(enabled),
        "postmap.mfs.hit",
        "kind={kind}"
    );
}
