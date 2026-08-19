// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic root-closure partitioning and final owner-atom coarsening.
//!
//! Initial partitioning may claim operation cones under bounded structural work
//! policy. Final partitioning instead consumes already frozen structural owner
//! atoms and may merge but never split them. Stable anchors derive from source
//! identity and semantic connectivity, never worker count or arena position.

use super::graph::{
    BoundaryPortId, BoundaryValueRevision, OperationAnchorId, RegionAnchorId, RegionBitFlow,
    RegionBoundaryPort, RegionBoundaryPortId, RegionGraphOwnerId, RegionPortDirection,
    RegionRevision, RegionRowId, SynthesisRegion, SynthesisRegionGraph, SynthesisRegionKind,
    SynthesisRegionRevision, packed_rows, remap_optional_owner_rows,
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
const COARSENING_ROUNDS: usize = 12;
const DEFAULT_TARGET_WORK: u64 = 32_768;

#[derive(Debug, Clone, Copy)]
/// Deterministic structural-work limits for partition activation and coarsening.
pub(crate) struct RegionPartitionPolicy {
    partition_start: u64,
    minimum: u64,
    target: u64,
    maximum: u64,
}

impl RegionPartitionPolicy {
    #[cfg(test)]
    pub(crate) const fn with_target_work(target_work: u64) -> Self {
        Self {
            partition_start: target_work,
            minimum: 1,
            target: target_work,
            maximum: target_work,
        }
    }

    #[cfg(test)]
    pub(crate) const fn with_work_limits(
        partition_start_work: u64,
        minimum_work: u64,
        target_work: u64,
        maximum_work: u64,
    ) -> Self {
        Self {
            partition_start: partition_start_work,
            minimum: minimum_work,
            target: target_work,
            maximum: maximum_work,
        }
    }
}

impl Default for RegionPartitionPolicy {
    fn default() -> Self {
        Self {
            // A region carries substantial import, planning, publication, and
            // scheduling overhead. Keep blocks within eight target workers as
            // one local problem unless their bounded-work estimate proves that
            // distribution is necessary.
            partition_start: DEFAULT_TARGET_WORK.saturating_mul(8),
            minimum: DEFAULT_TARGET_WORK / 8,
            target: DEFAULT_TARGET_WORK,
            maximum: DEFAULT_TARGET_WORK.saturating_mul(2),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TempBitFlow {
    source: usize,
    sink: Option<usize>,
    value: word::ValueId,
    bit: u32,
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

/// Builds the initial operation/memory partition from the synthesis root closure.
pub(crate) fn build(
    module: &word::WordModule,
    policy: RegionPartitionPolicy,
) -> Result<SynthesisRegionGraph, crate::SynthError> {
    build_inner(module, policy, None)
}

/// Builds the final graph while preserving every structural owner atom whole.
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
    if policy.partition_start == 0
        || policy.minimum == 0
        || policy.target == 0
        || policy.maximum < policy.target
        || policy.minimum > policy.target
    {
        return Err(crate::SynthError::invariant(
            "region work policy is inconsistent",
        ));
    }
    let drivers = SignalDriverIndex::new(module)?;
    let value_keys = semantic::value_keys(module)?;
    let anchors = operation_anchors(module)?;
    let mut regions = if let Some(ownership) = ownership {
        partition_owned_operations(module, &drivers, policy, ownership)?
    } else {
        partition_operations(module, &anchors, &drivers, policy)?
    };
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
    let (mut edges, bit_flows) = build_edges(
        module,
        &value_keys,
        &operation_owner,
        &memory_owner,
        &memory_signal_owner,
    )?;
    seal_edge_semantic_keys(module, &value_keys, &anchors, &regions, &mut edges)?;
    seal_region_identities(module, &value_keys, &edges, &bit_flows, &mut regions)?;
    canonicalize(
        module,
        regions,
        edges,
        operation_owner,
        memory_owner,
        anchors.into_vec(),
        bit_flows,
    )
}

fn partition_owned_operations(
    module: &word::WordModule,
    drivers: &SignalDriverIndex,
    policy: RegionPartitionPolicy,
    ownership: &crate::regional::StructuralOwnershipProvenance,
) -> Result<Vec<TempRegion>, crate::SynthError> {
    if ownership.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "final partition received incomplete structural ownership provenance",
        ));
    }
    let mut input_operations = InputOperations::new(module, drivers);
    let dependencies = operation_dependencies(module, &mut input_operations)?;
    let reachable = synthesis_reachable_operations(module)?;
    let estimates = StructuralEstimateIndex::build(module, &dependencies);
    let criticality = estimates.criticality(&dependencies);
    let mut owner_regions = BTreeMap::<RegionRowId, TempRegion>::new();
    for (index, &reachable) in reachable.iter().enumerate() {
        if !reachable {
            continue;
        }
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        let owner = ownership.owner(operation).ok_or_else(|| {
            crate::SynthError::invariant("live operation lost structural owner before final freeze")
        })?;
        let anchor = ownership.anchor(operation).ok_or_else(|| {
            crate::SynthError::invariant("live operation lost structural owner anchor")
        })?;
        let stored = &module.operations()[index];
        match owner_regions.entry(owner) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(TempRegion {
                    anchor,
                    kind: if is_state(&stored.kind) {
                        SynthesisRegionKind::State
                    } else {
                        SynthesisRegionKind::Combinational
                    },
                    operations: vec![operation],
                    memories: Vec::new(),
                    work: operation_work(module, stored),
                    delay: estimates.operations[index].delay,
                    wiring: estimates.operations[index].wiring_units,
                    id: RegionAnchorId::from_bytes([0; 32]),
                    revision: RegionRevision::from_bytes([0; 32]),
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let target = entry.get_mut();
                if target.anchor != anchor {
                    return Err(crate::SynthError::invariant(
                        "one structural owner has inconsistent stable anchors",
                    ));
                }
                target.operations.push(operation);
                target.work = target.work.saturating_add(operation_work(module, stored));
                target.delay = target
                    .delay
                    .saturating_add(estimates.operations[index].delay);
                target.wiring = target
                    .wiring
                    .saturating_add(estimates.operations[index].wiring_units);
                if is_state(&stored.kind) {
                    target.kind = SynthesisRegionKind::State;
                }
            }
        }
    }
    let mut regions = owner_regions.into_values().collect::<Vec<_>>();
    coarsen_regions(module, &dependencies, &criticality, policy, &mut regions)?;
    Ok(regions)
}

fn partition_operations(
    module: &word::WordModule,
    anchors: &[OperationAnchorId],
    drivers: &SignalDriverIndex,
    policy: RegionPartitionPolicy,
) -> Result<Vec<TempRegion>, crate::SynthError> {
    let mut input_operations = InputOperations::new(module, drivers);
    let dependencies = operation_dependencies(module, &mut input_operations)?;
    let roots = synthesis_root_operations(module, &mut input_operations)?;
    let reachable = synthesis_reachable_operations(module)?;
    let estimates = StructuralEstimateIndex::build(module, &dependencies);
    if let Some(region) = whole_design_region(module, &reachable, &estimates, policy)? {
        return Ok(vec![region]);
    }
    let (components, component_of) = dependency_components(module, &dependencies);
    let criticality = estimates.criticality(&dependencies);
    let seeds = initial_seeds(module, anchors, &criticality, &roots, &reachable);
    let mut regions = ConeClaimState {
        module,
        anchors,
        dependencies: &dependencies,
        components: &components,
        component_of: &component_of,
        criticality: &criticality,
        estimates: &estimates,
        reachable: &reachable,
        seeds,
        size_limit: policy.target,
        owners: vec![None; module.operations().len()],
        regions: Vec::new(),
    }
    .claim()?;
    coarsen_regions(module, &dependencies, &criticality, policy, &mut regions)?;
    Ok(regions)
}

pub(crate) fn synthesis_reachable_operations(
    module: &word::WordModule,
) -> Result<Box<[bool]>, crate::SynthError> {
    let observability = crate::word::uses::netlist_observability(module)?;
    module
        .operations()
        .iter()
        .map(|operation| {
            if matches!(operation.kind, word::OpKind::TriState { .. }) {
                // The resolved physical shell is not Boolean-owned; its data
                // and enable values are projected as roots instead.
                Ok(false)
            } else {
                observability.observes_value(operation.result)
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
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
        if work > policy.partition_start {
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

fn synthesis_root_values(
    module: &word::WordModule,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    let observability = crate::word::uses::netlist_observability(module)?;
    let mut roots = Vec::new();
    for &value in observability.root_values() {
        if let Some(word::OpKind::TriState { data, enable }) = module
            .value(value)
            .and_then(|value| match value.kind {
                word::ValueKind::Operation(operation) => module.operation(operation),
                word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
            })
            .map(|operation| &operation.kind)
        {
            roots.extend([*data, enable.value]);
        } else {
            roots.push(value);
        }
    }
    roots.sort_unstable();
    roots.dedup();
    Ok(roots)
}

fn synthesis_root_operations(
    module: &word::WordModule,
    inputs: &mut InputOperations<'_>,
) -> Result<BTreeSet<usize>, crate::SynthError> {
    let mut roots = BTreeSet::new();
    for value in synthesis_root_values(module)? {
        roots.extend(inputs.resolve(value));
    }
    Ok(roots)
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

/// Mutable ownership state for one deterministic cone-claim pass.
struct ConeClaimState<'a> {
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
    owners: Vec<Option<usize>>,
    regions: Vec<TempRegion>,
}

impl ConeClaimState<'_> {
    fn claim(self) -> Result<Vec<TempRegion>, crate::SynthError> {
        let Self {
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
            mut owners,
            mut regions,
        } = self;
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
                    operations
                        .push(word::OpId::from_index(member).map_err(crate::SynthError::from)?);
                    inputs.extend(dependencies[member].iter().copied().filter(|&input| {
                        component_of[input] != component_of[member]
                            && (!is_state(&module.operations()[input].kind)
                                || roots.contains(&input))
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
    policy: RegionPartitionPolicy,
    regions: &mut Vec<TempRegion>,
) -> Result<(), crate::SynthError> {
    let mut owners = vec![None; module.operations().len()];
    for (region, contents) in regions.iter().enumerate() {
        for operation in &contents.operations {
            owners[operation.index()] = Some(region);
        }
    }
    for _ in 0..COARSENING_ROUNDS {
        let edges = region_cut_gains(module, dependencies, criticality, &owners);
        let absorbed = absorb_fragments(&edges, policy, regions, &mut owners)?;
        if absorbed {
            continue;
        }
        if !merge_maximal_pairs(&edges, policy, regions, &mut owners) {
            break;
        }
    }
    regions.retain(|region| !region.operations.is_empty());
    Ok(())
}

fn region_cut_gains(
    module: &word::WordModule,
    dependencies: &[Vec<usize>],
    criticality: &[u64],
    owners: &[Option<usize>],
) -> BTreeMap<(usize, usize), u64> {
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
            let boundary_bits = module
                .value(module.operations()[source].result)
                .map_or(1, |value| u64::from(value.ty.width()))
                .max(1);
            let gain = criticality[source]
                .min(criticality[sink])
                .saturating_add(boundary_bits);
            edges
                .entry(pair)
                .and_modify(|weight| *weight = weight.saturating_add(gain))
                .or_insert(gain);
        }
    }
    edges
}

fn absorb_fragments(
    edges: &BTreeMap<(usize, usize), u64>,
    policy: RegionPartitionPolicy,
    regions: &mut [TempRegion],
    owners: &mut [Option<usize>],
) -> Result<bool, crate::SynthError> {
    let incident = region_incident_edges(regions.len(), edges)?;
    let mut proposals = BTreeMap::<usize, Vec<(u64, usize)>>::new();
    for source in 0..regions.len() {
        if regions[source].operations.is_empty() || regions[source].work >= policy.minimum {
            continue;
        }
        let neighbours = incident.get(source).ok_or_else(|| {
            crate::SynthError::invariant("region incident-edge index lost a region row")
        })?;
        let candidate = neighbours
            .iter()
            .filter_map(|&(receiver, gain)| {
                (!regions[receiver].operations.is_empty()
                    && regions[receiver].work >= policy.minimum
                    && regions[receiver].work.saturating_add(regions[source].work)
                        <= policy.maximum)
                    .then_some((gain, std::cmp::Reverse(regions[receiver].anchor), receiver))
            })
            .max();
        if let Some((gain, _, receiver)) = candidate {
            proposals.entry(receiver).or_default().push((gain, source));
        }
    }
    let mut changed = false;
    for (receiver, mut incoming) in proposals {
        incoming.sort_unstable_by_key(|&(gain, source)| {
            (std::cmp::Reverse(gain), regions[source].anchor)
        });
        for (_, source) in incoming {
            if regions[source].operations.is_empty()
                || regions[receiver].work.saturating_add(regions[source].work) > policy.maximum
            {
                continue;
            }
            merge_region_into(regions, owners, source, receiver);
            changed = true;
        }
    }
    Ok(changed)
}

fn region_incident_edges(
    region_count: usize,
    edges: &BTreeMap<(usize, usize), u64>,
) -> Result<opto_core::PackedRows<(usize, u64)>, crate::SynthError> {
    opto_core::PackedRows::try_from_entries(
        region_count,
        edges
            .iter()
            .flat_map(|(&(left, right), &gain)| [(left, (right, gain)), (right, (left, gain))]),
    )
    .map_err(|error| crate::SynthError::capacity(error.to_string()))
}

fn merge_maximal_pairs(
    edges: &BTreeMap<(usize, usize), u64>,
    policy: RegionPartitionPolicy,
    regions: &mut [TempRegion],
    owners: &mut [Option<usize>],
) -> bool {
    let mut candidates = edges
        .iter()
        .filter_map(|(&(left, right), &gain)| {
            (!regions[left].operations.is_empty()
                && !regions[right].operations.is_empty()
                && regions[left].work < policy.target
                && regions[right].work < policy.target
                && regions[left].work.saturating_add(regions[right].work) <= policy.maximum)
                .then_some((gain, left, right))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|&(gain, left, right)| {
        let anchors = if regions[left].anchor <= regions[right].anchor {
            (regions[left].anchor, regions[right].anchor)
        } else {
            (regions[right].anchor, regions[left].anchor)
        };
        (std::cmp::Reverse(gain), anchors)
    });
    let mut claimed = vec![false; regions.len()];
    let mut pairs = Vec::new();
    for (_, left, right) in candidates {
        if claimed[left] || claimed[right] {
            continue;
        }
        claimed[left] = true;
        claimed[right] = true;
        pairs.push((left, right));
    }
    for (left, right) in pairs.iter().copied() {
        let (source, receiver) = if regions[left].anchor <= regions[right].anchor {
            (right, left)
        } else {
            (left, right)
        };
        merge_region_into(regions, owners, source, receiver);
    }
    !pairs.is_empty()
}

fn merge_region_into(
    regions: &mut [TempRegion],
    owners: &mut [Option<usize>],
    source: usize,
    receiver: usize,
) {
    let removed = std::mem::replace(
        &mut regions[source],
        empty_unsealed_region(SynthesisRegionKind::Combinational, Vec::new(), 0),
    );
    let TempRegion {
        anchor,
        kind,
        operations,
        memories: _,
        work,
        delay,
        wiring,
        id: _,
        revision: _,
    } = removed;
    for operation in &operations {
        owners[operation.index()] = Some(receiver);
    }
    let receiver_operations = std::mem::take(&mut regions[receiver].operations);
    regions[receiver].operations = merge_sorted_operations(&receiver_operations, &operations);
    regions[receiver].work = regions[receiver].work.saturating_add(work);
    regions[receiver].delay = regions[receiver].delay.saturating_add(delay);
    regions[receiver].wiring = regions[receiver].wiring.saturating_add(wiring);
    if kind == SynthesisRegionKind::State {
        regions[receiver].kind = SynthesisRegionKind::State;
    }
    regions[receiver].anchor = regions[receiver].anchor.min(anchor);
}

fn merge_sorted_operations(left: &[word::OpId], right: &[word::OpId]) -> Vec<word::OpId> {
    let mut merged = Vec::with_capacity(left.len().saturating_add(right.len()));
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    while left_index < left.len() && right_index < right.len() {
        if left[left_index] <= right[right_index] {
            merged.push(left[left_index]);
            left_index += 1;
        } else {
            merged.push(right[right_index]);
            right_index += 1;
        }
    }
    merged.extend_from_slice(&left[left_index..]);
    merged.extend_from_slice(&right[right_index..]);
    merged
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
    let observability = crate::word::uses::netlist_observability(module)?;
    for (index, memory) in module.memories().iter().enumerate() {
        let memory_id = word::MemoryId::from_index(index).map_err(crate::SynthError::from)?;
        if !observability.observes_memory(memory_id)? {
            continue;
        }
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
        let Some(owner) = memory_owner.get(port.memory.index()).copied().flatten() else {
            continue;
        };
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
) -> Result<(Vec<TempEdge>, Vec<TempBitFlow>), crate::SynthError> {
    let mut edges = BTreeSet::new();
    let mut bit_flows = BTreeSet::new();
    let connectivity =
        ConnectivityIndex::new(module, value_keys, operation_owner, memory_signal_owner)?;
    for (index, operation) in module.operations().iter().enumerate() {
        let Some(sink) = operation_owner[index] else {
            continue;
        };
        for value in crate::word::operation_inputs(&operation.kind) {
            connectivity.append_input_edge(value, sink, &mut edges, &mut bit_flows)?;
        }
    }
    for read in module.memory_read_ports() {
        let Some(sink) = memory_owner[read.memory.index()] else {
            continue;
        };
        for value in memory_read_inputs(read) {
            connectivity.append_input_edge(value, sink, &mut edges, &mut bit_flows)?;
        }
    }
    for write in module.memory_write_ports() {
        let Some(sink) = memory_owner[write.memory.index()] else {
            continue;
        };
        for value in memory_write_inputs(write) {
            connectivity.append_input_edge(value, sink, &mut edges, &mut bit_flows)?;
        }
    }
    // Static connects are connectivity, not independent observations. Their
    // producers enter the publication table only through a real cross-region
    // dependency or a synthesis root; publishing every named intermediate
    // would retain dead substrate aliases and can make one net appear as both
    // a region input and output.
    for value in synthesis_root_values(module)? {
        connectivity.append_bit_flows(value, None, &mut bit_flows)?;
        if let Some(source) = connectivity.value_region(value)? {
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
    Ok((edges.into_iter().collect(), bit_flows.into_iter().collect()))
}

fn seal_region_identities(
    module: &word::WordModule,
    value_keys: &[[u8; 32]],
    edges: &[TempEdge],
    bit_flows: &[TempBitFlow],
    regions: &mut [TempRegion],
) -> Result<(), crate::SynthError> {
    let region_anchors = regions
        .iter()
        .map(|region| region.anchor)
        .collect::<Vec<_>>();
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
        let mut publications = bit_flows
            .iter()
            .filter(|flow| flow.source == index)
            .map(|flow| {
                (
                    value_keys[flow.value.index()],
                    flow.bit,
                    flow.sink.map(|sink| region_anchors[sink]),
                )
            })
            .collect::<Vec<_>>();
        publications.sort_unstable();
        local.update(&(publications.len() as u64).to_le_bytes());
        for (value_key, bit, consumer_anchor) in publications {
            local.update(&value_key);
            local.update(&bit.to_le_bytes());
            if let Some(anchor) = consumer_anchor {
                local.update(&[1]);
                local.update(&anchor);
            } else {
                local.update(&[0]);
            }
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
    bit_flows: Vec<TempBitFlow>,
) -> Result<SynthesisRegionGraph, crate::SynthError> {
    let graph_owner = RegionGraphOwnerId::fresh();
    let mut order = (0..regions.len()).collect::<Vec<_>>();
    order.sort_unstable_by_key(|&region| regions[region].id);
    let mut old_to_new = vec![RegionRowId::from_index(0)?; regions.len()];
    for (row, &old) in order.iter().enumerate() {
        old_to_new[old] = RegionRowId::from_index(row)?;
    }
    let operation_owners = remap_optional_owner_rows(operation_owners, &old_to_new);
    let memory_owners = remap_optional_owner_rows(memory_owners, &old_to_new);
    let mut publication_bits = vec![Vec::new(); regions.len()];
    for flow in bit_flows {
        let producer = old_to_new[flow.source];
        publication_bits[producer.index()].push(RegionBitFlow {
            producer,
            consumer: flow.sink.map(|sink| old_to_new[sink]),
            value: flow.value,
            bit: flow.bit,
        });
    }
    for row in &mut publication_bits {
        row.sort_unstable();
        row.dedup();
    }
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
    for row in &publication_bits {
        for flow in row {
            if let Some(consumer) = flow.consumer() {
                successors[flow.producer().index()].insert(consumer);
                predecessors[consumer.index()].insert(flow.producer());
            }
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
            partition_anchor: region.anchor,
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
    for row in &publication_bits {
        for publication in row {
            revision.update(&publication.producer().raw().to_le_bytes());
            revision.update(
                &publication
                    .consumer()
                    .map_or(u32::MAX, RegionRowId::raw)
                    .to_le_bytes(),
            );
            revision.update(&publication.value().raw().to_le_bytes());
            revision.update(&publication.bit().to_le_bytes());
        }
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
        bit_flows: packed_rows(publication_bits, "regional bit flows")?,
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
        word::OpKind::TriState { data, enable } => {
            digest.update(&[10, u8::from(enable.active_high)]);
            append_key(*data, keys, digest)?;
            append_key(enable.value, keys, digest)?;
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
