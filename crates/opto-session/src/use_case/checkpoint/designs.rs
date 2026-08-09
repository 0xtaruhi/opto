// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Checkpoint-only design records.
//!
//! Source [`DesignIndex`](opto_db::DesignIndex) values are rebuilt from RTL.
//! Mapped object queries borrow the serialized synthesis artifact directly;
//! the wire format stores only the one-bit active-view selection and rebuilds
//! its compact slot-order sidecar during restore.

use crate::{ArtifactBinding, DesignRecord, DesignStore};
use opto_db::RevisionId;
use opto_ir::rtl::{RtlModuleCheckpoint, RtlModuleCheckpointRef};
use opto_synth::{IncrementalSnapshot, SynthesisResult};
use serde::ser::{SerializeMap, SerializeStruct};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;

pub(super) struct CheckpointDesignStoreRef<'a>(&'a DesignStore);

impl<'a> CheckpointDesignStoreRef<'a> {
    pub(super) const fn new(store: &'a DesignStore) -> Self {
        Self(store)
    }
}

impl Serialize for CheckpointDesignStoreRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut store = serializer.serialize_struct("DesignStore", 1)?;
        store.serialize_field("records", &CheckpointDesignRecordMapRef(&self.0.records))?;
        store.end()
    }
}

struct CheckpointDesignRecordMapRef<'a>(&'a BTreeMap<String, DesignRecord>);

impl Serialize for CheckpointDesignRecordMapRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut records = serializer.serialize_map(Some(self.0.len()))?;
        for (name, record) in self.0 {
            records.serialize_entry(name, &CheckpointDesignRecordRef::from(record))?;
        }
        records.end()
    }
}

#[derive(Serialize)]
struct CheckpointDesignRecordRef<'a> {
    source: RtlModuleCheckpointRef<'a>,
    source_revision: RevisionId,
    synthesized: &'a Option<SynthesisResult>,
    synthesis_binding: &'a Option<ArtifactBinding>,
    incremental_snapshot: &'a Option<IncrementalSnapshot>,
    object_index_is_mapped: bool,
}

impl<'a> From<&'a DesignRecord> for CheckpointDesignRecordRef<'a> {
    fn from(record: &'a DesignRecord) -> Self {
        Self {
            source: RtlModuleCheckpointRef::new(&record.source),
            source_revision: record.source_revision,
            synthesized: &record.synthesized,
            synthesis_binding: &record.synthesis_binding,
            incremental_snapshot: &record.incremental_snapshot,
            object_index_is_mapped: record.mapped_object_index.is_some(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct CheckpointDesignStore {
    records: BTreeMap<String, CheckpointDesignRecord>,
}

/// Design owners and rebuilt indexes whose local checkpoint invariants hold.
///
/// The wrapper exposes no mutable access, so preparation can trust those
/// owner-local proofs while it validates relationships to the other restored
/// session owners.
pub(super) struct ValidatedDesignStore(DesignStore);

impl ValidatedDesignStore {
    pub(super) const fn as_store(&self) -> &DesignStore {
        &self.0
    }

    pub(super) fn into_store(self) -> DesignStore {
        self.0
    }
}

#[derive(Debug, Deserialize)]
struct CheckpointDesignRecord {
    source: RtlModuleCheckpoint,
    source_revision: RevisionId,
    synthesized: Option<SynthesisResult>,
    synthesis_binding: Option<ArtifactBinding>,
    incremental_snapshot: Option<IncrementalSnapshot>,
    object_index_is_mapped: bool,
}

impl CheckpointDesignStore {
    pub(super) fn rebuild(
        self,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<ValidatedDesignStore, crate::SessionError> {
        let mut records = BTreeMap::new();
        for (name, wire) in self.records {
            let CheckpointDesignRecord {
                source,
                source_revision,
                mut synthesized,
                synthesis_binding,
                incremental_snapshot,
                object_index_is_mapped,
            } = wire;
            let source = source.into_inner();
            if source.word().name() != name {
                return Err(crate::SessionError::checkpoint(format!(
                    "design store key '{name}' disagrees with its saved RTL name"
                )));
            }
            if !matches!(
                (
                    synthesized.is_some(),
                    synthesis_binding.is_some(),
                    incremental_snapshot.is_some(),
                ),
                (true, true, false) | (false, false, true | false)
            ) {
                return Err(crate::SessionError::checkpoint(format!(
                    "design '{name}' has a partial synthesized state"
                )));
            }
            if object_index_is_mapped && synthesized.is_none() {
                return Err(crate::SessionError::checkpoint(format!(
                    "design '{name}' selects mapped objects without a synthesis artifact"
                )));
            }
            source.validate().map_err(|error| {
                crate::SessionError::checkpoint(format!(
                    "design '{name}' cannot rebuild an index from invalid RTL: {error}"
                ))
            })?;
            if let Some(synthesis) = synthesized.as_mut() {
                synthesis.validate_checkpoint().map_err(|error| {
                    crate::SessionError::checkpoint(format!(
                        "design '{name}' has an invalid synthesis artifact: {error}"
                    ))
                })?;
                synthesis.compact();
            }
            if let Some(snapshot) = &incremental_snapshot {
                snapshot.validate_checkpoint().map_err(|error| {
                    crate::SessionError::checkpoint(format!(
                        "design '{name}' has an invalid incremental snapshot: {error}"
                    ))
                })?;
            }
            let object_index = crate::build_object_index(&source).map_err(|error| {
                crate::SessionError::checkpoint(format!(
                    "design '{name}' object index cannot be rebuilt: {error}"
                ))
            })?;
            if object_index.name != name {
                return Err(crate::SessionError::checkpoint(format!(
                    "design store key '{name}' disagrees with its rebuilt object index"
                )));
            }
            object_index.validate().map_err(|error| {
                crate::SessionError::checkpoint(format!(
                    "design '{name}' rebuilt an invalid object index: {error}"
                ))
            })?;
            let mapped_object_index = object_index_is_mapped
                .then(|| {
                    let synthesis = synthesized
                        .as_ref()
                        .expect("mapped object selection was validated");
                    crate::MappedObjectIndex::new(synthesis.mapped(), runtime)
                })
                .transpose()
                .map_err(|error| {
                    crate::SessionError::checkpoint(format!(
                        "design '{name}' mapped object sidecar cannot be rebuilt: {error}"
                    ))
                })?;
            let record = DesignRecord {
                source,
                source_revision,
                synthesized,
                synthesis_binding,
                incremental_snapshot,
                object_index,
                mapped_object_index,
            };
            if records.insert(name, record).is_some() {
                unreachable!("decoded map keys are unique");
            }
        }
        Ok(ValidatedDesignStore(DesignStore { records }))
    }
}
