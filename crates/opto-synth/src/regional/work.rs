// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable revision and execution rows derived from one sealed region graph.

use opto_ir::design::{
    Cell, CellClass, CellId, DesignBuilder, DesignRevision, DesignRevisionId, EntityId, EntitySet,
    NetBit, NetBitId, NetDriver, RevisionFootprint,
};
use opto_ir::word;
use opto_runtime::{Task, TaskKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

const WORK_TASK_DOMAIN: u32 = 0x574f_524b;
const COARSE_GROUP_SHARDS: usize = 16;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogicalOperation {
    kind: LogicalOperationKind,
    operands: Box<[word::WordType]>,
    result: word::WordType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LogicalOperationKind {
    Unary(word::UnaryOp),
    Binary(word::BinaryOp),
    Mux,
    Concat,
    Extract {
        lsb: u32,
        width: u32,
    },
    Cast(word::CastKind),
    TriState {
        enable_active_high: bool,
    },
    DynamicExtract {
        width: u32,
    },
    DynamicInsert,
    Register {
        edge: word::Edge,
        enable_active_high: Option<bool>,
        resets: Box<[LogicalReset]>,
    },
    Latch {
        enable_active_high: bool,
        resets: Box<[LogicalReset]>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogicalReset {
    kind: word::ResetKind,
    active_high: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogicalCell {
    Operation(LogicalOperation),
    Connection,
    Memory {
        element: word::WordType,
        depth: u32,
        interface: [u8; 32],
    },
}

impl opto_ir::design::DesignPayload for LogicalCell {
    fn semantic_fingerprint(&self) -> [u8; 32] {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/logical-cell/v1\0");
        hash_logical_cell(&mut digest, self);
        *digest.finalize().as_bytes()
    }
}

#[derive(Debug, Clone)]
/// Canonical immutable macro design consumed by every regional work epoch.
pub(crate) struct WorkDesign(DesignRevision<LogicalCell>);

#[derive(Debug)]
pub(crate) struct WorkItem {
    id: WorkItemId,
    core: EntitySet,
    halo: EntitySet,
    context: WorkContextKey,
    estimated_work: u64,
    estimated_memory: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkPacketItem {
    id: WorkItemId,
    core: EntitySet,
    halo: EntitySet,
    context: WorkContext,
}

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

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WorkResult<T> {
    item: WorkItemId,
    shard: CompilationShardId,
    context: WorkContextKey,
    footprint: RevisionFootprint,
    proof: opto_ir::design::EquivalenceCertificate,
    output: T,
}

pub(crate) trait SynthesisExecutor {
    fn execute<T, F>(
        &self,
        packets: Vec<Task<WorkPacket>>,
        operation: F,
    ) -> Result<Vec<WorkResult<T>>, crate::SynthError>
    where
        T: Send,
        F: Fn(
                &WorkPacketItem,
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

impl WorkPacketItem {
    pub(crate) const fn id(&self) -> WorkItemId {
        self.id
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
    item_regions: Box<[crate::RegionRowId]>,
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
        let revision = &design.0;
        let connectivity = crate::word::bit_connectivity::BitConnectivity::new(module)?;
        let rows = runtime.analyze_indexed(regions.regions().len(), |index| {
            let region = regions.regions()[index];
            let cells = region_cells(module, regions.as_ref(), region)?;
            let mut core = region_value_entities(
                module,
                regions.as_ref(),
                &connectivity,
                region,
                crate::RegionPortDirection::Output,
                revision,
            )?;
            let mut halo = region_value_entities(
                module,
                regions.as_ref(),
                &connectivity,
                region,
                crate::RegionPortDirection::Input,
                revision,
            )?;
            for cell in cells {
                let stored = revision.cell(cell).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "work item cell is absent from the logical revision",
                    )
                })?;
                core.insert(EntityId::Cell(cell));
                core.extend(stored.outputs.iter().copied().map(EntityId::NetBit));
                halo.extend(stored.inputs.iter().copied().map(EntityId::NetBit));
            }
            halo.retain(|entity| !core.contains(entity));
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
            let core =
                EntitySet::new(core.into_iter().collect()).map_err(|error| design_error(&error))?;
            let halo =
                EntitySet::new(halo.into_iter().collect()).map_err(|error| design_error(&error))?;
            let id = work_item_id(revision.revision(), region.id());
            let estimated_memory = u64::try_from(core.as_slice().len() + halo.as_slice().len())
                .unwrap_or(u64::MAX)
                .max(1);
            let item = WorkItem {
                id,
                core,
                halo,
                context: contexts[region.row().index()].key,
                estimated_work: region.estimated_work().max(1),
                estimated_memory,
            };
            Ok::<_, crate::SynthError>((id, region.row(), item))
        })?;
        let item_regions = rows.iter().map(|row| row.1).collect();
        let items = rows.into_iter().map(|row| row.2).collect::<Vec<_>>();
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
            item_regions,
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
                    id: shard_id(self.design.0.revision(), indices, &self.items),
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

    pub(crate) fn packet_tasks(&self) -> Vec<Task<WorkPacket>> {
        self.shards
            .iter()
            .enumerate()
            .map(|(ordinal, shard)| {
                let items = shard
                    .items
                    .iter()
                    .map(|&row| WorkPacketItem {
                        id: self.items[row].id,
                        core: self.items[row].core.clone(),
                        halo: self.items[row].halo.clone(),
                        context: self.contexts[row].clone(),
                    })
                    .collect();
                Task::new(
                    TaskKey::new(WORK_TASK_DOMAIN, ordinal as u64),
                    WorkPacket {
                        schema: 1,
                        design: self.design.0.revision(),
                        shard: shard.id,
                        items,
                        estimated_work: shard.estimated_work,
                        estimated_memory: shard.estimated_memory,
                    },
                )
                .with_estimated_work(shard.estimated_work)
                .with_estimated_memory(shard.estimated_memory)
            })
            .collect()
    }

    pub(crate) fn regions(&self) -> &crate::SynthesisRegionGraph {
        &self.regions
    }

    pub(crate) fn state_cells(&self) -> impl Iterator<Item = CellId> + '_ {
        self.design
            .0
            .cells()
            .filter(|cell| {
                cell.class == CellClass::StateBoundary
                    && matches!(cell.kind, LogicalCell::Operation(_))
            })
            .map(|cell| cell.id)
    }

    pub(crate) fn item_region(&self, item: WorkItemId) -> Option<crate::RegionRowId> {
        self.item_rows
            .get(&item)
            .and_then(|&row| self.item_regions.get(row))
            .copied()
    }

    pub(crate) fn accept_results<T>(
        &self,
        results: Vec<WorkResult<T>>,
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
            let shard = self
                .shards
                .iter()
                .find(|shard| shard.items.binary_search(&row).is_ok())
                .map(|shard| shard.id)
                .ok_or_else(|| {
                    crate::SynthError::invariant("work result item has no compilation shard")
                })?;
            if result.footprint.base != self.design.0.revision()
                || result.shard != shard
                || result.context != item.context
                || result.footprint.reads != reads
                || result.footprint.replaces != item.core
            {
                return Err(crate::SynthError::invariant(
                    "work result does not match its immutable revision, context, or footprint",
                ));
            }
            if outputs[row]
                .replace(WorkProduct {
                    proof: result.proof,
                    output: result.output,
                })
                .is_some()
            {
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

    fn validate(&self) -> Result<(), crate::SynthError> {
        if self.predecessors.row_count() != self.items.len()
            || self.successors.row_count() != self.items.len()
            || self.item_regions.len() != self.items.len()
            || self.contexts.len() != self.items.len()
            || self.item_rows.len() != self.items.len()
            || self.coarse_groups.value_count() != self.shards.len()
        {
            return Err(crate::SynthError::invariant(
                "work shards do not match their stable semantic items",
            ));
        }
        let revision = self.design.0.revision();
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
                    < u64::try_from(item.core.as_slice().len() + item.halo.as_slice().len())
                        .unwrap_or(u64::MAX)
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
                    || shard.id != shard_id(self.design.0.revision(), &shard.items, &self.items)
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
        packets: Vec<Task<WorkPacket>>,
        operation: F,
    ) -> Result<Vec<WorkResult<T>>, crate::SynthError>
    where
        T: Send,
        F: Fn(
                &WorkPacketItem,
                &opto_runtime::ExecutionContext,
            ) -> Result<WorkProduct<T>, crate::SynthError>
            + Send
            + Sync,
    {
        self.map_ordered_composite(packets, |packet, runtime| {
            if packet.schema != 1 {
                return Err(crate::SynthError::invariant(
                    "work packet has an unsupported schema",
                ));
            }
            if packet.estimated_work == 0 || packet.estimated_memory == 0 {
                return Err(crate::SynthError::invariant(
                    "work packet has an invalid work or memory estimate",
                ));
            }
            packet
                .items
                .iter()
                .map(|item| {
                    let product = operation(item, runtime)?;
                    Ok(WorkResult {
                        item: item.id,
                        shard: packet.shard,
                        context: item.context.key,
                        footprint: RevisionFootprint {
                            base: packet.design,
                            reads: entity_union(&item.core, &item.halo)?,
                            replaces: item.core.clone(),
                        },
                        proof: product.proof,
                        output: product.output,
                    })
                })
                .collect::<Result<Vec<_>, crate::SynthError>>()
        })
        .map(|rows| rows.into_iter().flatten().collect())
    }
}

fn item_read_set(item: &WorkItem) -> Result<EntitySet, crate::SynthError> {
    entity_union(&item.core, &item.halo)
}

fn entity_union(left: &EntitySet, right: &EntitySet) -> Result<EntitySet, crate::SynthError> {
    EntitySet::new(
        left.as_slice()
            .iter()
            .chain(right.as_slice())
            .copied()
            .collect(),
    )
    .map_err(|error| crate::SynthError::invariant(error.to_string()))
}

impl WorkDesign {
    pub(crate) fn seal(
        module: &word::WordModule,
        regions: &crate::SynthesisRegionGraph,
    ) -> Result<Self, crate::SynthError> {
        seal_logical_design(module, regions).map(Self)
    }

    pub(crate) const fn revision(&self) -> DesignRevisionId {
        self.0.revision()
    }
}

fn seal_logical_design(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
) -> Result<DesignRevision<LogicalCell>, crate::SynthError> {
    let mut nets = BTreeMap::<NetBitId, NetBit>::new();
    let mut cells = Vec::new();
    let connectivity = crate::word::bit_connectivity::BitConnectivity::new(module)?;
    for (index, signal) in module.signals().iter().enumerate() {
        let signal_id = word::SignalId::from_index(index).map_err(crate::SynthError::from)?;
        let anchor = signal_anchor(module, signal_id)?;
        for bit in 0..signal.ty.width() {
            install_net(
                &mut nets,
                signal_net_id(anchor, bit),
                signal.ty.state(),
                None,
            )?;
        }
    }
    for &region in regions.regions() {
        for &operation in regions.operations(region) {
            let kind = logical_operation(module, operation)?;
            let stored = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("logical revision references an unknown operation")
            })?;
            let cell = operation_cell_id(regions, operation)?;
            let inputs = crate::word::operation_inputs(&stored.kind)
                .into_iter()
                .map(|value| value_nets(module, regions, &connectivity, value, &mut nets))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Box<[_]>>();
            let outputs = operation_outputs(module, regions, operation, &mut nets)?;
            for (output, &net) in outputs.iter().enumerate() {
                let output = u32::try_from(output)
                    .map_err(|_| crate::SynthError::capacity("logical output bit ordinal"))?;
                install_driver(&mut nets, net, NetDriver::Cell { cell, output })?;
            }
            cells.push(Cell {
                id: cell,
                kind: LogicalCell::Operation(kind),
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
        for &memory in regions.memories(region) {
            let stored = module.memory(memory).ok_or_else(|| {
                crate::SynthError::invariant("logical revision references an unknown memory")
            })?;
            let cell = memory_cell_id(module, memory)?;
            let (inputs, outputs) = memory_nets(module, regions, &connectivity, memory, &mut nets)?;
            for (output, &net) in outputs.iter().enumerate() {
                install_driver(
                    &mut nets,
                    net,
                    NetDriver::Cell {
                        cell,
                        output: u32::try_from(output).map_err(|_| {
                            crate::SynthError::capacity("memory output bit ordinal")
                        })?,
                    },
                )?;
            }
            cells.push(Cell {
                id: cell,
                kind: LogicalCell::Memory {
                    element: stored.element_type,
                    depth: stored.depth.get(),
                    interface: memory_interface_id(module, memory)?,
                },
                class: CellClass::StateBoundary,
                inputs,
                outputs,
                source: stored.source.clone(),
            });
        }
        for &port in regions
            .input_ports(region)
            .iter()
            .chain(regions.output_ports(region))
        {
            let value = regions
                .port(port)
                .ok_or_else(|| crate::SynthError::invariant("logical boundary port is unknown"))?
                .value();
            value_nets(module, regions, &connectivity, value, &mut nets)?;
        }
        for flow in regions.bit_flows(region) {
            value_nets(module, regions, &connectivity, flow.value(), &mut nets)?;
        }
    }
    for (index, signal) in module.signals().iter().enumerate() {
        let signal_id = word::SignalId::from_index(index).map_err(crate::SynthError::from)?;
        let anchor = signal_anchor(module, signal_id)?;
        for bit in 0..signal.ty.width() {
            let Some(source) = connectivity.signal_source(signal_id, bit)? else {
                continue;
            };
            let input = source_net(module, regions, source, signal.ty.state(), &mut nets)?;
            let output = signal_net_id(anchor, bit);
            if input == output {
                continue;
            }
            let cell = connection_cell_id(anchor, bit);
            install_driver(&mut nets, output, NetDriver::Cell { cell, output: 0 })?;
            cells.push(Cell {
                id: cell,
                kind: LogicalCell::Connection,
                class: CellClass::Combinational,
                inputs: Box::new([input]),
                outputs: Box::new([output]),
                source: signal.source.clone(),
            });
        }
    }
    let revision = logical_revision_id(&cells, &nets);
    let mut builder = DesignBuilder::new(revision);
    for net in nets.into_values() {
        builder.add_net(net);
    }
    for cell in cells {
        builder.add_cell(cell);
    }
    builder.seal().map_err(|error| design_error(&error))
}

fn operation_outputs(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    operation: word::OpId,
    nets: &mut BTreeMap<NetBitId, NetBit>,
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
        .map(|bit| {
            let id = operation_net_id(regions, cell, bit)?;
            install_net(nets, id, result.ty.state(), None)?;
            Ok(id)
        })
        .collect()
}

fn region_value_entities(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    region: crate::SynthesisRegion,
    direction: crate::RegionPortDirection,
    design: &DesignRevision<LogicalCell>,
) -> Result<std::collections::BTreeSet<EntityId>, crate::SynthError> {
    let ports = match direction {
        crate::RegionPortDirection::Input => regions.input_ports(region),
        crate::RegionPortDirection::Output => regions.output_ports(region),
    };
    let mut scratch = BTreeMap::new();
    let mut entities = std::collections::BTreeSet::new();
    for &port in ports {
        let value = regions
            .port(port)
            .ok_or_else(|| crate::SynthError::invariant("work item boundary port is unknown"))?
            .value();
        for net in value_nets(module, regions, connectivity, value, &mut scratch)? {
            if design.net(net).is_none() {
                return Err(crate::SynthError::invariant(
                    "work item boundary net is absent from the logical revision",
                ));
            }
            entities.insert(EntityId::NetBit(net));
        }
    }
    if direction == crate::RegionPortDirection::Output {
        for flow in regions.bit_flows(region) {
            let value = value_nets(module, regions, connectivity, flow.value(), &mut scratch)?;
            let net = value.get(flow.bit() as usize).ok_or_else(|| {
                crate::SynthError::invariant("work item bit flow is outside its Word value")
            })?;
            if design.net(*net).is_none() {
                return Err(crate::SynthError::invariant(
                    "work item publication net is absent from the logical revision",
                ));
            }
            entities.insert(EntityId::NetBit(*net));
        }
    }
    Ok(entities)
}

type MemoryNetBinding = (Box<[NetBitId]>, Box<[NetBitId]>);

fn memory_nets(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    memory: word::MemoryId,
    nets: &mut BTreeMap<NetBitId, NetBit>,
) -> Result<MemoryNetBinding, crate::SynthError> {
    let reads = module
        .memory_read_ports()
        .iter()
        .filter(|read| read.memory == memory)
        .collect::<Vec<_>>();
    let writes = module
        .memory_write_ports()
        .iter()
        .filter(|write| write.memory == memory)
        .collect::<Vec<_>>();
    let mut inputs = Vec::new();
    for read in &reads {
        append_value_nets(
            &mut inputs,
            module,
            regions,
            connectivity,
            read.address,
            nets,
        )?;
        if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = read.timing {
            append_value_nets(
                &mut inputs,
                module,
                regions,
                connectivity,
                clock.value,
                nets,
            )?;
            if let Some(enable) = enable {
                append_value_nets(
                    &mut inputs,
                    module,
                    regions,
                    connectivity,
                    enable.value,
                    nets,
                )?;
            }
        }
    }
    for write in writes {
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
            append_value_nets(&mut inputs, module, regions, connectivity, value, nets)?;
        }
    }
    let outputs = reads
        .into_iter()
        .map(|read| signal_nets(module, read.data, nets))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok((inputs.into_boxed_slice(), outputs))
}

fn append_value_nets(
    output: &mut Vec<NetBitId>,
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    value: word::ValueId,
    nets: &mut BTreeMap<NetBitId, NetBit>,
) -> Result<(), crate::SynthError> {
    output.extend(value_nets(module, regions, connectivity, value, nets)?);
    Ok(())
}

fn signal_nets(
    module: &word::WordModule,
    signal: word::SignalId,
    nets: &mut BTreeMap<NetBitId, NetBit>,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    let anchor = signal_anchor(module, signal)?;
    let stored = module
        .signal(signal)
        .ok_or_else(|| crate::SynthError::invariant("logical memory output signal is unknown"))?;
    (0..stored.ty.width())
        .map(|bit| {
            let id = signal_net_id(anchor, bit);
            install_net(nets, id, stored.ty.state(), None)?;
            Ok(id)
        })
        .collect()
}

#[cfg(test)]
fn same_design(left: &DesignRevision<LogicalCell>, right: &DesignRevision<LogicalCell>) -> bool {
    left.cell_count() == right.cell_count()
        && left.net_count() == right.net_count()
        && left.cells().all(|cell| right.cell(cell.id) == Some(cell))
        && left.nets().all(|net| right.net(net.id) == Some(net))
}

fn logical_operation(
    module: &word::WordModule,
    operation: word::OpId,
) -> Result<LogicalOperation, crate::SynthError> {
    let stored = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant("logical operation recipe references an unknown operation")
    })?;
    let kind = match &stored.kind {
        word::OpKind::Unary { op, .. } => LogicalOperationKind::Unary(*op),
        word::OpKind::Binary { op, .. } => LogicalOperationKind::Binary(*op),
        word::OpKind::Mux { .. } => LogicalOperationKind::Mux,
        word::OpKind::TriState { enable, .. } => LogicalOperationKind::TriState {
            enable_active_high: enable.active_high,
        },
        word::OpKind::DynamicExtract { width, .. } => {
            LogicalOperationKind::DynamicExtract { width: width.get() }
        }
        word::OpKind::DynamicInsert { .. } => LogicalOperationKind::DynamicInsert,
        word::OpKind::Register(register) => LogicalOperationKind::Register {
            edge: register.edge,
            enable_active_high: register.enable.map(|enable| enable.active_high),
            resets: register
                .resets
                .iter()
                .map(|reset| LogicalReset {
                    kind: reset.kind,
                    active_high: reset.active_high,
                })
                .collect(),
        },
        word::OpKind::Latch(latch) => LogicalOperationKind::Latch {
            enable_active_high: latch.enable.active_high,
            resets: latch
                .resets
                .iter()
                .map(|reset| LogicalReset {
                    kind: reset.kind,
                    active_high: reset.active_high,
                })
                .collect(),
        },
        word::OpKind::Concat { .. } => LogicalOperationKind::Concat,
        word::OpKind::Extract { lsb, width, .. } => LogicalOperationKind::Extract {
            lsb: *lsb,
            width: width.get(),
        },
        word::OpKind::Cast { kind, .. } => LogicalOperationKind::Cast(*kind),
    };
    let operands = crate::word::operation_inputs(&stored.kind)
        .into_iter()
        .map(|value| {
            module.value(value).map(|value| value.ty).ok_or_else(|| {
                crate::SynthError::invariant("logical operation has an unknown operand")
            })
        })
        .collect::<Result<_, _>>()?;
    let result = module
        .value(stored.result)
        .map(|value| value.ty)
        .ok_or_else(|| crate::SynthError::invariant("logical operation has an unknown result"))?;
    Ok(LogicalOperation {
        kind,
        operands,
        result,
    })
}

fn region_cells(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    region: crate::SynthesisRegion,
) -> Result<Box<[CellId]>, crate::SynthError> {
    let mut cells = Vec::new();
    for &operation in regions.operations(region) {
        cells.push(operation_cell_id(regions, operation)?);
    }
    for &memory in regions.memories(region) {
        cells.push(memory_cell_id(module, memory)?);
    }
    cells.sort_unstable();
    cells.dedup();
    Ok(cells.into_boxed_slice())
}

fn value_nets(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    value: word::ValueId,
    nets: &mut BTreeMap<NetBitId, NetBit>,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    let stored = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("logical net references an unknown value"))?;
    (0..stored.ty.width())
        .map(|bit| {
            source_net(
                module,
                regions,
                connectivity.source(value, bit)?,
                stored.ty.state(),
                nets,
            )
        })
        .collect()
}

fn source_net(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    source: crate::word::bit_connectivity::BitSource,
    state: word::LogicStateKind,
    nets: &mut BTreeMap<NetBitId, NetBit>,
) -> Result<NetBitId, crate::SynthError> {
    let (id, state, driver) = match source {
        crate::word::bit_connectivity::BitSource::Constant(constant) => (
            constant_net_id(constant, state),
            state,
            Some(NetDriver::Constant(constant)),
        ),
        crate::word::bit_connectivity::BitSource::Value { value, bit } => {
            let source = module
                .value(value)
                .ok_or_else(|| crate::SynthError::invariant("logical bit source is unknown"))?;
            match source.kind {
                word::ValueKind::Operation(operation) => (
                    operation_net_id(regions, operation, bit)?,
                    source.ty.state(),
                    None,
                ),
                word::ValueKind::Signal(reference) => {
                    let physical = reference
                        .lsb
                        .checked_add(bit)
                        .ok_or_else(|| crate::SynthError::capacity("logical signal bit offset"))?;
                    (
                        signal_net_id(signal_anchor(module, reference.signal)?, physical),
                        source.ty.state(),
                        None,
                    )
                }
                word::ValueKind::Constant(_) => {
                    return Err(crate::SynthError::invariant(
                        "constant bit source lost its constant classification",
                    ));
                }
            }
        }
    };
    install_net(nets, id, state, driver)?;
    Ok(id)
}

fn install_net(
    nets: &mut BTreeMap<NetBitId, NetBit>,
    id: NetBitId,
    state: word::LogicStateKind,
    driver: Option<NetDriver>,
) -> Result<(), crate::SynthError> {
    match nets.entry(id) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(NetBit { id, state, driver });
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let net = entry.get_mut();
            if net.state != state
                || (net.driver.is_some() && driver.is_some() && net.driver != driver)
            {
                return Err(crate::SynthError::invariant(
                    "stable logical net has conflicting definitions",
                ));
            }
            if net.driver.is_none() {
                net.driver = driver;
            }
        }
    }
    Ok(())
}

fn install_driver(
    nets: &mut BTreeMap<NetBitId, NetBit>,
    id: NetBitId,
    driver: NetDriver,
) -> Result<(), crate::SynthError> {
    let net = nets
        .get_mut(&id)
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

fn logical_revision_id(
    cells: &[Cell<LogicalCell>],
    nets: &BTreeMap<NetBitId, NetBit>,
) -> DesignRevisionId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/logical-design-revision/v1\0");
    let mut cells = cells.iter().collect::<Vec<_>>();
    cells.sort_unstable_by_key(|cell| cell.id);
    for cell in cells {
        digest.update(&cell.id.bytes());
        digest.update(&[match cell.class {
            CellClass::Combinational => 0,
            CellClass::StateBoundary => 1,
        }]);
        hash_logical_cell(&mut digest, &cell.kind);
        for input in &cell.inputs {
            digest.update(&input.bytes());
        }
        digest.update(&[0xff]);
        for output in &cell.outputs {
            digest.update(&output.bytes());
        }
    }
    for net in nets.values() {
        digest.update(&net.id.bytes());
        digest.update(&[match net.state {
            word::LogicStateKind::TwoState => 0,
            word::LogicStateKind::FourState => 1,
        }]);
        match net.driver {
            None => {
                digest.update(&[0]);
            }
            Some(NetDriver::Cell { cell, output }) => {
                digest.update(&[1]);
                digest.update(&cell.bytes());
                digest.update(&output.to_le_bytes());
            }
            Some(NetDriver::Constant(value)) => {
                digest.update(&[2, bit_value_tag(value)]);
            }
        }
    }
    DesignRevisionId::from_bytes(*digest.finalize().as_bytes())
}

fn hash_logical_cell(digest: &mut blake3::Hasher, cell: &LogicalCell) {
    match cell {
        LogicalCell::Operation(operation) => {
            digest.update(&[0]);
            match &operation.kind {
                LogicalOperationKind::Unary(op) => {
                    digest.update(&[0, *op as u8]);
                }
                LogicalOperationKind::Binary(op) => {
                    digest.update(&[1, *op as u8]);
                }
                LogicalOperationKind::Mux => {
                    digest.update(&[2]);
                }
                LogicalOperationKind::Concat => {
                    digest.update(&[8]);
                }
                LogicalOperationKind::Extract { lsb, width } => {
                    digest.update(&[9]);
                    digest.update(&lsb.to_le_bytes());
                    digest.update(&width.to_le_bytes());
                }
                LogicalOperationKind::Cast(kind) => {
                    digest.update(&[10, *kind as u8]);
                }
                LogicalOperationKind::TriState { enable_active_high } => {
                    digest.update(&[3, u8::from(*enable_active_high)]);
                }
                LogicalOperationKind::DynamicExtract { width } => {
                    digest.update(&[4]);
                    digest.update(&width.to_le_bytes());
                }
                LogicalOperationKind::DynamicInsert => {
                    digest.update(&[5]);
                }
                LogicalOperationKind::Register {
                    edge,
                    enable_active_high,
                    resets,
                } => {
                    digest.update(&[6, *edge as u8, option_bool_tag(*enable_active_high)]);
                    hash_resets(digest, resets);
                }
                LogicalOperationKind::Latch {
                    enable_active_high,
                    resets,
                } => {
                    digest.update(&[7, u8::from(*enable_active_high)]);
                    hash_resets(digest, resets);
                }
            }
            for operand in &operation.operands {
                hash_word_type(digest, *operand);
            }
            digest.update(&[0xff]);
            hash_word_type(digest, operation.result);
        }
        LogicalCell::Memory {
            element,
            depth,
            interface,
        } => {
            digest.update(&[1]);
            hash_word_type(digest, *element);
            digest.update(&depth.to_le_bytes());
            digest.update(interface);
        }
        LogicalCell::Connection => {
            digest.update(&[2]);
        }
    }
}

fn hash_resets(digest: &mut blake3::Hasher, resets: &[LogicalReset]) {
    digest.update(&(resets.len() as u64).to_le_bytes());
    for reset in resets {
        digest.update(&[reset.kind as u8, u8::from(reset.active_high)]);
    }
}

fn hash_word_type(digest: &mut blake3::Hasher, ty: word::WordType) {
    digest.update(&ty.width().to_le_bytes());
    digest.update(&[
        u8::from(ty.is_signed()),
        match ty.state() {
            word::LogicStateKind::TwoState => 0,
            word::LogicStateKind::FourState => 1,
        },
    ]);
}

const fn option_bool_tag(value: Option<bool>) -> u8 {
    match value {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}

const fn bit_value_tag(value: opto_ir::BitVal) -> u8 {
    match value {
        opto_ir::BitVal::Zero => 0,
        opto_ir::BitVal::One => 1,
        opto_ir::BitVal::X => 2,
        opto_ir::BitVal::Z => 3,
    }
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

fn memory_cell_id(
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

fn operation_net_id(
    regions: &crate::SynthesisRegionGraph,
    operation: word::OpId,
    bit: u32,
) -> Result<NetBitId, crate::SynthError> {
    let cell = operation_cell_id(regions, operation)?;
    Ok(net_id(
        b"opto/logical-operation-net/v1\0",
        cell.bytes(),
        bit,
    ))
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
    let value = match value {
        opto_ir::BitVal::Zero => 0,
        opto_ir::BitVal::One => 1,
        opto_ir::BitVal::X => 2,
        opto_ir::BitVal::Z => 3,
    };
    let state = match state {
        word::LogicStateKind::TwoState => 0,
        word::LogicStateKind::FourState => 1,
    };
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/logical-constant-net/v1\0");
    digest.update(&[value, state]);
    NetBitId::from_bytes(*digest.finalize().as_bytes())
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

        assert_eq!(work.design.0.revision(), design.0.revision());
        assert!(design.0.cell_count() >= regions.regions().len());
        assert_eq!(work.items.len(), regions.regions().len());
        assert_eq!(work.packet_tasks().len(), regions.regions().len());
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(work.item_regions[index], region.row());
            for &operation in regions.operations(region) {
                let _ = logical_operation(&module, operation).unwrap();
                assert!(
                    design
                        .0
                        .cell(operation_cell_id(&regions, operation).unwrap())
                        .is_some()
                );
            }
        }
        let semantic_items = work.items.iter().map(|item| item.id).collect::<Vec<_>>();
        let execute = |work: &WorkGraph| {
            let results = SynthesisExecutor::execute(&runtime, work.packet_tasks(), |item, _| {
                Ok(WorkProduct {
                    proof: opto_ir::design::EquivalenceCertificate {
                        regime: opto_ir::design::EquivalenceRegime::ByConstruction,
                        digest: item.id.0,
                    },
                    output: item.id,
                })
            })
            .unwrap();
            work.accept_results(results)
                .unwrap()
                .into_vec()
                .into_iter()
                .map(|result| result.output)
                .collect::<Vec<_>>()
        };
        assert_eq!(execute(&work), semantic_items);
        let mut invalid = SynthesisExecutor::execute(&runtime, work.packet_tasks(), |item, _| {
            Ok(WorkProduct {
                proof: opto_ir::design::EquivalenceCertificate {
                    regime: opto_ir::design::EquivalenceRegime::ByConstruction,
                    digest: item.id.0,
                },
                output: item.id,
            })
        })
        .unwrap();
        invalid[0].footprint.replaces = EntitySet::new(vec![]).unwrap();
        assert!(work.accept_results(invalid).is_err());
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
        assert_eq!(serial_work.item_regions, work.item_regions);
        assert!(
            serial_work
                .items
                .iter()
                .zip(&work.items)
                .all(|(left, right)| {
                    left.id == right.id
                        && left.core == right.core
                        && left.halo == right.halo
                        && left.context == right.context
                        && left.estimated_work == right.estimated_work
                        && left.estimated_memory == right.estimated_memory
                })
        );
        work.rebatch(2).unwrap();
        assert_eq!(
            work.packet_tasks().len(),
            regions.regions().len().div_ceil(2)
        );
        assert_eq!(
            work.shards
                .iter()
                .flat_map(|shard| shard.items.iter().map(|&item| work.items[item].id))
                .collect::<Vec<_>>(),
            semantic_items
        );
        assert_eq!(execute(&work), semantic_items);
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(work.item_regions[index], region.row());
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
            assert!(matches!(
                design.0.net(net).and_then(|net| net.driver),
                Some(NetDriver::Cell { .. })
            ));
            (net, design.0.revision())
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

        assert_eq!(fine.0.revision(), coarse.0.revision());
        assert!(same_design(&fine.0, &coarse.0));
    }
}
