// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::required::{CheckRequirementTarget, check_requirement, output_port_requirements};
use super::*;

#[allow(
    clippy::too_many_lines,
    reason = "output-path construction keeps exception arbitration, endpoint slack, path steps, \
              and report metadata derived from the same selected arrival"
)]
pub(super) fn collect_output_candidates(
    inputs: &CandidateInputs<'_, '_>,
    best: &mut Vec<TimingAnalysis>,
    endpoint_slacks: &mut EndpointSlacks,
) -> Result<(), crate::TimingError> {
    let timing = inputs.timing;
    let design = inputs.design;
    let library = inputs.library;
    let options = inputs.options;
    let graph = inputs.graph;
    let arrivals = inputs.arrivals;
    let paths = inputs.paths;
    let origins = inputs.origins;
    let exception_inputs = PropagationInputs {
        timing,
        model: inputs.model,
        design,
        library,
        options,
        graph,
    };
    for (port_index, port) in design.ports().iter().enumerate() {
        if port.direction != TimingPortDirection::Output
            || !matches_report_objects(&options.to, &port.name)
            || timing.timing_endpoint_is_disabled(TimingEndpoint::Port(port.id))
        {
            continue;
        }
        let Some(net) = graph.port_net(port_index).map(crate::TimingNetId::index) else {
            continue;
        };
        for edge in TimingEdge::ALL {
            for arrival in arrivals.states(net, edge.index()) {
                let key = inputs.tags.key(arrival.tag)?;
                let endpoint_points = output_endpoint_points(&exception_inputs, port.id, net, edge);
                let resolved = resolve_path_exception(
                    timing,
                    &key.path_exceptions,
                    &endpoint_points,
                    edge,
                    options.delay_type,
                )?;
                if resolved.as_ref().is_some_and(|resolved| {
                    matches!(resolved.exception.kind, PathExceptionKind::FalsePath)
                }) {
                    continue;
                }
                let sink_delay = sink_interconnect_delay(
                    timing,
                    inputs.model,
                    graph,
                    net,
                    &port.name,
                    InterconnectDelayMode::data(edge, options.delay_type),
                );
                let candidate_delay = arrival.delay + sink_delay;
                let required = output_port_requirements(
                    &exception_inputs,
                    port,
                    &arrival,
                    key,
                    edge,
                    inputs.origins,
                    resolved.as_ref().map(|resolved| &resolved.exception.kind),
                )
                .into_iter()
                .reduce(|left, right| match options.delay_type {
                    DelayType::Max => left.min(right),
                    DelayType::Min => left.max(right),
                });
                let slack = required.map(|required| match options.delay_type {
                    DelayType::Max => required - candidate_delay,
                    DelayType::Min => candidate_delay - required,
                });
                endpoint_slacks.record(EndpointKey::Port(port.id), slack);
                if !candidate_may_rank(
                    best,
                    slack,
                    candidate_delay,
                    options.delay_type,
                    options.max_paths,
                ) {
                    continue;
                }
                let mut materialized = arrival.materialize(paths, origins)?;
                append_sink_delay(
                    &mut materialized,
                    sink_delay,
                    format!("{} (net)", port.name),
                    edge,
                    None,
                );
                let candidate = TimingAnalysis {
                    design: design.name().to_string(),
                    library: library_metadata(library),
                    delay_type: options.delay_type,
                    endpoint_edge: edge,
                    arrival: materialized,
                    endpoint: port.name.clone(),
                    endpoint_object: port.name.clone(),
                    endpoint_description: "output port".to_string(),
                    path_group: None,
                    required,
                    requirement: required.map(|_| {
                        match resolved.map(|value| &value.exception.kind) {
                            Some(PathExceptionKind::MinDelay { .. }) => TimingRequirement::MinDelay,
                            Some(PathExceptionKind::MaxDelay { .. }) => TimingRequirement::MaxDelay,
                            _ if !timing.output_delays(port.id).is_empty() => {
                                TimingRequirement::OutputDelay
                            }
                            _ => TimingRequirement::MaxDelay,
                        }
                    }),
                    path_exception: resolved.map(path_exception_report),
                    time_borrowed: None,
                    significant_digits: options.significant_digits,
                };
                select_report_path(best, candidate, options.max_paths);
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "check-path construction is the exhaustive mapping from Liberty checks to report \
              requirements and must rank the exact path whose metadata it emits"
)]
pub(super) fn collect_timing_check_candidates(
    inputs: &CandidateInputs<'_, '_>,
    best: &mut Vec<TimingAnalysis>,
    endpoint_slacks: &mut EndpointSlacks,
) -> Result<(), crate::TimingError> {
    let timing = inputs.timing;
    let model = inputs.model;
    let design = inputs.design;
    let library = inputs.library;
    let options = inputs.options;
    let graph = inputs.graph;
    let arrivals = inputs.arrivals;
    let paths = inputs.paths;
    let origins = inputs.origins;
    let exception_inputs = PropagationInputs {
        timing,
        model,
        design,
        library,
        options,
        graph,
    };
    for instance in model.instances() {
        let Some(cell) = graph.cell(library, instance.id()) else {
            continue;
        };
        let connections = connection_map_ref(instance);
        let instance_name = instance.name();
        let instance_cell = instance.cell();
        for check_kind in enabled_timing_check_kinds(options) {
            for (data_pin, constraint, clock_edge) in timing_check_arcs(cell, check_kind) {
                if !timing_check_arc_is_selected(
                    &exception_inputs,
                    &options.to,
                    &instance_name,
                    data_pin.name(),
                    constraint.related_pin(),
                ) {
                    continue;
                }
                let data_object = format!("{instance_name}/{}", data_pin.name());
                let (Some(data_net), Some(clock_net)) = (
                    connections.get(data_pin.name()),
                    connections.get(constraint.related_pin()),
                ) else {
                    continue;
                };
                let data_id = data_net.index();
                let element =
                    topology::sequential_element_for_control(cell, constraint.related_pin());
                let requirement_target = CheckRequirementTarget {
                    constraint,
                    check_kind,
                    clock_edge,
                    clock_net: clock_net.index(),
                    instance_name: &instance_name,
                    data_pin: data_pin.name(),
                    data_net: data_id,
                };
                for data_edge in TimingEdge::ALL {
                    for arrival in arrivals.states(data_id, data_edge.index()) {
                        for (_, clock) in clocks_on_net(timing, graph, clock_net.index()) {
                            let key = inputs.tags.key(arrival.tag)?;
                            let Some(endpoint_requirement) = check_requirement(
                                &exception_inputs,
                                requirement_target,
                                clock,
                                data_edge,
                                &arrival,
                                key,
                                origins,
                            )?
                            else {
                                continue;
                            };
                            let clock_point =
                                format!("{instance_name}/{}", constraint.related_pin());
                            let capture_edge_time = endpoint_requirement.capture_edge_time();
                            let clock_network_delay = endpoint_requirement.clock_network_delay;
                            let constraint_value = endpoint_requirement.constraint;
                            let sink_delay = sink_interconnect_delay(
                                timing,
                                model,
                                graph,
                                data_net.index(),
                                &data_object,
                                InterconnectDelayMode::data(data_edge, options.delay_type),
                            );
                            let sink_arrival = arrival.delay + sink_delay;
                            let required = endpoint_requirement.required;
                            let slack = Some(if check_is_upper_bound(check_kind) {
                                required - sink_arrival
                            } else {
                                sink_arrival - required
                            });
                            endpoint_slacks.record(
                                EndpointKey::Pin(
                                    instance.id(),
                                    data_pin.name().to_string(),
                                    clock.name.clone(),
                                    check_kind,
                                ),
                                slack,
                            );
                            if !candidate_may_rank(
                                best,
                                slack,
                                sink_arrival,
                                options.delay_type,
                                options.max_paths,
                            ) {
                                continue;
                            }
                            let requirement = match check_kind {
                                TimingCheckKind::Setup => TimingRequirement::Setup {
                                    clock: clock.name.clone(),
                                    clock_edge,
                                    capture_edge_time,
                                    clock_network_delay,
                                    clock_point,
                                    cell: instance_cell.to_string(),
                                    constraint: constraint_value,
                                },
                                TimingCheckKind::Hold => TimingRequirement::Hold {
                                    clock: clock.name.clone(),
                                    clock_edge,
                                    capture_edge_time,
                                    clock_network_delay,
                                    clock_point,
                                    cell: instance_cell.to_string(),
                                    constraint: constraint_value,
                                },
                                TimingCheckKind::Recovery => TimingRequirement::Recovery {
                                    clock: clock.name.clone(),
                                    clock_edge,
                                    capture_edge_time,
                                    clock_network_delay,
                                    clock_point,
                                    cell: instance_cell.to_string(),
                                    constraint: constraint_value,
                                },
                                TimingCheckKind::Removal => TimingRequirement::Removal {
                                    clock: clock.name.clone(),
                                    clock_edge,
                                    capture_edge_time,
                                    clock_network_delay,
                                    clock_point,
                                    cell: instance_cell.to_string(),
                                    constraint: constraint_value,
                                },
                            };
                            let time_borrowed = match (check_kind, element) {
                                (
                                    TimingCheckKind::Setup,
                                    SequentialElement::Latch { open_edge, .. },
                                ) => clock
                                    .edge_at_or_before(open_edge, capture_edge_time)
                                    .map(|opening| (sink_arrival - opening).max(0.0)),
                                _ => None,
                            };
                            let mut materialized = arrival.materialize(paths, origins)?;
                            append_sink_delay(
                                &mut materialized,
                                sink_delay,
                                format!("{data_object} ({instance_cell})"),
                                data_edge,
                                Some(instance.id()),
                            );
                            let candidate = TimingAnalysis {
                                design: design.name().to_string(),
                                library: library_metadata(library),
                                delay_type: options.delay_type,
                                endpoint_edge: data_edge,
                                arrival: materialized,
                                endpoint: instance_name.to_string(),
                                endpoint_object: data_object.clone(),
                                endpoint_description: sequential_description(
                                    element,
                                    clock_edge,
                                    &clock.name,
                                ),
                                path_group: Some(clock.name.clone()),
                                required: Some(required),
                                requirement: Some(requirement),
                                path_exception: endpoint_requirement
                                    .path_exception
                                    .map(path_exception_report),
                                time_borrowed,
                                significant_digits: options.significant_digits,
                            };
                            select_report_path(best, candidate, options.max_paths);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "pulse-width reporting keeps the evaluated edge pair, requirement, slack, and report \
              path synchronized for each candidate"
)]
pub(super) fn collect_pulse_width_candidates(
    inputs: &CandidateInputs<'_, '_>,
    best: &mut Vec<TimingAnalysis>,
    endpoint_slacks: &mut EndpointSlacks,
) {
    if !inputs.options.checks.pulse_width {
        return;
    }
    let analysis_inputs = PropagationInputs {
        timing: inputs.timing,
        model: inputs.model,
        design: inputs.design,
        library: inputs.library,
        options: inputs.options,
        graph: inputs.graph,
    };
    for instance in inputs.model.instances() {
        let Some(cell) = inputs.graph.cell(inputs.library, instance.id()) else {
            continue;
        };
        let connections = connection_map_ref(instance);
        let instance_name = instance.name();
        let instance_cell = instance.cell();
        for (pin, constraint) in pulse_width_arcs(cell) {
            let related_pin = pulse_width_related_pin(pin, constraint);
            if timing_arc_is_disabled(&analysis_inputs, &instance_name, related_pin, pin.name()) {
                continue;
            }
            let pin_object = format!("{instance_name}/{}", pin.name());
            if !matches_any_timing_object(
                &inputs.options.to,
                [instance_name.as_ref(), pin_object.as_str()],
            ) {
                continue;
            }
            let Some(clock_net) = connections.get(pin.name()) else {
                continue;
            };
            for (_, clock) in clocks_on_net(inputs.timing, inputs.graph, clock_net.index()) {
                for pulse_edge in TimingEdge::ALL {
                    let opposite = pulse_opposite_edge(pulse_edge);
                    let Some(constraint_value) = constraint.constraint_at(
                        pulse_edge,
                        clock.transition(pulse_edge, inputs.options.delay_type),
                        clock.transition(opposite, inputs.options.delay_type),
                    ) else {
                        continue;
                    };
                    let constraint_value = constraint_value
                        * inputs.timing.timing_derate(
                            TimingDerateKind::CellCheck,
                            false,
                            pulse_edge,
                            inputs.options.delay_type,
                        );
                    let start = clock.edge_time(pulse_edge)
                        + pulse_clock_pin_delay(
                            inputs,
                            clock,
                            clock_net.index(),
                            &pin_object,
                            pulse_edge,
                        );
                    let end = clock.next_edge_after(opposite, clock.edge_time(pulse_edge))
                        + pulse_clock_pin_delay(
                            inputs,
                            clock,
                            clock_net.index(),
                            &pin_object,
                            opposite,
                        );
                    let width = end - start;
                    let slack = width - constraint_value;
                    endpoint_slacks.record(
                        EndpointKey::PulseWidth(
                            instance.id(),
                            pin.name().to_string(),
                            clock.name.clone(),
                        ),
                        Some(slack),
                    );
                    let candidate = TimingAnalysis {
                        design: inputs.design.name().to_string(),
                        library: library_metadata(inputs.library),
                        delay_type: DelayType::Min,
                        endpoint_edge: pulse_edge,
                        arrival: Arrival {
                            startpoint: clock.name.clone(),
                            startpoint_description: "clock pulse".to_string(),
                            delay: width,
                            steps: vec![PathStep {
                                point: format!("{pin_object} ({instance_cell})"),
                                incr: width,
                                path: width,
                                edge: pulse_edge,
                                instance: Some(instance.id()),
                                kind: crate::PathStepKind::TimingCheck,
                                interconnect: None,
                            }],
                        },
                        endpoint: instance_name.to_string(),
                        endpoint_object: pin_object.clone(),
                        endpoint_description: format!(
                            "{} pulse-width check",
                            match pulse_edge {
                                TimingEdge::Rise => "high",
                                TimingEdge::Fall => "low",
                            }
                        ),
                        path_group: Some(clock.name.clone()),
                        required: Some(constraint_value),
                        requirement: Some(TimingRequirement::PulseWidth {
                            clock: clock.name.clone(),
                            pulse_edge,
                            clock_point: pin_object.clone(),
                            cell: instance_cell.to_string(),
                            constraint: constraint_value,
                        }),
                        path_exception: None,
                        time_borrowed: None,
                        significant_digits: inputs.options.significant_digits,
                    };
                    if !candidate_may_rank(
                        best,
                        candidate.slack(),
                        candidate.arrival(),
                        DelayType::Min,
                        inputs.options.max_paths,
                    ) {
                        continue;
                    }
                    select_report_path(best, candidate, inputs.options.max_paths);
                }
            }
        }
    }
}

fn pulse_clock_pin_delay(
    inputs: &CandidateInputs<'_, '_>,
    clock: &Clock,
    net: usize,
    pin_object: &str,
    edge: TimingEdge,
) -> f64 {
    let delay_type = inputs.options.delay_type;
    let source = clock.source_latency(edge, delay_type, delay_type == DelayType::Max);
    let network = if clock.is_propagated() {
        sink_interconnect_delay(
            inputs.timing,
            inputs.model,
            inputs.graph,
            net,
            pin_object,
            InterconnectDelayMode::clock(edge, delay_type),
        )
    } else {
        clock.network_latency(edge, delay_type)
            * inputs
                .timing
                .timing_derate(TimingDerateKind::NetDelay, true, edge, delay_type)
    };
    source + network
}

const fn pulse_opposite_edge(edge: TimingEdge) -> TimingEdge {
    match edge {
        TimingEdge::Rise => TimingEdge::Fall,
        TimingEdge::Fall => TimingEdge::Rise,
    }
}

fn path_exception_report(
    resolved: crate::constraints::ResolvedPathException<'_>,
) -> crate::TimingPathException {
    crate::TimingPathException {
        index: u32::try_from(resolved.slot.index())
            .expect("timing constraint slots originate from nonzero u32 values"),
        kind: resolved.exception.kind.clone(),
        priority: resolved.priority,
        comment: resolved.exception.comment.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EndpointKey {
    Port(crate::PortId),
    Pin(crate::TimingInstanceId, String, String, TimingCheckKind),
    PulseWidth(crate::TimingInstanceId, String, String),
}

#[derive(Debug, Default)]
pub(super) struct EndpointSlacks {
    by_endpoint: BTreeMap<EndpointKey, f64>,
}

impl EndpointSlacks {
    fn record(&mut self, endpoint: EndpointKey, slack: Option<f64>) {
        let Some(slack) = slack else {
            return;
        };
        self.by_endpoint
            .entry(endpoint)
            .and_modify(|current| *current = current.min(slack))
            .or_insert(slack);
    }

    pub(super) fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.by_endpoint.values().copied()
    }
}

fn append_sink_delay(
    arrival: &mut Arrival,
    delay: f64,
    point: String,
    edge: TimingEdge,
    instance: Option<crate::TimingInstanceId>,
) {
    if delay == 0.0 {
        return;
    }
    arrival.delay += delay;
    arrival.steps.push(PathStep {
        point,
        incr: delay,
        path: arrival.delay,
        edge,
        instance,
        kind: crate::PathStepKind::Interconnect,
        interconnect: None,
    });
}

fn candidate_may_rank(
    best: &[TimingAnalysis],
    candidate_slack: Option<f64>,
    candidate_delay: f64,
    delay_type: DelayType,
    max_paths: usize,
) -> bool {
    best.len() < max_paths
        || best.last().is_some_and(|cutoff_path| {
            scalar_path_is_worse(candidate_slack, candidate_delay, delay_type, cutoff_path)
        })
}

fn select_report_path(best: &mut Vec<TimingAnalysis>, candidate: TimingAnalysis, max_paths: usize) {
    let position = best
        .iter()
        .position(|current| path_is_worse(&candidate, current))
        .unwrap_or(best.len());
    if position < max_paths {
        best.insert(position, candidate);
        if best.len() > max_paths {
            best.pop();
        }
    }
}

pub(super) fn select_worse_path(best: &mut Option<TimingAnalysis>, candidate: TimingAnalysis) {
    let replace = best
        .as_ref()
        .is_none_or(|current| path_is_worse(&candidate, current));
    if replace {
        *best = Some(candidate);
    }
}

pub(super) fn path_is_worse(candidate: &TimingAnalysis, current: &TimingAnalysis) -> bool {
    scalar_path_is_worse(
        candidate.slack(),
        candidate.arrival.delay,
        candidate.delay_type,
        current,
    )
}

fn scalar_path_is_worse(
    candidate_slack: Option<f64>,
    candidate_delay: f64,
    delay_type: DelayType,
    current: &TimingAnalysis,
) -> bool {
    match (candidate_slack, current.slack()) {
        (Some(candidate_slack), Some(current_slack)) => candidate_slack < current_slack,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => match delay_type {
            DelayType::Max => candidate_delay > current.arrival.delay,
            DelayType::Min => candidate_delay < current.arrival.delay,
        },
    }
}
