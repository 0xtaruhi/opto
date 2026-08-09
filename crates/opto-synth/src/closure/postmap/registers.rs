// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::candidate::PostmapCandidate;
use hashbrown::HashSet;
use opto_ir::mapped::{CellId, ConnectionRef, ConnectionSignal, MappedNetlist, NetId, RegionDelta};
use opto_library::{TargetCellSet, TargetPinDirection, TargetSequentialKind};

pub(super) fn constant_register_candidates(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    boundary: &HashSet<NetId>,
) -> Result<Vec<PostmapCandidate>, crate::SynthError> {
    let mut candidates = Vec::new();
    for cell in mapped.cell_ids() {
        if let Some(candidate) = constant_register_candidate(mapped, library, boundary, cell)? {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
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

fn constant_register_candidate(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    boundary: &HashSet<NetId>,
    cell: CellId,
) -> Result<Option<PostmapCandidate>, crate::SynthError> {
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
    let mut names = Vec::new();
    collect_pins(next_state, &mut names);
    let mut unknowns = Vec::new();
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
                    unknowns.push(name);
                }
            }
            None => unknowns.push(name),
        }
    }
    if unknowns.len() > 4 {
        return Ok(None);
    }
    for assignment in 0u32..1 << unknowns.len() {
        let held = next_state.eval(&mut |name| {
            if let Some(&(_, value)) = fixed.iter().find(|(fixed, _)| *fixed == name) {
                return Some(value);
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
    let mut rewires = Vec::new();
    let mut cells = vec![cell];
    for &(net, value) in &outputs {
        let Some(pins) = mapped.pins_on_net(net) else {
            return Ok(None);
        };
        for pin in pins {
            let owner = mapped.pin_owner(pin);
            if owner != Some(cell) {
                rewires.push((pin, value));
                if let Some(owner) = owner
                    && !cells.contains(&owner)
                {
                    cells.push(owner);
                }
            }
        }
    }
    let mut nets = super::mapped_cell_nets(mapped, [cell])?;
    nets.extend(outputs.iter().map(|&(net, _)| net));
    let snapshot = mapped
        .snapshot_region(cells, nets)
        .map_err(crate::SynthError::from)?;
    let mut delta = RegionDelta::new(snapshot);
    for (pin, value) in rewires {
        delta
            .reconnect_pin(pin, ConnectionRef::Constant(value))
            .map_err(crate::SynthError::from)?;
    }
    delta.remove_cell(cell).map_err(crate::SynthError::from)?;
    for (net, _) in outputs {
        delta.remove_net(net).map_err(crate::SynthError::from)?;
    }
    Ok(Some(PostmapCandidate::new(delta)))
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
        ]
        .into()
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
        let candidates = constant_register_candidates(&mapped, &library, &boundary).unwrap();
        assert_eq!(candidates.len(), 1);
        let PostmapCandidate { delta, .. } = candidates.into_iter().next().unwrap();
        mapped.apply_region_delta(delta).unwrap();
        assert_eq!(mapped.cell_count(), 1);
        let inverter = mapped.cell_ids().next().unwrap();
        let connections = mapped.connections(inverter).unwrap();
        assert_eq!(connections[0].signal, ConnectionSignal::Constant(false));
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
        let candidates = constant_register_candidates(&mapped, &library, &boundary).unwrap();
        assert!(candidates.is_empty());
    }
}
