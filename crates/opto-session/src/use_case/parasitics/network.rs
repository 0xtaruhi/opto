// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::names::NameTransform;
use super::{ReadParasiticsCompletion, ReadParasiticsOptions};
use crate::SessionError;
use opto_formats::{Spef, SpefConnection, SpefConnectionKind, SpefDirection};
use opto_library::{TargetCellRef, TargetPinDirection, TargetPinRef};
use opto_timing::{
    ParasiticDelayModel, RcCapacitor, RcConnection, RcConnectionRole, RcNetwork, RcResistor,
    RcSourceWaveform, TimingLibrary, TimingModel, TimingPortDirection,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) fn rc_networks(
    spef: &Spef,
    model: &TimingModel,
    transform: &NameTransform,
    options: &ReadParasiticsOptions,
) -> Result<Vec<RcNetwork>, SessionError> {
    spef.nets
        .iter()
        .map(|net| {
            let connections = net
                .connections
                .iter()
                .map(|connection| {
                    rc_connection(
                        spef,
                        model,
                        transform,
                        connection,
                        net.total_capacitance_farads,
                        options,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            let source_waveforms = if options.delay_model == ParasiticDelayModel::Arnoldi {
                driver_waveforms(model, &connections, net.total_capacitance_farads)?
            } else {
                [None, None]
            };
            Ok(RcNetwork {
                name: transform.apply(&net.name, spef.divider)?,
                total_capacitance_farads: net.total_capacitance_farads,
                connections,
                capacitors: net
                    .capacitors
                    .iter()
                    .map(|capacitor| RcCapacitor {
                        first: capacitor.first.clone(),
                        second: capacitor.second.clone(),
                        capacitance_farads: capacitor.capacitance_farads,
                    })
                    .collect(),
                resistors: net
                    .resistors
                    .iter()
                    .map(|resistor| RcResistor {
                        first: resistor.first.clone(),
                        second: resistor.second.clone(),
                        resistance_ohms: resistor.resistance_ohms,
                    })
                    .collect(),
                source_waveforms,
            })
        })
        .collect()
}

fn rc_connection(
    spef: &Spef,
    model: &TimingModel,
    transform: &NameTransform,
    connection: &SpefConnection,
    total_capacitance_farads: f64,
    options: &ReadParasiticsOptions,
) -> Result<RcConnection, SessionError> {
    let object = match connection.kind {
        SpefConnectionKind::Port => transform.apply(&connection.node, spef.divider)?,
        SpefConnectionKind::Internal => {
            transform.apply_pin(&connection.node, spef.divider, spef.delimiter)?
        }
    };
    let role = match (connection.kind, connection.direction) {
        (SpefConnectionKind::Port, SpefDirection::Input)
        | (SpefConnectionKind::Internal, SpefDirection::Output) => RcConnectionRole::Driver,
        (SpefConnectionKind::Port, SpefDirection::Output)
        | (SpefConnectionKind::Internal, SpefDirection::Input) => RcConnectionRole::Sink,
        (_, SpefDirection::Inout) => resolve_inout_role(model, &object)?,
    };
    let pin_capacitance_farads =
        if connection.kind == SpefConnectionKind::Internal && role == RcConnectionRole::Sink {
            pin_capacitance(
                model,
                &object,
                total_capacitance_farads,
                options.delay_model,
            )?
        } else {
            [0.0; 2]
        };
    Ok(RcConnection {
        node: connection.node.clone(),
        object,
        role,
        pin_capacitance_farads,
    })
}

fn resolve_inout_role(model: &TimingModel, object: &str) -> Result<RcConnectionRole, SessionError> {
    if let Some(port) = model
        .design()
        .ports()
        .iter()
        .find(|port| port.name == object)
    {
        return Ok(match port.direction {
            TimingPortDirection::Input => RcConnectionRole::Driver,
            TimingPortDirection::Output => RcConnectionRole::Sink,
            TimingPortDirection::Inout => {
                return Err(SessionError::state(format!(
                    "read_parasitics: inout port '{object}' has no statically selected driver"
                )));
            }
        });
    }
    let (_, pin) = instance_pin(model, object)?;
    match pin.direction() {
        TargetPinDirection::Output => Ok(RcConnectionRole::Driver),
        TargetPinDirection::Input => Ok(RcConnectionRole::Sink),
        TargetPinDirection::Inout if pin.three_state().is_some() => {
            Err(SessionError::state(format!(
                "read_parasitics: three-state inout pin '{object}' requires case analysis to select an active driver"
            )))
        }
        _ => Err(SessionError::state(format!(
            "read_parasitics: cannot determine the active direction of inout pin '{object}'"
        ))),
    }
}

fn pin_capacitance(
    model: &TimingModel,
    object: &str,
    total_capacitance_farads: f64,
    delay_model: ParasiticDelayModel,
) -> Result<[f64; 2], SessionError> {
    let (_, pin) = instance_pin(model, object)?;
    let unit = model.library().units.capacitance_farads.ok_or_else(|| {
        SessionError::state(
            "read_parasitics: active_library_set has no capacitive_load_unit declaration",
        )
    })?;
    let output_load = total_capacitance_farads / unit;
    let scalar = pin
        .capacitance()
        .or(match (pin.rise_capacitance(), pin.fall_capacitance()) {
            (Some(rise), Some(fall)) => Some((rise + fall) * 0.5),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        })
        .unwrap_or(0.0);
    Ok(std::array::from_fn(|index| {
        let edge = opto_timing::TimingEdge::ALL[index];
        let advanced = (delay_model == ParasiticDelayModel::Arnoldi)
            .then(|| {
                pin.receiver_capacitance().and_then(|model| match model {
                    opto_timing::PinReceiverCapacitanceModel::Ccs(model) => {
                        model.capacitance_at(edge, None, Some(output_load))
                    }
                    opto_timing::PinReceiverCapacitanceModel::Ecsm(model) => {
                        model.capacitance_at(edge, None)
                    }
                })
            })
            .flatten();
        advanced.unwrap_or(scalar) * unit
    }))
}

fn instance_pin<'a>(
    model: &'a TimingModel,
    object: &str,
) -> Result<(TargetCellRef<'a>, TargetPinRef<'a>), SessionError> {
    let (instance_name, pin_name) = object.rsplit_once('/').ok_or_else(|| {
        SessionError::state(format!(
            "read_parasitics: internal connection '{object}' has no pin delimiter"
        ))
    })?;
    let instance = model
        .instance_id(instance_name)
        .and_then(|instance| model.instance_ref(instance))
        .ok_or_else(|| {
            SessionError::state(format!(
                "read_parasitics: instance '{instance_name}' is absent from the timing design"
            ))
        })?;
    let cell_id = model
        .instance_library_cell_id(instance.id())
        .ok_or_else(|| {
            SessionError::state(format!(
                "read_parasitics: cell '{}' for instance '{instance_name}' is absent from active_library_set",
                instance.cell()
            ))
        })?;
    let cell = model
        .library_cell(cell_id)
        .expect("timing model library-cell IDs remain valid");
    let pin = cell
        .pins()
        .find(|pin| pin.name() == pin_name)
        .ok_or_else(|| {
            SessionError::state(format!(
                "read_parasitics: pin '{object}' is absent from cell '{}'",
                cell.name()
            ))
        })?;
    Ok((cell, pin))
}

fn driver_waveforms(
    model: &TimingModel,
    connections: &[RcConnection],
    total_capacitance_farads: f64,
) -> Result<[Option<RcSourceWaveform>; 2], SessionError> {
    let library = model.library();
    let time_unit = library.units.time_seconds.ok_or_else(|| {
        SessionError::state("read_parasitics: CCS driver waveform requires a time unit")
    })?;
    let load = library
        .units
        .capacitance_farads
        .filter(|unit| *unit > 0.0)
        .map(|unit| total_capacitance_farads / unit);
    Ok(std::array::from_fn(|index| {
        let edge = opto_timing::TimingEdge::ALL[index];
        connections
            .iter()
            .filter(|connection| connection.role == RcConnectionRole::Driver)
            .filter_map(|driver| instance_pin(model, &driver.object).ok())
            .flat_map(|(_, pin)| pin.timing_arcs())
            .filter_map(|arc| {
                arc.delay_model()
                    .and_then(|model| model.driver_waveform_at(edge, None, load))
            })
            .map(|waveform| RcSourceWaveform {
                times: waveform
                    .times
                    .into_iter()
                    .map(|time| time * time_unit)
                    .collect(),
                normalized_voltage: waveform.normalized_voltage,
            })
            .max_by(|left, right| waveform_transition(left).total_cmp(&waveform_transition(right)))
    }))
}

fn waveform_transition(waveform: &RcSourceWaveform) -> f64 {
    let crossing = |threshold| {
        waveform
            .times
            .windows(2)
            .zip(waveform.normalized_voltage.windows(2))
            .find_map(|(times, values)| {
                (values[0] <= threshold && values[1] >= threshold).then(|| {
                    let ratio = (threshold - values[0]) / (values[1] - values[0]);
                    times[0] + ratio * (times[1] - times[0])
                })
            })
            .unwrap_or(0.0)
    };
    crossing(0.8) - crossing(0.2)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "bounded sink counts scale the approximate wire-load completion model"
)]
pub(super) fn complete_networks(
    networks: &mut [RcNetwork],
    library: &TimingLibrary,
    completion: Option<ReadParasiticsCompletion>,
) -> Result<usize, SessionError> {
    let mut completion_steps = 0usize;
    for network in networks {
        let components = network_components(network);
        if components.len() <= 1 {
            continue;
        }
        let Some(completion) = completion else {
            continue;
        };
        completion_steps = completion_steps
            .checked_add(components.len() - 1)
            .ok_or_else(|| {
                SessionError::state("read_parasitics: completion-step count overflow")
            })?;
        let driver = network
            .connections
            .iter()
            .find(|connection| connection.role == RcConnectionRole::Driver)
            .ok_or_else(|| {
                SessionError::state(format!(
                    "read_parasitics: net '{}' has no driver",
                    network.name
                ))
            })?
            .node
            .clone();
        match completion {
            ReadParasiticsCompletion::Zero => complete_with_zero(network, &components, &driver),
            ReadParasiticsCompletion::WireLoad => {
                let model = library.wire_load_model.as_ref().ok_or_else(|| {
                    SessionError::state(
                        "read_parasitics: -complete_with wlm requires an active wire-load model",
                    )
                })?;
                let sinks = network
                    .connections
                    .iter()
                    .filter(|connection| connection.role == RcConnectionRole::Sink)
                    .count() as f64;
                let capacitance_unit = library.units.capacitance_farads.ok_or_else(|| {
                    SessionError::state(
                        "read_parasitics: wire-load completion requires a capacitance unit",
                    )
                })?;
                let capacitance_farads = model.capacitance_at(sinks) * capacitance_unit;
                complete_with_wire_load(network, &components, &driver, capacitance_farads);
            }
        }
    }
    Ok(completion_steps)
}

pub(super) fn network_has_loop(network: &RcNetwork) -> bool {
    let names = network
        .resistors
        .iter()
        .flat_map(|resistor| [resistor.first.as_str(), resistor.second.as_str()])
        .collect::<BTreeSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(index, name)| (name, index))
        .collect::<BTreeMap<_, _>>();
    let mut parents = (0..names.len()).collect::<Vec<_>>();
    for resistor in &network.resistors {
        let first = names[resistor.first.as_str()];
        let second = names[resistor.second.as_str()];
        let first_root = disjoint_set_root(&mut parents, first);
        let second_root = disjoint_set_root(&mut parents, second);
        if first_root == second_root {
            return true;
        }
        parents[second_root] = first_root;
    }
    false
}

fn disjoint_set_root(parents: &mut [usize], mut node: usize) -> usize {
    // Path halving keeps the cycle check effectively linear without allocating
    // adjacency lists solely for union-find.
    while parents[node] != node {
        let parent = parents[node];
        parents[node] = parents[parent];
        node = parents[node];
    }
    node
}

fn network_components(network: &RcNetwork) -> Vec<Vec<String>> {
    let nodes = network
        .connections
        .iter()
        .map(|connection| connection.node.clone())
        .chain(
            network
                .resistors
                .iter()
                .flat_map(|resistor| [resistor.first.clone(), resistor.second.clone()]),
        )
        .chain(network.capacitors.iter().flat_map(|capacitor| {
            std::iter::once(capacitor.first.clone()).chain(capacitor.second.clone())
        }))
        .collect::<BTreeSet<_>>();
    let mut adjacency = nodes
        .iter()
        .map(|node| (node.clone(), Vec::new()))
        .collect::<BTreeMap<_, Vec<String>>>();
    for resistor in &network.resistors {
        adjacency
            .entry(resistor.first.clone())
            .or_default()
            .push(resistor.second.clone());
        adjacency
            .entry(resistor.second.clone())
            .or_default()
            .push(resistor.first.clone());
    }
    let mut unseen = nodes;
    let mut components = Vec::new();
    // BTree-backed seeds plus a final per-component sort make completion
    // deterministic even when the input lists equivalent edges in another order.
    while let Some(seed) = unseen.pop_first() {
        let mut component = Vec::new();
        let mut pending = VecDeque::from([seed]);
        while let Some(node) = pending.pop_front() {
            component.push(node.clone());
            for neighbor in &adjacency[&node] {
                if unseen.remove(neighbor) {
                    pending.push_back(neighbor.clone());
                }
            }
        }
        component.sort_unstable();
        components.push(component);
    }
    components
}

fn complete_with_zero(network: &mut RcNetwork, components: &[Vec<String>], driver: &str) {
    for component in components {
        if component.iter().any(|node| node == driver) {
            continue;
        }
        let representative = &component[0];
        rename_node(network, representative, driver);
    }
    network
        .resistors
        .retain(|resistor| resistor.first != resistor.second);
}

fn complete_with_wire_load(
    network: &mut RcNetwork,
    components: &[Vec<String>],
    driver: &str,
    capacitance_farads: f64,
) {
    let incomplete = components
        .iter()
        .any(|component| !component.iter().any(|node| node == driver));
    complete_with_zero(network, components, driver);
    if incomplete {
        network.capacitors.push(RcCapacitor {
            first: driver.to_string(),
            second: None,
            capacitance_farads,
        });
        network.total_capacitance_farads += capacitance_farads;
    }
}

fn rename_node(network: &mut RcNetwork, from: &str, to: &str) {
    for connection in &mut network.connections {
        if connection.node == from {
            connection.node = to.to_string();
        }
    }
    for capacitor in &mut network.capacitors {
        if capacitor.first == from {
            capacitor.first = to.to_string();
        }
        if capacitor.second.as_deref() == Some(from) {
            capacitor.second = Some(to.to_string());
        }
    }
    for resistor in &mut network.resistors {
        if resistor.first == from {
            resistor.first = to.to_string();
        }
        if resistor.second == from {
            resistor.second = to.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial_network() -> RcNetwork {
        RcNetwork {
            name: "n".to_string(),
            total_capacitance_farads: 1e-15,
            connections: vec![
                RcConnection {
                    node: "driver".to_string(),
                    object: "U1/Y".to_string(),
                    role: RcConnectionRole::Driver,
                    pin_capacitance_farads: [0.0; 2],
                },
                RcConnection {
                    node: "sink".to_string(),
                    object: "U2/A".to_string(),
                    role: RcConnectionRole::Sink,
                    pin_capacitance_farads: [0.0; 2],
                },
            ],
            capacitors: vec![RcCapacitor {
                first: "sink".to_string(),
                second: None,
                capacitance_farads: 1e-15,
            }],
            resistors: Vec::new(),
            source_waveforms: [None, None],
        }
    }

    #[test]
    fn zero_completion_connects_partial_components_without_rc_delay() {
        let mut network = partial_network();
        let components = network_components(&network);
        assert_eq!(components.len(), 2);

        complete_with_zero(&mut network, &components, "driver");

        assert_eq!(network_components(&network).len(), 1);
        assert_eq!(network.connections[1].node, "driver");
    }

    #[test]
    fn wire_load_completion_zero_connects_and_adds_estimated_load() {
        let mut network = partial_network();
        let components = network_components(&network);

        complete_with_wire_load(&mut network, &components, "driver", 3e-15);

        assert!(network.resistors.is_empty());
        assert_eq!(network.capacitors.len(), 2);
        assert!((network.total_capacitance_farads - 4e-15).abs() < 1e-27);
        assert_eq!(network.connections[1].node, "driver");
        assert_eq!(network_components(&network).len(), 1);
    }
}
