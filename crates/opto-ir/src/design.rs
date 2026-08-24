// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable structural identities, exact task footprints, and delta contracts.
//!
//! The typed Word module remains the semantic design authority. This module
//! deliberately does not retain a second whole-design cell/net database.
//! Stable cell and bit identities are derived where a work graph or private
//! rewrite needs them; only delta-local topology crosses the transaction
//! boundary.

use crate::word::{LogicStateKind, SourceSpan};
use thiserror::Error;

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
pub struct RevisionFootprint<S = EntitySet> {
    /// Immutable semantic generation read by the worker.
    pub base: DesignRevisionId,
    /// Every stable entity whose structure influenced the result.
    pub reads: S,
    /// Complete stable mutation footprint.
    pub replaces: S,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Exact driver endpoint of one delta-local net bit.
pub enum NetDriver {
    /// Output pin of a delta-local cell.
    Cell {
        /// Stable producing cell identity.
        cell: CellId,
        /// Zero-based output ordinal in [`Cell::outputs`].
        output: u32,
    },
    /// Canonical source-domain constant.
    Constant(crate::BitVal),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Scalar net retained only inside a proposed replacement fragment.
pub struct NetBit {
    /// Stable net identity.
    pub id: NetBitId,
    /// Two-state or four-state scalar domain.
    pub state: LogicStateKind,
    /// Absent for a fragment input; present for an internally driven net.
    pub driver: Option<NetDriver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Whether a delta-local cell propagates input changes to its outputs.
pub enum CellClass {
    /// Every output may depend combinationally on the inputs.
    Combinational,
    /// State or memory breaks combinational reachability.
    StateBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Delta-local cell with exact stable input and output bit identities.
pub struct Cell<L> {
    /// Stable cell identity independent of work shard and local record slot.
    pub id: CellId,
    /// Fragment-specific cell role.
    pub kind: L,
    /// Combinational-cycle classification.
    pub class: CellClass,
    /// Input net bits in semantic operand order.
    pub inputs: Box<[NetBitId]>,
    /// Output net bits in semantic result order.
    pub outputs: Box<[NetBitId]>,
    /// Source construct responsible for the cell.
    pub source: SourceSpan,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Exact public interface represented by a replacement fragment.
pub struct SemanticBinding {
    /// Read-only boundary bits consumed by the fragment.
    pub inputs: Box<[NetBitId]>,
    /// Stable boundary bits driven by the fragment after commit.
    pub outputs: Box<[NetBitId]>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Complete proof-carrying replacement proposed against one semantic revision.
///
/// `cells` and `nets` contain only the changed fragment. The coordinator checks
/// its stable footprint and boundary while the Word transaction publishes the
/// corresponding typed fragment.
pub struct RewriteDelta<L> {
    /// Stable transaction identity covering the generating recipe.
    pub id: RewriteDeltaId,
    /// Immutable base generation and exact read/replacement closure.
    pub footprint: RevisionFootprint,
    /// Delta-local cells.
    pub cells: Box<[Cell<L>]>,
    /// Delta-local net bits.
    pub nets: Box<[NetBit]>,
    /// Exact stable boundary interface.
    pub semantic: SemanticBinding,
    /// Equivalence evidence required before publication.
    pub proof: EquivalenceCertificate,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Footprint or proof validation failure.
pub enum DesignError {
    /// A footprint repeats one entity.
    #[error("design footprint repeats entity {0:?}")]
    DuplicateFootprintEntity(EntityId),
    /// The owning proof engine rejected a transaction certificate.
    #[error("rewrite equivalence certificate was rejected: {0}")]
    ProofRejected(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_sets_are_canonical_and_exact() {
        let cell = EntityId::Cell(CellId::from_bytes([1; 32]));
        let net = EntityId::NetBit(NetBitId::from_bytes([2; 32]));
        let set = EntitySet::new(vec![net, cell]).unwrap();

        assert_eq!(set.as_slice(), &[cell, net]);
        assert!(set.contains(cell));
        assert!(!set.is_empty());
        assert!(matches!(
            EntitySet::new(vec![cell, cell]),
            Err(DesignError::DuplicateFootprintEntity(entity)) if entity == cell
        ));
    }
}
