// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::required::output_port_requirements;
use super::*;

mod check_evaluation;
use check_evaluation::{evaluate_check, evaluate_pulse_width};

mod aggregate;

use aggregate::*;
use opto_core::RowArenaBuilder;
use std::collections::BTreeSet;

fn btree_memory_bytes<K, V>(len: usize) -> usize {
    opto_core::resident::slice_bytes::<(K, V, [usize; 4])>(len)
}

#[derive(Debug)]
pub(crate) struct ClosureIndex {
    endpoints: Vec<ClosureEndpoint>,
    endpoints_by_net: opto_core::RowArena<ClosureEndpointId>,
    endpoints_by_instance: opto_core::RowArena<ClosureEndpointId>,
    free_endpoints_by_group: BTreeMap<usize, Vec<usize>>,
    values: Vec<EndpointValue>,
    aggregate: ClosureAggregateIndex,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClosureUpdateContext<'a> {
    pub(crate) timing: &'a TimingContext,
    pub(crate) model: &'a TimingModel,
    pub(crate) options: &'a ReportTimingOptions,
    pub(crate) propagation: &'a PropagationState,
    pub(crate) runtime: Option<&'a opto_runtime::ExecutionContext>,
}

#[derive(Debug)]
struct ClosureEndpoint {
    net: usize,
    group: usize,
    kind: ClosureEndpointKind,
}

#[derive(Debug)]
enum ClosureEndpointKind {
    Output {
        port: usize,
    },
    Check {
        instance: TimingInstanceId,
        data_pin: ClosurePinId,
        clock: ClockSlot,
        check_kind: TimingCheckKind,
    },
    PulseWidth {
        instance: TimingInstanceId,
        pin: ClosurePinId,
        clock: ClockSlot,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
struct ClosureEndpointId(u32);

impl ClosureEndpointId {
    fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("closure endpoint capacity was validated"))
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
struct ClosurePinId(u32);

impl ClosurePinId {
    fn from_index(index: usize) -> Self {
        Self(
            u32::try_from(index)
                .expect("sealed timing graph already validated library pin capacity"),
        )
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy)]
struct CheckTarget {
    instance: TimingInstanceId,
    data_pin: ClosurePinId,
    capture_clock: ClockSlot,
    check_kind: TimingCheckKind,
    net: usize,
}

#[derive(Debug, Clone, Copy)]
struct PulseWidthTarget {
    instance: TimingInstanceId,
    pin: ClosurePinId,
    clock: ClockSlot,
    net: usize,
}

impl ClosureEndpoint {
    fn same_identity(&self, other: &Self) -> bool {
        if self.group != other.group {
            return false;
        }
        match (&self.kind, &other.kind) {
            (
                ClosureEndpointKind::Output { port: left },
                ClosureEndpointKind::Output { port: right },
            ) => left == right,
            (
                ClosureEndpointKind::Check {
                    instance: left_instance,
                    data_pin: left_pin,
                    clock: left_clock,
                    check_kind: left_kind,
                },
                ClosureEndpointKind::Check {
                    instance: right_instance,
                    data_pin: right_pin,
                    clock: right_clock,
                    check_kind: right_kind,
                },
            ) => {
                left_instance == right_instance
                    && left_pin == right_pin
                    && left_clock == right_clock
                    && left_kind == right_kind
            }
            (
                ClosureEndpointKind::PulseWidth {
                    instance: left_instance,
                    pin: left_pin,
                    clock: left_clock,
                },
                ClosureEndpointKind::PulseWidth {
                    instance: right_instance,
                    pin: right_pin,
                    clock: right_clock,
                },
            ) => {
                left_instance == right_instance
                    && left_pin == right_pin
                    && left_clock == right_clock
            }
            _ => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClosureEdit(ClosureEditKind);

#[derive(Debug)]
enum ClosureEditKind {
    Values {
        previous: Vec<(usize, usize, EndpointValue)>,
        old_net_count: usize,
        old_instance_row_count: usize,
    },
    Spliced {
        previous: Vec<(usize, usize, EndpointValue)>,
        removed: Vec<(usize, usize, EndpointValue)>,
        inserted: Vec<InsertedEndpoint>,
        instances: Vec<(TimingInstanceId, Vec<ClosureEndpointId>)>,
        old_net_count: usize,
        old_instance_row_count: usize,
    },
}

#[derive(Debug)]
enum InsertedEndpoint {
    Appended(usize),
    Reused {
        index: usize,
        previous: ClosureEndpoint,
    },
}

impl InsertedEndpoint {
    fn index(&self) -> usize {
        match self {
            Self::Appended(index) | Self::Reused { index, .. } => *index,
        }
    }
}

impl ClosureIndex {
    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let free_rows = self
            .free_endpoints_by_group
            .values()
            .map(|rows| opto_core::resident::slice_bytes::<usize>(rows.len()))
            .sum::<usize>();
        opto_core::resident::slice_bytes::<ClosureEndpoint>(self.endpoints.len())
            .saturating_add(self.endpoints_by_net.owned_memory_bytes())
            .saturating_add(self.endpoints_by_instance.owned_memory_bytes())
            .saturating_add(btree_memory_bytes::<usize, Vec<usize>>(
                self.free_endpoints_by_group.len(),
            ))
            .saturating_add(free_rows)
            .saturating_add(opto_core::resident::slice_bytes::<EndpointValue>(
                self.values.len(),
            ))
            .saturating_add(self.aggregate.owned_memory_bytes())
    }

    pub(crate) fn build(
        timing: &TimingContext,
        model: &TimingModel,
        options: &ReportTimingOptions,
        propagation: &PropagationState,
        runtime: Option<&opto_runtime::ExecutionContext>,
    ) -> Result<Self, crate::TimingError> {
        let mut endpoints = Vec::new();
        for (port, data) in model.design.ports().iter().enumerate() {
            if data.direction != TimingPortDirection::Output
                || !matches_report_objects(&options.to, &data.name)
                || timing.timing_endpoint_is_disabled(TimingEndpoint::Port(data.id))
            {
                continue;
            }
            if let Some(net) = model.graph.port_net(port).map(crate::TimingNetId::index) {
                endpoints.push(ClosureEndpoint {
                    net,
                    group: 0,
                    kind: ClosureEndpointKind::Output { port },
                });
            }
        }

        let instance_row_count = model
            .instances()
            .map(|instance| instance.id().raw() as usize + 1)
            .max()
            .unwrap_or_default();
        let mut instance_rows = RowArenaBuilder::try_with_capacity(instance_row_count)
            .map_err(|_| closure_adjacency_capacity())?;
        for raw in 0..instance_row_count {
            let first = endpoints.len();
            endpoints.extend(check_endpoints_for_instance(
                TimingInstanceId::from_raw(
                    u32::try_from(raw).expect("instance row count originates from stable u32 IDs"),
                ),
                timing,
                model,
                options,
            ));
            if endpoints.len() > u32::MAX as usize {
                return Err(closure_adjacency_capacity());
            }
            instance_rows
                .try_push_row((first..endpoints.len()).map(|endpoint| {
                    ClosureEndpointId(
                        u32::try_from(endpoint)
                            .expect("endpoint capacity is checked before row publication"),
                    )
                }))
                .map_err(|_| closure_adjacency_capacity())?;
        }

        let mut net_entries = endpoints
            .iter()
            .enumerate()
            .map(|(endpoint, data)| (data.net, ClosureEndpointId::from_index(endpoint)))
            .collect::<Vec<_>>();
        net_entries.sort_unstable();
        let mut net_rows = RowArenaBuilder::try_with_capacity(model.graph.net_count())
            .map_err(|_| closure_adjacency_capacity())?;
        let mut first = 0;
        for net in 0..model.graph.net_count() {
            let count = net_entries[first..].partition_point(|&(candidate, _)| candidate == net);
            net_rows
                .try_push_row(
                    net_entries[first..first + count]
                        .iter()
                        .map(|&(_, endpoint)| endpoint),
                )
                .map_err(|_| closure_adjacency_capacity())?;
            first += count;
        }
        debug_assert_eq!(first, net_entries.len());
        drop(net_entries);
        let mut index = Self {
            values: vec![
                EndpointValue {
                    slack: None,
                    path: None
                };
                endpoints.len()
            ],
            endpoints,
            endpoints_by_net: net_rows.finish(),
            endpoints_by_instance: instance_rows.finish(),
            free_endpoints_by_group: BTreeMap::new(),
            aggregate: ClosureAggregateIndex::build(&[], &[], options.delay_type),
        };
        let values = evaluate_endpoints(
            &index,
            (0..index.endpoints.len())
                .map(|endpoint| (endpoint, index.endpoints[endpoint].net))
                .collect(),
            timing,
            model,
            options,
            propagation,
            runtime,
        )?;
        index.values = values.into_iter().map(|(_, _, value)| value).collect();
        index.aggregate =
            ClosureAggregateIndex::build(&index.endpoints, &index.values, options.delay_type);
        Ok(index)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "closure update atomically reconciles endpoint arenas, reverse indices, values, \
                  and aggregate trees so rollback can restore a coherent snapshot"
    )]
    pub(crate) fn update(
        &mut self,
        context: ClosureUpdateContext<'_>,
        changed_nets: &[usize],
        affected_instances: Option<&[TimingInstanceId]>,
    ) -> Result<ClosureEdit, crate::TimingError> {
        let ClosureUpdateContext {
            timing,
            model,
            options,
            propagation,
            runtime,
        } = context;
        let old_net_count = self.endpoints_by_net.len();
        self.endpoints_by_net
            .resize_empty(model.graph.net_count())
            .map_err(|_| closure_adjacency_capacity())?;
        let old_instance_row_count = self.endpoints_by_instance.len();
        let mut replacement_rows = Vec::new();
        let mut replacement_count = 0usize;
        if let Some(affected_instances) = affected_instances {
            let mut seen = BTreeSet::new();
            for &instance in affected_instances {
                if seen.insert(instance) {
                    let replacement =
                        check_endpoints_for_instance(instance, timing, model, options);
                    replacement_count = replacement_count.saturating_add(replacement.len());
                    replacement_rows.push((instance, replacement));
                }
            }
        }
        if self
            .endpoints
            .len()
            .checked_add(replacement_count)
            .is_none_or(|count| count > u32::MAX as usize)
        {
            self.endpoints_by_net.truncate_empty(old_net_count);
            return Err(closure_adjacency_capacity());
        }
        if let Some(row_count) = replacement_rows
            .iter()
            .map(|(instance, _)| instance.raw() as usize + 1)
            .max()
            .filter(|&row_count| row_count > old_instance_row_count)
        {
            if row_count > u32::MAX as usize {
                self.endpoints_by_net.truncate_empty(old_net_count);
                return Err(closure_adjacency_capacity());
            }
            self.endpoints_by_instance
                .resize_empty(row_count)
                .map_err(|_| closure_adjacency_capacity())?;
        }
        let mut target_nets = BTreeMap::<usize, usize>::new();
        let mut removed = Vec::new();
        let mut inserted = Vec::new();
        let mut instances = Vec::new();
        if !replacement_rows.is_empty() {
            for (instance, replacement) in replacement_rows {
                let existing = self
                    .endpoints_by_instance
                    .get(instance.raw() as usize)
                    .unwrap_or_default();
                let same_shape = existing.len() == replacement.len()
                    && existing
                        .iter()
                        .zip(&replacement)
                        .all(|(&current, candidate)| {
                            self.endpoints[current.index()].same_identity(candidate)
                        });
                if same_shape {
                    for (&current, candidate) in existing.iter().zip(replacement) {
                        if self.endpoints[current.index()].net != candidate.net {
                            target_nets.insert(current.index(), candidate.net);
                        }
                    }
                    continue;
                }
                let existing = existing.to_vec();
                for &endpoint in &existing {
                    let net = self.endpoints[endpoint.index()].net;
                    let list = self.endpoints_by_net.row_mut(net);
                    let position = list
                        .binary_search(&endpoint)
                        .expect("closure endpoint must be indexed by its current net");
                    list.remove(position);
                    let neutral = EndpointValue {
                        slack: None,
                        path: None,
                    };
                    let previous_value =
                        std::mem::replace(&mut self.values[endpoint.index()], neutral);
                    self.aggregate.update(endpoint.index(), neutral);
                    removed.push((endpoint.index(), net, previous_value));
                }
                let mut new_indices = Vec::with_capacity(replacement.len());
                for candidate in replacement {
                    let net = candidate.net;
                    let neutral = EndpointValue {
                        slack: None,
                        path: None,
                    };
                    let insertion = if let Some(index) = self
                        .free_endpoints_by_group
                        .get_mut(&candidate.group)
                        .and_then(Vec::pop)
                    {
                        let previous = std::mem::replace(&mut self.endpoints[index], candidate);
                        debug_assert!(
                            self.values[index].slack.is_none() && self.values[index].path.is_none()
                        );
                        InsertedEndpoint::Reused { index, previous }
                    } else {
                        let index = self.endpoints.len();
                        let group = candidate.group;
                        self.endpoints.push(candidate);
                        self.values.push(neutral);
                        self.aggregate.push(group, neutral);
                        InsertedEndpoint::Appended(index)
                    };
                    let index = insertion.index();
                    let endpoint = ClosureEndpointId::from_index(index);
                    let list = self.endpoints_by_net.row_mut(net);
                    let position = list
                        .binary_search(&endpoint)
                        .unwrap_or_else(|position| position);
                    list.insert(position, endpoint);
                    new_indices.push(endpoint);
                    inserted.push(insertion);
                    target_nets.insert(index, net);
                }
                self.endpoints_by_instance
                    .replace(instance.raw() as usize, new_indices);
                instances.push((instance, existing));
            }
        }
        for &net in changed_nets {
            if let Some(endpoints) = self.endpoints_by_net.get(net) {
                for &endpoint in endpoints {
                    target_nets
                        .entry(endpoint.index())
                        .or_insert(self.endpoints[endpoint.index()].net);
                }
            }
        }
        let updates = evaluate_endpoints(
            self,
            target_nets.into_iter().collect(),
            timing,
            model,
            options,
            propagation,
            runtime,
        );
        let updates = match updates {
            Ok(updates) => updates,
            Err(error) => {
                if removed.is_empty() && inserted.is_empty() {
                    self.endpoints_by_net.truncate_empty(old_net_count);
                    if self.endpoints_by_instance.len() > old_instance_row_count {
                        self.endpoints_by_instance
                            .truncate_empty(old_instance_row_count);
                    }
                } else {
                    self.rollback(ClosureEdit(ClosureEditKind::Spliced {
                        previous: Vec::new(),
                        removed,
                        inserted,
                        instances,
                        old_net_count,
                        old_instance_row_count,
                    }));
                }
                return Err(error);
            }
        };
        let mut previous = Vec::with_capacity(updates.len());
        for (endpoint, net, value) in updates {
            let (previous_net, previous_value) = self.replace_endpoint(endpoint, net, value);
            previous.push((endpoint, previous_net, previous_value));
        }
        if removed.is_empty() && inserted.is_empty() {
            return Ok(ClosureEdit(ClosureEditKind::Values {
                previous,
                old_net_count,
                old_instance_row_count,
            }));
        }
        Ok(ClosureEdit(ClosureEditKind::Spliced {
            previous,
            removed,
            inserted,
            instances,
            old_net_count,
            old_instance_row_count,
        }))
    }

    /// Restores endpoint values and sparse indexes in reverse mutation order.
    pub(crate) fn rollback(&mut self, edit: ClosureEdit) {
        match edit.0 {
            ClosureEditKind::Values {
                previous,
                old_net_count,
                old_instance_row_count,
            } => {
                for (endpoint, net, value) in previous.into_iter().rev() {
                    self.replace_endpoint(endpoint, net, value);
                }
                self.endpoints_by_net.truncate_empty(old_net_count);
                if self.endpoints_by_instance.len() > old_instance_row_count {
                    self.endpoints_by_instance
                        .truncate_empty(old_instance_row_count);
                }
            }
            ClosureEditKind::Spliced {
                previous,
                removed,
                inserted,
                instances,
                old_net_count,
                old_instance_row_count,
            } => {
                for (endpoint, net, value) in previous.into_iter().rev() {
                    self.replace_endpoint(endpoint, net, value);
                }
                for insertion in inserted.into_iter().rev() {
                    let endpoint = insertion.index();
                    let endpoint_id = ClosureEndpointId::from_index(endpoint);
                    let net = self.endpoints[endpoint].net;
                    let list = self.endpoints_by_net.row_mut(net);
                    let position = list
                        .binary_search(&endpoint_id)
                        .expect("inserted closure endpoint must be indexed by its net");
                    list.remove(position);
                    match insertion {
                        InsertedEndpoint::Appended(endpoint) => {
                            debug_assert_eq!(endpoint + 1, self.endpoints.len());
                            self.endpoints.pop();
                            self.values.pop();
                            self.aggregate.pop_endpoint();
                        }
                        InsertedEndpoint::Reused { index, previous } => {
                            let group = self.endpoints[index].group;
                            debug_assert_eq!(group, previous.group);
                            self.endpoints[index] = previous;
                            self.free_endpoints_by_group
                                .entry(group)
                                .or_default()
                                .push(index);
                        }
                    }
                }
                for (endpoint, net, value) in removed.into_iter().rev() {
                    debug_assert_eq!(self.endpoints[endpoint].net, net);
                    let list = self.endpoints_by_net.row_mut(net);
                    let endpoint_id = ClosureEndpointId::from_index(endpoint);
                    let position = list
                        .binary_search(&endpoint_id)
                        .unwrap_or_else(|position| position);
                    list.insert(position, endpoint_id);
                    self.values[endpoint] = value;
                    self.aggregate.update(endpoint, value);
                }
                for (instance, original) in instances.into_iter().rev() {
                    self.endpoints_by_instance
                        .replace(instance.raw() as usize, original);
                }
                self.endpoints_by_instance
                    .truncate_empty(old_instance_row_count);
                self.endpoints_by_net.truncate_empty(old_net_count);
            }
        }
    }

    /// Releases removed endpoint slots only after the edit is accepted.
    pub(crate) fn commit(&mut self, edit: ClosureEdit) {
        let ClosureEditKind::Spliced { removed, .. } = edit.0 else {
            return;
        };
        for (endpoint, _, _) in removed {
            self.free_endpoints_by_group
                .entry(self.endpoints[endpoint].group)
                .or_default()
                .push(endpoint);
        }
    }

    /// Seals adjacency overlays before an infallible closure commit.
    pub(crate) fn compact_rows(&mut self) -> Result<(), crate::TimingError> {
        self.endpoints_by_net
            .compact()
            .map_err(|_| closure_adjacency_capacity())?;
        self.endpoints_by_instance
            .compact()
            .map_err(|_| closure_adjacency_capacity())
    }

    #[cfg(test)]
    pub(crate) fn slot_counts(&self) -> (usize, usize) {
        (
            self.endpoints.len(),
            self.free_endpoints_by_group.values().map(Vec::len).sum(),
        )
    }

    pub(crate) fn summary(&self) -> Option<crate::TimingQualitySummary> {
        self.aggregate.summary()
    }

    fn replace_endpoint(
        &mut self,
        endpoint: usize,
        net: usize,
        value: EndpointValue,
    ) -> (usize, EndpointValue) {
        let previous_net = std::mem::replace(&mut self.endpoints[endpoint].net, net);
        if previous_net != net {
            let endpoint_id = ClosureEndpointId::from_index(endpoint);
            let previous = self.endpoints_by_net.row_mut(previous_net);
            let position = previous
                .binary_search(&endpoint_id)
                .expect("closure endpoint must be indexed by its previous net");
            previous.remove(position);
            let current = self.endpoints_by_net.row_mut(net);
            let position = current
                .binary_search(&endpoint_id)
                .unwrap_or_else(|position| position);
            current.insert(position, endpoint_id);
        }
        let previous_value = std::mem::replace(&mut self.values[endpoint], value);
        self.aggregate.update(endpoint, value);
        (previous_net, previous_value)
    }

    fn evaluate_endpoint(
        &self,
        endpoint: usize,
        net: usize,
        timing: &TimingContext,
        model: &TimingModel,
        options: &ReportTimingOptions,
        propagation: &PropagationState,
    ) -> Result<EndpointValue, crate::TimingError> {
        let endpoint = &self.endpoints[endpoint];
        match &endpoint.kind {
            ClosureEndpointKind::Output { port } => {
                evaluate_output(*port, net, timing, model, options, propagation)
            }
            ClosureEndpointKind::Check {
                instance,
                data_pin,
                clock,
                check_kind,
            } => evaluate_check(
                CheckTarget {
                    instance: *instance,
                    data_pin: *data_pin,
                    capture_clock: *clock,
                    check_kind: *check_kind,
                    net,
                },
                timing,
                model,
                options,
                propagation,
            ),
            ClosureEndpointKind::PulseWidth {
                instance,
                pin,
                clock,
            } => Ok(evaluate_pulse_width(
                PulseWidthTarget {
                    instance: *instance,
                    pin: *pin,
                    clock: *clock,
                    net,
                },
                timing,
                model,
                options,
            )),
        }
    }
}

const PARALLEL_ENDPOINT_THRESHOLD: usize = 64;

fn evaluate_endpoints(
    index: &ClosureIndex,
    targets: Vec<(usize, usize)>,
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &PropagationState,
    runtime: Option<&opto_runtime::ExecutionContext>,
) -> Result<Vec<(usize, usize, EndpointValue)>, crate::TimingError> {
    match runtime {
        Some(runtime) if targets.len() >= PARALLEL_ENDPOINT_THRESHOLD => {
            runtime.analyze_indexed(targets.len(), |position| {
                let (endpoint, net) = targets[position];
                index
                    .evaluate_endpoint(endpoint, net, timing, model, options, propagation)
                    .map(|value| (endpoint, net, value))
            })
        }
        _ => targets
            .into_iter()
            .map(|(endpoint, net)| {
                index
                    .evaluate_endpoint(endpoint, net, timing, model, options, propagation)
                    .map(|value| (endpoint, net, value))
            })
            .collect(),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "endpoint discovery is an exhaustive mapping from Liberty check kinds to stable \
              closure identities; a single table keeps deduplication rules reviewable"
)]
fn check_endpoints_for_instance(
    instance_id: TimingInstanceId,
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
) -> Vec<ClosureEndpoint> {
    let Some(instance) = model.instance_ref(instance_id) else {
        return Vec::new();
    };
    let Some(cell) = model.graph.cell(&model.library, instance.id()) else {
        return Vec::new();
    };
    let connections = connection_map_ref(instance);
    let instance_name = instance.name();
    let analysis_inputs = PropagationInputs {
        timing,
        model,
        design: &model.design,
        library: &model.library,
        options,
        graph: &model.graph,
    };
    let enabled_checks = enabled_timing_check_kinds(options).collect::<SmallVec<[_; 2]>>();
    let mut candidates = Vec::new();
    for (pin_index, pin) in cell.pins().enumerate() {
        let pin_id = ClosurePinId::from_index(pin_index);
        for constraint in pin.timing_arcs() {
            let check_kind = match constraint.timing_type() {
                TargetTimingType::Check { kind, .. } => Some(kind),
                TargetTimingType::Recovery(_) => Some(TimingCheckKind::Recovery),
                TargetTimingType::Removal(_) => Some(TimingCheckKind::Removal),
                TargetTimingType::MinPulseWidth if options.checks.pulse_width => None,
                TargetTimingType::Combinational
                | TargetTimingType::ClockToQ(_)
                | TargetTimingType::Clear
                | TargetTimingType::Preset
                | TargetTimingType::MinPulseWidth
                | TargetTimingType::NonSequentialSetup(_)
                | TargetTimingType::NonSequentialHold(_)
                | TargetTimingType::ThreeStateEnable
                | TargetTimingType::ThreeStateDisable => continue,
            };
            if let Some(check_kind) = check_kind {
                if !enabled_checks.contains(&check_kind)
                    || !timing_check_arc_is_selected(
                        &analysis_inputs,
                        &options.to,
                        &instance_name,
                        pin.name(),
                        constraint.related_pin(),
                    )
                {
                    continue;
                }
                let (Some(data_net), Some(clock_net)) = (
                    connections.get(pin.name()),
                    connections.get(constraint.related_pin()),
                ) else {
                    continue;
                };
                for (clock, _) in clocks_on_net(timing, &model.graph, clock_net.index()) {
                    candidates.push(ClosureCandidate {
                        pin: pin_id,
                        clock,
                        check_kind: Some(check_kind),
                        net: data_net.index(),
                    });
                }
                continue;
            }

            let related_pin = pulse_width_related_pin(pin, constraint);
            if !timing_check_arc_is_selected(
                &analysis_inputs,
                &options.to,
                &instance_name,
                pin.name(),
                related_pin,
            ) {
                continue;
            }
            let Some(clock_net) = connections.get(pin.name()) else {
                continue;
            };
            for (clock, _) in clocks_on_net(timing, &model.graph, clock_net.index()) {
                candidates.push(ClosureCandidate {
                    pin: pin_id,
                    clock,
                    check_kind: None,
                    net: clock_net.index(),
                });
            }
        }
    }
    candidates.sort_unstable_by_key(ClosureCandidate::order_key);
    candidates.dedup_by(|right, left| right.same_identity(left));
    candidates
        .into_iter()
        .map(|candidate| ClosureEndpoint {
            net: candidate.net,
            group: candidate.clock.index() + 1,
            kind: match candidate.check_kind {
                Some(check_kind) => ClosureEndpointKind::Check {
                    instance: instance_id,
                    data_pin: candidate.pin,
                    clock: candidate.clock,
                    check_kind,
                },
                None => ClosureEndpointKind::PulseWidth {
                    instance: instance_id,
                    pin: candidate.pin,
                    clock: candidate.clock,
                },
            },
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct ClosureCandidate {
    pin: ClosurePinId,
    clock: ClockSlot,
    check_kind: Option<TimingCheckKind>,
    net: usize,
}

impl ClosureCandidate {
    fn order_key(&self) -> (bool, ClosurePinId, ClockSlot, Option<TimingCheckKind>) {
        (
            self.check_kind.is_none(),
            self.pin,
            self.clock,
            self.check_kind,
        )
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.pin == other.pin && self.clock == other.clock && self.check_kind == other.check_kind
    }
}

fn closure_adjacency_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "closure endpoint adjacency",
    }
    .into()
}

fn evaluate_output(
    port: usize,
    net: usize,
    timing: &TimingContext,
    model: &TimingModel,
    options: &ReportTimingOptions,
    propagation: &PropagationState,
) -> Result<EndpointValue, crate::TimingError> {
    let port = &model.design.ports()[port];
    if timing.timing_endpoint_is_disabled(TimingEndpoint::Port(port.id)) {
        return Ok(EndpointValue {
            slack: None,
            path: None,
        });
    }
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
    for edge in TimingEdge::ALL {
        for arrival in propagation.arrivals.states(net, edge.index()) {
            let key = propagation.tags.key(arrival.tag)?;
            let endpoint_points = output_endpoint_points(&exception_inputs, port.id, net, edge);
            let resolved = resolve_path_exception(
                timing,
                &key.path_exceptions,
                &endpoint_points,
                edge,
                options.delay_type,
            )?;
            if resolved.is_some_and(|resolved| {
                matches!(resolved.exception.kind, PathExceptionKind::FalsePath)
            }) {
                continue;
            }
            let sink_delay = sink_interconnect_delay(
                timing,
                model,
                &model.graph,
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
                &propagation.origins,
                resolved.as_ref().map(|resolved| &resolved.exception.kind),
            )
            .into_iter()
            .reduce(|left, right| match options.delay_type {
                DelayType::Max => left.min(right),
                DelayType::Min => left.max(right),
            });
            let candidate_slack = required.map(|required| match options.delay_type {
                DelayType::Max => required - candidate_delay,
                DelayType::Min => candidate_delay - required,
            });
            if let Some(candidate) = candidate_slack
                && slack.is_none_or(|current| candidate < current)
            {
                slack = Some(candidate);
            }
            let candidate = ScalarPath {
                slack: candidate_slack,
                arrival: candidate_delay,
            };
            if path.is_none_or(|current| scalar_is_worse(candidate, current, options.delay_type)) {
                path = Some(candidate);
            }
        }
    }
    Ok(EndpointValue { slack, path })
}
