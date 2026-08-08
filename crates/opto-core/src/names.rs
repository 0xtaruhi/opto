// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use hashbrown::{DefaultHashBuilder, HashTable};
use serde::de::{SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::hash::BuildHasher;
use std::mem::size_of;
use std::sync::Arc;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
/// Compact identifier for an interned string.
pub struct NameId(u32);

impl NameId {
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Construct a compact identifier from a zero-based arena index.
    ///
    /// This checks the representation limit; resolving the identifier against
    /// a particular [`NameTable`] still validates arena membership.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] when `index` exceeds the 32-bit identifier range.
    pub fn from_index(index: usize) -> Result<Self, NameError> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| NameError::capacity("identifier"))
    }

    #[must_use]
    /// Return the zero-based stored identifier.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Error returned by name-table capacity or checkpoint validation.
pub struct NameError(String);

impl NameError {
    fn capacity(kind: &str) -> Self {
        Self(format!("name table exceeds {kind} capacity"))
    }
}

impl fmt::Display for NameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NameError {}

#[derive(Debug, Clone)]
/// Append-oriented string interner with shared immutable storage.
pub struct NameTable {
    frozen: Arc<NameStore>,
    delta: NameStore,
}

impl PartialEq for NameTable {
    fn eq(&self, other: &Self) -> bool {
        self.entry_count() == other.entry_count()
            && (0..self.entry_count()).all(|raw| {
                let raw = u32::try_from(raw).expect("validated name table length fits in u32");
                self.resolve(NameId::from_raw(raw)) == other.resolve(NameId::from_raw(raw))
            })
    }
}

impl Eq for NameTable {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Rollback point for the mutable delta of a name table.
pub struct NameCheckpoint {
    frozen_entries: usize,
    delta_entries: usize,
}

impl Default for NameTable {
    fn default() -> Self {
        Self::new()
    }
}

impl NameTable {
    /// Construct a table containing the reserved empty name.
    ///
    /// # Panics
    ///
    /// Panics only if inserting the first, empty string into an empty table
    /// violates the table's internal capacity invariant.
    #[must_use]
    pub fn new() -> Self {
        let mut frozen = NameStore::new(0);
        let empty = frozen
            .insert("")
            .expect("the reserved empty name must fit in an empty name table");
        debug_assert_eq!(empty, NameId::default());
        Self {
            frozen: Arc::new(frozen),
            delta: NameStore::new(1),
        }
    }

    #[must_use]
    /// Number of interned names, including the reserved empty name.
    pub fn entry_count(&self) -> usize {
        self.frozen.len() + self.delta.len()
    }

    /// Approximate bytes occupied by stored string contents.
    #[must_use]
    pub fn stored_bytes(&self) -> usize {
        self.frozen.stored_bytes() + self.delta.stored_bytes()
    }

    /// Compacts the allocation-backed storage of a sealed table without
    /// changing its IDs. Shared frozen storage remains shared; uniquely owned
    /// storage and the mutable delta release spare vector/hash capacity.
    pub fn compact(&mut self) {
        if let Some(frozen) = Arc::get_mut(&mut self.frozen) {
            frozen.compact();
        }
        self.delta.compact();
    }

    /// Deterministic byte model for storage owned by this table, excluding the
    /// inline [`NameTable`] value. The model uses live payload lengths rather
    /// than allocator-dependent capacities and adds 25% slack plus two words
    /// of metadata per modeled allocation.
    #[must_use]
    pub fn owned_memory_bytes(&self) -> usize {
        let frozen_allocation = crate::resident::allocation_bytes(
            size_of::<NameStore>().saturating_add(size_of::<usize>() * 2),
        );
        frozen_allocation
            .saturating_add(self.frozen.owned_memory_bytes())
            .saturating_add(self.delta.owned_memory_bytes())
    }

    /// Look up an existing interned name without modifying the table.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<NameId> {
        if self.delta.is_empty() {
            self.frozen.get(name)
        } else {
            self.delta.get(name).or_else(|| self.frozen.get(name))
        }
    }

    /// Intern a name and return its stable identifier.
    ///
    /// Existing names retain their IDs and do not allocate. New names are
    /// appended to the mutable delta.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if another name or its bytes would exceed the
    /// compact table representation.
    pub fn intern(&mut self, name: &str) -> Result<NameId, NameError> {
        if let Some(id) = self.get(name) {
            return Ok(id);
        }
        self.delta.insert(name)
    }

    #[must_use]
    /// Capture a rollback point for the current mutable delta.
    pub fn checkpoint(&self) -> NameCheckpoint {
        NameCheckpoint {
            frozen_entries: self.frozen.len(),
            delta_entries: self.delta.len(),
        }
    }

    /// Discard names added after a checkpoint from this revision.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if `checkpoint` belongs to a different frozen
    /// revision or lies beyond the current mutable delta.
    pub fn rollback(&mut self, checkpoint: NameCheckpoint) -> Result<(), NameError> {
        self.validate_checkpoint(checkpoint)?;
        self.delta.truncate(checkpoint.delta_entries);
        Ok(())
    }

    /// Validates a rollback point without changing the table.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if `checkpoint` was captured from a different
    /// frozen revision or after the current delta was truncated.
    pub fn validate_checkpoint(&self, checkpoint: NameCheckpoint) -> Result<(), NameError> {
        if self.frozen.len() != checkpoint.frozen_entries
            || checkpoint.delta_entries > self.delta.len()
        {
            return Err(NameError(
                "name checkpoint does not belong to the current mutable revision".to_string(),
            ));
        }
        Ok(())
    }

    /// Resolve an identifier to its interned string.
    #[must_use]
    pub fn resolve(&self, id: NameId) -> Option<&str> {
        self.frozen.resolve(id).or_else(|| self.delta.resolve(id))
    }

    /// Merge the mutable delta into immutable shared storage.
    ///
    /// # Errors
    ///
    /// Returns [`NameError`] if the merged entry count or byte arena exceeds
    /// the compact representation.
    ///
    /// # Panics
    ///
    /// Panics if the table's internal ID ranges are not contiguous. Public
    /// mutation APIs preserve this invariant.
    pub fn freeze(&mut self) -> Result<(), NameError> {
        if self.delta.is_empty() {
            return Ok(());
        }
        let total_names = self.entry_count();
        let total_bytes = self.stored_bytes();
        let mut merged = NameStore::with_capacity(0, total_names, total_bytes);
        for raw in 0..self.frozen.end_id() {
            let id = NameId::from_raw(raw);
            let name = self
                .frozen
                .resolve(id)
                .expect("frozen name IDs must be contiguous");
            let merged_id = merged.insert(name)?;
            debug_assert_eq!(merged_id, id);
        }
        for raw in self.delta.first_id..self.delta.end_id() {
            let id = NameId::from_raw(raw);
            let name = self
                .delta
                .resolve(id)
                .expect("delta name IDs must be contiguous");
            let merged_id = merged.insert(name)?;
            debug_assert_eq!(merged_id, id);
        }
        let next_id = merged.end_id();
        self.frozen = Arc::new(merged);
        self.delta = NameStore::new(next_id);
        Ok(())
    }

    #[cfg(test)]
    fn frozen_storage_is_shared_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.frozen, &other.frozen)
    }
}

impl Serialize for NameTable {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        struct Names<'a>(&'a NameTable);

        impl Serialize for Names<'_> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let mut sequence = serializer.serialize_seq(Some(self.0.entry_count()))?;
                for raw in
                    0..u32::try_from(self.0.entry_count()).map_err(serde::ser::Error::custom)?
                {
                    let name = self.0.resolve(NameId::from_raw(raw)).ok_or_else(|| {
                        serde::ser::Error::custom("name table contains a missing ID")
                    })?;
                    sequence.serialize_element(name)?;
                }
                sequence.end()
            }
        }

        serializer.serialize_newtype_struct(crate::resident::NAME_TABLE_WIRE_NAME, &Names(self))
    }
}

impl<'de> Deserialize<'de> for NameTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NamesVisitor;

        impl<'de> Visitor<'de> for NamesVisitor {
            type Value = NameTable;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence of unique names in ID order")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut store = NameStore::new(0);
                let mut expected = 0usize;
                let mut stored_bytes = 0usize;
                while let Some(name) = sequence.next_element::<String>()? {
                    if expected == 0 && !name.is_empty() {
                        return Err(serde::de::Error::custom(
                            "serialized name table is missing the reserved empty name",
                        ));
                    }
                    stored_bytes = stored_bytes.checked_add(name.len()).ok_or_else(|| {
                        serde::de::Error::custom("serialized name table exceeds byte capacity")
                    })?;
                    let id = store.insert(&name).map_err(serde::de::Error::custom)?;
                    if id.raw() as usize != expected {
                        return Err(serde::de::Error::custom(
                            "serialized name table contains duplicate or reordered names",
                        ));
                    }
                    expected += 1;
                }
                if expected == 0 {
                    return Err(serde::de::Error::custom(
                        "serialized name table is missing the reserved empty name",
                    ));
                }
                debug_assert_eq!(store.stored_bytes(), stored_bytes);
                let next_id = store.end_id();
                Ok(NameTable {
                    frozen: Arc::new(store),
                    delta: NameStore::new(next_id),
                })
            }
        }

        struct NameTableVisitor;

        impl<'de> Visitor<'de> for NameTableVisitor {
            type Value = NameTable;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a compact name-table wire value")
            }

            fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                deserializer.deserialize_seq(NamesVisitor)
            }
        }

        deserializer
            .deserialize_newtype_struct(crate::resident::NAME_TABLE_WIRE_NAME, NameTableVisitor)
    }
}

#[derive(Debug, Clone, Copy)]
struct NameEntry {
    start: u32,
    len: u32,
    hash: u64,
}

#[derive(Debug, Clone)]
struct NameStore {
    first_id: u32,
    bytes: Vec<u8>,
    entries: Vec<NameEntry>,
    index: HashTable<NameId>,
    hash_builder: DefaultHashBuilder,
}

impl NameStore {
    fn new(first_id: u32) -> Self {
        Self::with_capacity(first_id, 0, 0)
    }

    fn with_capacity(first_id: u32, names: usize, bytes: usize) -> Self {
        Self {
            first_id,
            bytes: Vec::with_capacity(bytes),
            entries: Vec::with_capacity(names),
            index: HashTable::with_capacity(names),
            hash_builder: DefaultHashBuilder::default(),
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn stored_bytes(&self) -> usize {
        self.bytes.len()
    }

    fn compact(&mut self) {
        self.bytes.shrink_to_fit();
        self.entries.shrink_to_fit();
        let mut index = HashTable::with_capacity(self.entries.len());
        let first_id = self.first_id;
        for (local, entry) in self.entries.iter().enumerate() {
            let local = u32::try_from(local).expect("name-store entry count fits in u32");
            let id = NameId::from_raw(first_id + local);
            let entries = &self.entries;
            index.insert_unique(entry.hash, id, |stored_id| {
                entries[(stored_id.raw() - first_id) as usize].hash
            });
        }
        self.index = index;
    }

    fn owned_memory_bytes(&self) -> usize {
        // HashTable's physical bucket count is an implementation detail. Two
        // logical slots per live entry conservatively model its load-factor
        // slack and control bytes without making capacity checkpoint state.
        let hash_slot_bytes = size_of::<NameId>().saturating_add(1);
        let hash_bytes = self
            .entries
            .len()
            .saturating_mul(2)
            .saturating_mul(hash_slot_bytes);
        crate::resident::slice_bytes::<u8>(self.bytes.len())
            .saturating_add(crate::resident::slice_bytes::<NameEntry>(
                self.entries.len(),
            ))
            .saturating_add(crate::resident::allocation_bytes(hash_bytes))
    }

    fn end_id(&self) -> u32 {
        let len =
            u32::try_from(self.entries.len()).expect("validated name table length must fit in u32");
        self.first_id
            .checked_add(len)
            .expect("validated name table length must fit in u32")
    }

    fn get(&self, name: &str) -> Option<NameId> {
        let hash = self.hash_builder.hash_one(name);
        self.index
            .find(hash, |id| self.resolve(*id) == Some(name))
            .copied()
    }

    fn resolve(&self, id: NameId) -> Option<&str> {
        let local = id.raw().checked_sub(self.first_id)? as usize;
        let entry = self.entries.get(local)?;
        let start = entry.start as usize;
        let end = start.checked_add(entry.len as usize)?;
        std::str::from_utf8(self.bytes.get(start..end)?).ok()
    }

    fn insert(&mut self, name: &str) -> Result<NameId, NameError> {
        if let Some(id) = self.get(name) {
            return Ok(id);
        }
        let local_id =
            u32::try_from(self.entries.len()).map_err(|_| NameError::capacity("name ID"))?;
        let id = NameId::from_raw(
            self.first_id
                .checked_add(local_id)
                .ok_or_else(|| NameError::capacity("name ID"))?,
        );
        if id.raw() == u32::MAX {
            return Err(NameError::capacity("name ID"));
        }
        let start =
            u32::try_from(self.bytes.len()).map_err(|_| NameError::capacity("UTF-8 byte arena"))?;
        let len =
            u32::try_from(name.len()).map_err(|_| NameError::capacity("individual name length"))?;
        start
            .checked_add(len)
            .ok_or_else(|| NameError::capacity("UTF-8 byte arena"))?;
        let hash = self.hash_builder.hash_one(name);
        self.bytes.extend_from_slice(name.as_bytes());
        self.entries.push(NameEntry { start, len, hash });
        let first_id = self.first_id;
        let entries = &self.entries;
        self.index.insert_unique(hash, id, |stored_id| {
            entries[(stored_id.raw() - first_id) as usize].hash
        });
        Ok(id)
    }

    fn truncate(&mut self, len: usize) {
        if len == self.entries.len() {
            return;
        }
        debug_assert!(len < self.entries.len());
        for local in (len..self.entries.len()).rev() {
            let local_id = u32::try_from(local).expect("name-store entry count fits in u32");
            let id = NameId::from_raw(self.first_id + local_id);
            let hash = self.entries[local].hash;
            let removed = self
                .index
                .find_entry(hash, |stored| *stored == id)
                .expect("name index must contain every stored entry")
                .remove()
                .0;
            debug_assert_eq!(removed, id);
        }
        let byte_len = len
            .checked_sub(1)
            .and_then(|index| self.entries.get(index))
            .map_or(0, |entry| (entry.start as usize) + (entry.len as usize));
        self.entries.truncate(len);
        self.bytes.truncate(byte_len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_names_into_contiguous_storage() {
        let mut table = NameTable::new();
        let alpha = table.intern("alpha").unwrap();
        let beta = table.intern("beta").unwrap();

        assert_eq!(NameId::default().raw(), 0);
        assert_eq!(alpha.raw(), 1);
        assert_eq!(beta.raw(), 2);
        assert_eq!(table.intern("alpha").unwrap(), alpha);
        assert_eq!(table.resolve(alpha), Some("alpha"));
        assert_eq!(table.get("missing"), None);
        assert_eq!(table.stored_bytes(), "alpha".len() + "beta".len());
        assert_eq!(std::mem::size_of::<NameId>(), std::mem::size_of::<u32>());
    }

    #[test]
    fn rollback_removes_only_names_after_the_checkpoint() {
        let mut table = NameTable::new();
        let retained = (0..10_000)
            .map(|index| table.intern(&format!("retained_{index}")).unwrap())
            .collect::<Vec<_>>();
        let checkpoint = table.checkpoint();

        for iteration in 0..100 {
            let speculative = table.intern(&format!("speculative_{iteration}")).unwrap();
            assert_eq!(
                table.resolve(speculative),
                Some(format!("speculative_{iteration}").as_str())
            );
            table.rollback(checkpoint).unwrap();
            assert_eq!(table.get(&format!("speculative_{iteration}")), None);
        }

        for (index, id) in retained.into_iter().enumerate() {
            assert_eq!(table.get(&format!("retained_{index}")), Some(id));
        }
    }

    #[test]
    fn frozen_clones_share_storage_and_keep_ids_stable() {
        let mut table = NameTable::new();
        let alpha = table.intern("alpha").unwrap();
        table.freeze().unwrap();
        let mut clone = table.clone();

        assert!(table.frozen_storage_is_shared_with(&clone));
        let beta = clone.intern("beta").unwrap();
        clone.freeze().unwrap();

        assert_eq!(clone.resolve(alpha), Some("alpha"));
        assert_eq!(clone.resolve(beta), Some("beta"));
        assert_eq!(table.get("beta"), None);
    }

    #[test]
    fn frozen_table_supports_parallel_lock_free_reads() {
        let mut table = NameTable::new();
        let ids = (0..4096)
            .map(|index| table.intern(&format!("signal_{index}")))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        table.freeze().unwrap();

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for (index, id) in ids.iter().copied().enumerate() {
                        assert_eq!(table.resolve(id), Some(format!("signal_{index}").as_str()));
                    }
                });
            }
        });
    }

    #[test]
    fn serde_round_trip_preserves_name_ids() {
        let mut table = NameTable::new();
        let name = table.intern("data").unwrap();
        table.freeze().unwrap();

        let encoded = serde_json::to_string(&table).unwrap();
        let decoded: NameTable = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.resolve(name), Some("data"));
        assert_eq!(decoded.get("data"), Some(name));
    }

    #[test]
    fn serde_rejects_a_missing_reserved_name() {
        let error = serde_json::from_str::<NameTable>("[]").unwrap_err();
        assert!(error.to_string().contains("reserved empty name"));
    }
}
