// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable revision and execution rows derived from one sealed region graph.

use opto_ir::design::{
    Cell, CellClass, CellId, DesignBuilder, DesignRevision, DesignRevisionId, EntityId, EntitySet,
    EquivalenceCertificate, EquivalenceRegime, NetBit, NetBitId, NetDriver, RewriteDelta,
    RewriteDeltaId, SemanticBinding,
};
use opto_ir::word::SourceSpan;
use opto_runtime::{Task, TaskKey};
use std::collections::BTreeMap;

const WORK_TASK_DOMAIN: u32 = 0x574f_524b;
const COARSE_GROUP_SHARDS: usize = 16;

macro_rules! digest_id {
    ($name:ident, $doc:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

    fn structural(design: DesignRevisionId, local: [u8; 32]) -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto.structural-work-context.v1\0");
        digest.update(&design.bytes());
        digest.update(&local);
        Self::from_bytes(*digest.finalize().as_bytes())
    }
}

impl From<crate::RegionContextKey> for WorkContextKey {
    fn from(context: crate::RegionContextKey) -> Self {
        Self::from_bytes(context.bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegionCell {
    pub(crate) region: crate::RegionAnchorId,
    pub(crate) revision: crate::RegionRevision,
    pub(crate) kind: crate::SynthesisRegionKind,
}

#[derive(Debug, Clone)]
/// Canonical immutable macro design consumed by every regional work epoch.
pub(crate) struct WorkDesign(DesignRevision<RegionCell>);

#[derive(Debug)]
pub(crate) struct WorkItem {
    id: WorkItemId,
    core: EntitySet,
    halo: EntitySet,
    context: WorkContextKey,
    kind: WorkItemKind,
    estimated_work: u64,
    estimated_memory: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkItemKind {
    Local,
    Reduce,
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
    design: DesignRevision<RegionCell>,
    items: Box<[WorkItem]>,
    shards: Box<[CompilationShard]>,
    coarse_groups: opto_core::PackedRows<CompilationShardId>,
    predecessors: opto_core::PackedRows<WorkItemId>,
    successors: opto_core::PackedRows<WorkItemId>,
}

/// Dense Word-region rows confined to the adapter invocation that built a graph.
pub(crate) struct WorkBinding(Box<[crate::RegionRowId]>);

impl WorkGraph {
    pub(crate) fn build(
        regions: &crate::SynthesisRegionGraph,
        design: &WorkDesign,
        contexts: &[WorkContextKey],
    ) -> Result<(Self, WorkBinding), crate::SynthError> {
        if contexts.len() != regions.regions().len() {
            return Err(crate::SynthError::invariant(
                "work contexts do not cover the sealed region graph",
            ));
        }
        let expected = seal_region_design(regions)?;
        if !same_design(&design.0, &expected) {
            return Err(crate::SynthError::invariant(
                "work graph and immutable design revision disagree",
            ));
        }
        let design = design.0.clone();
        let rows = regions
            .regions()
            .iter()
            .copied()
            .map(|region| {
                let cell = cell_id(region.id());
                let inputs = region_nets(regions, region, crate::RegionPortDirection::Input)?;
                let outputs = region_nets(regions, region, crate::RegionPortDirection::Output)?;
                let core = EntitySet::new(
                    std::iter::once(EntityId::Cell(cell))
                        .chain(outputs.iter().copied().map(EntityId::NetBit))
                        .collect(),
                )
                .map_err(|error| design_error(&error))?;
                let halo = EntitySet::new(inputs.iter().copied().map(EntityId::NetBit).collect())
                    .map_err(|error| design_error(&error))?;
                let id = work_item_id(design.revision(), region.id());
                let estimated_memory = u64::try_from(core.as_slice().len() + halo.as_slice().len())
                    .unwrap_or(u64::MAX)
                    .max(1);
                let item = WorkItem {
                    id,
                    core,
                    halo,
                    context: contexts[region.row().index()],
                    kind: WorkItemKind::Local,
                    estimated_work: region.estimated_work().max(1),
                    estimated_memory,
                };
                Ok((id, region.row(), item))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let binding = WorkBinding(rows.iter().map(|row| row.1).collect());
        let items = rows.into_iter().map(|row| row.2).collect::<Vec<_>>();
        let dependency_rows = |predecessors: bool| {
            regions
                .regions()
                .iter()
                .copied()
                .map(|region| {
                    let rows = if predecessors {
                        regions.predecessors(region)
                    } else {
                        regions.successors(region)
                    };
                    rows.iter().map(|row| items[row.index()].id).collect()
                })
                .collect::<Vec<Vec<_>>>()
        };
        let predecessors = opto_core::PackedRows::try_from_rows(dependency_rows(true))
            .map_err(|_| crate::SynthError::capacity("work-item predecessors"))?;
        let successors = opto_core::PackedRows::try_from_rows(dependency_rows(false))
            .map_err(|_| crate::SynthError::capacity("work-item successors"))?;
        let mut graph = Self {
            design,
            items: items.into_boxed_slice(),
            shards: Box::new([]),
            coarse_groups: opto_core::PackedRows::try_from_rows(Vec::<Vec<_>>::new())
                .map_err(|_| crate::SynthError::capacity("coarse compilation groups"))?,
            predecessors,
            successors,
        };
        graph.rebatch(1)?;
        graph.validate()?;
        Ok((graph, binding))
    }

    pub(crate) fn build_structural(
        regions: &crate::SynthesisRegionGraph,
        design: &WorkDesign,
    ) -> Result<Self, crate::SynthError> {
        let contexts = regions
            .regions()
            .iter()
            .map(|region| {
                WorkContextKey::structural(design.0.revision(), region.revision().bytes())
            })
            .collect::<Vec<_>>();
        let (mut graph, _) = Self::build(regions, design, &contexts)?;
        let core = EntitySet::new(
            graph
                .design
                .cells()
                .map(|cell| EntityId::Cell(cell.id))
                .chain(graph.design.nets().map(|net| EntityId::NetBit(net.id)))
                .collect(),
        )
        .map_err(|error| design_error(&error))?;
        if core.is_empty() {
            return Ok(graph);
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto.structural-reduce-item.v1\0");
        digest.update(&graph.design.revision().bytes());
        let id = WorkItemId::from_bytes(*digest.finalize().as_bytes());
        let estimated_work = graph.items.iter().fold(0u64, |total, item| {
            total.saturating_add(item.estimated_work)
        });
        let estimated_memory = u64::try_from(core.as_slice().len())
            .unwrap_or(u64::MAX)
            .max(1);
        graph.items = Box::new([WorkItem {
            id,
            core,
            halo: EntitySet::new(Vec::new()).map_err(|error| design_error(&error))?,
            context: WorkContextKey::structural(
                graph.design.revision(),
                regions.revision().bytes(),
            ),
            kind: WorkItemKind::Reduce,
            estimated_work,
            estimated_memory,
        }]);
        graph.predecessors = opto_core::PackedRows::try_from_rows(vec![Vec::new()])
            .map_err(|_| crate::SynthError::capacity("structural reduce predecessors"))?;
        graph.successors = opto_core::PackedRows::try_from_rows(vec![Vec::new()])
            .map_err(|_| crate::SynthError::capacity("structural reduce successors"))?;
        graph.rebatch(1)?;
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
                    id: shard_id(self.design.revision(), indices, &self.items),
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

    pub(crate) fn tasks(&self) -> Vec<Task<usize>> {
        self.shards
            .iter()
            .enumerate()
            .map(|(index, shard)| {
                Task::new(TaskKey::new(WORK_TASK_DOMAIN, index as u64), index)
                    .with_estimated_work(shard.estimated_work)
                    .with_estimated_memory(shard.estimated_memory)
            })
            .collect()
    }

    pub(crate) fn shard_items(&self, shard: usize) -> Option<Vec<(usize, &WorkItem)>> {
        self.shards.get(shard).map(|shard| {
            shard
                .items
                .iter()
                .map(|&item| (item, &self.items[item]))
                .collect()
        })
    }

    fn validate(&self) -> Result<(), crate::SynthError> {
        if self.predecessors.row_count() != self.items.len()
            || self.successors.row_count() != self.items.len()
            || self.coarse_groups.value_count() != self.shards.len()
            || self.items.iter().any(|item| {
                item.core.is_empty()
                    || (item.kind == WorkItemKind::Reduce && !item.halo.is_empty())
                    || item.estimated_memory
                        < u64::try_from(item.core.as_slice().len() + item.halo.as_slice().len())
                            .unwrap_or(u64::MAX)
            })
        {
            return Err(crate::SynthError::invariant(
                "work shards do not match their stable semantic items",
            ));
        }
        let scheduled = self
            .shards
            .iter()
            .flat_map(|shard| shard.items.iter().copied())
            .collect::<Vec<_>>();
        if scheduled != (0..self.items.len()).collect::<Vec<_>>()
            || self.shards.iter().any(|shard| {
                shard.items.is_empty()
                    || shard.id != shard_id(self.design.revision(), &shard.items, &self.items)
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

impl WorkDesign {
    pub(crate) fn seal(regions: &crate::SynthesisRegionGraph) -> Result<Self, crate::SynthError> {
        seal_region_design(regions).map(Self)
    }

    pub(crate) fn rewrite_all(
        &self,
        regions: &crate::SynthesisRegionGraph,
        proof: [u8; 32],
    ) -> Result<Self, crate::SynthError> {
        let expected = seal_region_design(regions)?;
        let reads = EntitySet::new(
            self.0
                .cells()
                .map(|cell| EntityId::Cell(cell.id))
                .chain(self.0.nets().map(|net| EntityId::NetBit(net.id)))
                .collect(),
        )
        .map_err(|error| design_error(&error))?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto.structural-rewrite-delta.v1\0");
        digest.update(&self.0.revision().bytes());
        digest.update(&expected.revision().bytes());
        digest.update(&proof);
        let delta = RewriteDelta {
            id: RewriteDeltaId::from_bytes(*digest.finalize().as_bytes()),
            base: self.0.revision(),
            replaces: reads.clone(),
            reads,
            cells: expected.cells().cloned().collect(),
            nets: expected.nets().cloned().collect(),
            semantic: SemanticBinding {
                inputs: Box::new([]),
                outputs: Box::new([]),
            },
            proof: EquivalenceCertificate {
                regime: EquivalenceRegime::Sequential,
                digest: proof,
            },
        };
        let committed = self
            .0
            .commit(vec![delta], |delta| {
                if delta.proof.regime == EquivalenceRegime::Sequential
                    && delta.proof.digest == proof
                {
                    Ok(())
                } else {
                    Err(opto_ir::design::DesignError::ProofRejected(
                        "structural task returned another proof certificate".to_string(),
                    ))
                }
            })
            .map_err(|error| design_error(&error))?;
        if !same_design(&committed, &expected) {
            return Err(crate::SynthError::invariant(
                "structural rewrite transaction differs from its sealed result",
            ));
        }
        Ok(Self(committed))
    }
}

impl WorkBinding {
    pub(crate) fn region(&self, item: usize) -> Option<crate::RegionRowId> {
        self.0.get(item).copied()
    }
}

fn seal_region_design(
    regions: &crate::SynthesisRegionGraph,
) -> Result<DesignRevision<RegionCell>, crate::SynthError> {
    let revision = DesignRevisionId::from_bytes(regions.revision().bytes());
    let mut builder = DesignBuilder::new(revision);
    let mut nets = BTreeMap::<NetBitId, NetBit>::new();
    for &region in regions.regions() {
        let cell = cell_id(region.id());
        let outputs = region_net_bits(regions, region, crate::RegionPortDirection::Output)?;
        for (output, (id, state)) in outputs.iter().copied().enumerate() {
            let output = u32::try_from(output)
                .map_err(|_| crate::SynthError::capacity("region output bit ordinal"))?;
            if nets
                .insert(
                    id,
                    NetBit {
                        id,
                        state,
                        driver: Some(NetDriver::Cell { cell, output }),
                    },
                )
                .is_some()
            {
                return Err(crate::SynthError::invariant(
                    "one stable region net has multiple producers",
                ));
            }
        }
    }
    for &region in regions.regions() {
        for &(id, state) in &region_net_bits(regions, region, crate::RegionPortDirection::Input)? {
            match nets.entry(id) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(NetBit {
                        id,
                        state,
                        driver: None,
                    });
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if entry.get().state != state =>
                {
                    return Err(crate::SynthError::invariant(
                        "region boundary changes logic state domain",
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
    }
    for net in nets.into_values() {
        builder.add_net(net);
    }
    for &region in regions.regions() {
        builder.add_cell(Cell {
            id: cell_id(region.id()),
            kind: RegionCell {
                region: region.id(),
                revision: region.revision(),
                kind: region.kind(),
            },
            class: if region.kind() == crate::SynthesisRegionKind::Combinational {
                CellClass::Combinational
            } else {
                CellClass::StateBoundary
            },
            inputs: region_nets(regions, region, crate::RegionPortDirection::Input)?,
            outputs: region_nets(regions, region, crate::RegionPortDirection::Output)?,
            source: SourceSpan::stable(region.id().bytes()),
        });
    }
    builder.seal().map_err(|error| design_error(&error))
}

fn same_design(left: &DesignRevision<RegionCell>, right: &DesignRevision<RegionCell>) -> bool {
    left.cell_count() == right.cell_count()
        && left.net_count() == right.net_count()
        && left.cells().all(|cell| right.cell(cell.id) == Some(cell))
        && left.nets().all(|net| right.net(net.id) == Some(net))
}

fn region_nets(
    regions: &crate::SynthesisRegionGraph,
    region: crate::SynthesisRegion,
    direction: crate::RegionPortDirection,
) -> Result<Box<[NetBitId]>, crate::SynthError> {
    Ok(region_net_bits(regions, region, direction)?
        .iter()
        .map(|&(id, _)| id)
        .collect())
}

fn region_net_bits(
    regions: &crate::SynthesisRegionGraph,
    region: crate::SynthesisRegion,
    direction: crate::RegionPortDirection,
) -> Result<Box<[(NetBitId, opto_ir::word::LogicStateKind)]>, crate::SynthError> {
    let ports = match direction {
        crate::RegionPortDirection::Input => regions.input_ports(region),
        crate::RegionPortDirection::Output => regions.output_ports(region),
    };
    let mut nets = Vec::new();
    for &port in ports {
        let port = regions.port(port).ok_or_else(|| {
            crate::SynthError::invariant("work item has an unknown boundary port")
        })?;
        let boundary = if port.peer().is_some() {
            port.semantic_key()
        } else {
            port.stable_id().bytes()
        };
        for bit in 0..port.ty().width() {
            nets.push((net_id(boundary, bit), port.ty().state()));
        }
    }
    Ok(nets.into_boxed_slice())
}

fn digest(domain: &[u8], parts: impl IntoIterator<Item = [u8; 32]>) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(domain);
    for part in parts {
        digest.update(&part);
    }
    *digest.finalize().as_bytes()
}

fn cell_id(region: crate::RegionAnchorId) -> CellId {
    CellId::from_bytes(digest(b"opto/work-region-cell/v1\0", [region.bytes()]))
}

fn net_id(edge: [u8; 32], bit: u32) -> NetBitId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/work-region-net/v1\0");
    digest.update(&edge);
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
        let contexts = (0..regions.regions().len())
            .map(|index| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
                WorkContextKey::from(crate::RegionContextKey::from_bytes_for_test(bytes))
            })
            .collect::<Vec<_>>();

        let design = WorkDesign::seal(&regions).unwrap();
        let reduce = WorkGraph::build_structural(&regions, &design).unwrap();
        assert_eq!(reduce.items.len(), 1);
        assert_eq!(reduce.items[0].kind, WorkItemKind::Reduce);
        assert_eq!(reduce.tasks().len(), 1);
        let (mut work, binding) = WorkGraph::build(&regions, &design, &contexts).unwrap();

        assert_eq!(work.design.cell_count(), regions.regions().len());
        assert_eq!(work.items.len(), regions.regions().len());
        assert_eq!(work.tasks().len(), regions.regions().len());
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(binding.region(index), Some(region.row()));
            let cell = work.design.cell(cell_id(region.id())).unwrap();
            assert_eq!(cell.kind.region, region.id());
            assert_eq!(cell.kind.revision, region.revision());
            assert_eq!(cell.kind.kind, region.kind());
        }
        let semantic_items = work.items.iter().map(|item| item.id).collect::<Vec<_>>();
        work.rebatch(2).unwrap();
        assert_eq!(work.tasks().len(), regions.regions().len().div_ceil(2));
        assert_eq!(
            work.shards
                .iter()
                .flat_map(|shard| shard.items.iter().map(|&item| work.items[item].id))
                .collect::<Vec<_>>(),
            semantic_items
        );
        for (index, &region) in regions.regions().iter().enumerate() {
            assert_eq!(binding.region(index), Some(region.row()));
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
            let region = regions.regions()[0];
            region_nets(&regions, region, crate::RegionPortDirection::Output).unwrap()
        };

        let inverted = build(word::UnaryOp::BitNot);
        let reduced = build(word::UnaryOp::ReductionOr);

        assert_eq!(inverted, reduced);
    }
}
