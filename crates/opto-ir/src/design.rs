// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Immutable stable-identity cell/net revisions and transactional replacement.
//!
//! [`DesignRevision`] is the canonical shared graph. Workers construct private
//! [`RewriteDelta`] values against one revision; [`DesignRevision::commit`]
//! validates their complete read and replacement footprints, proof
//! certificates, exact bit boundaries, and disjointness before returning a new
//! revision. The input revision is never mutated.
//!
//! Dense Word, Boolean, and mapped IDs are deliberately absent from this
//! module. Stable entity identities resolve through a persistent directory to
//! copy-on-write record pages, so scheduling and storage layout cannot become
//! semantic identity.

use crate::word::{LogicStateKind, SourceSpan};
use opto_core::PagedCowVec;
use std::collections::BTreeSet;
use thiserror::Error;

mod directory;
use directory::{PersistentDirectory, StableKey};

macro_rules! stable_id {
    ($name:ident, $kind:literal) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[repr(transparent)]
        #[doc = concat!("Stable identity of one ", $kind, ".")]
        pub struct $name([u8; 32]);

        impl $name {
            #[must_use]
            #[doc = concat!("Constructs a ", $kind, " identity from its canonical digest.")]
            pub const fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            #[must_use]
            #[doc = concat!("Returns the canonical digest of this ", $kind, " identity.")]
            pub const fn bytes(self) -> [u8; 32] {
                self.0
            }
        }

        impl StableKey for $name {
            fn bytes(self) -> [u8; 32] {
                self.0
            }
        }
    };
}

stable_id!(CellId, "design cell");
stable_id!(NetBitId, "design net bit");
stable_id!(DesignRevisionId, "design revision");
stable_id!(RewriteDeltaId, "rewrite delta");

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
/// Stable design entity identity accepted by read and replacement footprints.
pub enum EntityId {
    /// Cell entity.
    Cell(CellId),
    /// Scalar net entity.
    NetBit(NetBitId),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Sorted exact entity set used as a transaction footprint.
pub struct EntitySet(Box<[EntityId]>);

impl EntitySet {
    /// Constructs an exact footprint and rejects duplicate entity identities.
    ///
    /// # Errors
    ///
    /// Returns [`DesignError::DuplicateFootprintEntity`] when one entity is
    /// listed more than once.
    pub fn new(mut entities: Vec<EntityId>) -> Result<Self, DesignError> {
        entities.sort_unstable();
        if let Some(entity) = entities
            .windows(2)
            .find(|pair| pair[0] == pair[1])
            .map(|pair| pair[0])
        {
            return Err(DesignError::DuplicateFootprintEntity(entity));
        }
        Ok(Self(entities.into_boxed_slice()))
    }

    /// Returns the stable ordered entities.
    #[must_use]
    pub fn as_slice(&self) -> &[EntityId] {
        &self.0
    }

    /// Returns whether the footprint contains `entity`.
    #[must_use]
    pub fn contains(&self, entity: EntityId) -> bool {
        self.0.binary_search(&entity).is_ok()
    }

    /// Returns whether the footprint is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Immutable revision and exact entity closure read and replaced by one task.
pub struct RevisionFootprint {
    /// Immutable revision read by the worker.
    pub base: DesignRevisionId,
    /// Every entity whose structure influenced the result.
    pub reads: EntitySet,
    /// Complete mutation footprint.
    pub replaces: EntitySet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Exact driver endpoint of one canonical net bit.
pub enum NetDriver {
    /// Output pin of a design cell.
    Cell {
        /// Stable producing cell identity.
        cell: CellId,
        /// Zero-based output ordinal in [`Cell::outputs`].
        output: u32,
    },
    /// Canonical source-domain constant.
    Constant(crate::BitVal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Canonical scalar net with stable identity and at most one logical driver.
pub struct NetBit {
    /// Stable net identity.
    pub id: NetBitId,
    /// Two-state or four-state scalar domain.
    pub state: LogicStateKind,
    /// Absent for a design input; present for an internally driven net.
    pub driver: Option<NetDriver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Whether a cell propagates input changes combinationally to its outputs.
pub enum CellClass {
    /// Every output may depend combinationally on the inputs.
    Combinational,
    /// State or memory breaks combinational reachability.
    StateBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Canonical logical cell with exact input and output net bits.
pub struct Cell<L> {
    /// Stable cell identity independent of record slot and work shard.
    pub id: CellId,
    /// Logical operation or mapped target-cell payload.
    pub kind: L,
    /// Combinational-cycle classification independent of the payload type.
    pub class: CellClass,
    /// Input net bits in semantic operand order.
    pub inputs: Box<[NetBitId]>,
    /// Output net bits in semantic result order.
    pub outputs: Box<[NetBitId]>,
    /// Source construct responsible for the cell.
    pub source: SourceSpan,
}

/// Canonical semantic identity contributed by a design cell payload.
///
/// Implementations must return the same digest for semantically equal payloads
/// and different digests whenever the payload changes design behavior.
pub trait DesignPayload {
    /// Returns the payload's canonical semantic fingerprint.
    fn semantic_fingerprint(&self) -> [u8; 32];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Proof regime carried by one rewrite transaction.
pub enum EquivalenceRegime {
    /// Combinational boundary equivalence.
    Combinational,
    /// Sequential equivalence under an explicit state relation.
    Sequential,
    /// Exact construction whose equality follows from its generating recipe.
    ByConstruction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Immutable certificate identity validated before a rewrite is published.
pub struct EquivalenceCertificate {
    /// Proof regime required by the semantic change.
    pub regime: EquivalenceRegime,
    /// Canonical certificate or construction digest.
    pub digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Exact public interface represented by a replacement fragment.
pub struct SemanticBinding {
    /// Read-only boundary bits consumed by the fragment.
    pub inputs: Box<[NetBitId]>,
    /// Stable boundary bits driven by the fragment after commit.
    pub outputs: Box<[NetBitId]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Complete proof-carrying replacement proposed against one design revision.
///
/// Records whose IDs occur in [`Self::replaces`] replace existing entities.
/// Replacement entities omitted from `cells` or `nets` are removed. Records
/// with previously unknown IDs are new entities. A surviving entity outside
/// `replaces` cannot be modified.
pub struct RewriteDelta<L> {
    /// Stable transaction identity covering the recipe and complete fragment.
    pub id: RewriteDeltaId,
    /// Immutable base generation and exact read/replacement closure.
    pub footprint: RevisionFootprint,
    /// New or replacement cells.
    pub cells: Box<[Cell<L>]>,
    /// New or replacement net bits.
    pub nets: Box<[NetBit]>,
    /// Exact stable boundary interface.
    pub semantic: SemanticBinding,
    /// Equivalence evidence required before publication.
    pub proof: EquivalenceCertificate,
}

#[derive(Debug, Clone)]
/// Builder for the first immutable design generation.
pub struct DesignBuilder<L> {
    revision: DesignRevisionId,
    cells: Vec<Cell<L>>,
    nets: Vec<NetBit>,
}

impl<L> DesignBuilder<L> {
    /// Starts a design generation whose ID is the caller's canonical complete
    /// input fingerprint.
    #[must_use]
    pub const fn new(revision: DesignRevisionId) -> Self {
        Self {
            revision,
            cells: Vec::new(),
            nets: Vec::new(),
        }
    }

    /// Adds one canonical cell. Stable-ID uniqueness is checked at seal.
    pub fn add_cell(&mut self, cell: Cell<L>) {
        self.cells.push(cell);
    }

    /// Adds one canonical net bit. Stable-ID uniqueness is checked at seal.
    pub fn add_net(&mut self, net: NetBit) {
        self.nets.push(net);
    }
}

impl<L> DesignBuilder<L>
where
    L: Clone + Eq + DesignPayload,
{
    /// Sorts stable entities, constructs persistent directories, and validates
    /// complete connectivity.
    ///
    /// # Errors
    ///
    /// Returns [`DesignError`] for duplicate IDs, capacity exhaustion, or an
    /// invalid cell/net endpoint.
    pub fn seal(mut self) -> Result<DesignRevision<L>, DesignError> {
        self.cells.sort_unstable_by_key(|cell| cell.id);
        self.nets.sort_unstable_by_key(|net| net.id);
        let mut design = DesignRevision {
            revision: self.revision,
            cells: PagedCowVec::new(None),
            nets: PagedCowVec::new(None),
            cell_directory: PersistentDirectory::default(),
            net_directory: PersistentDirectory::default(),
            live_cells: 0,
            live_nets: 0,
        };
        for net in self.nets {
            design.insert_new_net(net)?;
        }
        for cell in self.cells {
            design.insert_new_cell(cell)?;
        }
        design.validate()?;
        Ok(design)
    }
}

#[derive(Debug, Clone)]
/// Immutable canonical design generation.
///
/// Cloning shares record pages and persistent directory paths. Mutation is
/// available only through [`Self::commit`], which returns another revision.
pub struct DesignRevision<L> {
    revision: DesignRevisionId,
    cells: PagedCowVec<Option<Cell<L>>>,
    nets: PagedCowVec<Option<NetBit>>,
    cell_directory: PersistentDirectory<CellId, u32>,
    net_directory: PersistentDirectory<NetBitId, u32>,
    live_cells: usize,
    live_nets: usize,
}

impl<L> DesignRevision<L> {
    /// Returns this complete generation's stable identity.
    #[must_use]
    pub const fn revision(&self) -> DesignRevisionId {
        self.revision
    }

    /// Returns one live cell by stable identity.
    #[must_use]
    pub fn cell(&self, id: CellId) -> Option<&Cell<L>> {
        let slot = self.cell_directory.get(id)? as usize;
        self.cells.get(slot)?.as_ref()
    }

    /// Returns one live net bit by stable identity.
    #[must_use]
    pub fn net(&self, id: NetBitId) -> Option<&NetBit> {
        let slot = self.net_directory.get(id)? as usize;
        self.nets.get(slot)?.as_ref()
    }

    /// Iterates live cells in immutable record order.
    ///
    /// Callers that publish semantic results must key or sort them by
    /// [`Cell::id`]; record order is a storage detail and may differ between
    /// revisions after copy-on-write publication.
    pub fn cells(&self) -> impl Iterator<Item = &Cell<L>> {
        (0..self.cells.len()).filter_map(|slot| self.cells.get(slot)?.as_ref())
    }

    /// Iterates live net bits in immutable record order.
    ///
    /// Callers that publish semantic results must key or sort them by
    /// [`NetBit::id`]; record order is a storage detail and may differ between
    /// revisions after copy-on-write publication.
    pub fn nets(&self) -> impl Iterator<Item = &NetBit> {
        (0..self.nets.len()).filter_map(|slot| self.nets.get(slot)?.as_ref())
    }

    /// Returns the number of live canonical cells.
    #[must_use]
    pub const fn cell_count(&self) -> usize {
        self.live_cells
    }

    /// Returns the number of live canonical net bits.
    #[must_use]
    pub const fn net_count(&self) -> usize {
        self.live_nets
    }
}

impl<L> DesignRevision<L>
where
    L: Clone + Eq + DesignPayload,
{
    /// Validates and atomically commits one deterministic wave of disjoint
    /// rewrite deltas.
    ///
    /// `validate_proof` is the owning synthesis layer's formal or
    /// construction-equivalence authority. It is called before any provisional
    /// page is built. Deltas are sorted by stable transaction ID, making the
    /// new revision independent of completion and packet order.
    ///
    /// # Errors
    ///
    /// Returns [`DesignError`] when a delta is stale, overlaps another write
    /// footprint, omits a read or boundary entity, changes an entity outside
    /// its footprint, collides with a stable ID, fails proof validation, or
    /// produces invalid connectivity. `self` remains unchanged on every error.
    pub fn commit(
        &self,
        mut deltas: Vec<RewriteDelta<L>>,
        validate_proof: impl Fn(&RewriteDelta<L>) -> Result<(), DesignError>,
    ) -> Result<Self, DesignError> {
        if deltas.is_empty() {
            return Ok(self.clone());
        }
        deltas.sort_unstable_by_key(|delta| delta.id);
        if let Some(id) = deltas
            .windows(2)
            .find(|pair| pair[0].id == pair[1].id)
            .map(|pair| pair[0].id)
        {
            return Err(DesignError::DuplicateDelta(id));
        }
        let mut claimed = BTreeSet::new();
        for delta in &deltas {
            self.validate_delta(delta)?;
            validate_proof(delta)?;
            for &entity in delta.footprint.replaces.as_slice() {
                if !claimed.insert(entity) {
                    return Err(DesignError::OverlappingReplacement(entity));
                }
            }
            for entity in delta
                .cells
                .iter()
                .map(|cell| EntityId::Cell(cell.id))
                .chain(delta.nets.iter().map(|net| EntityId::NetBit(net.id)))
            {
                if !delta.footprint.replaces.contains(entity) && !claimed.insert(entity) {
                    return Err(DesignError::OverlappingFragmentEntity(entity));
                }
            }
        }

        let mut provisional = self.clone();
        for delta in &deltas {
            provisional.apply_delta(delta)?;
        }
        provisional.revision = derive_revision_id(self.revision, &deltas);
        provisional.validate()?;
        Ok(provisional)
    }

    fn validate_delta(&self, delta: &RewriteDelta<L>) -> Result<(), DesignError> {
        if delta.footprint.base != self.revision {
            return Err(DesignError::StaleBase {
                expected: self.revision,
                received: delta.footprint.base,
            });
        }
        for &entity in delta.footprint.reads.as_slice() {
            if !self.contains(entity) {
                return Err(DesignError::UnknownRead(entity));
            }
        }
        for &entity in delta.footprint.replaces.as_slice() {
            if !self.contains(entity) {
                return Err(DesignError::UnknownReplacement(entity));
            }
            if !delta.footprint.reads.contains(entity) {
                return Err(DesignError::IncompleteReadFootprint(entity));
            }
        }
        validate_unique_cells(&delta.cells)?;
        validate_unique_nets(&delta.nets)?;
        for cell in &delta.cells {
            if self.cell(cell.id).is_some()
                && !delta.footprint.replaces.contains(EntityId::Cell(cell.id))
            {
                return Err(DesignError::UnclaimedMutation(EntityId::Cell(cell.id)));
            }
            if self.cell(cell.id).is_none() && self.cell_directory.get(cell.id).is_some() {
                return Err(DesignError::StableIdCollision(EntityId::Cell(cell.id)));
            }
        }
        for net in &delta.nets {
            if self.net(net.id).is_some()
                && !delta.footprint.replaces.contains(EntityId::NetBit(net.id))
            {
                return Err(DesignError::UnclaimedMutation(EntityId::NetBit(net.id)));
            }
            if self.net(net.id).is_none() && self.net_directory.get(net.id).is_some() {
                return Err(DesignError::StableIdCollision(EntityId::NetBit(net.id)));
            }
        }
        for &input in &delta.semantic.inputs {
            if self.net(input).is_none() {
                return Err(DesignError::UnknownBoundaryNet(input));
            }
            if !delta.footprint.reads.contains(EntityId::NetBit(input)) {
                return Err(DesignError::IncompleteReadFootprint(EntityId::NetBit(
                    input,
                )));
            }
        }
        for &output in &delta.semantic.outputs {
            if !delta.footprint.replaces.contains(EntityId::NetBit(output)) {
                return Err(DesignError::UnclaimedBoundaryOutput(output));
            }
            let previous = self
                .net(output)
                .expect("replacement validation established the boundary net");
            let Some(replacement) = delta.nets.iter().find(|net| net.id == output) else {
                return Err(DesignError::MissingBoundaryOutput(output));
            };
            if replacement.state != previous.state {
                return Err(DesignError::BoundaryStateMismatch(output));
            }
        }
        for cell in &delta.cells {
            for &input in &cell.inputs {
                let entity = EntityId::NetBit(input);
                if self.net(input).is_some()
                    && !delta.footprint.replaces.contains(entity)
                    && !delta.footprint.reads.contains(entity)
                {
                    return Err(DesignError::IncompleteReadFootprint(entity));
                }
            }
        }
        let semantic_inputs = delta
            .semantic
            .inputs
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let required_inputs = delta
            .cells
            .iter()
            .flat_map(|cell| cell.inputs.iter().copied())
            .filter(|&net| {
                self.net(net).is_some() && !delta.footprint.replaces.contains(EntityId::NetBit(net))
            })
            .collect::<BTreeSet<_>>();
        if let Some(net) = required_inputs.difference(&semantic_inputs).next().copied() {
            return Err(DesignError::IncompleteSemanticInput(net));
        }
        let semantic_outputs = delta
            .semantic
            .outputs
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let required_outputs = self
            .cells()
            .filter(|cell| !delta.footprint.replaces.contains(EntityId::Cell(cell.id)))
            .flat_map(|cell| cell.inputs.iter().copied())
            .filter(|&net| delta.footprint.replaces.contains(EntityId::NetBit(net)))
            .collect::<BTreeSet<_>>();
        if let Some(net) = required_outputs
            .difference(&semantic_outputs)
            .next()
            .copied()
        {
            return Err(DesignError::IncompleteSemanticOutput(net));
        }
        Ok(())
    }

    fn apply_delta(&mut self, delta: &RewriteDelta<L>) -> Result<(), DesignError> {
        for &entity in delta.footprint.replaces.as_slice() {
            match entity {
                EntityId::Cell(id) if !delta.cells.iter().any(|cell| cell.id == id) => {
                    self.remove_cell(id)?;
                }
                EntityId::NetBit(id) if !delta.nets.iter().any(|net| net.id == id) => {
                    self.remove_net(id)?;
                }
                EntityId::Cell(_) | EntityId::NetBit(_) => {}
            }
        }
        let mut nets = delta.nets.to_vec();
        nets.sort_unstable_by_key(|net| net.id);
        for net in nets {
            self.upsert_net(net)?;
        }
        let mut cells = delta.cells.to_vec();
        cells.sort_unstable_by_key(|cell| cell.id);
        for cell in cells {
            self.upsert_cell(cell)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), DesignError> {
        let mut counted_cells = 0usize;
        for slot in 0..self.cells.len() {
            let Some(cell) = self.cells.get(slot).and_then(Option::as_ref) else {
                continue;
            };
            counted_cells += 1;
            let encoded =
                u32::try_from(slot).map_err(|_| DesignError::Capacity(opto_core::CapacityError))?;
            if self.cell_directory.get(cell.id) != Some(encoded) {
                return Err(DesignError::DirectoryMismatch(EntityId::Cell(cell.id)));
            }
            for &input in &cell.inputs {
                if self.net(input).is_none() {
                    return Err(DesignError::UnknownCellNet {
                        cell: cell.id,
                        net: input,
                    });
                }
            }
            for (output, &net) in cell.outputs.iter().enumerate() {
                let expected = NetDriver::Cell {
                    cell: cell.id,
                    output: u32::try_from(output)
                        .map_err(|_| DesignError::Capacity(opto_core::CapacityError))?,
                };
                if self.net(net).and_then(|net| net.driver) != Some(expected) {
                    return Err(DesignError::OutputDriverMismatch { cell: cell.id, net });
                }
            }
        }
        let mut counted_nets = 0usize;
        for slot in 0..self.nets.len() {
            let Some(net) = self.nets.get(slot).and_then(Option::as_ref) else {
                continue;
            };
            counted_nets += 1;
            let encoded =
                u32::try_from(slot).map_err(|_| DesignError::Capacity(opto_core::CapacityError))?;
            if self.net_directory.get(net.id) != Some(encoded) {
                return Err(DesignError::DirectoryMismatch(EntityId::NetBit(net.id)));
            }
            match net.driver {
                Some(NetDriver::Cell { cell, output }) => {
                    let Some(driver) = self.cell(cell) else {
                        return Err(DesignError::UnknownNetDriver { net: net.id, cell });
                    };
                    if driver.outputs.get(output as usize).copied() != Some(net.id) {
                        return Err(DesignError::OutputDriverMismatch { cell, net: net.id });
                    }
                }
                Some(NetDriver::Constant(_)) | None => {}
            }
        }
        if counted_cells != self.live_cells || counted_nets != self.live_nets {
            return Err(DesignError::LiveCountMismatch);
        }
        self.validate_combinational_acyclic()?;
        Ok(())
    }

    fn validate_combinational_acyclic(&self) -> Result<(), DesignError> {
        let combinational = self
            .cells()
            .filter(|cell| cell.class == CellClass::Combinational)
            .map(|cell| cell.id)
            .collect::<BTreeSet<_>>();
        let mut indegree = combinational
            .iter()
            .copied()
            .map(|cell| (cell, 0usize))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut successors = std::collections::BTreeMap::<CellId, BTreeSet<CellId>>::new();
        for cell in self
            .cells()
            .filter(|cell| cell.class == CellClass::Combinational)
        {
            for &input in &cell.inputs {
                let Some(NetDriver::Cell { cell: source, .. }) =
                    self.net(input).and_then(|net| net.driver)
                else {
                    continue;
                };
                if combinational.contains(&source)
                    && successors.entry(source).or_default().insert(cell.id)
                {
                    *indegree
                        .get_mut(&cell.id)
                        .expect("combinational cell was indexed") += 1;
                }
            }
        }
        let mut ready = indegree
            .iter()
            .filter_map(|(&cell, &degree)| (degree == 0).then_some(cell))
            .collect::<BTreeSet<_>>();
        let mut visited = 0usize;
        while let Some(cell) = ready.pop_first() {
            visited += 1;
            for &successor in successors.get(&cell).into_iter().flatten() {
                let degree = indegree
                    .get_mut(&successor)
                    .expect("combinational successor was indexed");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(successor);
                }
            }
        }
        if visited != combinational.len() {
            return Err(DesignError::CombinationalCycle);
        }
        Ok(())
    }

    fn contains(&self, entity: EntityId) -> bool {
        match entity {
            EntityId::Cell(id) => self.cell(id).is_some(),
            EntityId::NetBit(id) => self.net(id).is_some(),
        }
    }

    fn insert_new_cell(&mut self, cell: Cell<L>) -> Result<(), DesignError> {
        let id = cell.id;
        if self.cell_directory.get(id).is_some() {
            return Err(DesignError::StableIdCollision(EntityId::Cell(id)));
        }
        let slot = self.cells.len();
        let encoded =
            u32::try_from(slot).map_err(|_| DesignError::Capacity(opto_core::CapacityError))?;
        self.cells.try_set(slot, Some(cell))?;
        let (directory, previous) = self.cell_directory.insert(id, encoded);
        debug_assert!(previous.is_none());
        self.cell_directory = directory;
        self.live_cells += 1;
        Ok(())
    }

    fn insert_new_net(&mut self, net: NetBit) -> Result<(), DesignError> {
        let id = net.id;
        if self.net_directory.get(id).is_some() {
            return Err(DesignError::StableIdCollision(EntityId::NetBit(id)));
        }
        let slot = self.nets.len();
        let encoded =
            u32::try_from(slot).map_err(|_| DesignError::Capacity(opto_core::CapacityError))?;
        self.nets.try_set(slot, Some(net))?;
        let (directory, previous) = self.net_directory.insert(id, encoded);
        debug_assert!(previous.is_none());
        self.net_directory = directory;
        self.live_nets += 1;
        Ok(())
    }

    fn upsert_cell(&mut self, cell: Cell<L>) -> Result<(), DesignError> {
        if let Some(slot) = self.cell_directory.get(cell.id) {
            let was_live = self.cells.get(slot as usize).is_some_and(Option::is_some);
            self.cells.try_set(slot as usize, Some(cell))?;
            self.live_cells += usize::from(!was_live);
            Ok(())
        } else {
            self.insert_new_cell(cell)
        }
    }

    fn upsert_net(&mut self, net: NetBit) -> Result<(), DesignError> {
        if let Some(slot) = self.net_directory.get(net.id) {
            let was_live = self.nets.get(slot as usize).is_some_and(Option::is_some);
            self.nets.try_set(slot as usize, Some(net))?;
            self.live_nets += usize::from(!was_live);
            Ok(())
        } else {
            self.insert_new_net(net)
        }
    }

    fn remove_cell(&mut self, id: CellId) -> Result<(), DesignError> {
        let slot =
            self.cell_directory
                .get(id)
                .ok_or(DesignError::UnknownReplacement(EntityId::Cell(id)))? as usize;
        if self
            .cells
            .try_set(slot, None)?
            .is_some_and(|cell| cell.is_some())
        {
            self.live_cells -= 1;
        }
        Ok(())
    }

    fn remove_net(&mut self, id: NetBitId) -> Result<(), DesignError> {
        let slot =
            self.net_directory
                .get(id)
                .ok_or(DesignError::UnknownReplacement(EntityId::NetBit(id)))? as usize;
        if self
            .nets
            .try_set(slot, None)?
            .is_some_and(|net| net.is_some())
        {
            self.live_nets -= 1;
        }
        Ok(())
    }
}

fn validate_unique_cells<L>(cells: &[Cell<L>]) -> Result<(), DesignError> {
    let mut identities = cells.iter().map(|cell| cell.id).collect::<Vec<_>>();
    identities.sort_unstable();
    if let Some(id) = identities
        .windows(2)
        .find(|pair| pair[0] == pair[1])
        .map(|pair| pair[0])
    {
        return Err(DesignError::DuplicateFragmentEntity(EntityId::Cell(id)));
    }
    Ok(())
}

fn validate_unique_nets(nets: &[NetBit]) -> Result<(), DesignError> {
    let mut identities = nets.iter().map(|net| net.id).collect::<Vec<_>>();
    identities.sort_unstable();
    if let Some(id) = identities
        .windows(2)
        .find(|pair| pair[0] == pair[1])
        .map(|pair| pair[0])
    {
        return Err(DesignError::DuplicateFragmentEntity(EntityId::NetBit(id)));
    }
    Ok(())
}

fn derive_revision_id<L: DesignPayload>(
    base: DesignRevisionId,
    deltas: &[RewriteDelta<L>],
) -> DesignRevisionId {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto.design-revision.v2\0");
    digest.update(&base.bytes());
    for delta in deltas {
        digest.update(&delta.id.bytes());
        hash_entities(&mut digest, delta.footprint.reads.as_slice());
        hash_entities(&mut digest, delta.footprint.replaces.as_slice());
        let mut cells = delta.cells.iter().collect::<Vec<_>>();
        cells.sort_unstable_by_key(|cell| cell.id);
        digest.update(&(cells.len() as u64).to_le_bytes());
        for cell in cells {
            digest.update(&cell.id.bytes());
            digest.update(&cell.kind.semantic_fingerprint());
            digest.update(&[match cell.class {
                CellClass::Combinational => 0,
                CellClass::StateBoundary => 1,
            }]);
            hash_net_ids(&mut digest, &cell.inputs);
            hash_net_ids(&mut digest, &cell.outputs);
        }
        let mut nets = delta.nets.iter().collect::<Vec<_>>();
        nets.sort_unstable_by_key(|net| net.id);
        digest.update(&(nets.len() as u64).to_le_bytes());
        for net in nets {
            digest.update(&net.id.bytes());
            digest.update(&[match net.state {
                LogicStateKind::TwoState => 0,
                LogicStateKind::FourState => 1,
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
                    digest.update(&[2, value as u8]);
                }
            }
        }
        hash_net_ids(&mut digest, &delta.semantic.inputs);
        hash_net_ids(&mut digest, &delta.semantic.outputs);
        digest.update(&[match delta.proof.regime {
            EquivalenceRegime::Combinational => 0,
            EquivalenceRegime::Sequential => 1,
            EquivalenceRegime::ByConstruction => 2,
        }]);
        digest.update(&delta.proof.digest);
    }
    DesignRevisionId::from_bytes(*digest.finalize().as_bytes())
}

fn hash_entities(digest: &mut blake3::Hasher, entities: &[EntityId]) {
    digest.update(&(entities.len() as u64).to_le_bytes());
    for entity in entities {
        match entity {
            EntityId::Cell(id) => {
                digest.update(&[0]);
                digest.update(&id.bytes());
            }
            EntityId::NetBit(id) => {
                digest.update(&[1]);
                digest.update(&id.bytes());
            }
        }
    }
}

fn hash_net_ids(digest: &mut blake3::Hasher, nets: &[NetBitId]) {
    digest.update(&(nets.len() as u64).to_le_bytes());
    for net in nets {
        digest.update(&net.bytes());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Construction, transaction, capacity, proof, or connectivity failure.
pub enum DesignError {
    /// A footprint repeats one entity.
    #[error("design footprint repeats entity {0:?}")]
    DuplicateFootprintEntity(EntityId),
    /// A stable identity already names another record.
    #[error("stable design identity collides at {0:?}")]
    StableIdCollision(EntityId),
    /// A fragment contains one stable identity more than once.
    #[error("rewrite fragment repeats entity {0:?}")]
    DuplicateFragmentEntity(EntityId),
    /// A commit wave repeats a transaction identity.
    #[error("rewrite wave repeats delta {0:?}")]
    DuplicateDelta(RewriteDeltaId),
    /// A delta was built from another immutable generation.
    #[error("rewrite delta base is {received:?}, expected {expected:?}")]
    StaleBase {
        /// Accepted revision.
        expected: DesignRevisionId,
        /// Delta revision.
        received: DesignRevisionId,
    },
    /// One declared read does not exist in the base revision.
    #[error("rewrite delta reads unknown entity {0:?}")]
    UnknownRead(EntityId),
    /// One declared replacement does not exist in the base revision.
    #[error("rewrite delta replaces unknown entity {0:?}")]
    UnknownReplacement(EntityId),
    /// A base entity used or replaced by the fragment was omitted from reads.
    #[error("rewrite delta omits base dependency {0:?} from its read footprint")]
    IncompleteReadFootprint(EntityId),
    /// Two transactions in one ordinary wave replace the same entity.
    #[error("rewrite deltas overlap at replacement entity {0:?}")]
    OverlappingReplacement(EntityId),
    /// Two transactions introduce the same stable fragment identity.
    #[error("rewrite deltas introduce the same fragment entity {0:?}")]
    OverlappingFragmentEntity(EntityId),
    /// A fragment changes an existing entity outside its write footprint.
    #[error("rewrite fragment mutates unclaimed entity {0:?}")]
    UnclaimedMutation(EntityId),
    /// A semantic boundary refers to a missing net.
    #[error("rewrite boundary refers to unknown net {0:?}")]
    UnknownBoundaryNet(NetBitId),
    /// A fragment consumes an external net omitted from its semantic inputs.
    #[error("rewrite semantic inputs omit external net {0:?}")]
    IncompleteSemanticInput(NetBitId),
    /// A changed net drives an unchanged consumer but is absent from outputs.
    #[error("rewrite semantic outputs omit externally consumed net {0:?}")]
    IncompleteSemanticOutput(NetBitId),
    /// A driven boundary output was not declared writable.
    #[error("rewrite boundary output {0:?} is outside the replacement footprint")]
    UnclaimedBoundaryOutput(NetBitId),
    /// A boundary output is removed instead of receiving its replacement driver.
    #[error("rewrite fragment omits replacement boundary output {0:?}")]
    MissingBoundaryOutput(NetBitId),
    /// A replacement changes the logical state domain of a stable boundary bit.
    #[error("rewrite fragment changes the state domain of boundary output {0:?}")]
    BoundaryStateMismatch(NetBitId),
    /// A cell references a missing net.
    #[error("design cell {cell:?} references unknown net {net:?}")]
    UnknownCellNet {
        /// Referencing cell.
        cell: CellId,
        /// Missing net.
        net: NetBitId,
    },
    /// A net driver references a missing cell.
    #[error("design net {net:?} references unknown driver cell {cell:?}")]
    UnknownNetDriver {
        /// Driven net.
        net: NetBitId,
        /// Missing driver.
        cell: CellId,
    },
    /// Cell output and net driver records disagree.
    #[error("cell {cell:?} and net {net:?} disagree on their driver binding")]
    OutputDriverMismatch {
        /// Cell side of the binding.
        cell: CellId,
        /// Net side of the binding.
        net: NetBitId,
    },
    /// Persistent directory and record slot disagree.
    #[error("stable design directory does not resolve {0:?} to its record")]
    DirectoryMismatch(EntityId),
    /// Cached live counts disagree with record pages.
    #[error("design revision live counts do not match its record pages")]
    LiveCountMismatch,
    /// The scalar cell/net graph contains a combinational cycle.
    #[error("design revision contains a combinational cycle")]
    CombinationalCycle,
    /// The owning proof engine rejected a transaction certificate.
    #[error("rewrite equivalence certificate was rejected: {0}")]
    ProofRejected(String),
    /// Stable slot or record-page capacity was exhausted.
    #[error("design revision exceeds stable record capacity")]
    Capacity(#[from] opto_core::CapacityError),
}

#[cfg(test)]
mod tests {
    use super::*;

    impl DesignPayload for &'_ str {
        fn semantic_fingerprint(&self) -> [u8; 32] {
            *blake3::hash(self.as_bytes()).as_bytes()
        }
    }

    fn digest(value: u8) -> [u8; 32] {
        [value; 32]
    }

    fn net(id: u8, driver: Option<(u8, u32)>) -> NetBit {
        NetBit {
            id: NetBitId::from_bytes(digest(id)),
            state: LogicStateKind::TwoState,
            driver: driver.map(|(cell, output)| NetDriver::Cell {
                cell: CellId::from_bytes(digest(cell)),
                output,
            }),
        }
    }

    fn cell(id: u8, kind: &'static str, inputs: &[u8], outputs: &[u8]) -> Cell<&'static str> {
        Cell {
            id: CellId::from_bytes(digest(id)),
            kind,
            class: CellClass::Combinational,
            inputs: inputs
                .iter()
                .map(|id| NetBitId::from_bytes(digest(*id)))
                .collect(),
            outputs: outputs
                .iter()
                .map(|id| NetBitId::from_bytes(digest(*id)))
                .collect(),
            source: SourceSpan::stable(kind),
        }
    }

    fn base_design() -> DesignRevision<&'static str> {
        let mut builder = DesignBuilder::new(DesignRevisionId::from_bytes(digest(90)));
        builder.add_net(net(10, None));
        builder.add_net(net(11, Some((1, 0))));
        builder.add_cell(cell(1, "not", &[10], &[11]));
        builder.seal().unwrap()
    }

    fn set(entities: Vec<EntityId>) -> EntitySet {
        EntitySet::new(entities).unwrap()
    }

    fn footprint(
        base: DesignRevisionId,
        reads: Vec<EntityId>,
        replaces: Vec<EntityId>,
    ) -> RevisionFootprint {
        RevisionFootprint {
            base,
            reads: set(reads),
            replaces: set(replaces),
        }
    }

    fn delta(id: u8, base: DesignRevisionId, kind: &'static str) -> RewriteDelta<&'static str> {
        RewriteDelta {
            id: RewriteDeltaId::from_bytes(digest(id)),
            footprint: footprint(
                base,
                vec![
                    EntityId::Cell(CellId::from_bytes(digest(1))),
                    EntityId::NetBit(NetBitId::from_bytes(digest(10))),
                    EntityId::NetBit(NetBitId::from_bytes(digest(11))),
                ],
                vec![
                    EntityId::Cell(CellId::from_bytes(digest(1))),
                    EntityId::NetBit(NetBitId::from_bytes(digest(11))),
                ],
            ),
            cells: vec![cell(2, kind, &[10], &[11])].into_boxed_slice(),
            nets: vec![net(11, Some((2, 0)))].into_boxed_slice(),
            semantic: SemanticBinding {
                inputs: vec![NetBitId::from_bytes(digest(10))].into_boxed_slice(),
                outputs: vec![NetBitId::from_bytes(digest(11))].into_boxed_slice(),
            },
            proof: EquivalenceCertificate {
                regime: EquivalenceRegime::Combinational,
                digest: digest(id + 1),
            },
        }
    }

    #[test]
    fn commit_replaces_a_driver_without_changing_the_boundary_net() {
        let base = base_design();
        let replacement = delta(20, base.revision(), "nand-as-not");
        let committed = base.commit(vec![replacement], |_| Ok(())).unwrap();

        assert_eq!(base.cell_count(), 1);
        assert!(base.cell(CellId::from_bytes(digest(1))).is_some());
        assert_eq!(committed.cell_count(), 1);
        assert!(committed.cell(CellId::from_bytes(digest(1))).is_none());
        assert_eq!(
            committed.cell(CellId::from_bytes(digest(2))).unwrap().kind,
            "nand-as-not"
        );
        assert_eq!(
            committed
                .net(NetBitId::from_bytes(digest(11)))
                .unwrap()
                .driver,
            Some(NetDriver::Cell {
                cell: CellId::from_bytes(digest(2)),
                output: 0,
            })
        );
    }

    #[test]
    fn empty_commit_preserves_the_exact_revision() {
        let base = base_design();
        let committed = base.commit(Vec::new(), |_| unreachable!()).unwrap();

        assert_eq!(committed.revision(), base.revision());
        assert_eq!(committed.cell_count(), base.cell_count());
        assert_eq!(committed.net_count(), base.net_count());
    }

    #[test]
    fn sealing_rejects_a_bit_level_combinational_cycle() {
        let mut builder = DesignBuilder::new(DesignRevisionId::from_bytes(digest(90)));
        builder.add_net(net(10, Some((2, 0))));
        builder.add_net(net(11, Some((1, 0))));
        builder.add_cell(cell(1, "left", &[10], &[11]));
        builder.add_cell(cell(2, "right", &[11], &[10]));

        assert_eq!(builder.seal().unwrap_err(), DesignError::CombinationalCycle);
    }

    #[test]
    fn state_boundary_breaks_a_bit_level_cycle() {
        let mut builder = DesignBuilder::new(DesignRevisionId::from_bytes(digest(90)));
        builder.add_net(net(10, Some((2, 0))));
        builder.add_net(net(11, Some((1, 0))));
        builder.add_cell(cell(1, "logic", &[10], &[11]));
        let mut state = cell(2, "register", &[11], &[10]);
        state.class = CellClass::StateBoundary;
        builder.add_cell(state);

        builder.seal().unwrap();
    }

    #[test]
    fn iteration_skips_tombstones_without_redefining_identity() {
        let base = base_design();
        let removed = base
            .commit(
                vec![RewriteDelta {
                    id: RewriteDeltaId::from_bytes(digest(20)),
                    footprint: footprint(
                        base.revision(),
                        vec![
                            EntityId::Cell(CellId::from_bytes(digest(1))),
                            EntityId::NetBit(NetBitId::from_bytes(digest(11))),
                        ],
                        vec![
                            EntityId::Cell(CellId::from_bytes(digest(1))),
                            EntityId::NetBit(NetBitId::from_bytes(digest(11))),
                        ],
                    ),
                    cells: Box::new([]),
                    nets: Box::new([]),
                    semantic: SemanticBinding {
                        inputs: Box::new([]),
                        outputs: Box::new([]),
                    },
                    proof: EquivalenceCertificate {
                        regime: EquivalenceRegime::ByConstruction,
                        digest: digest(21),
                    },
                }],
                |_| Ok(()),
            )
            .unwrap();

        assert_eq!(removed.cells().count(), removed.cell_count());
        assert_eq!(removed.nets().count(), removed.net_count());
        assert_eq!(
            removed.nets().map(|net| net.id).collect::<Vec<_>>(),
            vec![NetBitId::from_bytes(digest(10))]
        );
    }

    #[test]
    fn failed_proof_leaves_the_input_revision_unchanged() {
        let base = base_design();
        let revision = base.revision();
        let error = base
            .commit(vec![delta(20, revision, "bad")], |_| {
                Err(DesignError::ProofRejected("counterexample".to_string()))
            })
            .unwrap_err();

        assert!(matches!(error, DesignError::ProofRejected(_)));
        assert_eq!(base.revision(), revision);
        assert_eq!(base.cell_count(), 1);
        assert_eq!(
            base.cell(CellId::from_bytes(digest(1))).unwrap().kind,
            "not"
        );
    }

    #[test]
    fn commit_rejects_an_incomplete_read_footprint() {
        let base = base_design();
        let mut replacement = delta(20, base.revision(), "missing-read");
        replacement.footprint.reads = set(vec![
            EntityId::NetBit(NetBitId::from_bytes(digest(10))),
            EntityId::NetBit(NetBitId::from_bytes(digest(11))),
        ]);

        assert!(matches!(
            base.commit(vec![replacement], |_| Ok(())),
            Err(DesignError::IncompleteReadFootprint(EntityId::Cell(_)))
        ));
    }

    #[test]
    fn commit_rejects_an_incomplete_semantic_input() {
        let base = base_design();
        let mut replacement = delta(20, base.revision(), "missing-input");
        replacement.semantic.inputs = Box::new([]);

        assert_eq!(
            base.commit(vec![replacement], |_| Ok(())).unwrap_err(),
            DesignError::IncompleteSemanticInput(NetBitId::from_bytes(digest(10)))
        );
    }

    #[test]
    fn commit_rejects_an_omitted_externally_consumed_output() {
        let mut builder = DesignBuilder::new(DesignRevisionId::from_bytes(digest(90)));
        builder.add_net(net(10, None));
        builder.add_net(net(11, Some((1, 0))));
        builder.add_net(net(12, Some((3, 0))));
        builder.add_cell(cell(1, "not", &[10], &[11]));
        builder.add_cell(cell(3, "consumer", &[11], &[12]));
        let base = builder.seal().unwrap();
        let mut replacement = delta(20, base.revision(), "replacement");
        replacement.semantic.outputs = Box::new([]);

        assert_eq!(
            base.commit(vec![replacement], |_| Ok(())).unwrap_err(),
            DesignError::IncompleteSemanticOutput(NetBitId::from_bytes(digest(11)))
        );
    }

    #[test]
    fn stable_boundary_cannot_change_state_domain() {
        let base = base_design();
        let mut replacement = delta(20, base.revision(), "four-state");
        replacement.nets[0].state = LogicStateKind::FourState;

        assert_eq!(
            base.commit(vec![replacement], |_| Ok(())).unwrap_err(),
            DesignError::BoundaryStateMismatch(NetBitId::from_bytes(digest(11)))
        );
    }

    #[test]
    fn ordinary_wave_rejects_overlapping_write_footprints() {
        let base = base_design();
        let first = delta(20, base.revision(), "first");
        let second = delta(30, base.revision(), "second");
        let error = base.commit(vec![second, first], |_| Ok(())).unwrap_err();

        assert!(matches!(error, DesignError::OverlappingReplacement(_)));
        assert_eq!(
            base.cell(CellId::from_bytes(digest(1))).unwrap().kind,
            "not"
        );
    }

    #[test]
    fn ordinary_wave_rejects_duplicate_new_fragment_identity() {
        let base = base_design();
        let added = |id, kind| RewriteDelta {
            id: RewriteDeltaId::from_bytes(digest(id)),
            footprint: footprint(
                base.revision(),
                vec![EntityId::NetBit(NetBitId::from_bytes(digest(10)))],
                vec![],
            ),
            cells: vec![cell(2, kind, &[10], &[12])].into_boxed_slice(),
            nets: vec![net(12, Some((2, 0)))].into_boxed_slice(),
            semantic: SemanticBinding {
                inputs: vec![NetBitId::from_bytes(digest(10))].into_boxed_slice(),
                outputs: Box::new([]),
            },
            proof: EquivalenceCertificate {
                regime: EquivalenceRegime::ByConstruction,
                digest: digest(id + 1),
            },
        };

        assert!(matches!(
            base.commit(vec![added(20, "first"), added(30, "second")], |_| Ok(())),
            Err(DesignError::OverlappingFragmentEntity(_))
        ));
    }

    #[test]
    fn committed_revision_hashes_fragment_payload_content() {
        let base = base_design();
        let first = base
            .commit(vec![delta(20, base.revision(), "first")], |_| Ok(()))
            .unwrap();
        let second = base
            .commit(vec![delta(20, base.revision(), "second")], |_| Ok(()))
            .unwrap();

        assert_ne!(first.revision(), second.revision());
    }

    #[test]
    fn removed_stable_identity_cannot_be_reused() {
        let base = base_design();
        let removed = base
            .commit(
                vec![RewriteDelta {
                    id: RewriteDeltaId::from_bytes(digest(20)),
                    footprint: footprint(
                        base.revision(),
                        vec![
                            EntityId::Cell(CellId::from_bytes(digest(1))),
                            EntityId::NetBit(NetBitId::from_bytes(digest(11))),
                        ],
                        vec![
                            EntityId::Cell(CellId::from_bytes(digest(1))),
                            EntityId::NetBit(NetBitId::from_bytes(digest(11))),
                        ],
                    ),
                    cells: Box::new([]),
                    nets: Box::new([]),
                    semantic: SemanticBinding {
                        inputs: Box::new([]),
                        outputs: Box::new([]),
                    },
                    proof: EquivalenceCertificate {
                        regime: EquivalenceRegime::ByConstruction,
                        digest: digest(21),
                    },
                }],
                |_| Ok(()),
            )
            .unwrap();
        let error = removed
            .commit(
                vec![RewriteDelta {
                    id: RewriteDeltaId::from_bytes(digest(30)),
                    footprint: footprint(
                        removed.revision(),
                        vec![EntityId::NetBit(NetBitId::from_bytes(digest(10)))],
                        vec![],
                    ),
                    cells: vec![cell(1, "reused", &[10], &[11])].into_boxed_slice(),
                    nets: vec![net(11, Some((1, 0)))].into_boxed_slice(),
                    semantic: SemanticBinding {
                        inputs: vec![NetBitId::from_bytes(digest(10))].into_boxed_slice(),
                        outputs: Box::new([]),
                    },
                    proof: EquivalenceCertificate {
                        regime: EquivalenceRegime::ByConstruction,
                        digest: digest(31),
                    },
                }],
                |_| Ok(()),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            DesignError::StableIdCollision(EntityId::Cell(_))
        ));
    }

    #[test]
    fn packet_order_does_not_change_the_committed_revision() {
        let mut builder = DesignBuilder::new(DesignRevisionId::from_bytes(digest(90)));
        builder.add_net(net(10, None));
        builder.add_net(net(11, Some((1, 0))));
        builder.add_net(net(12, None));
        builder.add_net(net(13, Some((3, 0))));
        builder.add_cell(cell(1, "not-a", &[10], &[11]));
        builder.add_cell(cell(3, "not-b", &[12], &[13]));
        let base = builder.seal().unwrap();
        let first = delta(20, base.revision(), "replace-a");
        let second = RewriteDelta {
            id: RewriteDeltaId::from_bytes(digest(30)),
            footprint: footprint(
                base.revision(),
                vec![
                    EntityId::Cell(CellId::from_bytes(digest(3))),
                    EntityId::NetBit(NetBitId::from_bytes(digest(12))),
                    EntityId::NetBit(NetBitId::from_bytes(digest(13))),
                ],
                vec![
                    EntityId::Cell(CellId::from_bytes(digest(3))),
                    EntityId::NetBit(NetBitId::from_bytes(digest(13))),
                ],
            ),
            cells: vec![cell(4, "replace-b", &[12], &[13])].into_boxed_slice(),
            nets: vec![net(13, Some((4, 0)))].into_boxed_slice(),
            semantic: SemanticBinding {
                inputs: vec![NetBitId::from_bytes(digest(12))].into_boxed_slice(),
                outputs: vec![NetBitId::from_bytes(digest(13))].into_boxed_slice(),
            },
            proof: EquivalenceCertificate {
                regime: EquivalenceRegime::Combinational,
                digest: digest(31),
            },
        };

        let forward = base
            .commit(vec![first.clone(), second.clone()], |_| Ok(()))
            .unwrap();
        let reverse = base.commit(vec![second, first], |_| Ok(())).unwrap();
        assert_eq!(forward.revision(), reverse.revision());
        assert_eq!(forward.cell_count(), reverse.cell_count());
        assert_eq!(forward.net_count(), reverse.net_count());
    }
}
