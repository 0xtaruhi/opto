// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(crate) fn check_timing(timing: &TimingContext, model: &TimingModel) -> CheckTimingAnalysis {
    let design = &model.design;
    let library = &model.library;
    let graph = &model.graph;
    let missing_input_delays = design
        .ports()
        .iter()
        .filter(|port| {
            matches!(
                port.direction,
                TimingPortDirection::Input | TimingPortDirection::Inout
            ) && timing.input_delays(port.id).is_empty()
                && !timing.timing_endpoint_is_disabled(TimingEndpoint::Port(port.id))
                && !timing
                    .clocks()
                    .iter()
                    .any(|clock| clock.sources.contains(&port.id))
        })
        .map(|port| port.name.clone())
        .collect();

    let mut sequentially_reachable = vec![false; graph.net_count()];
    for instance in model.instances() {
        let Some(cell) = graph.cell(library, instance.id()) else {
            continue;
        };
        let connections = connection_map_ref(instance);
        for (output_pin, arc, _) in clock_to_q_arcs(cell) {
            let Some(clock_net) = connections.get(arc.related_pin()) else {
                continue;
            };
            if clocks_on_net(timing, graph, clock_net.index())
                .next()
                .is_none()
            {
                continue;
            }
            let Some(output_net) = connections.get(output_pin.name()) else {
                continue;
            };
            sequentially_reachable[output_net.index()] = true;
        }
    }
    for &from in &graph.topological_order {
        if !sequentially_reachable[from] {
            continue;
        }
        for &arc in &graph.outgoing[from] {
            sequentially_reachable[graph.arc(arc).to.index()] = true;
        }
    }

    let mut unconstrained_endpoints = Vec::new();
    for instance in model.instances() {
        let Some(cell) = graph.cell(library, instance.id()) else {
            continue;
        };
        let connections = connection_map_ref(instance);
        let instance_name = instance.name();
        for (data_pin, _, _) in timing_check_arcs(cell, TimingCheckKind::Setup)
            .chain(timing_check_arcs(cell, TimingCheckKind::Recovery))
        {
            let Some(data_net) = connections.get(data_pin.name()) else {
                continue;
            };
            let endpoint = format!("{instance_name}/{}", data_pin.name());
            let explicitly_constrained = false;
            if !sequentially_reachable[data_net.index()] && !explicitly_constrained {
                unconstrained_endpoints.push(endpoint);
            }
        }
    }
    unconstrained_endpoints.extend(
        design
            .ports()
            .iter()
            .filter(|port| port.direction == TimingPortDirection::Output)
            .filter(|port| !timing.timing_endpoint_is_disabled(TimingEndpoint::Port(port.id)))
            .filter(|port| {
                timing.output_delays(port.id).is_empty()
                    && !timing
                        .path_exceptions()
                        .iter()
                        .any(|exception| exception.to.matches_any(&[TimingEndpoint::Port(port.id)]))
            })
            .map(|port| port.name.clone()),
    );
    unconstrained_endpoints.sort();
    unconstrained_endpoints.dedup();

    CheckTimingAnalysis {
        no_clocks: timing.clocks().is_empty(),
        missing_input_delays,
        unconstrained_endpoints,
    }
}

pub(super) fn clock_to_q_arcs(
    cell: TargetCellRef<'_>,
) -> impl Iterator<Item = (TargetPinRef<'_>, TargetTimingArcRef<'_>, TimingEdge)> {
    cell.pins().flat_map(|pin| {
        pin.timing_arcs().filter_map(move |arc| {
            let TargetTimingType::ClockToQ(edge) = arc.timing_type() else {
                return None;
            };
            Some((pin, arc, edge))
        })
    })
}

pub(super) fn timing_check_arcs(
    cell: TargetCellRef<'_>,
    check_kind: TimingCheckKind,
) -> impl Iterator<Item = (TargetPinRef<'_>, TargetTimingArcRef<'_>, TimingEdge)> {
    cell.pins().flat_map(move |pin| {
        pin.timing_arcs().filter_map(move |arc| {
            let (kind, clock_edge) = match arc.timing_type() {
                TargetTimingType::Check { kind, clock_edge } => (kind, clock_edge),
                TargetTimingType::Recovery(clock_edge) => (TimingCheckKind::Recovery, clock_edge),
                TargetTimingType::Removal(clock_edge) => (TimingCheckKind::Removal, clock_edge),
                TargetTimingType::Combinational
                | TargetTimingType::ClockToQ(_)
                | TargetTimingType::Clear
                | TargetTimingType::Preset
                | TargetTimingType::MinPulseWidth
                | TargetTimingType::NonSequentialSetup(_)
                | TargetTimingType::NonSequentialHold(_)
                | TargetTimingType::ThreeStateEnable
                | TargetTimingType::ThreeStateDisable => return None,
            };
            (kind == check_kind).then_some((pin, arc, clock_edge))
        })
    })
}

pub(super) fn pulse_width_arcs(
    cell: TargetCellRef<'_>,
) -> impl Iterator<Item = (TargetPinRef<'_>, TargetTimingArcRef<'_>)> {
    cell.pins().flat_map(|pin| {
        pin.timing_arcs()
            .filter(|arc| arc.timing_type() == TargetTimingType::MinPulseWidth)
            .map(move |arc| (pin, arc))
    })
}

pub(super) fn pulse_width_related_pin<'a>(
    pin: TargetPinRef<'a>,
    arc: TargetTimingArcRef<'a>,
) -> &'a str {
    let related = arc.related_pin().trim();
    if related.is_empty() {
        pin.name()
    } else {
        related
    }
}

pub(super) fn enabled_timing_check_kinds(
    options: &crate::ReportTimingOptions,
) -> impl Iterator<Item = TimingCheckKind> {
    let checks = options.checks;
    match options.delay_type {
        crate::DelayType::Max => [
            checks.setup.then_some(TimingCheckKind::Setup),
            checks.recovery.then_some(TimingCheckKind::Recovery),
        ],
        crate::DelayType::Min => [
            checks.hold.then_some(TimingCheckKind::Hold),
            checks.removal.then_some(TimingCheckKind::Removal),
        ],
    }
    .into_iter()
    .flatten()
}

pub(super) const fn check_is_upper_bound(kind: TimingCheckKind) -> bool {
    matches!(kind, TimingCheckKind::Setup | TimingCheckKind::Recovery)
}
