// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical object-registry checkpoint wire boundary.

use super::{
    Arc, Deserialize, HashMap, NameTable, ObjectKey, ObjectRegistry, ObjectUid, RegistryError,
    Serialize, fmt,
};
use serde::de::{DeserializeSeed, Error as DeError, MapAccess, SeqAccess, Visitor};
use serde::ser::{Error as SerError, SerializeSeq};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(in crate::object) struct SnapshotRecord {
    pub(in crate::object) uid: ObjectUid,
    pub(in crate::object) key: ObjectKey,
}

#[derive(Debug)]
/// Streaming restore owner for the canonical [`ObjectRegistry`] wire image.
///
/// Deserialization validates each record and inserts it directly into the
/// final compact arena and indexes. A semantic failure is retained until
/// [`ObjectRegistry::from_snapshot`] so checkpoint error classification stays
/// independent from wire decoding.
pub struct ObjectRegistrySnapshot(Result<ObjectRegistry, RegistryError>);

/// Borrowed serialization view with the same canonical wire layout as
/// [`ObjectRegistrySnapshot`].
#[derive(Debug, Clone, Copy)]
pub struct ObjectRegistrySnapshotRef<'a>(&'a ObjectRegistry);

impl Serialize for ObjectRegistrySnapshotRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct SnapshotRef<'a> {
            names: &'a NameTable,
            next_uid: u64,
            records: LiveRecords<'a>,
        }

        SnapshotRef {
            names: &self.0.names,
            next_uid: self.0.next_uid,
            records: LiveRecords(self.0),
        }
        .serialize(serializer)
    }
}

impl Serialize for ObjectRegistrySnapshot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            Ok(registry) => ObjectRegistrySnapshotRef(registry).serialize(serializer),
            Err(error) => Err(S::Error::custom(error.to_string())),
        }
    }
}

impl<'de> Deserialize<'de> for ObjectRegistrySnapshot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "ObjectRegistrySnapshot",
            &["names", "next_uid", "records"],
            SnapshotVisitor,
        )
    }
}

struct SnapshotVisitor;

impl<'de> Visitor<'de> for SnapshotVisitor {
    type Value = ObjectRegistrySnapshot;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object registry snapshot")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let names = sequence
            .next_element()?
            .ok_or_else(|| A::Error::invalid_length(0, &self))?;
        let next_uid = sequence
            .next_element()?
            .ok_or_else(|| A::Error::invalid_length(1, &self))?;
        sequence
            .next_element_seed(SnapshotRecordsSeed { names, next_uid })?
            .ok_or_else(|| A::Error::invalid_length(2, &self))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut names = None;
        let mut next_uid = None;
        let mut snapshot = None;
        while let Some(field) = map.next_key()? {
            match field {
                SnapshotField::Names => {
                    if names.is_some() || snapshot.is_some() {
                        return Err(A::Error::duplicate_field("names"));
                    }
                    names = Some(map.next_value()?);
                }
                SnapshotField::NextUid => {
                    if next_uid.is_some() || snapshot.is_some() {
                        return Err(A::Error::duplicate_field("next_uid"));
                    }
                    next_uid = Some(map.next_value()?);
                }
                SnapshotField::Records => {
                    if snapshot.is_some() {
                        return Err(A::Error::duplicate_field("records"));
                    }
                    let names = names.take().ok_or_else(|| {
                        A::Error::custom("object registry records precede its name table")
                    })?;
                    let next_uid = next_uid.take().ok_or_else(|| {
                        A::Error::custom("object registry records precede its UID high-water mark")
                    })?;
                    snapshot = Some(map.next_value_seed(SnapshotRecordsSeed { names, next_uid })?);
                }
            }
        }
        if let Some(snapshot) = snapshot {
            return Ok(snapshot);
        }
        names.ok_or_else(|| A::Error::missing_field("names"))?;
        next_uid.ok_or_else(|| A::Error::missing_field("next_uid"))?;
        Err(A::Error::missing_field("records"))
    }
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum SnapshotField {
    Names,
    NextUid,
    Records,
}

struct SnapshotRecordsSeed {
    names: NameTable,
    next_uid: u64,
}

impl<'de> DeserializeSeed<'de> for SnapshotRecordsSeed {
    type Value = ObjectRegistrySnapshot;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(SnapshotRecordsVisitor {
            names: self.names,
            next_uid: self.next_uid,
        })
    }
}

struct SnapshotRecordsVisitor {
    names: NameTable,
    next_uid: u64,
}

impl<'de> Visitor<'de> for SnapshotRecordsVisitor {
    type Value = ObjectRegistrySnapshot;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a UID-ordered object registry record sequence")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut restore = Ok(SnapshotRestore::new(self.names, self.next_uid));
        while let Some(record) = sequence.next_element()? {
            restore_snapshot_record(&mut restore, record);
        }
        Ok(ObjectRegistrySnapshot(restore.map(SnapshotRestore::finish)))
    }
}

struct SnapshotRestore {
    registry: ObjectRegistry,
    previous_uid: Option<ObjectUid>,
}

impl SnapshotRestore {
    fn new(names: NameTable, next_uid: u64) -> Self {
        Self {
            registry: ObjectRegistry {
                owner: Arc::new(()),
                next_uid,
                names,
                slots: Vec::new(),
                head: None,
                tail: None,
                free: None,
                len: 0,
                slots_by_uid: HashMap::new(),
                active: HashMap::new(),
                by_design: HashMap::new(),
            },
            previous_uid: None,
        }
    }

    fn push(&mut self, SnapshotRecord { uid, key }: SnapshotRecord) -> Result<(), RegistryError> {
        if self.previous_uid.is_some_and(|previous| previous >= uid) {
            return Err(RegistryError::InvalidSnapshot(
                "live object records are not strictly UID-sorted".to_string(),
            ));
        }
        self.previous_uid = Some(uid);
        if uid.get().get() > self.registry.next_uid {
            return Err(RegistryError::InvalidSnapshot(format!(
                "contains object UID {} above high-water mark {}",
                uid.get(),
                self.registry.next_uid
            )));
        }
        if key.resolve(&self.registry.names).is_none() {
            return Err(RegistryError::InvalidSnapshot(
                "contains an object key with an unknown name ID".to_string(),
            ));
        }
        if self.registry.active.contains_key(&key) {
            return Err(RegistryError::InvalidSnapshot(
                "contains duplicate live object locators".to_string(),
            ));
        }
        self.registry.push_live(uid, key).map(|_| ())
    }

    fn finish(self) -> ObjectRegistry {
        self.registry
    }
}

fn restore_snapshot_record(
    restore: &mut Result<SnapshotRestore, RegistryError>,
    record: SnapshotRecord,
) {
    let error = restore
        .as_mut()
        .ok()
        .and_then(|restore| restore.push(record).err());
    if let Some(error) = error {
        *restore = Err(error);
    }
}

struct LiveRecords<'a>(&'a ObjectRegistry);

impl Serialize for LiveRecords<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len))?;
        for (_, record) in self.0.live_records() {
            sequence.serialize_element(&SnapshotRecord {
                uid: record.uid,
                key: record.key,
            })?;
        }
        sequence.end()
    }
}

impl ObjectRegistry {
    #[must_use]
    /// Borrows a serializable view of the registry's current state.
    pub fn snapshot_ref(&self) -> ObjectRegistrySnapshotRef<'_> {
        ObjectRegistrySnapshotRef(self)
    }

    /// Takes ownership of a registry restored and validated while decoding.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::InvalidSnapshot`] for malformed ordering,
    /// duplicate locators, unknown names, or UIDs beyond the high-water mark.
    /// Capacity failures are reported through [`RegistryError::Capacity`].
    pub fn from_snapshot(snapshot: ObjectRegistrySnapshot) -> Result<Self, RegistryError> {
        snapshot.0
    }
}

#[cfg(test)]
impl ObjectRegistrySnapshot {
    pub(in crate::object) fn from_records(
        names: NameTable,
        next_uid: u64,
        records: impl IntoIterator<Item = SnapshotRecord>,
    ) -> Self {
        let mut restore = Ok(SnapshotRestore::new(names, next_uid));
        for record in records {
            restore_snapshot_record(&mut restore, record);
        }
        Self(restore.map(SnapshotRestore::finish))
    }
}
