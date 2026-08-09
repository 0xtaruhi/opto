// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::graph::{
    BoundaryPortId, BoundaryValueRevision, OperationAnchorId, RegionAnchorId, RegionBoundaryPort,
    RegionBoundaryPortId, RegionGraphOwnerId, RegionPortDirection, RegionRevision, RegionRowId,
    SynthesisRegion, SynthesisRegionGraph, SynthesisRegionKind, SynthesisRegionRevision,
    packed_rows, remap_optional_owner_rows, remap_owner_rows,
};
use crate::word::signal_driver::SignalDriverIndex;
use opto_ir::word;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

mod connectivity;
mod semantic;
mod work;
use connectivity::{ConnectivityIndex, InputOperations};
use work::{is_state, memory_read_inputs, memory_work, memory_write_inputs, operation_work};

const OPERATION_ANCHOR_DOMAIN: &[u8] = b"opto/operation-anchor/v1\0";
const WHOLE_DESIGN_REGION_ANCHOR_DOMAIN: &[u8] = b"opto/whole-design-region-anchor/v1\0";
const REGION_ID_DOMAIN: &[u8] = b"opto/region-anchor/v1\0";
const REGION_LOCAL_KEY_DOMAIN: &[u8] = b"opto/synthesis-region/local-key/v1\0";
const REGION_REVISION_DOMAIN: &[u8] = b"opto/synthesis-region/revision/v1\0";
const MEMORY_ANCHOR_DOMAIN: &[u8] = b"opto/memory-region-anchor/v1\0";
const BOUNDARY_EDGE_ID_DOMAIN: &[u8] = b"opto/boundary-port/id/v1\0";
const BOUNDARY_ENDPOINT_ID_DOMAIN: &[u8] = b"opto/boundary-port/endpoint/v1\0";
const MATCHING_ROUNDS: usize = 12;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RegionPartitionPolicy {
    target_work: u64,
}

impl RegionPartitionPolicy {
    #[cfg(test)]
    pub(crate) const fn with_target_work(target_work: u64) -> Self {
        Self { target_work }
    }
}

impl Default for RegionPartitionPolicy {
    fn default() -> Self {
        Self {
            target_work: 32_768,
        }
    }
}

#[derive(Debug)]
struct TempRegion {
    anchor: [u8; 32],
    kind: SynthesisRegionKind,
    operations: Vec<word::OpId>,
    memories: Vec<word::MemoryId>,
    work: u64,
    delay: u64,
    wiring: u64,
    id: RegionAnchorId,
    revision: RegionRevision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TempEndpoint {
    Signal(word::SignalRef),
    Operation(word::OpId),
    Constant(word::ValueId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TempEdge {
    source: Option<usize>,
    sink: Option<usize>,
    value: word::ValueId,
    endpoint: TempEndpoint,
    ty: word::WordType,
    semantic_key: [u8; 32],
    value_revision: [u8; 32],
}

impl Ord for TempEdge {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.source, self.sink, self.endpoint, type_key(self.ty)).cmp(&(
            other.source,
            other.sink,
            other.endpoint,
            type_key(other.ty),
        ))
    }
}

fn temp_endpoint(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<TempEndpoint, crate::SynthError> {
    match module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("regional edge value is unknown"))?
        .kind
    {
        word::ValueKind::Signal(reference) => Ok(TempEndpoint::Signal(reference)),
        word::ValueKind::Operation(operation) => Ok(TempEndpoint::Operation(operation)),
        word::ValueKind::Constant(_) => Ok(TempEndpoint::Constant(value)),
    }
}

impl PartialOrd for TempEdge {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy)]
struct CanonicalEdge {
    source: Option<RegionRowId>,
    sink: Option<RegionRowId>,
    value: word::ValueId,
    ty: word::WordType,
    semantic_key: [u8; 32],
    value_revision: [u8; 32],
}

fn operation_anchors(
    module: &word::WordModule,
) -> Result<Box<[OperationAnchorId]>, crate::SynthError> {
    let mut occurrences = BTreeMap::<word::SourceIdentity, u32>::new();
    module
        .operations()
        .iter()
        .map(|operation| {
            let identity = operation.source.identity().ok_or_else(|| {
                crate::SynthError::invariant(
                    "synthesis operation has no stable frontend source identity",
                )
            })?;
            let ordinal = occurrences.entry(identity).or_default();
            let current = *ordinal;
            *ordinal = ordinal.checked_add(1).ok_or_else(|| {
                crate::SynthError::capacity("source-local operation ordinal exceeds 32 bits")
            })?;
            let mut digest = blake3::Hasher::new();
            digest.update(OPERATION_ANCHOR_DOMAIN);
            append_hash_text(&mut digest, module.name());
            digest.update(&identity.bytes());
            digest.update(&current.to_le_bytes());
            Ok(OperationAnchorId::from_bytes(*digest.finalize().as_bytes()))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()
        .map(Vec::into_boxed_slice)
}

pub(crate) fn build(
    module: &word::WordModule,
    policy: RegionPartitionPolicy,
) -> Result<SynthesisRegionGraph, crate::SynthError> {
    build_inner(module, policy, None)
}

pub(crate) fn build_with_ownership(
    module: &word::WordModule,
    policy: RegionPartitionPolicy,
    ownership: &crate::regional::StructuralOwnershipProvenance,
) -> Result<SynthesisRegionGraph, crate::SynthError> {
    build_inner(module, policy, Some(ownership))
}

fn build_inner(
    module: &word::WordModule,
    policy: RegionPartitionPolicy,
    ownership: Option<&crate::regional::StructuralOwnershipProvenance>,
) -> Result<SynthesisRegionGraph, crate::SynthError> {
    if policy.target_work == 0 {
        return Err(crate::SynthError::invariant(
            "region target work must be nonzero",
        ));
    }
    let drivers = SignalDriverIndex::new(module)?;
    let value_keys = semantic::value_keys(module)?;
    let anchors = operation_anchors(module)?;
    let mut regions = partition_operations(module, &anchors, &drivers, policy)?;
    if let Some(ownership) = ownership {
        merge_ownership_claims(module, ownership, &mut regions)?;
    }
    append_memory_regions(module, &mut regions)?;
    let mut operation_owner = vec![None; module.operations().len()];
    let mut memory_owner = vec![None; module.memories().len()];
    for (region, contents) in regions.iter().enumerate() {
        for &operation in &contents.operations {
            operation_owner[operation.index()] = Some(region);
        }
        for &memory in &contents.memories {
            memory_owner[memory.index()] = Some(region);
        }
    }
    let memory_signal_owner = memory_signal_owners(module, &memory_owner)?;
    let mut edges = build_edges(
        module,
        &value_keys,
        &operation_owner,
        &memory_owner,
        &memory_signal_owner,
        &drivers,
    )?;
    seal_edge_semantic_keys(module, &value_keys, &anchors, &regions, &mut edges)?;
    seal_region_identities(module, &value_keys, &edges, &mut regions)?;
    canonicalize(
        module,
        regions,
        edges,
        operation_owner,
        memory_owner,
        anchors.into_vec(),
    )
}

fn merge_ownership_claims(
    module: &word::WordModule,
    ownership: &crate::regional::StructuralOwnershipProvenance,
    regions: &mut Vec<TempRegion>,
) -> Result<(), crate::SynthError> {
    if ownership.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "final partition received incomplete structural ownership provenance",
        ));
    }
    let mut operation_regions = vec![None; module.operations().len()];
    for (region, contents) in regions.iter().enumerate() {
        for operation in &contents.operations {
            operation_regions[operation.index()] = Some(region);
        }
    }
    let mut parents = (0..regions.len()).collect::<Vec<_>>();
    let mut owner_regions = BTreeMap::new();
    for (index, region) in operation_regions.iter().copied().enumerate() {
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        let Some(owner) = ownership.owner(operation) else {
            continue;
        };
        let Some(region) = region else {
            continue;
        };
        if let Some(previous) = owner_regions.insert(owner, region) {
            union_region_claims(&mut parents, region, previous);
        }
    }
    if parents
        .iter()
        .enumerate()
        .all(|(index, &parent)| index == parent)
    {
        return Ok(());
    }
    for index in 0..parents.len() {
        let root = find_region_claim(&mut parents, index);
        parents[index] = root;
    }
    let mut merged = BTreeMap::<usize, TempRegion>::new();
    for (index, region) in std::mem::take(regions).into_iter().enumerate() {
        let root = parents[index];
        match merged.entry(root) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(region);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let target = entry.get_mut();
                target.operations.extend(region.operations);
                target.operations.sort_unstable();
                target.work = target.work.saturating_add(region.work);
                target.delay = target.delay.saturating_add(region.delay);
                target.wiring = target.wiring.saturating_add(region.wiring);
                target.anchor = target.anchor.min(region.anchor);
                if region.kind == SynthesisRegionKind::State {
                    target.kind = SynthesisRegionKind::State;
                }
            }
        }
    }
    *regions = merged.into_values().collect();
    Ok(())
}

fn find_region_claim(parents: &mut [usize], mut region: usize) -> usize {
    while parents[region] != region {
        let parent = parents[region];
        parents[region] = parents[parent];
        region = parents[region];
    }
    region
}

fn union_region_claims(parents: &mut [usize], left: usize, right: usize) {
    let left = find_region_claim(parents, left);
    let right = find_region_claim(parents, right);
    if left == right {
        return;
    }
    let (root, child) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    parents[child] = root;
}

fn partition_operations(
    module: &word::WordModule,
    anchors: &[OperationAnchorId],
    drivers: &SignalDriverIndex,
    policy: RegionPartitionPolicy,
) -> Result<Vec<TempRegion>, crate::SynthError> {
    let mut input_operations = InputOperations::new(module, drivers);
    let dependencies = operation_dependencies(module, &mut input_operations)?;
    let roots = synthesis_root_operations(module, &mut input_operations);
    let reachable = synthesis_root_closure(&dependencies, &roots);
    let estimates = StructuralEstimateIndex::build(module, &dependencies);
    if let Some(region) = whole_design_region(module, &reachable, &estimates, policy)? {
        return Ok(vec![region]);
    }
    let (components, component_of) = dependency_components(module, &dependencies);
    let criticality = estimates.criticality(&dependencies);
    let seeds = initial_seeds(module, anchors, &criticality, &roots, &reachable);
    let mut regions = claim_cones(ConeClaimInputs {
        module,
        anchors,
        dependencies: &dependencies,
        components: &components,
        component_of: &component_of,
        criticality: &criticality,
        estimates: &estimates,
        reachable: &reachable,
        seeds,
        size_limit: policy.target_work,
    })?;
    coarsen_regions(
        module,
        &dependencies,
        &criticality,
        policy.target_work,
        &mut regions,
    );
    Ok(regions)
}

fn whole_design_region(
    module: &word::WordModule,
    reachable: &[bool],
    estimates: &StructuralEstimateIndex,
    policy: RegionPartitionPolicy,
) -> Result<Option<TempRegion>, crate::SynthError> {
    let mut operations = Vec::new();
    let mut work = 0u64;
    let mut delay = 0u64;
    let mut wiring = 0u64;
    let mut has_state = false;
    for (index, operation) in module.operations().iter().enumerate() {
        if !reachable[index] {
            continue;
        }
        operations.push(word::OpId::from_index(index).map_err(crate::SynthError::from)?);
        work = work.saturating_add(operation_work(module, operation));
        if work > policy.target_work {
            return Ok(None);
        }
        delay = delay.saturating_add(estimates.operations[index].delay);
        wiring = wiring.saturating_add(estimates.operations[index].wiring_units);
        has_state |= is_state(&operation.kind);
    }
    if operations.is_empty() {
        return Ok(None);
    }
    let mut anchor = blake3::Hasher::new();
    anchor.update(WHOLE_DESIGN_REGION_ANCHOR_DOMAIN);
    append_hash_text(&mut anchor, module.name());
    Ok(Some(TempRegion {
        anchor: *anchor.finalize().as_bytes(),
        kind: if has_state {
            SynthesisRegionKind::State
        } else {
            SynthesisRegionKind::Combinational
        },
        operations,
        memories: Vec::new(),
        work,
        delay,
        wiring,
        id: RegionAnchorId::from_bytes([0; 32]),
        revision: RegionRevision::from_bytes([0; 32]),
    }))
}

#[derive(Debug, Clone, Copy)]
struct StructuralEstimate {
    delay: u64,
    logic_units: u64,
    wiring_units: u64,
}

#[derive(Debug)]
struct StructuralEstimateIndex {
    operations: Box<[StructuralEstimate]>,
}

impl StructuralEstimateIndex {
    fn build(module: &word::WordModule, dependencies: &[Vec<usize>]) -> Self {
        let operations = module
            .operations()
            .iter()
            .enumerate()
            .map(|(index, operation)| {
                let width = module
                    .value(operation.result)
                    .map_or(1, |value| u64::from(value.ty.width()));
                let delay = match operation.kind {
                    word::OpKind::Register(_) | word::OpKind::Latch(_) => 0,
                    word::OpKind::Binary {
                        op: word::BinaryOp::Mul,
                        ..
                    } => width.max(1),
                    word::OpKind::Binary {
                        op: word::BinaryOp::Div | word::BinaryOp::Mod,
                        ..
                    } => width.saturating_mul(2).max(1),
                    word::OpKind::Binary {
                        op:
                            word::BinaryOp::Add
                            | word::BinaryOp::Sub
                            | word::BinaryOp::Lt
                            | word::BinaryOp::Le
                            | word::BinaryOp::Gt
                            | word::BinaryOp::Ge,
                        ..
                    } => u64::from(width.next_power_of_two().ilog2()).max(1),
                    _ => 1,
                };
                let wiring_units = dependencies[index].iter().fold(0u64, |total, &input| {
                    total.saturating_add(
                        module
                            .operations()
                            .get(input)
                            .and_then(|input| module.value(input.result))
                            .map_or(1, |value| u64::from(value.ty.width())),
                    )
                });
                StructuralEstimate {
                    delay,
                    logic_units: operation_work(module, operation),
                    wiring_units,
                }
            })
            .collect();
        Self { operations }
    }

    fn criticality(&self, dependencies: &[Vec<usize>]) -> Box<[u64]> {
        let mut consumers = vec![Vec::new(); dependencies.len()];
        let mut pending_inputs = vec![0usize; dependencies.len()];
        for (sink, inputs) in dependencies.iter().enumerate() {
            if self.operations[sink].delay == 0 {
                continue;
            }
            for &source in inputs {
                if self.operations[source].delay != 0 {
                    consumers[source].push(sink);
                    pending_inputs[sink] += 1;
                }
            }
        }
        let mut ready = (0..dependencies.len())
            .filter(|&operation| pending_inputs[operation] == 0)
            .collect::<BTreeSet<_>>();
        let mut arrival = vec![0u64; dependencies.len()];
        let mut order = Vec::with_capacity(dependencies.len());
        while let Some(operation) = ready.pop_first() {
            let estimate = self.operations[operation];
            let input_arrival = dependencies[operation]
                .iter()
                .filter(|&&input| self.operations[input].delay != 0)
                .map(|&input| arrival[input])
                .max()
                .unwrap_or(0);
            arrival[operation] = input_arrival
                .saturating_add(estimate.delay)
                .saturating_add(estimate.wiring_units.min(estimate.logic_units) / 64);
            order.push(operation);
            for &consumer in &consumers[operation] {
                pending_inputs[consumer] -= 1;
                if pending_inputs[consumer] == 0 {
                    ready.insert(consumer);
                }
            }
        }
        let mut remaining = vec![0u64; dependencies.len()];
        for &operation in order.iter().rev() {
            remaining[operation] = consumers[operation]
                .iter()
                .map(|&consumer| {
                    self.operations[consumer]
                        .delay
                        .saturating_add(remaining[consumer])
                })
                .max()
                .unwrap_or(0);
        }
        arrival
            .into_iter()
            .zip(remaining)
            .map(|(arrival, remaining)| arrival.saturating_add(remaining))
            .collect()
    }
}

fn operation_dependencies(
    module: &word::WordModule,
    inputs: &mut InputOperations<'_>,
) -> Result<Vec<Vec<usize>>, crate::SynthError> {
    module
        .operations()
        .iter()
        .map(|operation| {
            let mut dependencies = Vec::new();
            for value in crate::word::operation_inputs(&operation.kind) {
                dependencies.extend_from_slice(inputs.resolve(value));
            }
            dependencies.sort_unstable();
            dependencies.dedup();
            Ok(dependencies)
        })
        .collect()
}

fn synthesis_root_values(module: &word::WordModule) -> Vec<word::ValueId> {
    let outputs = module
        .ports()
        .iter()
        .filter(|port| {
            matches!(
                port.direction,
                word::PortDirection::Output | word::PortDirection::Inout
            )
        })
        .map(|port| port.signal)
        .chain(module.preserved_signals())
        .collect::<BTreeSet<_>>();
    let mut roots = module
        .connects()
        .iter()
        .filter(|connect| outputs.contains(&connect.target.signal))
        .flat_map(|connect| {
            std::iter::once(connect.value)
                .chain(connect.target.dynamic.map(|dynamic| dynamic.offset))
        })
        .chain(
            module
                .instances()
                .iter()
                .flat_map(|instance| &instance.connections)
                .map(|connection| connection.value),
        )
        .chain(
            module
                .memory_read_ports()
                .iter()
                .flat_map(memory_read_inputs),
        )
        .chain(
            module
                .memory_write_ports()
                .iter()
                .flat_map(memory_write_inputs),
        )
        .collect::<Vec<_>>();
    roots.sort_unstable();
    roots.dedup();
    roots
}

fn synthesis_root_operations(
    module: &word::WordModule,
    inputs: &mut InputOperations<'_>,
) -> BTreeSet<usize> {
    let mut roots = module
        .operations()
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| is_state(&operation.kind).then_some(index))
        .collect::<BTreeSet<_>>();
    for value in synthesis_root_values(module) {
        roots.extend(inputs.resolve(value));
    }
    roots
}

fn synthesis_root_closure(dependencies: &[Vec<usize>], roots: &BTreeSet<usize>) -> Box<[bool]> {
    let mut reachable = vec![false; dependencies.len()];
    let mut pending = roots.iter().copied().collect::<Vec<_>>();
    while let Some(operation) = pending.pop() {
        if std::mem::replace(&mut reachable[operation], true) {
            continue;
        }
        pending.extend(dependencies[operation].iter().copied());
    }
    reachable.into_boxed_slice()
}

fn initial_seeds(
    module: &word::WordModule,
    anchors: &[OperationAnchorId],
    criticality: &[u64],
    roots: &BTreeSet<usize>,
    reachable: &[bool],
) -> BTreeMap<(std::cmp::Reverse<u64>, OperationAnchorId), BTreeSet<usize>> {
    let mut seeds = BTreeMap::new();
    for (index, operation) in module.operations().iter().enumerate() {
        if reachable[index] && is_state(&operation.kind) {
            enqueue_seed(&mut seeds, index, anchors, criticality);
        }
    }
    for &operation in roots {
        enqueue_seed(&mut seeds, operation, anchors, criticality);
    }
    seeds
}

fn enqueue_seed(
    seeds: &mut BTreeMap<(std::cmp::Reverse<u64>, OperationAnchorId), BTreeSet<usize>>,
    operation: usize,
    anchors: &[OperationAnchorId],
    criticality: &[u64],
) {
    seeds
        .entry((
            std::cmp::Reverse(criticality[operation]),
            anchors[operation],
        ))
        .or_default()
        .insert(operation);
}

struct ConeClaimInputs<'a> {
    module: &'a word::WordModule,
    anchors: &'a [OperationAnchorId],
    dependencies: &'a [Vec<usize>],
    components: &'a [Vec<usize>],
    component_of: &'a [usize],
    criticality: &'a [u64],
    estimates: &'a StructuralEstimateIndex,
    reachable: &'a [bool],
    seeds: BTreeMap<(std::cmp::Reverse<u64>, OperationAnchorId), BTreeSet<usize>>,
    size_limit: u64,
}

fn claim_cones(request: ConeClaimInputs<'_>) -> Result<Vec<TempRegion>, crate::SynthError> {
    let ConeClaimInputs {
        module,
        anchors,
        dependencies,
        components,
        component_of,
        criticality,
        estimates,
        reachable,
        mut seeds,
        size_limit,
    } = request;
    let mut owners = vec![None; module.operations().len()];
    let mut regions = Vec::new();
    loop {
        if seeds.is_empty() {
            let next = owners
                .iter()
                .enumerate()
                .filter(|&(operation, owner)| reachable[operation] && owner.is_none())
                .max_by_key(|&(operation, _)| {
                    (
                        criticality[operation],
                        std::cmp::Reverse(anchors[operation]),
                    )
                })
                .map(|(operation, _)| operation);
            let Some(next) = next else { break };
            enqueue_seed(&mut seeds, next, anchors, criticality);
        }
        let (key, roots) = seeds.pop_first().expect("nonempty seed queue was checked");
        let mut pending = roots.iter().copied().collect::<Vec<_>>();
        pending.sort_unstable_by_key(|&operation| {
            (
                criticality[operation],
                std::cmp::Reverse(anchors[operation]),
            )
        });
        let mut operations = Vec::new();
        let mut work = 0u64;
        let mut delay = 0u64;
        let mut wiring = 0u64;
        let region_index = regions.len();
        while let Some(operation) = pending.pop() {
            if owners[operation].is_some() {
                continue;
            }
            let component = &components[component_of[operation]];
            let component_work = component.iter().fold(0u64, |total, &member| {
                total.saturating_add(operation_work(module, &module.operations()[member]))
            });
            if !operations.is_empty() && work.saturating_add(component_work) > size_limit {
                enqueue_seed(&mut seeds, operation, anchors, criticality);
                continue;
            }
            let mut inputs = Vec::new();
            for &member in component {
                if owners[member].replace(region_index).is_some() {
                    return Err(crate::SynthError::invariant(
                        "dependency component was split between cone owners",
                    ));
                }
                operations.push(word::OpId::from_index(member).map_err(crate::SynthError::from)?);
                inputs.extend(dependencies[member].iter().copied().filter(|&input| {
                    component_of[input] != component_of[member]
                        && (!is_state(&module.operations()[input].kind) || roots.contains(&input))
                }));
            }
            work = work.saturating_add(component_work);
            delay = component.iter().fold(delay, |total, &member| {
                total.saturating_add(estimates.operations[member].delay)
            });
            wiring = component.iter().fold(wiring, |total, &member| {
                total.saturating_add(estimates.operations[member].wiring_units)
            });
            inputs.sort_unstable_by_key(|&input| {
                (criticality[input], std::cmp::Reverse(anchors[input]))
            });
            inputs.dedup();
            pending.extend(inputs);
        }
        if operations.is_empty() {
            continue;
        }
        operations.sort_unstable();
        let kind = if operations
            .iter()
            .any(|operation| is_state(&module.operations()[operation.index()].kind))
        {
            SynthesisRegionKind::State
        } else {
            SynthesisRegionKind::Combinational
        };
        regions.push(TempRegion {
            anchor: key.1.bytes(),
            kind,
            operations,
            memories: Vec::new(),
            work,
            delay,
            wiring,
            id: RegionAnchorId::from_bytes([0; 32]),
            revision: RegionRevision::from_bytes([0; 32]),
        });
    }
    Ok(regions)
}

fn dependency_components(
    module: &word::WordModule,
    dependencies: &[Vec<usize>],
) -> (Vec<Vec<usize>>, Box<[usize]>) {
    let mut consumers = vec![Vec::new(); dependencies.len()];
    for (sink, inputs) in dependencies.iter().enumerate() {
        if is_state(&module.operations()[sink].kind) {
            continue;
        }
        for &source in inputs {
            if !is_state(&module.operations()[source].kind) {
                consumers[source].push(sink);
            }
        }
    }
    let mut visited = vec![false; dependencies.len()];
    let mut finish = Vec::with_capacity(dependencies.len());
    for start in 0..dependencies.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![(start, 0usize)];
        while let Some((operation, next)) = stack.pop() {
            if let Some(&consumer) = consumers[operation].get(next) {
                stack.push((operation, next + 1));
                if !visited[consumer] {
                    visited[consumer] = true;
                    stack.push((consumer, 0));
                }
            } else {
                finish.push(operation);
            }
        }
    }
    visited.fill(false);
    let mut components = Vec::new();
    let mut component_of = vec![0usize; dependencies.len()];
    for start in finish.into_iter().rev() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut component = Vec::new();
        let mut pending = vec![start];
        while let Some(operation) = pending.pop() {
            component.push(operation);
            if is_state(&module.operations()[operation].kind) {
                continue;
            }
            for &input in &dependencies[operation] {
                if !is_state(&module.operations()[input].kind) && !visited[input] {
                    visited[input] = true;
                    pending.push(input);
                }
            }
        }
        component.sort_unstable();
        let index = components.len();
        for &operation in &component {
            component_of[operation] = index;
        }
        components.push(component);
    }
    (components, component_of.into_boxed_slice())
}

fn coarsen_regions(
    module: &word::WordModule,
    dependencies: &[Vec<usize>],
    criticality: &[u64],
    size_limit: u64,
    regions: &mut Vec<TempRegion>,
) {
    let mut owners = vec![None; module.operations().len()];
    for (region, contents) in regions.iter().enumerate() {
        for operation in &contents.operations {
            owners[operation.index()] = Some(region);
        }
    }
    for _ in 0..MATCHING_ROUNDS {
        let mut edges = BTreeMap::<(usize, usize), u64>::new();
        for (sink, inputs) in dependencies.iter().enumerate() {
            let Some(sink_region) = owners[sink] else {
                continue;
            };
            for &source in inputs {
                let Some(source_region) = owners[source] else {
                    continue;
                };
                if source_region == sink_region {
                    continue;
                }
                let pair = if source_region < sink_region {
                    (source_region, sink_region)
                } else {
                    (sink_region, source_region)
                };
                let edge = criticality[source].min(criticality[sink]);
                edges
                    .entry(pair)
                    .and_modify(|weight| *weight = (*weight).max(edge))
                    .or_insert(edge);
            }
        }
        let mut nominations = vec![None; regions.len()];
        for (&(left, right), &weight) in &edges {
            if regions[left].operations.is_empty()
                || regions[right].operations.is_empty()
                || regions[left].work >= size_limit
                || regions[right].work >= size_limit
                || regions[left].work.saturating_add(regions[right].work) > size_limit
            {
                continue;
            }
            nominate(regions, &mut nominations, left, right, weight);
            nominate(regions, &mut nominations, right, left, weight);
        }
        let pairs = nominations
            .iter()
            .enumerate()
            .filter_map(|(left, &nomination)| {
                nomination.and_then(|(_, right)| {
                    (left < right && nominations[right].is_some_and(|(_, peer)| peer == left))
                        .then_some((left, right))
                })
            })
            .collect::<Vec<_>>();
        for (left, right) in pairs {
            let (survivor, removed) = if regions[left].anchor <= regions[right].anchor {
                (left, right)
            } else {
                (right, left)
            };
            let removed_region = std::mem::replace(
                &mut regions[removed],
                empty_unsealed_region(SynthesisRegionKind::Combinational, Vec::new(), 0),
            );
            regions[survivor]
                .operations
                .extend(removed_region.operations);
            regions[survivor].operations.sort_unstable();
            regions[survivor].work = regions[survivor].work.saturating_add(removed_region.work);
            regions[survivor].delay = regions[survivor].delay.saturating_add(removed_region.delay);
            regions[survivor].wiring = regions[survivor]
                .wiring
                .saturating_add(removed_region.wiring);
            if removed_region.kind == SynthesisRegionKind::State {
                regions[survivor].kind = SynthesisRegionKind::State;
            }
            regions[survivor].anchor = regions[survivor].anchor.min(removed_region.anchor);
            for operation in &regions[survivor].operations {
                owners[operation.index()] = Some(survivor);
            }
        }
    }
    regions.retain(|region| !region.operations.is_empty());
}

fn nominate(
    regions: &[TempRegion],
    nominations: &mut [Option<(u64, usize)>],
    source: usize,
    candidate: usize,
    weight: u64,
) {
    let replace = nominations[source].is_none_or(|(current_weight, current)| {
        weight > current_weight
            || (weight == current_weight && regions[candidate].anchor < regions[current].anchor)
    });
    if replace {
        nominations[source] = Some((weight, candidate));
    }
}

fn empty_unsealed_region(
    kind: SynthesisRegionKind,
    operations: Vec<word::OpId>,
    work: u64,
) -> TempRegion {
    TempRegion {
        anchor: [0; 32],
        kind,
        operations,
        memories: Vec::new(),
        work,
        delay: 0,
        wiring: 0,
        id: RegionAnchorId::from_bytes([0; 32]),
        revision: RegionRevision::from_bytes([0; 32]),
    }
}

fn append_memory_regions(
    module: &word::WordModule,
    regions: &mut Vec<TempRegion>,
) -> Result<(), crate::SynthError> {
    for (index, memory) in module.memories().iter().enumerate() {
        let memory_id = word::MemoryId::from_index(index).map_err(crate::SynthError::from)?;
        let port_work = module
            .memory_read_ports()
            .iter()
            .filter(|port| port.memory == memory_id)
            .count()
            .saturating_add(
                module
                    .memory_write_ports()
                    .iter()
                    .filter(|port| port.memory == memory_id)
                    .count(),
            );
        regions.push(TempRegion {
            anchor: {
                let mut digest = blake3::Hasher::new();
                digest.update(MEMORY_ANCHOR_DOMAIN);
                append_hash_text(&mut digest, module.name());
                append_hash_text(&mut digest, module.name_str(memory.name));
                *digest.finalize().as_bytes()
            },
            kind: SynthesisRegionKind::Memory,
            operations: Vec::new(),
            memories: vec![memory_id],
            work: memory_work(memory, port_work),
            delay: 0,
            wiring: 0,
            id: RegionAnchorId::from_bytes([0; 32]),
            revision: RegionRevision::from_bytes([0; 32]),
        });
    }
    Ok(())
}

fn memory_signal_owners(
    module: &word::WordModule,
    memory_owner: &[Option<usize>],
) -> Result<BTreeMap<word::SignalId, usize>, crate::SynthError> {
    let mut owners = BTreeMap::new();
    for port in module.memory_read_ports() {
        let owner = memory_owner
            .get(port.memory.index())
            .copied()
            .flatten()
            .ok_or_else(|| crate::SynthError::invariant("memory read has no region owner"))?;
        if owners.insert(port.data, owner).is_some() {
            return Err(crate::SynthError::invariant(
                "memory read signal has more than one producer region",
            ));
        }
    }
    Ok(owners)
}

fn build_edges(
    module: &word::WordModule,
    value_keys: &[[u8; 32]],
    operation_owner: &[Option<usize>],
    memory_owner: &[Option<usize>],
    memory_signal_owner: &BTreeMap<word::SignalId, usize>,
    drivers: &SignalDriverIndex,
) -> Result<Vec<TempEdge>, crate::SynthError> {
    let mut edges = BTreeSet::new();
    let connectivity = ConnectivityIndex::new(
        module,
        value_keys,
        operation_owner,
        memory_signal_owner,
        drivers,
    );
    for (index, operation) in module.operations().iter().enumerate() {
        let Some(sink) = operation_owner[index] else {
            continue;
        };
        for value in crate::word::operation_inputs(&operation.kind) {
            connectivity.append_input_edge(value, sink, &mut edges)?;
        }
    }
    for read in module.memory_read_ports() {
        let sink = memory_owner[read.memory.index()]
            .ok_or_else(|| crate::SynthError::invariant("memory read has no region owner"))?;
        for value in memory_read_inputs(read) {
            connectivity.append_input_edge(value, sink, &mut edges)?;
        }
    }
    for write in module.memory_write_ports() {
        let sink = memory_owner[write.memory.index()]
            .ok_or_else(|| crate::SynthError::invariant("memory write has no region owner"))?;
        for value in memory_write_inputs(write) {
            connectivity.append_input_edge(value, sink, &mut edges)?;
        }
    }
    for value in synthesis_root_values(module) {
        if let Some(source) = connectivity.value_region(value) {
            let stored = module.value(value).ok_or_else(|| {
                crate::SynthError::invariant("structural root references an unknown value")
            })?;
            edges.insert(TempEdge {
                source: Some(source),
                sink: None,
                value,
                endpoint: temp_endpoint(module, value)?,
                ty: stored.ty,
                semantic_key: value_keys[value.index()],
                value_revision: value_keys[value.index()],
            });
        }
    }
    Ok(edges.into_iter().collect())
}

fn seal_region_identities(
    module: &word::WordModule,
    value_keys: &[[u8; 32]],
    edges: &[TempEdge],
    regions: &mut [TempRegion],
) -> Result<(), crate::SynthError> {
    for (index, region) in regions.iter_mut().enumerate() {
        let mut local = blake3::Hasher::new();
        local.update(REGION_LOCAL_KEY_DOMAIN);
        append_region_content_identity(module, region, value_keys, &mut local)?;
        let mut boundary = edges
            .iter()
            .filter_map(|edge| {
                if edge.source == Some(index) {
                    Some((1u8, edge.sink.is_none(), edge.ty, edge.semantic_key))
                } else if edge.sink == Some(index) {
                    Some((0u8, edge.source.is_none(), edge.ty, edge.semantic_key))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        boundary.sort_unstable_by_key(|&(direction, external, ty, key)| {
            (direction, external, type_key(ty), key)
        });
        local.update(&(boundary.len() as u64).to_le_bytes());
        for (direction, external, ty, key) in boundary {
            local.update(&[direction, u8::from(external)]);
            append_type_hash(&mut local, ty);
            local.update(&key);
        }
        region.revision = RegionRevision::from_bytes(*local.finalize().as_bytes());
        let mut anchor = blake3::Hasher::new();
        anchor.update(REGION_ID_DOMAIN);
        anchor.update(&region.anchor);
        region.id = RegionAnchorId::from_bytes(*anchor.finalize().as_bytes());
    }
    let mut ids = BTreeSet::new();
    if regions.iter().any(|region| !ids.insert(region.id)) {
        return Err(crate::SynthError::invariant(
            "stable region anchors are not unique; frontend syntax provenance is incomplete",
        ));
    }
    Ok(())
}

fn seal_edge_semantic_keys(
    module: &word::WordModule,
    value_keys: &[[u8; 32]],
    operation_anchors: &[OperationAnchorId],
    regions: &[TempRegion],
    edges: &mut [TempEdge],
) -> Result<(), crate::SynthError> {
    for edge in edges {
        let mut digest = blake3::Hasher::new();
        digest.update(BOUNDARY_EDGE_ID_DOMAIN);
        append_optional_anchor(&mut digest, edge.source, regions);
        append_optional_anchor(&mut digest, edge.sink, regions);
        append_boundary_endpoint(
            module,
            value_keys,
            operation_anchors,
            edge.value,
            &mut digest,
        )?;
        append_type_hash(&mut digest, edge.ty);
        edge.semantic_key = *digest.finalize().as_bytes();
    }
    Ok(())
}

fn append_optional_anchor(
    digest: &mut blake3::Hasher,
    region: Option<usize>,
    regions: &[TempRegion],
) {
    if let Some(region) = region {
        digest.update(&[1]);
        digest.update(&regions[region].anchor);
    } else {
        digest.update(&[0]);
    }
}

fn append_boundary_endpoint(
    module: &word::WordModule,
    value_keys: &[[u8; 32]],
    operation_anchors: &[OperationAnchorId],
    value: word::ValueId,
    digest: &mut blake3::Hasher,
) -> Result<(), crate::SynthError> {
    let stored = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("boundary endpoint value is unknown"))?;
    match stored.kind {
        word::ValueKind::Signal(reference) => {
            digest.update(&[0]);
            let signal = module.signal(reference.signal).ok_or_else(|| {
                crate::SynthError::invariant("boundary endpoint signal is unknown")
            })?;
            append_hash_text(digest, signal.name.map_or("", |name| module.name_str(name)));
            digest.update(&reference.lsb.to_le_bytes());
            digest.update(&reference.width().to_le_bytes());
        }
        word::ValueKind::Operation(operation) => {
            digest.update(&[1]);
            digest.update(&operation_anchors[operation.index()].bytes());
        }
        word::ValueKind::Constant(_) => {
            digest.update(&[2]);
            digest.update(&value_keys[value.index()]);
        }
    }
    Ok(())
}

fn append_region_content_identity(
    module: &word::WordModule,
    region: &TempRegion,
    value_keys: &[[u8; 32]],
    digest: &mut blake3::Hasher,
) -> Result<(), crate::SynthError> {
    digest.update(&[match region.kind {
        SynthesisRegionKind::Combinational => 0,
        SynthesisRegionKind::State => 1,
        SynthesisRegionKind::Memory => 2,
    }]);
    digest.update(&(region.operations.len() as u64).to_le_bytes());
    let mut operation_keys = region
        .operations
        .iter()
        .map(|operation| value_keys[module.operations()[operation.index()].result.index()])
        .collect::<Vec<_>>();
    operation_keys.sort_unstable();
    for key in operation_keys {
        digest.update(&key);
    }
    append_memory_identity(module, region, value_keys, digest)
}

fn append_memory_identity(
    module: &word::WordModule,
    region: &TempRegion,
    value_keys: &[[u8; 32]],
    digest: &mut blake3::Hasher,
) -> Result<(), crate::SynthError> {
    digest.update(&(region.memories.len() as u64).to_le_bytes());
    for &memory_id in &region.memories {
        let memory = module
            .memory(memory_id)
            .ok_or_else(|| crate::SynthError::invariant("region owns an unknown memory"))?;
        append_hash_text(digest, module.name_str(memory.name));
        append_type_hash(digest, memory.element_type);
        digest.update(&memory.depth.get().to_le_bytes());
        for read in module
            .memory_read_ports()
            .iter()
            .filter(|read| read.memory == memory_id)
        {
            digest.update(&[0]);
            digest.update(&value_keys[read.address.index()]);
            digest.update(&[match read.timing {
                word::MemoryReadTiming::Asynchronous => 0,
                word::MemoryReadTiming::Synchronous { .. } => 1,
            }]);
        }
        for write in module
            .memory_write_ports()
            .iter()
            .filter(|write| write.memory == memory_id)
        {
            digest.update(&[1]);
            digest.update(&value_keys[write.address.index()]);
            digest.update(&value_keys[write.data.index()]);
            digest.update(&write.priority.to_le_bytes());
        }
    }
    Ok(())
}

fn canonicalize(
    module: &word::WordModule,
    mut regions: Vec<TempRegion>,
    edges: Vec<TempEdge>,
    operation_owners: Vec<Option<usize>>,
    memory_owners: Vec<Option<usize>>,
    operation_anchors: Vec<OperationAnchorId>,
) -> Result<SynthesisRegionGraph, crate::SynthError> {
    let graph_owner = RegionGraphOwnerId::fresh();
    let mut order = (0..regions.len()).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&region| regions[region].id);
    let mut old_to_new = vec![RegionRowId::from_index(0)?; regions.len()];
    for (row, &old) in order.iter().enumerate() {
        old_to_new[old] = RegionRowId::from_index(row)?;
    }
    let operation_owners = remap_optional_owner_rows(operation_owners, &old_to_new);
    let memory_owners = remap_owner_rows(memory_owners, &old_to_new, "Word memory")?;
    let mut canonical_edges = edges
        .into_iter()
        .map(|edge| CanonicalEdge {
            source: edge.source.map(|region| old_to_new[region]),
            sink: edge.sink.map(|region| old_to_new[region]),
            value: edge.value,
            ty: edge.ty,
            semantic_key: edge.semantic_key,
            value_revision: edge.value_revision,
        })
        .collect::<Vec<_>>();
    canonical_edges.sort_by_key(|edge| {
        (
            edge.source.map(RegionRowId::raw),
            edge.sink.map(RegionRowId::raw),
            edge.semantic_key,
            edge.value.raw(),
        )
    });

    let mut ports = Vec::with_capacity(canonical_edges.len().saturating_mul(2));
    let mut input_ports = vec![Vec::new(); regions.len()];
    let mut output_ports = vec![Vec::new(); regions.len()];
    let mut predecessors = vec![BTreeSet::new(); regions.len()];
    let mut successors = vec![BTreeSet::new(); regions.len()];
    for edge in canonical_edges {
        if let Some(source) = edge.source {
            let id = RegionBoundaryPortId::from_index(ports.len())?;
            ports.push(RegionBoundaryPort {
                id,
                owner: source,
                peer: edge.sink,
                direction: RegionPortDirection::Output,
                value: edge.value,
                ty: edge.ty,
                stable_id: boundary_endpoint_id(edge.semantic_key, RegionPortDirection::Output),
                value_revision: BoundaryValueRevision::from_bytes(edge.value_revision),
                edge_key: edge.semantic_key,
            });
            output_ports[source.index()].push(id);
        }
        if let Some(sink) = edge.sink {
            let id = RegionBoundaryPortId::from_index(ports.len())?;
            ports.push(RegionBoundaryPort {
                id,
                owner: sink,
                peer: edge.source,
                direction: RegionPortDirection::Input,
                value: edge.value,
                ty: edge.ty,
                stable_id: boundary_endpoint_id(edge.semantic_key, RegionPortDirection::Input),
                value_revision: BoundaryValueRevision::from_bytes(edge.value_revision),
                edge_key: edge.semantic_key,
            });
            input_ports[sink.index()].push(id);
        }
        if let (Some(source), Some(sink)) = (edge.source, edge.sink) {
            successors[source.index()].insert(sink);
            predecessors[sink.index()].insert(source);
        }
    }

    let mut rows = Vec::with_capacity(regions.len());
    let mut operations = Vec::with_capacity(regions.len());
    let mut memories = Vec::with_capacity(regions.len());
    for (row_index, old) in order.into_iter().enumerate() {
        let row = RegionRowId::from_index(row_index)?;
        let region = std::mem::replace(
            &mut regions[old],
            empty_unsealed_region(SynthesisRegionKind::Combinational, Vec::new(), 0),
        );
        rows.push(SynthesisRegion {
            graph_owner,
            row,
            id: region.id,
            revision: region.revision,
            kind: region.kind,
            estimated_work: region.work,
            estimated_delay: region.delay,
            estimated_wiring: region.wiring,
        });
        operations.push(region.operations);
        memories.push(region.memories);
    }
    let mut revision = blake3::Hasher::new();
    revision.update(REGION_REVISION_DOMAIN);
    append_hash_text(&mut revision, module.name());
    for region in &rows {
        revision.update(&region.id().bytes());
        revision.update(&region.revision().bytes());
    }
    for port in &ports {
        revision.update(&port.owner().raw().to_le_bytes());
        revision.update(&port.peer().map_or(u32::MAX, RegionRowId::raw).to_le_bytes());
        revision.update(&[port.direction() as u8]);
        revision.update(&port.semantic_key());
        revision.update(&port.stable_id().bytes());
        revision.update(&port.value_revision().bytes());
    }
    let graph = SynthesisRegionGraph {
        owner: graph_owner,
        revision: SynthesisRegionRevision::from_bytes(*revision.finalize().as_bytes()),
        regions: rows.into_boxed_slice(),
        operations: packed_rows(operations, "region operation membership")?,
        operation_anchors: operation_anchors.into_boxed_slice(),
        operation_owners,
        memories: packed_rows(memories, "region memory membership")?,
        memory_owners,
        ports: ports.into_boxed_slice(),
        input_ports: packed_rows(input_ports, "region input ports")?,
        output_ports: packed_rows(output_ports, "region output ports")?,
        predecessors: packed_rows(
            predecessors
                .into_iter()
                .map(BTreeSet::into_iter)
                .map(Iterator::collect)
                .collect(),
            "region predecessors",
        )?,
        successors: packed_rows(
            successors
                .into_iter()
                .map(BTreeSet::into_iter)
                .map(Iterator::collect)
                .collect(),
            "region successors",
        )?,
    };
    graph.validate_for_module(module)?;
    Ok(graph)
}

fn boundary_endpoint_id(edge: [u8; 32], direction: RegionPortDirection) -> BoundaryPortId {
    let mut digest = blake3::Hasher::new();
    digest.update(BOUNDARY_ENDPOINT_ID_DOMAIN);
    digest.update(&edge);
    digest.update(&[direction as u8]);
    BoundaryPortId::from_bytes(*digest.finalize().as_bytes())
}

fn append_operation_hash(
    module: &word::WordModule,
    operation: &word::Operation,
    keys: &[[u8; 32]],
    digest: &mut blake3::Hasher,
) -> Result<(), crate::SynthError> {
    match &operation.kind {
        word::OpKind::Unary { op, arg } => {
            digest.update(&[0, *op as u8]);
            append_key(*arg, keys, digest)?;
        }
        word::OpKind::Binary { op, left, right } => {
            digest.update(&[1, *op as u8]);
            append_key(*left, keys, digest)?;
            append_key(*right, keys, digest)?;
        }
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            digest.update(&[2]);
            append_key(*cond, keys, digest)?;
            append_key(*then_value, keys, digest)?;
            append_key(*else_value, keys, digest)?;
        }
        word::OpKind::Concat { parts } => {
            digest.update(&[3]);
            digest.update(&(parts.len() as u64).to_le_bytes());
            for &part in parts {
                append_key(part, keys, digest)?;
            }
        }
        word::OpKind::Extract { value, lsb, width } => {
            digest.update(&[4]);
            append_key(*value, keys, digest)?;
            digest.update(&lsb.to_le_bytes());
            digest.update(&width.get().to_le_bytes());
        }
        word::OpKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            digest.update(&[5]);
            append_key(*value, keys, digest)?;
            append_key(*offset, keys, digest)?;
            digest.update(&width.get().to_le_bytes());
        }
        word::OpKind::DynamicInsert {
            value,
            offset,
            replacement,
        } => {
            digest.update(&[6]);
            append_key(*value, keys, digest)?;
            append_key(*offset, keys, digest)?;
            append_key(*replacement, keys, digest)?;
        }
        word::OpKind::Cast {
            kind,
            value,
            target,
        } => {
            digest.update(&[7, *kind as u8]);
            append_key(*value, keys, digest)?;
            append_type_hash(digest, *target);
        }
        word::OpKind::Register(register) => {
            digest.update(&[8]);
            append_hash_text(
                digest,
                register.name.map_or("", |name| module.name_str(name)),
            );
            append_key(register.d, keys, digest)?;
            append_key(register.clock, keys, digest)?;
            digest.update(&[register.edge as u8]);
            append_enable_hash(register.enable, keys, digest)?;
            digest.update(&(register.resets.len() as u64).to_le_bytes());
            for reset in &register.resets {
                digest.update(&[reset.kind as u8, u8::from(reset.active_high)]);
                append_key(reset.value, keys, digest)?;
                append_key(reset.reset_value, keys, digest)?;
            }
        }
        word::OpKind::Latch(latch) => {
            digest.update(&[9]);
            append_hash_text(digest, latch.name.map_or("", |name| module.name_str(name)));
            append_key(latch.d, keys, digest)?;
            append_key(latch.enable.value, keys, digest)?;
            digest.update(&[u8::from(latch.enable.active_high)]);
            digest.update(&(latch.resets.len() as u64).to_le_bytes());
            for reset in &latch.resets {
                digest.update(&[reset.kind as u8, u8::from(reset.active_high)]);
                append_key(reset.value, keys, digest)?;
                append_key(reset.reset_value, keys, digest)?;
            }
        }
    }
    Ok(())
}

fn append_enable_hash(
    enable: Option<word::Enable>,
    keys: &[[u8; 32]],
    digest: &mut blake3::Hasher,
) -> Result<(), crate::SynthError> {
    if let Some(enable) = enable {
        digest.update(&[1, u8::from(enable.active_high)]);
        append_key(enable.value, keys, digest)
    } else {
        digest.update(&[0]);
        Ok(())
    }
}

fn append_key(
    value: word::ValueId,
    keys: &[[u8; 32]],
    digest: &mut blake3::Hasher,
) -> Result<(), crate::SynthError> {
    digest.update(keys.get(value.index()).ok_or_else(|| {
        crate::SynthError::invariant("operation references a non-preceding Word value")
    })?);
    Ok(())
}

fn append_type_hash(digest: &mut blake3::Hasher, ty: word::WordType) {
    digest.update(&ty.width().to_le_bytes());
    digest.update(&[u8::from(ty.is_signed())]);
    digest.update(&[ty.state() as u8]);
}

fn type_key(ty: word::WordType) -> (u32, bool, u8) {
    (ty.width(), ty.is_signed(), ty.state() as u8)
}

fn append_hash_text(digest: &mut blake3::Hasher, text: &str) {
    digest.update(&(text.len() as u64).to_le_bytes());
    digest.update(text.as_bytes());
}
