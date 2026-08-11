// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::ParasiticsState;
use crate::power::PowerContext;
use crate::state::DatabaseSettings;
use crate::{DesignStore, HdlCatalog, Session};
use opto_db::{
    AnyObjectId, ObjectRegistry, ObjectRegistrySnapshot, ObjectRegistrySnapshotRef, RevisionId,
};
use opto_synth::SynthesisReport;
use opto_timing::{TimingContext, TimingContextCheckpoint};
use serde::{Deserialize, Serialize};
use std::fs::File;
#[cfg(test)]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

const CHECKPOINT_MAGIC: &[u8; 8] = b"OPTOCKPT";
const CHECKPOINT_SCHEMA: u32 = 31;
const SYNTHESIS_CACHE_ABI: u32 = 12;
const NATIVE_FRONTEND_FINGERPRINT_BYTES: usize = 16;
const SCHEMA_OFFSET: usize = 8;
const CACHE_ABI_OFFSET: usize = SCHEMA_OFFSET + 4;
const FRONTEND_FINGERPRINT_OFFSET: usize = CACHE_ABI_OFFSET + 4;
const PAYLOAD_LEN_OFFSET: usize = FRONTEND_FINGERPRINT_OFFSET + NATIVE_FRONTEND_FINGERPRINT_BYTES;
const CHECKSUM_OFFSET: usize = PAYLOAD_LEN_OFFSET + 8;
const HEADER_BYTES: usize = CHECKSUM_OFFSET + 32;
const CHECKSUM_BUFFER_BYTES: usize = 16 * 1024;
#[cfg(test)]
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod designs;
mod validation;

use designs::{CheckpointDesignStore, CheckpointDesignStoreRef, ValidatedDesignStore};
use validation::{validate_checkpoint_objects, validate_design_relationships};

#[derive(Debug, Deserialize)]
struct SessionCheckpointWire {
    revision: RevisionId,
    designs: CheckpointDesignStore,
    current_design: Option<String>,
    settings: DatabaseSettings,
    timing: TimingContextCheckpoint,
    parasitics: ParasiticsState,
    power: PowerContext,
    last_synthesis: Option<SynthesisReport>,
    objects: ObjectRegistrySnapshot,
}

struct SessionCheckpoint {
    revision: RevisionId,
    designs: ValidatedDesignStore,
    current_design: Option<String>,
    settings: DatabaseSettings,
    timing: TimingContextCheckpoint,
    parasitics: ParasiticsState,
    power: PowerContext,
    last_synthesis: Option<SynthesisReport>,
    objects: ObjectRegistrySnapshot,
}

impl SessionCheckpointWire {
    fn rebuild_design_indexes(
        self,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<SessionCheckpoint, crate::SessionError> {
        Ok(SessionCheckpoint {
            revision: self.revision,
            designs: self.designs.rebuild(runtime)?,
            current_design: self.current_design,
            settings: self.settings,
            timing: self.timing,
            parasitics: self.parasitics,
            power: self.power,
            last_synthesis: self.last_synthesis,
            objects: self.objects,
        })
    }
}

#[derive(Serialize)]
struct SessionCheckpointRef<'a> {
    revision: RevisionId,
    designs: CheckpointDesignStoreRef<'a>,
    current_design: &'a Option<String>,
    settings: &'a DatabaseSettings,
    timing: &'a TimingContext,
    parasitics: &'a ParasiticsState,
    power: &'a PowerContext,
    last_synthesis: &'a Option<SynthesisReport>,
    objects: ObjectRegistrySnapshotRef<'a>,
}

#[derive(Debug, Clone, Copy)]
struct CheckpointHeader {
    payload_len: usize,
    checksum: [u8; 32],
}

struct PreparedCheckpoint {
    revision: RevisionId,
    designs: DesignStore,
    current_design: Option<String>,
    settings: DatabaseSettings,
    timing: TimingContext,
    parasitics: ParasiticsState,
    power: PowerContext,
    last_synthesis: Option<SynthesisReport>,
    objects: ObjectRegistry,
    restored_names: String,
}

struct HashingWriter<W> {
    inner: W,
    hasher: blake3::Hasher,
    bytes: usize,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            bytes: 0,
        }
    }

    fn finish(self) -> (usize, [u8; 32]) {
        (self.bytes, *self.hasher.finalize().as_bytes())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.bytes = self
            .bytes
            .checked_add(written)
            .ok_or_else(|| std::io::Error::other("checkpoint payload length overflow"))?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn prepare_checkpoint(
    checkpoint: SessionCheckpoint,
) -> Result<PreparedCheckpoint, crate::SessionError> {
    let SessionCheckpoint {
        revision,
        designs,
        current_design,
        settings,
        timing,
        parasitics,
        power,
        last_synthesis,
        objects,
    } = checkpoint;
    if designs.as_store().records.is_empty() {
        return Err(crate::SessionError::checkpoint("state contains no designs"));
    }
    if let Some(current) = current_design.as_deref()
        && !designs.as_store().contains_key(current)
    {
        return Err(crate::SessionError::checkpoint(format!(
            "current design '{current}' is absent from the saved design store"
        )));
    }
    validate_design_relationships(&designs, revision)?;
    for (design, (design_revision, database)) in parasitics.iter() {
        if !designs.as_store().contains_key(design) {
            return Err(crate::SessionError::checkpoint(format!(
                "parasitics reference absent design '{design}'"
            )));
        }
        if *design_revision > parasitics.revision() {
            return Err(crate::SessionError::checkpoint(format!(
                "design '{design}' has a parasitic revision newer than the saved generator"
            )));
        }
        database.validate_checkpoint().map_err(|error| {
            crate::SessionError::checkpoint(format!(
                "design '{design}' has invalid parasitics: {error}"
            ))
        })?;
    }
    let objects = ObjectRegistry::from_snapshot(objects)
        .map_err(|error| crate::SessionError::checkpoint(error.to_string()))?;
    let timing = timing
        .restore()
        .map_err(|error| crate::SessionError::checkpoint(error.to_string()))?;
    validate_checkpoint_objects(designs.as_store(), &objects)?;
    if !timing.checkpoint_objects_are_valid(&objects) {
        return Err(crate::SessionError::checkpoint(
            "timing constraints reference objects absent from the restored registry",
        ));
    }
    for object in power.activities.keys() {
        if !matches!(object, AnyObjectId::Port(_) | AnyObjectId::Net(_))
            || objects.resolve(*object).is_none()
        {
            return Err(crate::SessionError::checkpoint(format!(
                "switching activity references absent or invalid object {object:?}"
            )));
        }
    }

    let restored_names = restored_design_names(designs.as_store())?;
    Ok(PreparedCheckpoint {
        revision,
        designs: designs.into_store(),
        current_design,
        settings,
        timing,
        parasitics,
        power,
        last_synthesis,
        objects,
        restored_names,
    })
}

fn install_checkpoint(
    session: &mut Session,
    checkpoint: PreparedCheckpoint,
) -> Result<String, crate::SessionError> {
    session.process.handles.validate_registry_replacement()?;
    session.process.clear_analysis_caches();
    session
        .process
        .handles
        .invalidate_for_registry_replacement()?;
    session.state.revision = checkpoint.revision;
    session.state.designs = checkpoint.designs;
    session.state.current_design = checkpoint.current_design;
    session.state.settings = checkpoint.settings;
    session.state.timing = checkpoint.timing;
    session.state.parasitics = checkpoint.parasitics;
    session.state.power = checkpoint.power;
    session.state.last_synthesis = checkpoint.last_synthesis;
    session.state.objects = checkpoint.objects;
    session.state.hdl_catalog = HdlCatalog::from_designs(&session.state.designs);
    Ok(checkpoint.restored_names)
}

fn restored_design_names_len(designs: &DesignStore) -> Result<usize, crate::SessionError> {
    let names = designs
        .keys()
        .try_fold(0usize, |bytes, name| bytes.checked_add(name.len()));
    names
        .and_then(|bytes| bytes.checked_add(designs.records.len().saturating_sub(1)))
        .ok_or_else(|| {
            crate::SessionError::checkpoint("restored design names exceed host capacity")
        })
}

fn restored_design_names(designs: &DesignStore) -> Result<String, crate::SessionError> {
    let capacity = restored_design_names_len(designs)?;
    let mut names = String::new();
    names.try_reserve_exact(capacity).map_err(|_| {
        crate::SessionError::checkpoint("could not allocate restored design-name result")
    })?;
    for name in designs.keys() {
        if !names.is_empty() {
            names.push(' ');
        }
        names.push_str(name);
    }
    Ok(names)
}

fn encode_checkpoint(
    checkpoint: &SessionCheckpointRef<'_>,
    writer: &mut impl Write,
) -> Result<usize, crate::SessionError> {
    opto_archive::encode_into_std_write(checkpoint, writer).map_err(|error| {
        crate::SessionError::checkpoint(format!("failed to encode state: {error}"))
    })
}

fn checkpoint_header_bytes(
    header: CheckpointHeader,
) -> Result<[u8; HEADER_BYTES], crate::SessionError> {
    let payload_len = u64::try_from(header.payload_len).map_err(|_| {
        crate::SessionError::checkpoint("encoded state exceeds 64-bit file capacity")
    })?;
    let mut bytes = [0; HEADER_BYTES];
    bytes[..8].copy_from_slice(CHECKPOINT_MAGIC);
    bytes[SCHEMA_OFFSET..CACHE_ABI_OFFSET].copy_from_slice(&CHECKPOINT_SCHEMA.to_le_bytes());
    bytes[CACHE_ABI_OFFSET..FRONTEND_FINGERPRINT_OFFSET]
        .copy_from_slice(&SYNTHESIS_CACHE_ABI.to_le_bytes());
    bytes[FRONTEND_FINGERPRINT_OFFSET..PAYLOAD_LEN_OFFSET]
        .copy_from_slice(native_frontend_fingerprint());
    bytes[PAYLOAD_LEN_OFFSET..CHECKSUM_OFFSET].copy_from_slice(&payload_len.to_le_bytes());
    bytes[CHECKSUM_OFFSET..].copy_from_slice(&header.checksum);
    Ok(bytes)
}

fn native_frontend_fingerprint() -> &'static [u8; NATIVE_FRONTEND_FINGERPRINT_BYTES] {
    opto_hdl::NATIVE_FRONTEND_FINGERPRINT
        .as_bytes()
        .try_into()
        .expect("native frontend fingerprint is a 64-bit lowercase hexadecimal digest")
}

fn checksum_payload(
    file: &mut File,
    payload_len: usize,
    path: &Path,
) -> Result<[u8; 32], crate::SessionError> {
    let mut hasher = blake3::Hasher::new();
    let mut remaining = payload_len;
    let mut buffer = [0; CHECKSUM_BUFFER_BYTES];
    // Stream the declared payload so checksum validation never duplicates the
    // checkpoint's serialized bytes in memory. The trailing-byte probe also
    // binds the header length to the physical file length.
    while remaining != 0 {
        let chunk = remaining.min(buffer.len());
        file.read_exact(&mut buffer[..chunk])
            .map_err(|source| crate::SessionError::Io {
                operation: "read checkpoint",
                path: path.to_path_buf(),
                source,
            })?;
        hasher.update(&buffer[..chunk]);
        remaining -= chunk;
    }
    let mut trailing = [0; 1];
    if file
        .read(&mut trailing)
        .map_err(|source| crate::SessionError::Io {
            operation: "read checkpoint",
            path: path.to_path_buf(),
            source,
        })?
        != 0
    {
        return Err(crate::SessionError::checkpoint(
            "checkpoint contains data beyond its declared payload",
        ));
    }
    Ok(*hasher.finalize().as_bytes())
}

fn checkpoint_header(bytes: &[u8]) -> Result<CheckpointHeader, crate::SessionError> {
    if bytes.get(..8) != Some(CHECKPOINT_MAGIC) {
        return Err(crate::SessionError::checkpoint(
            "file is not an Opto checkpoint",
        ));
    }
    if bytes.len() < HEADER_BYTES {
        return Err(crate::SessionError::checkpoint("state header is truncated"));
    }
    let schema = u32::from_le_bytes(
        bytes[SCHEMA_OFFSET..CACHE_ABI_OFFSET]
            .try_into()
            .expect("checked header length"),
    );
    let cache_abi = u32::from_le_bytes(
        bytes[CACHE_ABI_OFFSET..FRONTEND_FINGERPRINT_OFFSET]
            .try_into()
            .expect("checked header length"),
    );
    if schema != CHECKPOINT_SCHEMA || cache_abi != SYNTHESIS_CACHE_ABI {
        return Err(crate::SessionError::checkpoint(format!(
            "unsupported state version (schema {schema}, synthesis cache ABI {cache_abi})"
        )));
    }
    if bytes[FRONTEND_FINGERPRINT_OFFSET..PAYLOAD_LEN_OFFSET] != *native_frontend_fingerprint() {
        return Err(crate::SessionError::checkpoint(
            "checkpoint was produced by a different native frontend implementation",
        ));
    }
    let declared = u64::from_le_bytes(
        bytes[PAYLOAD_LEN_OFFSET..CHECKSUM_OFFSET]
            .try_into()
            .expect("checked header length"),
    );
    let payload_len = usize::try_from(declared)
        .map_err(|_| crate::SessionError::checkpoint("payload length exceeds host capacity"))?;
    let checksum = bytes[CHECKSUM_OFFSET..HEADER_BYTES]
        .try_into()
        .expect("checksum field has fixed length");
    Ok(CheckpointHeader {
        payload_len,
        checksum,
    })
}

fn atomic_stream_write(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<(), crate::SessionError>,
) -> Result<(), crate::SessionError> {
    super::atomic_file::write_atomically(path, "write checkpoint", write)
}

#[cfg(test)]
mod tests;
impl Session {
    /// Serialize persistent session state to a validated checkpoint file.
    pub fn write_checkpoint_file(&self, path: &Path) -> Result<String, crate::SessionError> {
        let checkpoint = SessionCheckpointRef {
            revision: self.state.revision,
            designs: CheckpointDesignStoreRef::new(&self.state.designs),
            current_design: &self.state.current_design,
            settings: &self.state.settings,
            timing: &self.state.timing,
            parasitics: &self.state.parasitics,
            power: &self.state.power,
            last_synthesis: &self.state.last_synthesis,
            objects: self.state.objects.snapshot_ref(),
        };
        atomic_stream_write(path, |file| {
            file.write_all(&[0; HEADER_BYTES])
                .map_err(|source| crate::SessionError::Io {
                    operation: "reserve checkpoint header",
                    path: path.to_path_buf(),
                    source,
                })?;
            let mut payload = HashingWriter::new(&mut *file);
            let encoded_len = encode_checkpoint(&checkpoint, &mut payload)?;
            let (payload_len, checksum) = payload.finish();
            if encoded_len != payload_len {
                return Err(crate::SessionError::checkpoint(
                    "checkpoint encoder reported an inconsistent payload length",
                ));
            }
            let header = checkpoint_header_bytes(CheckpointHeader {
                payload_len,
                checksum,
            })?;
            file.seek(SeekFrom::Start(0))
                .map_err(|source| crate::SessionError::Io {
                    operation: "seek checkpoint header",
                    path: path.to_path_buf(),
                    source,
                })?;
            file.write_all(&header)
                .map_err(|source| crate::SessionError::Io {
                    operation: "write checkpoint header",
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(())
        })?;
        Ok("1".to_string())
    }

    /// Replace persistent session state from a validated checkpoint file.
    pub fn read_checkpoint_file(&mut self, path: &Path) -> Result<String, crate::SessionError> {
        let mut file = File::open(path).map_err(|source| crate::SessionError::Io {
            operation: "read checkpoint",
            path: path.to_path_buf(),
            source,
        })?;
        let mut header_bytes = [0u8; HEADER_BYTES];
        let mut header_len = 0usize;
        // A regular file usually fills this in one read, but the format reader also
        // accepts short reads. EOF before HEADER_BYTES is passed to the header
        // decoder so it reports a format error rather than an incidental I/O error.
        while header_len < HEADER_BYTES {
            let read = file
                .read(&mut header_bytes[header_len..])
                .map_err(|source| crate::SessionError::Io {
                    operation: "read checkpoint",
                    path: path.to_path_buf(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            header_len += read;
        }
        let header = checkpoint_header(&header_bytes[..header_len])?;
        let file_len = file
            .metadata()
            .map_err(|source| crate::SessionError::Io {
                operation: "inspect checkpoint",
                path: path.to_path_buf(),
                source,
            })?
            .len();
        let payload_len = u64::try_from(header.payload_len).map_err(|_| {
            crate::SessionError::checkpoint("checkpoint size exceeds host capacity")
        })?;
        let expected_len = (HEADER_BYTES as u64)
            .checked_add(payload_len)
            .ok_or_else(|| {
                crate::SessionError::checkpoint("checkpoint size exceeds host capacity")
            })?;
        if file_len != expected_len {
            let actual_payload = file_len.saturating_sub(HEADER_BYTES as u64);
            return Err(crate::SessionError::checkpoint(format!(
                "payload length mismatch: header declares {} bytes, file contains {actual_payload}",
                header.payload_len
            )));
        }
        let checksum = checksum_payload(&mut file, header.payload_len, path)?;
        if checksum != header.checksum {
            return Err(crate::SessionError::checkpoint("payload checksum mismatch"));
        }
        file.seek(SeekFrom::Start(HEADER_BYTES as u64))
            .map_err(|source| crate::SessionError::Io {
                operation: "seek checkpoint payload",
                path: path.to_path_buf(),
                source,
            })?;
        let checkpoint: SessionCheckpointWire =
            opto_archive::decode_from_std_read(&mut file, header.payload_len).map_err(|error| {
                crate::SessionError::checkpoint(format!("failed to decode state: {error}"))
            })?;
        let checkpoint = checkpoint.rebuild_design_indexes(&self.process.runtime)?;
        install_checkpoint(self, prepare_checkpoint(checkpoint)?)
    }
}
