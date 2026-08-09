// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable-order arena storage used by timing constraints.
//!
//! Live rows retain insertion order while removed slots may be reused. Public
//! row views hide slot identities so command semantics never depend on arena
//! reuse.

use serde::de::Error as _;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::num::NonZeroU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct RawSlot(NonZeroU32);

impl RawSlot {
    fn from_index(index: usize) -> Result<Self, crate::TimingError> {
        let raw = u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .ok_or(crate::TimingModelError::Capacity {
                resource: "timing constraint slot",
            })?;
        Ok(Self(raw))
    }

    pub(super) fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

#[derive(Debug, Clone)]
pub(super) struct OrderedArena<T> {
    slots: Vec<OrderedSlot<T>>,
    head: Option<RawSlot>,
    tail: Option<RawSlot>,
    free: Option<RawSlot>,
    len: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ArenaInsertion {
    slot: RawSlot,
    appended: bool,
}

impl ArenaInsertion {
    pub(super) const fn slot(self) -> RawSlot {
        self.slot
    }
}

#[derive(Debug)]
pub(super) struct ArenaRemoval<T> {
    slot: RawSlot,
    value: T,
    previous: Option<RawSlot>,
    next: Option<RawSlot>,
}

impl<T> ArenaRemoval<T> {
    pub(super) const fn slot(&self) -> RawSlot {
        self.slot
    }

    pub(super) const fn value(&self) -> &T {
        &self.value
    }
}

#[derive(Debug, Clone)]
struct OrderedSlot<T> {
    value: Option<T>,
    previous: Option<RawSlot>,
    next: Option<RawSlot>,
    next_free: Option<RawSlot>,
}

impl<T> Default for OrderedArena<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            head: None,
            tail: None,
            free: None,
            len: 0,
        }
    }
}

impl<T> OrderedArena<T> {
    pub(super) fn from_values(values: Vec<T>) -> Result<Self, crate::TimingError> {
        let mut arena = Self {
            slots: Vec::with_capacity(values.len()),
            ..Self::default()
        };
        for value in values {
            arena.insert(value)?;
        }
        Ok(arena)
    }

    pub(super) fn into_values(mut self) -> Vec<T> {
        let mut values = Vec::with_capacity(self.len);
        let mut next = self.head;
        while let Some(slot) = next {
            let stored = &mut self.slots[slot.index()];
            next = stored.next;
            values.push(
                stored
                    .value
                    .take()
                    .expect("the ordered list only links live timing rows"),
            );
        }
        values
    }

    pub(super) fn insert(&mut self, value: T) -> Result<RawSlot, crate::TimingError> {
        Ok(self.insert_tracked(value)?.slot)
    }

    pub(super) fn insert_tracked(
        &mut self,
        value: T,
    ) -> Result<ArenaInsertion, crate::TimingError> {
        let appended = self.free.is_none();
        let slot = if let Some(slot) = self.free {
            let stored = &mut self.slots[slot.index()];
            self.free = stored.next_free;
            stored.value = Some(value);
            stored.previous = self.tail;
            stored.next = None;
            stored.next_free = None;
            slot
        } else {
            let slot = RawSlot::from_index(self.slots.len())?;
            self.slots.push(OrderedSlot {
                value: Some(value),
                previous: self.tail,
                next: None,
                next_free: None,
            });
            slot
        };
        if let Some(tail) = self.tail {
            self.slots[tail.index()].next = Some(slot);
        } else {
            self.head = Some(slot);
        }
        self.tail = Some(slot);
        self.len += 1;
        Ok(ArenaInsertion { slot, appended })
    }

    pub(super) fn get_slot(&self, slot: RawSlot) -> Option<&T> {
        self.slots.get(slot.index())?.value.as_ref()
    }

    pub(super) fn get_slot_mut(&mut self, slot: RawSlot) -> Option<&mut T> {
        self.slots.get_mut(slot.index())?.value.as_mut()
    }

    pub(super) fn replace(&mut self, slot: RawSlot, value: T) -> T {
        self.slots[slot.index()]
            .value
            .replace(value)
            .expect("a prepared timing edit references a live slot")
    }

    pub(super) fn remove(&mut self, slot: RawSlot) -> T {
        self.remove_tracked(slot).value
    }

    pub(super) fn remove_tracked(&mut self, slot: RawSlot) -> ArenaRemoval<T> {
        let stored = &self.slots[slot.index()];
        let previous = stored.previous;
        let next = stored.next;
        if let Some(previous) = previous {
            self.slots[previous.index()].next = next;
        } else {
            self.head = next;
        }
        if let Some(next) = next {
            self.slots[next.index()].previous = previous;
        } else {
            self.tail = previous;
        }
        let stored = &mut self.slots[slot.index()];
        let value = stored
            .value
            .take()
            .expect("a prepared timing edit references a live slot");
        stored.previous = None;
        stored.next = None;
        stored.next_free = self.free;
        self.free = Some(slot);
        self.len -= 1;
        ArenaRemoval {
            slot,
            value,
            previous,
            next,
        }
    }

    pub(super) fn undo_insertion(&mut self, insertion: ArenaInsertion) -> T {
        let value = self.remove(insertion.slot);
        if insertion.appended {
            assert_eq!(self.free, Some(insertion.slot));
            let stored = self
                .slots
                .pop()
                .expect("an appended timing slot remains the arena tail during rollback");
            assert_eq!(insertion.slot.index(), self.slots.len());
            self.free = stored.next_free;
        }
        value
    }

    pub(super) fn restore_removal(&mut self, removal: ArenaRemoval<T>) {
        assert_eq!(self.free, Some(removal.slot));
        let stored = &mut self.slots[removal.slot.index()];
        self.free = stored.next_free;
        stored.value = Some(removal.value);
        stored.previous = removal.previous;
        stored.next = removal.next;
        stored.next_free = None;
        if let Some(previous) = removal.previous {
            self.slots[previous.index()].next = Some(removal.slot);
        } else {
            self.head = Some(removal.slot);
        }
        if let Some(next) = removal.next {
            self.slots[next.index()].previous = Some(removal.slot);
        } else {
            self.tail = Some(removal.slot);
        }
        self.len += 1;
    }

    pub(super) fn iter(&self) -> TimingRowIter<'_, T> {
        TimingRowIter {
            arena: self,
            next: self.head,
            remaining: self.len,
        }
    }

    pub(super) fn entries(&self) -> OrderedEntryIter<'_, T> {
        OrderedEntryIter {
            arena: self,
            next: self.head,
            remaining: self.len,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn owned_memory_bytes(&self, nested: impl Fn(&T) -> usize) -> usize {
        opto_core::resident::slice_bytes::<OrderedSlot<T>>(self.slots.len()).saturating_add(
            self.slots
                .iter()
                .filter_map(|slot| slot.value.as_ref())
                .map(nested)
                .sum::<usize>(),
        )
    }

    #[cfg(test)]
    pub(super) fn slot_capacity(&self) -> usize {
        self.slots.len()
    }
}

impl<T: PartialEq> PartialEq for OrderedArena<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl<T: Serialize> Serialize for OrderedArena<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.len))?;
        for value in self {
            sequence.serialize_element(value)?;
        }
        sequence.end()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OrderedArena<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::from_values(Vec::<T>::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Exact-size iterator over live constraint rows in insertion order.
#[derive(Debug)]
pub struct TimingRowIter<'a, T> {
    arena: &'a OrderedArena<T>,
    next: Option<RawSlot>,
    remaining: usize,
}

impl<T> Clone for TimingRowIter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            arena: self.arena,
            next: self.next,
            remaining: self.remaining,
        }
    }
}

impl<'a, T> Iterator for TimingRowIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.next?;
        let stored = &self.arena.slots[slot.index()];
        self.next = stored.next;
        self.remaining -= 1;
        stored.value.as_ref()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<T> ExactSizeIterator for TimingRowIter<'_, T> {}

impl<'a, T> IntoIterator for &'a OrderedArena<T> {
    type Item = &'a T;
    type IntoIter = TimingRowIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub(super) struct OrderedEntryIter<'a, T> {
    arena: &'a OrderedArena<T>,
    next: Option<RawSlot>,
    remaining: usize,
}

impl<'a, T> Iterator for OrderedEntryIter<'a, T> {
    type Item = (RawSlot, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.next?;
        let stored = &self.arena.slots[slot.index()];
        self.next = stored.next;
        self.remaining -= 1;
        Some((
            slot,
            stored
                .value
                .as_ref()
                .expect("the ordered list only links live timing rows"),
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<T> ExactSizeIterator for OrderedEntryIter<'_, T> {}

/// Cloneable borrowed view of live constraint rows.
#[derive(Debug)]
pub struct TimingRows<'a, T> {
    rows: TimingRowIter<'a, T>,
}

impl<'a, T> TimingRows<'a, T> {
    pub(super) fn new(arena: &'a OrderedArena<T>) -> Self {
        Self { rows: arena.iter() }
    }

    /// Iterates over rows in insertion order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &'a T> + Clone {
        self.rows.clone()
    }

    #[must_use]
    /// Returns the number of live rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    /// Returns `true` when no live rows exist.
    pub fn is_empty(&self) -> bool {
        self.rows.len() == 0
    }
}

impl<T> Clone for TimingRows<'_, T> {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
        }
    }
}

impl<'a, T> IntoIterator for TimingRows<'a, T> {
    type Item = &'a T;
    type IntoIter = TimingRowIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows
    }
}

impl<T: PartialEq> PartialEq for TimingRows<'_, T> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}
