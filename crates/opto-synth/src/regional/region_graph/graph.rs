// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_REGION_GRAPH_OWNER: AtomicU64 = AtomicU64::new(1);

macro_rules! digest_id {
    ($name:ident, $doc:literal) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[doc = $doc]
        pub struct $name([u8; 32]);

        impl $name {
            #[must_use]
            /// Return the canonical serialized digest.
            pub const fn bytes(self) -> [u8; 32] {
                self.0
            }

            pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }
    };
}

digest_id!(
    OperationAnchorId,
    "Stable identity of a source operation occurrence, independent of arena order and source spans."
);

digest_id!(
    BoundaryPortId,
    "Stable identity of one typed region-boundary endpoint across compatible revisions."
);
digest_id!(
    BoundaryValueRevision,
    "Content revision of the Word value currently carried by a stable boundary endpoint."
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RegionGraphOwnerId(NonZeroU64);

impl RegionGraphOwnerId {
    pub(super) fn fresh() -> Self {
        let raw = NEXT_REGION_GRAPH_OWNER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("synthesis-region graph owner space is exhausted");
        Self(NonZeroU64::new(raw).expect("synthesis-region graph owners start at one"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Stable partition identity derived solely from a region's source anchor.
pub struct RegionAnchorId([u8; 32]);

impl RegionAnchorId {
    #[must_use]
    /// Return the stable digest used for serialization and cache lookup.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[cfg(test)]
    pub(crate) const fn from_bytes_for_test(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Dense row identity scoped to one immutable [`SynthesisRegionGraph`].
pub struct RegionRowId(u32);

impl RegionRowId {
    pub(crate) fn from_index(index: usize) -> Result<Self, crate::SynthError> {
        u32::try_from(index).map(Self).map_err(|_| {
            crate::SynthError::capacity("synthesis region row exceeds 32-bit capacity")
        })
    }

    #[must_use]
    /// Return the compact row number within the owning region graph.
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    /// Return the row number as a native slice index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

pub(super) fn remap_optional_owner_rows(
    owners: Vec<Option<usize>>,
    remap: &[RegionRowId],
) -> Box<[Option<RegionRowId>]> {
    owners
        .into_iter()
        .map(|owner| owner.and_then(|owner| remap.get(owner).copied()))
        .collect()
}

pub(super) fn packed_rows<T>(
    rows: Vec<Vec<T>>,
    resource: &'static str,
) -> Result<opto_core::PackedRows<T>, crate::SynthError> {
    opto_core::PackedRows::try_from_rows(rows)
        .map_err(|_| crate::SynthError::capacity(format!("{resource} exceed 32-bit capacity")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Dense boundary-port identity scoped to one region-graph revision.
pub struct RegionBoundaryPortId(u32);

impl RegionBoundaryPortId {
    pub(super) fn from_index(index: usize) -> Result<Self, crate::SynthError> {
        u32::try_from(index).map(Self).map_err(|_| {
            crate::SynthError::capacity("region boundary port exceeds 32-bit capacity")
        })
    }

    #[must_use]
    /// Return the compact port number within the owning region graph.
    pub const fn raw(self) -> u32 {
        self.0
    }

    #[must_use]
    /// Return the port number as a native slice index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Semantic role of a region.
pub enum SynthesisRegionKind {
    /// Pure combinational logic with no hard state boundary.
    Combinational,
    /// Registers, latches, and their tightly coupled logic.
    State,
    /// A memory and the logic required by its access ports.
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
/// Direction of one value at the owning region boundary.
pub enum RegionPortDirection {
    /// Value consumed by the owning region.
    Input,
    /// Value produced by the owning region.
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One explicit value crossing a hard synthesis boundary.
///
/// `peer` is absent at the root interface. Internal edges have one output row
/// and one input row, each owned by exactly one region.
pub struct RegionBoundaryPort {
    pub(super) id: RegionBoundaryPortId,
    pub(super) owner: RegionRowId,
    pub(super) peer: Option<RegionRowId>,
    pub(super) direction: RegionPortDirection,
    pub(super) value: word::ValueId,
    pub(super) ty: word::WordType,
    pub(super) stable_id: BoundaryPortId,
    pub(super) value_revision: BoundaryValueRevision,
    pub(super) edge_key: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// One frozen per-bit publication obligation derived before region placement.
///
/// The value and bit identify the semantic producer. `consumer` is absent for
/// a design root and names the consuming region for an internal crossing.
pub struct RegionBitFlow {
    pub(super) producer: RegionRowId,
    pub(super) consumer: Option<RegionRowId>,
    pub(super) value: word::ValueId,
    pub(super) bit: u32,
}

impl RegionBitFlow {
    #[must_use]
    /// Return the region responsible only for placing this producer artifact.
    pub const fn producer(self) -> RegionRowId {
        self.producer
    }

    #[must_use]
    /// Return the consuming region, or `None` for a design-root publication.
    pub const fn consumer(self) -> Option<RegionRowId> {
        self.consumer
    }

    #[must_use]
    /// Return the canonical producer value before bit lowering.
    pub const fn value(self) -> word::ValueId {
        self.value
    }

    #[must_use]
    /// Return the least-significant-bit-based index within the producer value.
    pub const fn bit(self) -> u32 {
        self.bit
    }
}

impl RegionBoundaryPort {
    #[must_use]
    /// Return the dense boundary-port ID within the owning graph.
    pub const fn id(self) -> RegionBoundaryPortId {
        self.id
    }

    #[must_use]
    /// Return the row of the region that owns this port.
    pub const fn owner(self) -> RegionRowId {
        self.owner
    }

    #[must_use]
    /// Return the region row at the opposite end of an internal edge.
    ///
    /// Root-interface ports have no peer.
    pub const fn peer(self) -> Option<RegionRowId> {
        self.peer
    }

    #[must_use]
    /// Return the direction from the owning region's perspective.
    pub const fn direction(self) -> RegionPortDirection {
        self.direction
    }

    #[must_use]
    /// Return the revision-local Word value crossing the boundary.
    pub const fn value(self) -> word::ValueId {
        self.value
    }

    #[must_use]
    /// Return the exact Word type of the crossing value.
    pub const fn ty(self) -> word::WordType {
        self.ty
    }

    #[must_use]
    /// Return the stable typed endpoint identity.
    pub const fn stable_id(self) -> BoundaryPortId {
        self.stable_id
    }

    #[must_use]
    /// Return the content revision of the crossing value.
    pub const fn value_revision(self) -> BoundaryValueRevision {
        self.value_revision
    }

    #[must_use]
    /// Return the content-derived port identity used across revisions.
    pub const fn semantic_key(self) -> [u8; 32] {
        self.edge_key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Content revision of one stable region anchor, excluding timing context.
pub struct RegionRevision([u8; 32]);

impl RegionRevision {
    #[must_use]
    /// Return the context-independent region digest.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[cfg(test)]
    pub(crate) const fn from_bytes_for_test(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Semantic generation of a complete immutable synthesis-region graph.
pub struct SynthesisRegionRevision([u8; 32]);

impl SynthesisRegionRevision {
    #[must_use]
    /// Return the digest of the complete immutable graph.
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Compact metadata for one region row.
pub struct SynthesisRegion {
    pub(super) graph_owner: RegionGraphOwnerId,
    pub(super) row: RegionRowId,
    pub(super) partition_anchor: [u8; 32],
    pub(super) id: RegionAnchorId,
    pub(super) revision: RegionRevision,
    pub(super) kind: SynthesisRegionKind,
    pub(super) estimated_work: u64,
    pub(super) estimated_delay: u64,
    pub(super) estimated_wiring: u64,
}

impl SynthesisRegion {
    #[must_use]
    /// Return the compact row within the owning graph.
    pub const fn row(self) -> RegionRowId {
        self.row
    }

    #[must_use]
    /// Return the content-anchored identity stable across compatible revisions.
    pub const fn id(self) -> RegionAnchorId {
        self.id
    }

    #[must_use]
    /// Return the stable partition anchor recorded when the region was formed.
    pub(crate) const fn partition_anchor(self) -> [u8; 32] {
        self.partition_anchor
    }

    #[must_use]
    /// Return the context-independent semantics key used by regional caches.
    pub const fn revision(self) -> RegionRevision {
        self.revision
    }

    #[must_use]
    /// Return the hard-boundary classification of the region.
    pub const fn kind(self) -> SynthesisRegionKind {
        self.kind
    }

    #[must_use]
    /// Return the deterministic work estimate used for worker allocation.
    pub const fn estimated_work(self) -> u64 {
        self.estimated_work
    }

    pub(crate) fn structural_estimate(self) -> crate::planning::provider::StructuralEstimate {
        crate::planning::provider::StructuralEstimate {
            logic_depth: u32::try_from(self.estimated_delay).unwrap_or(u32::MAX),
            logic_units: self.estimated_work,
            wiring_units: self.estimated_wiring,
        }
    }
}

#[derive(Debug)]
/// Immutable Word-revision partition with packed membership, typed ports, and
/// exact predecessor/successor CSR.
pub struct SynthesisRegionGraph {
    pub(super) owner: RegionGraphOwnerId,
    pub(super) revision: SynthesisRegionRevision,
    pub(super) regions: Box<[SynthesisRegion]>,
    pub(super) operations: opto_core::PackedRows<word::OpId>,
    pub(super) operation_anchors: Box<[OperationAnchorId]>,
    pub(super) operation_owners: Box<[Option<RegionRowId>]>,
    pub(super) memories: opto_core::PackedRows<word::MemoryId>,
    pub(super) memory_owners: Box<[Option<RegionRowId>]>,
    pub(super) ports: Box<[RegionBoundaryPort]>,
    pub(super) input_ports: opto_core::PackedRows<RegionBoundaryPortId>,
    pub(super) output_ports: opto_core::PackedRows<RegionBoundaryPortId>,
    pub(super) bit_flows: opto_core::PackedRows<RegionBitFlow>,
    pub(super) predecessors: opto_core::PackedRows<RegionRowId>,
    pub(super) successors: opto_core::PackedRows<RegionRowId>,
}

impl SynthesisRegionGraph {
    /// Builds the canonical graph with the versioned production cost policy.
    ///
    /// # Errors
    ///
    /// Returns [`crate::SynthError`] for malformed Word references, region
    /// invariant failures, or 32-bit packed-storage capacity exhaustion.
    pub fn build(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        super::partition::build(module, super::RegionPartitionPolicy::default())
    }

    #[must_use]
    /// Return the semantic generation of the complete graph.
    pub const fn revision(&self) -> SynthesisRegionRevision {
        self.revision
    }

    #[must_use]
    /// Return regions in dense-row order.
    pub fn regions(&self) -> &[SynthesisRegion] {
        &self.regions
    }

    #[must_use]
    /// Resolve a dense row, returning `None` when it is outside this graph.
    pub fn region(&self, row: RegionRowId) -> Option<SynthesisRegion> {
        self.regions.get(row.index()).copied()
    }

    // Row-indexed columns take a runtime-stamped `SynthesisRegion`. The stamp
    // rejects a row copied from another live graph before it can index any
    // packed column; a lifetime alone would only prove that some graph lives.

    fn checked_row(&self, region: SynthesisRegion) -> usize {
        assert_eq!(
            region.graph_owner, self.owner,
            "synthesis region belongs to another graph"
        );
        let row = region.row().index();
        assert_eq!(
            self.regions.get(row),
            Some(&region),
            "synthesis region metadata does not match its owning graph row"
        );
        row
    }

    #[must_use]
    /// Return source operations owned by `region` in canonical ID order.
    pub fn operations(&self, region: SynthesisRegion) -> &[word::OpId] {
        &self.operations[self.checked_row(region)]
    }

    #[must_use]
    /// Return the stable source-occurrence anchor for an operation.
    pub fn operation_anchor(&self, operation: word::OpId) -> Option<OperationAnchorId> {
        self.operation_anchors.get(operation.index()).copied()
    }

    #[must_use]
    pub(crate) fn operation_owner(&self, operation: word::OpId) -> Option<SynthesisRegion> {
        self.operation_owners
            .get(operation.index())
            .copied()
            .flatten()
            .and_then(|row| self.region(row))
    }

    #[must_use]
    pub(crate) fn operation_owner_rows(&self) -> &[Option<RegionRowId>] {
        &self.operation_owners
    }

    #[must_use]
    /// Return source memories owned by `region` in canonical ID order.
    pub fn memories(&self, region: SynthesisRegion) -> &[word::MemoryId] {
        &self.memories[self.checked_row(region)]
    }

    #[must_use]
    pub(crate) fn memory_owner_rows(&self) -> &[Option<RegionRowId>] {
        &self.memory_owners
    }

    #[must_use]
    /// Resolve a dense boundary-port ID within this graph.
    pub fn port(&self, id: RegionBoundaryPortId) -> Option<RegionBoundaryPort> {
        self.ports.get(id.index()).copied()
    }

    #[must_use]
    /// Return input boundary ports for `region` in semantic-key order.
    pub fn input_ports(&self, region: SynthesisRegion) -> &[RegionBoundaryPortId] {
        &self.input_ports[self.checked_row(region)]
    }

    #[must_use]
    /// Return output boundary ports for `region` in semantic-key order.
    pub fn output_ports(&self, region: SynthesisRegion) -> &[RegionBoundaryPortId] {
        &self.output_ports[self.checked_row(region)]
    }

    #[must_use]
    /// Return exact bit publication obligations owned by `region`.
    pub fn bit_flows(&self, region: SynthesisRegion) -> &[RegionBitFlow] {
        &self.bit_flows[self.checked_row(region)]
    }

    #[must_use]
    /// Return unique predecessor rows in ascending order.
    pub fn predecessors(&self, region: SynthesisRegion) -> &[RegionRowId] {
        &self.predecessors[self.checked_row(region)]
    }

    #[must_use]
    /// Return unique successor rows in ascending order.
    pub fn successors(&self, region: SynthesisRegion) -> &[RegionRowId] {
        &self.successors[self.checked_row(region)]
    }

    pub(crate) fn validate_for_module(
        &self,
        module: &word::WordModule,
    ) -> Result<(), crate::SynthError> {
        if self.operation_owners.len() != module.operations().len()
            || self.memory_owners.len() != module.memories().len()
            || self.operations.value_count() != self.operation_owners.iter().flatten().count()
            || self.memories.value_count() != self.memory_owners.iter().flatten().count()
        {
            return Err(crate::SynthError::invariant(
                "region reverse-owner columns do not match the Word arenas",
            ));
        }
        if self.bit_flows.row_count() != self.regions.len() {
            return Err(crate::SynthError::invariant(
                "regional bit-flow rows do not match the region arena",
            ));
        }
        let memory_data_owners = module
            .memory_read_ports()
            .iter()
            .filter_map(|read| {
                self.memory_owners
                    .get(read.memory.index())
                    .copied()
                    .flatten()
                    .map(|owner| (read.data, owner))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        for region in &self.regions {
            for publication in self.bit_flows(*region) {
                if publication.producer() != region.row()
                    || publication
                        .consumer()
                        .is_some_and(|consumer| consumer.index() >= self.regions.len())
                    || publication.consumer() == Some(publication.producer())
                {
                    return Err(crate::SynthError::invariant(
                        "regional bit publication has an invalid endpoint",
                    ));
                }
                let stored = module.value(publication.value()).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "regional bit publication references an unknown value",
                    )
                })?;
                if publication.bit() >= stored.ty.width() {
                    return Err(crate::SynthError::invariant(
                        "regional bit publication exceeds its producer value",
                    ));
                }
                match stored.kind {
                    word::ValueKind::Operation(operation) => {
                        if self.operation_owners.get(operation.index())
                            != Some(&Some(publication.producer()))
                        {
                            return Err(crate::SynthError::invariant(
                                "regional bit flow disagrees with operation placement",
                            ));
                        }
                    }
                    word::ValueKind::Signal(reference) => {
                        let memory_owner = memory_data_owners.get(&reference.signal).copied();
                        if memory_owner != Some(publication.producer()) {
                            return Err(crate::SynthError::invariant(
                                "regional bit flow disagrees with memory placement",
                            ));
                        }
                    }
                    word::ValueKind::Constant(_) => {
                        return Err(crate::SynthError::invariant(
                            "regional bit flow cannot use a constant producer",
                        ));
                    }
                }
            }
        }
        for region in &self.regions {
            if region.row().index() >= self.regions.len() {
                return Err(crate::SynthError::invariant(
                    "region row is outside its owning graph",
                ));
            }
            if self
                .operations(*region)
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
                || self
                    .memories(*region)
                    .windows(2)
                    .any(|pair| pair[0] >= pair[1])
            {
                return Err(crate::SynthError::invariant(
                    "region membership CSR is not strictly ordered",
                ));
            }
            for &operation in self.operations(*region) {
                if self.operation_owners.get(operation.index()) != Some(&Some(region.row())) {
                    return Err(crate::SynthError::invariant(
                        "region operation CSR disagrees with its reverse-owner column",
                    ));
                }
            }
            for &memory in self.memories(*region) {
                if self.memory_owners.get(memory.index()) != Some(&Some(region.row())) {
                    return Err(crate::SynthError::invariant(
                        "region memory CSR disagrees with its reverse-owner column",
                    ));
                }
            }
        }
        if self
            .operation_owners
            .iter()
            .flatten()
            .any(|owner| owner.index() >= self.regions.len())
            || self
                .memory_owners
                .iter()
                .flatten()
                .any(|owner| owner.index() >= self.regions.len())
        {
            return Err(crate::SynthError::invariant(
                "region reverse-owner column contains an unknown row",
            ));
        }
        for (index, port) in self.ports.iter().copied().enumerate() {
            if port.id().index() != index || port.owner().index() >= self.regions.len() {
                return Err(crate::SynthError::invariant(
                    "boundary port identity or owner is invalid",
                ));
            }
            if port
                .peer()
                .is_some_and(|peer| peer.index() >= self.regions.len())
            {
                return Err(crate::SynthError::invariant(
                    "boundary port peer is outside the region graph",
                ));
            }
            let owner = self.region(port.owner()).ok_or_else(|| {
                crate::SynthError::invariant("boundary port owner is outside the region graph")
            })?;
            let owner_ports = match port.direction() {
                RegionPortDirection::Input => self.input_ports(owner),
                RegionPortDirection::Output => self.output_ports(owner),
            };
            if owner_ports.binary_search(&port.id()).is_err() {
                return Err(crate::SynthError::invariant(
                    "boundary port is absent from its owner CSR",
                ));
            }
        }
        Ok(())
    }
}
