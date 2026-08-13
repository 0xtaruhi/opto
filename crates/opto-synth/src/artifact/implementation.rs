// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Provenance from source operators to their mapped implementations.
//!
//! Mapping and post-map optimization may replace all cells that originally
//! implemented an operator. [`ImplementationDb`] therefore stores both
//! operator origins and explicit synthesis-region ownership for current mapped
//! cells; post-map rewrites propagate both relations independently.

use crate::{ImplementationCandidateId, OperatorId, RegionAnchorId};
use opto_core::resident;
use opto_ir::mapped::{CellId, MappedGenerationId, MappedNetlist};
use opto_ir::word;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::BTreeMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};

mod edit;
mod ownership;
mod publication;

pub(crate) use edit::ImplementationDelta;
use ownership::{BoundaryEdge, MappedOwnerId, RegionOwnerId, seal_owners};
pub use ownership::{BoundaryEdgeId, MappedCellOwnership};
pub(crate) use ownership::{InitialCellOwner, MappedOwnerImpact};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Dense identifier for one operator implementation region.
pub struct ImplementationRegionId(u32);

impl ImplementationRegionId {
    /// Return the zero-based region index.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Source and recipe provenance for one selected operator implementation.
pub struct ImplementationRegion {
    id: ImplementationRegionId,
    operator: OperatorId,
    candidate: ImplementationCandidateId,
    synthesis_region: RegionAnchorId,
    source_operation: word::OpId,
    source_operations: Box<[word::OpId]>,
    source_lines: Box<[Option<u32>]>,
    source_inputs: Box<[word::ValueId]>,
    source_result: word::ValueId,
    width: u32,
    recipe: Box<str>,
    implementation: Box<str>,
    module: Box<str>,
    mnemonic: Box<str>,
    source_file: Option<Box<str>>,
    source_line: Option<u32>,
    mapped_cells: Vec<CellId>,
}

#[derive(Clone, Copy)]
pub(crate) struct ImplementationRegionMetadata<'a> {
    pub(crate) recipe: &'a str,
    pub(crate) implementation: &'a str,
    pub(crate) module: &'a str,
    pub(crate) mnemonic: &'a str,
    pub(crate) source_file: Option<&'a str>,
    pub(crate) source_line: Option<u32>,
}

#[derive(Clone, Copy)]
pub(crate) struct ImplementationRegionIdentity {
    pub(crate) operator: OperatorId,
    pub(crate) candidate: ImplementationCandidateId,
    pub(crate) synthesis_region: RegionAnchorId,
    pub(crate) source_operation: word::OpId,
    pub(crate) source_result: word::ValueId,
    pub(crate) width: u32,
}

pub(crate) struct ImplementationRegionSource<'a> {
    pub(crate) operations: &'a [word::OpId],
    pub(crate) inputs: Vec<word::ValueId>,
    pub(crate) lines: Vec<Option<u32>>,
}

impl ImplementationRegion {
    pub(crate) fn new(
        raw_id: u32,
        identity: ImplementationRegionIdentity,
        source: ImplementationRegionSource<'_>,
        metadata: ImplementationRegionMetadata<'_>,
        mapped_cells: Vec<CellId>,
    ) -> Self {
        Self {
            id: ImplementationRegionId(raw_id),
            operator: identity.operator,
            candidate: identity.candidate,
            synthesis_region: identity.synthesis_region,
            source_operation: identity.source_operation,
            source_operations: source.operations.into(),
            source_lines: source.lines.into_boxed_slice(),
            source_inputs: source.inputs.into_boxed_slice(),
            source_result: identity.source_result,
            width: identity.width,
            recipe: metadata.recipe.into(),
            implementation: metadata.implementation.into(),
            module: metadata.module.into(),
            mnemonic: metadata.mnemonic.into(),
            source_file: metadata.source_file.map(Into::into),
            source_line: metadata.source_line,
            mapped_cells,
        }
    }

    /// Return this region's dense identifier.
    #[must_use]
    pub fn id(&self) -> ImplementationRegionId {
        self.id
    }

    #[must_use]
    /// Return the semantic operator implemented by this region.
    pub fn operator(&self) -> OperatorId {
        self.operator
    }

    #[must_use]
    /// Return the selected implementation candidate.
    pub fn candidate(&self) -> ImplementationCandidateId {
        self.candidate
    }

    /// Return the stable source synthesis region that owns this operator.
    #[must_use]
    pub const fn synthesis_region(&self) -> RegionAnchorId {
        self.synthesis_region
    }

    #[must_use]
    /// Return the representative source operation for diagnostics.
    pub fn source_operation(&self) -> word::OpId {
        self.source_operation
    }

    /// Return all source operations absorbed by the implementation.
    #[must_use]
    pub fn source_operations(&self) -> &[word::OpId] {
        &self.source_operations
    }

    /// Return source line numbers parallel to [`Self::source_operations`].
    #[must_use]
    pub fn source_lines(&self) -> &[Option<u32>] {
        &self.source_lines
    }

    /// Return every source value entering the implementation region.
    #[must_use]
    pub fn source_inputs(&self) -> &[word::ValueId] {
        &self.source_inputs
    }

    #[must_use]
    /// Return the source value produced by the semantic operator.
    pub fn source_result(&self) -> word::ValueId {
        self.source_result
    }

    /// Return mapped cells currently carrying this operator's provenance.
    #[must_use]
    pub fn mapped_cells(&self) -> &[CellId] {
        &self.mapped_cells
    }

    /// Return the implementation width, including any structural guard bits.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    /// Return the stable recipe class used to construct the implementation.
    pub fn recipe(&self) -> &str {
        &self.recipe
    }

    #[must_use]
    /// Return the selected implementation's user-visible name.
    pub fn implementation_name(&self) -> &str {
        &self.implementation
    }

    #[must_use]
    /// Return the source module containing the implemented operator.
    pub fn module_name(&self) -> &str {
        &self.module
    }

    #[must_use]
    /// Return the source operation mnemonic used in resource reports.
    pub fn operation_mnemonic(&self) -> &str {
        &self.mnemonic
    }

    /// Return the source file when debug locations were available.
    #[must_use]
    pub fn source_file(&self) -> Option<&str> {
        self.source_file.as_deref()
    }

    /// Return the primary source line when debug locations were available.
    #[must_use]
    pub fn source_line(&self) -> Option<u32> {
        self.source_line
    }

    fn compact(&mut self) {
        self.mapped_cells.shrink_to_fit();
    }

    fn owned_memory_bytes(&self) -> usize {
        resident::slice_bytes::<word::OpId>(self.source_operations.len())
            .saturating_add(resident::slice_bytes::<Option<u32>>(
                self.source_lines.len(),
            ))
            .saturating_add(resident::slice_bytes::<word::ValueId>(
                self.source_inputs.len(),
            ))
            .saturating_add(resident::allocation_bytes(self.recipe.len()))
            .saturating_add(resident::allocation_bytes(self.implementation.len()))
            .saturating_add(resident::allocation_bytes(self.module.len()))
            .saturating_add(resident::allocation_bytes(self.mnemonic.len()))
            .saturating_add(
                self.source_file
                    .as_ref()
                    .map_or(0, |file| resident::allocation_bytes(file.len())),
            )
            .saturating_add(resident::slice_bytes::<CellId>(self.mapped_cells.len()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub(crate) struct OriginSetId(pub(crate) u32);

impl OriginSetId {
    pub(crate) const EMPTY: Self = Self(0);
}

type OriginHashIndex = BTreeMap<u64, SmallVec<[OriginSetId; 1]>>;

#[derive(Debug, Serialize, Deserialize)]
/// Bidirectional provenance index for a mapped synthesis artifact.
///
/// Origin sets are interned so cells produced by the same rewrite share one
/// compact operator list. Region and boundary owners use separate interned
/// arenas, so ownership stays explicit without semantic operator provenance.
/// Cell identifiers are interpreted in the mapped netlist stored beside this
/// database in [`crate::SynthesisResult`].
pub struct ImplementationDb {
    /// Runtime-only binding to the mapped ID owner stored beside this database.
    /// Restored checkpoints start unbound and bind during joint validation.
    #[serde(skip)]
    mapped_generation: AtomicU64,
    regions: Box<[ImplementationRegion]>,
    cell_origins: Vec<OriginSetId>,
    origin_offsets: Vec<u32>,
    origin_operators: Vec<OperatorId>,
    origin_ids: OriginHashIndex,
    cell_owners: Vec<Option<MappedOwnerId>>,
    region_owners: Vec<RegionAnchorId>,
    region_owner_ids: BTreeMap<RegionAnchorId, RegionOwnerId>,
    boundary_edges: Vec<BoundaryEdge>,
    boundary_edge_cells: Vec<Vec<CellId>>,
    boundary_edge_ids: BTreeMap<BoundaryEdge, BoundaryEdgeId>,
    #[serde(skip)]
    committed_owner_impact: MappedOwnerImpact,
}

impl ImplementationDb {
    #[cfg(test)]
    pub(crate) fn empty(cell_slots: usize) -> Self {
        Self::new_unbound(
            Vec::new().into_boxed_slice(),
            vec![OriginSetId::EMPTY; cell_slots],
            vec![0, 0],
            Vec::new(),
            std::iter::repeat_n(Some(InitialCellOwner::Global), cell_slots).collect(),
        )
        .expect("empty implementation ownership is valid")
    }

    pub(crate) fn new(
        mapped_generation: MappedGenerationId,
        regions: Box<[ImplementationRegion]>,
        cell_origins: Vec<OriginSetId>,
        origin_offsets: Vec<u32>,
        origin_operators: Vec<OperatorId>,
        cell_owners: Vec<Option<InitialCellOwner>>,
    ) -> Result<Self, crate::SynthError> {
        let database = Self::new_unbound(
            regions,
            cell_origins,
            origin_offsets,
            origin_operators,
            cell_owners,
        )?;
        database
            .mapped_generation
            .store(mapped_generation.get().get(), Ordering::Relaxed);
        Ok(database)
    }

    fn new_unbound(
        regions: Box<[ImplementationRegion]>,
        cell_origins: Vec<OriginSetId>,
        origin_offsets: Vec<u32>,
        origin_operators: Vec<OperatorId>,
        cell_owners: Vec<Option<InitialCellOwner>>,
    ) -> Result<Self, crate::SynthError> {
        // Rebuild the operator reverse map from its serialized CSR form. Cell
        // ownership is sealed independently; a missing owner is a removed slot.
        let origin_ids = build_origin_index(&origin_offsets, &origin_operators)?;
        let (
            cell_owners,
            region_owners,
            region_owner_ids,
            boundary_edges,
            boundary_edge_cells,
            boundary_edge_ids,
        ) = seal_owners(cell_owners)?;
        Ok(Self {
            mapped_generation: AtomicU64::new(0),
            regions,
            cell_origins,
            origin_offsets,
            origin_operators,
            origin_ids,
            cell_owners,
            region_owners,
            region_owner_ids,
            boundary_edges,
            boundary_edge_cells,
            boundary_edge_ids,
            committed_owner_impact: MappedOwnerImpact::default(),
        })
    }

    fn bind_or_validate_generation(
        &self,
        generation: MappedGenerationId,
    ) -> Result<(), crate::SynthError> {
        let expected = generation.get().get();
        match self.mapped_generation.compare_exchange(
            0,
            expected,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(actual) if actual == expected => Ok(()),
            Err(actual) => Err(crate::SynthError::invariant(format!(
                "implementation database belongs to mapped generation {actual}, not {expected}"
            ))),
        }
    }

    fn require_generation(&self, generation: MappedGenerationId) -> Result<(), crate::SynthError> {
        let expected = generation.get().get();
        let actual = self.mapped_generation.load(Ordering::Acquire);
        if actual == expected {
            Ok(())
        } else if actual == 0 {
            Err(crate::SynthError::invariant(
                "implementation database has no mapped generation binding",
            ))
        } else {
            Err(crate::SynthError::invariant(format!(
                "implementation database belongs to mapped generation {actual}, not {expected}"
            )))
        }
    }

    /// Return finalized implementation regions in dense-ID order.
    #[must_use]
    pub fn regions(&self) -> &[ImplementationRegion] {
        &self.regions
    }

    /// Look up an implementation region by its dense identifier.
    pub fn region(&self, id: ImplementationRegionId) -> Option<&ImplementationRegion> {
        self.regions.get(id.0 as usize)
    }

    /// Return the region for `operator` when it survived finalization.
    pub fn region_for_operator(&self, operator: OperatorId) -> Option<&ImplementationRegion> {
        let region = self.regions.get(operator.raw() as usize)?;
        (region.operator == operator).then_some(region)
    }

    /// Return semantic operators that contributed to a mapped cell.
    ///
    /// Returns `None` when `cell` is outside the mapped netlist's slot domain or
    /// names a removed stable slot. A live global cell returns an empty slice.
    pub fn operators_for_cell(&self, cell: CellId) -> Option<&[OperatorId]> {
        self.cell_owners.get(cell.index())?.as_ref()?;
        let origin = *self.cell_origins.get(cell.index())?;
        self.operators_for_origin(origin)
    }

    /// Return the explicit source-region ownership of a mapped cell slot.
    ///
    /// Region ownership is independent of semantic-operator provenance: a pure
    /// Boolean regional cell is still regional, while a static live cell is
    /// [`MappedCellOwnership::Global`].
    ///
    /// # Errors
    ///
    /// Returns an invariant error if a live regional owner ID has no matching
    /// finalized implementation region.
    pub fn cell_ownership(&self, cell: CellId) -> Result<MappedCellOwnership, crate::SynthError> {
        let Some(owner) = self.cell_owners.get(cell.index()) else {
            return Ok(MappedCellOwnership::Unknown);
        };
        let Some(owner) = owner else {
            return Ok(MappedCellOwnership::Removed);
        };
        if let Some(region_owner) = owner.region_id() {
            return if region_owner == RegionOwnerId::GLOBAL {
                Ok(MappedCellOwnership::Global)
            } else {
                self.region_for_owner(region_owner)
                    .map(MappedCellOwnership::Region)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "mapped cell references an unknown synthesis-region owner",
                        )
                    })
            };
        }
        let edge = owner
            .boundary_id()
            .and_then(|id| {
                self.boundary_edges
                    .get(id.0 as usize)
                    .copied()
                    .map(|edge| (id, edge))
            })
            .ok_or_else(|| {
                crate::SynthError::invariant("mapped cell references an unknown boundary edge")
            })?;
        Ok(MappedCellOwnership::Boundary {
            edge: edge.0,
            driver: edge.1.driver,
            sink: edge.1.sink,
        })
    }

    /// Return the stable mapped footprint currently owned by a boundary edge.
    pub fn boundary_edge_cells(&self, id: BoundaryEdgeId) -> Option<&[CellId]> {
        self.boundary_edge_cells
            .get(id.0 as usize)
            .map(Vec::as_slice)
    }

    /// Drains source-region ownership touched by committed mapped edits.
    pub(crate) fn take_committed_owner_impact(&mut self) -> MappedOwnerImpact {
        std::mem::take(&mut self.committed_owner_impact)
    }

    /// Releases spare capacity without changing any stable owner or origin ID.
    pub(crate) fn compact(&mut self) {
        for region in &mut self.regions {
            region.compact();
        }
        self.cell_origins.shrink_to_fit();
        self.origin_offsets.shrink_to_fit();
        self.origin_operators.shrink_to_fit();
        for origins in self.origin_ids.values_mut() {
            origins.shrink_to_fit();
        }
        self.cell_owners.shrink_to_fit();
        self.region_owners.shrink_to_fit();
        self.boundary_edges.shrink_to_fit();
        for cells in &mut self.boundary_edge_cells {
            cells.shrink_to_fit();
        }
        self.boundary_edge_cells.shrink_to_fit();
    }

    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let regions = resident::slice_bytes::<ImplementationRegion>(self.regions.len())
            .saturating_add(
                self.regions
                    .iter()
                    .map(ImplementationRegion::owned_memory_bytes)
                    .fold(0usize, usize::saturating_add),
            );
        let origin_index = self.origin_ids.values().fold(0usize, |bytes, ids| {
            let node_payload = size_of::<(u64, SmallVec<[OriginSetId; 1]>)>()
                .saturating_add(size_of::<usize>() * 4);
            bytes
                .saturating_add(resident::allocation_bytes(node_payload))
                .saturating_add(if ids.len() > 1 {
                    resident::slice_bytes::<OriginSetId>(ids.len())
                } else {
                    0
                })
        });
        let owner_index = self
            .region_owner_ids
            .len()
            .saturating_mul(resident::allocation_bytes(
                size_of::<(RegionAnchorId, RegionOwnerId)>() + size_of::<usize>() * 4,
            ));
        let boundary_index =
            self.boundary_edge_ids
                .len()
                .saturating_mul(resident::allocation_bytes(
                    size_of::<(BoundaryEdge, BoundaryEdgeId)>() + size_of::<usize>() * 4,
                ));
        regions
            .saturating_add(resident::slice_bytes::<OriginSetId>(
                self.cell_origins.len(),
            ))
            .saturating_add(resident::slice_bytes::<u32>(self.origin_offsets.len()))
            .saturating_add(resident::slice_bytes::<OperatorId>(
                self.origin_operators.len(),
            ))
            .saturating_add(origin_index)
            .saturating_add(resident::slice_bytes::<Option<MappedOwnerId>>(
                self.cell_owners.len(),
            ))
            .saturating_add(resident::slice_bytes::<RegionAnchorId>(
                self.region_owners.len(),
            ))
            .saturating_add(owner_index)
            .saturating_add(resident::slice_bytes::<BoundaryEdge>(
                self.boundary_edges.len(),
            ))
            .saturating_add(boundary_index)
            .saturating_add(resident::slice_bytes::<Vec<CellId>>(
                self.boundary_edge_cells.len(),
            ))
            .saturating_add(
                self.boundary_edge_cells
                    .iter()
                    .map(|cells| resident::slice_bytes::<CellId>(cells.len()))
                    .fold(0usize, usize::saturating_add),
            )
    }

    /// Validates the complete implementation owner before persistence.
    ///
    /// Checkpoints may not retain unconsumed edit impact, generation-mismatched
    /// cell rows, non-canonical origin sets, or reverse boundary footprints that
    /// disagree with the primary ownership column.
    pub(crate) fn validate_checkpoint(
        &self,
        mapped: &MappedNetlist,
    ) -> Result<(), crate::SynthError> {
        self.bind_or_validate_generation(mapped.generation_id())?;
        if !self.committed_owner_impact.is_empty() {
            return Err(crate::SynthError::invariant(
                "checkpoint retains unconsumed mapped owner changes",
            ));
        }
        if self.cell_origins.len() != mapped.cell_slot_count()
            || self.cell_owners.len() != mapped.cell_slot_count()
        {
            return Err(crate::SynthError::invariant(
                "checkpoint implementation indexes do not match mapped cell slots",
            ));
        }
        for (index, (&origin, owner)) in self.cell_origins.iter().zip(&self.cell_owners).enumerate()
        {
            let cell = CellId::from_index(index).map_err(crate::SynthError::Mapped)?;
            if mapped.is_live_cell(cell) != owner.is_some()
                || owner.is_none() && origin != OriginSetId::EMPTY
            {
                return Err(crate::SynthError::invariant(
                    "checkpoint mapped cell liveness and ownership disagree",
                ));
            }
        }
        if self.origin_offsets.first() != Some(&0)
            || self.origin_offsets.len() < 2
            || self.origin_offsets.get(1) != Some(&0)
            || self
                .origin_offsets
                .windows(2)
                .any(|bounds| bounds[0] > bounds[1])
            || self
                .origin_offsets
                .last()
                .and_then(|offset| usize::try_from(*offset).ok())
                != Some(self.origin_operators.len())
        {
            return Err(crate::SynthError::invariant(
                "checkpoint implementation origin arena is malformed",
            ));
        }
        let origin_count = self.origin_offsets.len() - 1;
        if self
            .cell_origins
            .iter()
            .any(|origin| origin.0 as usize >= origin_count)
        {
            return Err(crate::SynthError::invariant(
                "checkpoint mapped cell references an unknown implementation origin",
            ));
        }
        if self.cell_owners.iter().flatten().any(|owner| {
            owner
                .region_id()
                .is_some_and(|owner| owner.0 as usize > self.region_owners.len())
                || owner
                    .boundary_id()
                    .is_some_and(|edge| edge.0 as usize >= self.boundary_edges.len())
        }) {
            return Err(crate::SynthError::invariant(
                "checkpoint mapped cell references an unknown owner atom",
            ));
        }
        for (index, region) in self.regions.iter().enumerate() {
            if region.id.0 as usize != index
                || region.operator.raw() as usize != index
                || region.source_operations.len() != region.source_lines.len()
                || region
                    .mapped_cells
                    .windows(2)
                    .any(|cells| cells[0] >= cells[1])
                || region
                    .mapped_cells
                    .iter()
                    .any(|cell| !mapped.is_live_cell(*cell))
            {
                return Err(crate::SynthError::invariant(
                    "checkpoint implementation region is malformed",
                ));
            }
        }
        if self
            .origin_operators
            .iter()
            .any(|&operator| self.region_for_operator(operator).is_none())
        {
            return Err(crate::SynthError::invariant(
                "checkpoint implementation origins reference an unknown operator",
            ));
        }
        // Each deterministic hash bucket is strictly ordered by its canonical
        // CSR row. Matching total cardinality plus exact row hashes proves the
        // compact reverse index is a bijection without duplicating operator
        // payloads in tree keys.
        if self
            .origin_ids
            .values()
            .map(smallvec::SmallVec::len)
            .sum::<usize>()
            != origin_count
            || self.origin_offsets.windows(2).any(|bounds| {
                let operators = &self.origin_operators[bounds[0] as usize..bounds[1] as usize];
                operators.windows(2).any(|pair| pair[0] >= pair[1])
            })
            || self.origin_ids.iter().any(|(&hash, origins)| {
                origins.iter().any(|&origin| {
                    self.operators_for_origin(origin)
                        .is_none_or(|operators| implementation_origin_hash(operators) != hash)
                }) || origins.windows(2).any(|pair| {
                    let left = self
                        .operators_for_origin(pair[0])
                        .expect("origin hash validation checked the left row");
                    let right = self
                        .operators_for_origin(pair[1])
                        .expect("origin hash validation checked the right row");
                    left >= right
                })
            })
        {
            return Err(crate::SynthError::invariant(
                "checkpoint implementation origin index is inconsistent",
            ));
        }
        // Both sides are sets because origin rows and region cell rows are
        // strictly ordered. The forward membership check establishes that the
        // region index is a subset of cell origins; equal incidence proves that
        // neither side omits a pair without a reverse binary search.
        let mut indexed_operator_cell_incidence = 0_u128;
        for region in &self.regions {
            indexed_operator_cell_incidence += region.mapped_cells.len() as u128;
            for &cell in &region.mapped_cells {
                let operators = self.operators_for_cell(cell).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "checkpoint implementation region references an unowned mapped cell",
                    )
                })?;
                if operators.binary_search(&region.operator).is_err() {
                    return Err(crate::SynthError::invariant(
                        "checkpoint implementation region contains a cell without its operator origin",
                    ));
                }
            }
        }
        let mut origin_operator_cell_incidence = 0_u128;
        for (&origin, owner) in self.cell_origins.iter().zip(&self.cell_owners) {
            if owner.is_none() {
                continue;
            }
            let operators = self.operators_for_origin(origin).ok_or_else(|| {
                crate::SynthError::invariant(
                    "checkpoint mapped cell references an invalid implementation origin range",
                )
            })?;
            origin_operator_cell_incidence += operators.len() as u128;
        }
        if indexed_operator_cell_incidence != origin_operator_cell_incidence {
            return Err(crate::SynthError::invariant(
                "checkpoint operator origin omits its mapped cell reverse index",
            ));
        }
        if self.region_owner_ids.len() != self.region_owners.len()
            || self
                .region_owner_ids
                .iter()
                .any(|(region, owner)| self.region_for_owner(*owner) != Some(*region))
        {
            return Err(crate::SynthError::invariant(
                "checkpoint mapped region-owner index is inconsistent",
            ));
        }
        if self.boundary_edge_ids.len() != self.boundary_edges.len()
            || self.boundary_edge_cells.len() != self.boundary_edges.len()
            || self
                .boundary_edges
                .iter()
                .any(|edge| edge.driver == edge.sink)
            || self
                .boundary_edge_ids
                .iter()
                .any(|(edge, id)| self.boundary_edges.get(id.0 as usize) != Some(edge))
        {
            return Err(crate::SynthError::invariant(
                "checkpoint mapped boundary-edge arena is inconsistent",
            ));
        }
        // A strictly ordered footprint is a set. Owner equality below proves
        // every indexed cell belongs to the corresponding edge; matching the
        // total number of boundary-owned cells then proves the reverse relation.
        let mut indexed_boundary_cell_incidence = 0_u128;
        for (index, cells) in self.boundary_edge_cells.iter().enumerate() {
            let id = BoundaryEdgeId(
                u32::try_from(index)
                    .map_err(|_| crate::SynthError::capacity("mapped boundary-edge count"))?,
            );
            indexed_boundary_cell_incidence += cells.len() as u128;
            if cells.windows(2).any(|pair| pair[0] >= pair[1])
                || cells.iter().any(|&cell| {
                    !mapped.is_live_cell(cell)
                        || self
                            .owner_for_cell(cell)
                            .and_then(MappedOwnerId::boundary_id)
                            != Some(id)
                })
            {
                return Err(crate::SynthError::invariant(
                    "checkpoint mapped boundary-edge footprint is inconsistent",
                ));
            }
        }
        let boundary_owned_cells = self
            .cell_owners
            .iter()
            .flatten()
            .filter(|owner| owner.boundary_id().is_some())
            .count() as u128;
        if indexed_boundary_cell_incidence != boundary_owned_cells {
            return Err(crate::SynthError::invariant(
                "checkpoint boundary-owned cell is absent from its edge footprint",
            ));
        }
        Ok(())
    }

    fn operators_for_origin(&self, origin: OriginSetId) -> Option<&[OperatorId]> {
        let index = origin.0 as usize;
        let start = *self.origin_offsets.get(index)?;
        let end = *self.origin_offsets.get(index + 1)?;
        self.origin_operators.get(start as usize..end as usize)
    }

    fn origin_id(&self, operators: &[OperatorId]) -> Option<OriginSetId> {
        let candidates = self
            .origin_ids
            .get(&implementation_origin_hash(operators))?;
        candidates
            .binary_search_by(|origin| {
                self.operators_for_origin(*origin)
                    .expect("origin hash index references one canonical CSR row")
                    .cmp(operators)
            })
            .ok()
            .map(|position| candidates[position])
    }

    fn insert_origin_id(&mut self, origin: OriginSetId) {
        let operators = self
            .operators_for_origin(origin)
            .expect("new origin ID references the appended canonical CSR row");
        let hash = implementation_origin_hash(operators);
        let position = self.origin_ids.get(&hash).map_or(0, |candidates| {
            candidates
                .binary_search_by(|candidate| {
                    self.operators_for_origin(*candidate)
                        .expect("origin hash index references one canonical CSR row")
                        .cmp(operators)
                })
                .expect_err("new origin set was checked before insertion")
        });
        self.origin_ids
            .entry(hash)
            .or_default()
            .insert(position, origin);
    }

    fn region_for_owner(&self, owner: RegionOwnerId) -> Option<RegionAnchorId> {
        owner
            .0
            .checked_sub(1)
            .and_then(|index| self.region_owners.get(index as usize))
            .copied()
    }

    fn owner_for_cell(&self, cell: CellId) -> Option<MappedOwnerId> {
        self.cell_owners.get(cell.index()).copied().flatten()
    }

    pub(crate) fn ownership_endpoint(
        &self,
        cell: CellId,
    ) -> Result<Option<RegionAnchorId>, crate::SynthError> {
        match self.cell_ownership(cell)? {
            MappedCellOwnership::Region(region) => Ok(Some(region)),
            MappedCellOwnership::Boundary { sink, .. } => Ok(Some(sink)),
            MappedCellOwnership::Global => Ok(None),
            MappedCellOwnership::Removed | MappedCellOwnership::Unknown => {
                Err(crate::SynthError::invariant(format!(
                    "mapped cell {cell:?} has no live ownership endpoint"
                )))
            }
        }
    }

    pub(crate) fn cells_share_owner(
        &self,
        left: CellId,
        right: CellId,
    ) -> Result<bool, crate::SynthError> {
        let left = self.owner_for_cell(left).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped cell {left:?} has no live implementation owner"
            ))
        })?;
        let right = self.owner_for_cell(right).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped cell {right:?} has no live implementation owner"
            ))
        })?;
        Ok(left == right)
    }
}

fn build_origin_index(
    offsets: &[u32],
    operators: &[OperatorId],
) -> Result<OriginHashIndex, crate::SynthError> {
    let mut index = OriginHashIndex::new();
    for (raw, bounds) in offsets.windows(2).enumerate() {
        let origin = OriginSetId(u32::try_from(raw).map_err(|_| {
            crate::SynthError::capacity("implementation origin-set ID exceeds 32-bit capacity")
        })?);
        let row = operators
            .get(bounds[0] as usize..bounds[1] as usize)
            .ok_or_else(|| {
                crate::SynthError::invariant("implementation origin range is invalid")
            })?;
        let hash = implementation_origin_hash(row);
        let candidates = index.entry(hash).or_default();
        let Err(position) = candidates.binary_search_by(|candidate| {
            let candidate = candidate.0 as usize;
            let start = offsets[candidate] as usize;
            let end = offsets[candidate + 1] as usize;
            operators[start..end].cmp(row)
        }) else {
            return Err(crate::SynthError::invariant(
                "duplicate implementation origin set",
            ));
        };
        candidates.insert(position, origin);
    }
    Ok(index)
}

pub(crate) fn implementation_origin_hash(operators: &[OperatorId]) -> u64 {
    operators
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, operator| {
            (hash ^ u64::from(operator.raw())).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

#[cfg(test)]
mod tests;
