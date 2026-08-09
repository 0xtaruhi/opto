// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn header_round_trip_preserves_payload_identity() {
    let expected = CheckpointHeader {
        payload_len: 12_345,
        checksum: [0x5a; 32],
    };
    let encoded = checkpoint_header_bytes(expected).unwrap();
    let actual = checkpoint_header(&encoded).unwrap();
    assert_eq!(actual.payload_len, expected.payload_len);
    assert_eq!(actual.checksum, expected.checksum);
}

#[test]
fn header_rejects_truncation_and_foreign_magic() {
    assert!(checkpoint_header(&[]).is_err());
    let mut encoded = checkpoint_header_bytes(CheckpointHeader {
        payload_len: 0,
        checksum: [0; 32],
    })
    .unwrap();
    encoded[0] ^= 1;
    assert!(checkpoint_header(&encoded).is_err());
}

fn persistent_state_digest(session: &Session) -> blake3::Hash {
    let checkpoint = SessionCheckpointRef {
        revision: session.state.revision,
        designs: CheckpointDesignStoreRef::new(&session.state.designs),
        current_design: &session.state.current_design,
        settings: &session.state.settings,
        timing: &session.state.timing,
        parasitics: &session.state.parasitics,
        power: &session.state.power,
        last_synthesis: &session.state.last_synthesis,
        objects: session.state.objects.snapshot_ref(),
    };
    let mut digest = HashingWriter::new(std::io::sink());
    encode_checkpoint(&checkpoint, &mut digest).expect("test session state must serialize");
    let (_, checksum) = digest.finish();
    blake3::Hash::from_bytes(checksum)
}

fn corrupt_last_byte(path: &Path) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut byte = [0; 1];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0xff;
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn schema_and_cache_abi_changes_are_rejected() {
    let valid = checkpoint_header_bytes(CheckpointHeader {
        payload_len: 0,
        checksum: [0; 32],
    })
    .unwrap();

    let mut previous_schema = valid;
    previous_schema[SCHEMA_OFFSET..CACHE_ABI_OFFSET]
        .copy_from_slice(&(CHECKPOINT_SCHEMA - 1).to_le_bytes());
    assert!(
        checkpoint_header(&previous_schema)
            .unwrap_err()
            .to_string()
            .contains("unsupported state version")
    );

    let mut previous_abi = valid;
    previous_abi[CACHE_ABI_OFFSET..FRONTEND_FINGERPRINT_OFFSET]
        .copy_from_slice(&(SYNTHESIS_CACHE_ABI - 1).to_le_bytes());
    assert!(
        checkpoint_header(&previous_abi)
            .unwrap_err()
            .to_string()
            .contains("synthesis cache ABI")
    );
}

#[test]
fn native_frontend_change_rejects_the_checkpoint() {
    let mut bytes = checkpoint_header_bytes(CheckpointHeader {
        payload_len: 0,
        checksum: [0; 32],
    })
    .unwrap();
    bytes[FRONTEND_FINGERPRINT_OFFSET] ^= 1;

    assert!(
        checkpoint_header(&bytes)
            .unwrap_err()
            .to_string()
            .contains("different native frontend implementation")
    );
}

#[test]
fn restore_rejects_declared_length_before_decode() {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "{}-{}-checkpoint-declared-length-{sequence}.ock",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    let declared = 1u64 << 40;
    let mut header = checkpoint_header_bytes(CheckpointHeader {
        payload_len: 0,
        checksum: [0; 32],
    })
    .unwrap();
    header[PAYLOAD_LEN_OFFSET..CHECKSUM_OFFSET].copy_from_slice(&declared.to_le_bytes());
    std::fs::write(&path, header).unwrap();

    let mut session = Session::new();
    let error = session.read_checkpoint_file(&path).unwrap_err();

    assert_eq!(
        error.to_string(),
        format!(
            "checkpoint: payload length mismatch: header declares {declared} bytes, file contains 0"
        )
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn restore_rejects_a_forged_archive_before_publishing_state() {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "{}-{}-checkpoint-inner-length-{sequence}.ock",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    let mut payload = vec![1, 253];
    payload.extend_from_slice(&u64::MAX.to_le_bytes());
    let header = checkpoint_header_bytes(CheckpointHeader {
        payload_len: payload.len(),
        checksum: *blake3::hash(&payload).as_bytes(),
    })
    .unwrap();
    let mut file = File::create(&path).unwrap();
    file.write_all(&header).unwrap();
    file.write_all(&payload).unwrap();
    file.sync_all().unwrap();
    drop(file);

    let mut target = Session::new();
    target.set_clock_gating_enabled(true);
    let before = persistent_state_digest(&target);
    let error = target.read_checkpoint_file(&path).unwrap_err();

    assert!(error.to_string().contains("rkyv validation failed"));
    assert_eq!(persistent_state_digest(&target), before);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn corrupted_checksum_is_rejected_without_publishing_state() {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "{}-{}-checkpoint-checksum-{sequence}.ock",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    Session::new().write_checkpoint_file(&path).unwrap();
    corrupt_last_byte(&path);

    let mut target = Session::new();
    target.set_clock_gating_enabled(true);
    let before = persistent_state_digest(&target);
    let error = target.read_checkpoint_file(&path).unwrap_err();

    assert_eq!(error.to_string(), "checkpoint: payload checksum mismatch");
    assert_eq!(persistent_state_digest(&target), before);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn decoded_checkpoint_is_fully_validated_before_publication() {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "{}-{}-checkpoint-invalid-state-{sequence}.ock",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    Session::new().write_checkpoint_file(&path).unwrap();

    let mut target = Session::new();
    target.set_clock_gating_enabled(true);
    let before = persistent_state_digest(&target);
    let error = target.read_checkpoint_file(&path).unwrap_err();

    assert_eq!(error.to_string(), "checkpoint: state contains no designs");
    assert_eq!(persistent_state_digest(&target), before);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn failed_atomic_stream_keeps_published_file_and_removes_temporary() {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "{}-{}-checkpoint-atomic-{sequence}",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("state.ock");
    std::fs::write(&path, b"published").unwrap();

    let error = atomic_stream_write(&path, |file| {
        file.write_all(b"partial").unwrap();
        Err(crate::SessionError::checkpoint("injected write failure"))
    })
    .unwrap_err();

    assert_eq!(error.to_string(), "checkpoint: injected write failure");
    assert_eq!(std::fs::read(&path).unwrap(), b"published");
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn streaming_write_replaces_the_target_atomically() {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "{}-{}-checkpoint-stream-{sequence}",
        env!("CARGO_PKG_NAME"),
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("state.ock");
    std::fs::write(&path, b"published").unwrap();

    assert_eq!(Session::new().write_checkpoint_file(&path).unwrap(), "1");

    let bytes = std::fs::read(&path).unwrap();
    assert_ne!(bytes, b"published");
    let header = checkpoint_header(&bytes[..HEADER_BYTES]).unwrap();
    assert_eq!(bytes.len(), HEADER_BYTES + header.payload_len);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
