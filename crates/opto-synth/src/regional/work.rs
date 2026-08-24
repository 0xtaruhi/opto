// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable revision and execution rows derived from one sealed region graph.

use opto_ir::design::{
    Cell, CellClass, CellId, DesignRevisionId, EntityId, EntitySet, NetBit, NetBitId, NetDriver,
    RevisionFootprint,
};
use opto_ir::word;
use opto_runtime::{Task, TaskKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

const WORK_TASK_DOMAIN: u32 = 0x574f_524b;
const COARSE_GROUP_SHARDS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Delta-local cell role retained only while validating Word publication.
pub(crate) enum WordFragmentCell {
    /// Operation appended by the private Word fragment.
    Operation,
    /// Connection from the fragment result to its stable published signal bit.
    Connection,
}

#[derive(Default)]
struct FragmentNetTable {
    nets: Vec<NetBit>,
    rows: HashMap<NetBitId, usize>,
}

impl FragmentNetTable {
    fn into_nets(self) -> Vec<NetBit> {
        self.nets
    }
}

struct LogicalAnchors {
    operations: Box<[CellId]>,
    signals: Box<[[u8; 32]]>,
}

impl LogicalAnchors {
    fn new(
        module: &word::WordModule,
        regions: &crate::SynthesisRegionGraph,
    ) -> Result<Self, crate::SynthError> {
        let operations = (0..module.operations().len())
            .map(|index| {
                let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
                operation_cell_id(regions, operation)
            })
            .collect::<Result<_, _>>()?;
        let signals = (0..module.signals().len())
            .map(|index| {
                let signal = word::SignalId::from_index(index).map_err(crate::SynthError::from)?;
                signal_anchor(module, signal)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            operations,
            signals,
        })
    }

    fn operation(&self, operation: word::OpId) -> Result<CellId, crate::SynthError> {
        self.operations
            .get(operation.index())
            .copied()
            .ok_or_else(|| crate::SynthError::invariant("logical operation anchor is out of range"))
    }

    fn signal(&self, signal: word::SignalId) -> Result<[u8; 32], crate::SynthError> {
        self.signals
            .get(signal.index())
            .copied()
            .ok_or_else(|| crate::SynthError::invariant("logical signal anchor is out of range"))
    }
}

struct MemoryPortIndex {
    reads: Vec<Vec<usize>>,
    writes: Vec<Vec<usize>>,
}

impl MemoryPortIndex {
    fn new(module: &word::WordModule) -> Self {
        let mut index = Self {
            reads: vec![Vec::new(); module.memories().len()],
            writes: vec![Vec::new(); module.memories().len()],
        };
        for (row, port) in module.memory_read_ports().iter().enumerate() {
            index.reads[port.memory.index()].push(row);
        }
        for (row, port) in module.memory_write_ports().iter().enumerate() {
            index.writes[port.memory.index()].push(row);
        }
        index
    }
}

macro_rules! digest_id {
    ($name:ident, $doc:literal) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[doc = $doc]
        pub(crate) struct $name([u8; 32]);

        impl $name {
            const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }
    };
}

digest_id!(
    WorkItemId,
    "Stable identity of one revision-local semantic task."
);
digest_id!(
    CompilationShardId,
    "Epoch-local identity of one scheduled batch."
);
digest_id!(
    WorkContextKey,
    "Versioned identity of one work item's complete analysis context."
);

impl WorkContextKey {
    const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl From<crate::RegionContextKey> for WorkContextKey {
    fn from(context: crate::RegionContextKey) -> Self {
        Self::from_bytes(context.bytes())
    }
}

#[derive(Debug, Clone)]
/// Canonical immutable macro design consumed by every regional work epoch.
pub(crate) struct WorkDesign {
    revision: DesignRevisionId,
    state_cells: Box<[CellId]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum WorkEntityKind {
    Cell(CellId),
    OperationNet(CellId),
    SignalNet([u8; 32]),
    ConstantNet { value: u8, state: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct WorkEntitySpan {
    kind: WorkEntityKind,
    lsb: u32,
    width: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct WorkEntityRange {
    lsb: u32,
    width: u32,
}

impl WorkEntityRange {
    fn end(self) -> Result<u32, crate::SynthError> {
        self.lsb
            .checked_add(self.width)
            .ok_or_else(|| crate::SynthError::capacity("work-entity span"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkEntityGroup {
    kind: WorkEntityKind,
    ranges: Box<[WorkEntityRange]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct WorkEntitySet(Box<[WorkEntityGroup]>);

#[derive(Default)]
struct WorkEntitySetBuilder(BTreeMap<WorkEntityKind, Vec<WorkEntityRange>>);

impl WorkEntitySetBuilder {
    fn push(&mut self, span: WorkEntitySpan) -> Result<(), crate::SynthError> {
        if span.width == 0 {
            return Err(crate::SynthError::invariant(
                "work-entity span has zero width",
            ));
        }
        let range = WorkEntityRange {
            lsb: span.lsb,
            width: span.width,
        };
        range.end()?;
        let ranges = self.0.entry(span.kind).or_default();
        if let Some(previous) = ranges.last_mut()
            && previous.end()? == range.lsb
        {
            previous.width = previous
                .width
                .checked_add(range.width)
                .ok_or_else(|| crate::SynthError::capacity("work-entity span"))?;
        } else {
            ranges.push(range);
        }
        Ok(())
    }

    fn finish(self) -> Result<WorkEntitySet, crate::SynthError> {
        let mut groups = Vec::with_capacity(self.0.len());
        for (kind, mut ranges) in self.0 {
            ranges.sort_unstable();
            let mut merged = Vec::<WorkEntityRange>::with_capacity(ranges.len());
            for range in ranges {
                let range_end = range.end()?;
                if let Some(previous) = merged.last_mut() {
                    let previous_end = previous.end()?;
                    if range.lsb <= previous_end {
                        previous.width = previous_end.max(range_end) - previous.lsb;
                        continue;
                    }
                }
                merged.push(range);
            }
            groups.push(WorkEntityGroup {
                kind,
                ranges: merged.into_boxed_slice(),
            });
        }
        Ok(WorkEntitySet(groups.into_boxed_slice()))
    }
}

impl WorkEntitySet {
    fn new(spans: Vec<WorkEntitySpan>) -> Result<Self, crate::SynthError> {
        let mut builder = WorkEntitySetBuilder::default();
        for span in spans {
            builder.push(span)?;
        }
        builder.finish()
    }

    fn spans(&self) -> impl Iterator<Item = WorkEntitySpan> + '_ {
        self.0.iter().flat_map(|group| {
            group.ranges.iter().map(|range| WorkEntitySpan {
                kind: group.kind,
                lsb: range.lsb,
                width: range.width,
            })
        })
    }

    fn union(&self, other: &Self) -> Result<Self, crate::SynthError> {
        Self::new(self.spans().chain(other.spans()).collect())
    }

    fn difference(&self, other: &Self) -> Result<Self, crate::SynthError> {
        let mut builder = WorkEntitySetBuilder::default();
        for group in &self.0 {
            let blockers = other
                .0
                .binary_search_by_key(&group.kind, |candidate| candidate.kind)
                .ok()
                .map(|index| other.0[index].ranges.as_ref())
                .unwrap_or_default();
            let mut first_candidate = 0usize;
            for range in &group.ranges {
                let range_end = range.end()?;
                while let Some(blocker) = blockers.get(first_candidate) {
                    if blocker.end()? > range.lsb {
                        break;
                    }
                    first_candidate += 1;
                }
                let mut cursor = range.lsb;
                for blocker in &blockers[first_candidate..] {
                    if blocker.lsb >= range_end {
                        break;
                    }
                    if blocker.lsb > cursor {
                        builder.push(WorkEntitySpan {
                            kind: group.kind,
                            lsb: cursor,
                            width: blocker.lsb - cursor,
                        })?;
                    }
                    cursor = cursor.max(blocker.end()?);
                    if cursor >= range_end {
                        break;
                    }
                }
                if cursor < range_end {
                    builder.push(WorkEntitySpan {
                        kind: group.kind,
                        lsb: cursor,
                        width: range_end - cursor,
                    })?;
                }
            }
        }
        builder.finish()
    }

    fn cardinality(&self) -> u64 {
        self.0
            .iter()
            .flat_map(|group| group.ranges.iter())
            .map(|range| u64::from(range.width))
            .sum()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    fn contains_cell(&self, cell: CellId) -> bool {
        self.0
            .binary_search_by_key(&WorkEntityKind::Cell(cell), |group| group.kind)
            .is_ok()
    }
}

#[derive(Debug)]
pub(crate) struct WorkItem {
    id: WorkItemId,
    kind: WorkItemKind,
    core: WorkEntitySet,
    halo: WorkEntitySet,
    context: WorkContextKey,
    estimated_work: u64,
    estimated_memory: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum WorkItemKind {
    FixedLogic(crate::RegionAnchorId),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkContext {
    key: WorkContextKey,
    design: DesignRevisionId,
    scenarios: opto_timing::ScenarioGeneration,
    target: [u8; 32],
    boundaries: Box<[WorkBoundaryContext]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkBoundaryContext {
    semantic_key: [u8; 32],
    input: bool,
    scenarios: opto_timing::ScenarioGeneration,
    generation: [u8; 32],
    rows: Box<[crate::BoundaryContractRow]>,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkPacketItem {
    id: WorkItemId,
    kind: WorkItemKind,
    core: WorkEntitySet,
    halo: WorkEntitySet,
    context: WorkContext,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkPacket {
    schema: u32,
    design: DesignRevisionId,
    shard: CompilationShardId,
    items: Box<[WorkPacketItem]>,
    estimated_work: u64,
    estimated_memory: u64,
}

pub(crate) struct WorkProduct<T> {
    pub(crate) proof: opto_ir::design::EquivalenceCertificate,
    pub(crate) output: T,
}

impl<T> WorkProduct<T> {
    pub(crate) const fn compiled_artifact(
        proof: opto_ir::design::EquivalenceCertificate,
        output: T,
    ) -> Self {
        Self { proof, output }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CompiledWorkArtifact<T> {
    footprint: RevisionFootprint<WorkEntitySet>,
    proof: opto_ir::design::EquivalenceCertificate,
    output: T,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkResult<T> {
    item: WorkItemId,
    shard: CompilationShardId,
    context: WorkContextKey,
    artifact: CompiledWorkArtifact<T>,
}

pub(crate) trait SynthesisExecutor {
    fn execute<T, F>(
        &self,
        work: &WorkGraph,
        operation: F,
    ) -> Result<Vec<WorkResult<T>>, crate::SynthError>
    where
        T: Send,
        F: Fn(
                &WorkItem,
                &opto_runtime::ExecutionContext,
            ) -> Result<WorkProduct<T>, crate::SynthError>
            + Send
            + Sync;
}

impl WorkContext {
    pub(crate) fn logical(
        key: WorkContextKey,
        design: DesignRevisionId,
        scenarios: opto_timing::ScenarioGeneration,
        target: [u8; 32],
        contracts: &[crate::BoundaryContract],
    ) -> Self {
        Self {
            key,
            design,
            scenarios,
            target,
            boundaries: contracts
                .iter()
                .map(|contract| WorkBoundaryContext {
                    semantic_key: contract.port().semantic_key(),
                    input: contract.port().direction() == crate::RegionPortDirection::Input,
                    scenarios: contract.scenario_generation(),
                    generation: contract.generation().bytes(),
                    rows: contract.rows().into(),
                })
                .collect(),
        }
    }
}

impl WorkItem {
    pub(crate) const fn fixed_logic(&self) -> crate::RegionAnchorId {
        match self.kind {
            WorkItemKind::FixedLogic(region) => region,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CompilationShard {
    id: CompilationShardId,
    items: Box<[usize]>,
    estimated_work: u64,
    estimated_memory: u64,
}

#[derive(Debug)]
pub(crate) struct WorkGraph {
    design: Arc<WorkDesign>,
    regions: Arc<crate::SynthesisRegionGraph>,
    contexts: Box<[WorkContext]>,
    items: Box<[WorkItem]>,
    item_rows: BTreeMap<WorkItemId, usize>,
    shards: Box<[CompilationShard]>,
    coarse_groups: opto_core::PackedRows<CompilationShardId>,
    predecessors: opto_core::PackedRows<WorkItemId>,
    successors: opto_core::PackedRows<WorkItemId>,
}

impl WorkGraph {
    pub(crate) fn build(
        module: &word::WordModule,
        regions: Arc<crate::SynthesisRegionGraph>,
        design: Arc<WorkDesign>,
        contexts: Box<[WorkContext]>,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        if contexts.len() != regions.regions().len() {
            return Err(crate::SynthError::invariant(
                "work contexts do not cover the sealed region graph",
            ));
        }
        let anchors = LogicalAnchors::new(module, regions.as_ref())?;
        let connectivity = crate::word::bit_connectivity::BitConnectivity::new(module)?;
        let rows = runtime.analyze_indexed(regions.regions().len(), |index| {
            let region = regions.regions()[index];
            let (core, halo) = region_entities(
                module,
                &anchors,
                regions.as_ref(),
                &connectivity,
                region,
            )?;
            if core.is_empty() {
                return Err(crate::SynthError::invariant(format!(
                    "region {} has no writable logical entity (operations={}, memories={}, outputs={}, publications={})",
                    region.row().raw(),
                    regions.operations(region).len(),
                    regions.memories(region).len(),
                    regions.output_ports(region).len(),
                    regions.bit_flows(region).len(),
                )));
            }
            let id = work_item_id(design.revision, region.id());
            let estimated_memory = core
                .cardinality()
                .saturating_add(halo.cardinality())
                .max(1);
            let item = WorkItem {
                id,
                kind: WorkItemKind::FixedLogic(region.id()),
                core,
                halo,
                context: contexts[region.row().index()].key,
                estimated_work: region.estimated_work().max(1),
                estimated_memory,
            };
            Ok::<_, crate::SynthError>((id, item))
        })?;
        let items = rows.into_iter().map(|row| row.1).collect::<Vec<_>>();
        let item_rows = items
            .iter()
            .enumerate()
            .map(|(row, item)| (item.id, row))
            .collect();
        let dependency_rows = |predecessors: bool| -> Result<Vec<Vec<_>>, crate::SynthError> {
            runtime.analyze_indexed(items.len(), |index| {
                let region = regions.regions()[index];
                let adjacent = if predecessors {
                    regions.predecessors(region)
                } else {
                    regions.successors(region)
                };
                Ok(adjacent.iter().map(|&row| items[row.index()].id).collect())
            })
        };
        let predecessors = opto_core::PackedRows::try_from_rows(dependency_rows(true)?)
            .map_err(|_| crate::SynthError::capacity("work-item predecessors"))?;
        let successors = opto_core::PackedRows::try_from_rows(dependency_rows(false)?)
            .map_err(|_| crate::SynthError::capacity("work-item successors"))?;
        let mut graph = Self {
            design,
            regions,
            contexts,
            items: items.into_boxed_slice(),
            item_rows,
            shards: Box::new([]),
            coarse_groups: opto_core::PackedRows::try_from_rows(Vec::<Vec<_>>::new())
                .map_err(|_| crate::SynthError::capacity("coarse compilation groups"))?,
            predecessors,
            successors,
        };
        graph.rebatch(1)?;
        graph.validate()?;
        Ok(graph)
    }

    /// Deterministically changes only scheduler batching, never semantic items.
    pub(crate) fn rebatch(&mut self, maximum_items: usize) -> Result<(), crate::SynthError> {
        let maximum_items = maximum_items.max(1);
        let shards = (0..self.items.len())
            .collect::<Vec<_>>()
            .chunks(maximum_items)
            .map(|indices| {
                let estimated_work = indices.iter().fold(0_u64, |total, &index| {
                    total.saturating_add(self.items[index].estimated_work)
                });
                let estimated_memory = indices.iter().fold(0_u64, |total, &index| {
                    total.saturating_add(self.items[index].estimated_memory)
                });
                CompilationShard {
                    id: shard_id(self.design.revision, indices, &self.items),
                    items: indices.into(),
                    estimated_work,
                    estimated_memory,
                }
            })
            .collect::<Vec<_>>();
        self.coarse_groups = opto_core::PackedRows::try_from_rows(
            shards
                .chunks(COARSE_GROUP_SHARDS)
                .map(|group| group.iter().map(|shard| shard.id).collect())
                .collect(),
        )
        .map_err(|_| crate::SynthError::capacity("coarse compilation groups"))?;
        self.shards = shards.into_boxed_slice();
        self.validate()
    }

    pub(crate) fn rebatch_for_workers(&mut self, workers: usize) -> Result<(), crate::SynthError> {
        let target_shards = workers.max(1).saturating_mul(8);
        self.rebatch(self.items.len().div_ceil(target_shards).max(1))
    }

    fn local_tasks(&self) -> Vec<Task<usize>> {
        self.shards
            .iter()
            .enumerate()
            .map(|(ordinal, shard)| {
                Task::new(TaskKey::new(WORK_TASK_DOMAIN, ordinal as u64), ordinal)
                    .with_estimated_work(shard.estimated_work)
                    .with_estimated_memory(shard.estimated_memory)
            })
            .collect()
    }

    #[cfg(test)]
    fn portable_packets(&self) -> Vec<WorkPacket> {
        self.shards
            .iter()
            .map(|shard| {
                let items: Box<[WorkPacketItem]> = shard
                    .items
                    .iter()
                    .map(|&row| WorkPacketItem {
                        id: self.items[row].id,
                        kind: self.items[row].kind,
                        core: self.items[row].core.clone(),
                        halo: self.items[row].halo.clone(),
                        context: self.contexts[row].clone(),
                    })
                    .collect();
                WorkPacket {
                    schema: 1,
                    design: self.design.revision,
                    shard: shard.id,
                    items,
                    estimated_work: shard.estimated_work,
                    estimated_memory: shard.estimated_memory,
                }
            })
            .collect()
    }

    pub(crate) fn regions(&self) -> &crate::SynthesisRegionGraph {
        &self.regions
    }

    pub(crate) fn state_cells(&self) -> impl Iterator<Item = CellId> + '_ {
        self.design.state_cells.iter().copied()
    }

    pub(crate) fn accept_results<T>(
        &self,
        results: Vec<WorkResult<T>>,
        expected_proof: impl Fn(
            &WorkItem,
            &T,
        )
            -> Result<opto_ir::design::EquivalenceCertificate, crate::SynthError>,
    ) -> Result<Box<[WorkProduct<T>]>, crate::SynthError> {
        if results.len() != self.items.len() {
            return Err(crate::SynthError::invariant(
                "work execution did not return exactly one result per item",
            ));
        }
        let mut outputs = std::iter::repeat_with(|| None)
            .take(self.items.len())
            .collect::<Vec<_>>();
        for result in results {
            let row = self.item_rows.get(&result.item).copied().ok_or_else(|| {
                crate::SynthError::invariant("work result references an unknown item")
            })?;
            let item = &self.items[row];
            let reads = item_read_set(item)?;
            let shard = self.shard_for_item(row).ok_or_else(|| {
                crate::SynthError::invariant("work result item has no compilation shard")
            })?;
            if result.shard != shard || result.context != item.context {
                return Err(crate::SynthError::invariant(
                    "work result does not match its immutable revision, context, or footprint",
                ));
            }
            if result.artifact.footprint.base != self.design.revision
                || result.artifact.footprint.reads != reads
                || result.artifact.footprint.replaces != item.core
            {
                return Err(crate::SynthError::invariant(
                    "compiled work artifact does not match its exact task footprint",
                ));
            }
            let product = WorkProduct {
                proof: result.artifact.proof,
                output: result.artifact.output,
            };
            if product.proof != expected_proof(item, &product.output)? {
                return Err(crate::SynthError::invariant(
                    "work product proof does not match its accepted output",
                ));
            }
            if outputs[row].replace(product).is_some() {
                return Err(crate::SynthError::invariant(
                    "work item produced more than one result",
                ));
            }
        }
        outputs
            .into_iter()
            .map(|output| {
                output.ok_or_else(|| crate::SynthError::invariant("work item produced no result"))
            })
            .collect()
    }

    fn shard_for_item(&self, row: usize) -> Option<CompilationShardId> {
        let index = self
            .shards
            .partition_point(|shard| shard.items.last().is_some_and(|&last| last < row));
        self.shards
            .get(index)
            .filter(|shard| shard.items.binary_search(&row).is_ok())
            .map(|shard| shard.id)
    }

    fn validate(&self) -> Result<(), crate::SynthError> {
        if self.predecessors.row_count() != self.items.len()
            || self.successors.row_count() != self.items.len()
            || self.contexts.len() != self.items.len()
            || self.item_rows.len() != self.items.len()
            || self.coarse_groups.value_count() != self.shards.len()
        {
            return Err(crate::SynthError::invariant(
                "work shards do not match their stable semantic items",
            ));
        }
        let revision = self.design.revision;
        if self
            .contexts
            .iter()
            .zip(&self.items)
            .any(|(context, item)| {
                context.key != item.context
                    || context.design != revision
                    || context
                        .boundaries
                        .iter()
                        .any(|boundary| boundary.scenarios != context.scenarios)
            })
            || self.contexts.windows(2).any(|pair| {
                pair[0].scenarios != pair[1].scenarios || pair[0].target != pair[1].target
            })
        {
            return Err(crate::SynthError::invariant(
                "work context does not match its design or scenario generation",
            ));
        }
        if let Some((row, _)) = self.items.iter().enumerate().find(|(_, item)| {
            item.core.is_empty()
                || item.estimated_memory
                    < item
                        .core
                        .cardinality()
                        .saturating_add(item.halo.cardinality())
        }) {
            return Err(crate::SynthError::invariant(format!(
                "work item {row} has an invalid core, halo, or memory estimate"
            )));
        }
        let scheduled = self
            .shards
            .iter()
            .flat_map(|shard| shard.items.iter().copied())
            .collect::<Vec<_>>();
        if scheduled != (0..self.items.len()).collect::<Vec<_>>()
            || self.shards.iter().any(|shard| {
                shard.items.is_empty()
                    || shard.id != shard_id(self.design.revision, &shard.items, &self.items)
                    || shard.estimated_work
                        != shard.items.iter().fold(0_u64, |total, &item| {
                            total.saturating_add(self.items[item].estimated_work)
                        })
                    || shard.estimated_memory
                        != shard.items.iter().fold(0_u64, |total, &item| {
                            total.saturating_add(self.items[item].estimated_memory)
                        })
            })
        {
            return Err(crate::SynthError::invariant(
                "compilation shards do not form an exact ordered batching of work items",
            ));
        }
        let ids = self
            .items
            .iter()
            .map(|item| item.id)
            .collect::<std::collections::BTreeSet<_>>();
        if self
            .items
            .iter()
            .enumerate()
            .any(|(row, item)| self.item_rows.get(&item.id) != Some(&row))
        {
            return Err(crate::SynthError::invariant(
                "work-item directory does not match its stable identities",
            ));
        }
        if (0..self.items.len()).any(|row| {
            self.predecessors[row]
                .iter()
                .chain(&self.successors[row])
                .any(|id| !ids.contains(id))
        }) {
            return Err(crate::SynthError::invariant(
                "work-item dependency references an unknown item",
            ));
        }
        Ok(())
    }
}

impl SynthesisExecutor for opto_runtime::ExecutionContext {
    fn execute<T, F>(
        &self,
        work: &WorkGraph,
        operation: F,
    ) -> Result<Vec<WorkResult<T>>, crate::SynthError>
    where
        T: Send,
        F: Fn(
                &WorkItem,
                &opto_runtime::ExecutionContext,
            ) -> Result<WorkProduct<T>, crate::SynthError>
            + Send
            + Sync,
    {
        self.map_ordered_composite(work.local_tasks(), |shard, runtime| {
            let shard = &work.shards[shard];
            shard
                .items
                .iter()
                .map(|&row| {
                    let item = &work.items[row];
                    let product = operation(item, runtime)?;
                    let artifact = CompiledWorkArtifact {
                        footprint: RevisionFootprint {
                            base: work.design.revision,
                            reads: entity_union(&item.core, &item.halo)?,
                            replaces: item.core.clone(),
                        },
                        proof: product.proof,
                        output: product.output,
                    };
                    Ok(WorkResult {
                        item: item.id,
                        shard: shard.id,
                        context: item.context,
                        artifact,
                    })
                })
                .collect::<Result<Vec<_>, crate::SynthError>>()
        })
        .map(|rows| rows.into_iter().flatten().collect())
    }
}

fn item_read_set(item: &WorkItem) -> Result<WorkEntitySet, crate::SynthError> {
    entity_union(&item.core, &item.halo)
}

fn entity_union(
    left: &WorkEntitySet,
    right: &WorkEntitySet,
) -> Result<WorkEntitySet, crate::SynthError> {
    left.union(right)
}

impl WorkDesign {
    pub(crate) fn revision_of(
        module: &word::WordModule,
    ) -> Result<DesignRevisionId, crate::SynthError> {
        logical_revision_id(module)
    }

    pub(crate) fn seal(
        module: &word::WordModule,
        regions: &crate::SynthesisRegionGraph,
    ) -> Result<Self, crate::SynthError> {
        let anchors = LogicalAnchors::new(module, regions)?;
        let state_cells = regions
            .regions()
            .iter()
            .flat_map(|&region| regions.operations(region))
            .filter(|&&operation| {
                module.operation(operation).is_some_and(|operation| {
                    matches!(
                        operation.kind,
                        word::OpKind::Register(_) | word::OpKind::Latch(_)
                    )
                })
            })
            .map(|&operation| anchors.operation(operation))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            revision: logical_revision_id(module)?,
            state_cells: state_cells.into_boxed_slice(),
        })
    }

    pub(crate) const fn revision(&self) -> DesignRevisionId {
        self.revision
    }
}

/// Lowers published static-wire coalescing fragments into bit-level revision
/// deltas against the sealed base generation.
///
/// Every fragment replaces the published bits of one candidate wire and
/// describes only the delta-local operation/connection topology needed to
/// validate that replacement. Stable identities reuse the sealing recipes over
/// the spliced module, so a fresh seal reproduces exactly these entities
/// without retaining a second whole-design topology.
pub(crate) fn coalesce_revision_deltas(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    base: DesignRevisionId,
    published: &word::PublishedWave,
    signals: &[(word::FragmentKey, word::SignalId)],
) -> Result<Vec<opto_ir::design::RewriteDelta<WordFragmentCell>>, crate::SynthError> {
    use opto_ir::design::{
        EquivalenceCertificate, EquivalenceRegime, RewriteDelta, RewriteDeltaId, SemanticBinding,
    };

    if published.entries().len() != signals.len() {
        return Err(crate::SynthError::invariant(
            "published coalescing wave does not match its candidate wires",
        ));
    }
    let anchors = LogicalAnchors::new(module, regions)?;
    let connectivity = crate::word::bit_connectivity::BitConnectivity::new(module)?;
    let mut deltas = Vec::with_capacity(published.entries().len());
    for (entry, &(_, signal)) in published.entries().iter().zip(signals) {
        let stored_signal = module
            .signal(signal)
            .ok_or_else(|| crate::SynthError::invariant("coalesced candidate wire disappeared"))?;
        let width = stored_signal.ty.width();
        let state = stored_signal.ty.state();
        let signal_anchor = signal_anchor(module, signal)?;
        let mut nets = FragmentNetTable::default();
        let mut connection_cells = Vec::<Cell<WordFragmentCell>>::new();
        let mut replaces = Vec::<EntityId>::new();
        let mut outputs = Vec::with_capacity(width as usize);
        for bit in 0..width {
            let Some(source) = connectivity.signal_source(signal, bit)? else {
                return Err(crate::SynthError::invariant(format!(
                    "coalesced wire lost the driver for bit {bit}"
                )));
            };
            let input_net = source_net(module, &anchors, source, state, &mut nets)?;
            let cell = connection_cell_id(signal_anchor, bit);
            let output = signal_net_id(signal_anchor, bit);
            install_net(&mut nets, output, state, None)?;
            install_driver(&mut nets, output, NetDriver::Cell { cell, output: 0 })?;
            replaces.push(EntityId::NetBit(output));
            outputs.push(output);
            connection_cells.push(Cell {
                id: cell,
                kind: WordFragmentCell::Connection,
                class: CellClass::Combinational,
                inputs: Box::new([input_net]),
                outputs: Box::new([output]),
                source: stored_signal.source.clone(),
            });
        }
        let mut operation_cells =
            Vec::<Cell<WordFragmentCell>>::with_capacity(entry.operations().len());
        for &operation in entry.operations() {
            let stored = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("published coalescing operation disappeared")
            })?;
            let cell = anchors.operation(operation)?;
            let inputs = crate::word::operation_inputs(&stored.kind)
                .into_iter()
                .map(|value| {
                    materialize_value_nets(module, &anchors, &connectivity, value, &mut nets)
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Box<[_]>>();
            let outputs = operation_outputs(module, &anchors, operation)?;
            for (output, &net) in outputs.iter().enumerate() {
                let state = module
                    .value(stored.result)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "published coalescing result value disappeared",
                        )
                    })?
                    .ty
                    .state();
                install_net(&mut nets, net, state, None)?;
                install_driver(
                    &mut nets,
                    net,
                    NetDriver::Cell {
                        cell,
                        output: u32::try_from(output).map_err(|_| {
                            crate::SynthError::capacity("logical output bit ordinal")
                        })?,
                    },
                )?;
            }
            operation_cells.push(Cell {
                id: cell,
                kind: WordFragmentCell::Operation,
                class: if matches!(
                    stored.kind,
                    word::OpKind::Register(_) | word::OpKind::Latch(_)
                ) {
                    CellClass::StateBoundary
                } else {
                    CellClass::Combinational
                },
                inputs,
                outputs,
                source: stored.source.clone(),
            });
        }
        let nets = nets.into_nets();
        let mut cells = operation_cells;
        cells.extend(connection_cells);
        let net_rows = nets
            .iter()
            .enumerate()
            .map(|(row, net)| (net.id, row))
            .collect::<BTreeMap<_, _>>();
        let mut existing_inputs = BTreeSet::new();
        for &input in cells.iter().flat_map(|cell| cell.inputs.iter()) {
            let net = net_rows
                .get(&input)
                .and_then(|&row| nets.get(row))
                .ok_or_else(|| crate::SynthError::invariant("fragment input net is absent"))?;
            if net.driver.is_none() {
                existing_inputs.insert(input);
            }
        }
        let mut reads = replaces.clone();
        reads.extend(existing_inputs.iter().copied().map(EntityId::NetBit));
        deltas.push(RewriteDelta {
            id: RewriteDeltaId::from_bytes(entry.key().bytes()),
            footprint: RevisionFootprint {
                base,
                reads: EntitySet::new(reads).map_err(|error| design_error(&error))?,
                replaces: EntitySet::new(replaces).map_err(|error| design_error(&error))?,
            },
            cells: cells.into_boxed_slice(),
            nets: nets.into_boxed_slice(),
            semantic: SemanticBinding {
                inputs: existing_inputs.into_iter().collect::<Box<[_]>>(),
                outputs: outputs.into_boxed_slice(),
            },
            proof: EquivalenceCertificate {
                regime: EquivalenceRegime::ByConstruction,
                digest: coalesce_proof_digest(entry.key()),
            },
        });
    }
    Ok(deltas)
}

/// Rejects any coalescing certificate that does not carry this layer's own
/// construction-equivalence recipe.
pub(crate) fn validate_coalesce_proof(
    delta: &opto_ir::design::RewriteDelta<WordFragmentCell>,
) -> Result<(), opto_ir::design::DesignError> {
    use opto_ir::design::{DesignError, EquivalenceRegime};
    if delta.proof.regime != EquivalenceRegime::ByConstruction
        || delta.proof.digest
            != coalesce_proof_digest(opto_ir::word::FragmentKey::from_bytes(delta.id.bytes()))
    {
        return Err(DesignError::ProofRejected(
            "static-wire coalescing requires its own by-construction certificate".to_owned(),
        ));
    }
    Ok(())
}

fn validate_coalesce_fragment(
    delta: &opto_ir::design::RewriteDelta<WordFragmentCell>,
) -> Result<(), crate::SynthError> {
    let mut cells = BTreeMap::new();
    for cell in &delta.cells {
        if cells.insert(cell.id, cell).is_some() {
            return Err(crate::SynthError::invariant(
                "coalescing fragment repeats a cell identity",
            ));
        }
    }
    let mut nets = BTreeMap::new();
    for net in &delta.nets {
        if nets.insert(net.id, net).is_some() {
            return Err(crate::SynthError::invariant(
                "coalescing fragment repeats a net identity",
            ));
        }
    }

    let mut boundary_inputs = BTreeSet::new();
    for cell in &delta.cells {
        for &input in &cell.inputs {
            let net = nets.get(&input).ok_or_else(|| {
                crate::SynthError::invariant("coalescing cell input is absent from its fragment")
            })?;
            if net.driver.is_none() {
                boundary_inputs.insert(input);
            }
        }
        for (output, &net_id) in cell.outputs.iter().enumerate() {
            let output = u32::try_from(output)
                .map_err(|_| crate::SynthError::capacity("coalescing output ordinal"))?;
            if nets.get(&net_id).and_then(|net| net.driver)
                != Some(NetDriver::Cell {
                    cell: cell.id,
                    output,
                })
            {
                return Err(crate::SynthError::invariant(
                    "coalescing cell output has an inconsistent driver",
                ));
            }
        }
    }
    for net in &delta.nets {
        let Some(NetDriver::Cell { cell, output }) = net.driver else {
            continue;
        };
        if cells
            .get(&cell)
            .and_then(|cell| cell.outputs.get(output as usize))
            .copied()
            != Some(net.id)
        {
            return Err(crate::SynthError::invariant(
                "coalescing net driver is absent from its fragment",
            ));
        }
    }

    if !boundary_inputs
        .iter()
        .copied()
        .eq(delta.semantic.inputs.iter().copied())
    {
        return Err(crate::SynthError::invariant(
            "coalescing fragment input boundary is not exact",
        ));
    }
    let boundary_outputs = EntitySet::new(
        delta
            .semantic
            .outputs
            .iter()
            .copied()
            .map(EntityId::NetBit)
            .collect(),
    )
    .map_err(|error| design_error(&error))?;
    if boundary_outputs != delta.footprint.replaces {
        return Err(crate::SynthError::invariant(
            "coalescing fragment output boundary is not its exact replacement footprint",
        ));
    }
    let expected_reads = EntitySet::new(
        boundary_inputs
            .into_iter()
            .map(EntityId::NetBit)
            .chain(boundary_outputs.as_slice().iter().copied())
            .collect(),
    )
    .map_err(|error| design_error(&error))?;
    if expected_reads != delta.footprint.reads {
        return Err(crate::SynthError::invariant(
            "coalescing fragment read footprint is not its exact boundary closure",
        ));
    }
    Ok(())
}

pub(crate) fn commit_coalescing_revision(
    base: DesignRevisionId,
    next: WorkDesign,
    deltas: &[opto_ir::design::RewriteDelta<WordFragmentCell>],
) -> Result<WorkDesign, crate::SynthError> {
    let mut ids = std::collections::BTreeSet::new();
    let mut replacements = std::collections::BTreeSet::new();
    for delta in deltas {
        if delta.footprint.base != base || !ids.insert(delta.id) {
            return Err(crate::SynthError::invariant(
                "coalescing wave does not target one unique base revision",
            ));
        }
        validate_coalesce_proof(delta).map_err(|error| design_error(&error))?;
        validate_coalesce_fragment(delta)?;
        for &entity in delta.footprint.replaces.as_slice() {
            if !replacements.insert(entity) {
                return Err(crate::SynthError::invariant(
                    "coalescing wave has overlapping replacement footprints",
                ));
            }
        }
    }
    Ok(next)
}

fn coalesce_proof_digest(key: opto_ir::word::FragmentKey) -> [u8; 32] {
    *blake3::Hasher::new()
        .update(b"opto/dataflow-coalesce-proof/v1\0")
        .update(&key.bytes())
        .finalize()
        .as_bytes()
}

fn operation_outputs(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    operation: word::OpId,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    let operation = module
        .operation(operation)
        .ok_or_else(|| crate::SynthError::invariant("logical operation is unknown"))?;
    let result = module
        .value(operation.result)
        .ok_or_else(|| crate::SynthError::invariant("logical operation result is unknown"))?;
    let cell = match result.kind {
        word::ValueKind::Operation(operation) => operation,
        word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => {
            return Err(crate::SynthError::invariant(
                "logical operation result lost its operation identity",
            ));
        }
    };
    (0..result.ty.width())
        .map(|bit| Ok(operation_net_id(anchors.operation(cell)?, bit)))
        .collect()
}

fn region_entities(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    regions: &crate::SynthesisRegionGraph,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    region: crate::SynthesisRegion,
) -> Result<(WorkEntitySet, WorkEntitySet), crate::SynthError> {
    let mut core = WorkEntitySetBuilder::default();
    let mut halo = WorkEntitySetBuilder::default();
    for &operation in regions.operations(region) {
        let stored = module
            .operation(operation)
            .ok_or_else(|| crate::SynthError::invariant("work item operation is unknown"))?;
        let cell = anchors.operation(operation)?;
        core.push(WorkEntitySpan {
            kind: WorkEntityKind::Cell(cell),
            lsb: 0,
            width: 1,
        })?;
        let result = module
            .value(stored.result)
            .ok_or_else(|| crate::SynthError::invariant("work item operation result is unknown"))?;
        core.push(WorkEntitySpan {
            kind: WorkEntityKind::OperationNet(cell),
            lsb: 0,
            width: result.ty.width(),
        })?;
        for value in crate::word::operation_inputs(&stored.kind) {
            append_value_spans(&mut halo, module, anchors, connectivity, value, 0, None)?;
        }
    }
    let memory_ports = MemoryPortIndex::new(module);
    for &memory in regions.memories(region) {
        core.push(WorkEntitySpan {
            kind: WorkEntityKind::Cell(logical_memory_cell_id(module, memory)?),
            lsb: 0,
            width: 1,
        })?;
        append_memory_spans(
            &mut halo,
            &mut core,
            module,
            anchors,
            connectivity,
            &memory_ports,
            memory,
        )?;
    }
    for &port in regions.input_ports(region) {
        let value = regions
            .port(port)
            .ok_or_else(|| crate::SynthError::invariant("work item input port is unknown"))?
            .value();
        append_value_spans(&mut halo, module, anchors, connectivity, value, 0, None)?;
    }
    for &port in regions.output_ports(region) {
        let value = regions
            .port(port)
            .ok_or_else(|| crate::SynthError::invariant("work item output port is unknown"))?
            .value();
        append_value_spans(&mut core, module, anchors, connectivity, value, 0, None)?;
    }
    for flow in regions.bit_flows(region) {
        append_value_spans(
            &mut core,
            module,
            anchors,
            connectivity,
            flow.value(),
            flow.lsb(),
            Some(flow.width()),
        )?;
    }
    let core = core.finish()?;
    let halo = halo.finish()?.difference(&core)?;
    Ok((core, halo))
}

fn append_memory_spans(
    inputs: &mut WorkEntitySetBuilder,
    outputs: &mut WorkEntitySetBuilder,
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    ports: &MemoryPortIndex,
    memory: word::MemoryId,
) -> Result<(), crate::SynthError> {
    let reads = ports.reads.get(memory.index()).ok_or_else(|| {
        crate::SynthError::invariant("logical memory read-port row is out of range")
    })?;
    let writes = ports.writes.get(memory.index()).ok_or_else(|| {
        crate::SynthError::invariant("logical memory write-port row is out of range")
    })?;
    for &row in reads {
        let read = &module.memory_read_ports()[row];
        append_value_spans(inputs, module, anchors, connectivity, read.address, 0, None)?;
        if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = read.timing {
            append_value_spans(inputs, module, anchors, connectivity, clock.value, 0, None)?;
            if let Some(enable) = enable {
                append_value_spans(inputs, module, anchors, connectivity, enable.value, 0, None)?;
            }
        }
    }
    for &row in writes {
        let write = &module.memory_write_ports()[row];
        for value in [
            Some(write.address),
            Some(write.data),
            Some(write.clock.value),
            write.enable.map(|enable| enable.value),
            write.mask.map(|mask| mask.value),
        ]
        .into_iter()
        .flatten()
        {
            append_value_spans(inputs, module, anchors, connectivity, value, 0, None)?;
        }
    }
    for &row in reads {
        let signal = module.memory_read_ports()[row].data;
        let stored = module.signal(signal).ok_or_else(|| {
            crate::SynthError::invariant("logical memory output signal is unknown")
        })?;
        outputs.push(WorkEntitySpan {
            kind: WorkEntityKind::SignalNet(anchors.signal(signal)?),
            lsb: 0,
            width: stored.ty.width(),
        })?;
    }
    Ok(())
}

fn append_value_spans(
    output: &mut WorkEntitySetBuilder,
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    value: word::ValueId,
    lsb: u32,
    width: Option<u32>,
) -> Result<(), crate::SynthError> {
    let stored = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("logical net references an unknown value"))?;
    let width = width.unwrap_or_else(|| stored.ty.width().saturating_sub(lsb));
    let end = lsb
        .checked_add(width)
        .filter(|&end| width != 0 && end <= stored.ty.width())
        .ok_or_else(|| crate::SynthError::invariant("work-entity range exceeds its Word value"))?;
    for bit in lsb..end {
        let span = source_entity_span(
            module,
            anchors,
            connectivity.source(value, bit)?,
            stored.ty.state(),
        )?;
        append_adjacent_span(output, span)?;
    }
    Ok(())
}

fn source_entity_span(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    source: crate::word::bit_connectivity::BitSource,
    state: word::LogicStateKind,
) -> Result<WorkEntitySpan, crate::SynthError> {
    let (kind, lsb) = match source {
        crate::word::bit_connectivity::BitSource::Constant(value) => (
            WorkEntityKind::ConstantNet {
                value: bit_value_key(value),
                state: logic_state_key(state),
            },
            0,
        ),
        crate::word::bit_connectivity::BitSource::Value { value, bit } => {
            let stored = module
                .value(value)
                .ok_or_else(|| crate::SynthError::invariant("work-entity bit source is unknown"))?;
            match stored.kind {
                word::ValueKind::Operation(operation) => (
                    WorkEntityKind::OperationNet(anchors.operation(operation)?),
                    bit,
                ),
                word::ValueKind::Signal(reference) => (
                    WorkEntityKind::SignalNet(anchors.signal(reference.signal)?),
                    reference.lsb.checked_add(bit).ok_or_else(|| {
                        crate::SynthError::capacity("work-entity signal bit offset")
                    })?,
                ),
                word::ValueKind::Constant(_) => {
                    return Err(crate::SynthError::invariant(
                        "constant work-entity source lost its classification",
                    ));
                }
            }
        }
    };
    Ok(WorkEntitySpan {
        kind,
        lsb,
        width: 1,
    })
}

fn append_adjacent_span(
    spans: &mut WorkEntitySetBuilder,
    span: WorkEntitySpan,
) -> Result<(), crate::SynthError> {
    spans.push(span)
}

fn materialize_value_nets(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    value: word::ValueId,
    nets: &mut FragmentNetTable,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant("logical fragment references an unknown value")
    })?;
    (0..stored.ty.width())
        .map(|bit| {
            source_net(
                module,
                anchors,
                connectivity.source(value, bit)?,
                stored.ty.state(),
                nets,
            )
        })
        .collect()
}

fn source_net(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    source: crate::word::bit_connectivity::BitSource,
    state: word::LogicStateKind,
    nets: &mut FragmentNetTable,
) -> Result<NetBitId, crate::SynthError> {
    let id = source_net_id(module, anchors, source, state)?;
    let driver = match source {
        crate::word::bit_connectivity::BitSource::Constant(constant) => {
            Some(NetDriver::Constant(constant))
        }
        crate::word::bit_connectivity::BitSource::Value { .. } => None,
    };
    install_net(nets, id, state, driver)?;
    Ok(id)
}

fn source_net_id(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    source: crate::word::bit_connectivity::BitSource,
    state: word::LogicStateKind,
) -> Result<NetBitId, crate::SynthError> {
    match source {
        crate::word::bit_connectivity::BitSource::Constant(constant) => {
            Ok(constant_net_id(constant, state))
        }
        crate::word::bit_connectivity::BitSource::Value { value, bit } => {
            let source = module
                .value(value)
                .ok_or_else(|| crate::SynthError::invariant("logical bit source is unknown"))?;
            match source.kind {
                word::ValueKind::Operation(operation) => {
                    Ok(operation_net_id(anchors.operation(operation)?, bit))
                }
                word::ValueKind::Signal(reference) => {
                    let physical = reference
                        .lsb
                        .checked_add(bit)
                        .ok_or_else(|| crate::SynthError::capacity("logical signal bit offset"))?;
                    Ok(signal_net_id(anchors.signal(reference.signal)?, physical))
                }
                word::ValueKind::Constant(_) => Err(crate::SynthError::invariant(
                    "constant bit source lost its constant classification",
                )),
            }
        }
    }
}

fn install_net(
    nets: &mut FragmentNetTable,
    id: NetBitId,
    state: word::LogicStateKind,
    driver: Option<NetDriver>,
) -> Result<(), crate::SynthError> {
    if let Some(&row) = nets.rows.get(&id) {
        let net = &mut nets.nets[row];
        if net.state != state || (net.driver.is_some() && driver.is_some() && net.driver != driver)
        {
            return Err(crate::SynthError::invariant(
                "stable logical net has conflicting definitions",
            ));
        }
        if net.driver.is_none() {
            net.driver = driver;
        }
    } else {
        nets.rows.insert(id, nets.nets.len());
        nets.nets.push(NetBit { id, state, driver });
    }
    Ok(())
}

fn install_driver(
    nets: &mut FragmentNetTable,
    id: NetBitId,
    driver: NetDriver,
) -> Result<(), crate::SynthError> {
    let net = nets
        .rows
        .get(&id)
        .and_then(|&row| nets.nets.get_mut(row))
        .ok_or_else(|| crate::SynthError::invariant("logical driver net is absent"))?;
    if net
        .driver
        .replace(driver)
        .is_some_and(|current| current != driver)
    {
        return Err(crate::SynthError::invariant(
            "stable logical net has multiple drivers",
        ));
    }
    Ok(())
}

fn logical_revision_id(module: &word::WordModule) -> Result<DesignRevisionId, crate::SynthError> {
    let value_keys = crate::regional::region_graph::partition::semantic::value_keys(module)?;
    let operation_anchors = crate::regional::region_graph::partition::operation_anchors(module)?;
    let mut records = Vec::new();
    for (index, stored) in module.operations().iter().enumerate() {
        let mut record = blake3::Hasher::new();
        record.update(b"opto/logical-revision-operation/v2\0");
        record.update(&operation_anchors[index].bytes());
        record.update(&value_keys[stored.result.index()]);
        records.push(*record.finalize().as_bytes());
    }
    for index in 0..module.memories().len() {
        let memory = word::MemoryId::from_index(index).map_err(crate::SynthError::from)?;
        let stored = &module.memories()[index];
        let mut record = blake3::Hasher::new();
        record.update(b"opto/logical-revision-memory/v2\0");
        record.update(&logical_memory_cell_id(module, memory)?.bytes());
        record.update(&stored.element_type.width().to_le_bytes());
        record.update(&[
            u8::from(stored.element_type.is_signed()),
            stored.element_type.state() as u8,
        ]);
        record.update(&stored.depth.get().to_le_bytes());
        record.update(&memory_interface_id(module, memory)?);
        records.push(*record.finalize().as_bytes());
    }
    for connect in module.connects() {
        let mut record = blake3::Hasher::new();
        record.update(b"opto/logical-revision-connect/v2\0");
        record.update(&signal_anchor(module, connect.target.signal)?);
        if let Some(range) = connect.target.range {
            record.update(&[1]);
            record.update(&range.msb.to_le_bytes());
            record.update(&range.lsb.to_le_bytes());
        } else {
            record.update(&[0]);
        }
        if let Some(dynamic) = connect.target.dynamic {
            record.update(&[1]);
            record.update(&value_keys[dynamic.offset.index()]);
            record.update(&dynamic.width.get().to_le_bytes());
        } else {
            record.update(&[0]);
        }
        record.update(&value_keys[connect.value.index()]);
        records.push(*record.finalize().as_bytes());
    }
    records.sort_unstable();
    let mut revision = blake3::Hasher::new();
    revision.update(b"opto/logical-footprint-revision/v2\0");
    revision.update(&(module.name().len() as u64).to_le_bytes());
    revision.update(module.name().as_bytes());
    revision.update(&(records.len() as u64).to_le_bytes());
    for record in records {
        revision.update(&record);
    }
    Ok(DesignRevisionId::from_bytes(
        *revision.finalize().as_bytes(),
    ))
}

fn digest(domain: &[u8], parts: impl IntoIterator<Item = [u8; 32]>) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(domain);
    for part in parts {
        digest.update(&part);
    }
    *digest.finalize().as_bytes()
}

fn operation_cell_id(
    regions: &crate::SynthesisRegionGraph,
    operation: word::OpId,
) -> Result<CellId, crate::SynthError> {
    let anchor = regions.operation_anchor(operation).ok_or_else(|| {
        crate::SynthError::invariant("logical operation has no stable source anchor")
    })?;
    Ok(CellId::from_bytes(digest(
        b"opto/logical-operation-cell/v1\0",
        [anchor.bytes()],
    )))
}

pub(crate) fn logical_operation_cell_id(
    regions: &crate::SynthesisRegionGraph,
    operation: word::OpId,
) -> Result<CellId, crate::SynthError> {
    operation_cell_id(regions, operation)
}

pub(crate) fn logical_memory_cell_id(
    module: &word::WordModule,
    memory: word::MemoryId,
) -> Result<CellId, crate::SynthError> {
    let stored = module
        .memory(memory)
        .ok_or_else(|| crate::SynthError::invariant("logical memory is unknown"))?;
    let identity = stored.source.identity().ok_or_else(|| {
        crate::SynthError::invariant("logical memory has no stable source identity")
    })?;
    Ok(CellId::from_bytes(digest(
        b"opto/logical-memory-cell/v1\0",
        [identity.bytes()],
    )))
}

fn connection_cell_id(signal: [u8; 32], bit: u32) -> CellId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/logical-connection-cell/v1\0");
    digest.update(&signal);
    digest.update(&bit.to_le_bytes());
    CellId::from_bytes(*digest.finalize().as_bytes())
}

fn memory_interface_id(
    module: &word::WordModule,
    memory: word::MemoryId,
) -> Result<[u8; 32], crate::SynthError> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/logical-memory-interface/v1\0");
    for read in module
        .memory_read_ports()
        .iter()
        .filter(|read| read.memory == memory)
    {
        digest.update(
            &read
                .source
                .identity()
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "logical memory read has no stable source identity",
                    )
                })?
                .bytes(),
        );
        digest.update(&[match read.read_during_write {
            word::ReadDuringWrite::OldData => 0,
            word::ReadDuringWrite::NewData => 1,
            word::ReadDuringWrite::NoChange => 2,
            word::ReadDuringWrite::Undefined => 3,
        }]);
        match read.timing {
            word::MemoryReadTiming::Asynchronous => {
                digest.update(&[0]);
            }
            word::MemoryReadTiming::Synchronous {
                clock,
                enable,
                disabled,
            } => {
                digest.update(&[
                    1,
                    clock.edge as u8,
                    option_bool_tag(enable.map(|enable| enable.active_high)),
                    disabled as u8,
                ]);
            }
        }
    }
    for write in module
        .memory_write_ports()
        .iter()
        .filter(|write| write.memory == memory)
    {
        digest.update(
            &write
                .source
                .identity()
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "logical memory write has no stable source identity",
                    )
                })?
                .bytes(),
        );
        digest.update(&[
            write.clock.edge as u8,
            option_bool_tag(write.enable.map(|enable| enable.active_high)),
        ]);
        if let Some(mask) = write.mask {
            digest.update(&[1, u8::from(mask.active_high)]);
            digest.update(&mask.granularity.get().to_le_bytes());
        } else {
            digest.update(&[0]);
        }
        digest.update(&write.priority.to_le_bytes());
    }
    Ok(*digest.finalize().as_bytes())
}

const fn option_bool_tag(value: Option<bool>) -> u8 {
    match value {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}

fn operation_net_id(cell: CellId, bit: u32) -> NetBitId {
    net_id(b"opto/logical-operation-net/v1\0", cell.bytes(), bit)
}

fn signal_anchor(
    module: &word::WordModule,
    signal: word::SignalId,
) -> Result<[u8; 32], crate::SynthError> {
    let signal = module
        .signal(signal)
        .ok_or_else(|| crate::SynthError::invariant("logical signal is unknown"))?;
    let identity = signal.source.identity().ok_or_else(|| {
        crate::SynthError::invariant("logical signal has no stable source identity")
    })?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/logical-signal/v1\0");
    digest.update(&identity.bytes());
    if let Some(name) = signal.name {
        let name = module.name_str(name);
        digest.update(&(name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
    }
    Ok(*digest.finalize().as_bytes())
}

fn signal_net_id(signal: [u8; 32], bit: u32) -> NetBitId {
    net_id(b"opto/logical-signal-net/v1\0", signal, bit)
}

fn constant_net_id(value: opto_ir::BitVal, state: word::LogicStateKind) -> NetBitId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/logical-constant-net/v1\0");
    digest.update(&[bit_value_key(value), logic_state_key(state)]);
    NetBitId::from_bytes(*digest.finalize().as_bytes())
}

const fn bit_value_key(value: opto_ir::BitVal) -> u8 {
    match value {
        opto_ir::BitVal::Zero => 0,
        opto_ir::BitVal::One => 1,
        opto_ir::BitVal::X => 2,
        opto_ir::BitVal::Z => 3,
    }
}

const fn logic_state_key(state: word::LogicStateKind) -> u8 {
    match state {
        word::LogicStateKind::TwoState => 0,
        word::LogicStateKind::FourState => 1,
    }
}

fn net_id(domain: &[u8], source: [u8; 32], bit: u32) -> NetBitId {
    let mut digest = blake3::Hasher::new();
    digest.update(domain);
    digest.update(&source);
    digest.update(&bit.to_le_bytes());
    NetBitId::from_bytes(*digest.finalize().as_bytes())
}

fn work_item_id(design: DesignRevisionId, region: crate::RegionAnchorId) -> WorkItemId {
    WorkItemId::from_bytes(digest(
        b"opto/work-item/regional-architecture/v1\0",
        [design.bytes(), region.bytes()],
    ))
}

fn shard_id(design: DesignRevisionId, indices: &[usize], items: &[WorkItem]) -> CompilationShardId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/compilation-shard/v2\0");
    digest.update(&design.bytes());
    for &index in indices {
        digest.update(&items[index].id.0);
        digest.update(&items[index].context.bytes());
    }
    CompilationShardId::from_bytes(*digest.finalize().as_bytes())
}

fn design_error(error: &opto_ir::design::DesignError) -> crate::SynthError {
    crate::SynthError::invariant(format!("stable work revision is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word;

    #[test]
    fn work_entity_ranges_merge_and_subtract_exactly() {
        let kind = WorkEntityKind::SignalNet([3; 32]);
        let set = WorkEntitySet::new(vec![
            WorkEntitySpan {
                kind,
                lsb: 8,
                width: 8,
            },
            WorkEntitySpan {
                kind,
                lsb: 0,
                width: 8,
            },
        ])
        .unwrap();
        let removed = WorkEntitySet::new(vec![
            WorkEntitySpan {
                kind,
                lsb: 4,
                width: 4,
            },
            WorkEntitySpan {
                kind,
                lsb: 10,
                width: 2,
            },
        ])
        .unwrap();

        assert_eq!(set.0.len(), 1);
        assert_eq!(set.cardinality(), 16);
        let difference = set.difference(&removed).unwrap();
        assert_eq!(difference.0.len(), 1);
        assert_eq!(difference.0[0].kind, kind);
        assert_eq!(
            difference.0[0].ranges.as_ref(),
            &[
                WorkEntityRange { lsb: 0, width: 4 },
                WorkEntityRange { lsb: 8, width: 2 },
                WorkEntityRange { lsb: 12, width: 4 },
            ]
        );
    }

    #[test]
    fn stable_work_rows_are_independent_of_dense_word_ids() {
        let mut module = word::WordModule::new("work_graph");
        let bit = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::stable("work graph test");
        let input = module
            .add_port("a", word::PortDirection::Input, bit, source.clone())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, source.clone())
            .unwrap();
        for index in 0..2 {
            let value = module
                .unary(word::UnaryOp::BitNot, input, source.clone())
                .unwrap();
            let output = module
                .add_port(
                    format!("y{index}"),
                    word::PortDirection::Output,
                    bit,
                    source.clone(),
                )
                .unwrap();
            module
                .connect(
                    word::LValue::signal(module.port(output).unwrap().signal),
                    value,
                    source.clone(),
                )
                .unwrap();
        }
        let regions = super::super::region_graph::partition::build(
            &module,
            super::super::region_graph::RegionPartitionPolicy::with_target_work(1),
        )
        .unwrap();
        let design = Arc::new(WorkDesign::seal(&module, &regions).unwrap());
        let regions = Arc::new(regions);
        let scenarios = opto_timing::ScenarioSet::single(
            Arc::new(opto_timing::TimingContext::default()),
            Arc::new(opto_timing::TimingLibrary::default()),
            opto_timing::Parasitics::default(),
        )
        .generation();
        let contexts = (0..regions.regions().len())
            .map(|index| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
                WorkContext::logical(
                    WorkContextKey::from(crate::RegionContextKey::from_bytes_for_test(bytes)),
                    design.revision(),
                    scenarios,
                    [0; 32],
                    &[],
                )
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let runtime =
            opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 4 })
                .unwrap();
        let mut work = WorkGraph::build(
            &module,
            Arc::clone(&regions),
            Arc::clone(&design),
            contexts.clone(),
            &runtime,
        )
        .unwrap();

        assert_eq!(work.design.revision(), design.revision());
        assert_eq!(work.items.len(), regions.regions().len());
        assert_eq!(work.shards.len(), regions.regions().len());
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(
                work.items[index].kind,
                WorkItemKind::FixedLogic(region.id())
            );
            for &operation in regions.operations(region) {
                assert!(
                    work.items[index]
                        .core
                        .contains_cell(operation_cell_id(&regions, operation).unwrap())
                );
            }
        }
        for packet in work.portable_packets() {
            let bytes = opto_archive::to_bytes(&packet).unwrap();
            let restored: WorkPacket = opto_archive::from_bytes(&bytes).unwrap();
            assert_eq!(restored.design, packet.design);
            assert_eq!(opto_archive::to_bytes(&restored).unwrap(), bytes);
        }
        let semantic_items = work.items.iter().map(|item| item.id).collect::<Vec<_>>();
        let proof = |item: WorkItemId| opto_ir::design::EquivalenceCertificate {
            regime: opto_ir::design::EquivalenceRegime::ByConstruction,
            digest: item.0,
        };
        let expected_proof = |item: &WorkItem, output: &WorkItemId| {
            assert_eq!(*output, item.id);
            Ok(proof(item.id))
        };
        let execute = |work: &WorkGraph| {
            let results = SynthesisExecutor::execute(&runtime, work, |item, _| {
                Ok(WorkProduct::compiled_artifact(proof(item.id), item.id))
            })
            .unwrap();
            work.accept_results(results, expected_proof)
                .unwrap()
                .into_vec()
                .into_iter()
                .map(|result| result.output)
                .collect::<Vec<_>>()
        };
        assert_eq!(execute(&work), semantic_items);
        let mut invalid = SynthesisExecutor::execute(&runtime, &work, |item, _| {
            Ok(WorkProduct::compiled_artifact(proof(item.id), item.id))
        })
        .unwrap();
        let bytes = opto_archive::to_bytes(&invalid).unwrap();
        let restored: Vec<WorkResult<WorkItemId>> = opto_archive::from_bytes(&bytes).unwrap();
        assert_eq!(restored, invalid);
        invalid[0].artifact.footprint.replaces = WorkEntitySet::new(vec![]).unwrap();
        assert!(work.accept_results(invalid, expected_proof).is_err());
        let invalid_proof = SynthesisExecutor::execute(&runtime, &work, |item, _| {
            Ok(WorkProduct::compiled_artifact(
                opto_ir::design::EquivalenceCertificate {
                    regime: opto_ir::design::EquivalenceRegime::ByConstruction,
                    digest: [0; 32],
                },
                item.id,
            ))
        })
        .unwrap();
        assert!(work.accept_results(invalid_proof, expected_proof).is_err());
        let serial =
            opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 1 })
                .unwrap();
        let serial_work = WorkGraph::build(
            &module,
            Arc::clone(&regions),
            Arc::clone(&design),
            contexts,
            &serial,
        )
        .unwrap();
        assert!(
            serial_work
                .items
                .iter()
                .zip(&work.items)
                .all(|(left, right)| {
                    left.id == right.id
                        && left.kind == right.kind
                        && left.core == right.core
                        && left.halo == right.halo
                        && left.context == right.context
                        && left.estimated_work == right.estimated_work
                        && left.estimated_memory == right.estimated_memory
                })
        );
        work.rebatch(2).unwrap();
        assert_eq!(work.shards.len(), regions.regions().len().div_ceil(2));
        assert_eq!(
            work.shards
                .iter()
                .flat_map(|shard| shard.items.iter().map(|&item| work.items[item].id))
                .collect::<Vec<_>>(),
            semantic_items
        );
        assert_eq!(execute(&work), semantic_items);
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(
                work.items[index].kind,
                WorkItemKind::FixedLogic(region.id())
            );
        }
    }

    #[test]
    fn root_net_identity_survives_a_changed_driver_recipe() {
        let build = |operator| {
            let mut module = word::WordModule::new("stable_root_net");
            let bit = word::WordType::bits(1).unwrap();
            let source = word::SourceSpan::stable("stable root net test");
            let input = module
                .add_port("a", word::PortDirection::Input, bit, source.clone())
                .unwrap();
            let input = module
                .read_signal(module.port(input).unwrap().signal, source.clone())
                .unwrap();
            let value = module.unary(operator, input, source.clone()).unwrap();
            let output = module
                .add_port("y", word::PortDirection::Output, bit, source.clone())
                .unwrap();
            let output_signal = module.port(output).unwrap().signal;
            module
                .connect(
                    word::LValue::signal(module.port(output).unwrap().signal),
                    value,
                    source,
                )
                .unwrap();
            let regions = super::super::region_graph::partition::build(
                &module,
                super::super::region_graph::RegionPartitionPolicy::default(),
            )
            .unwrap();
            let design = WorkDesign::seal(&module, &regions).unwrap();
            let net = signal_net_id(signal_anchor(&module, output_signal).unwrap(), 0);
            (net, design.revision())
        };

        let inverted = build(word::UnaryOp::BitNot);
        let reduced = build(word::UnaryOp::ReductionOr);

        assert_eq!(inverted.0, reduced.0);
        assert_ne!(inverted.1, reduced.1);
    }

    #[test]
    fn logical_revision_is_independent_of_region_geometry() {
        let mut module = word::WordModule::new("geometry_independent_revision");
        let bit = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::stable("geometry independent revision");
        let input = module
            .add_port("a", word::PortDirection::Input, bit, source.clone())
            .unwrap();
        let mut value = module
            .read_signal(module.port(input).unwrap().signal, source.clone())
            .unwrap();
        for _ in 0..8 {
            value = module
                .unary(word::UnaryOp::BitNot, value, source.clone())
                .unwrap();
        }
        let output = module
            .add_port("y", word::PortDirection::Output, bit, source.clone())
            .unwrap();
        module
            .connect(
                word::LValue::signal(module.port(output).unwrap().signal),
                value,
                source,
            )
            .unwrap();
        let fine = super::super::region_graph::partition::build(
            &module,
            super::super::region_graph::RegionPartitionPolicy::with_target_work(1),
        )
        .unwrap();
        let coarse = super::super::region_graph::partition::build(
            &module,
            super::super::region_graph::RegionPartitionPolicy::with_target_work(1 << 20),
        )
        .unwrap();
        assert_ne!(fine.regions().len(), coarse.regions().len());

        let fine = WorkDesign::seal(&module, &fine).unwrap();
        let coarse = WorkDesign::seal(&module, &coarse).unwrap();

        assert_eq!(fine.revision(), coarse.revision());
    }

    #[test]
    fn logical_revision_ignores_constant_gated_feedback() {
        let mut module = word::WordModule::new("constant_feedback_revision");
        let bit = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::stable("constant feedback revision");
        let signal = module.add_wire("feedback", bit, source.clone()).unwrap();
        let feedback = module.read_signal(signal, source.clone()).unwrap();
        let disabled = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                source.clone(),
            )
            .unwrap();
        let value = module
            .binary(word::BinaryOp::BitAnd, disabled, feedback, source.clone())
            .unwrap();
        module
            .connect(word::LValue::signal(signal), value, source)
            .unwrap();
        let regions = super::super::region_graph::partition::build(
            &module,
            super::super::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();

        WorkDesign::seal(&module, &regions).unwrap();
    }

    #[test]
    fn logical_revision_tracks_dynamic_selection_per_bit() {
        let mut module = word::WordModule::new("dynamic_feedback_revision");
        let source = word::SourceSpan::stable("dynamic feedback revision");
        let pair = word::WordType::bits(2).unwrap();
        let records = module.add_wire("records", pair, source.clone()).unwrap();
        let records_value = module.read_signal(records, source.clone()).unwrap();
        let offset = module
            .constant(
                opto_ir::ConstBits::from_bin_str("1").unwrap(),
                word::WordType::bits(1).unwrap(),
                source.clone(),
            )
            .unwrap();
        let selected = module
            .dynamic_extract(records_value, offset, 1, source.clone())
            .unwrap();
        module
            .connect(
                word::LValue::signal(records).with_range(word::BitRange { msb: 0, lsb: 0 }),
                selected,
                source.clone(),
            )
            .unwrap();
        let high = module
            .constant(
                opto_ir::ConstBits::from_bin_str("1").unwrap(),
                word::WordType::bits(1).unwrap(),
                source.clone(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(records).with_range(word::BitRange { msb: 1, lsb: 1 }),
                high,
                source,
            )
            .unwrap();
        let regions = super::super::region_graph::partition::build(
            &module,
            super::super::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();

        WorkDesign::seal(&module, &regions).unwrap();
    }
}

#[cfg(test)]
mod coalescing_publication_tests {
    use super::*;
    use opto_ir::word;

    fn multi_driver_module(name: &str) -> word::WordModule {
        let mut module = word::WordModule::new(name);
        let byte = word::WordType::bits(8).unwrap();
        let wide = word::WordType::bits(32).unwrap();
        let source = word::SourceSpan::stable("coalescing publication test");
        let inputs = (0..4)
            .map(|index| {
                let port = module
                    .add_port(
                        format!("a{index}"),
                        word::PortDirection::Input,
                        byte,
                        source.clone(),
                    )
                    .unwrap();
                module
                    .read_signal(module.port(port).unwrap().signal, source.clone())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let aggregate = module.add_wire("aggregate", wide, source.clone()).unwrap();
        for (index, &value) in inputs.iter().enumerate() {
            let lsb = u32::try_from(index).unwrap() * 8;
            module
                .connect(
                    word::LValue::signal(aggregate)
                        .with_range(word::BitRange { msb: lsb + 7, lsb }),
                    value,
                    source.clone(),
                )
                .unwrap();
        }
        // Keep the wire observable so every published entity stays live.
        let output = module
            .add_port("y", word::PortDirection::Output, wide, source.clone())
            .unwrap();
        let read = module.read_signal(aggregate, source.clone()).unwrap();
        module
            .connect(
                word::LValue::signal(module.port(output).unwrap().signal),
                read,
                source,
            )
            .unwrap();
        module
    }

    #[test]
    fn committed_coalescing_revision_matches_a_fresh_seal() {
        let mut module = multi_driver_module("coalescing");
        let coalescing = crate::planning::dataflow::static_wire_driver_fragments(&module).unwrap();
        assert!(!coalescing.is_empty());
        let base_regions = crate::regional::region_graph::partition::build(
            &module,
            crate::regional::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();
        let base = WorkDesign::seal(&module, &base_regions).unwrap();
        let (wave, signals) = coalescing.into_parts();
        let published = module
            .publish_fragments(wave)
            .map_err(crate::SynthError::from)
            .unwrap();

        let regions = crate::regional::region_graph::partition::build(
            &module,
            crate::regional::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();
        let deltas =
            coalesce_revision_deltas(&module, &regions, base.revision(), &published, &signals)
                .unwrap();
        assert_eq!(deltas.len(), signals.len());
        let fresh = WorkDesign::seal(&module, &regions).unwrap();
        let committed =
            commit_coalescing_revision(base.revision(), fresh.clone(), &deltas).unwrap();
        assert_eq!(committed.revision(), fresh.revision());
        assert_ne!(committed.revision(), base.revision());
    }

    #[test]
    fn failed_commit_leaves_the_base_generation_unchanged() {
        use opto_ir::design::{EquivalenceCertificate, EquivalenceRegime};

        let mut module = multi_driver_module("rollback");
        let coalescing = crate::planning::dataflow::static_wire_driver_fragments(&module).unwrap();
        let base_regions = crate::regional::region_graph::partition::build(
            &module,
            crate::regional::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();
        let base = WorkDesign::seal(&module, &base_regions).unwrap();
        let before = base.revision();

        let (wave, signals) = coalescing.into_parts();
        let published = module
            .publish_fragments(wave)
            .map_err(crate::SynthError::from)
            .unwrap();
        let regions = crate::regional::region_graph::partition::build(
            &module,
            crate::regional::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();
        let mut deltas =
            coalesce_revision_deltas(&module, &regions, base.revision(), &published, &signals)
                .unwrap();
        // Corrupt one certificate so the wave must be rejected wholesale.
        let proof = deltas[0].proof;
        deltas[0].proof = EquivalenceCertificate {
            regime: EquivalenceRegime::ByConstruction,
            digest: [0u8; 32],
        };
        let next = WorkDesign::seal(&module, &regions).unwrap();
        assert!(commit_coalescing_revision(base.revision(), next.clone(), &deltas).is_err());
        deltas[0].proof = proof;
        let driven = deltas[0]
            .nets
            .iter_mut()
            .find(|net| matches!(net.driver, Some(NetDriver::Cell { .. })))
            .unwrap();
        driven.driver = None;
        assert!(commit_coalescing_revision(base.revision(), next, &deltas).is_err());
        assert_eq!(base.revision(), before);
    }
}
