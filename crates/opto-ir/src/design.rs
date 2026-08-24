// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable structural identities and exact task footprints.
//!
//! The typed Word module remains the semantic design authority. This module
//! deliberately does not retain a second whole-design cell/net database.
//! Stable cell and bit identities are derived where a work graph or private
//! rewrite needs them. Word remains the only structural topology.

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
/// Proof-carrying boundary of a Word replacement transaction.
///
/// Word publication validates the changed topology. The delta records only the
/// stable identity, exact footprint, semantic boundary, and proof needed by the
/// coordinator, avoiding a second representation of the same fragment.
pub struct RewriteDelta {
    /// Stable transaction identity covering the generating recipe.
    pub id: RewriteDeltaId,
    /// Immutable base generation and exact read/replacement closure.
    pub footprint: RevisionFootprint,
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
        assert!(matches!(
            EntitySet::new(vec![cell, cell]),
            Err(DesignError::DuplicateFootprintEntity(entity)) if entity == cell
        ));
    }
}
