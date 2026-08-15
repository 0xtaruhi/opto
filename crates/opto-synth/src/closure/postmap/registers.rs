// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::PostmapCandidate;
use hashbrown::HashSet;
use opto_ir::mapped::{CellId, ConnectionRef, ConnectionSignal, MappedNetlist, NetId, RegionDelta};
use opto_library::{TargetCellSet, TargetPinDirection, TargetSequentialKind};

/// Proposes one removal per register whose reachable value is a single
/// constant.
///
/// # Initial-state contract
///
/// A removal is proved by induction over one register: the base case is that
/// the register is at its reset value, and the step is that its next state is
/// that same value. Only registers that have an asynchronous clear or preset
/// are considered, and only the reset value is ever folded, so the base case is
/// exactly the synthesis contract that every such register is reset before the
/// design is observed. Nothing in a mapped netlist can establish that contract,
/// so it is a stated assumption, not a derived fact; a register whose own reset
/// the netlist holds inactive is declined because even the assumption cannot
/// reach it. A design run without asserting reset is outside the contract, and
/// a register whose next state is its own output is the case that shows it: the
/// induction holds, and the hardware holds its power-up value forever.
///
/// Generation is read-only, so it is sharded across the worker pool and the
/// shards are concatenated in cell order. Selection and commit stay ordered in
/// the caller, so no accepted edit depends on completion order.
/// One register proved to hold a single constant, with the value each of its
/// outputs holds.
pub(super) struct ConstantRegister {
    cell: CellId,
    outputs: Vec<(NetId, bool)>,
}

/// Builds one transaction that removes every proved-constant register together.
///
/// Each removal is an independent proof, but each transaction pays one
/// incremental-STA update over the affected timing closure. Committing them one
/// at a time costs that update per register; committing the batch pays it once.
/// Returns `None` when the batch is empty or the mapped region cannot be
/// snapshotted.
pub(super) fn constant_register_removal(
    mapped: &MappedNetlist,
    registers: &[ConstantRegister],
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
    if registers.is_empty() {
        return Ok(None);
    }
    let removed = registers
        .iter()
        .map(|register| register.cell)
        .collect::<HashSet<_>>();
    let mut cells = registers
        .iter()
        .map(|register| register.cell)
        .collect::<Vec<_>>();
    let mut rewires = Vec::new();
    for register in registers {
        for &(net, value) in &register.outputs {
            let Some(pins) = mapped.pins_on_net(net) else {
                return Ok(None);
            };
            for pin in pins {
                let owner = mapped.pin_owner(pin);
                if owner == Some(register.cell) {
                    continue;
                }
                // A consumer that is itself leaving keeps its stale connection;
                // the delta removes the whole cell, so rewiring its pins first
                // would only churn the snapshot.
                if owner.is_some_and(|owner| removed.contains(&owner)) {
                    continue;
                }
                rewires.push((pin, value));
                if let Some(owner) = owner
                    && !cells.contains(&owner)
                {
                    cells.push(owner);
                }
            }
        }
    }
    let mut nets = super::mapped_cell_nets(mapped, cells.iter().copied())?;
    nets.extend(
        registers
            .iter()
            .flat_map(|register| register.outputs.iter().map(|&(net, _)| net)),
    );
    let snapshot = mapped
        .snapshot_region(cells, nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    for (pin, value) in rewires {
        delta
            .reconnect_pin(pin, ConnectionRef::Constant(value))
            .map_err(crate::SynthError::from)?;
    }
    for register in registers {
        delta
            .remove_cell(register.cell)
            .map_err(crate::SynthError::from)?;
        for &(net, _) in &register.outputs {
            delta.remove_net(net).map_err(crate::SynthError::from)?;
        }
    }
    Ok(Some(PostmapCandidate::new(delta)))
}

pub(super) fn constant_register_candidates(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    boundary: &HashSet<NetId>,
    scope: Option<&std::collections::HashSet<CellId>>,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<Vec<ConstantRegister>, crate::SynthError> {
    // A scoped round visits only the registers whose inputs an accepted removal
    // just changed. Rescanning the whole netlist after every removal costs one
    // full sweep per removed register and finds the same answer.
    let cells = match scope {
        Some(scope) => {
            let mut cells = scope.iter().copied().collect::<Vec<_>>();
            cells.sort_unstable();
            cells
        }
        None => mapped.cell_ids().collect(),
    };
    let shards = runtime.analyze_indexed(cells.len(), |index| {
        constant_register_candidate(mapped, library, boundary, cells[index])
    })?;
    Ok(shards.into_iter().flatten().collect())
}

fn collect_pins<'function>(
    function: opto_library::BooleanFunctionRef<'function>,
    names: &mut Vec<&'function str>,
) {
    function.for_each_pin(&mut |name| {
        if !names.contains(&name) {
            names.push(name);
        }
    });
}

/// Reports whether a control function can still assert.
///
/// Every pin the function reads that is tied to a constant is substituted; a pin
/// driven by a net can take either value, so the function is assumed assertable.
/// A control the constants alone hold inactive can never assert, which is what
/// makes it interesting: the register it resets never reaches its reset value.
fn control_can_assert(
    control: opto_library::BooleanFunctionRef<'_>,
    inputs: &[(&str, ConnectionSignal)],
) -> bool {
    let mut names = Vec::new();
    collect_pins(control, &mut names);
    let constants = names
        .iter()
        .map(|name| {
            inputs
                .iter()
                .find(|(pin, _)| pin == name)
                .and_then(|&(_, signal)| match signal {
                    ConnectionSignal::Constant(value) => Some(value),
                    ConnectionSignal::Net(_) => None,
                })
        })
        .collect::<Vec<_>>();
    if constants.iter().any(Option::is_none) {
        return true;
    }
    control
        .eval(&mut |name| {
            names
                .iter()
                .position(|candidate| *candidate == name)
                .and_then(|index| constants[index])
        })
        .unwrap_or(true)
}

/// Unknown boundary nets a constant-register proof may enumerate. The proof
/// cost is `2^unknowns`, so this is a hard work bound, not a quality dial.
const MAX_CONSTANT_REGISTER_UNKNOWNS: usize = 6;

/// Combinational gates one proof may fold behind a register's inputs.
const MAX_CONSTANT_REGISTER_CONE_GATES: usize = 16;

/// Combinational cells one proof may walk forward from a register's outputs
/// while deciding which nets its own value can still affect.
const MAX_CONSTANT_REGISTER_INFLUENCE_CELLS: usize = 64;

/// Collects the nets one register's own outputs can still reach through
/// combinational logic.
///
/// This is what makes the backward fold bounded and meaningful. A net outside
/// this set cannot depend on the register, so its value is an unconstrained
/// input to the proof and folding it would only enumerate logic that answers a
/// question nobody asked. Returns `None` when the register drives more logic
/// than this proof is willing to walk.
fn influenced_nets(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    outputs: &[(NetId, bool)],
) -> Option<HashSet<NetId>> {
    let mut influenced = HashSet::new();
    let mut visited_cells = HashSet::new();
    let mut pending = outputs.iter().map(|&(net, _)| net).collect::<Vec<_>>();
    while let Some(net) = pending.pop() {
        let Some(pins) = mapped.pins_on_net(net) else {
            continue;
        };
        for pin in pins {
            let Some(cell) = mapped.pin_owner(pin) else {
                continue;
            };
            if !visited_cells.insert(cell) {
                continue;
            }
            if visited_cells.len() > MAX_CONSTANT_REGISTER_INFLUENCE_CELLS {
                return None;
            }
            let Some(mapped_cell) = mapped.cell(cell) else {
                continue;
            };
            let Some(library_index) = mapped_cell.library_cell else {
                continue;
            };
            let Some(target) = library.get(library_index as usize) else {
                continue;
            };
            if target.sequential().next().is_some() {
                continue;
            }
            let Some(connections) = mapped.connections(cell) else {
                continue;
            };
            for connection in connections {
                let Some(library_pin) = connection.library_pin else {
                    continue;
                };
                let Some(target_pin) = target.pins().nth(library_pin as usize) else {
                    continue;
                };
                if target_pin.direction() != TargetPinDirection::Output {
                    continue;
                }
                if let ConnectionSignal::Net(output) = connection.signal
                    && influenced.insert(output)
                {
                    pending.push(output);
                }
            }
        }
    }
    Some(influenced)
}

/// One folded gate in a driver cone: which library cell it is, which of its
/// pins is the output being computed, and where each pin is connected.
struct ConeGate {
    output: NetId,
    library_index: usize,
    output_pin: usize,
    /// Connections indexed by library pin, so evaluating the output function
    /// resolves a pin name with one scan rather than one scan per pin per
    /// assignment. A gate is evaluated once per enumerated assignment, so this
    /// is the inner loop of the proof.
    inputs: Vec<Option<ConnectionSignal>>,
}

/// A bounded combinational cone behind one register's input pins.
///
/// Gates are stored in evaluation order, produced by a post-order walk so a
/// gate's inputs are always resolved before the gate itself. Nets the walk
/// cannot drive combinationally, and nets that close a structural loop, become
/// unknown leaves and are enumerated rather than followed.
struct DriverCone {
    gates: Vec<ConeGate>,
    known: Vec<(NetId, bool)>,
    unknowns: Vec<NetId>,
}

impl DriverCone {
    /// Folds the cone feeding `roots`, substituting `known` net values.
    ///
    /// Returns `None` when the cone exceeds its gate or unknown budget, which
    /// is a "do not remove this register" answer rather than an error.
    fn build(
        mapped: &MappedNetlist,
        library: &TargetCellSet,
        roots: impl Iterator<Item = NetId>,
        known: &[(NetId, bool)],
        influenced: &HashSet<NetId>,
    ) -> Option<Self> {
        let mut gates = Vec::new();
        let mut unknowns = Vec::new();
        let mut emitted = HashSet::new();
        let mut visiting = HashSet::new();
        let mut pending_gates = hashbrown::HashMap::new();
        let mut pending = roots.map(|net| (net, false)).collect::<Vec<_>>();
        while let Some((net, expanded)) = pending.pop() {
            if emitted.contains(&net) || known.iter().any(|&(resolved, _)| resolved == net) {
                continue;
            }
            // Only a net the register can still affect is worth folding. Every
            // other net is an unconstrained input, so it becomes one enumerated
            // leaf instead of dragging its own cone into the proof.
            if expanded {
                // Every input of this gate has been resolved, so the gate can be
                // appended in evaluation order. `expanded` entries are only
                // pushed once the driver has been found.
                let gate = pending_gates.remove(&net)?;
                if gates.len() == MAX_CONSTANT_REGISTER_CONE_GATES {
                    return None;
                }
                gates.push(gate);
                emitted.insert(net);
                continue;
            }
            let driver = influenced
                .contains(&net)
                .then(|| combinational_driver(mapped, library, net))
                .flatten();
            let Some(gate) = driver.filter(|_| visiting.insert(net)) else {
                if unknowns.len() == MAX_CONSTANT_REGISTER_UNKNOWNS {
                    return None;
                }
                unknowns.push(net);
                emitted.insert(net);
                continue;
            };
            pending.push((net, true));
            pending.extend(gate.input_nets().map(|input| (input, false)));
            pending_gates.insert(net, gate);
        }
        Some(Self {
            gates,
            known: known.to_vec(),
            unknowns,
        })
    }

    /// Resolves every cone net for one assignment of the unknown leaves.
    ///
    /// `values` is a caller-owned scratch buffer so one proof reuses it across
    /// all `2^unknowns` assignments. Returns `false` when a gate function does
    /// not evaluate, which aborts the proof.
    fn evaluate(
        &self,
        library: &TargetCellSet,
        assignment: u32,
        values: &mut Vec<(NetId, bool)>,
    ) -> bool {
        values.clear();
        values.extend(self.known.iter().copied());
        for (index, &net) in self.unknowns.iter().enumerate() {
            values.push((net, assignment & (1 << index) != 0));
        }
        for gate in &self.gates {
            let Some(value) = gate.evaluate(library, values) else {
                return false;
            };
            values.push((gate.output, value));
        }
        true
    }
}

impl ConeGate {
    fn evaluate(&self, library: &TargetCellSet, values: &[(NetId, bool)]) -> Option<bool> {
        let target = library.get(self.library_index)?;
        let function = target.pins().nth(self.output_pin)?.function()?;
        function.eval(&mut |name| {
            let pin = target.pins().position(|pin| pin.name() == name)?;
            match self.inputs.get(pin).copied().flatten()? {
                ConnectionSignal::Constant(value) => Some(value),
                ConnectionSignal::Net(net) => values
                    .iter()
                    .find(|&&(resolved, _)| resolved == net)
                    .map(|&(_, value)| value),
            }
        })
    }

    /// Iterates the nets this gate reads.
    fn input_nets(&self) -> impl Iterator<Item = NetId> + '_ {
        self.inputs.iter().filter_map(|signal| match signal {
            Some(ConnectionSignal::Net(net)) => Some(*net),
            _ => None,
        })
    }
}

/// Finds the combinational cell driving `net`, when exactly one does.
///
/// The cone folds this net into a Boolean function of its inputs, so it has to
/// be the whole story of what puts a value on the net. Every pin on the net is
/// scanned, not just until a driver is found: a second output driver, an `Inout`
/// pin, or a three-state output all mean the net's value is a resolution the
/// cone does not represent, and each returns `None`. A sequential driver, an
/// unresolvable library reference, and a net driven by nothing return `None` for
/// the same reason. The caller reads `None` as "this net is an unknown leaf",
/// which is the conservative answer in every one of those cases.
fn combinational_driver(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    net: NetId,
) -> Option<ConeGate> {
    let mut driver = None;
    for pin in mapped.pins_on_net(net)? {
        let Some(cell) = mapped.pin_owner(pin) else {
            continue;
        };
        let Some(mapped_cell) = mapped.cell(cell) else {
            continue;
        };
        let Some(library_index) = mapped_cell.library_cell else {
            continue;
        };
        let library_index = library_index as usize;
        let target = library.get(library_index)?;
        let connections = mapped.connections(cell)?;
        let mut output_pin = None;
        let mut inputs = vec![None; target.pins().count()];
        for connection in connections {
            let library_pin = connection.library_pin? as usize;
            let target_pin = target.pins().nth(library_pin)?;
            let drives_net = connection.signal == ConnectionSignal::Net(net);
            match target_pin.direction() {
                TargetPinDirection::Output => {
                    if drives_net {
                        // A conditional output puts a value on the net only when
                        // its enable says so, and the cone has no way to say
                        // "and otherwise whatever else drives this net".
                        target_pin.three_state().is_none().then_some(())?;
                        if target.sequential().next().is_some() {
                            return None;
                        }
                        output_pin = Some(library_pin);
                    }
                }
                TargetPinDirection::Inout => {
                    // An `Inout` pin on this net may be driving it. Nothing here
                    // distinguishes that from it only reading, so the net stops
                    // being a function of one driver either way.
                    return None;
                }
                TargetPinDirection::Input | TargetPinDirection::Internal => {
                    if let Some(slot) = inputs.get_mut(library_pin) {
                        *slot = Some(connection.signal);
                    }
                }
            }
        }
        let Some(output_pin) = output_pin else {
            continue;
        };
        if driver
            .replace(ConeGate {
                output: net,
                library_index,
                output_pin,
                inputs,
            })
            .is_some()
        {
            return None;
        }
    }
    driver
}

fn constant_register_candidate(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    boundary: &HashSet<NetId>,
    cell: CellId,
) -> Result<Option<ConstantRegister>, crate::SynthError> {
    let Some(mapped_cell) = mapped.cell(cell) else {
        return Ok(None);
    };
    let Some(library_index) = mapped_cell.library_cell else {
        return Ok(None);
    };
    let Some(target) = library.get(library_index as usize) else {
        return Ok(None);
    };
    let mut sequentials = target.sequential();
    let Some(sequential) = sequentials.next() else {
        return Ok(None);
    };
    if sequentials.next().is_some() {
        return Ok(None);
    }
    if sequential.kind() != TargetSequentialKind::FlipFlop {
        return Ok(None);
    }
    let reset_value = match (sequential.clear(), sequential.preset()) {
        (Some(_), None) => false,
        (None, Some(_)) => true,
        _ => return Ok(None),
    };
    let Some(next_state) = sequential.next_state() else {
        return Ok(None);
    };
    let connections = mapped.connections(cell).ok_or_else(|| {
        crate::SynthError::invariant(format!("cell {cell:?} has no connection table"))
    })?;
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for connection in connections {
        let Some(library_pin) = connection.library_pin else {
            return Ok(None);
        };
        let Some(pin) = target.pins().nth(library_pin as usize) else {
            return Ok(None);
        };
        if pin.direction() == TargetPinDirection::Output {
            let Some((state, positive)) = pin
                .function()
                .and_then(opto_library::BooleanFunctionRef::as_literal)
            else {
                return Ok(None);
            };
            let direct = Some(state) == sequential.state_variables().next();
            let shadow = Some(state) == sequential.state_variables().nth(1);
            let value = match (direct, shadow) {
                (true, _) => positive == reset_value,
                (_, true) => positive != reset_value,
                _ => return Ok(None),
            };
            let ConnectionSignal::Net(net) = connection.signal else {
                return Ok(None);
            };
            outputs.push((net, value));
        } else {
            inputs.push((pin.name(), connection.signal));
        }
    }
    if outputs.is_empty() {
        return Ok(None);
    }
    // The proof below is an induction over this register, and its base case is
    // the reset. A register whose reset can never assert has no base case: its
    // power-up value is arbitrary and stays arbitrary, so the reset value is not
    // the value it holds.
    let ((Some(control), None) | (None, Some(control))) = (sequential.clear(), sequential.preset())
    else {
        return Ok(None);
    };
    if !control_can_assert(control, &inputs) {
        return Ok(None);
    }
    let mut names = Vec::new();
    collect_pins(next_state, &mut names);
    let mut unknowns = Vec::new();
    let mut driven = Vec::new();
    let mut fixed = Vec::new();
    for name in names {
        let state = if Some(name) == sequential.state_variables().next() {
            Some(reset_value)
        } else if Some(name) == sequential.state_variables().nth(1) {
            Some(!reset_value)
        } else {
            None
        };
        if let Some(value) = state {
            fixed.push((name, value));
            continue;
        }
        let connection = inputs.iter().find(|(pin, _)| *pin == name);
        match connection {
            Some(&(_, ConnectionSignal::Constant(value))) => fixed.push((name, value)),
            Some(&(_, ConnectionSignal::Net(net))) => {
                if let Some(&(_, value)) = outputs.iter().find(|&&(output, _)| output == net) {
                    fixed.push((name, value));
                } else {
                    driven.push((name, net));
                }
            }
            None => unknowns.push(name),
        }
    }
    // A reserved or hardwired RTL field usually reaches its register through a
    // write-enable gate rather than through a constant pin, so the pin-local
    // question "is D tied to my reset value" answers no while the design still
    // never leaves that value. Folding a bounded cone behind the driven pins,
    // with this register's own outputs already substituted, asks the question
    // the design actually poses.
    let Some(influenced) = influenced_nets(mapped, library, &outputs) else {
        return Ok(None);
    };
    let Some(cone) = DriverCone::build(
        mapped,
        library,
        driven.iter().map(|&(_, net)| net),
        &outputs,
        &influenced,
    ) else {
        return Ok(None);
    };
    let cone_unknowns = cone.unknowns.len();
    if unknowns.len() + cone_unknowns > MAX_CONSTANT_REGISTER_UNKNOWNS {
        return Ok(None);
    }
    let mut values = Vec::new();
    for assignment in 0u32..1 << (unknowns.len() + cone_unknowns) {
        if !cone.evaluate(library, assignment >> unknowns.len(), &mut values) {
            return Ok(None);
        }
        let held = next_state.eval(&mut |name| {
            if let Some(&(_, value)) = fixed.iter().find(|(fixed, _)| *fixed == name) {
                return Some(value);
            }
            if let Some(&(_, net)) = driven.iter().find(|(driven, _)| *driven == name) {
                return values
                    .iter()
                    .find(|&&(resolved, _)| resolved == net)
                    .map(|&(_, value)| value);
            }
            unknowns
                .iter()
                .position(|unknown| *unknown == name)
                .map(|index| assignment & (1 << index) != 0)
        });
        if held != Some(reset_value) {
            return Ok(None);
        }
    }
    if outputs.iter().any(|(net, _)| boundary.contains(net)) {
        return Ok(None);
    }
    Ok(Some(ConstantRegister { cell, outputs }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::mapped::MappedBuilder;
    use opto_library::{
        BooleanFunction, TargetCell, TargetPin, TargetPinDirection, TargetSequential,
        TargetSequentialKind,
    };

    fn pin(name: &str, direction: TargetPinDirection, function: Option<&str>) -> TargetPin {
        TargetPin {
            name: name.to_string(),
            direction,
            function: function.map(|text| BooleanFunction::parse(text).unwrap()),
            three_state: None,
            capacitance: None,
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        }
    }

    fn library() -> TargetCellSet {
        vec![
            TargetCell {
                name: "EDFCNQ".to_string(),
                area: Some(2.9),
                dont_use: false,
                usage: opto_library::TargetCellUsage::default(),
                pins: vec![
                    pin("D", TargetPinDirection::Input, None),
                    pin("E", TargetPinDirection::Input, None),
                    pin("CP", TargetPinDirection::Input, None),
                    pin("CDN", TargetPinDirection::Input, None),
                    pin("Q", TargetPinDirection::Output, Some("IQ")),
                ],
                sequential: vec![TargetSequential {
                    kind: TargetSequentialKind::FlipFlop,
                    state_variables: vec!["IQ".to_string(), "IQN".to_string()],
                    clocked_on: Some(BooleanFunction::parse("CP").unwrap()),
                    next_state: Some(BooleanFunction::parse("(D E)+(IQ !E)").unwrap()),
                    enable: None,
                    clear: Some(BooleanFunction::parse("!CDN").unwrap()),
                    preset: None,
                }],
                clock_gate: None,
                memory: None,
            },
            TargetCell {
                name: "INV".to_string(),
                area: Some(0.3),
                dont_use: false,
                usage: opto_library::TargetCellUsage::default(),
                pins: vec![
                    pin("I", TargetPinDirection::Input, None),
                    pin("ZN", TargetPinDirection::Output, Some("!I")),
                ],
                sequential: Vec::new(),
                clock_gate: None,
                memory: None,
            },
            TargetCell {
                name: "AND2".to_string(),
                area: Some(0.5),
                dont_use: false,
                usage: opto_library::TargetCellUsage::default(),
                pins: vec![
                    pin("A", TargetPinDirection::Input, None),
                    pin("B", TargetPinDirection::Input, None),
                    pin("Z", TargetPinDirection::Output, Some("A B")),
                ],
                sequential: Vec::new(),
                clock_gate: None,
                memory: None,
            },
        ]
        .into()
    }

    /// A net two cells drive is not a function of either of them, so the cone
    /// cannot fold it and the register behind it is not provably constant.
    ///
    /// The net is on the feedback path from the register's own output back to
    /// its data pin, which is where the cone actually looks. Read alone, the
    /// first driver says the data is constant zero.
    #[test]
    fn keeps_registers_behind_a_multiply_driven_net() {
        let library = library();
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let clock = builder.add_net(Some("clk")).unwrap();
        let reset = builder.add_net(Some("rst")).unwrap();
        let enable = builder.add_net(Some("en")).unwrap();
        let d = builder.add_net(Some("d")).unwrap();
        let q = builder.add_net(Some("q")).unwrap();
        builder
            .add_cell(
                "r0",
                "EDFCNQ",
                Some(0),
                &[
                    ("D".to_string(), Some(0), ConnectionSignal::Net(d)),
                    ("E".to_string(), Some(1), ConnectionSignal::Net(enable)),
                    ("CP".to_string(), Some(2), ConnectionSignal::Net(clock)),
                    ("CDN".to_string(), Some(3), ConnectionSignal::Net(reset)),
                    ("Q".to_string(), Some(4), ConnectionSignal::Net(q)),
                ],
            )
            .unwrap();
        // `q AND 0` is zero, so folding this driver alone proves the register
        // constant. `en AND 1` is not, and both drive `d`.
        for (name, source, constant) in [("u_a", q, false), ("u_b", enable, true)] {
            builder
                .add_cell(
                    name,
                    "AND2",
                    Some(2),
                    &[
                        ("A".to_string(), Some(0), ConnectionSignal::Net(source)),
                        (
                            "B".to_string(),
                            Some(1),
                            ConnectionSignal::Constant(constant),
                        ),
                        ("Z".to_string(), Some(2), ConnectionSignal::Net(d)),
                    ],
                )
                .unwrap();
        }
        let mapped = builder.freeze().unwrap();
        let registers = constant_register_candidates(
            &mapped,
            &library,
            &HashSet::new(),
            None,
            crate::test_runtime(),
        )
        .unwrap();
        assert!(registers.is_empty());
    }

    #[test]
    fn folds_enable_registers_holding_their_reset_value() {
        let library = library();
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let clock = builder.add_net(Some("clk")).unwrap();
        let reset = builder.add_net(Some("rst")).unwrap();
        let enable = builder.add_net(Some("en")).unwrap();
        let q = builder.add_net(Some("q")).unwrap();
        let out = builder.add_net(Some("out")).unwrap();
        builder
            .add_cell(
                "r0",
                "EDFCNQ",
                Some(0),
                &[
                    ("D".to_string(), Some(0), ConnectionSignal::Constant(false)),
                    ("E".to_string(), Some(1), ConnectionSignal::Net(enable)),
                    ("CP".to_string(), Some(2), ConnectionSignal::Net(clock)),
                    ("CDN".to_string(), Some(3), ConnectionSignal::Net(reset)),
                    ("Q".to_string(), Some(4), ConnectionSignal::Net(q)),
                ],
            )
            .unwrap();
        builder
            .add_cell(
                "u0",
                "INV",
                Some(1),
                &[
                    ("I".to_string(), Some(0), ConnectionSignal::Net(q)),
                    ("ZN".to_string(), Some(1), ConnectionSignal::Net(out)),
                ],
            )
            .unwrap();
        let mut mapped = builder.freeze().unwrap();
        let boundary = HashSet::new();
        let registers =
            constant_register_candidates(&mapped, &library, &boundary, None, crate::test_runtime())
                .unwrap();
        assert_eq!(registers.len(), 1);
        let PostmapCandidate { delta, .. } = constant_register_removal(&mapped, &registers)
            .unwrap()
            .unwrap();
        mapped.apply_region_delta(delta).unwrap();
        assert_eq!(mapped.cell_count(), 1);
        let inverter = mapped.cell_ids().next().unwrap();
        let connections = mapped.connections(inverter).unwrap();
        assert_eq!(connections[0].signal, ConnectionSignal::Constant(false));
    }

    /// Pins the initial-state contract on the case that makes it visible.
    ///
    /// A register whose data is its own output satisfies the induction while
    /// holding whatever it powered up with, so folding it is correct exactly
    /// when the contract holds. The test exists so that the assumption is
    /// visible in the suite rather than implied by the proof.
    #[test]
    fn folds_a_self_holding_register_under_the_reset_contract() {
        let library = library();
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let clock = builder.add_net(Some("clk")).unwrap();
        let reset = builder.add_net(Some("rst")).unwrap();
        let enable = builder.add_net(Some("en")).unwrap();
        let q = builder.add_net(Some("q")).unwrap();
        let out = builder.add_net(Some("out")).unwrap();
        builder
            .add_cell(
                "r0",
                "EDFCNQ",
                Some(0),
                &[
                    ("D".to_string(), Some(0), ConnectionSignal::Net(q)),
                    ("E".to_string(), Some(1), ConnectionSignal::Net(enable)),
                    ("CP".to_string(), Some(2), ConnectionSignal::Net(clock)),
                    ("CDN".to_string(), Some(3), ConnectionSignal::Net(reset)),
                    ("Q".to_string(), Some(4), ConnectionSignal::Net(q)),
                ],
            )
            .unwrap();
        builder
            .add_cell(
                "u0",
                "INV",
                Some(1),
                &[
                    ("I".to_string(), Some(0), ConnectionSignal::Net(q)),
                    ("ZN".to_string(), Some(1), ConnectionSignal::Net(out)),
                ],
            )
            .unwrap();
        let mapped = builder.freeze().unwrap();
        let registers = constant_register_candidates(
            &mapped,
            &library,
            &HashSet::new(),
            None,
            crate::test_runtime(),
        )
        .unwrap();
        assert_eq!(registers.len(), 1);
    }

    /// A register the netlist can never reset has no base case for the
    /// induction, so its power-up value is arbitrary and it is not constant.
    #[test]
    fn keeps_registers_whose_reset_is_tied_inactive() {
        let library = library();
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let clock = builder.add_net(Some("clk")).unwrap();
        let enable = builder.add_net(Some("en")).unwrap();
        let q = builder.add_net(Some("q")).unwrap();
        let out = builder.add_net(Some("out")).unwrap();
        builder
            .add_cell(
                "r0",
                "EDFCNQ",
                Some(0),
                &[
                    ("D".to_string(), Some(0), ConnectionSignal::Constant(false)),
                    ("E".to_string(), Some(1), ConnectionSignal::Net(enable)),
                    ("CP".to_string(), Some(2), ConnectionSignal::Net(clock)),
                    // CDN is active low, so tying it high means this register is
                    // never cleared.
                    ("CDN".to_string(), Some(3), ConnectionSignal::Constant(true)),
                    ("Q".to_string(), Some(4), ConnectionSignal::Net(q)),
                ],
            )
            .unwrap();
        builder
            .add_cell(
                "u0",
                "INV",
                Some(1),
                &[
                    ("I".to_string(), Some(0), ConnectionSignal::Net(q)),
                    ("ZN".to_string(), Some(1), ConnectionSignal::Net(out)),
                ],
            )
            .unwrap();
        let mapped = builder.freeze().unwrap();
        let registers = constant_register_candidates(
            &mapped,
            &library,
            &HashSet::new(),
            None,
            crate::test_runtime(),
        )
        .unwrap();
        assert!(registers.is_empty());
    }

    #[test]
    fn keeps_registers_whose_data_can_diverge_from_reset() {
        let library = library();
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let clock = builder.add_net(Some("clk")).unwrap();
        let reset = builder.add_net(Some("rst")).unwrap();
        let enable = builder.add_net(Some("en")).unwrap();
        let data = builder.add_net(Some("d")).unwrap();
        for (name, signal) in [
            ("r_live", ConnectionSignal::Net(data)),
            ("r_one", ConnectionSignal::Constant(true)),
        ] {
            let q = builder.add_net(Some(name)).unwrap();
            builder
                .add_cell(
                    name,
                    "EDFCNQ",
                    Some(0),
                    &[
                        ("D".to_string(), Some(0), signal),
                        ("E".to_string(), Some(1), ConnectionSignal::Net(enable)),
                        ("CP".to_string(), Some(2), ConnectionSignal::Net(clock)),
                        ("CDN".to_string(), Some(3), ConnectionSignal::Net(reset)),
                        ("Q".to_string(), Some(4), ConnectionSignal::Net(q)),
                    ],
                )
                .unwrap();
        }
        let mapped = builder.freeze().unwrap();
        let boundary = HashSet::new();
        let registers =
            constant_register_candidates(&mapped, &library, &boundary, None, crate::test_runtime())
                .unwrap();
        assert!(registers.is_empty());
        assert!(
            constant_register_removal(&mapped, &registers)
                .unwrap()
                .is_none()
        );
    }
}
