// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Persistent identities and canonical locators for design objects.
//!
//! [`ObjectId`] separates an object's stable numeric identity from its
//! user-facing name. [`ObjectRegistry`] owns the mapping between those two
//! representations. Typed IDs prevent accidental cross-class use, while
//! [`AnyObjectId`] provides explicit type erasure at heterogeneous boundaries.

use opto_core::{NameError, NameId, NameTable, ObjectUid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::marker::PhantomData;

mod registry;

#[cfg(test)]
use registry::{DesignPosition, SnapshotRecord};
pub use registry::{
    ObjectReconcileDesign, ObjectReconcileMode, ObjectReconcileSource, ObjectRegistry,
    ObjectRegistryCheckpoint, ObjectRegistryMarker, ObjectRegistryReconcilePlan,
    ObjectRegistrySnapshot, ObjectRegistrySnapshotRef, ObjectRemovalView, PreparedObjectReconcile,
    RegistryError,
};

mod sealed {
    pub trait Sealed {}
}

macro_rules! object_kinds {
    ($(($marker:ident, $alias:ident, $variant:ident, $class:ident)),+ $(,)?) => {
        $(
            #[doc = concat!("Type marker for [`ObjectClass::", stringify!($class), "`] objects.")]
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $marker;

            #[doc = concat!("Typed persistent identity of a [`", stringify!($marker), "`].")]
            pub type $alias = ObjectId<$marker>;
        )+

        /// The schema class of a design object.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum ObjectClass {
            $(
                #[doc = concat!("A `", stringify!($class), "` object.")]
                $class
            ),+
        }

        /// A persistent object ID whose schema class is known at runtime.
        ///
        /// Use [`Self::downcast`] to recover a typed [`ObjectId`] after
        /// checking the class.
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub enum AnyObjectId {
            $(
                #[doc = concat!("A typed [`", stringify!($alias), "`].")]
                $variant($alias)
            ),+
        }

        impl AnyObjectId {
            /// Returns the class-independent persistent UID.
            pub const fn uid(self) -> ObjectUid {
                match self {
                    $(Self::$variant(id) => id.uid()),+
                }
            }

            /// Returns the schema class carried by this ID.
            pub const fn class(self) -> ObjectClass {
                match self {
                    $(Self::$variant(_) => ObjectClass::$class),+
                }
            }

            /// Returns the typed ID when this value has class `T`.
            #[must_use]
            pub fn downcast<T: ObjectKind>(self) -> Option<ObjectId<T>> {
                T::downcast(self)
            }

            fn from_class(uid: ObjectUid, class: ObjectClass) -> Self {
                match class {
                    $(ObjectClass::$class => Self::$variant($alias::from_uid(uid))),+
                }
            }
        }

        $(
            impl sealed::Sealed for $marker {}

            impl ObjectKind for $marker {
                const CLASS: ObjectClass = ObjectClass::$class;

                fn erase(id: ObjectId<Self>) -> AnyObjectId {
                    AnyObjectId::$variant(id)
                }

                fn downcast(id: AnyObjectId) -> Option<ObjectId<Self>> {
                    match id {
                        AnyObjectId::$variant(id) => Some(id),
                        _ => None,
                    }
                }
            }
        )+
    };
}

object_kinds!(
    (DesignObject, DesignId, Design, Design),
    (PortObject, PortId, Port, Port),
    (CellObject, CellId, Cell, Cell),
    (PinObject, PinId, Pin, Pin),
    (NetObject, NetId, Net, Net),
    (ClockObject, ClockId, Clock, Clock),
);

/// Read-only membership over a deterministic set of persistent object IDs.
///
/// Large lifecycle transactions implement this interface with compact
/// registry-slot plans instead of allocating one tree node per removed object.
/// The standard [`BTreeSet`] implementation remains useful at command and test
/// boundaries where the set is already owned.
pub trait ObjectIdSet {
    /// Returns the number of members.
    fn len(&self) -> usize;

    /// Returns whether `object` belongs to this set.
    fn contains(&self, object: &AnyObjectId) -> bool;

    /// Visits members in deterministic order.
    fn iter(&self) -> impl Iterator<Item = AnyObjectId> + '_;

    /// Returns whether this set contains no objects.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ObjectIdSet for BTreeSet<AnyObjectId> {
    fn len(&self) -> usize {
        BTreeSet::len(self)
    }

    fn contains(&self, object: &AnyObjectId) -> bool {
        BTreeSet::contains(self, object)
    }

    fn iter(&self) -> impl Iterator<Item = AnyObjectId> + '_ {
        BTreeSet::iter(self).copied()
    }
}

/// Associates a zero-sized Rust marker with one design-object schema class.
///
/// This trait is sealed: only the marker types exported by this crate can
/// implement it, so each marker has exactly one [`ObjectClass`] and
/// [`AnyObjectId`] representation.
pub trait ObjectKind: sealed::Sealed + Copy + 'static {
    /// The schema class represented by this marker.
    const CLASS: ObjectClass;

    /// Erases the compile-time marker while preserving the runtime class.
    fn erase(id: ObjectId<Self>) -> AnyObjectId;

    /// Recovers a typed ID when `id` carries this marker's class.
    fn downcast(id: AnyObjectId) -> Option<ObjectId<Self>>;
}

/// A typed, persistent identity for a design object.
///
/// Equality and ordering compare only the globally unique [`ObjectUid`]. The
/// sealed marker `T` prevents IDs for different object classes from being
/// mixed by safe Rust code.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct ObjectId<T: ObjectKind> {
    uid: ObjectUid,
    kind: PhantomData<fn() -> T>,
}

impl<T: ObjectKind> ObjectId<T> {
    /// Wraps a persistent UID with the object class represented by `T`.
    ///
    /// This constructor does not consult an [`ObjectRegistry`]. Callers must
    /// therefore ensure that `uid` actually belongs to class `T`.
    #[must_use]
    pub const fn from_uid(uid: ObjectUid) -> Self {
        Self {
            uid,
            kind: PhantomData,
        }
    }

    /// Returns the underlying class-independent UID.
    #[must_use]
    pub const fn uid(self) -> ObjectUid {
        self.uid
    }

    /// Converts this ID to its runtime-class representation.
    #[must_use]
    pub fn erase(self) -> AnyObjectId {
        T::erase(self)
    }
}

impl<T: ObjectKind> PartialEq for ObjectId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.uid == other.uid
    }
}

impl<T: ObjectKind> Eq for ObjectId<T> {}

impl<T: ObjectKind> PartialOrd for ObjectId<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: ObjectKind> Ord for ObjectId<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.uid.cmp(&other.uid)
    }
}

impl<T: ObjectKind> std::hash::Hash for ObjectId<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.uid.hash(state);
    }
}

impl<T: ObjectKind> fmt::Debug for ObjectId<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple(&format!("{:?}Id", T::CLASS))
            .field(&self.uid.get())
            .finish()
    }
}

impl<T: ObjectKind> Serialize for ObjectId<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.uid.serialize(serializer)
    }
}

impl<'de, T: ObjectKind> Deserialize<'de> for ObjectId<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self::from_uid(ObjectUid::deserialize(deserializer)?))
    }
}

macro_rules! object_schema {
    ($($variant:ident { $($field:ident),+ $(,)? }),+ $(,)?) => {
        /// An owned, canonical name-based locator for a design object.
        ///
        /// Locators are suitable for serialization and command boundaries.
        /// Database-internal relationships should use persistent object IDs.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum ObjectLocator {
            $(
                #[doc = concat!("Locates a `", stringify!($variant), "` object.")]
                $variant {
                    $(
                        #[doc = concat!("The locator's `", stringify!($field), "` component.")]
                        $field: String
                    ),+
                },
            )+
        }

        /// Borrowed view of an object locator resolved from the registry's name table.
        ///
        /// Unlike [`ObjectLocator`], this view does not allocate or duplicate object
        /// names. Convert it to an owned locator only at an ownership or serialization
        /// boundary.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ResolvedObject<'a> {
            $(
                #[doc = concat!("Borrows the locator of a `", stringify!($variant), "` object.")]
                $variant {
                    $(
                        #[doc = concat!("The borrowed `", stringify!($field), "` component.")]
                        $field: &'a str
                    ),+
                },
            )+
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[repr(u8)]
        enum ObjectKey {
            $(
                $variant { $($field: NameId),+ },
            )+
        }
    };
}

object_schema! {
    Design { name },
    Port { design, name },
    Cell { design, name },
    Pin {
        design,
        cell,
        name,
        full_name,
    },
    Net { design, name },
    Clock { name },
}

impl ObjectLocator {
    ///
    /// For pins this is the stored full name; for all other classes it is the
    /// class-local `name` component.
    #[must_use]
    pub fn object_name(&self) -> &str {
        match self {
            Self::Design { name }
            | Self::Port { name, .. }
            | Self::Cell { name, .. }
            | Self::Net { name, .. }
            | Self::Clock { name } => name,
            Self::Pin { full_name, .. } => full_name,
        }
    }

    /// Returns the containing design name, when the object is design-scoped.
    #[must_use]
    pub fn design_name(&self) -> Option<&str> {
        match self {
            Self::Design { name } => Some(name),
            Self::Port { design, .. }
            | Self::Cell { design, .. }
            | Self::Pin { design, .. }
            | Self::Net { design, .. } => Some(design),
            Self::Clock { .. } => None,
        }
    }

    #[must_use]
    /// Returns the object's runtime class.
    pub const fn class(&self) -> ObjectClass {
        match self {
            Self::Design { .. } => ObjectClass::Design,
            Self::Port { .. } => ObjectClass::Port,
            Self::Cell { .. } => ObjectClass::Cell,
            Self::Pin { .. } => ObjectClass::Pin,
            Self::Net { .. } => ObjectClass::Net,
            Self::Clock { .. } => ObjectClass::Clock,
        }
    }
}

impl<'a> ResolvedObject<'a> {
    /// Returns the object name without its containing design qualifier.
    ///
    /// For pins this is the stored full name.
    #[must_use]
    pub const fn object_name(self) -> &'a str {
        match self {
            Self::Design { name }
            | Self::Port { name, .. }
            | Self::Cell { name, .. }
            | Self::Net { name, .. }
            | Self::Clock { name } => name,
            Self::Pin { full_name, .. } => full_name,
        }
    }

    /// Returns the containing design name, when the object is design-scoped.
    #[must_use]
    pub const fn design_name(self) -> Option<&'a str> {
        match self {
            Self::Design { name } => Some(name),
            Self::Port { design, .. }
            | Self::Cell { design, .. }
            | Self::Pin { design, .. }
            | Self::Net { design, .. } => Some(design),
            Self::Clock { .. } => None,
        }
    }

    #[must_use]
    /// Returns the object's runtime class.
    pub const fn class(self) -> ObjectClass {
        match self {
            Self::Design { .. } => ObjectClass::Design,
            Self::Port { .. } => ObjectClass::Port,
            Self::Cell { .. } => ObjectClass::Cell,
            Self::Pin { .. } => ObjectClass::Pin,
            Self::Net { .. } => ObjectClass::Net,
            Self::Clock { .. } => ObjectClass::Clock,
        }
    }

    /// Copies this borrowed view into an owned [`ObjectLocator`].
    #[must_use]
    pub fn to_locator(self) -> ObjectLocator {
        match self {
            Self::Design { name } => ObjectLocator::Design {
                name: name.to_string(),
            },
            Self::Port { design, name } => ObjectLocator::Port {
                design: design.to_string(),
                name: name.to_string(),
            },
            Self::Cell { design, name } => ObjectLocator::Cell {
                design: design.to_string(),
                name: name.to_string(),
            },
            Self::Pin {
                design,
                cell,
                name,
                full_name,
            } => ObjectLocator::Pin {
                design: design.to_string(),
                cell: cell.to_string(),
                name: name.to_string(),
                full_name: full_name.to_string(),
            },
            Self::Net { design, name } => ObjectLocator::Net {
                design: design.to_string(),
                name: name.to_string(),
            },
            Self::Clock { name } => ObjectLocator::Clock {
                name: name.to_string(),
            },
        }
    }
}

impl ObjectKey {
    fn lookup(locator: &ObjectLocator, names: &NameTable) -> Option<Self> {
        Some(match locator {
            ObjectLocator::Design { name } => Self::Design {
                name: names.get(name)?,
            },
            ObjectLocator::Port { design, name } => Self::Port {
                design: names.get(design)?,
                name: names.get(name)?,
            },
            ObjectLocator::Cell { design, name } => Self::Cell {
                design: names.get(design)?,
                name: names.get(name)?,
            },
            ObjectLocator::Pin {
                design,
                cell,
                name,
                full_name,
            } => Self::Pin {
                design: names.get(design)?,
                cell: names.get(cell)?,
                name: names.get(name)?,
                full_name: names.get(full_name)?,
            },
            ObjectLocator::Net { design, name } => Self::Net {
                design: names.get(design)?,
                name: names.get(name)?,
            },
            ObjectLocator::Clock { name } => Self::Clock {
                name: names.get(name)?,
            },
        })
    }

    fn lookup_resolved(locator: ResolvedObject<'_>, names: &NameTable) -> Option<Self> {
        Some(match locator {
            ResolvedObject::Design { name } => Self::Design {
                name: names.get(name)?,
            },
            ResolvedObject::Port { design, name } => Self::Port {
                design: names.get(design)?,
                name: names.get(name)?,
            },
            ResolvedObject::Cell { design, name } => Self::Cell {
                design: names.get(design)?,
                name: names.get(name)?,
            },
            ResolvedObject::Pin {
                design,
                cell,
                name,
                full_name,
            } => Self::Pin {
                design: names.get(design)?,
                cell: names.get(cell)?,
                name: names.get(name)?,
                full_name: names.get(full_name)?,
            },
            ResolvedObject::Net { design, name } => Self::Net {
                design: names.get(design)?,
                name: names.get(name)?,
            },
            ResolvedObject::Clock { name } => Self::Clock {
                name: names.get(name)?,
            },
        })
    }

    fn intern(locator: &ObjectLocator, names: &mut NameTable) -> Result<Self, NameError> {
        Ok(match locator {
            ObjectLocator::Design { name } => Self::Design {
                name: names.intern(name)?,
            },
            ObjectLocator::Port { design, name } => Self::Port {
                design: names.intern(design)?,
                name: names.intern(name)?,
            },
            ObjectLocator::Cell { design, name } => Self::Cell {
                design: names.intern(design)?,
                name: names.intern(name)?,
            },
            ObjectLocator::Pin {
                design,
                cell,
                name,
                full_name,
            } => Self::Pin {
                design: names.intern(design)?,
                cell: names.intern(cell)?,
                name: names.intern(name)?,
                full_name: names.intern(full_name)?,
            },
            ObjectLocator::Net { design, name } => Self::Net {
                design: names.intern(design)?,
                name: names.intern(name)?,
            },
            ObjectLocator::Clock { name } => Self::Clock {
                name: names.intern(name)?,
            },
        })
    }

    fn intern_resolved(
        locator: ResolvedObject<'_>,
        names: &mut NameTable,
    ) -> Result<Self, NameError> {
        Ok(match locator {
            ResolvedObject::Design { name } => Self::Design {
                name: names.intern(name)?,
            },
            ResolvedObject::Port { design, name } => Self::Port {
                design: names.intern(design)?,
                name: names.intern(name)?,
            },
            ResolvedObject::Cell { design, name } => Self::Cell {
                design: names.intern(design)?,
                name: names.intern(name)?,
            },
            ResolvedObject::Pin {
                design,
                cell,
                name,
                full_name,
            } => Self::Pin {
                design: names.intern(design)?,
                cell: names.intern(cell)?,
                name: names.intern(name)?,
                full_name: names.intern(full_name)?,
            },
            ResolvedObject::Net { design, name } => Self::Net {
                design: names.intern(design)?,
                name: names.intern(name)?,
            },
            ResolvedObject::Clock { name } => Self::Clock {
                name: names.intern(name)?,
            },
        })
    }

    fn semantic_cmp(self, other: Self, names: &NameTable) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        let class = self.class().cmp(&other.class());
        if class != Ordering::Equal {
            return class;
        }
        let resolve = |name| {
            names
                .resolve(name)
                .expect("live and prepared object keys only contain interned names")
        };
        match (self, other) {
            (Self::Design { name: left }, Self::Design { name: right })
            | (Self::Clock { name: left }, Self::Clock { name: right }) => {
                resolve(left).cmp(resolve(right))
            }
            (
                Self::Port {
                    design: left_design,
                    name: left_name,
                }
                | Self::Cell {
                    design: left_design,
                    name: left_name,
                }
                | Self::Net {
                    design: left_design,
                    name: left_name,
                },
                Self::Port {
                    design: right_design,
                    name: right_name,
                }
                | Self::Cell {
                    design: right_design,
                    name: right_name,
                }
                | Self::Net {
                    design: right_design,
                    name: right_name,
                },
            ) => (resolve(left_design), resolve(left_name))
                .cmp(&(resolve(right_design), resolve(right_name))),
            (
                Self::Pin {
                    design: left_design,
                    cell: left_cell,
                    name: left_name,
                    full_name: left_full_name,
                },
                Self::Pin {
                    design: right_design,
                    cell: right_cell,
                    name: right_name,
                    full_name: right_full_name,
                },
            ) => (
                resolve(left_design),
                resolve(left_cell),
                resolve(left_name),
                resolve(left_full_name),
            )
                .cmp(&(
                    resolve(right_design),
                    resolve(right_cell),
                    resolve(right_name),
                    resolve(right_full_name),
                )),
            _ => unreachable!("equal object classes use the same key variant"),
        }
    }

    const fn class(self) -> ObjectClass {
        match self {
            Self::Design { .. } => ObjectClass::Design,
            Self::Port { .. } => ObjectClass::Port,
            Self::Cell { .. } => ObjectClass::Cell,
            Self::Pin { .. } => ObjectClass::Pin,
            Self::Net { .. } => ObjectClass::Net,
            Self::Clock { .. } => ObjectClass::Clock,
        }
    }

    const fn design(self) -> Option<NameId> {
        match self {
            Self::Design { name } => Some(name),
            Self::Port { design, .. }
            | Self::Cell { design, .. }
            | Self::Pin { design, .. }
            | Self::Net { design, .. } => Some(design),
            Self::Clock { .. } => None,
        }
    }

    fn resolve(self, names: &NameTable) -> Option<ResolvedObject<'_>> {
        Some(match self {
            Self::Design { name } => ResolvedObject::Design {
                name: names.resolve(name)?,
            },
            Self::Port { design, name } => ResolvedObject::Port {
                design: names.resolve(design)?,
                name: names.resolve(name)?,
            },
            Self::Cell { design, name } => ResolvedObject::Cell {
                design: names.resolve(design)?,
                name: names.resolve(name)?,
            },
            Self::Pin {
                design,
                cell,
                name,
                full_name,
            } => ResolvedObject::Pin {
                design: names.resolve(design)?,
                cell: names.resolve(cell)?,
                name: names.resolve(name)?,
                full_name: names.resolve(full_name)?,
            },
            Self::Net { design, name } => ResolvedObject::Net {
                design: names.resolve(design)?,
                name: names.resolve(name)?,
            },
            Self::Clock { name } => ResolvedObject::Clock {
                name: names.resolve(name)?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(design: &str, name: &str) -> ObjectLocator {
        ObjectLocator::Port {
            design: design.to_string(),
            name: name.to_string(),
        }
    }

    fn decode_snapshot(registry: &ObjectRegistry) -> ObjectRegistrySnapshot {
        let encoded = opto_archive::to_bytes(&registry.snapshot_ref()).unwrap();
        let snapshot: ObjectRegistrySnapshot = opto_archive::from_bytes(&encoded).unwrap();
        snapshot
    }

    fn snapshot_record(registry: &ObjectRegistry, locator: &ObjectLocator) -> SnapshotRecord {
        SnapshotRecord {
            uid: registry.get(locator).unwrap().uid(),
            key: ObjectKey::lookup(locator, &registry.names).unwrap(),
        }
    }

    #[test]
    fn active_locators_are_interned_once_with_typed_identity() {
        let mut registry = ObjectRegistry::default();
        let first = registry.intern(port("top", "clk")).unwrap();
        let second = registry.intern(port("top", "clk")).unwrap();

        assert_eq!(first, second);
        assert!(matches!(first, AnyObjectId::Port(_)));
        assert_eq!(
            registry.resolve(first),
            Some(ResolvedObject::Port {
                design: "top",
                name: "clk"
            })
        );
        assert_eq!(
            registry.get_resolved(ResolvedObject::Port {
                design: "top",
                name: "clk",
            }),
            Some(first)
        );
        assert_eq!(registry.names.entry_count(), 3);
    }

    #[test]
    fn removed_ids_never_bind_to_same_named_new_objects() {
        let mut registry = ObjectRegistry::default();
        let old = registry.intern(port("top", "clk")).unwrap();
        let removed = registry.design_objects("top").collect::<BTreeSet<_>>();
        assert_eq!(removed, BTreeSet::from([old]));
        registry.apply_edit(&removed, [port("top", "clk")]).unwrap();
        assert!(registry.resolve(old).is_none());

        let new = registry.get(&port("top", "clk")).unwrap();
        assert_ne!(old, new);
        assert!(registry.resolve(old).is_none());
        assert_eq!(
            registry.resolve(new),
            Some(ResolvedObject::Port {
                design: "top",
                name: "clk"
            })
        );
    }

    #[test]
    fn resolve_rejects_an_id_with_the_wrong_object_class() {
        let mut registry = ObjectRegistry::default();
        let AnyObjectId::Port(port) = registry.intern(port("top", "clk")).unwrap() else {
            panic!("expected a port ID");
        };
        let forged = AnyObjectId::Clock(ClockId::from_uid(port.uid()));
        assert!(registry.resolve(forged).is_none());
    }

    #[test]
    fn compact_keys_do_not_embed_owned_strings() {
        assert!(std::mem::size_of::<ObjectKey>() <= 20);
        assert!(std::mem::size_of::<ObjectKey>() < std::mem::size_of::<ObjectLocator>());
    }

    #[test]
    fn snapshot_round_trip_preserves_identity_and_uid_high_water() {
        let mut registry = ObjectRegistry::default();
        let removed = registry.intern(port("top", "old")).unwrap();
        let live = registry.intern(port("top", "clk")).unwrap();
        let keep = BTreeSet::from([port("top", "clk")]);
        let edit = BTreeSet::from([removed]);
        registry.apply_edit(&edit, keep).unwrap();

        let mut restored = ObjectRegistry::from_snapshot(decode_snapshot(&registry)).unwrap();
        assert!(restored.resolve(removed).is_none());
        assert_eq!(
            restored.resolve(live),
            Some(ResolvedObject::Port {
                design: "top",
                name: "clk"
            })
        );
        assert_eq!(restored.get(&port("top", "clk")), Some(live));
        let replacement = restored.intern(port("top", "replacement")).unwrap();
        assert!(replacement.uid() > live.uid());
        assert_ne!(replacement, removed);
    }

    #[test]
    fn borrowed_snapshot_round_trips_through_the_streaming_owner() {
        let mut registry = ObjectRegistry::default();
        let removed = registry.intern(port("top", "removed")).unwrap();
        let clock = registry.intern(port("top", "clk")).unwrap();
        registry
            .apply_edit(&BTreeSet::from([removed]), [port("top", "replacement")])
            .unwrap();
        let replacement = registry.get(&port("top", "replacement")).unwrap();

        let borrowed = opto_archive::to_bytes(&registry.snapshot_ref()).unwrap();
        let snapshot: ObjectRegistrySnapshot = opto_archive::from_bytes(&borrowed).unwrap();
        let restored_wire = opto_archive::to_bytes(&snapshot).unwrap();
        assert_eq!(borrowed, restored_wire);
        let restored = ObjectRegistry::from_snapshot(snapshot).unwrap();
        assert_eq!(restored.get(&port("top", "clk")), Some(clock));
        assert_eq!(restored.get(&port("top", "replacement")), Some(replacement));
    }

    #[test]
    fn edit_addition_order_does_not_change_identity_or_snapshot_bytes() {
        let additions = [port("top", "z"), port("top", "a"), port("top", "middle")];
        let mut forward = ObjectRegistry::default();
        forward
            .apply_edit(&BTreeSet::new(), additions.clone())
            .unwrap();
        let mut reverse = ObjectRegistry::default();
        reverse
            .apply_edit(&BTreeSet::new(), additions.into_iter().rev())
            .unwrap();

        for locator in [port("top", "a"), port("top", "middle"), port("top", "z")] {
            assert_eq!(forward.get(&locator), reverse.get(&locator));
        }
        let forward = opto_archive::to_bytes(&forward.snapshot_ref()).unwrap();
        let reverse = opto_archive::to_bytes(&reverse.snapshot_ref()).unwrap();
        assert_eq!(forward, reverse);
    }

    #[test]
    fn per_design_swap_removal_keeps_every_slot_index_consistent() {
        let mut registry = ObjectRegistry::default();
        let first = registry.intern(port("top", "first")).unwrap();
        let middle = registry.intern(port("top", "middle")).unwrap();
        let last = registry.intern(port("top", "last")).unwrap();

        registry
            .apply_edit(&BTreeSet::from([middle]), Vec::new())
            .unwrap();
        assert_eq!(
            registry.design_objects("top").collect::<BTreeSet<_>>(),
            BTreeSet::from([first, last])
        );
        assert!(registry.resolve(last).is_some());

        registry
            .apply_edit(&BTreeSet::from([first]), [port("top", "new")])
            .unwrap();
        let new = registry.get(&port("top", "new")).unwrap();
        assert_eq!(
            registry.design_objects("top").collect::<BTreeSet<_>>(),
            BTreeSet::from([last, new])
        );
        let design = registry.names.get("top").unwrap();
        for (position, slot) in registry.by_design[&design].iter().copied().enumerate() {
            assert_eq!(
                registry.record_at(slot).design_position,
                Some(DesignPosition::from_index(position).unwrap())
            );
        }
    }

    #[test]
    fn edit_churn_reuses_peak_slots_without_reusing_uids() {
        let mut registry = ObjectRegistry::default();
        let locator = port("top", "signal");
        let mut current = registry.intern(locator.clone()).unwrap();
        for _ in 0..1_000 {
            registry
                .apply_edit(&BTreeSet::from([current]), [locator.clone()])
                .unwrap();
            let replacement = registry.get(&locator).unwrap();
            assert!(replacement.uid() > current.uid());
            current = replacement;
        }

        assert_eq!(registry.len, 1);
        assert_eq!(registry.slots.len(), 1);
        assert_eq!(registry.resolve(current).unwrap().object_name(), "signal");
    }

    #[test]
    fn checkpoint_rollback_consumes_uids_without_cloning_the_registry() {
        let mut registry = ObjectRegistry::default();
        let stable = registry.intern(port("top", "stable")).unwrap();
        let checkpoint = registry.checkpoint();
        let transient = registry.intern(port("top", "transient")).unwrap();

        registry.rollback(checkpoint).unwrap();

        assert!(registry.resolve(stable).is_some());
        assert!(registry.resolve(transient).is_none());
        assert!(registry.get(&port("top", "transient")).is_none());
        assert_eq!(registry.names.entry_count(), 3);
        let replacement = registry.intern(port("top", "transient")).unwrap();
        assert_ne!(replacement, transient);
        assert!(replacement.uid() > transient.uid());
        assert_eq!(registry.len, 2);
        assert_eq!(registry.slots.len(), 2);
    }

    #[test]
    fn rollback_churn_keeps_only_live_keys_and_never_reuses_a_uid() {
        let mut registry = ObjectRegistry::default();
        let stable = registry.intern(port("top", "stable")).unwrap();
        let mut previous = stable;

        for index in 0..512 {
            let checkpoint = registry.checkpoint();
            let transient = registry
                .intern(port("top", &format!("transient_{index}")))
                .unwrap();
            assert!(transient.uid() > previous.uid());
            previous = transient;
            registry.rollback(checkpoint).unwrap();
            assert!(registry.resolve(transient).is_none());
        }

        assert_eq!(registry.len, 1);
        assert_eq!(registry.slots.len(), 2);
        assert!(registry.slots_by_uid.contains_key(&stable.uid()));
        assert_eq!(registry.names.entry_count(), 3);
        assert!(std::mem::size_of::<ObjectRegistryCheckpoint>() <= 32);
    }

    #[test]
    fn edit_validation_failure_does_not_tombstone_or_intern_names() {
        let mut registry = ObjectRegistry::default();
        let stable = registry.intern(port("top", "stable")).unwrap();
        let name_count = registry.names.entry_count();
        registry.next_uid = u64::MAX;

        let error = registry
            .apply_edit(&BTreeSet::from([stable]), [port("top", "replacement")])
            .unwrap_err();

        assert_eq!(error, RegistryError::UidExhausted);
        assert!(registry.resolve(stable).is_some());
        assert!(registry.get(&port("top", "replacement")).is_none());
        assert_eq!(registry.names.entry_count(), name_count);
    }

    #[test]
    fn snapshot_rejects_unknown_name_ids() {
        let mut unrelated_names = NameTable::new();
        let unknown = unrelated_names.intern("missing").unwrap();
        let snapshot = ObjectRegistrySnapshot::from_records(
            NameTable::new(),
            1,
            [SnapshotRecord {
                uid: ObjectUid::from_raw(1).unwrap(),
                key: ObjectKey::Design { name: unknown },
            }],
        );

        let error = ObjectRegistry::from_snapshot(snapshot).unwrap_err();
        assert!(error.to_string().contains("unknown name ID"));
    }

    #[test]
    fn snapshot_rejects_duplicate_live_keys() {
        let mut registry = ObjectRegistry::default();
        let locator = port("top", "clk");
        registry.intern(locator.clone()).unwrap();
        let record = snapshot_record(&registry, &locator);
        let snapshot = ObjectRegistrySnapshot::from_records(
            registry.names.clone(),
            2,
            [
                record,
                SnapshotRecord {
                    uid: ObjectUid::from_raw(2).unwrap(),
                    key: record.key,
                },
            ],
        );

        let error = ObjectRegistry::from_snapshot(snapshot).unwrap_err();
        assert!(error.to_string().contains("duplicate live object locators"));
    }

    #[test]
    fn snapshot_rejects_noncanonical_uid_order() {
        let mut registry = ObjectRegistry::default();
        let first = port("top", "first");
        let second = port("top", "second");
        registry.intern(first.clone()).unwrap();
        registry.intern(second.clone()).unwrap();
        let snapshot = ObjectRegistrySnapshot::from_records(
            registry.names.clone(),
            registry.next_uid,
            [
                snapshot_record(&registry, &second),
                snapshot_record(&registry, &first),
            ],
        );

        let error = ObjectRegistry::from_snapshot(snapshot).unwrap_err();
        assert!(error.to_string().contains("strictly UID-sorted"));
    }

    #[test]
    fn snapshot_rejects_a_live_uid_above_its_high_water_mark() {
        let mut registry = ObjectRegistry::default();
        let locator = port("top", "clk");
        registry.intern(locator.clone()).unwrap();
        let snapshot = ObjectRegistrySnapshot::from_records(
            registry.names.clone(),
            0,
            [snapshot_record(&registry, &locator)],
        );

        let error = ObjectRegistry::from_snapshot(snapshot).unwrap_err();
        assert!(error.to_string().contains("above high-water mark"));
    }
}
