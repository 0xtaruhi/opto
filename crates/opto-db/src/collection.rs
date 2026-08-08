// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Homogeneous collections of persistent design-object identities.
//!
//! A collection owns only typed IDs. Object names and attributes remain in
//! the database that issued those IDs, so cloning a collection does not clone
//! design data.

use crate::{ObjectId, ObjectKind};

/// An ordered collection of object IDs belonging to one [`ObjectKind`].
///
/// The collection does not deduplicate or sort its input. Callers that expose
/// collection semantics to Tcl are responsible for preserving the ordering
/// required by the originating command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection<T: ObjectKind> {
    objects: Vec<ObjectId<T>>,
}

impl<T: ObjectKind> Collection<T> {
    /// Wraps `objects` without changing their order.
    #[must_use]
    pub fn new(objects: Vec<ObjectId<T>>) -> Self {
        Self { objects }
    }

    #[must_use]
    /// Creates an empty collection.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    #[must_use]
    /// Returns the number of object IDs in the collection.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Returns `true` when the collection contains no IDs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Borrows the IDs in collection order.
    #[must_use]
    pub fn objects(&self) -> &[ObjectId<T>] {
        &self.objects
    }

    /// Consumes the collection and returns its backing ID vector.
    #[must_use]
    pub fn into_objects(self) -> Vec<ObjectId<T>> {
        self.objects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ObjectUid, PortObject};

    #[test]
    fn collections_store_typed_permanent_ids() {
        let id = ObjectId::<PortObject>::from_uid(ObjectUid::from_raw(7).unwrap());
        let collection = Collection::new(vec![id]);
        assert_eq!(collection.objects(), &[id]);
        assert_eq!(collection.len(), 1);
    }
}
