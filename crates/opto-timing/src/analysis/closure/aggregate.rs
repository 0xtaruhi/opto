// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Endpoint aggregation trees used by incremental closure updates.

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct EndpointValue {
    pub(super) slack: Option<f64>,
    pub(super) path: Option<ScalarPath>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ScalarPath {
    pub(super) slack: Option<f64>,
    pub(super) arrival: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ClosureAggregate {
    pub(super) path: Option<ScalarPath>,
    pub(super) wns: Option<f64>,
    pub(super) tns: f64,
    pub(super) violating_paths: usize,
}

impl ClosureAggregate {
    pub(super) fn from_endpoint(value: EndpointValue) -> Self {
        let slack = value.slack;
        Self {
            path: value.path,
            wns: slack,
            tns: slack.filter(|slack| *slack < 0.0).unwrap_or(0.0),
            violating_paths: usize::from(slack.is_some_and(|slack| slack < 0.0)),
        }
    }

    pub(super) fn merge(self, other: Self, delay_type: DelayType) -> Self {
        let path = match (self.path, other.path) {
            (Some(current), Some(candidate)) => {
                Some(if scalar_is_worse(candidate, current, delay_type) {
                    candidate
                } else {
                    current
                })
            }
            (Some(path), None) | (None, Some(path)) => Some(path),
            (None, None) => None,
        };
        let wns = match (self.wns, other.wns) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(slack), None) | (None, Some(slack)) => Some(slack),
            (None, None) => None,
        };
        Self {
            path,
            wns,
            tns: self.tns + other.tns,
            violating_paths: self.violating_paths + other.violating_paths,
        }
    }
}

#[derive(Debug)]
pub(super) struct ClosureAggregateTree {
    pub(super) leaf_base: usize,
    pub(super) len: usize,
    pub(super) nodes: Vec<ClosureAggregate>,
    pub(super) delay_type: DelayType,
}

impl ClosureAggregateTree {
    pub(super) fn build(leaves: &[ClosureAggregate], delay_type: DelayType) -> Self {
        let leaf_base = leaves.len().next_power_of_two().max(1);
        let mut nodes = vec![ClosureAggregate::default(); leaf_base * 2];
        nodes[leaf_base..leaf_base + leaves.len()].copy_from_slice(leaves);
        for node in (1..leaf_base).rev() {
            nodes[node] = nodes[node * 2].merge(nodes[node * 2 + 1], delay_type);
        }
        Self {
            leaf_base,
            len: leaves.len(),
            nodes,
            delay_type,
        }
    }

    pub(super) fn root(&self) -> ClosureAggregate {
        self.nodes[1]
    }

    pub(super) fn update(&mut self, leaf: usize, value: ClosureAggregate) {
        let mut node = self.leaf_base + leaf;
        self.nodes[node] = value;
        while node > 1 {
            node /= 2;
            self.nodes[node] =
                self.nodes[node * 2].merge(self.nodes[node * 2 + 1], self.delay_type);
        }
    }

    pub(super) fn push(&mut self, value: ClosureAggregate) -> usize {
        let position = self.len;
        if position == self.leaf_base {
            let mut leaves = self.nodes[self.leaf_base..self.leaf_base + self.len].to_vec();
            leaves.push(value);
            *self = Self::build(&leaves, self.delay_type);
        } else {
            self.len += 1;
            self.update(position, value);
        }
        position
    }

    pub(super) fn pop(&mut self) {
        debug_assert!(self.len > 0);
        self.len -= 1;
        self.update(self.len, ClosureAggregate::default());
    }
}

#[derive(Debug)]
pub(super) struct ClosureAggregateIndex {
    pub(super) endpoint_positions: Vec<(usize, usize)>,
    pub(super) groups: Vec<ClosureAggregateTree>,
    pub(super) total: ClosureAggregateTree,
}

impl ClosureAggregateIndex {
    pub(super) fn owned_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<(usize, usize)>(self.endpoint_positions.len())
            .saturating_add(opto_core::resident::slice_bytes::<ClosureAggregateTree>(
                self.groups.len(),
            ))
            .saturating_add(
                self.groups
                    .iter()
                    .map(ClosureAggregateTree::owned_memory_bytes)
                    .sum::<usize>(),
            )
            .saturating_add(self.total.owned_memory_bytes())
    }

    pub(super) fn build(
        endpoints: &[ClosureEndpoint],
        values: &[EndpointValue],
        delay_type: DelayType,
    ) -> Self {
        let group_count = endpoints
            .iter()
            .map(|endpoint| endpoint.group)
            .max()
            .map_or(0, |group| group + 1);
        let mut leaves_by_group = vec![Vec::new(); group_count];
        let mut endpoint_positions = Vec::with_capacity(endpoints.len());
        for (endpoint, value) in endpoints.iter().zip(values.iter().copied()) {
            let leaves = &mut leaves_by_group[endpoint.group];
            endpoint_positions.push((endpoint.group, leaves.len()));
            leaves.push(ClosureAggregate::from_endpoint(value));
        }
        let groups = leaves_by_group
            .iter()
            .map(|leaves| ClosureAggregateTree::build(leaves, delay_type))
            .collect::<Vec<_>>();
        let group_roots = groups
            .iter()
            .map(ClosureAggregateTree::root)
            .collect::<Vec<_>>();
        Self {
            endpoint_positions,
            groups,
            total: ClosureAggregateTree::build(&group_roots, delay_type),
        }
    }

    pub(super) fn update(&mut self, endpoint: usize, value: EndpointValue) {
        let (group, position) = self.endpoint_positions[endpoint];
        self.groups[group].update(position, ClosureAggregate::from_endpoint(value));
        self.total.update(group, self.groups[group].root());
    }

    pub(super) fn push(&mut self, group: usize, value: EndpointValue) {
        let delay_type = self.total.delay_type;
        while self.groups.len() <= group {
            self.groups
                .push(ClosureAggregateTree::build(&[], delay_type));
            self.total.push(ClosureAggregate::default());
        }
        let position = self.groups[group].push(ClosureAggregate::from_endpoint(value));
        self.endpoint_positions.push((group, position));
        self.total.update(group, self.groups[group].root());
    }

    pub(super) fn pop_endpoint(&mut self) {
        let (group, position) = self
            .endpoint_positions
            .pop()
            .expect("closure aggregate pop requires an appended endpoint");
        debug_assert_eq!(position + 1, self.groups[group].len);
        self.groups[group].pop();
        self.total.update(group, self.groups[group].root());
    }

    /// Returns closure quality, or `None` when the closure holds no path.
    ///
    /// An unconstrained design reaches no endpoint through a constrained
    /// launch, so "no path" is a well-defined empty answer rather than an
    /// analysis failure. Reporting commands raise `NoTimingPaths` themselves.
    pub(super) fn summary(&self) -> Option<crate::TimingQualitySummary> {
        let aggregate = self.total.root();
        let arrival = aggregate.path.map(|path| path.arrival)?;
        Some(crate::TimingQualitySummary::aggregate(
            arrival,
            aggregate.wns,
            aggregate.tns,
            aggregate.violating_paths,
        ))
    }
}

impl ClosureAggregateTree {
    fn owned_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<ClosureAggregate>(self.nodes.len())
    }
}

pub(super) fn scalar_is_worse(
    candidate: ScalarPath,
    current: ScalarPath,
    delay_type: DelayType,
) -> bool {
    match (candidate.slack, current.slack) {
        (Some(candidate), Some(current)) => candidate < current,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => match delay_type {
            DelayType::Max => candidate.arrival > current.arrival,
            DelayType::Min => candidate.arrival < current.arrival,
        },
    }
}
