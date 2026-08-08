// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Streaming, design-local reconciliation of persistent object identities.

use super::{
    AnyObjectId, Arc, DesignPosition, HashMap, LiveSlot, NameCheckpoint, NameId, ObjectIdSet,
    ObjectKey, ObjectRegistry, ObjectUid, RegistryError, ResolvedObject,
};
use std::cmp::Ordering;

/// Whether one design keeps identities for locators present in the new view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectReconcileMode {
    /// Remove every previous object owned by the design before publishing the
    /// source. Locators emitted by the source receive new UIDs.
    Replace,
    /// Retain the UID of every emitted locator that is already live and remove
    /// only objects absent from the source.
    Update,
}

/// One design participating in an object-registry reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectReconcileDesign<'a> {
    /// Canonical design name.
    pub name: &'a str,
    /// Identity-retention policy for this design.
    pub mode: ObjectReconcileMode,
}

/// Replayable borrowed source for one atomic registry reconciliation.
///
/// `design(index)` must be strictly name-ordered. `visit` must emit every
/// desired locator exactly once in [`crate::ObjectLocator`] semantic order. The
/// registry validates both contracts before mutation. A visitor may retain a
/// locator only for the duration of the call, allowing sources to reuse one
/// scratch buffer for derived pin names.
pub trait ObjectReconcileSource {
    /// Number of participating designs.
    fn design_count(&self) -> usize;

    /// Returns one participating design by canonical index.
    fn design(&self, index: usize) -> ObjectReconcileDesign<'_>;

    /// Replays the complete desired object set in canonical locator order.
    fn visit(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>));
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedDesign {
    name: String,
    mode: ObjectReconcileMode,
}

/// Compact, immutable removal plan produced before any session owner changes.
///
/// Removed objects are represented by 32-bit live-slot IDs. The plan remains
/// valid only while the originating registry retains the same owner, UID
/// high-water mark, and live length.
#[derive(Debug)]
pub struct ObjectRegistryReconcilePlan {
    owner: Arc<()>,
    next_uid: u64,
    live_len: usize,
    designs: Box<[PlannedDesign]>,
    removed: Box<[LiveSlot]>,
    source_count: usize,
    source_digest: [u8; 32],
}

/// Borrowed deterministic object-ID set backed by compact registry slots.
#[derive(Debug, Clone, Copy)]
pub struct ObjectRemovalView<'a> {
    registry: &'a ObjectRegistry,
    removed: &'a [LiveSlot],
}

impl ObjectIdSet for ObjectRemovalView<'_> {
    fn len(&self) -> usize {
        self.removed.len()
    }

    fn contains(&self, object: &AnyObjectId) -> bool {
        self.removed
            .binary_search_by(|slot| self.registry.record_at(*slot).id().cmp(object))
            .is_ok()
    }

    fn iter(&self) -> impl Iterator<Item = AnyObjectId> + '_ {
        self.removed
            .iter()
            .map(|slot| self.registry.record_at(*slot).id())
    }
}

impl ObjectRegistryReconcilePlan {
    /// Borrows the planned removals while checking that no registry edit has
    /// occurred since planning.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::InvalidEdit`] if `registry` is not the exact
    /// unchanged owner against which this plan was computed.
    pub fn removed<'a>(
        &'a self,
        registry: &'a ObjectRegistry,
    ) -> Result<ObjectRemovalView<'a>, RegistryError> {
        self.validate_owner(registry)?;
        Ok(ObjectRemovalView {
            registry,
            removed: &self.removed,
        })
    }

    fn validate_owner(&self, registry: &ObjectRegistry) -> Result<(), RegistryError> {
        if !Arc::ptr_eq(&self.owner, &registry.owner)
            || self.next_uid != registry.next_uid
            || self.live_len != registry.len
        {
            return Err(RegistryError::InvalidEdit(
                "object reconciliation plan is stale or belongs to another registry".to_string(),
            ));
        }
        Ok(())
    }

    fn contains_id(&self, registry: &ObjectRegistry, object: AnyObjectId) -> bool {
        ObjectRemovalView {
            registry,
            removed: &self.removed,
        }
        .contains(&object)
    }
}

/// Fully preflighted registry edit.
///
/// Name interning and every capacity check have completed. Dropping this
/// token rolls back newly interned names; [`Self::commit`] performs only
/// deterministic, prevalidated ownership moves.
#[must_use = "a prepared registry reconciliation has no effect unless committed"]
#[derive(Debug)]
pub struct PreparedObjectReconcile<'registry> {
    registry: &'registry mut ObjectRegistry,
    plan: ObjectRegistryReconcilePlan,
    names_checkpoint: NameCheckpoint,
    additions: Box<[ObjectKey]>,
    committed: bool,
}

impl PreparedObjectReconcile<'_> {
    /// Applies the prevalidated edit without a recoverable failure path.
    ///
    /// # Panics
    ///
    /// Panics if the registry is mutated between preparation and this consuming
    /// commit, or if a preflighted UID/index capacity invariant is violated.
    pub fn commit(mut self) {
        for &slot in &self.plan.removed {
            self.registry.remove_slot(slot);
        }

        for &key in &self.additions {
            assert!(
                !self.registry.active.contains_key(&key),
                "prepared object addition became live before commit"
            );
            let raw = self
                .registry
                .next_uid
                .checked_add(1)
                .expect("prepared object UID capacity remains valid");
            let uid = ObjectUid::from_raw(raw)
                .expect("a prevalidated positive object UID remains nonzero");
            self.registry
                .push_live(uid, key)
                .expect("prepared object arena capacity remains valid");
            self.registry.next_uid = raw;
        }
        self.committed = true;
    }
}

impl Drop for PreparedObjectReconcile<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.registry
                .names
                .rollback(self.names_checkpoint)
                .expect("an uncommitted registry edit owns its immediate name checkpoint");
        }
    }
}

impl ObjectRegistry {
    /// Discovers design-local removals using one byte per live registry slot.
    ///
    /// Existing locators are resolved through the active hash index; neither
    /// this pass nor the returned plan owns user-visible object strings.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for duplicate/out-of-order source objects,
    /// unknown designs, source traversal failure, or arena capacity overflow.
    ///
    /// # Panics
    ///
    /// Panics only if a live registry slot violates the registry's design index
    /// or interned-name invariants.
    pub fn plan_reconcile(
        &self,
        source: &dyn ObjectReconcileSource,
    ) -> Result<ObjectRegistryReconcilePlan, RegistryError> {
        let designs = collect_designs(source)?;
        let mut retained = designs
            .iter()
            .map(|design| {
                if design.mode == ObjectReconcileMode::Update {
                    vec![0u8; self.design_slots(&design.name).map_or(0, <[LiveSlot]>::len)]
                } else {
                    Vec::new()
                }
            })
            .collect::<Vec<_>>();
        let mut source_error = None;
        let mut source_count = 0usize;
        let mut source_digest = reconcile_digest();
        source.visit(&mut |object| {
            if source_error.is_some() {
                return;
            }
            source_count = if let Some(count) = source_count.checked_add(1) {
                count
            } else {
                source_error = Some(RegistryError::Capacity {
                    resource: "replayed object source",
                });
                return;
            };
            update_source_digest(object, &mut source_digest);
            let Some(design) = object.design_name() else {
                source_error = Some(RegistryError::InvalidEdit(
                    "design reconciliation source contains a global object".to_string(),
                ));
                return;
            };
            let Ok(index) =
                designs.binary_search_by(|candidate| candidate.name.as_str().cmp(design))
            else {
                source_error = Some(RegistryError::InvalidEdit(format!(
                    "object reconciliation source contains undeclared design '{design}'"
                )));
                return;
            };
            if designs[index].mode != ObjectReconcileMode::Update {
                return;
            }
            let Some(key) = ObjectKey::lookup_resolved(object, &self.names) else {
                return;
            };
            let Some(id) = self.active.get(&key) else {
                return;
            };
            let slot = self.slots_by_uid[&id.uid()];
            let position = self
                .record_at(slot)
                .design_position
                .expect("design-scoped live objects have a design position")
                .index();
            retained[index][position] = 1;
        });
        if let Some(error) = source_error {
            return Err(error);
        }

        let mut removed = Vec::new();
        for (index, design) in designs.iter().enumerate() {
            let Some(slots) = self.design_slots(&design.name) else {
                continue;
            };
            removed.extend(
                slots
                    .iter()
                    .copied()
                    .enumerate()
                    .filter_map(|(position, slot)| {
                        (design.mode == ObjectReconcileMode::Replace
                            || retained[index][position] == 0)
                            .then_some(slot)
                    }),
            );
        }
        removed.sort_unstable_by_key(|slot| self.record_at(*slot).id());
        debug_assert!(
            !removed
                .windows(2)
                .any(|pair| self.record_at(pair[0]).id() == self.record_at(pair[1]).id())
        );

        Ok(ObjectRegistryReconcilePlan {
            owner: Arc::clone(&self.owner),
            next_uid: self.next_uid,
            live_len: self.len,
            designs,
            removed: removed.into_boxed_slice(),
            source_count,
            source_digest: *source_digest.finalize().as_bytes(),
        })
    }

    /// Interns the replayed names and validates the complete final edit while
    /// every live registry relationship is still immutable.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] if the plan is stale, replay differs from the
    /// planned source digest/order, a resulting key conflicts, or name/UID
    /// capacity is exhausted. Name interning is rolled back on failure.
    ///
    /// # Panics
    ///
    /// Panics only if an unchanged live slot violates registry ownership or
    /// design-index invariants established by prior commits.
    #[allow(
        clippy::too_many_lines,
        reason = "replay validation and name rollback form one preparation transaction"
    )]
    pub fn prepare_reconcile<'registry>(
        &'registry mut self,
        plan: ObjectRegistryReconcilePlan,
        source: &dyn ObjectReconcileSource,
    ) -> Result<PreparedObjectReconcile<'registry>, RegistryError> {
        plan.validate_owner(self)?;
        validate_designs(source, &plan.designs)?;

        let names_checkpoint = self.names.checkpoint();
        let prepared = (|| {
            let mut previous: Option<ObjectKey> = None;
            let mut additions = Vec::new();
            let mut source_count = 0usize;
            let mut source_digest = reconcile_digest();
            let mut changes = HashMap::<NameId, (usize, usize)>::new();
            for &slot in &plan.removed {
                if let Some(design) = self.record_at(slot).key.design() {
                    changes.entry(design).or_default().0 += 1;
                }
            }

            let mut source_error = None;
            source.visit(&mut |object| {
                if source_error.is_some() {
                    return;
                }
                source_count = if let Some(count) = source_count.checked_add(1) {
                    count
                } else {
                    source_error = Some(RegistryError::Capacity {
                        resource: "replayed object source",
                    });
                    return;
                };
                update_source_digest(object, &mut source_digest);
                let key = match ObjectKey::intern_resolved(object, &mut self.names) {
                    Ok(key) => key,
                    Err(error) => {
                        source_error = Some(RegistryError::Name(error));
                        return;
                    }
                };
                if previous.is_some_and(|previous| {
                    previous.semantic_cmp(key, &self.names) != Ordering::Less
                }) {
                    source_error = Some(RegistryError::InvalidEdit(
                        "object reconciliation source is not strictly locator ordered".to_string(),
                    ));
                    return;
                }
                previous = Some(key);

                let Some(design_id) = key.design() else {
                    source_error = Some(RegistryError::InvalidEdit(
                        "design reconciliation source contains a global object".to_string(),
                    ));
                    return;
                };
                let design_name = self
                    .names
                    .resolve(design_id)
                    .expect("prepared design name was just interned");
                if plan
                    .designs
                    .binary_search_by(|candidate| candidate.name.as_str().cmp(design_name))
                    .is_err()
                {
                    source_error = Some(RegistryError::InvalidEdit(format!(
                        "object reconciliation source contains undeclared design '{design_name}'"
                    )));
                    return;
                }

                if self
                    .active
                    .get(&key)
                    .is_none_or(|id| plan.contains_id(self, *id))
                {
                    additions.push(key);
                    changes.entry(design_id).or_default().1 += 1;
                }
            });
            if let Some(error) = source_error {
                return Err(error);
            }

            if source_count != plan.source_count
                || *source_digest.finalize().as_bytes() != plan.source_digest
            {
                return Err(RegistryError::InvalidEdit(
                    "object reconciliation source changed after removal planning".to_string(),
                ));
            }
            let added = u64::try_from(additions.len()).map_err(|_| RegistryError::UidExhausted)?;
            self.next_uid
                .checked_add(added)
                .ok_or(RegistryError::UidExhausted)?;
            self.validate_reconcile_capacity(plan.removed.len(), additions.len(), changes)?;
            Ok(additions.into_boxed_slice())
        })();

        let additions = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.names
                    .rollback(names_checkpoint)
                    .expect("registry reconciliation owns the immediate name checkpoint");
                return Err(error);
            }
        };

        Ok(PreparedObjectReconcile {
            registry: self,
            plan,
            names_checkpoint,
            additions,
            committed: false,
        })
    }

    fn validate_reconcile_capacity(
        &self,
        removed: usize,
        additions: usize,
        changes: HashMap<NameId, (usize, usize)>,
    ) -> Result<(), RegistryError> {
        let free_after_removals = self
            .slots
            .len()
            .checked_sub(self.len)
            .and_then(|free| free.checked_add(removed))
            .ok_or(RegistryError::Capacity {
                resource: "live object slots",
            })?;
        let new_slots = additions.saturating_sub(free_after_removals);
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
}

fn reconcile_digest() -> blake3::Hasher {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/object-reconcile-source/v1\0");
    digest
}

fn update_source_digest(object: ResolvedObject<'_>, digest: &mut blake3::Hasher) {
    match object {
        ResolvedObject::Design { name } => {
            digest.update(&[0]);
            update_digest_field(digest, name);
        }
        ResolvedObject::Port { design, name } => {
            digest.update(&[1]);
            update_digest_field(digest, design);
            update_digest_field(digest, name);
        }
        ResolvedObject::Cell { design, name } => {
            digest.update(&[2]);
            update_digest_field(digest, design);
            update_digest_field(digest, name);
        }
        ResolvedObject::Pin {
            design,
            cell,
            name,
            full_name,
        } => {
            digest.update(&[3]);
            update_digest_field(digest, design);
            update_digest_field(digest, cell);
            update_digest_field(digest, name);
            update_digest_field(digest, full_name);
        }
        ResolvedObject::Net { design, name } => {
            digest.update(&[4]);
            update_digest_field(digest, design);
            update_digest_field(digest, name);
        }
        ResolvedObject::Clock { name } => {
            digest.update(&[5]);
            update_digest_field(digest, name);
        }
    }
}

fn update_digest_field(digest: &mut blake3::Hasher, value: &str) {
    digest.update(&(value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

fn collect_designs(
    source: &dyn ObjectReconcileSource,
) -> Result<Box<[PlannedDesign]>, RegistryError> {
    let mut designs = Vec::with_capacity(source.design_count());
    for index in 0..source.design_count() {
        let design = source.design(index);
        if designs
            .last()
            .is_some_and(|previous: &PlannedDesign| previous.name.as_str() >= design.name)
        {
            return Err(RegistryError::InvalidEdit(
                "object reconciliation designs are not strictly name ordered".to_string(),
            ));
        }
        designs.push(PlannedDesign {
            name: design.name.to_string(),
            mode: design.mode,
        });
    }
    Ok(designs.into_boxed_slice())
}

fn validate_designs(
    source: &dyn ObjectReconcileSource,
    expected: &[PlannedDesign],
) -> Result<(), RegistryError> {
    if source.design_count() != expected.len()
        || expected.iter().enumerate().any(|(index, expected)| {
            let actual = source.design(index);
            actual.name != expected.name || actual.mode != expected.mode
        })
    {
        return Err(RegistryError::InvalidEdit(
            "object reconciliation source changed after removal planning".to_string(),
        ));
    }
    Ok(())
}
