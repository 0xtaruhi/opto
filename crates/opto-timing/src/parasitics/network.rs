// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    ParasiticAnalysisOptions, ParasiticDelayModel, RcConnectionRole, RcNetwork, RcSourceWaveform,
    TimingEdge, arnoldi, checked_count, elmore, invalid_net,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) struct ComputedNet {
    pub(super) name: String,
    pub(super) total_capacitance: f64,
    pub(super) load_annotated: bool,
    pub(super) delay_model: ParasiticDelayModel,
    pub(super) pin_capacitance_included: bool,
    pub(super) nodes: Vec<ComputedNode>,
    pub(super) resistors: Vec<ComputedResistor>,
    pub(super) connections: Vec<ComputedConnection>,
}

pub(super) struct ComputedNode {
    pub(super) name: String,
    pub(super) ground_capacitance_farads: f64,
}

pub(super) struct ComputedResistor {
    pub(super) first: u32,
    pub(super) second: u32,
    pub(super) resistance_ohms: f64,
}

pub(super) struct ComputedConnection {
    pub(super) object: String,
    pub(super) node: u32,
    pub(super) role: RcConnectionRole,
    pub(super) pin_capacitance_farads: [f64; 2],
    pub(super) delay: Option<[f64; 2]>,
    pub(super) transition: Option<[f64; 2]>,
}

#[allow(
    clippy::too_many_lines,
    reason = "RC network assembly validates connectivity, builds the passive matrices, selects the \
              reduction strategy, and maps responses back to the same canonical node order"
)]
pub(super) fn compute(
    network: RcNetwork,
    time_unit: f64,
    capacitance_unit: f64,
    options: ParasiticAnalysisOptions,
) -> Result<ComputedNet, crate::TimingError> {
    let net = network.name.clone();
    if !network.total_capacitance_farads.is_finite() || network.total_capacitance_farads < 0.0 {
        return Err(invalid_net(&net, "total capacitance is invalid"));
    }
    validate_waveforms(&net, &network.source_waveforms)?;
    let drivers = network
        .connections
        .iter()
        .filter(|connection| connection.role == RcConnectionRole::Driver)
        .collect::<Vec<_>>();
    if drivers.is_empty() {
        return Err(invalid_net(
            &net,
            "expected at least one driver, found none",
        ));
    }

    let node_names = network
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
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let node_ids = node_names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let roots = drivers
        .iter()
        .map(|driver| {
            node_ids.get(driver.node.as_str()).copied().ok_or_else(|| {
                invalid_net(&net, format!("driver node '{}' is absent", driver.node))
            })
        })
        .collect::<Result<BTreeSet<_>, crate::TimingError>>()?
        .into_iter()
        .collect::<Vec<_>>();

    let mut ground_capacitance = vec![0.0; node_ids.len()];
    for capacitor in &network.capacitors {
        if !capacitor.capacitance_farads.is_finite() || capacitor.capacitance_farads < 0.0 {
            return Err(invalid_net(
                &net,
                "capacitance must be finite and nonnegative",
            ));
        }
        let first = node_ids[capacitor.first.as_str()];
        match capacitor.second.as_deref() {
            None => ground_capacitance[first] += capacitor.capacitance_farads,
            Some(second) => {
                return Err(invalid_net(
                    &net,
                    format!(
                        "read_parasitics does not support coupling capacitor '{}'-'{second}'",
                        capacitor.first
                    ),
                ));
            }
        }
    }
    for connection in &network.connections {
        if connection
            .pin_capacitance_farads
            .into_iter()
            .any(|capacitance| !capacitance.is_finite() || capacitance < 0.0)
        {
            return Err(invalid_net(
                &net,
                format!(
                    "connection '{}' has invalid pin capacitance",
                    connection.object
                ),
            ));
        }
    }

    let mut adjacency = vec![Vec::<(usize, f64)>::new(); node_ids.len()];
    let mut computed_resistors = Vec::with_capacity(network.resistors.len());
    for resistor in &network.resistors {
        if !resistor.resistance_ohms.is_finite() || resistor.resistance_ohms <= 0.0 {
            return Err(invalid_net(&net, "resistance must be positive and finite"));
        }
        let first = node_ids[resistor.first.as_str()];
        let second = node_ids[resistor.second.as_str()];
        if first == second {
            return Err(invalid_net(&net, "resistor connects a node to itself"));
        }
        adjacency[first].push((second, resistor.resistance_ohms));
        adjacency[second].push((first, resistor.resistance_ohms));
        computed_resistors.push(ComputedResistor {
            first: checked_count(first, "parasitic local node ID")?,
            second: checked_count(second, "parasitic local node ID")?,
            resistance_ohms: resistor.resistance_ohms,
        });
    }
    let connected = connected_order(&adjacency, roots[0]).len() == node_ids.len();
    let mut node_capacitances = [ground_capacitance.clone(), ground_capacitance.clone()];
    if !options.pin_capacitance_included {
        for connection in &network.connections {
            let node = node_ids[connection.node.as_str()];
            for edge in TimingEdge::ALL {
                node_capacitances[edge.index()][node] +=
                    connection.pin_capacitance_farads[edge.index()];
            }
        }
    }

    let annotate_delay =
        !options.net_capacitance_only && options.delay_model != ParasiticDelayModel::None;
    let responses = if !annotate_delay || !connected {
        vec![None; node_ids.len()]
    } else {
        let mut responses = vec![None; node_ids.len()];
        for root in roots {
            let driver_responses = match options.delay_model {
                ParasiticDelayModel::None => unreachable!("no-delay mode handled above"),
                ParasiticDelayModel::Elmore => {
                    elmore::analyze(&net, &node_capacitances, &adjacency, root, time_unit)?
                }
                ParasiticDelayModel::Arnoldi => arnoldi::analyze(
                    &net,
                    &node_capacitances,
                    &adjacency,
                    root,
                    &network.source_waveforms,
                    time_unit,
                )?,
            };
            merge_responses(&mut responses, driver_responses);
        }
        responses
    };

    let mut connections = network
        .connections
        .into_iter()
        .map(|connection| {
            let node = node_ids[connection.node.as_str()];
            let response = responses[node];
            Ok(ComputedConnection {
                object: connection.object,
                node: checked_count(node, "parasitic local node ID")?,
                role: connection.role,
                pin_capacitance_farads: connection.pin_capacitance_farads,
                delay: response.map(|response| response.delay),
                transition: response.and_then(|response| response.transition),
            })
        })
        .collect::<Result<Vec<_>, crate::TimingError>>()?;
    connections.sort_unstable_by(|left, right| {
        left.role
            .cmp(&right.role)
            .then_with(|| left.object.cmp(&right.object))
    });
    for pair in connections.windows(2) {
        if pair[0].role == pair[1].role && pair[0].object == pair[1].object {
            return Err(invalid_net(
                &net,
                format!("duplicate connection '{}'", pair[0].object),
            ));
        }
    }
    Ok(ComputedNet {
        name: net,
        total_capacitance: network.total_capacitance_farads / capacitance_unit,
        load_annotated: !annotate_delay || connected,
        delay_model: options.delay_model,
        pin_capacitance_included: options.pin_capacitance_included,
        nodes: node_names
            .into_iter()
            .zip(ground_capacitance)
            .map(|(name, ground_capacitance_farads)| ComputedNode {
                name,
                ground_capacitance_farads,
            })
            .collect(),
        resistors: computed_resistors,
        connections,
    })
}

fn merge_responses(
    merged: &mut [Option<arnoldi::RcResponse>],
    responses: Vec<Option<arnoldi::RcResponse>>,
) {
    for (merged, response) in merged.iter_mut().zip(responses) {
        let Some(response) = response else {
            continue;
        };
        match merged {
            Some(merged) => {
                for edge in 0..2 {
                    merged.delay[edge] = merged.delay[edge].max(response.delay[edge]);
                }
                match (&mut merged.transition, response.transition) {
                    (Some(merged), Some(response)) => {
                        for edge in 0..2 {
                            merged[edge] = merged[edge].max(response[edge]);
                        }
                    }
                    (slot @ None, Some(response)) => *slot = Some(response),
                    _ => {}
                }
            }
            slot @ None => *slot = Some(response),
        }
    }
}

fn validate_waveforms(
    net: &str,
    waveforms: &[Option<RcSourceWaveform>; 2],
) -> Result<(), crate::TimingError> {
    for waveform in waveforms.iter().flatten() {
        if waveform.times.len() < 2
            || waveform.times.len() != waveform.normalized_voltage.len()
            || waveform
                .times
                .iter()
                .chain(&waveform.normalized_voltage)
                .any(|value| !value.is_finite())
            || waveform.times.windows(2).any(|pair| pair[0] >= pair[1])
            || waveform
                .normalized_voltage
                .windows(2)
                .any(|pair| pair[0] > pair[1])
        {
            return Err(invalid_net(net, "driver waveform is invalid"));
        }
    }
    Ok(())
}

fn connected_order(adjacency: &[Vec<(usize, f64)>], root: usize) -> Vec<usize> {
    let mut visited = vec![false; adjacency.len()];
    let mut order = Vec::with_capacity(adjacency.len());
    let mut pending = VecDeque::from([root]);
    visited[root] = true;
    while let Some(node) = pending.pop_front() {
        order.push(node);
        for &(neighbor, _) in &adjacency[node] {
            if !visited[neighbor] {
                visited[neighbor] = true;
                pending.push_back(neighbor);
            }
        }
    }
    order
}
