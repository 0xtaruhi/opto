// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Endpoint requirements indexed by graph net so single-net required
/// recomputation does not rescan the whole design.
pub(super) struct RequiredEndpoints<'model> {
    output_ports: opto_core::PackedRows<&'model TimingPort>,
    checks: opto_core::PackedRows<CheckEndpoint<'model>>,
}

pub(super) struct CheckEndpoint<'model> {
    constraint: TargetTimingArcRef<'model>,
    check_kind: TimingCheckKind,
    clock_edge: TimingEdge,
    clock_net: usize,
    instance: crate::TimingInstanceRef<'model>,
    data_pin: TargetPinRef<'model>,
}

#[derive(Clone, Copy)]
pub(super) struct RequiredSources<'a> {
    arrivals: &'a ArrivalSlotStore,
    origins: &'a OriginArena,
    tags: &'a TagArena,
}

impl<'a> RequiredSources<'a> {
    pub(super) const fn new(
        arrivals: &'a ArrivalSlotStore,
        origins: &'a OriginArena,
        tags: &'a TagArena,
    ) -> Self {
        Self {
            arrivals,
            origins,
            tags,
        }
    }
}

impl<'a> From<&'a PropagationState> for RequiredSources<'a> {
    fn from(state: &'a PropagationState) -> Self {
        Self {
            arrivals: &state.arrivals,
            origins: &state.origins,
            tags: &state.tags,
        }
    }
}

pub(super) struct RequiredTask {
    net: usize,
    downstream: Vec<(usize, RequiredRow)>,
}

impl RequiredTask {
    pub(super) fn net(&self) -> usize {
        self.net
    }

    pub(super) fn prepare(graph: &TimingGraph, requireds: &RequiredSlotStore, net: usize) -> Self {
        let mut downstream = graph.outgoing[net]
            .iter()
            .map(|&arc| graph.arc(arc).to.index())
            .collect::<Vec<_>>();
        downstream.sort_unstable();
        downstream.dedup();
        Self {
            net,
            downstream: downstream
                .into_iter()
                .map(|to| {
                    (
                        to,
                        requireds
                            .row(to)
                            .expect("graph fanout references a live required row"),
                    )
                })
                .collect(),
        }
    }

    pub(super) fn analyze(
        self,
        inputs: &PropagationInputs<'_, '_>,
        endpoints: &RequiredEndpoints<'_>,
        sources: RequiredSources<'_>,
    ) -> Result<RequiredRow, crate::TimingError> {
        required_slots_for_net_with(
            inputs,
            endpoints,
            self.net,
            sources,
            self.downstream.as_slice(),
        )
    }
}

trait RequiredLookup {
    fn states(&self, net: usize, edge: usize) -> Option<RequiredLookupIter<'_>>;
}

enum RequiredLookupIter<'a> {
    Resident(RequiredStateIter<'a>),
    Snapshot(std::iter::Copied<std::slice::Iter<'a, RequiredState>>),
}

impl Iterator for RequiredLookupIter<'_> {
    type Item = RequiredState;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Resident(states) => states.next(),
            Self::Snapshot(states) => states.next(),
        }
    }
}

impl RequiredLookup for RequiredSlotStore {
    fn states(&self, net: usize, edge: usize) -> Option<RequiredLookupIter<'_>> {
        (net < self.len())
            .then(|| RequiredLookupIter::Resident(RequiredSlotStore::states(self, net, edge)))
    }
}

impl RequiredLookup for [(usize, RequiredRow)] {
    fn states(&self, net: usize, edge: usize) -> Option<RequiredLookupIter<'_>> {
        self.binary_search_by_key(&net, |(to, _)| *to)
            .ok()
            .map(|index| RequiredLookupIter::Snapshot(self[index].1[edge].iter().copied()))
    }
}

impl<'model> RequiredEndpoints<'model> {
    pub(super) fn build(
        inputs: &PropagationInputs<'_, 'model>,
    ) -> Result<RequiredEndpoints<'model>, crate::TimingError> {
        let net_count = inputs.graph.net_count();
        let mut output_ports = Vec::new();
        for (port_index, port) in inputs.design.ports().iter().enumerate() {
            if port.direction != TimingPortDirection::Output
                || !matches_report_objects(&inputs.options.to, &port.name)
                || inputs
                    .timing
                    .timing_endpoint_is_disabled(TimingEndpoint::Port(port.id))
            {
                continue;
            }
            if let Some(net) = inputs
                .graph
                .port_net(port_index)
                .map(crate::TimingNetId::index)
            {
                output_ports.push((net, port));
            }
        }
        let check_kinds = enabled_timing_check_kinds(inputs.options).collect::<Vec<_>>();
        let mut checks = Vec::new();
        let mut cell_has_checks = vec![None; inputs.library.cells.len()];
        for instance in inputs.model.instances() {
            let cell = match inputs.graph.instance_cell_index(instance.id()) {
                Some(index) => {
                    let has_checks = *cell_has_checks[index.index()].get_or_insert_with(|| {
                        inputs.library.cells.get(index.index()).is_some_and(|cell| {
                            check_kinds
                                .iter()
                                .copied()
                                .any(|kind| timing_check_arcs(cell, kind).next().is_some())
                        })
                    });
                    if !has_checks {
                        continue;
                    }
                    inputs.library.cells.get(index.index())
                }
                None => inputs.graph.cell(inputs.library, instance.id()),
            };
            let Some(cell) = cell else {
                continue;
            };
            let connections = connection_map_ref(instance);
            let instance_name = instance.name();
            for &check_kind in &check_kinds {
                for (data_pin, constraint, clock_edge) in timing_check_arcs(cell, check_kind) {
                    if timing_arc_is_disabled(
                        inputs,
                        &instance_name,
                        constraint.related_pin(),
                        data_pin.name(),
                    ) {
                        continue;
                    }
                    if !inputs.options.to.is_empty() {
                        let data_object = format!("{instance_name}/{}", data_pin.name());
                        if !matches_any_timing_object(
                            &inputs.options.to,
                            [instance_name.as_ref(), data_object.as_str()],
                        ) {
                            continue;
                        }
                    }
                    let (Some(data_net), Some(clock_net)) = (
                        connections.get(data_pin.name()),
                        connections.get(constraint.related_pin()),
                    ) else {
                        continue;
                    };
                    checks.push((
                        data_net.index(),
                        CheckEndpoint {
                            constraint,
                            check_kind,
                            clock_edge,
                            clock_net: clock_net.index(),
                            instance,
                            data_pin,
                        },
                    ));
                }
            }
        }
        let capacity = |_| crate::TimingAnalysisError::Capacity {
            resource: "required endpoint rows",
        };
        Ok(RequiredEndpoints {
            output_ports: opto_core::PackedRows::try_from_entries(net_count, output_ports)
                .map_err(capacity)?,
            checks: opto_core::PackedRows::try_from_entries(net_count, checks).map_err(capacity)?,
        })
    }
}

pub(super) fn recompute_required(
    inputs: &PropagationInputs<'_, '_>,
    dirty: &[bool],
    state: &mut PropagationState,
    runtime: Option<&opto_runtime::ExecutionContext>,
) -> Result<(), crate::TimingError> {
    let endpoints = RequiredEndpoints::build(inputs)?;
    let mut worklist = inputs.graph.propagation_worklist(
        opto_runtime::DependencyDirection::Reverse,
        dirty
            .iter()
            .enumerate()
            .filter_map(|(net, &dirty)| dirty.then_some(net)),
    )?;
    if let Some(runtime) = runtime {
        let sources = RequiredSources::new(&state.arrivals, &state.origins, &state.tags);
        let publication =
            opto_runtime::DependencyPublicationPlan::identity(inputs.graph.net_count());
        return runtime
            .publish_dependency_rows(
                worklist,
                &mut state.requireds,
                opto_runtime::DependencyRun::new(
                    &publication,
                    opto_runtime::DependencyActivation::all(),
                ),
                |requireds, net| Ok(RequiredTask::prepare(inputs.graph, requireds, net)),
                |task| {
                    let net = task.net();
                    task.analyze(inputs, &endpoints, sources)
                        .map(|slots| opto_runtime::DependencyPublication::row(net, slots))
                },
            )
            .map(|_| ());
    }
    // Reverse dependency readiness guarantees every fanout required time has
    // been published before a net is analyzed. Dirty filtering skips unchanged
    // rows, but all claimed nodes must still be finished to release predecessors.
    while let Some(claimed) = worklist.claim_ready()? {
        let nets = claimed
            .iter()
            .copied()
            .filter(|&net| dirty[net])
            .collect::<Vec<_>>();
        let analyze = |position: usize| {
            let net = nets[position];
            required_slots_for_net(inputs, &endpoints, net, state).map(|slots| (net, slots))
        };
        let computed = (0..nets.len())
            .map(analyze)
            .collect::<Result<Vec<_>, _>>()?;
        for (net, slots) in computed {
            state
                .requireds
                .replace_row(net, slots)
                .expect("claimed timing net owns a live required row");
        }
        for net in claimed {
            worklist.finish(net)?;
        }
    }
    Ok(())
}

pub(super) fn required_slots_for_net(
    inputs: &PropagationInputs<'_, '_>,
    endpoints: &RequiredEndpoints<'_>,
    net: usize,
    state: &PropagationState,
) -> Result<RequiredRow, crate::TimingError> {
    required_slots_for_net_with(inputs, endpoints, net, state.into(), &state.requireds)
}

fn required_slots_for_net_with(
    inputs: &PropagationInputs<'_, '_>,
    endpoints: &RequiredEndpoints<'_>,
    net: usize,
    sources: RequiredSources<'_>,
    requireds: &(impl RequiredLookup + ?Sized),
) -> Result<RequiredRow, crate::TimingError> {
    let mut slots = RequiredRow::new();
    seed_output_port_required(inputs, endpoints, net, sources, &mut slots)?;
    seed_check_required(inputs, endpoints, net, sources, &mut slots)?;
    propagate_required_from_fanout(inputs, net, sources, requireds, &mut slots)?;
    Ok(slots)
}

pub(super) fn required_slots_match(left: &RequiredRow, right: &RequiredRow) -> bool {
    left.iter().zip(right.iter()).all(|(left, right)| {
        left.len() == right.len()
            && left.iter().zip(right).all(|(left, right)| {
                left.tag == right.tag && left.required.to_bits() == right.required.to_bits()
            })
    })
}

pub(super) fn seed_output_port_required(
    inputs: &PropagationInputs<'_, '_>,
    endpoints: &RequiredEndpoints<'_>,
    net: usize,
    sources: RequiredSources<'_>,
    slots: &mut RequiredRow,
) -> Result<(), crate::TimingError> {
    let ports = endpoints.output_ports.get(net).unwrap_or_default();
    if ports.is_empty() {
        return Ok(());
    }
    for port in ports {
        for edge in TimingEdge::ALL {
            let endpoint_points = output_endpoint_points(inputs, port.id, net, edge);
            for arrival in sources.arrivals.states(net, edge.index()) {
                let key = sources.tags.key(arrival.tag)?;
                let resolved = resolve_path_exception(
                    inputs.timing,
                    &key.path_exceptions,
                    &endpoint_points,
                    edge,
                    inputs.options.delay_type,
                )?;
                if resolved.as_ref().is_some_and(|resolved| {
                    matches!(resolved.exception.kind, PathExceptionKind::FalsePath)
                }) {
                    continue;
                }
                let requirements = output_port_requirements(
                    inputs,
                    port,
                    &arrival,
                    key,
                    edge,
                    sources.origins,
                    resolved.as_ref().map(|resolved| &resolved.exception.kind),
                );
                for required in requirements {
                    let required = required
                        - sink_interconnect_delay(
                            inputs.timing,
                            inputs.model,
                            inputs.graph,
                            net,
                            &port.name,
                            InterconnectDelayMode::data(edge, inputs.options.delay_type),
                        );
                    merge_required(
                        &mut slots[edge.index()],
                        arrival.tag,
                        required,
                        inputs.options.delay_type,
                    );
                }
            }
        }
    }
    Ok(())
}

pub(super) fn output_port_requirements(
    inputs: &PropagationInputs<'_, '_>,
    port: &TimingPort,
    arrival: &ArrivalState,
    key: &TagKey,
    edge: TimingEdge,
    origins: &OriginArena,
    exception: Option<&PathExceptionKind>,
) -> Vec<f64> {
    let exception_required = match exception {
        Some(PathExceptionKind::MaxDelay { delay })
            if inputs.options.delay_type == DelayType::Max =>
        {
            Some(*delay)
        }
        Some(PathExceptionKind::MinDelay { delay })
            if inputs.options.delay_type == DelayType::Min =>
        {
            Some(*delay)
        }
        Some(PathExceptionKind::MultiCycle {
            cycles,
            use_end_clock: _,
        }) if inputs.options.delay_type == DelayType::Max => match key.launch_domain {
            LaunchDomain::Clock { clock, edge } => inputs
                .timing
                .clock_by_slot(clock)
                .map(|clock| clock.edge_time(edge) + f64::from(*cycles) * clock.period),
            LaunchDomain::PrimaryInput => None,
        },
        _ => None,
    };
    if let Some(required) = exception_required {
        return vec![required];
    }
    inputs
        .timing
        .output_delays(port.id)
        .iter()
        .filter_map(|row| {
            let delay = row.delay(edge, inputs.options.delay_type)?;
            let target = match row.clock {
                Some(clock_id) => {
                    let (_, clock) = inputs.timing.clock_entry(clock_id)?;
                    let launch_time = origins
                        .get(arrival.origin)
                        .ok()
                        .and_then(|origin| origin.launch_clock.as_ref())
                        .map(|clock| clock.edge_time);
                    let nominal = match (inputs.options.delay_type, launch_time) {
                        (DelayType::Max, Some(launch)) => {
                            clock.next_edge_after(row.clock_edge, launch)
                        }
                        (DelayType::Min, Some(launch)) => {
                            clock.edge_at_or_after(row.clock_edge, launch)
                        }
                        (DelayType::Max, None) => clock.edge_time(row.clock_edge) + clock.period,
                        (DelayType::Min, None) => clock.edge_time(row.clock_edge),
                    };
                    let uncertainty = match key.launch_domain {
                        LaunchDomain::Clock {
                            clock: launch_clock,
                            edge: launch_edge,
                        } => inputs
                            .timing
                            .clock_by_slot(launch_clock)
                            .map_or(0.0, |launch| {
                                inputs.timing.clock_uncertainty(
                                    launch.id,
                                    launch_edge,
                                    clock.id,
                                    row.clock_edge,
                                    inputs.options.delay_type,
                                )
                            }),
                        LaunchDomain::PrimaryInput => 0.0,
                    };
                    let crpr = match key.launch_domain {
                        LaunchDomain::Clock {
                            clock: launch_clock,
                            ..
                        } if inputs
                            .timing
                            .clock_by_slot(launch_clock)
                            .is_some_and(|launch| launch.id == clock.id) =>
                        {
                            origins
                                .get(arrival.origin)
                                .ok()
                                .and_then(|origin| origin.launch_clock.as_ref())
                                .map_or(0.0, |launch| launch.source_latency)
                        }
                        LaunchDomain::Clock { .. } | LaunchDomain::PrimaryInput => 0.0,
                    };
                    let target = nominal + crpr;
                    Some(match inputs.options.delay_type {
                        DelayType::Max => target - uncertainty,
                        DelayType::Min => target + uncertainty,
                    })
                }
                None => Some(0.0),
            }?;
            Some(target - delay)
        })
        .collect()
}

#[derive(Clone, Copy)]
pub(super) struct CheckRequirementTarget<'model, 'object> {
    pub(super) constraint: TargetTimingArcRef<'model>,
    pub(super) check_kind: TimingCheckKind,
    pub(super) clock_edge: TimingEdge,
    pub(super) clock_net: usize,
    pub(super) instance_name: &'object str,
    pub(super) data_pin: &'object str,
    pub(super) data_net: usize,
}

pub(super) struct CheckRequirement<'timing> {
    pub(super) required: f64,
    pub(super) constraint: f64,
    pub(super) capture_source_time: f64,
    pub(super) clock_network_delay: f64,
    pub(super) path_exception: Option<crate::constraints::ResolvedPathException<'timing>>,
}

impl CheckRequirement<'_> {
    pub(super) fn capture_edge_time(&self) -> f64 {
        self.capture_source_time + self.clock_network_delay
    }
}

/// Evaluate the requirement of one concrete timing-check endpoint.
///
/// This deliberately does not consult the graph-wide required-time frontier:
/// that frontier merges all downstream endpoints and may be deferred during an
/// optimization transaction. Endpoint `QoR` must remain exact from the current
/// arrivals and constraints even while the frontier is stale.
#[allow(
    clippy::too_many_lines,
    reason = "this function is the canonical setup/hold/recovery/removal requirement equation, \
              including multicycle, uncertainty, CRPR, and latch borrowing terms"
)]
pub(super) fn check_requirement<'timing>(
    inputs: &PropagationInputs<'timing, '_>,
    target: CheckRequirementTarget<'_, '_>,
    capture: &'timing Clock,
    data_edge: TimingEdge,
    arrival: &ArrivalState,
    key: &TagKey,
    origins: &OriginArena,
) -> Result<Option<CheckRequirement<'timing>>, crate::TimingError> {
    let LaunchDomain::Clock {
        clock: launch_clock,
        edge: launch_edge,
    } = key.launch_domain
    else {
        return Ok(None);
    };
    let Some(launch) = inputs.timing.clock_by_slot(launch_clock) else {
        return Ok(None);
    };
    let launch_edge_time = launch.edge_time(launch_edge);
    let endpoint_points = check_endpoint_points(
        inputs,
        capture.id,
        target.clock_edge,
        target.instance_name,
        target.data_pin,
        target.data_net,
        data_edge,
    );
    let path_exception = resolve_path_exception(
        inputs.timing,
        &key.path_exceptions,
        &endpoint_points,
        data_edge,
        inputs.options.delay_type,
    )?;
    if path_exception
        .is_some_and(|resolved| matches!(resolved.exception.kind, PathExceptionKind::FalsePath))
    {
        return Ok(None);
    }
    let Some(constraint) = target.constraint.constraint_at(
        data_edge,
        capture.transition(target.clock_edge, inputs.options.delay_type),
        arrival.transition,
    ) else {
        return Ok(None);
    };
    let constraint = constraint
        * inputs.timing.timing_derate(
            TimingDerateKind::CellCheck,
            false,
            data_edge,
            inputs.options.delay_type,
        );
    let capture_source_latency = capture.source_latency(
        target.clock_edge,
        inputs.options.delay_type,
        inputs.options.delay_type == DelayType::Max,
    );
    let source_crpr = if launch.id == capture.id {
        origins
            .get(arrival.origin)?
            .launch_clock
            .as_ref()
            .map_or(0.0, |clock| clock.source_latency)
            - capture_source_latency
    } else {
        0.0
    };
    let uncertainty = inputs.timing.clock_uncertainty(
        launch.id,
        launch_edge,
        capture.id,
        target.clock_edge,
        inputs.options.delay_type,
    );
    let capture_source_time = if check_is_upper_bound(target.check_kind) {
        capture.next_edge_after(target.clock_edge, launch_edge_time)
            + capture_source_latency
            + source_crpr
    } else {
        capture.edge_at_or_after(target.clock_edge, launch_edge_time)
            + capture_source_latency
            + source_crpr
    };
    let baseline = if check_is_upper_bound(target.check_kind) {
        capture_source_time - constraint - uncertainty
    } else {
        capture_source_time + constraint + uncertainty
    };
    let mut required = match path_exception.map(|resolved| &resolved.exception.kind) {
        Some(PathExceptionKind::MaxDelay { delay })
            if inputs.options.delay_type == DelayType::Max =>
        {
            launch_edge_time + *delay - constraint
        }
        Some(PathExceptionKind::MinDelay { delay })
            if inputs.options.delay_type == DelayType::Min =>
        {
            launch_edge_time + *delay + constraint
        }
        _ => baseline,
    };
    let has_path_delay = path_exception.is_some_and(|resolved| {
        matches!(
            resolved.exception.kind,
            PathExceptionKind::MaxDelay { .. } | PathExceptionKind::MinDelay { .. }
        )
    });
    if !has_path_delay {
        match target.check_kind {
            TimingCheckKind::Setup => {
                if let Some(multicycle) = resolve_multicycle(
                    inputs.timing,
                    &key.path_exceptions,
                    &endpoint_points,
                    data_edge,
                    ExceptionCorner::Setup,
                )? {
                    let period = if multicycle.use_end_clock {
                        capture.period
                    } else {
                        launch.period
                    };
                    required += f64::from(multicycle.cycles.saturating_sub(1)) * period;
                }
            }
            TimingCheckKind::Hold => {
                let setup = resolve_multicycle(
                    inputs.timing,
                    &key.path_exceptions,
                    &endpoint_points,
                    data_edge,
                    ExceptionCorner::Setup,
                )?;
                let hold = resolve_multicycle(
                    inputs.timing,
                    &key.path_exceptions,
                    &endpoint_points,
                    data_edge,
                    ExceptionCorner::Hold,
                )?;
                let adjustment = setup.map_or(0.0, |multicycle| {
                    let period = if multicycle.use_end_clock {
                        capture.period
                    } else {
                        launch.period
                    };
                    f64::from(multicycle.cycles.saturating_sub(1)) * period
                }) - hold.map_or(0.0, |multicycle| {
                    let period = if multicycle.use_end_clock {
                        capture.period
                    } else {
                        launch.period
                    };
                    f64::from(multicycle.cycles) * period
                });
                required += adjustment;
            }
            TimingCheckKind::Recovery | TimingCheckKind::Removal => {}
        }
    }
    let include_clock_latency = !path_exception.is_some_and(|resolved| {
        matches!(
            resolved.exception.kind,
            PathExceptionKind::MaxDelay { .. } | PathExceptionKind::MinDelay { .. }
        ) && resolved.exception.ignore_clock_latency
    });
    let clock_network_delay = if include_clock_latency {
        if capture.is_propagated() {
            sink_interconnect_delay_parts(
                inputs.timing,
                inputs.model,
                inputs.graph,
                target.clock_net,
                target.instance_name,
                target.constraint.related_pin(),
                InterconnectDelayMode::clock(target.clock_edge, inputs.options.delay_type),
            )
        } else {
            capture.network_latency(target.clock_edge, inputs.options.delay_type)
                * inputs.timing.timing_derate(
                    TimingDerateKind::NetDelay,
                    true,
                    target.clock_edge,
                    inputs.options.delay_type,
                )
        }
    } else {
        0.0
    };
    required += clock_network_delay;
    Ok(Some(CheckRequirement {
        required,
        constraint,
        capture_source_time,
        clock_network_delay,
        path_exception,
    }))
}

pub(super) fn seed_check_required(
    inputs: &PropagationInputs<'_, '_>,
    endpoints: &RequiredEndpoints<'_>,
    net: usize,
    sources: RequiredSources<'_>,
    slots: &mut RequiredRow,
) -> Result<(), crate::TimingError> {
    let checks = endpoints.checks.get(net).unwrap_or_default();
    if checks.is_empty() {
        return Ok(());
    }
    for check in checks {
        let instance_name = check.instance.name();
        let data_pin = check.data_pin.name();
        let target = CheckRequirementTarget {
            constraint: check.constraint,
            check_kind: check.check_kind,
            clock_edge: check.clock_edge,
            clock_net: check.clock_net,
            instance_name: &instance_name,
            data_pin,
            data_net: net,
        };
        for data_edge in TimingEdge::ALL {
            for arrival in sources.arrivals.states(net, data_edge.index()) {
                let key = sources.tags.key(arrival.tag)?;
                for (_, capture) in clocks_on_net(inputs.timing, inputs.graph, check.clock_net) {
                    let Some(requirement) = check_requirement(
                        inputs,
                        target,
                        capture,
                        data_edge,
                        &arrival,
                        key,
                        sources.origins,
                    )?
                    else {
                        continue;
                    };
                    let required = requirement.required
                        - sink_interconnect_delay_parts(
                            inputs.timing,
                            inputs.model,
                            inputs.graph,
                            net,
                            &instance_name,
                            data_pin,
                            InterconnectDelayMode::data(data_edge, inputs.options.delay_type),
                        );
                    merge_required(
                        &mut slots[data_edge.index()],
                        arrival.tag,
                        required,
                        inputs.options.delay_type,
                    );
                }
            }
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "backward propagation updates required time, exception progress, latch transparency, \
              and arc delay together for each fanout state"
)]
fn propagate_required_from_fanout(
    inputs: &PropagationInputs<'_, '_>,
    net: usize,
    sources: RequiredSources<'_>,
    requireds: &(impl RequiredLookup + ?Sized),
    slots: &mut RequiredRow,
) -> Result<(), crate::TimingError> {
    for &graph_arc in &inputs.graph.outgoing[net] {
        let graph_arc = inputs.graph.arc(graph_arc);
        let to = graph_arc.to.index();
        for input_edge in TimingEdge::ALL {
            for arrival in sources.arrivals.states(net, input_edge.index()) {
                if let GraphArcKind::LatchData {
                    enable_net,
                    open_edge,
                    close_edge,
                } = graph_arc.kind
                    && !latch_data_is_transparent(
                        inputs,
                        sources
                            .origins
                            .get(arrival.origin)?
                            .launch_clock
                            .as_ref()
                            .map(|clock| clock.edge_time),
                        arrival.delay,
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
                    &arrival,
                    net,
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
                for &output_edge in arc.timing_sense().output_edges(input_edge) {
                    let Some(evaluation) =
                        evaluate_timing_arc(inputs, to, arc, output_edge, input_transition)
                    else {
                        continue;
                    };
                    let arc_delay = (graph_arc.interconnect_delay(input_edge)
                        + effective_resistance(
                            inputs.timing,
                            inputs.model,
                            inputs.graph,
                            net,
                            Some((graph_arc.instance, arc.related_pin())),
                            InterconnectDelayMode::data(input_edge, inputs.options.delay_type),
                        ) * timing_load(
                            inputs.timing,
                            inputs.graph,
                            net,
                            input_edge,
                            inputs.options.delay_type,
                        )
                        .unwrap_or(0.0))
                        * inputs.timing.timing_derate(
                            TimingDerateKind::NetDelay,
                            false,
                            input_edge,
                            inputs.options.delay_type,
                        )
                        + evaluation.delay;
                    let mut downstream = requireds
                        .states(to, output_edge.index())
                        .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: to })?;
                    let mut downstream_tag = arrival.tag;
                    for (point, edge) in arc_exception_steps(
                        inputs,
                        ArcExceptionTraversal {
                            instance: graph_arc.instance.raw() as usize,
                            related_pin: arc.related_pin(),
                            output_pin,
                            from_net: net,
                            to_net: to,
                            input_edge,
                            output_edge,
                        },
                    ) {
                        downstream_tag = sources.tags.advance(
                            downstream_tag,
                            inputs.timing,
                            std::slice::from_ref(&point),
                            edge,
                        )?;
                    }
                    let Some(downstream) =
                        downstream.find(|required| required.tag == downstream_tag)
                    else {
                        continue;
                    };
                    merge_required(
                        &mut slots[input_edge.index()],
                        arrival.tag,
                        downstream.required - arc_delay,
                        inputs.options.delay_type,
                    );
                }
            }
        }
    }
    Ok(())
}

pub(super) fn merge_required(
    set: &mut RequiredEdge,
    tag: TagId,
    candidate: f64,
    delay_type: DelayType,
) {
    if let Some(current) = set.iter_mut().find(|state| state.tag == tag) {
        let tighter = match delay_type {
            DelayType::Max => candidate < current.required,
            DelayType::Min => candidate > current.required,
        };
        if tighter {
            current.required = candidate;
        }
    } else {
        set.push(RequiredState {
            tag,
            required: candidate,
        });
    }
}
