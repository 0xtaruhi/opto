// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::super::required::{CheckRequirementTarget, check_requirement};
use super::*;

#[allow(
    clippy::too_many_lines,
    reason = "the timing-check equation must keep capture selection, uncertainty, CRPR, derating, \
              and scalar-path ranking in one auditable calculation"
)]
pub(super) fn evaluate_check(
    target: CheckTarget,
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &PropagationState,
) -> Result<EndpointValue, crate::TimingError> {
    let CheckTarget {
        instance: instance_id,
        data_pin: data_pin_id,
        capture_clock,
        check_kind,
        net,
    } = target;
    let Some(instance) = model.instance_ref(instance_id) else {
        return Ok(EndpointValue {
            slack: None,
            path: None,
        });
    };
    let Some(cell) = model.graph.cell(&model.library, instance.id()) else {
        return Ok(EndpointValue {
            slack: None,
            path: None,
        });
    };
    let Some(data_pin) = cell.pins().nth(data_pin_id.index()) else {
        return Ok(EndpointValue {
            slack: None,
            path: None,
        });
    };
    let Some(clock) = timing.clock_by_slot(capture_clock) else {
        return Ok(EndpointValue {
            slack: None,
            path: None,
        });
    };
    let connections = connection_map_ref(instance);
    let instance_name = instance.name();
    let exception_inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph: &model.graph,
    };
    let mut slack = None::<f64>;
    let mut path = None;
    for constraint in data_pin.timing_arcs() {
        let Some(clock_edge) = check_clock_edge(constraint, check_kind) else {
            continue;
        };
        if timing_arc_is_disabled(
            &exception_inputs,
            &instance_name,
            constraint.related_pin(),
            data_pin.name(),
        ) {
            continue;
        }
        let Some(clock_net) = connections.get(constraint.related_pin()) else {
            continue;
        };
        if !clock
            .sources
            .iter()
            .any(|source| model.graph.net_has_port(clock_net.index(), *source))
        {
            continue;
        }
        let data_object = format!("{instance_name}/{}", data_pin.name());
        let requirement_target = CheckRequirementTarget {
            constraint,
            check_kind,
            clock_edge,
            clock_net: clock_net.index(),
            instance_name: &instance_name,
            data_pin: data_pin.name(),
            data_net: net,
        };
        for data_edge in TimingEdge::ALL {
            for arrival in propagation.arrivals.states(net, data_edge.index()) {
                let key = propagation.tags.key(arrival.tag)?;
                let Some(requirement) = check_requirement(
                    &exception_inputs,
                    requirement_target,
                    clock,
                    data_edge,
                    &arrival,
                    key,
                    &propagation.origins,
                )?
                else {
                    continue;
                };
                let sink_delay = sink_interconnect_delay(
                    timing,
                    model,
                    &model.graph,
                    net,
                    &data_object,
                    InterconnectDelayMode::data(data_edge, options.delay_type),
                );
                let sink_arrival = arrival.delay + sink_delay;
                let candidate_slack = if check_is_upper_bound(check_kind) {
                    requirement.required - sink_arrival
                } else {
                    sink_arrival - requirement.required
                };
                if slack.is_none_or(|current| candidate_slack < current) {
                    slack = Some(candidate_slack);
                }
                let candidate = ScalarPath {
                    slack: Some(candidate_slack),
                    arrival: sink_arrival,
                };
                if path
                    .is_none_or(|current| scalar_is_worse(candidate, current, options.delay_type))
                {
                    path = Some(candidate);
                }
            }
        }
    }
    Ok(EndpointValue { slack, path })
}

#[allow(
    clippy::too_many_lines,
    reason = "pulse-width evaluation is one equation over both edges, propagated clock delay, \
              derating, and worst-candidate selection"
)]
pub(super) fn evaluate_pulse_width(
    target: PulseWidthTarget,
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> EndpointValue {
    let PulseWidthTarget {
        instance: instance_id,
        pin: pin_id,
        clock: clock_slot,
        net,
    } = target;
    let Some(instance) = model.instance_ref(instance_id) else {
        return EndpointValue {
            slack: None,
            path: None,
        };
    };
    let Some(cell) = model.graph.cell(&model.library, instance.id()) else {
        return EndpointValue {
            slack: None,
            path: None,
        };
    };
    let Some(clock) = timing.clock_by_slot(clock_slot) else {
        return EndpointValue {
            slack: None,
            path: None,
        };
    };
    let Some(pin) = cell.pins().nth(pin_id.index()) else {
        return EndpointValue {
            slack: None,
            path: None,
        };
    };
    let instance_name = instance.name();
    let pin_name = pin.name();
    let pin_object = format!("{instance_name}/{pin_name}");
    let analysis_inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph: &model.graph,
    };
    let mut slack = None::<f64>;
    let mut path = None;
    for constraint in pin
        .timing_arcs()
        .filter(|arc| arc.timing_type() == TargetTimingType::MinPulseWidth)
    {
        if timing_arc_is_disabled(
            &analysis_inputs,
            &instance_name,
            pulse_width_related_pin(pin, constraint),
            pin.name(),
        ) {
            continue;
        }
        for pulse_edge in TimingEdge::ALL {
            let opposite = opposite_edge(pulse_edge);
            let Some(constraint_value) = constraint.constraint_at(
                pulse_edge,
                clock.transition(pulse_edge, options.delay_type),
                clock.transition(opposite, options.delay_type),
            ) else {
                continue;
            };
            let constraint_value = constraint_value
                * timing.timing_derate(
                    TimingDerateKind::CellCheck,
                    false,
                    pulse_edge,
                    options.delay_type,
                );
            let start = clock.edge_time(pulse_edge)
                + clock_pin_delay(
                    timing,
                    model,
                    clock,
                    net,
                    &pin_object,
                    pulse_edge,
                    options.delay_type,
                );
            let end = clock.next_edge_after(opposite, clock.edge_time(pulse_edge))
                + clock_pin_delay(
                    timing,
                    model,
                    clock,
                    net,
                    &pin_object,
                    opposite,
                    options.delay_type,
                );
            let width = end - start;
            let candidate_slack = width - constraint_value;
            if slack.is_none_or(|current| candidate_slack < current) {
                slack = Some(candidate_slack);
            }
            let candidate = ScalarPath {
                slack: Some(candidate_slack),
                arrival: width,
            };
            if path.is_none_or(|current| scalar_is_worse(candidate, current, options.delay_type)) {
                path = Some(candidate);
            }
        }
    }
    EndpointValue { slack, path }
}

fn check_clock_edge(arc: TargetTimingArcRef<'_>, expected: TimingCheckKind) -> Option<TimingEdge> {
    match arc.timing_type() {
        TargetTimingType::Check { kind, clock_edge } if kind == expected => Some(clock_edge),
        TargetTimingType::Recovery(clock_edge) if expected == TimingCheckKind::Recovery => {
            Some(clock_edge)
        }
        TargetTimingType::Removal(clock_edge) if expected == TimingCheckKind::Removal => {
            Some(clock_edge)
        }
        TargetTimingType::Combinational
        | TargetTimingType::ClockToQ(_)
        | TargetTimingType::Check { .. }
        | TargetTimingType::Recovery(_)
        | TargetTimingType::Removal(_)
        | TargetTimingType::Clear
        | TargetTimingType::Preset
        | TargetTimingType::MinPulseWidth
        | TargetTimingType::NonSequentialSetup(_)
        | TargetTimingType::NonSequentialHold(_)
        | TargetTimingType::ThreeStateEnable
        | TargetTimingType::ThreeStateDisable => None,
    }
}

fn clock_pin_delay(
    timing: &TimingContext,
    model: &TimingModel,
    clock: &Clock,
    net: usize,
    pin_object: &str,
    edge: TimingEdge,
    delay_type: DelayType,
) -> f64 {
    let source = clock.source_latency(edge, delay_type, delay_type == DelayType::Max);
    let network = if clock.is_propagated() {
        sink_interconnect_delay(
            timing,
            model,
            &model.graph,
            net,
            pin_object,
            InterconnectDelayMode::clock(edge, delay_type),
        )
    } else {
        clock.network_latency(edge, delay_type)
            * timing.timing_derate(TimingDerateKind::NetDelay, true, edge, delay_type)
    };
    source + network
}

const fn opposite_edge(edge: TimingEdge) -> TimingEdge {
    match edge {
        TimingEdge::Rise => TimingEdge::Fall,
        TimingEdge::Fall => TimingEdge::Rise,
    }
}
