// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Compact identities, revisions, and interned storage shared by Opto.
//!
//! Persistent Opto structures use these allocation primitives. [`ObjectUid`]
//! identifies a logical object for its entire lifetime, while typed arena IDs
//! identify compact slots inside one sealed representation. [`RevisionId`]
//! orders published state, and [`NameTable`] stores each distinct string once
//! so large designs do not duplicate names.
//!
//! The central invariant is that compact IDs are meaningful only in the arena
//! that created them. Persistent references must use a UID or carry the owning
//! revision. Tables expose checkpoints for transactional mutation; rolling back
//! a checkpoint removes only allocations made after it.

mod diagnostic;
mod names;
mod paged;
pub mod resident;
mod rows;

pub use diagnostic::{Diagnostic, DiagnosticLabel, DiagnosticLocation, DiagnosticSource};
pub use names::{NameCheckpoint, NameError, NameId, NameTable};
pub use paged::PagedCowVec;
pub use rows::{PackedRows, PackedRowsBuilder, PackedRowsError, RowArena, RowArenaBuilder};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::num::NonZeroU64;

/// Permanent identity of a user-visible object. UIDs are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ObjectUid(NonZeroU64);

impl ObjectUid {
    /// Construct a UID from its nonzero stored representation.
    #[must_use]
    pub fn from_raw(raw: u64) -> Option<Self> {
        NonZeroU64::new(raw).map(Self)
    }

    /// Return the nonzero stored representation.
    #[must_use]
    pub fn get(self) -> NonZeroU64 {
        self.0
    }

    /// Return the zero-based allocation sequence number.
    #[must_use]
    pub fn sequence(self) -> u64 {
        self.0.get() - 1
    }
}

impl Serialize for ObjectUid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(self.0.get())
    }
}

impl<'de> Deserialize<'de> for ObjectUid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(NonZeroU64::deserialize(deserializer)?))
    }
}

/// Monotonic version of published session state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RevisionId(NonZeroU64);

impl Default for RevisionId {
    fn default() -> Self {
        Self::INITIAL
    }
}

impl RevisionId {
    /// First published revision.
    pub const INITIAL: Self = Self(NonZeroU64::MIN);

    /// Return the nonzero stored representation.
    #[must_use]
    pub fn get(self) -> NonZeroU64 {
        self.0
    }

    /// Advance to the next revision, or report exhaustion.
    ///
    /// # Errors
    ///
    /// Returns [`RevisionExhausted`] when this revision already stores
    /// `u64::MAX` and therefore has no representable successor.
    pub fn next(self) -> Result<Self, RevisionExhausted> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or(RevisionExhausted)
    }
}

impl Serialize for RevisionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(self.0.get())
    }
}

impl<'de> Deserialize<'de> for RevisionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self(NonZeroU64::deserialize(deserializer)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Error returned when the revision counter cannot advance.
pub struct RevisionExhausted;

impl fmt::Display for RevisionExhausted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("session revision space is exhausted")
    }
}

impl std::error::Error for RevisionExhausted {}

#[repr(transparent)]
struct CompactIndex<T> {
    raw: NonZeroU32,
    marker: PhantomData<fn() -> T>,
}

impl<T> CompactIndex<T> {
    const FIRST: Self = Self {
        raw: NonZeroU32::MIN,
        marker: PhantomData,
    };

    fn from_index(index: usize) -> Result<Self, CapacityError> {
        let one_based = index.checked_add(1).ok_or(CapacityError)?;
        let raw = u32::try_from(one_based)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or(CapacityError)?;
        Ok(Self {
            raw,
            marker: PhantomData,
        })
    }

    const fn from_raw(raw: NonZeroU32) -> Self {
        Self {
            raw,
            marker: PhantomData,
        }
    }

    fn index(self) -> usize {
        usize::try_from(self.raw.get() - 1).expect("u32 always fits in usize on supported targets")
    }
}

impl<T> Clone for CompactIndex<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for CompactIndex<T> {}

macro_rules! define_compact_index {
    ($(#[$meta:meta])* $name:ident, $debug_name:literal) => {
        $(#[$meta])*
        #[repr(transparent)]
        pub struct $name<T>(CompactIndex<T>);

        impl<T> $name<T> {
            /// First valid arena identifier.
            pub const FIRST: Self = Self(CompactIndex::FIRST);

            /// Construct an identifier from a zero-based arena index.
            ///
            /// # Errors
            ///
            /// Returns [`CapacityError`] when `index` does not fit the compact
            /// nonzero 32-bit representation.
            pub fn from_index(index: usize) -> Result<Self, CapacityError> {
                CompactIndex::from_index(index).map(Self)
            }

            /// Return the zero-based arena index.
            pub fn index(self) -> usize {
                self.0.index()
            }

            /// Return the nonzero stored representation.
            pub fn get(self) -> NonZeroU32 {
                self.0.raw
            }
        }

        impl<T> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<T> Copy for $name<T> {}

        impl<T> PartialEq for $name<T> {
            fn eq(&self, other: &Self) -> bool {
                self.0.raw == other.0.raw
            }
        }

        impl<T> Eq for $name<T> {}

        impl<T> PartialOrd for $name<T> {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl<T> Ord for $name<T> {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.0.raw.cmp(&other.0.raw)
            }
        }

        impl<T> std::hash::Hash for $name<T> {
            fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                self.0.raw.hash(state);
            }
        }

        impl<T> fmt::Debug for $name<T> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_tuple($debug_name).field(&self.index()).finish()
            }
        }
    };
}

define_compact_index!(
    /// A compact, type-safe index into one dense immutable arena or snapshot.
    DenseId,
    "DenseId"
);

define_compact_index!(
    /// A compact, type-safe identity for an append-only slot that may be tombstoned.
    SlotId,
    "SlotId"
);

impl<T> Serialize for DenseId<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.get().get())
    }
}

impl<'de, T> Deserialize<'de> for DenseId<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = NonZeroU32::deserialize(deserializer)?;
        Ok(Self(CompactIndex::from_raw(raw)))
    }
}

impl<T> Serialize for SlotId<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.get().get())
    }
}

impl<'de, T> Deserialize<'de> for SlotId<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = NonZeroU32::deserialize(deserializer)?;
        Ok(Self(CompactIndex::from_raw(raw)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Error returned when a compact arena exceeds 32-bit capacity.
pub struct CapacityError;

impl fmt::Display for CapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("arena exceeds 32-bit ID capacity")
    }
}

impl std::error::Error for CapacityError {}

#[cfg(test)]
mod tests {
    use super::*;

    enum Item {}

    #[test]
    fn dense_ids_are_zero_based_and_option_sized() {
        let id = DenseId::<Item>::from_index(7).unwrap();
        assert_eq!(id.index(), 7);
        assert_eq!(id.get().get(), 8);
        assert_eq!(
            std::mem::size_of::<Option<DenseId<Item>>>(),
            std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn dense_ids_reject_out_of_range_indices() {
        assert_eq!(
            DenseId::<Item>::from_index(u32::MAX as usize),
            Err(CapacityError)
        );
    }

    #[test]
    fn slot_ids_preserve_indices_and_option_niche() {
        let id = SlotId::<Item>::from_index(11).unwrap();
        assert_eq!(id.index(), 11);
        assert_eq!(id.get().get(), 12);
        assert_eq!(
            std::mem::size_of::<Option<SlotId<Item>>>(),
            std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn object_uids_are_non_zero_and_option_sized() {
        assert!(ObjectUid::from_raw(0).is_none());
        let uid = ObjectUid::from_raw(42).unwrap();
        assert_eq!(uid.get().get(), 42);
        assert_eq!(uid.sequence(), 41);
        assert_eq!(
            std::mem::size_of::<Option<ObjectUid>>(),
            std::mem::size_of::<u64>()
        );
    }

    #[test]
    fn revisions_advance_monotonically() {
        let next = RevisionId::INITIAL.next().unwrap();
        assert_eq!(RevisionId::INITIAL.get().get(), 1);
        assert_eq!(next.get().get(), 2);
    }
}
