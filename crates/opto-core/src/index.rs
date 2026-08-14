// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Dense arenas whose element type is paired with a typed index.

use crate::{CapacityError, DenseId};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

/// Compact index accepted by [`IndexVec`] and other dense storage primitives.
pub trait ArenaIndex: Copy {
    /// Error produced when the index representation is exhausted.
    type Error;

    /// Constructs an ID from a zero-based arena index.
    ///
    /// # Errors
    ///
    /// Returns the index type's capacity error when `index` is outside its
    /// representable domain.
    fn try_from_index(index: usize) -> Result<Self, Self::Error>;

    /// Returns the zero-based arena index.
    fn index(self) -> usize;
}

impl<Tag> ArenaIndex for DenseId<Tag> {
    type Error = CapacityError;

    fn try_from_index(index: usize) -> Result<Self, Self::Error> {
        Self::from_index(index)
    }

    fn index(self) -> usize {
        self.index()
    }
}

/// Contiguous storage indexed only by its paired typed ID.
///
/// The wrapper has the same allocation and traversal behavior as `Vec<T>`.
/// Its only additional contract is that insertion returns `I`, preventing a
/// neighboring arena's ID from indexing the storage accidentally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IndexVec<I, T> {
    values: Vec<T>,
    #[serde(skip)]
    marker: PhantomData<fn(I) -> I>,
}

impl<I, T> IndexVec<I, T> {
    /// Creates an empty typed arena.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            values: Vec::new(),
            marker: PhantomData,
        }
    }

    /// Creates an empty typed arena with at least `capacity` slots.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            marker: PhantomData,
        }
    }

    /// Returns the number of allocated elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns the allocated element capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.values.capacity()
    }

    /// Returns whether the arena contains no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns all values in dense-ID order.
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.values
    }

    /// Consumes the typed arena and returns its contiguous storage.
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.values
    }

    /// Removes every element whose dense index is at least `len`.
    pub fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
    }

    /// Iterates values in dense-ID order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.values.iter()
    }
}

impl<I: ArenaIndex, T> IndexVec<I, T> {
    /// Appends a value and returns its paired dense ID.
    ///
    /// # Errors
    ///
    /// Returns the index type's capacity error when the new slot cannot be
    /// represented by `I`. The arena is unchanged on error.
    pub fn try_push(&mut self, value: T) -> Result<I, I::Error> {
        let id = I::try_from_index(self.values.len())?;
        self.values.push(value);
        Ok(id)
    }

    /// Resolves an ID allocated by this arena.
    #[must_use]
    pub fn get(&self, id: I) -> Option<&T> {
        self.values.get(id.index())
    }

    /// Resolves an ID allocated by this arena for exclusive access.
    #[must_use]
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.values.get_mut(id.index())
    }

    /// Iterates IDs and values together in dense order.
    ///
    /// # Panics
    ///
    /// Panics only if an `ArenaIndex` implementation accepts an index during
    /// insertion but later rejects the same index.
    #[must_use]
    pub fn iter_enumerated(&self) -> impl ExactSizeIterator<Item = (I, &T)> + '_ {
        self.values.iter().enumerate().map(|(index, value)| {
            let id = I::try_from_index(index)
                .ok()
                .expect("stored index was validated before insertion");
            (id, value)
        })
    }
}

impl<I, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a, I, T> IntoIterator for &'a IndexVec<I, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

impl<I: ArenaIndex, T> Index<I> for IndexVec<I, T> {
    type Output = T;

    fn index(&self, id: I) -> &Self::Output {
        &self.values[id.index()]
    }
}

impl<I: ArenaIndex, T> IndexMut<I> for IndexVec<I, T> {
    fn index_mut(&mut self, id: I) -> &mut Self::Output {
        &mut self.values[id.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Item {}
    type ItemId = DenseId<Item>;

    #[test]
    fn insertion_returns_the_matching_dense_id() {
        let mut values = IndexVec::<ItemId, _>::new();
        let first = values.try_push("first").unwrap();
        let second = values.try_push("second").unwrap();

        assert_eq!(first.index(), 0);
        assert_eq!(values[second], "second");
        assert_eq!(values.iter_enumerated().collect::<Vec<_>>()[0].0, first);
    }
}
