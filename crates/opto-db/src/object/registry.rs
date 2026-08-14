// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Mutable storage and indexes for persistent design-object identities.
//!
//! The registry owns canonical object names, monotonically allocated UIDs, and
//! derived lookup indexes. Removing or rolling back an object never reuses its
//! UID, which prevents stale IDs from silently naming a different object.

use super::{
    AnyObjectId, Deserialize, NameError, NameId, NameTable, ObjectIdSet, ObjectKey, ObjectLocator,
    ObjectUid, ResolvedObject, Serialize, fmt,
};
use opto_core::{NameCheckpoint, OwnerToken};
use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroU32;

mod reconcile;
mod snapshot;

pub use reconcile::{
    ObjectReconcileDesign, ObjectReconcileMode, ObjectReconcileSource, ObjectRegistryReconcilePlan,
    ObjectRemovalView, PreparedObjectReconcile,
};
#[cfg(test)]
pub(super) use snapshot::SnapshotRecord;
pub use snapshot::{ObjectRegistrySnapshot, ObjectRegistrySnapshotRef};

pub(super) enum ObjectRegistryOwner {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(super) struct LiveSlot(NonZeroU32);

impl LiveSlot {
    fn from_index(index: usize) -> Result<Self, RegistryError> {
        u32::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .and_then(NonZeroU32::new)
            .map(Self)
            .ok_or(RegistryError::Capacity {
                resource: "live object slots",
            })
    }

    const fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

/// Compact arena marker for read-only validation of one live registry object.
///
/// Markers are process-local and may be reused after any registry mutation.
/// Persistent references must continue to use [`AnyObjectId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ObjectRegistryMarker(u32);

impl ObjectRegistryMarker {
    const fn from_slot(slot: LiveSlot) -> Self {
        Self(slot.0.get() - 1)
    }

    /// Returns the zero-based marker index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(super) struct DesignPosition(u32);

impl DesignPosition {
    pub(super) fn from_index(index: usize) -> Result<Self, RegistryError> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| RegistryError::Capacity {
                resource: "per-design object slots",
            })
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LiveRecord {
    pub(super) uid: ObjectUid,
    pub(super) key: ObjectKey,
    pub(super) design_position: Option<DesignPosition>,
}

impl LiveRecord {
    fn id(self) -> AnyObjectId {
        AnyObjectId::from_class(self.uid, self.key.class())
    }
}

#[derive(Debug)]
pub(super) struct ArenaSlot {
    record: Option<LiveRecord>,
    previous: Option<LiveSlot>,
    next: Option<LiveSlot>,
    next_free: Option<LiveSlot>,
}

#[derive(Debug, Default)]
/// Registry of live design objects and their persistent identities.
///
/// Insertion is idempotent by [`ObjectLocator`]. Iteration and snapshots retain
/// monotonically increasing UID order, independent of arena-slot reuse.
pub struct ObjectRegistry {
    owner: OwnerToken<ObjectRegistryOwner>,
    pub(super) next_uid: u64,
    pub(super) names: NameTable,
    pub(super) slots: Vec<ArenaSlot>,
    head: Option<LiveSlot>,
    tail: Option<LiveSlot>,
    free: Option<LiveSlot>,
    pub(super) len: usize,
    pub(super) slots_by_uid: HashMap<ObjectUid, LiveSlot>,
    active: HashMap<ObjectKey, AnyObjectId>,
    pub(super) by_design: HashMap<NameId, Vec<LiveSlot>>,
}

struct LiveRecordIter<'a> {
    registry: &'a ObjectRegistry,
    next: Option<LiveSlot>,
    remaining: usize,
}

impl<'a> Iterator for LiveRecordIter<'a> {
    type Item = (LiveSlot, &'a LiveRecord);

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.next?;
        let stored = &self.registry.slots[slot.index()];
        self.next = stored.next;
        self.remaining -= 1;
        Some((
            slot,
            stored
                .record
                .as_ref()
                .expect("live registry order only references occupied slots"),
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for LiveRecordIter<'_> {}

/// Constant-size rollback point for transactions that only create objects.
///
/// UIDs allocated after this point remain permanently consumed after rollback;
/// only their unpublished live records and names are discarded.
#[derive(Debug, Clone, Copy)]
pub struct ObjectRegistryCheckpoint {
    names: NameCheckpoint,
    next_uid: u64,
}

impl ObjectRegistry {
    /// Captures a constant-size rollback point for subsequent insertions.
    #[must_use]
    pub fn checkpoint(&self) -> ObjectRegistryCheckpoint {
        ObjectRegistryCheckpoint {
            names: self.names.checkpoint(),
            next_uid: self.next_uid,
        }
    }

    /// Removes objects created after `checkpoint` and restores the name table.
    ///
    /// Consumed UIDs are not reused.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::InvalidCheckpoint`] if the checkpoint is newer
    /// than this registry or belongs to an incompatible name-table revision.
    ///
    /// # Panics
    ///
    /// Panics only if a UID enumerated from the validated nonzero checkpoint
    /// range cannot be reconstructed as [`ObjectUid`].
    pub fn rollback(&mut self, checkpoint: ObjectRegistryCheckpoint) -> Result<(), RegistryError> {
        self.validate_checkpoint(checkpoint)?;
        if let Some(first_transient) = checkpoint.next_uid.checked_add(1) {
            for raw in (first_transient..=self.next_uid).rev() {
                let uid =
                    ObjectUid::from_raw(raw).expect("post-checkpoint object UIDs are nonzero");
                if let Some(&slot) = self.slots_by_uid.get(&uid) {
                    self.remove_slot(slot);
                }
            }
        }
        self.names
            .rollback(checkpoint.names)
            .map_err(|_| RegistryError::InvalidCheckpoint)?;
        Ok(())
    }

    /// Validates a rollback point without changing the registry.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::InvalidCheckpoint`] if the checkpoint is newer
    /// than this registry or incompatible with its name-table history.
    pub fn validate_checkpoint(
        &self,
        checkpoint: ObjectRegistryCheckpoint,
    ) -> Result<(), RegistryError> {
        if checkpoint.next_uid > self.next_uid
            || self.names.validate_checkpoint(checkpoint.names).is_err()
        {
            Err(RegistryError::InvalidCheckpoint)
        } else {
            Ok(())
        }
    }

    /// Looks up a live object by an owned locator.
    #[must_use]
    pub fn get(&self, locator: &ObjectLocator) -> Option<AnyObjectId> {
        let key = ObjectKey::lookup(locator, &self.names)?;
        self.active.get(&key).copied()
    }

    /// Resolves a borrowed locator without allocating an owned
    /// [`ObjectLocator`].
    #[must_use]
    pub fn get_resolved(&self, locator: ResolvedObject<'_>) -> Option<AnyObjectId> {
        let key = ObjectKey::lookup_resolved(locator, &self.names)?;
        self.active.get(&key).copied()
    }

    /// Returns a compact marker for a borrowed live locator.
    ///
    /// The marker is suitable for a temporary bitset or byte map while `self`
    /// remains immutably borrowed. It must not survive a registry mutation.
    #[must_use]
    pub fn resolved_marker(&self, locator: ResolvedObject<'_>) -> Option<ObjectRegistryMarker> {
        let key = ObjectKey::lookup_resolved(locator, &self.names)?;
        let id = self.active.get(&key)?;
        self.slots_by_uid
            .get(&id.uid())
            .copied()
            .map(ObjectRegistryMarker::from_slot)
    }

    /// Returns the byte-map capacity required to index every current marker.
    #[must_use]
    pub fn marker_capacity(&self) -> usize {
        self.slots.len()
    }

    /// Visits borrowed live locators in stable UID order with compact markers.
    ///
    /// This is intended for whole-registry ownership validation. Hot object
    /// queries should use the indexed lookup APIs.
    ///
    /// # Panics
    ///
    /// Panics only if a live record contains an object key whose interned names
    /// no longer resolve; registry edits maintain both indexes atomically.
    #[must_use]
    pub fn live_resolved(
        &self,
    ) -> impl ExactSizeIterator<Item = (ObjectRegistryMarker, ResolvedObject<'_>)> + '_ {
        self.live_records().map(|(slot, record)| {
            let object = record
                .key
                .resolve(&self.names)
                .expect("live object keys only reference interned names");
            (ObjectRegistryMarker::from_slot(slot), object)
        })
    }

    /// Returns the existing ID for `locator` or inserts a new live object.
    ///
    /// # Errors
    ///
    /// Returns an error when the UID space, compact indexes, or name table
    /// cannot represent the new object. Failed insertion leaves the registry
    /// and its name table unchanged.
    pub fn intern(
        &mut self,
        locator: impl std::borrow::Borrow<ObjectLocator>,
    ) -> Result<AnyObjectId, RegistryError> {
        let locator = locator.borrow();
        if let Some(key) = ObjectKey::lookup(locator, &self.names)
            && let Some(id) = self.active.get(&key)
        {
            return Ok(*id);
        }
        self.insert_with(|names| ObjectKey::intern(locator, names))
    }

    /// Returns the existing ID for a borrowed locator or inserts it without
    /// first materializing an owned [`ObjectLocator`].
    ///
    /// # Errors
    ///
    /// Returns an error when the UID space, compact indexes, or name table
    /// cannot represent the new object. Failed insertion leaves the registry
    /// and its name table unchanged.
    pub fn intern_resolved(
        &mut self,
        locator: ResolvedObject<'_>,
    ) -> Result<AnyObjectId, RegistryError> {
        if let Some(key) = ObjectKey::lookup_resolved(locator, &self.names)
            && let Some(id) = self.active.get(&key)
        {
            return Ok(*id);
        }
        self.insert_with(|names| ObjectKey::intern_resolved(locator, names))
    }

    fn insert_with(
        &mut self,
        build_key: impl FnOnce(&mut NameTable) -> Result<ObjectKey, NameError>,
    ) -> Result<AnyObjectId, RegistryError> {
        let raw = self
            .next_uid
            .checked_add(1)
            .ok_or(RegistryError::UidExhausted)?;
        let uid = ObjectUid::from_raw(raw).ok_or(RegistryError::UidExhausted)?;

        let checkpoint = self.names.checkpoint();
        let prepared = (|| {
            let key = build_key(&mut self.names).map_err(RegistryError::Name)?;
            self.validate_push_capacity(key)?;
            Ok::<_, RegistryError>(key)
        })();
        let key = match prepared {
            Ok(key) => key,
            Err(error) => {
                self.names
                    .rollback(checkpoint)
                    .expect("name-table rollback must accept its immediate checkpoint");
                return Err(error);
            }
        };
        let id = self
            .push_live(uid, key)
            .expect("object capacity was validated before insertion");
        self.next_uid = raw;
        Ok(id)
    }

    /// Resolves a live ID to a borrowed canonical locator.
    ///
    /// Returns `None` for removed, unknown, or class-mismatched IDs.
    #[must_use]
    pub fn resolve(&self, id: AnyObjectId) -> Option<ResolvedObject<'_>> {
        let record = self.record(id.uid())?;
        (record.key.class() == id.class())
            .then(|| record.key.resolve(&self.names))
            .flatten()
    }

    /// Visits live object IDs owned by one design without walking the
    /// registry-wide UID arena or materializing a second ID set.
    pub fn design_objects(&self, design: &str) -> impl Iterator<Item = AnyObjectId> + '_ {
        self.design_slots(design)
            .into_iter()
            .flatten()
            .map(|slot| self.record_at(*slot).id())
    }

    /// Applies one preplanned remove/add edit atomically with respect to
    /// recoverable errors.
    ///
    /// All names and UID capacity are validated before any live object is
    /// tombstoned. Once validation succeeds the remaining operations are
    /// infallible ownership moves, so callers do not need a registry-wide
    /// rollback snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for stale removals, duplicate/conflicting
    /// additions, name/UID capacity failure, or an invalid prepared key.
    /// Recoverable failures leave live-object state unchanged.
    ///
    /// # Panics
    ///
    /// Panics only if the preflighted UID/index capacities change during the
    /// immediately following single-threaded commit phase.
    pub fn apply_edit(
        &mut self,
        removed: &BTreeSet<AnyObjectId>,
        additions: impl IntoIterator<Item = ObjectLocator>,
    ) -> Result<(), RegistryError> {
        if let Some(stale) = removed.iter().find(|id| self.resolve(**id).is_none()) {
            return Err(RegistryError::InvalidEdit(format!(
                "object {stale:?} is not live"
            )));
        }

        let names_checkpoint = self.names.checkpoint();
        let prepared = (|| {
            let locators = additions.into_iter().collect::<BTreeSet<_>>();
            let mut additions = Vec::with_capacity(locators.len());
            for locator in locators {
                let key =
                    ObjectKey::intern(&locator, &mut self.names).map_err(RegistryError::Name)?;
                if self.active.get(&key).is_none_or(|id| removed.contains(id)) {
                    additions.push(key);
                }
            }
            let added = u64::try_from(additions.len()).map_err(|_| RegistryError::UidExhausted)?;
            self.next_uid
                .checked_add(added)
                .ok_or(RegistryError::UidExhausted)?;
            self.validate_edit_capacity(removed, &additions)?;
            Ok::<_, RegistryError>(additions)
        })();
        let additions = match prepared {
            Ok(additions) => additions,
            Err(error) => {
                self.names
                    .rollback(names_checkpoint)
                    .expect("an object edit owns the immediate name checkpoint");
                return Err(error);
            }
        };

        for id in removed {
            let slot = self
                .slots_by_uid
                .get(&id.uid())
                .copied()
                .expect("object edit removals were validated as live");
            self.remove_slot(slot);
        }
        for key in additions {
            let raw = self
                .next_uid
                .checked_add(1)
                .expect("object edit UID capacity was prevalidated");
            let uid = ObjectUid::from_raw(raw).expect("a positive object UID remains nonzero");
            self.push_live(uid, key)
                .expect("object edit capacity was prevalidated");
            self.next_uid = raw;
        }
        Ok(())
    }

    fn live_records(&self) -> LiveRecordIter<'_> {
        LiveRecordIter {
            registry: self,
            next: self.head,
            remaining: self.len,
        }
    }

    fn record(&self, uid: ObjectUid) -> Option<&LiveRecord> {
        self.slots_by_uid
            .get(&uid)
            .map(|slot| self.record_at(*slot))
    }

    pub(super) fn record_at(&self, slot: LiveSlot) -> &LiveRecord {
        self.slots[slot.index()]
            .record
            .as_ref()
            .expect("live object indexes only reference occupied slots")
    }

    fn design_slots(&self, design: &str) -> Option<&[LiveSlot]> {
        self.names
            .get(design)
            .and_then(|design| self.by_design.get(&design))
            .map(Vec::as_slice)
    }

    fn validate_push_capacity(&self, key: ObjectKey) -> Result<(), RegistryError> {
        if self.free.is_none() {
            LiveSlot::from_index(self.slots.len())?;
        }
        if let Some(design) = key.design() {
            let position = self.by_design.get(&design).map_or(0, Vec::len);
            DesignPosition::from_index(position)?;
        }
        Ok(())
    }

    fn validate_edit_capacity(
        &self,
        removed: &BTreeSet<AnyObjectId>,
        additions: &[ObjectKey],
    ) -> Result<(), RegistryError> {
        let free_after_removals = self
            .slots
            .len()
            .checked_sub(self.len)
            .and_then(|free| free.checked_add(removed.len()))
            .ok_or(RegistryError::Capacity {
                resource: "live object slots",
            })?;
        let new_slots = additions.len().saturating_sub(free_after_removals);
        if new_slots > 0 {
            let last =
                self.slots
                    .len()
                    .checked_add(new_slots - 1)
                    .ok_or(RegistryError::Capacity {
                        resource: "live object slots",
                    })?;
            LiveSlot::from_index(last)?;
        }

        let mut changes = HashMap::<NameId, (usize, usize)>::new();
        for id in removed {
            if let Some(design) = self
                .record(id.uid())
                .expect("object edit removals were validated as live")
                .key
                .design()
            {
                changes.entry(design).or_default().0 += 1;
            }
        }
        for key in additions {
            if let Some(design) = key.design() {
                changes.entry(design).or_default().1 += 1;
            }
        }
        for (design, (removed, added)) in changes {
            let current = self.by_design.get(&design).map_or(0, Vec::len);
            let future = current
                .checked_sub(removed)
                .and_then(|current| current.checked_add(added))
                .ok_or(RegistryError::Capacity {
                    resource: "per-design object slots",
                })?;
            if future > 0 {
                DesignPosition::from_index(future - 1)?;
            }
        }
        Ok(())
    }

    fn push_live(&mut self, uid: ObjectUid, key: ObjectKey) -> Result<AnyObjectId, RegistryError> {
        self.validate_push_capacity(key)?;
        let design_position = key.design().map(|design| {
            DesignPosition::from_index(self.by_design.get(&design).map_or(0, Vec::len))
                .expect("object capacity was validated before insertion")
        });
        let record = LiveRecord {
            uid,
            key,
            design_position,
        };
        let slot = if let Some(slot) = self.free {
            let stored = &mut self.slots[slot.index()];
            self.free = stored.next_free;
            stored.record = Some(record);
            stored.previous = self.tail;
            stored.next = None;
            stored.next_free = None;
            slot
        } else {
            let slot = LiveSlot::from_index(self.slots.len())?;
            self.slots.push(ArenaSlot {
                record: Some(record),
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

        let id = record.id();
        assert!(self.slots_by_uid.insert(uid, slot).is_none());
        assert!(self.active.insert(key, id).is_none());
        if let Some(design) = key.design() {
            self.by_design.entry(design).or_default().push(slot);
        }
        Ok(id)
    }

    fn remove_slot(&mut self, slot: LiveSlot) -> LiveRecord {
        let record = *self.record_at(slot);
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

        if let Some(design) = record.key.design() {
            let position = record
                .design_position
                .expect("design-owned live objects have a design position")
                .index();
            let (moved, empty) = {
                let slots = self
                    .by_design
                    .get_mut(&design)
                    .expect("design-owned live objects have a design index");
                assert_eq!(slots[position], slot);
                slots.swap_remove(position);
                (slots.get(position).copied(), slots.is_empty())
            };
            if let Some(moved) = moved {
                self.slots[moved.index()]
                    .record
                    .as_mut()
                    .expect("per-design indexes only reference occupied slots")
                    .design_position = Some(
                    DesignPosition::from_index(position)
                        .expect("an existing design position fits its typed index"),
                );
            }
            if empty {
                self.by_design.remove(&design);
            }
        }

        assert_eq!(self.slots_by_uid.remove(&record.uid), Some(slot));
        assert_eq!(self.active.remove(&record.key), Some(record.id()));
        let stored = &mut self.slots[slot.index()];
        let removed = stored
            .record
            .take()
            .expect("a live object slot contains a record");
        stored.previous = None;
        stored.next = None;
        stored.next_free = self.free;
        self.free = Some(slot);
        self.len -= 1;
        removed
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Failure while mutating, restoring, or indexing an object registry.
pub enum RegistryError {
    /// The monotonically allocated 64-bit UID space is exhausted.
    UidExhausted,
    /// A compact internal index cannot represent another entry.
    Capacity {
        /// The internal resource whose capacity was exceeded.
        resource: &'static str,
    },
    /// A locator component could not be interned.
    Name(NameError),
    /// A rollback checkpoint is not valid for the current revision.
    InvalidCheckpoint,
    /// A requested batch edit violates registry invariants.
    InvalidEdit(String),
    /// A serialized snapshot violates registry invariants.
    InvalidSnapshot(String),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UidExhausted => formatter.write_str("object UID space is exhausted"),
            Self::Capacity { resource } => {
                write!(
                    formatter,
                    "object registry {resource} exceed 32-bit capacity"
                )
            }
            Self::Name(error) => write!(formatter, "could not intern object name: {error}"),
            Self::InvalidCheckpoint => {
                formatter.write_str("object registry checkpoint does not belong to this revision")
            }
            Self::InvalidEdit(message) => {
                write!(formatter, "invalid object registry edit: {message}")
            }
            Self::InvalidSnapshot(message) => {
                write!(formatter, "invalid object registry snapshot: {message}")
            }
        }
    }
}

impl std::error::Error for RegistryError {}
