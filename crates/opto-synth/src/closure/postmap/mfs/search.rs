// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CELL_INPUT_CAP, CellFunction, CellId, ConnectionRef, ConnectionSignal, DriverReplacement,
    HashMap, HashSet, ImplementationDb, MappedNetlist, NetId, OptimizationContext, PinId,
    PostmapCandidate, ResynthesisCell, ResynthesisCells, RewireReplacement, SmallVec,
    cell_output_pin, collect_window, debug_mfs, driver_mffc, driver_mffc_reading, filled,
    observability_care, replace_driver_candidate, rewire_candidate, sorted_candidate_nets,
    truth_mask,
};

const DIRECT_REMAP_NET_CAP: usize = 16;
type PinBindings<'a> = SmallVec<[(&'a str, NetId); CELL_INPUT_CAP]>;

struct WireReplacementSearch<'a> {
    mapped: &'a MappedNetlist,
    implementations: &'a ImplementationDb,
    functions: &'a HashMap<String, CellFunction>,
    resynthesis: &'a ResynthesisCells,
    boundary: &'a HashSet<NetId>,
    cell: CellId,
    out_net: NetId,
    consumers: &'a [PinId],
    guard: &'a [CellId],
    out_bits: &'a [u64],
    care: &'a [u64],
    full_words: &'a [u64],
    nets: &'a [(NetId, &'a [u64])],
    current_area: f64,
    debug: bool,
}

impl WireReplacementSearch<'_> {
    fn find(&self) -> Option<PostmapCandidate> {
        self.constant_or_wire()
            .or_else(|| self.inverter())
            .or_else(|| self.direct_cell())
    }

    fn guarded(&self, candidate: PostmapCandidate) -> PostmapCandidate {
        candidate.with_guard(self.guard.to_vec())
    }

    fn constant_or_wire(&self) -> Option<PostmapCandidate> {
        if self.boundary.contains(&self.out_net) {
            return None;
        }
        let constant = if self
            .out_bits
            .iter()
            .zip(self.care)
            .all(|(out, care)| out & care == 0)
        {
            Some(false)
        } else if self
            .out_bits
            .iter()
            .zip(self.full_words.iter().zip(self.care))
            .all(|(out, (full, care))| (out ^ full) & care == 0)
        {
            Some(true)
        } else {
            None
        };
        if let Some(value) = constant {
            debug_mfs(if value { "const1" } else { "const0" }, self.debug);
            return rewire_candidate(
                self.mapped,
                self.implementations,
                RewireReplacement {
                    cell: self.cell,
                    out_net: self.out_net,
                    consumers: self.consumers,
                    target: ConnectionRef::Constant(value),
                    target_net: None,
                    dying: &driver_mffc(
                        self.mapped,
                        self.implementations,
                        self.functions,
                        self.boundary,
                        self.cell,
                        &[],
                    ),
                },
            )
            .ok()
            .map(|candidate| self.guarded(candidate));
        }
        for &(candidate_net, candidate_bits) in self.nets {
            if candidate_bits
                .iter()
                .zip(self.out_bits)
                .zip(self.care)
                .any(|((candidate, out), care)| (candidate ^ out) & care != 0)
            {
                continue;
            }
            debug_mfs("wire", self.debug);
            return rewire_candidate(
                self.mapped,
                self.implementations,
                RewireReplacement {
                    cell: self.cell,
                    out_net: self.out_net,
                    consumers: self.consumers,
                    target: ConnectionRef::Net(candidate_net),
                    target_net: Some(candidate_net),
                    dying: &driver_mffc(
                        self.mapped,
                        self.implementations,
                        self.functions,
                        self.boundary,
                        self.cell,
                        &[candidate_net],
                    ),
                },
            )
            .ok()
            .map(|candidate| self.guarded(candidate));
        }
        None
    }

    fn inverter(&self) -> Option<PostmapCandidate> {
        let inverter = self.resynthesis.inverter.as_ref()?;
        if !self.resynthesis.allows(inverter, self.current_area) {
            return None;
        }
        for &(candidate_net, candidate_bits) in self.nets {
            if candidate_bits
                .iter()
                .zip(
                    self.full_words
                        .iter()
                        .zip(self.out_bits.iter().zip(self.care)),
                )
                .any(|(candidate, (full, (out, care)))| ((candidate ^ full) ^ out) & care != 0)
            {
                continue;
            }
            debug_mfs("inv", self.debug);
            return replace_driver_candidate(
                self.mapped,
                self.implementations,
                DriverReplacement {
                    cell: self.cell,
                    out_net: self.out_net,
                    cell_name: &inverter.name,
                    library_index: inverter.index,
                    output_pin: &inverter.output,
                    inputs: &[(inverter.pins[0].as_str(), candidate_net)],
                    dying: &driver_mffc(
                        self.mapped,
                        self.implementations,
                        self.functions,
                        self.boundary,
                        self.cell,
                        &[candidate_net],
                    ),
                },
            )
            .ok()
            .map(|candidate| self.guarded(candidate));
        }
        None
    }

    fn direct_cell(&self) -> Option<PostmapCandidate> {
        let net_count = self.nets.len().min(DIRECT_REMAP_NET_CAP);
        let viable_depths = std::array::from_fn::<_, { CELL_INPUT_CAP + 1 }, _>(|input_count| {
            self.resynthesis.by_input_count[input_count]
                .iter()
                .any(|candidate| self.resynthesis.allows(candidate, self.current_area))
        });
        let depth_cap = viable_depths
            .iter()
            .rposition(|&viable| viable)?
            .min(net_count);
        if depth_cap < 2 {
            return None;
        }
        let mut best: Option<(&ResynthesisCell, PinBindings<'_>)> = None;
        for_each_projection(
            self.nets,
            net_count,
            depth_cap,
            self.care,
            |positions, classes| {
                if !viable_depths[positions.len()] {
                    return;
                }
                let candidates = &self.resynthesis.by_input_count[positions.len()];
                let Some((required_one, required_zero)) = required_truth(classes, self.out_bits)
                else {
                    return;
                };
                let range = compatible_candidate_range(
                    candidates,
                    positions.len(),
                    required_one,
                    required_zero,
                );
                for candidate in &candidates[range] {
                    if candidate.truth & required_one == required_one
                        && candidate.truth & required_zero == 0
                        && self.resynthesis.allows(candidate, self.current_area)
                        && best
                            .as_ref()
                            .is_none_or(|(chosen, _)| self.resynthesis.precedes(candidate, chosen))
                    {
                        best = Some((
                            candidate,
                            candidate
                                .pins
                                .iter()
                                .zip(positions)
                                .map(|(pin, &position)| (pin.as_str(), self.nets[position].0))
                                .collect(),
                        ));
                    }
                }
            },
        );
        let (candidate, inputs) = best?;
        let keep = inputs
            .iter()
            .map(|&(_, net)| net)
            .collect::<SmallVec<[NetId; CELL_INPUT_CAP]>>();
        debug_mfs("direct", self.debug);
        replace_driver_candidate(
            self.mapped,
            self.implementations,
            DriverReplacement {
                cell: self.cell,
                out_net: self.out_net,
                cell_name: &candidate.name,
                library_index: candidate.index,
                output_pin: &candidate.output,
                inputs: &inputs,
                dying: &driver_mffc(
                    self.mapped,
                    self.implementations,
                    self.functions,
                    self.boundary,
                    self.cell,
                    &keep,
                ),
            },
        )
        .ok()
        .map(|candidate| self.guarded(candidate))
    }
}

fn compatible_candidate_range(
    candidates: &[ResynthesisCell],
    input_count: usize,
    required_one: u64,
    required_zero: u64,
) -> std::ops::Range<usize> {
    if required_one | required_zero == truth_mask(input_count) {
        return candidates
            .binary_search_by_key(&required_one, |candidate| candidate.truth)
            .map_or(0..0, |index| index..index + 1);
    }
    0..candidates.len()
}

fn required_truth(classes: &[u64], output: &[u64]) -> Option<(u64, u64)> {
    let mut required_one = 0u64;
    let mut required_zero = 0u64;
    for (minterm, class) in classes.chunks_exact(output.len()).enumerate() {
        let required = 1 << minterm;
        for word in 0..output.len() {
            let mask = class[word];
            if mask & output[word] != 0 {
                if required_zero & required != 0 {
                    return None;
                }
                required_one |= required;
            }
            if mask & !output[word] != 0 {
                if required_one & required != 0 {
                    return None;
                }
                required_zero |= required;
            }
        }
    }
    Some((required_one, required_zero))
}

fn for_each_projection(
    inputs: &[(NetId, &[u64])],
    input_count: usize,
    depth_cap: usize,
    care: &[u64],
    mut visit: impl FnMut(&[usize], &[u64]),
) {
    let mut traversal = ProjectionTraversal {
        inputs: &inputs[..input_count],
        depth_cap,
        positions: [0; CELL_INPUT_CAP],
        levels: std::array::from_fn(|_| Vec::new()),
        visit: &mut visit,
    };
    traversal.levels[0] = care.to_vec();
    traversal.descend(0, 0);
}

struct ProjectionTraversal<'inputs, 'visit, F> {
    inputs: &'inputs [(NetId, &'inputs [u64])],
    depth_cap: usize,
    positions: [usize; CELL_INPUT_CAP],
    levels: [Vec<u64>; CELL_INPUT_CAP + 1],
    visit: &'visit mut F,
}

impl<F: FnMut(&[usize], &[u64])> ProjectionTraversal<'_, '_, F> {
    fn descend(&mut self, depth: usize, start: usize) {
        if depth == self.depth_cap {
            return;
        }
        let words = self.levels[0].len();
        for position in start..self.inputs.len() {
            self.positions[depth] = position;
            let next_depth = depth + 1;
            {
                let (parents, children) = self.levels.split_at_mut(next_depth);
                let parent = &parents[depth];
                let child = &mut children[0];
                let patterns = 1 << depth;
                child.resize(patterns * 2 * words, 0);
                let bits = self.inputs[position].1;
                for pattern in 0..patterns {
                    for word in 0..words {
                        let class = parent[pattern * words + word];
                        child[pattern * words + word] = class & !bits[word];
                        child[(pattern + patterns) * words + word] = class & bits[word];
                    }
                }
                if next_depth >= 2 {
                    (self.visit)(&self.positions[..next_depth], child);
                }
            }
            self.descend(next_depth, position + 1);
        }
    }
}

pub(super) fn wire_replacement_for(
    context: OptimizationContext<'_>,
    cell: CellId,
    read: &mut Vec<CellId>,
) -> Option<PostmapCandidate> {
    let OptimizationContext {
        mapped,
        implementations,
        functions,
        resynthesis,
        drivers,
        boundary,
        diagnostics,
    } = context;
    if !mapped.is_live_cell(cell) {
        return None;
    }
    let function = mapped
        .cell_type(cell)
        .and_then(|name| functions.get(name))?;
    let (output_pin, out_net) = cell_output_pin(mapped, cell, function)?;
    let mut roots = vec![out_net];
    if let Some(pins) = mapped.pins_on_net(out_net) {
        for pin in pins {
            let Some(owner) = mapped.pin_owner(pin) else {
                continue;
            };
            let Some(connections) = mapped.connections(owner) else {
                continue;
            };
            for connection in connections {
                if let ConnectionSignal::Net(net) = connection.signal
                    && !roots.contains(&net)
                {
                    roots.push(net);
                }
            }
        }
    }
    let current_inputs = mapped
        .connections(cell)?
        .iter()
        .filter_map(|connection| {
            let name = mapped.pin_name(connection)?;
            (function.inputs.iter().any(|input| input == name)).then_some(connection.signal)
        })
        .filter_map(|signal| match signal {
            ConnectionSignal::Net(net) => Some(net),
            ConnectionSignal::Constant(_) => None,
        })
        .collect::<Vec<_>>();
    let mut siblings = current_inputs
        .iter()
        .flat_map(|&input| mapped.pins_on_net(input).into_iter().flatten())
        .filter_map(|pin| mapped.pin_owner(pin))
        .filter(|&owner| owner != cell)
        .collect::<Vec<_>>();
    siblings.sort_unstable_by_key(|cell| cell.index());
    siblings.dedup();
    for sibling in siblings {
        if roots.len() >= 5 {
            break;
        }
        let Some(sibling_function) = mapped
            .cell_type(sibling)
            .and_then(|name| functions.get(name))
        else {
            continue;
        };
        let shared_inputs = mapped.connections(sibling).map_or(0, |connections| {
            connections
                .iter()
                .filter(|connection| {
                    mapped.pin_name(connection).is_some_and(|name| {
                        sibling_function.inputs.iter().any(|input| input == name)
                    }) && matches!(connection.signal, ConnectionSignal::Net(net) if current_inputs.contains(&net))
                })
                .count()
        });
        if shared_inputs < 2 {
            continue;
        }
        if let Some((_, sibling_out)) = cell_output_pin(mapped, sibling, sibling_function)
            && !roots.contains(&sibling_out)
        {
            roots.push(sibling_out);
        }
    }
    let window = collect_window(mapped, functions, drivers, &roots)?;
    let input_count = window.inputs.len();
    let out_bits = window.bits.get(&out_net).cloned()?;
    let mut tainted = HashSet::new();
    tainted.insert(out_net);
    for &window_cell in &window.cells {
        let Some(cell_function) = mapped
            .cell_type(window_cell)
            .and_then(|name| functions.get(name))
        else {
            continue;
        };
        let Some((_, cell_out)) = cell_output_pin(mapped, window_cell, cell_function) else {
            continue;
        };
        let feeds_tainted = mapped.connections(window_cell).is_some_and(|connections| {
            connections.iter().any(|connection| {
                matches!(connection.signal, ConnectionSignal::Net(net) if tainted.contains(&net))
                    && mapped
                        .pin_name(connection)
                        .is_some_and(|name| cell_function.inputs.iter().any(|input| input == name))
            })
        });
        if feeds_tainted {
            tainted.insert(cell_out);
        }
    }
    let consumers = mapped
        .pins_on_net(out_net)?
        .filter(|&pin| pin != output_pin)
        .collect::<Vec<_>>();
    if consumers.is_empty() {
        return None;
    }
    let (care, cone_cells) = observability_care(
        mapped,
        functions,
        boundary,
        &window,
        out_net,
        output_pin,
        input_count,
    )?;
    let mut guard = window.cells.clone();
    for &cone_cell in &cone_cells {
        if !guard.contains(&cone_cell) {
            guard.push(cone_cell);
        }
    }
    read.extend(guard.iter().copied());
    read.extend(consumers.iter().filter_map(|&pin| mapped.pin_owner(pin)));
    read.push(cell);
    let full_words = filled(true, input_count);
    let nets = sorted_candidate_nets(&window.bits, &tainted);
    let mffc_area = driver_mffc_reading(
        mapped,
        implementations,
        functions,
        boundary,
        cell,
        &[],
        read,
    )
    .iter()
    .map(|&(_, _, area)| area)
    .sum::<f64>();
    // The cost model reads cells the window never named, so the recorded set has
    // to cover them too or a cached "found nothing" outlives the netlist it was
    // derived from.
    read.sort_unstable();
    read.dedup();
    WireReplacementSearch {
        mapped,
        implementations,
        functions,
        resynthesis,
        boundary,
        cell,
        out_net,
        consumers: &consumers,
        guard: &guard,
        out_bits: &out_bits,
        care: &care,
        full_words: &full_words,
        nets: &nets,
        current_area: function.area + mffc_area,
        debug: diagnostics,
    }
    .find()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(truth: u64) -> ResynthesisCell {
        ResynthesisCell {
            name: format!("cell_{truth}"),
            index: 0,
            area: 1.0,
            delay: 1.0,
            transition: 1.0,
            truth,
            pins: Vec::new(),
            output: "Y".to_string(),
        }
    }

    #[test]
    fn full_truth_compatibility_uses_the_exact_sorted_candidate() {
        let candidates = [candidate(0b0001), candidate(0b0110), candidate(0b1110)];
        assert_eq!(
            compatible_candidate_range(&candidates, 2, 0b0110, 0b1001),
            1..2
        );
        assert_eq!(
            compatible_candidate_range(&candidates, 2, 0b0010, 0b0001),
            0..3
        );
        assert_eq!(
            compatible_candidate_range(&candidates, 2, 0b1010, 0b0101),
            0..0
        );
    }

    #[test]
    fn direct_remap_derives_four_input_care_truth() {
        let nets = (0..4)
            .map(|index| {
                let mut bits = 0u64;
                for assignment in 0..16 {
                    if assignment & (1 << index) != 0 {
                        bits |= 1 << assignment;
                    }
                }
                (NetId::from_index(index).unwrap(), vec![bits])
            })
            .collect::<Vec<_>>();
        let inputs = nets
            .iter()
            .map(|(net, bits)| (*net, bits.as_slice()))
            .collect::<Vec<_>>();

        let mut truth = None;
        for_each_projection(&inputs, 4, 4, &[0xffff], |positions, classes| {
            if positions == [0, 1, 2, 3] {
                truth = required_truth(classes, &[1 << 15]);
            }
        });
        assert_eq!(truth, Some((1 << 15, 0x7fff)));
    }

    #[test]
    fn direct_remap_combination_order_is_stable() {
        let bits = std::array::from_fn::<_, 5, _>(|_| vec![0u64]);
        let inputs = bits
            .iter()
            .enumerate()
            .map(|(index, bits)| (NetId::from_index(index).unwrap(), bits.as_slice()))
            .collect::<Vec<_>>();
        let mut combinations = Vec::new();
        for_each_projection(&inputs, 5, 3, &[u64::MAX], |positions, _| {
            if positions.len() == 3 {
                combinations.push(positions.to_vec());
            }
        });
        assert_eq!(
            combinations,
            vec![
                vec![0, 1, 2],
                vec![0, 1, 3],
                vec![0, 1, 4],
                vec![0, 2, 3],
                vec![0, 2, 4],
                vec![0, 3, 4],
                vec![1, 2, 3],
                vec![1, 2, 4],
                vec![1, 3, 4],
                vec![2, 3, 4],
            ]
        );
    }
}
