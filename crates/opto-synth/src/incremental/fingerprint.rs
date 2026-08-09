// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{Hash, Hasher, canonical_len};

pub(super) struct Fingerprint(blake3::Hasher);

impl Fingerprint {
    pub(super) fn new() -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/source-entry/v1\0");
        Self(digest)
    }

    pub(super) fn tag(&mut self, tag: u8) {
        tag.hash(self);
    }

    pub(super) fn id(&mut self, id: u32) {
        id.hash(self);
    }

    pub(super) fn finish(self) -> u64 {
        fingerprint_u64(&self.0)
    }

    pub(super) fn bytes(&self) -> [u8; 32] {
        *self.0.clone().finalize().as_bytes()
    }
}

impl Hasher for Fingerprint {
    fn finish(&self) -> u64 {
        fingerprint_u64(&self.0)
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn write_u8(&mut self, value: u8) {
        self.write(&[value]);
    }

    fn write_u16(&mut self, value: u16) {
        self.write(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.write(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    fn write_u128(&mut self, value: u128) {
        self.write(&value.to_le_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(canonical_len(value));
    }

    fn write_i8(&mut self, value: i8) {
        self.write(&value.to_le_bytes());
    }

    fn write_i16(&mut self, value: i16) {
        self.write(&value.to_le_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.write(&value.to_le_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.write(&value.to_le_bytes());
    }

    fn write_i128(&mut self, value: i128) {
        self.write(&value.to_le_bytes());
    }

    fn write_isize(&mut self, value: isize) {
        self.write_i64(
            i64::try_from(value)
                .expect("semantic fingerprint signed value exceeds 64-bit capacity"),
        );
    }
}

fn fingerprint_u64(digest: &blake3::Hasher) -> u64 {
    let bytes = digest.clone().finalize();
    u64::from_le_bytes(
        bytes.as_bytes()[..size_of::<u64>()]
            .try_into()
            .expect("BLAKE3 output contains at least eight bytes"),
    )
}
