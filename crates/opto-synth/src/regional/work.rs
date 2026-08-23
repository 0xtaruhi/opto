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
#[cfg(test)]
use std::collections::BTreeSet;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const WORK_TASK_DOMAIN: u32 = 0x574f_524b;
const COARSE_GROUP_SHARDS: usize = 16;

#[derive(Default)]
struct NetTable {
    nets: Vec<NetBit>,
    rows: HashMap<NetBitId, usize>,
}

impl NetTable {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            nets: Vec::with_capacity(capacity),
            rows: HashMap::with_capacity(capacity),
        }
    }

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LogicalOperation {
    kind: LogicalOperationKind,
    operands: Box<[word::WordType]>,
    result: word::WordType,
    dependencies: Option<Box<LogicalDependencyRefinement>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LogicalDependencyRefinement {
    known_outputs: Box<[u64]>,
    mux_branch: Option<bool>,
    exact: Option<opto_core::PackedRows<LogicalDependencyRange>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct LogicalDependencyRange {
    start: u32,
    width: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct LogicalReset {
    kind: word::ResetKind,
    active_high: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    fn visit_comb_dependencies(
        &self,
        output: usize,
        input_count: usize,
        visit: &mut dyn FnMut(usize),
    ) {
        match self {
            Self::Operation(operation) => operation.visit_dependencies(output, visit),
            Self::Connection => visit(0),
            Self::Memory { .. } => {
                let _ = (output, input_count, visit);
            }
        }
    }
}

impl LogicalOperation {
    fn visit_dependencies(&self, output: usize, visit: &mut dyn FnMut(usize)) {
        let refinement = self.dependencies.as_deref();
        if refinement
            .and_then(|dependencies| dependencies.known_outputs.get(output / u64::BITS as usize))
            .is_some_and(|word| word & (1 << (output % u64::BITS as usize)) != 0)
        {
            return;
        }
        if let Some(rows) = refinement.and_then(|dependencies| dependencies.exact.as_ref()) {
            let Some(range) = rows.row_range(output) else {
                return;
            };
            for encoded in &rows.values()[range] {
                for input in encoded.start..encoded.start + encoded.width {
                    visit(input as usize);
                }
            }
            return;
        }
        let all = |visit: &mut dyn FnMut(usize)| {
            for input in 0..self.operands.iter().map(|ty| ty.width() as usize).sum() {
                visit(input);
            }
        };
        match self.kind {
            LogicalOperationKind::Unary(word::UnaryOp::BitNot) => {
                self.visit_operand_bit(0, output, false, visit);
            }
            LogicalOperationKind::Binary(
                word::BinaryOp::BitAnd | word::BinaryOp::BitOr | word::BinaryOp::BitXor,
            ) => {
                self.visit_operand_bit(0, output, true, visit);
                self.visit_operand_bit(1, output, true, visit);
            }
            LogicalOperationKind::Mux => {
                if let Some(branch) = refinement.and_then(|dependencies| dependencies.mux_branch) {
                    self.visit_operand_bit(if branch { 1 } else { 2 }, output, false, visit);
                } else {
                    self.visit_operand(0, visit);
                    self.visit_operand_bit(1, output, false, visit);
                    self.visit_operand_bit(2, output, false, visit);
                }
            }
            LogicalOperationKind::TriState { .. } => {
                self.visit_operand_bit(0, output, false, visit);
                self.visit_operand(1, visit);
            }
            LogicalOperationKind::Concat => {
                let mut result_lsb = 0usize;
                for operand in (0..self.operands.len()).rev() {
                    let width = self.operands[operand].width() as usize;
                    if (result_lsb..result_lsb + width).contains(&output) {
                        self.visit_operand_bit(operand, output - result_lsb, false, visit);
                        break;
                    }
                    result_lsb += width;
                }
            }
            LogicalOperationKind::Extract { lsb, .. } => {
                self.visit_operand_bit(0, lsb as usize + output, false, visit);
            }
            LogicalOperationKind::Cast(kind) => {
                let width = self.operands[0].width() as usize;
                if output < width {
                    self.visit_operand_bit(0, output, false, visit);
                } else if kind == word::CastKind::SignExtend {
                    self.visit_operand_bit(0, width - 1, false, visit);
                }
            }
            LogicalOperationKind::Register { .. } | LogicalOperationKind::Latch { .. } => {}
            LogicalOperationKind::Unary(_)
            | LogicalOperationKind::Binary(_)
            | LogicalOperationKind::DynamicExtract { .. }
            | LogicalOperationKind::DynamicInsert => all(visit),
        }
    }

    fn visit_operand(&self, operand: usize, visit: &mut dyn FnMut(usize)) {
        let start = self.operands[..operand]
            .iter()
            .map(|ty| ty.width() as usize)
            .sum::<usize>();
        for input in start..start + self.operands[operand].width() as usize {
            visit(input);
        }
    }

    fn visit_operand_bit(
        &self,
        operand: usize,
        bit: usize,
        extend: bool,
        visit: &mut dyn FnMut(usize),
    ) {
        let width = self.operands[operand].width() as usize;
        let bit = if bit < width {
            Some(bit)
        } else if extend && self.operands[operand].is_signed() {
            Some(width - 1)
        } else {
            None
        };
        if let Some(bit) = bit {
            let start = self.operands[..operand]
                .iter()
                .map(|ty| ty.width() as usize)
                .sum::<usize>();
            visit(start + bit);
        }
    }
}

#[derive(Debug, Clone)]
/// Canonical immutable macro design consumed by every regional work epoch.
pub(crate) struct WorkDesign(DesignRevision<LogicalCell>);

#[derive(Debug)]
pub(crate) struct WorkItem {
    id: WorkItemId,
    kind: WorkItemKind,
    core: EntitySet,
    halo: EntitySet,
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
    core: EntitySet,
    halo: EntitySet,
    context: WorkContext,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkPacketDesign {
    revision: DesignRevisionId,
    cells: Box<[Cell<LogicalCell>]>,
    nets: Box<[NetBit]>,
}

#[cfg(test)]
impl WorkPacketDesign {
    fn validate(&self, items: &[WorkPacketItem]) -> Result<(), crate::SynthError> {
        if self.cells.windows(2).any(|pair| pair[0].id >= pair[1].id)
            || self.nets.windows(2).any(|pair| pair[0].id >= pair[1].id)
        {
            return Err(crate::SynthError::invariant(
                "work packet design records are not in stable identity order",
            ));
        }
        let cells = self
            .cells
            .iter()
            .map(|cell| cell.id)
            .collect::<BTreeSet<_>>();
        let nets = self.nets.iter().map(|net| net.id).collect::<BTreeSet<_>>();
        let halo_nets = items
            .iter()
            .flat_map(|item| item.halo.as_slice())
            .filter_map(|entity| match *entity {
                EntityId::NetBit(net) => Some(net),
                EntityId::Cell(_) => None,
            })
            .collect::<BTreeSet<_>>();
        if items.iter().any(|item| {
            item.core
                .as_slice()
                .iter()
                .chain(item.halo.as_slice())
                .any(|entity| match *entity {
                    EntityId::Cell(cell) => !cells.contains(&cell),
                    EntityId::NetBit(net) => !nets.contains(&net),
                })
        }) {
            return Err(crate::SynthError::invariant(
                "work packet omits a core or halo design record",
            ));
        }
        for cell in &self.cells {
            if cell
                .inputs
                .iter()
                .chain(&cell.outputs)
                .any(|net| !nets.contains(net))
            {
                return Err(crate::SynthError::invariant(
                    "work packet cell references a net outside its fragment",
                ));
            }
            for (output, &net) in cell.outputs.iter().enumerate() {
                let output = u32::try_from(output)
                    .map_err(|_| crate::SynthError::capacity("packet cell output ordinal"))?;
                if self
                    .nets
                    .binary_search_by_key(&net, |candidate| candidate.id)
                    .ok()
                    .and_then(|index| self.nets[index].driver)
                    != Some(NetDriver::Cell {
                        cell: cell.id,
                        output,
                    })
                {
                    return Err(crate::SynthError::invariant(
                        "work packet cell and output net disagree",
                    ));
                }
            }
        }
        if self.nets.iter().any(|net| {
            matches!(net.driver, Some(NetDriver::Cell { cell, .. }) if !cells.contains(&cell))
                && !halo_nets.contains(&net.id)
        }) {
            return Err(crate::SynthError::invariant(
                "work packet core net has a driver outside its fragment",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkPacket {
    schema: u32,
    design: DesignRevisionId,
    shard: CompilationShardId,
    fragment: WorkPacketDesign,
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
    footprint: RevisionFootprint,
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
        let revision = &design.0;
        let anchors = LogicalAnchors::new(module, regions.as_ref())?;
        let connectivity = crate::word::bit_connectivity::BitConnectivity::new(module)?;
        let rows = runtime.analyze_indexed(regions.regions().len(), |index| {
            let region = regions.regions()[index];
            let cells = region_cells(module, regions.as_ref(), &anchors, region)?;
            let mut core = region_value_entities(
                module,
                &anchors,
                regions.as_ref(),
                &connectivity,
                region,
                crate::RegionPortDirection::Output,
                revision,
            )?;
            let mut halo = region_value_entities(
                module,
                &anchors,
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
                let fragment = self
                    .packet_design(shard, &items)
                    .expect("validated work shard resolves its exact design fragment");
                WorkPacket {
                    schema: 1,
                    design: self.design.0.revision(),
                    shard: shard.id,
                    fragment,
                    items,
                    estimated_work: shard.estimated_work,
                    estimated_memory: shard.estimated_memory,
                }
            })
            .collect()
    }

    #[cfg(test)]
    fn packet_design(
        &self,
        shard: &CompilationShard,
        items: &[WorkPacketItem],
    ) -> Result<WorkPacketDesign, crate::SynthError> {
        let entities = shard
            .items
            .iter()
            .flat_map(|&row| {
                self.items[row]
                    .core
                    .as_slice()
                    .iter()
                    .chain(self.items[row].halo.as_slice())
            })
            .copied()
            .collect::<BTreeSet<_>>();
        let cells = entities
            .iter()
            .filter_map(|entity| match *entity {
                EntityId::Cell(id) => Some(
                    self.design
                        .0
                        .cell(id)
                        .cloned()
                        .ok_or_else(|| crate::SynthError::invariant("packet cell is not live")),
                ),
                EntityId::NetBit(_) => None,
            })
            .collect::<Result<_, _>>()?;
        let nets = entities
            .iter()
            .filter_map(|entity| match *entity {
                EntityId::NetBit(id) => Some(
                    self.design
                        .0
                        .net(id)
                        .cloned()
                        .ok_or_else(|| crate::SynthError::invariant("packet net is not live")),
                ),
                EntityId::Cell(_) => None,
            })
            .collect::<Result<_, _>>()?;
        let fragment = WorkPacketDesign {
            revision: self.design.0.revision(),
            cells,
            nets,
        };
        fragment.validate(items)?;
        Ok(fragment)
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
            if result.artifact.footprint.base != self.design.0.revision()
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
                            base: work.design.0.revision(),
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

    pub(crate) const fn design(&self) -> &DesignRevision<LogicalCell> {
        &self.0
    }

    pub(crate) const fn from_revision(design: DesignRevision<LogicalCell>) -> Self {
        Self(design)
    }
}

/// Lowers published static-wire coalescing fragments into bit-level revision
/// deltas against the sealed base generation.
///
/// Every fragment replaces the connection cells of one candidate wire so each
/// signal bit is driven from its coalesced value, and installs the appended
/// extract/concat/cast cells those inputs read. Stable identities reuse the
/// sealing recipes over the spliced module, so a fresh seal of the same module
/// reproduces exactly these entities.
pub(crate) fn coalesce_revision_deltas(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    base: &DesignRevision<LogicalCell>,
    published: &word::PublishedWave,
    signals: &[(word::FragmentKey, word::SignalId)],
) -> Result<Vec<opto_ir::design::RewriteDelta<LogicalCell>>, crate::SynthError> {
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
    let mut known_bits = word::KnownBitsAnalysis::new(module);
    let mut unsigned_values = word::UnsignedValueAnalysis::new(module);
    let mut deltas = Vec::with_capacity(published.entries().len());
    for (entry, &(_, signal)) in published.entries().iter().zip(signals) {
        let stored_signal = module
            .signal(signal)
            .ok_or_else(|| crate::SynthError::invariant("coalesced candidate wire disappeared"))?;
        let width = stored_signal.ty.width();
        let state = stored_signal.ty.state();
        let source_span = stored_signal.source.clone();
        let signal_anchor = signal_anchor(module, signal)?;
        let mut nets = NetTable::default();
        let mut connection_cells = Vec::<Cell<LogicalCell>>::new();
        let mut replaces = Vec::<EntityId>::new();
        let mut existing_inputs = std::collections::BTreeSet::new();
        for bit in 0..width {
            let Some(source) = connectivity.signal_source(signal, bit)? else {
                return Err(crate::SynthError::invariant(format!(
                    "coalesced wire lost the driver for bit {bit}"
                )));
            };
            let input_net = source_net(module, &anchors, source, state, &mut nets)?;
            if base.net(input_net).is_some() {
                existing_inputs.insert(input_net);
            }
            let cell = connection_cell_id(signal_anchor, bit);
            replaces.push(EntityId::Cell(cell));
            connection_cells.push(Cell {
                id: cell,
                kind: LogicalCell::Connection,
                class: CellClass::Combinational,
                inputs: Box::new([input_net]),
                outputs: Box::new([signal_net_id(signal_anchor, bit)]),
                source: source_span.clone(),
            });
        }
        let mut operation_cells = Vec::<Cell<LogicalCell>>::with_capacity(entry.operations().len());
        for &operation in entry.operations() {
            let kind = logical_operation(module, operation, &mut known_bits, &mut unsigned_values)?;
            let stored = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("published coalescing operation disappeared")
            })?;
            let cell = anchors.operation(operation)?;
            let inputs = crate::word::operation_inputs(&stored.kind)
                .into_iter()
                .map(|value| value_nets(module, &anchors, &connectivity, value, &mut nets))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Box<[_]>>();
            for &input in &inputs {
                if base.net(input).is_some() {
                    existing_inputs.insert(input);
                }
            }
            let outputs = operation_outputs(module, &anchors, operation, &mut nets)?;
            for (output, &net) in outputs.iter().enumerate() {
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
        // Only genuinely new nets may enter the fragment; producer and
        // boundary nets already live in the base generation.
        let nets = nets
            .into_nets()
            .into_iter()
            .filter(|net| base.net(net.id).is_none())
            .collect::<Vec<_>>();
        let mut cells = operation_cells;
        cells.extend(connection_cells);
        let mut reads = replaces.clone();
        reads.extend(existing_inputs.iter().copied().map(EntityId::NetBit));
        deltas.push(RewriteDelta {
            id: RewriteDeltaId::from_bytes(entry.key().bytes()),
            footprint: RevisionFootprint {
                base: base.revision(),
                reads: EntitySet::new(reads).map_err(|error| design_error(&error))?,
                replaces: EntitySet::new(replaces).map_err(|error| design_error(&error))?,
            },
            cells: cells.into_boxed_slice(),
            nets: nets.into_boxed_slice(),
            semantic: SemanticBinding {
                inputs: existing_inputs.into_iter().collect::<Box<[_]>>(),
                outputs: Box::new([]),
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
    delta: &opto_ir::design::RewriteDelta<LogicalCell>,
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

fn coalesce_proof_digest(key: opto_ir::word::FragmentKey) -> [u8; 32] {
    *blake3::Hasher::new()
        .update(b"opto/dataflow-coalesce-proof/v1\0")
        .update(&key.bytes())
        .finalize()
        .as_bytes()
}

fn seal_logical_design(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
) -> Result<DesignRevision<LogicalCell>, crate::SynthError> {
    let signal_bits = module.signals().iter().fold(0usize, |total, signal| {
        total.saturating_add(signal.ty.width() as usize)
    });
    let operation_bits = module.operations().iter().fold(0usize, |total, operation| {
        total.saturating_add(
            module
                .value(operation.result)
                .map_or(0, |value| value.ty.width() as usize),
        )
    });
    let mut nets = NetTable::with_capacity(signal_bits.saturating_add(operation_bits));
    let mut cells = Vec::with_capacity(
        module
            .operations()
            .len()
            .saturating_add(module.memories().len())
            .saturating_add(signal_bits),
    );
    let anchors = LogicalAnchors::new(module, regions)?;
    let memory_ports = MemoryPortIndex::new(module);
    let connectivity = crate::word::bit_connectivity::BitConnectivity::new(module)?;
    let mut known_bits = word::KnownBitsAnalysis::new(module);
    let mut unsigned_values = word::UnsignedValueAnalysis::new(module);
    for (index, signal) in module.signals().iter().enumerate() {
        let signal_id = word::SignalId::from_index(index).map_err(crate::SynthError::from)?;
        let anchor = anchors.signal(signal_id)?;
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
            let kind = logical_operation(module, operation, &mut known_bits, &mut unsigned_values)?;
            let stored = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("logical revision references an unknown operation")
            })?;
            let cell = anchors.operation(operation)?;
            let inputs = crate::word::operation_inputs(&stored.kind)
                .into_iter()
                .map(|value| value_nets(module, &anchors, &connectivity, value, &mut nets))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Box<[_]>>();
            let outputs = operation_outputs(module, &anchors, operation, &mut nets)?;
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
            let cell = logical_memory_cell_id(module, memory)?;
            let (inputs, outputs) = memory_nets(
                module,
                &anchors,
                &connectivity,
                &memory_ports,
                memory,
                &mut nets,
            )?;
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
            value_nets(module, &anchors, &connectivity, value, &mut nets)?;
        }
        for flow in regions.bit_flows(region) {
            value_nets(module, &anchors, &connectivity, flow.value(), &mut nets)?;
        }
    }
    for (index, signal) in module.signals().iter().enumerate() {
        let signal_id = word::SignalId::from_index(index).map_err(crate::SynthError::from)?;
        let anchor = anchors.signal(signal_id)?;
        for bit in 0..signal.ty.width() {
            let Some(source) = connectivity.signal_source(signal_id, bit)? else {
                continue;
            };
            let input = source_net(module, &anchors, source, signal.ty.state(), &mut nets)?;
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
    let mut nets = nets.into_nets();
    nets.sort_unstable_by_key(|net| net.id);
    let revision = logical_revision_id(&cells, &nets);
    let mut builder = DesignBuilder::new(revision);
    for net in nets {
        builder.add_net(net);
    }
    for cell in cells {
        builder.add_cell(cell);
    }
    builder.seal().map_err(|error| design_error(&error))
}

fn operation_outputs(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    operation: word::OpId,
    nets: &mut NetTable,
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
            let id = operation_net_id(anchors.operation(cell)?, bit);
            install_net(nets, id, result.ty.state(), None)?;
            Ok(id)
        })
        .collect()
}

fn region_value_entities(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
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
    let mut scratch = NetTable::default();
    let mut entities = std::collections::BTreeSet::new();
    for &port in ports {
        let value = regions
            .port(port)
            .ok_or_else(|| crate::SynthError::invariant("work item boundary port is unknown"))?
            .value();
        for net in value_nets(module, anchors, connectivity, value, &mut scratch)? {
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
            let value = value_nets(module, anchors, connectivity, flow.value(), &mut scratch)?;
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
    anchors: &LogicalAnchors,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    ports: &MemoryPortIndex,
    memory: word::MemoryId,
    nets: &mut NetTable,
) -> Result<MemoryNetBinding, crate::SynthError> {
    let reads = ports.reads.get(memory.index()).ok_or_else(|| {
        crate::SynthError::invariant("logical memory read-port row is out of range")
    })?;
    let writes = ports.writes.get(memory.index()).ok_or_else(|| {
        crate::SynthError::invariant("logical memory write-port row is out of range")
    })?;
    let mut inputs = Vec::new();
    for &row in reads {
        let read = &module.memory_read_ports()[row];
        append_value_nets(
            &mut inputs,
            module,
            anchors,
            connectivity,
            read.address,
            nets,
        )?;
        if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = read.timing {
            append_value_nets(
                &mut inputs,
                module,
                anchors,
                connectivity,
                clock.value,
                nets,
            )?;
            if let Some(enable) = enable {
                append_value_nets(
                    &mut inputs,
                    module,
                    anchors,
                    connectivity,
                    enable.value,
                    nets,
                )?;
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
            append_value_nets(&mut inputs, module, anchors, connectivity, value, nets)?;
        }
    }
    let outputs = reads
        .iter()
        .map(|&row| signal_nets(module, anchors, module.memory_read_ports()[row].data, nets))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok((inputs.into_boxed_slice(), outputs))
}

fn append_value_nets(
    output: &mut Vec<NetBitId>,
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    value: word::ValueId,
    nets: &mut NetTable,
) -> Result<(), crate::SynthError> {
    output.extend(value_nets(module, anchors, connectivity, value, nets)?);
    Ok(())
}

fn signal_nets(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    signal: word::SignalId,
    nets: &mut NetTable,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    let anchor = anchors.signal(signal)?;
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
    known_bits: &mut word::KnownBitsAnalysis,
    unsigned_values: &mut word::UnsignedValueAnalysis,
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
    let operand_values = crate::word::operation_inputs(&stored.kind);
    let operands: Box<[word::WordType]> = operand_values
        .iter()
        .map(|&value| {
            module.value(value).map(|value| value.ty).ok_or_else(|| {
                crate::SynthError::invariant("logical operation has an unknown operand")
            })
        })
        .collect::<Result<_, _>>()?;
    let result = module
        .value(stored.result)
        .map(|value| value.ty)
        .ok_or_else(|| crate::SynthError::invariant("logical operation has an unknown result"))?;
    let mut known_outputs = vec![0u64; (result.width() as usize).div_ceil(u64::BITS as usize)];
    for bit in 0..result.width() {
        if known_bits.bit(module, stored.result, bit) != word::KnownBit::Unknown {
            known_outputs[bit as usize / u64::BITS as usize] |= 1 << (bit % u64::BITS);
        }
    }
    while known_outputs.last() == Some(&0) {
        known_outputs.pop();
    }
    let mux_branch = match stored.kind {
        word::OpKind::Mux { cond, .. } => match known_bits.bit(module, cond, 0) {
            word::KnownBit::Zero => Some(false),
            word::KnownBit::One => Some(true),
            word::KnownBit::Unknown => None,
        },
        _ => None,
    };
    let exact = if matches!(stored.kind, word::OpKind::DynamicExtract { .. }) {
        let mut rows = opto_core::PackedRowsBuilder::<LogicalDependencyRange>::try_with_capacity(
            result.width() as usize,
            0,
        )
        .map_err(|_| crate::SynthError::capacity("logical dependency range directory"))?;
        for output in 0..result.width() {
            let mut ranges = Vec::new();
            if known_bits.bit(module, stored.result, output) == word::KnownBit::Unknown {
                for dependency in crate::word::cycle::operation_dependencies(
                    module,
                    known_bits,
                    unsigned_values,
                    &stored.kind,
                    crate::word::cycle::ValueSlice {
                        value: stored.result,
                        lsb: output,
                        width: 1,
                    },
                )? {
                    let operand = operand_values
                        .iter()
                        .position(|&value| value == dependency.value)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "logical dependency is not a direct operation input",
                            )
                        })?;
                    let start = operands[..operand]
                        .iter()
                        .try_fold(0u32, |total, ty| total.checked_add(ty.width()))
                        .ok_or_else(|| {
                            crate::SynthError::capacity("logical dependency operand width")
                        })?
                        .checked_add(dependency.lsb)
                        .ok_or_else(|| {
                            crate::SynthError::capacity("logical dependency input offset")
                        })?;
                    ranges.push(LogicalDependencyRange {
                        start,
                        width: dependency.width,
                    });
                }
            }
            rows.try_push_row(ranges)
                .map_err(|_| crate::SynthError::capacity("logical dependency range directory"))?;
        }
        Some(rows.finish())
    } else {
        None
    };
    let dependencies =
        (!known_outputs.is_empty() || mux_branch.is_some() || exact.is_some()).then(|| {
            Box::new(LogicalDependencyRefinement {
                known_outputs: known_outputs.into_boxed_slice(),
                mux_branch,
                exact,
            })
        });
    Ok(LogicalOperation {
        kind,
        operands,
        result,
        dependencies,
    })
}

fn region_cells(
    module: &word::WordModule,
    regions: &crate::SynthesisRegionGraph,
    anchors: &LogicalAnchors,
    region: crate::SynthesisRegion,
) -> Result<Box<[CellId]>, crate::SynthError> {
    let mut cells = Vec::new();
    for &operation in regions.operations(region) {
        cells.push(anchors.operation(operation)?);
    }
    for &memory in regions.memories(region) {
        cells.push(logical_memory_cell_id(module, memory)?);
    }
    cells.sort_unstable();
    cells.dedup();
    Ok(cells.into_boxed_slice())
}

fn value_nets(
    module: &word::WordModule,
    anchors: &LogicalAnchors,
    connectivity: &crate::word::bit_connectivity::BitConnectivity<'_>,
    value: word::ValueId,
    nets: &mut NetTable,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    let stored = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("logical net references an unknown value"))?;
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
    nets: &mut NetTable,
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
                    operation_net_id(anchors.operation(operation)?, bit),
                    source.ty.state(),
                    None,
                ),
                word::ValueKind::Signal(reference) => {
                    let physical = reference
                        .lsb
                        .checked_add(bit)
                        .ok_or_else(|| crate::SynthError::capacity("logical signal bit offset"))?;
                    (
                        signal_net_id(anchors.signal(reference.signal)?, physical),
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
    nets: &mut NetTable,
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
    nets: &mut NetTable,
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

fn logical_revision_id(cells: &[Cell<LogicalCell>], nets: &[NetBit]) -> DesignRevisionId {
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
    for net in nets {
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
        assert_eq!(work.shards.len(), regions.regions().len());
        let mut known_bits = word::KnownBitsAnalysis::new(&module);
        let mut unsigned_values = word::UnsignedValueAnalysis::new(&module);
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(
                work.items[index].kind,
                WorkItemKind::FixedLogic(region.id())
            );
            for &operation in regions.operations(region) {
                let _ =
                    logical_operation(&module, operation, &mut known_bits, &mut unsigned_values)
                        .unwrap();
                assert!(
                    design
                        .0
                        .cell(operation_cell_id(&regions, operation).unwrap())
                        .is_some()
                );
            }
        }
        for packet in work.portable_packets() {
            let bytes = opto_archive::to_bytes(&packet).unwrap();
            let restored: WorkPacket = opto_archive::from_bytes(&bytes).unwrap();
            assert_eq!(restored.design, packet.design);
            assert_eq!(restored.fragment, packet.fragment);
            restored.fragment.validate(&restored.items).unwrap();
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
        invalid[0].artifact.footprint.replaces = EntitySet::new(vec![]).unwrap();
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
            coalesce_revision_deltas(&module, &regions, base.design(), &published, &signals)
                .unwrap();
        assert_eq!(deltas.len(), signals.len());
        let committed = base
            .design()
            .commit(deltas, validate_coalesce_proof)
            .map_err(|error| design_error(&error))
            .unwrap();

        // The published generation must reproduce exactly what sealing the
        // spliced module from scratch would produce.
        let fresh = WorkDesign::seal(&module, &regions).unwrap();
        let cells = |design: &DesignRevision<LogicalCell>| {
            let mut stored = design.cells().cloned().collect::<Vec<_>>();
            stored.sort_unstable_by_key(|cell| cell.id);
            stored
        };
        let nets = |design: &DesignRevision<LogicalCell>| {
            let mut stored = design.nets().cloned().collect::<Vec<_>>();
            stored.sort_unstable_by_key(|net| net.id);
            stored
        };
        assert_eq!(cells(&committed), cells(fresh.design()));
        assert_eq!(nets(&committed), nets(fresh.design()));
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
        let before = base.design().clone();

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
            coalesce_revision_deltas(&module, &regions, base.design(), &published, &signals)
                .unwrap();
        // Corrupt one certificate so the wave must be rejected wholesale.
        deltas[0].proof = EquivalenceCertificate {
            regime: EquivalenceRegime::ByConstruction,
            digest: [0u8; 32],
        };
        assert!(
            base.design()
                .commit(deltas, validate_coalesce_proof)
                .is_err()
        );
        // Byte-identity is observable through identical live content: every
        // surviving cell and net must still resolve to the same record.
        let after = base.design().clone();
        assert_eq!(after.cell_count(), before.cell_count());
        assert_eq!(after.net_count(), before.net_count());
        for (fresh, frozen) in after.cells().zip(before.cells()) {
            assert_eq!(fresh.id, frozen.id);
            assert_eq!(fresh.inputs, frozen.inputs);
            assert_eq!(fresh.outputs, frozen.outputs);
        }
        for (fresh, frozen) in after.nets().zip(before.nets()) {
            assert_eq!(fresh.id, frozen.id);
            assert_eq!(fresh.driver, frozen.driver);
        }
    }
}
