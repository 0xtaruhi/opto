// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Provenance from source operators to their mapped implementations.
//!
//! Mapping and post-map optimization may replace all cells that originally
//! implemented an operator. [`ImplementationDb`] therefore stores both
//! operator origins and immutable fragment containment for current mapped
//! cells; post-map rewrites publish both relations in one transaction.

use crate::{ImplementationCandidateId, OperationAnchorId, OperatorId, RegionAnchorId};
use opto_core::resident;
use opto_ir::mapped::{CellId, MappedGenerationId, MappedNetlist};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::BTreeMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicU64, Ordering};

mod edit;
mod fragment;
mod publication;

pub(crate) use edit::ImplementationDelta;
pub(crate) use fragment::FragmentImpact;
use fragment::seal_fragments;
pub use fragment::{FragmentFootprint, MappedFragmentId};

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
    source_operations: Box<[OperationAnchorId]>,
    source_lines: Box<[Option<u32>]>,
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
    pub(crate) width: u32,
}

pub(crate) struct ImplementationRegionSource<'a> {
    pub(crate) operations: &'a [OperationAnchorId],
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
            source_operations: source.operations.into(),
            source_lines: source.lines.into_boxed_slice(),
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
    /// Return the representative stable source operation for diagnostics.
    pub fn source_operation(&self) -> OperationAnchorId {
        self.source_operations[0]
    }

    /// Return all source operations absorbed by the implementation.
    #[must_use]
    pub fn source_operations(&self) -> &[OperationAnchorId] {
        &self.source_operations
    }

    /// Return source line numbers parallel to [`Self::source_operations`].
    #[must_use]
    pub fn source_lines(&self) -> &[Option<u32>] {
        &self.source_lines
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
        resident::slice_bytes::<OperationAnchorId>(self.source_operations.len())
            .saturating_add(resident::slice_bytes::<Option<u32>>(
                self.source_lines.len(),
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
/// compact operator list. Fragment footprints are interned independently from
/// semantic operator provenance.
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
    cell_fragments: Vec<Option<MappedFragmentId>>,
    fragments: Vec<FragmentFootprint>,
    fragment_ids: BTreeMap<FragmentFootprint, MappedFragmentId>,
    fragment_cells: Vec<Vec<CellId>>,
    #[serde(skip)]
    committed_fragment_impact: FragmentImpact,
}

impl ImplementationDb {
    #[cfg(test)]
    pub(crate) fn empty(cell_slots: usize) -> Self {
        Self::new_unbound(
            Vec::new().into_boxed_slice(),
            vec![OriginSetId::EMPTY; cell_slots],
            vec![0, 0],
            Vec::new(),
            std::iter::repeat_n(Some(FragmentFootprint::Global), cell_slots).collect(),
        )
        .expect("empty implementation containment is valid")
    }

    pub(crate) fn new(
        mapped_generation: MappedGenerationId,
        regions: Box<[ImplementationRegion]>,
        cell_origins: Vec<OriginSetId>,
        origin_offsets: Vec<u32>,
        origin_operators: Vec<OperatorId>,
        cell_fragments: Vec<Option<FragmentFootprint>>,
    ) -> Result<Self, crate::SynthError> {
        let database = Self::new_unbound(
            regions,
            cell_origins,
            origin_offsets,
            origin_operators,
            cell_fragments,
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
        cell_fragments: Vec<Option<FragmentFootprint>>,
    ) -> Result<Self, crate::SynthError> {
        // Rebuild the operator reverse map from its serialized CSR form. Cell
        // Containment is sealed independently; a missing row is a removed slot.
        let origin_ids = build_origin_index(&origin_offsets, &origin_operators)?;
        let (cell_fragments, fragments, fragment_ids, fragment_cells) =
            seal_fragments(cell_fragments)?;
        Ok(Self {
            mapped_generation: AtomicU64::new(0),
            regions,
            cell_origins,
            origin_offsets,
            origin_operators,
            origin_ids,
            cell_fragments,
            fragments,
            fragment_ids,
            fragment_cells,
            committed_fragment_impact: FragmentImpact::default(),
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
        self.cell_fragments.get(cell.index())?.as_ref()?;
        let origin = *self.cell_origins.get(cell.index())?;
        self.operators_for_origin(origin)
    }

    /// Returns the immutable fragment containing a live mapped cell.
    #[must_use]
    pub fn cell_fragment(&self, cell: CellId) -> Option<(MappedFragmentId, FragmentFootprint)> {
        let id = self.cell_fragments.get(cell.index()).copied().flatten()?;
        self.fragments
            .get(id.raw() as usize)
            .copied()
            .map(|fragment| (id, fragment))
    }

    /// Returns the live mapped cells contained by one fragment.
    #[must_use]
    pub fn fragment_cells(&self, id: MappedFragmentId) -> Option<&[CellId]> {
        self.fragment_cells
            .get(id.raw() as usize)
            .map(Vec::as_slice)
    }

    /// Drains fragment containment touched by committed mapped edits.
    pub(crate) fn take_committed_fragment_impact(&mut self) -> FragmentImpact {
        std::mem::take(&mut self.committed_fragment_impact)
    }

    /// Releases spare capacity without changing any fragment or origin ID.
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
        self.cell_fragments.shrink_to_fit();
        self.fragments.shrink_to_fit();
        for cells in &mut self.fragment_cells {
            cells.shrink_to_fit();
        }
        self.fragment_cells.shrink_to_fit();
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
        let fragment_index = self
            .fragment_ids
            .len()
            .saturating_mul(resident::allocation_bytes(
                size_of::<(FragmentFootprint, MappedFragmentId)>() + size_of::<usize>() * 4,
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
            .saturating_add(resident::slice_bytes::<Option<MappedFragmentId>>(
                self.cell_fragments.len(),
            ))
            .saturating_add(resident::slice_bytes::<FragmentFootprint>(
                self.fragments.len(),
            ))
            .saturating_add(fragment_index)
            .saturating_add(resident::slice_bytes::<Vec<CellId>>(
                self.fragment_cells.len(),
            ))
            .saturating_add(
                self.fragment_cells
                    .iter()
                    .map(|cells| resident::slice_bytes::<CellId>(cells.len()))
                    .fold(0usize, usize::saturating_add),
            )
    }

    /// Validates complete provenance and fragment containment before persistence.
    ///
    /// Checkpoints may not retain unconsumed edit impact, generation-mismatched
    /// cell rows, non-canonical origin sets, or reverse fragment footprints that
    /// disagree with the primary containment column.
    pub(crate) fn validate_checkpoint(
        &self,
        mapped: &MappedNetlist,
    ) -> Result<(), crate::SynthError> {
        self.bind_or_validate_generation(mapped.generation_id())?;
        if !self.committed_fragment_impact.is_empty() {
            return Err(crate::SynthError::invariant(
                "checkpoint retains unconsumed mapped fragment changes",
            ));
        }
        if self.cell_origins.len() != mapped.cell_slot_count()
            || self.cell_fragments.len() != mapped.cell_slot_count()
        {
            return Err(crate::SynthError::invariant(
                "checkpoint implementation indexes do not match mapped cell slots",
            ));
        }
        for (index, (&origin, fragment)) in self
            .cell_origins
            .iter()
            .zip(&self.cell_fragments)
            .enumerate()
        {
            let cell = CellId::from_index(index).map_err(crate::SynthError::Mapped)?;
            if mapped.is_live_cell(cell) != fragment.is_some()
                || fragment.is_none() && origin != OriginSetId::EMPTY
            {
                return Err(crate::SynthError::invariant(
                    "checkpoint mapped cell liveness and fragment containment disagree",
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
        if self
            .cell_fragments
            .iter()
            .flatten()
            .any(|fragment| fragment.raw() as usize >= self.fragments.len())
        {
            return Err(crate::SynthError::invariant(
                "checkpoint mapped cell references an unknown fragment",
            ));
        }
        for (index, region) in self.regions.iter().enumerate() {
            if region.id.0 as usize != index
                || region.operator.raw() as usize != index
                || region.source_operations.is_empty()
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
                        "checkpoint implementation region references an uncontained mapped cell",
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
        for (&origin, fragment) in self.cell_origins.iter().zip(&self.cell_fragments) {
            if fragment.is_none() {
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
        if self.fragment_ids.len() != self.fragments.len()
            || self.fragment_cells.len() != self.fragments.len()
            || self
                .fragments
                .iter()
                .any(|fragment| matches!(fragment, FragmentFootprint::Boundary { driver, sink } if driver == sink))
            || self
                .fragment_ids
                .iter()
                .any(|(fragment, id)| self.fragments.get(id.raw() as usize) != Some(fragment))
            || !self.fragment_ids.contains_key(&FragmentFootprint::Global)
        {
            return Err(crate::SynthError::invariant(
                "checkpoint mapped fragment arena is inconsistent",
            ));
        }
        let mut indexed_fragment_cell_incidence = 0_usize;
        for (index, cells) in self.fragment_cells.iter().enumerate() {
            let id = MappedFragmentId::from_index(index)?;
            indexed_fragment_cell_incidence = indexed_fragment_cell_incidence
                .checked_add(cells.len())
                .ok_or_else(|| crate::SynthError::capacity("mapped fragment incidence"))?;
            if cells.windows(2).any(|pair| pair[0] >= pair[1])
                || cells.iter().any(|&cell| {
                    !mapped.is_live_cell(cell)
                        || self.cell_fragments.get(cell.index()).copied().flatten() != Some(id)
                })
            {
                return Err(crate::SynthError::invariant(
                    "checkpoint mapped fragment footprint is inconsistent",
                ));
            }
        }
        if indexed_fragment_cell_incidence != self.cell_fragments.iter().flatten().count() {
            return Err(crate::SynthError::invariant(
                "checkpoint contained cell is absent from its fragment footprint",
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

    pub(crate) fn fragment_endpoint(
        &self,
        cell: CellId,
    ) -> Result<Option<RegionAnchorId>, crate::SynthError> {
        self.cell_fragment(cell)
            .map(|(_, fragment)| fragment.endpoint())
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "mapped cell {cell:?} has no live fragment endpoint"
                ))
            })
    }

    pub(crate) fn common_fragment(
        &self,
        cells: &[CellId],
    ) -> Result<FragmentFootprint, crate::SynthError> {
        let (&first, rest) = cells.split_first().ok_or_else(|| {
            crate::SynthError::invariant("mapped fragment requires at least one source cell")
        })?;
        let (_, fragment) = self.cell_fragment(first).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped source cell {first:?} has no live fragment"
            ))
        })?;
        for &cell in rest {
            if self.cell_fragment(cell).map(|row| row.1) != Some(fragment) {
                return Err(crate::SynthError::invariant(
                    "one mapped artifact cannot span multiple fragments",
                ));
            }
        }
        Ok(fragment)
    }

    pub(crate) fn repair_fragment(
        &self,
        drivers: &[CellId],
        sink: CellId,
    ) -> Result<FragmentFootprint, crate::SynthError> {
        let driver_fragment = (!drivers.is_empty())
            .then(|| self.common_fragment(drivers))
            .transpose()?;
        let sink_fragment = self.cell_fragment(sink).map(|row| row.1).ok_or_else(|| {
            crate::SynthError::invariant(format!("mapped sink cell {sink:?} has no live fragment"))
        })?;
        let driver_endpoint = driver_fragment.and_then(FragmentFootprint::endpoint);
        let sink_endpoint = sink_fragment.endpoint();
        if let (Some(driver), Some(sink)) = (driver_endpoint, sink_endpoint)
            && driver != sink
        {
            return Ok(FragmentFootprint::Boundary { driver, sink });
        }
        Ok(driver_fragment
            .filter(|fragment| fragment.endpoint().is_some())
            .unwrap_or(sink_fragment))
    }

    pub(crate) fn cells_share_fragment(
        &self,
        left: CellId,
        right: CellId,
    ) -> Result<bool, crate::SynthError> {
        let left = self.cell_fragment(left).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped cell {left:?} has no live implementation fragment"
            ))
        })?;
        let right = self.cell_fragment(right).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped cell {right:?} has no live implementation fragment"
            ))
        })?;
        Ok(left.0 == right.0)
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
