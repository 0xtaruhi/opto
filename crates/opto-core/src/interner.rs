// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic dense interning for ordered semantic keys.

use crate::{ArenaIndex, IndexVec};
use std::collections::BTreeMap;

/// Deterministic interner backed by dense values and an ordered reverse index.
///
/// IDs follow first-insertion order. Truncation removes both values and reverse
/// mappings, making the interner suitable for append-only transactional arenas.
#[derive(Debug, Clone)]
pub struct DenseInterner<I, K> {
    values: IndexVec<I, K>,
    ids: BTreeMap<K, I>,
}

impl<I, K> DenseInterner<I, K> {
    /// Creates an empty interner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: IndexVec::new(),
            ids: BTreeMap::new(),
        }
    }

    /// Returns the number of interned keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether no keys have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns keys in dense-ID order.
    #[must_use]
    pub fn values(&self) -> &[K] {
        self.values.as_slice()
    }

    /// Returns the allocated value capacity for resident-memory accounting.
    #[must_use]
    pub fn value_capacity(&self) -> usize {
        self.values.capacity()
    }

    /// Iterates the reverse index's owned keys in semantic order.
    ///
    /// This view exists for resident-memory accounting; semantic traversal
    /// should use [`Self::values`] so it follows dense-ID order.
    #[must_use]
    pub fn reverse_keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.ids.keys()
    }
}

impl<I: ArenaIndex, K: Clone + Ord> DenseInterner<I, K> {
    /// Returns the ID already assigned to `key`.
    #[must_use]
    pub fn find(&self, key: &K) -> Option<I> {
        self.ids.get(key).copied()
    }

    /// Returns the canonical ID for `key`, inserting it when absent.
    ///
    /// # Errors
    ///
    /// Returns the ID type's capacity error when a new dense ID cannot be
    /// represented. Existing keys remain resolvable after the error.
    pub fn intern(&mut self, key: K) -> Result<I, I::Error> {
        if let Some(id) = self.find(&key) {
            return Ok(id);
        }
        let id = self.values.try_push(key.clone())?;
        self.ids.insert(key, id);
        Ok(id)
    }

    /// Resolves an ID allocated by this interner.
    #[must_use]
    pub fn get(&self, id: I) -> Option<&K> {
        self.values.get(id)
    }

    /// Removes every key allocated at or after dense index `len`.
    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
        self.ids.retain(|_, id| id.index() < len);
    }
}

impl<I, K> Default for DenseInterner<I, K> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DenseId;

    enum Word {}
    type WordId = DenseId<Word>;

    #[test]
    fn duplicate_keys_share_ids_and_truncation_removes_reverse_entries() {
        let mut words = DenseInterner::<WordId, _>::new();
        let alpha = words.intern("alpha").unwrap();
        let beta = words.intern("beta").unwrap();
        assert_eq!(words.intern("alpha").unwrap(), alpha);

        words.truncate(1);
        let replacement = words.intern("beta").unwrap();
        assert_eq!(replacement.index(), beta.index());
        assert_eq!(words.values(), &["alpha", "beta"]);
    }
}
