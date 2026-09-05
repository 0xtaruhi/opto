// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::PortValueSlots;

pub(super) fn startpoint_points(
    inputs: &PropagationInputs<'_, '_>,
    endpoint: TimingEndpoint,
    net: usize,
) -> SmallVec<[TimingEndpoint; 4]> {
    let mut points = SmallVec::new();
    points.push(endpoint);
    if let Some(name) = inputs.graph.net_name(net)
        && let Some(net) = inputs.model.object_bindings.net(name)
    {
        points.push(net);
    }
    points
}

pub(super) fn sequential_startpoint_points(
    inputs: &PropagationInputs<'_, '_>,
    clock: ClockId,
    instance_name: &str,
    output_pin: &str,
    net: usize,
) -> SmallVec<[TimingEndpoint; 4]> {
    let mut points = SmallVec::new();
    points.push(TimingEndpoint::Clock(clock));
    if let Some(cell) = inputs.model.object_bindings.cell(instance_name) {
        points.push(cell);
    }
    if let Some(pin) = inputs
        .model
        .object_bindings
        .pin(&format!("{instance_name}/{output_pin}"))
    {
        points.push(pin);
    }
    if let Some(name) = inputs.graph.net_name(net)
        && let Some(net) = inputs.model.object_bindings.net(name)
    {
        points.push(net);
    }
    points
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ArcExceptionTraversal<'a> {
    pub(super) instance: usize,
    pub(super) related_pin: &'a str,
    pub(super) output_pin: &'a str,
    pub(super) from_net: usize,
    pub(super) to_net: usize,
    pub(super) input_edge: TimingEdge,
    pub(super) output_edge: TimingEdge,
}

pub(super) fn arc_exception_steps(
    inputs: &PropagationInputs<'_, '_>,
    traversal: ArcExceptionTraversal<'_>,
) -> SmallVec<[(TimingEndpoint, TimingEdge); 6]> {
    let mut points = SmallVec::new();
    if let Some(name) = inputs.graph.net_name(traversal.from_net)
        && let Some(net) = inputs.model.object_bindings.net(name)
    {
        points.push((net, traversal.input_edge));
    }
    let instance_id = crate::TimingInstanceId::from_raw(
        u32::try_from(traversal.instance)
            .expect("timing graph instance identifiers originate from u32"),
    );
    if let Some(instance) = inputs.model.instance_ref(instance_id) {
        let instance_name = instance.name();
        if let Some(pin) = inputs
            .model
            .object_bindings
            .pin(&format!("{instance_name}/{}", traversal.related_pin))
        {
            points.push((pin, traversal.input_edge));
        }
        if let Some(cell) = inputs.model.object_bindings.cell(&instance_name) {
            points.push((cell, traversal.output_edge));
        }
        if let Some(pin) = inputs
            .model
            .object_bindings
            .pin(&format!("{instance_name}/{}", traversal.output_pin))
        {
            points.push((pin, traversal.output_edge));
        }
    }
    if let Some(name) = inputs.graph.net_name(traversal.to_net)
        && let Some(net) = inputs.model.object_bindings.net(name)
    {
        points.push((net, traversal.output_edge));
    }
    points
}

pub(super) fn output_endpoint_points(
    inputs: &PropagationInputs<'_, '_>,
    port: PortId,
    net: usize,
    edge: TimingEdge,
) -> SmallVec<[(TimingEndpoint, TimingEdge); 2]> {
    let mut points = SmallVec::new();
    points.push((TimingEndpoint::Port(port), edge));
    if let Some(name) = inputs.graph.net_name(net)
        && let Some(net) = inputs.model.object_bindings.net(name)
    {
        points.push((net, edge));
    }
    points
}

pub(super) fn check_endpoint_points(
    inputs: &PropagationInputs<'_, '_>,
    capture_clock: ClockId,
    clock_edge: TimingEdge,
    instance_name: &str,
    data_pin: &str,
    net: usize,
    data_edge: TimingEdge,
) -> SmallVec<[(TimingEndpoint, TimingEdge); 4]> {
    let mut points = SmallVec::new();
    points.push((TimingEndpoint::Clock(capture_clock), clock_edge));
    if let Some(cell) = inputs.model.object_bindings.cell(instance_name) {
        points.push((cell, data_edge));
    }
    if let Some(pin) = inputs
        .model
        .object_bindings
        .pin(&format!("{instance_name}/{data_pin}"))
    {
        points.push((pin, data_edge));
    }
    if let Some(name) = inputs.graph.net_name(net)
        && let Some(net) = inputs.model.object_bindings.net(name)
    {
        points.push((net, data_edge));
    }
    points
}

pub(super) fn pin_case_allows(
    inputs: &PropagationInputs<'_, '_>,
    instance: usize,
    pin: &str,
    edge: TimingEdge,
) -> bool {
    let instance = crate::TimingInstanceId::from_raw(
        u32::try_from(instance).expect("timing graph instance identifiers originate from u32"),
    );
    inputs
        .model
        .flat_instance(instance)
        .and_then(|instance| {
            inputs
                .model
                .object_bindings
                .pin(&format!("{}/{pin}", instance.name))
        })
        .is_none_or(|endpoint| inputs.timing.case_analysis_allows(endpoint, edge))
}

pub(super) fn timing_arc_is_disabled(
    inputs: &PropagationInputs<'_, '_>,
    instance_name: &str,
    related_pin: &str,
    output_pin: &str,
) -> bool {
    let mut targets = SmallVec::<[TimingEndpoint; 3]>::new();
    if let Some(cell) = inputs.model.object_bindings.cell(instance_name) {
        targets.push(cell);
    }
    if let Some(pin) = inputs
        .model
        .object_bindings
        .pin(&format!("{instance_name}/{related_pin}"))
    {
        targets.push(pin);
    }
    if let Some(pin) = inputs
        .model
        .object_bindings
        .pin(&format!("{instance_name}/{output_pin}"))
    {
        targets.push(pin);
    }
    inputs
        .timing
        .timing_arc_is_disabled(&targets, related_pin, output_pin)
}

pub(super) fn clocks_on_net<'a>(
    timing: &'a TimingContext,
    graph: &'a TimingGraph,
    net: usize,
) -> impl Iterator<Item = (ClockSlot, &'a Clock)> {
    timing
        .clock_entries()
        .filter(move |(_, clock)| graph.clock_reaches_net(&clock.sources, net))
}

pub(super) fn explicit_transition(
    timing: &TimingContext,
    graph: &TimingGraph,
    net: usize,
) -> Option<f64> {
    graph
        .endpoint_for_net(net)
        .and_then(|endpoint| match endpoint {
            TimingEndpoint::Port(port) => timing
                .input_transitions
                .get(&port)
                .copied()
                .and_then(PortValueSlots::maximum),
            TimingEndpoint::Cell(_)
            | TimingEndpoint::Pin(_)
            | TimingEndpoint::Net(_)
            | TimingEndpoint::Clock(_) => None,
        })
}

pub(super) fn explicit_transition_for(
    timing: &TimingContext,
    graph: &TimingGraph,
    net: usize,
    edge: TimingEdge,
    delay_type: DelayType,
) -> Option<f64> {
    graph
        .endpoint_for_net(net)
        .and_then(|endpoint| match endpoint {
            TimingEndpoint::Port(port) => timing
                .input_transitions
                .get(&port)
                .and_then(|slots| slots.value(edge, delay_type)),
            TimingEndpoint::Cell(_)
            | TimingEndpoint::Pin(_)
            | TimingEndpoint::Net(_)
            | TimingEndpoint::Clock(_) => None,
        })
}

pub(super) fn explicit_load(
    timing: &TimingContext,
    graph: &TimingGraph,
    net: usize,
) -> Option<f64> {
    graph
        .endpoint_for_net(net)
        .and_then(|endpoint| match endpoint {
            TimingEndpoint::Port(port) => timing
                .loads
                .get(&port)
                .copied()
                .and_then(PortValueSlots::maximum),
            TimingEndpoint::Cell(_)
            | TimingEndpoint::Pin(_)
            | TimingEndpoint::Net(_)
            | TimingEndpoint::Clock(_) => None,
        })
}

fn explicit_load_for(
    timing: &TimingContext,
    graph: &TimingGraph,
    net: usize,
    edge: TimingEdge,
    delay_type: DelayType,
) -> Option<f64> {
    graph
        .endpoint_for_net(net)
        .and_then(|endpoint| match endpoint {
            TimingEndpoint::Port(port) => timing
                .loads
                .get(&port)
                .and_then(|slots| slots.value(edge, delay_type)),
            TimingEndpoint::Cell(_)
            | TimingEndpoint::Pin(_)
            | TimingEndpoint::Net(_)
            | TimingEndpoint::Clock(_) => None,
        })
}

pub(super) fn timing_input_transition(
    timing: &TimingContext,
    graph: &TimingGraph,
    arrival: &ArrivalState,
    net: usize,
    edge: TimingEdge,
    delay_type: DelayType,
) -> Option<f64> {
    explicit_transition_for(timing, graph, net, edge, delay_type).or(arrival.transition)
}

pub(super) fn timing_load(
    timing: &TimingContext,
    graph: &TimingGraph,
    net: usize,
    edge: TimingEdge,
    delay_type: DelayType,
) -> Option<f64> {
    let explicit = explicit_load_for(timing, graph, net, edge, delay_type).unwrap_or(0.0);
    let total = explicit + graph.capacitive_loads[net][edge.index()];
    (total > 0.0).then_some(total)
}

#[derive(Clone, Copy)]
pub(super) struct InterconnectDelayMode {
    edge: TimingEdge,
    delay_type: DelayType,
    clock_path: bool,
}

impl InterconnectDelayMode {
    pub(super) const fn data(edge: TimingEdge, delay_type: DelayType) -> Self {
        Self {
            edge,
            delay_type,
            clock_path: false,
        }
    }

    pub(super) const fn clock(edge: TimingEdge, delay_type: DelayType) -> Self {
        Self {
            edge,
            delay_type,
            clock_path: true,
        }
    }
}

pub(super) fn sink_interconnect_delay(
    timing: &TimingContext,
    model: &TimingModel,
    graph: &TimingGraph,
    net: usize,
    object: &str,
    mode: InterconnectDelayMode,
) -> f64 {
    let parasitic = graph.parasitic_sink_delay(net, object, mode.edge);
    let sink = object
        .rsplit_once('/')
        .and_then(|(instance, pin)| model.instance_id(instance).map(|id| (id, pin)));
    let wire = effective_resistance(timing, model, graph, net, sink, mode)
        * timing_load(timing, graph, net, mode.edge, mode.delay_type).unwrap_or(0.0);
    (parasitic + wire)
        * timing.timing_derate(
            TimingDerateKind::NetDelay,
            mode.clock_path,
            mode.edge,
            mode.delay_type,
        )
}

pub(super) fn sink_interconnect_delay_parts(
    timing: &TimingContext,
    model: &TimingModel,
    graph: &TimingGraph,
    net: usize,
    instance: &str,
    pin: &str,
    mode: InterconnectDelayMode,
) -> f64 {
    let parasitic = graph.parasitic_sink_delay_parts(net, instance, pin, mode.edge);
    let sink = model.instance_id(instance).map(|id| (id, pin));
    let wire = effective_resistance(timing, model, graph, net, sink, mode)
        * timing_load(timing, graph, net, mode.edge, mode.delay_type).unwrap_or(0.0);
    (parasitic + wire)
        * timing.timing_derate(
            TimingDerateKind::NetDelay,
            mode.clock_path,
            mode.edge,
            mode.delay_type,
        )
}

/// Receiver-equivalent resistance against the total net load. Explicit drive
/// and net resistance remain lumped; only the estimated wire is distributed.
/// Extracted nets have zero estimated wire R/C and retain their sink delays.
pub(super) fn effective_resistance(
    timing: &TimingContext,
    model: &TimingModel,
    graph: &TimingGraph,
    net: usize,
    sink: Option<(crate::TimingInstanceId, &str)>,
    mode: InterconnectDelayMode,
) -> f64 {
    let InterconnectDelayMode {
        edge, delay_type, ..
    } = mode;
    let drive = graph
        .endpoint_for_net(net)
        .filter(|endpoint| matches!(endpoint, TimingEndpoint::Port(_)))
        .map_or(0.0, |endpoint| {
            timing.resistance(endpoint, edge, delay_type)
        });
    let explicit_net = graph
        .net_name(net)
        .and_then(|name| model.object_bindings.net(name))
        .map_or(0.0, |endpoint| {
            timing.resistance(endpoint, edge, delay_type)
        });
    let library = model.library();
    let explicit = library.units.normalize_resistance(drive + explicit_net);
    if graph.wire_resistance(net) == 0.0 {
        return explicit;
    }
    let load = timing_load(timing, graph, net, edge, delay_type).unwrap_or(0.0);
    let sink_capacitance = match sink {
        Some((instance, pin)) => graph
            .cell(library, instance)
            .and_then(|cell| cell.pins().find(|candidate| candidate.name() == pin))
            .map_or(0.0, |pin| pin.design_input_capacitance_at(edge)),
        None => explicit_load_for(timing, graph, net, edge, delay_type).unwrap_or(0.0),
    };
    let wire_delay = library.wire_load_tree.sink_delay(
        library
            .units
            .normalize_resistance(graph.wire_resistance(net)),
        graph.wire_capacitance(net),
        graph.wire_fanout(net),
        load,
        sink_capacitance,
    );
    explicit + if load > 0.0 { wire_delay / load } else { 0.0 }
}

pub(super) struct ArcTimingEvaluation {
    pub(super) delay: f64,
    pub(super) transition: Option<f64>,
}

pub(super) fn evaluate_timing_arc(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    arc: TargetTimingArcRef<'_>,
    output_edge: TimingEdge,
    input_transition: Option<f64>,
) -> Option<ArcTimingEvaluation> {
    let mut evaluation = evaluate_at_load(
        arc,
        output_edge,
        input_transition,
        timing_load(
            inputs.timing,
            inputs.graph,
            net,
            output_edge,
            inputs.options.delay_type,
        ),
    )?;
    evaluation.delay *= inputs.timing.timing_derate(
        TimingDerateKind::CellDelay,
        false,
        output_edge,
        inputs.options.delay_type,
    );
    Some(evaluation)
}

fn evaluate_at_load(
    arc: TargetTimingArcRef<'_>,
    output_edge: TimingEdge,
    input_transition: Option<f64>,
    output_load: Option<f64>,
) -> Option<ArcTimingEvaluation> {
    Some(ArcTimingEvaluation {
        delay: arc.delay_at(output_edge, input_transition, output_load)?,
        transition: arc.transition_at(output_edge, input_transition, output_load),
    })
}

/// Whether one timing-check arc survives the report's endpoint filter and the
/// design's disabled-arc set.
///
/// Arrival closure, required-time closure, and path reporting all enumerate the
/// same check arcs and must agree on which ones are live.
pub(super) fn timing_check_arc_is_selected(
    inputs: &PropagationInputs<'_, '_>,
    endpoints: &[String],
    instance_name: &str,
    data_pin: &str,
    related_pin: &str,
) -> bool {
    if timing_arc_is_disabled(inputs, instance_name, related_pin, data_pin) {
        return false;
    }
    let data_object = format!("{instance_name}/{data_pin}");
    matches_any_timing_object(endpoints, [instance_name, data_object.as_str()])
}

pub(super) fn matches_any_timing_object<'a>(
    objects: &[String],
    points: impl IntoIterator<Item = &'a str>,
) -> bool {
    objects.is_empty()
        || points
            .into_iter()
            .any(|point| matches_report_objects(objects, point))
}

pub(super) fn matches_report_objects(objects: &[String], point: &str) -> bool {
    objects.is_empty()
        || objects
            .iter()
            .any(|object| object == point || bus_base_name(point) == Some(object.as_str()))
}

pub(super) fn library_metadata(library: &TimingLibrary) -> TimingLibraryMetadata {
    TimingLibraryMetadata {
        name: library.name.clone(),
        operating_conditions: library.operating_conditions.clone(),
        wire_load: library.wire_load.clone(),
        wire_load_mode: library.wire_load_mode.clone(),
    }
}

pub(super) fn edge_name(edge: TimingEdge) -> &'static str {
    match edge {
        TimingEdge::Rise => "rise",
        TimingEdge::Fall => "fall",
    }
}

pub(super) fn edge_adjective(edge: TimingEdge) -> &'static str {
    match edge {
        TimingEdge::Rise => "rising",
        TimingEdge::Fall => "falling",
    }
}
