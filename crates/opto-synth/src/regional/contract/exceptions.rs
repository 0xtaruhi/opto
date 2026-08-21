// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn regional_exception_classes(
    regions: &crate::SynthesisRegionGraph,
    endpoints: &WordTimingEndpointIndex,
    constraints: &opto_timing::TimingContext,
) -> Result<Vec<Box<[u32]>>, crate::SynthError> {
    let mut rows = vec![BTreeSet::new(); regions.regions().len()];
    for (index, exception) in constraints.path_exceptions().iter().enumerate() {
        let class = u32::try_from(index + 1).map_err(|_| {
            crate::SynthError::capacity("timing exception class exceeds 32-bit capacity")
        })?;
        for row in exception_path_regions(regions, endpoints, exception) {
            rows[row.index()].insert(class);
        }
    }
    Ok(rows
        .into_iter()
        .map(|classes| classes.into_iter().collect())
        .collect())
}

#[derive(Debug)]
enum ExceptionAnchors {
    Any,
    Rows(BTreeSet<crate::RegionRowId>),
}

fn exception_path_regions(
    regions: &crate::SynthesisRegionGraph,
    endpoints: &WordTimingEndpointIndex,
    exception: &opto_timing::PathException,
) -> BTreeSet<crate::RegionRowId> {
    let mut anchors = Vec::with_capacity(exception.through.len().saturating_add(2));
    anchors.push(exception_anchors(regions, endpoints, &exception.from));
    anchors.extend(
        exception
            .through
            .iter()
            .map(|filter| exception_anchors(regions, endpoints, filter)),
    );
    anchors.push(exception_anchors(regions, endpoints, &exception.to));
    let mut path = BTreeSet::new();
    for segment in anchors.windows(2) {
        let rows = match (&segment[0], &segment[1]) {
            (ExceptionAnchors::Any, ExceptionAnchors::Any) => regions
                .regions()
                .iter()
                .map(|region| region.row())
                .collect(),
            (ExceptionAnchors::Rows(from), ExceptionAnchors::Any) => {
                reachable_regions(regions, from, true)
            }
            (ExceptionAnchors::Any, ExceptionAnchors::Rows(to)) => {
                reachable_regions(regions, to, false)
            }
            (ExceptionAnchors::Rows(from), ExceptionAnchors::Rows(to)) => {
                let forward = reachable_regions(regions, from, true);
                let backward = reachable_regions(regions, to, false);
                forward.intersection(&backward).copied().collect()
            }
        };
        if rows.is_empty() {
            return BTreeSet::new();
        }
        path.extend(rows);
    }
    path
}

fn exception_anchors(
    regions: &crate::SynthesisRegionGraph,
    endpoints: &WordTimingEndpointIndex,
    filter: &opto_timing::ExceptionFilter,
) -> ExceptionAnchors {
    if filter.is_unrestricted() {
        return ExceptionAnchors::Any;
    }
    let mut rows = BTreeSet::new();
    for region in regions.regions() {
        for &port in regions
            .input_ports(*region)
            .iter()
            .chain(regions.output_ports(*region))
        {
            let Some(port) = regions.port(port) else {
                continue;
            };
            if endpoints.values.get(&port.value()).is_some_and(|points| {
                points
                    .iter()
                    .any(|point| filter.objects().binary_search(point).is_ok())
            }) {
                rows.insert(port.region());
                rows.extend(port.peer());
            }
        }
    }
    if rows.is_empty() {
        // Clock endpoints and source objects that do not materialize as a Word
        // boundary are conservatively retained. Global STA performs exact
        // matching; contract allocation must never collapse their tag class.
        ExceptionAnchors::Any
    } else {
        ExceptionAnchors::Rows(rows)
    }
}

fn reachable_regions(
    regions: &crate::SynthesisRegionGraph,
    seeds: &BTreeSet<crate::RegionRowId>,
    forward: bool,
) -> BTreeSet<crate::RegionRowId> {
    let mut reached = BTreeSet::new();
    let mut pending = seeds.iter().copied().collect::<Vec<_>>();
    while let Some(row) = pending.pop() {
        if !reached.insert(row) {
            continue;
        }
        let Some(region) = regions.region(row) else {
            continue;
        };
        let is_hard_endpoint = matches!(
            region.kind(),
            crate::SynthesisRegionKind::State | crate::SynthesisRegionKind::Memory
        );
        if is_hard_endpoint && !seeds.contains(&row) {
            continue;
        }
        let adjacent = if forward {
            regions.successors(region)
        } else {
            regions.predecessors(region)
        };
        pending.extend(adjacent.iter().copied());
    }
    reached
}

pub(super) fn filter_matches_boundary(
    filter: &opto_timing::ExceptionFilter,
    endpoints: &[opto_timing::TimingEndpoint],
) -> bool {
    !filter.is_unrestricted()
        && endpoints
            .iter()
            .any(|endpoint| filter.objects().binary_search(endpoint).is_ok())
}

#[derive(Debug, Default)]
pub(super) struct WordTimingEndpointIndex {
    values: BTreeMap<word::ValueId, BTreeSet<opto_timing::TimingEndpoint>>,
}

impl WordTimingEndpointIndex {
    pub(super) fn build(
        module: &word::WordModule,
        bindings: &opto_timing::TimingObjectBindings,
    ) -> Self {
        let mut index = Self::default();
        for (value_index, value) in module.values().iter().enumerate() {
            let word::ValueKind::Signal(reference) = value.kind else {
                continue;
            };
            let Some(name) = module
                .signal(reference.signal)
                .and_then(|signal| signal.name)
                .map(|name| module.name_str(name))
            else {
                continue;
            };
            if let Some(endpoint) = bindings.net_endpoint(name)
                && let Ok(value) = word::ValueId::from_index(value_index)
            {
                index.values.entry(value).or_default().insert(endpoint);
            }
        }
        for instance in module.instances() {
            let instance_name = module.name_str(instance.name);
            let cell = bindings.cell_endpoint(instance_name);
            for connection in &instance.connections {
                let pin_name = module.name_str(connection.port);
                let pin = bindings.pin_endpoint(&format!("{instance_name}/{pin_name}"));
                index.bind_connection_values(module, connection.value, cell, pin);
            }
        }
        index
    }

    fn bind_connection_values(
        &mut self,
        module: &word::WordModule,
        root: word::ValueId,
        cell: Option<opto_timing::TimingEndpoint>,
        pin: Option<opto_timing::TimingEndpoint>,
    ) {
        let mut pending = vec![root];
        let mut visited = BTreeSet::new();
        while let Some(value) = pending.pop() {
            if !visited.insert(value) {
                continue;
            }
            let endpoints = self.values.entry(value).or_default();
            endpoints.extend(cell);
            endpoints.extend(pin);
            let Some(stored) = module.value(value) else {
                continue;
            };
            let word::ValueKind::Operation(operation) = stored.kind else {
                continue;
            };
            let Some(operation) = module.operation(operation) else {
                continue;
            };
            match &operation.kind {
                word::OpKind::Concat { parts } => pending.extend(parts.iter().copied()),
                word::OpKind::Cast { value, .. } | word::OpKind::Extract { value, .. } => {
                    pending.push(*value);
                }
                _ => {}
            }
        }
    }

    pub(super) fn endpoints(
        &self,
        value: word::ValueId,
        port: Option<opto_timing::PortId>,
    ) -> Vec<opto_timing::TimingEndpoint> {
        let mut endpoints = self.values.get(&value).cloned().unwrap_or_default();
        endpoints.extend(port.map(opto_timing::TimingEndpoint::Port));
        endpoints.into_iter().collect()
    }
}
