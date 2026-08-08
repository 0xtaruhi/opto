// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn recompute_net(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &mut PropagationState,
) -> Result<(), crate::TimingError> {
    seed_net(inputs, net, state)?;
    propagate_into_net(inputs, net, state)
}

pub(super) fn seed_net(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &mut PropagationState,
) -> Result<(), crate::TimingError> {
    let PropagationState {
        arrivals,
        paths,
        origins,
        tags,
        ..
    } = state;
    let mut seeded = ArrivalRow::new();
    {
        let mut row = SeedRow {
            arrivals: &mut seeded,
            paths: paths.as_mut(),
            origins,
            tags,
        };
        seed_primary_inputs(inputs, net, &mut row)?;
        seed_sequential_outputs(inputs, net, &mut row)?;
    }
    if arrivals.replace_row(net, seeded).is_none() {
        return Err(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net }.into());
    }
    Ok(())
}

pub(super) fn seed_summary_slots(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    origins: &mut OriginArena,
    tags: &mut TagArena,
) -> Result<ArrivalRow, crate::TimingError> {
    let mut arrivals = ArrivalRow::new();
    let mut row = SeedRow {
        arrivals: &mut arrivals,
        paths: None,
        origins,
        tags,
    };
    seed_primary_inputs(inputs, net, &mut row)?;
    seed_sequential_outputs(inputs, net, &mut row)?;
    Ok(arrivals)
}

pub(super) fn propagate_summary_slots(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &PropagationState,
    slots: ArrivalRow,
) -> Result<ArrivalRow, crate::TimingError> {
    propagate_slots(
        inputs,
        net,
        &state.arrivals,
        &state.origins,
        slots,
        |_| Ok(None),
        &state.tags,
    )
}

pub(super) struct ArrivalTask {
    net: usize,
    slots: ArrivalRow,
    sources: Vec<(usize, ArrivalRow)>,
    launches: Vec<(OriginId, Option<f64>)>,
}

struct SeedRow<'a> {
    arrivals: &'a mut ArrivalRow,
    paths: Option<&'a mut PathArena>,
    origins: &'a mut OriginArena,
    tags: &'a mut TagArena,
}

impl ArrivalTask {
    pub(super) fn net(&self) -> usize {
        self.net
    }

    pub(super) fn prepare(
        inputs: &PropagationInputs<'_, '_>,
        arrivals: &ArrivalSlotStore,
        origins: &OriginArena,
        net: usize,
    ) -> Result<Self, crate::TimingError> {
        let mut sources = inputs.graph.incoming[net]
            .iter()
            .map(|&arc| inputs.graph.arc(arc).from.index())
            .collect::<Vec<_>>();
        sources.sort_unstable();
        sources.dedup();
        let sources = sources
            .into_iter()
            .map(|source| {
                arrivals
                    .row(source)
                    .map(|row| (source, row))
                    .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: source })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut origin_ids = sources
            .iter()
            .flat_map(|(_, edges)| edges.iter())
            .flat_map(|arrivals| arrivals.iter())
            .map(|arrival| arrival.origin)
            .collect::<Vec<_>>();
        origin_ids.sort_unstable();
        origin_ids.dedup();
        let launches = origin_ids
            .into_iter()
            .map(|origin| {
                origins.get(origin).map(|value| {
                    (
                        origin,
                        value.launch_clock.as_ref().map(|clock| clock.edge_time),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            net,
            slots: arrivals
                .row(net)
                .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?,
            sources,
            launches,
        })
    }

    pub(super) fn prepare_with_slots(
        inputs: &PropagationInputs<'_, '_>,
        arrivals: &ArrivalSlotStore,
        origins: &OriginArena,
        net: usize,
        slots: ArrivalRow,
    ) -> Result<Self, crate::TimingError> {
        Self::prepare(inputs, arrivals, origins, net).map(|mut task| {
            task.slots = slots;
            task
        })
    }

    pub(super) fn analyze(
        self,
        inputs: &PropagationInputs<'_, '_>,
        tags: &TagArena,
    ) -> Result<ArrivalRow, crate::TimingError> {
        let Self {
            net,
            slots,
            sources,
            launches,
        } = self;
        propagate_slots(
            inputs,
            net,
            sources.as_slice(),
            launches.as_slice(),
            slots,
            |_| Ok(None),
            tags,
        )
    }
}

enum ArrivalLookupIter<'a> {
    Store(ArrivalStateIter<'a>),
    Row(std::slice::Iter<'a, ArrivalState>),
}

impl Iterator for ArrivalLookupIter<'_> {
    type Item = ArrivalState;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Store(states) => states.next(),
            Self::Row(states) => states.next().copied(),
        }
    }
}

trait ArrivalLookup {
    fn states(&self, net: usize, edge: usize) -> Option<ArrivalLookupIter<'_>>;
}

impl ArrivalLookup for ArrivalSlotStore {
    fn states(&self, net: usize, edge: usize) -> Option<ArrivalLookupIter<'_>> {
        (net < self.len()).then(|| ArrivalLookupIter::Store(self.states(net, edge)))
    }
}

impl ArrivalLookup for [(usize, ArrivalRow)] {
    fn states(&self, net: usize, edge: usize) -> Option<ArrivalLookupIter<'_>> {
        self.binary_search_by_key(&net, |(source, _)| *source)
            .ok()
            .map(|index| ArrivalLookupIter::Row(self[index].1[edge].iter()))
    }
}

trait OriginLookup {
    fn launch(&self, origin: OriginId) -> Result<Option<f64>, crate::TimingError>;
}

impl OriginLookup for OriginArena {
    fn launch(&self, origin: OriginId) -> Result<Option<f64>, crate::TimingError> {
        self.get(origin)
            .map(|value| value.launch_clock.as_ref().map(|clock| clock.edge_time))
    }
}

impl OriginLookup for [(OriginId, Option<f64>)] {
    fn launch(&self, origin: OriginId) -> Result<Option<f64>, crate::TimingError> {
        self.binary_search_by_key(&origin, |(id, _)| *id)
            .ok()
            .map(|index| self[index].1)
            .ok_or_else(|| {
                crate::TimingAnalysisError::UnknownArrivalOrigin { id: origin.raw() }.into()
            })
    }
}

struct PropagationCandidate<'model> {
    instance: usize,
    output_pin: &'model str,
    arc: TargetTimingArcRef<'model>,
    source: ArrivalState,
    input_edge: TimingEdge,
    output_edge: TimingEdge,
    interconnect: crate::InterconnectPathContribution,
    interconnect_delay: f64,
    arc_delay: f64,
    delay: f64,
}

#[allow(
    clippy::too_many_lines,
    reason = "the propagation kernel keeps tag, origin, transition, path, and exception columns \
              synchronized while evaluating one graph arc"
)]
fn propagate_slots<F>(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    arrivals: &(impl ArrivalLookup + ?Sized),
    origins: &(impl OriginLookup + ?Sized),
    mut slots: ArrivalRow,
    mut build_path: F,
    tags: &TagArena,
) -> Result<ArrivalRow, crate::TimingError>
where
    F: FnMut(PropagationCandidate<'_>) -> Result<Option<PathId>, crate::TimingError>,
{
    for &graph_arc in &inputs.graph.incoming[net] {
        let graph_arc = inputs.graph.arc(graph_arc);
        let from = graph_arc.from.index();
        for input_edge in TimingEdge::ALL {
            let source = arrivals
                .states(from, input_edge.index())
                .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: from })?;
            for from_arrival in source {
                if let GraphArcKind::LatchData {
                    enable_net,
                    open_edge,
                    close_edge,
                } = graph_arc.kind
                    && !latch_data_is_transparent(
                        inputs,
                        origins.launch(from_arrival.origin)?,
                        from_arrival.delay,
                        enable_net.index(),
                        open_edge,
                        close_edge,
                    )?
                {
                    continue;
                }
                let source_transition = timing_input_transition(
                    inputs.timing,
                    inputs.graph,
                    &from_arrival,
                    from,
                    input_edge,
                    inputs.options.delay_type,
                );
                let input_transition = graph_arc
                    .interconnect_transition(input_edge)
                    .or(source_transition);
                let (output_pin, arc) = inputs
                    .graph
                    .cell_pin_arc(
                        inputs.library,
                        graph_arc.instance,
                        graph_arc.pin,
                        graph_arc.arc,
                    )
                    .ok_or(crate::TimingAnalysisError::UnknownArc {
                        instance: graph_arc.instance.raw() as usize,
                        pin: graph_arc.pin.index(),
                        arc: graph_arc.arc.index(),
                    })?;
                let instance_name = inputs
                    .model
                    .flat_instance(graph_arc.instance)
                    .expect("timing graph arcs reference live stable instance IDs")
                    .name;
                if timing_arc_is_disabled(inputs, instance_name, arc.related_pin(), output_pin) {
                    continue;
                }
                if !pin_case_allows(
                    inputs,
                    graph_arc.instance.raw() as usize,
                    arc.related_pin(),
                    input_edge,
                ) {
                    continue;
                }
                for &output_edge in arc.timing_sense().output_edges(input_edge) {
                    if !pin_case_allows(
                        inputs,
                        graph_arc.instance.raw() as usize,
                        output_pin,
                        output_edge,
                    ) {
                        continue;
                    }
                    let resistance = effective_resistance(
                        inputs.timing,
                        inputs.model,
                        inputs.graph,
                        from,
                        input_edge,
                        inputs.options.delay_type,
                    );
                    let load = timing_load(
                        inputs.timing,
                        inputs.graph,
                        from,
                        input_edge,
                        inputs.options.delay_type,
                    )
                    .unwrap_or(0.0);
                    let derate = inputs.timing.timing_derate(
                        TimingDerateKind::NetDelay,
                        false,
                        input_edge,
                        inputs.options.delay_type,
                    );
                    let interconnect = crate::InterconnectPathContribution {
                        net: crate::TimingNetId::from_index(from).map_err(|_| {
                            crate::TimingAnalysisError::Capacity {
                                resource: "timing path net ID",
                            }
                        })?,
                        fanout: inputs.graph.fanout_loads[from],
                        load,
                        resistance,
                        parasitic_delay: graph_arc.interconnect_delay(input_edge),
                        derate,
                    };
                    let interconnect_delay =
                        (interconnect.parasitic_delay + interconnect.wire_delay()) * derate;
                    let Some(evaluation) =
                        evaluate_timing_arc(inputs, net, arc, output_edge, input_transition)
                    else {
                        continue;
                    };
                    let candidate_delay =
                        from_arrival.delay + interconnect_delay + evaluation.delay;
                    let mut tag = from_arrival.tag;
                    for (point, edge) in arc_exception_steps(
                        inputs,
                        ArcExceptionTraversal {
                            instance: graph_arc.instance.raw() as usize,
                            related_pin: arc.related_pin(),
                            output_pin,
                            from_net: from,
                            to_net: net,
                            input_edge,
                            output_edge,
                        },
                    ) {
                        tag =
                            tags.advance(tag, inputs.timing, std::slice::from_ref(&point), edge)?;
                    }
                    let slot = &mut slots[output_edge.index()];
                    if !should_replace_arrival(
                        slot,
                        tag,
                        candidate_delay,
                        inputs.options.delay_type,
                    ) {
                        continue;
                    }
                    let path = build_path(PropagationCandidate {
                        instance: graph_arc.instance.raw() as usize,
                        output_pin,
                        arc,
                        source: from_arrival,
                        input_edge,
                        output_edge,
                        interconnect,
                        interconnect_delay,
                        arc_delay: evaluation.delay,
                        delay: candidate_delay,
                    })?;
                    replace_arrival(
                        slot,
                        ArrivalState {
                            tag,
                            origin: from_arrival.origin,
                            delay: candidate_delay,
                            transition: evaluation.transition,
                            path,
                        },
                    );
                }
            }
        }
    }
    Ok(slots)
}

pub(super) fn recompute_net_changed(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &mut PropagationState,
) -> Result<bool, crate::TimingError> {
    let previous = state
        .arrivals
        .row(net)
        .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?;
    recompute_net(inputs, net, state)?;
    let current = state
        .arrivals
        .row(net)
        .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?;
    Ok(!arrival_slots_match(&previous, &current))
}

pub(super) fn arrival_slots_match(left: &ArrivalRow, right: &ArrivalRow) -> bool {
    left.iter().zip(right.iter()).all(|(left, right)| {
        left.len() == right.len()
            && left.iter().zip(right).all(|(left, right)| {
                left.tag == right.tag
                    && left.origin == right.origin
                    && left.delay.to_bits() == right.delay.to_bits()
                    && left.transition.map(f64::to_bits) == right.transition.map(f64::to_bits)
            })
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "primary-input seeding is the canonical precedence table for clocks, input delays, \
              case analysis, derates, and path exceptions"
)]
fn seed_primary_inputs(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &mut SeedRow<'_>,
) -> Result<(), crate::TimingError> {
    for &port_index in &inputs.graph.primary_inputs[net] {
        let port = inputs
            .design
            .ports()
            .get(port_index)
            .ok_or(crate::TimingAnalysisError::UnknownPrimaryInput { index: port_index })?;
        if !matches_report_objects(&inputs.options.from, &port.name) {
            continue;
        }
        let delay_rows = inputs.timing.input_delays(port.id);
        let rows = if delay_rows.is_empty() {
            vec![(None, None, 0.0)]
        } else {
            delay_rows
                .iter()
                .enumerate()
                .flat_map(|(row_index, row)| {
                    TimingEdge::ALL.into_iter().filter_map(move |edge| {
                        row.delay(edge, inputs.options.delay_type)
                            .map(|delay| (Some(row_index), Some((row, edge)), delay))
                    })
                })
                .collect::<Vec<_>>()
        };
        for (delay_row, row_edge, delay) in rows {
            let (edge, launch_domain, launch_clock, clock_name, clock_edge_time) = match row_edge {
                Some((row, edge)) => match row.clock {
                    None => (edge, LaunchDomain::PrimaryInput, None, None, 0.0),
                    Some(clock_id) => {
                        let (clock_slot, clock) = inputs
                            .timing
                            .clock_entry(clock_id)
                            .expect("port-delay clock references are indexed and live");
                        let nominal_edge_time = clock.edge_time(row.clock_edge);
                        let source_latency = if row.source_latency_included {
                            0.0
                        } else {
                            clock.source_latency(
                                row.clock_edge,
                                inputs.options.delay_type,
                                inputs.options.delay_type == DelayType::Min,
                            )
                        };
                        let network_latency = if row.network_latency_included {
                            0.0
                        } else {
                            clock.network_latency(row.clock_edge, inputs.options.delay_type)
                                * inputs.timing.timing_derate(
                                    TimingDerateKind::NetDelay,
                                    true,
                                    row.clock_edge,
                                    inputs.options.delay_type,
                                )
                        };
                        let edge_time = nominal_edge_time + source_latency + network_latency;
                        (
                            edge,
                            LaunchDomain::Clock {
                                clock: clock_slot,
                                edge: row.clock_edge,
                            },
                            Some(LaunchClock {
                                edge_time: nominal_edge_time,
                                source_latency,
                            }),
                            Some(clock.name.as_str()),
                            edge_time,
                        )
                    }
                },
                None => (
                    TimingEdge::Rise,
                    LaunchDomain::PrimaryInput,
                    None,
                    None,
                    0.0,
                ),
            };
            let edges: &[TimingEdge] = if row_edge.is_none() {
                &TimingEdge::ALL
            } else {
                std::slice::from_ref(&edge)
            };
            for &edge in edges {
                if inputs
                    .timing
                    .timing_endpoint_is_disabled(TimingEndpoint::Port(port.id))
                {
                    continue;
                }
                if !inputs
                    .timing
                    .case_analysis_allows(TimingEndpoint::Port(port.id), edge)
                {
                    continue;
                }
                let origin = state.origins.intern(
                    OriginKey::PrimaryInput {
                        port: port_index,
                        delay_row,
                    },
                    ArrivalOrigin {
                        startpoint: port.name.clone(),
                        startpoint_description: "input port".to_string(),
                        launch_clock: launch_clock.clone(),
                    },
                )?;
                let points = startpoint_points(inputs, TimingEndpoint::Port(port.id), net)
                    .into_iter()
                    .map(|point| (point, edge))
                    .collect::<SmallVec<[_; 4]>>();
                let candidates =
                    initial_candidates(inputs.timing, &points, inputs.options.delay_type);
                let tag = state
                    .tags
                    .intern_family(inputs.timing, launch_domain, &candidates)?;
                let arrival_time = clock_edge_time + delay;
                let path = if let Some(paths) = state.paths.as_deref_mut() {
                    let mut steps = Vec::with_capacity(3);
                    if let Some(clock_name) = clock_name {
                        steps.push(PathStep {
                            point: format!(
                                "clock {} ({} edge)",
                                clock_name,
                                edge_name(
                                    row_edge
                                        .expect("a clock name only exists for a delay row")
                                        .0
                                        .clock_edge
                                )
                            ),
                            incr: clock_edge_time,
                            path: clock_edge_time,
                            edge: row_edge
                                .expect("a clock name only exists for a delay row")
                                .0
                                .clock_edge,
                            instance: None,
                            kind: crate::PathStepKind::Clock,
                            interconnect: None,
                        });
                    }
                    steps.extend([
                        PathStep {
                            point: "input external delay".to_string(),
                            incr: delay,
                            path: arrival_time,
                            edge,
                            instance: None,
                            kind: crate::PathStepKind::InputDelay,
                            interconnect: None,
                        },
                        PathStep {
                            point: format!("{} (in)", port.name),
                            incr: 0.0,
                            path: arrival_time,
                            edge,
                            instance: None,
                            kind: crate::PathStepKind::Point,
                            interconnect: None,
                        },
                    ]);
                    Some(paths.chain(None, steps)?)
                } else {
                    None
                };
                state.arrivals[edge.index()].push(ArrivalState {
                    tag,
                    origin,
                    delay: arrival_time,
                    transition: explicit_transition_for(
                        inputs.timing,
                        inputs.graph,
                        net,
                        edge,
                        inputs.options.delay_type,
                    ),
                    path,
                });
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "sequential seeding evaluates the complete clock-to-Q launch equation and publishes \
              its origin and predecessor metadata as one unit"
)]
fn seed_sequential_outputs(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &mut SeedRow<'_>,
) -> Result<(), crate::TimingError> {
    for (launch_index, launch) in inputs.graph.sequential_outputs[net].iter().enumerate() {
        let instance = inputs.model.instance_ref(launch.instance).ok_or(
            crate::TimingAnalysisError::UnknownInstance {
                index: launch.instance.raw() as usize,
            },
        )?;
        let (output_pin, arc) = inputs
            .graph
            .cell_pin_arc(inputs.library, launch.instance, launch.pin, launch.arc)
            .ok_or(crate::TimingAnalysisError::UnknownClockToQArc {
                instance: launch.instance.raw() as usize,
                pin: launch.pin.index(),
                arc: launch.arc.index(),
            })?;
        let TargetTimingType::ClockToQ(clock_edge) = arc.timing_type() else {
            return Err(crate::TimingAnalysisError::NonClockToQArc.into());
        };
        let instance_name = instance.name();
        if timing_arc_is_disabled(inputs, &instance_name, arc.related_pin(), output_pin) {
            continue;
        }
        let output_object = format!("{instance_name}/{output_pin}");
        if !matches_any_timing_object(
            &inputs.options.from,
            [instance_name.as_ref(), output_object.as_str()],
        ) {
            continue;
        }
        for (clock_index, clock) in
            clocks_on_net(inputs.timing, inputs.graph, launch.clock_net.index())
        {
            let clock_edge_time = clock.edge_time(clock_edge);
            let clock_point = format!("{instance_name}/{}", arc.related_pin());
            let source_latency = clock.source_latency(
                clock_edge,
                inputs.options.delay_type,
                inputs.options.delay_type == DelayType::Min,
            );
            let clock_network_delay = if clock.is_propagated() {
                sink_interconnect_delay(
                    inputs.timing,
                    inputs.model,
                    inputs.graph,
                    launch.clock_net.index(),
                    &clock_point,
                    InterconnectDelayMode::clock(clock_edge, inputs.options.delay_type),
                )
            } else {
                clock.network_latency(clock_edge, inputs.options.delay_type)
                    * inputs.timing.timing_derate(
                        TimingDerateKind::NetDelay,
                        true,
                        clock_edge,
                        inputs.options.delay_type,
                    )
            };
            let launch_edge_time = clock_edge_time + source_latency + clock_network_delay;
            let clock_transition = clock.transition(clock_edge, inputs.options.delay_type);
            let origin = state.origins.intern(
                OriginKey::Sequential {
                    net,
                    launch: launch_index,
                    clock: clock_index,
                },
                ArrivalOrigin {
                    startpoint: instance_name.to_string(),
                    startpoint_description: sequential_description(
                        launch.element,
                        clock_edge,
                        &clock.name,
                    ),
                    launch_clock: Some(LaunchClock {
                        edge_time: clock_edge_time,
                        source_latency,
                    }),
                },
            )?;
            for &output_edge in arc.timing_sense().output_edges(clock_edge) {
                let points =
                    sequential_startpoint_points(inputs, clock.id, &instance_name, output_pin, net)
                        .into_iter()
                        .map(|point| {
                            let edge = if matches!(point, TimingEndpoint::Clock(_)) {
                                clock_edge
                            } else {
                                output_edge
                            };
                            (point, edge)
                        })
                        .collect::<SmallVec<[_; 4]>>();
                let candidates =
                    initial_candidates(inputs.timing, &points, inputs.options.delay_type);
                let tag = state.tags.intern_family(
                    inputs.timing,
                    LaunchDomain::Clock {
                        clock: clock_index,
                        edge: clock_edge,
                    },
                    &candidates,
                )?;
                let Some(evaluation) =
                    evaluate_timing_arc(inputs, net, arc, output_edge, clock_transition)
                else {
                    continue;
                };
                let delay = evaluation.delay;
                let arrival_time = launch_edge_time + delay;
                let transition = evaluation.transition;
                let slot = &mut state.arrivals[output_edge.index()];
                if !should_replace_arrival(slot, tag, arrival_time, inputs.options.delay_type) {
                    continue;
                }
                let path = if let Some(paths) = state.paths.as_deref_mut() {
                    Some(paths.chain(
                        None,
                        [
                            PathStep {
                                point: format!(
                                    "clock {} ({} edge)",
                                    clock.name,
                                    edge_name(clock_edge)
                                ),
                                incr: clock_edge_time,
                                path: clock_edge_time,
                                edge: clock_edge,
                                instance: None,
                                kind: crate::PathStepKind::Clock,
                                interconnect: None,
                            },
                            PathStep {
                                point: "clock source latency".to_string(),
                                incr: source_latency,
                                path: clock_edge_time + source_latency,
                                edge: clock_edge,
                                instance: None,
                                kind: crate::PathStepKind::Clock,
                                interconnect: None,
                            },
                            PathStep {
                                point: if clock_network_delay == 0.0 {
                                    "clock network delay (ideal)".to_string()
                                } else {
                                    "clock network delay (propagated)".to_string()
                                },
                                incr: clock_network_delay,
                                path: launch_edge_time,
                                edge: clock_edge,
                                instance: None,
                                kind: crate::PathStepKind::Clock,
                                interconnect: None,
                            },
                            PathStep {
                                point: format!(
                                    "{}/{} ({})",
                                    instance_name,
                                    arc.related_pin(),
                                    instance.cell()
                                ),
                                incr: 0.0,
                                path: launch_edge_time,
                                edge: clock_edge,
                                instance: Some(instance.id()),
                                kind: crate::PathStepKind::Point,
                                interconnect: None,
                            },
                            PathStep {
                                point: format!("{} ({})", output_object, instance.cell()),
                                incr: delay,
                                path: arrival_time,
                                edge: output_edge,
                                instance: Some(instance.id()),
                                kind: crate::PathStepKind::CellArc,
                                interconnect: None,
                            },
                        ],
                    )?)
                } else {
                    None
                };
                replace_arrival(
                    slot,
                    ArrivalState {
                        tag,
                        origin,
                        delay: arrival_time,
                        transition,
                        path,
                    },
                );
            }
        }
    }
    Ok(())
}

pub(super) fn propagate_into_net(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    state: &mut PropagationState,
) -> Result<(), crate::TimingError> {
    let slots = state
        .arrivals
        .row(net)
        .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net })?;
    let arrivals = &state.arrivals;
    let origins = &state.origins;
    let paths = &mut state.paths;
    let tags = &state.tags;
    let slots = propagate_slots(
        inputs,
        net,
        arrivals,
        origins,
        slots,
        |candidate| {
            let Some(paths) = paths.as_mut() else {
                return Ok(None);
            };
            let instance_id = TimingInstanceId::from_raw(
                u32::try_from(candidate.instance)
                    .expect("timing graph instance identifiers originate from u32"),
            );
            let instance = inputs.model.instance_ref(instance_id).ok_or(
                crate::TimingAnalysisError::UnknownInstance {
                    index: candidate.instance,
                },
            )?;
            let instance_name = instance.name();
            let instance_cell = instance.cell();
            let output_step = PathStep {
                point: format!(
                    "{}/{} ({})",
                    instance_name, candidate.output_pin, instance_cell
                ),
                incr: candidate.arc_delay,
                path: candidate.delay,
                edge: candidate.output_edge,
                instance: Some(instance.id()),
                kind: crate::PathStepKind::CellArc,
                interconnect: None,
            };
            let previous = candidate
                .source
                .path
                .ok_or(crate::TimingAnalysisError::EmptyPath {
                    operation: "propagate",
                })?;
            if candidate.interconnect_delay == 0.0 {
                Ok(Some(paths.push(Some(previous), output_step)?))
            } else {
                Ok(Some(paths.chain(
                    Some(previous),
                    [
                        PathStep {
                            point: format!(
                                "{}/{} ({})",
                                instance_name,
                                candidate.arc.related_pin(),
                                instance_cell
                            ),
                            incr: candidate.interconnect_delay,
                            path: candidate.source.delay + candidate.interconnect_delay,
                            edge: candidate.input_edge,
                            instance: None,
                            kind: crate::PathStepKind::Interconnect,
                            interconnect: Some(candidate.interconnect),
                        },
                        output_step,
                    ],
                )?))
            }
        },
        tags,
    )?;
    if state.arrivals.replace_row(net, slots).is_none() {
        return Err(crate::TimingAnalysisError::DirtyNetOutOfRange { index: net }.into());
    }
    Ok(())
}

fn should_replace_arrival(
    arrivals: &ArrivalEdge,
    tag: TagId,
    candidate_delay: f64,
    delay_type: DelayType,
) -> bool {
    arrivals
        .iter()
        .find(|arrival| arrival.tag == tag)
        .is_none_or(|current| match delay_type {
            DelayType::Max => candidate_delay > current.delay,
            DelayType::Min => candidate_delay < current.delay,
        })
}

fn replace_arrival(arrivals: &mut ArrivalEdge, candidate: ArrivalState) {
    if let Some(current) = arrivals
        .iter_mut()
        .find(|arrival| arrival.tag == candidate.tag)
    {
        *current = candidate;
    } else {
        arrivals.push(candidate);
    }
}
