// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn fingerprint_value<T: Serialize + ?Sized>(
    writer: &mut blake3::Hasher,
    value: &T,
) -> Result<(), opto_archive::ArchiveError> {
    opto_archive::encode_into_std_write(value, writer).map(|_| ())
}

pub(super) fn fingerprint_f64(
    writer: &mut blake3::Hasher,
    value: f64,
) -> Result<(), opto_archive::ArchiveError> {
    fingerprint_value(writer, &canonical_f64(value))
}

pub(super) fn fingerprint_f64_pair(
    writer: &mut blake3::Hasher,
    values: [f64; 2],
) -> Result<(), opto_archive::ArchiveError> {
    fingerprint_value(writer, &values.map(canonical_f64))
}

pub(super) fn fingerprint_optional_f64_pair(
    writer: &mut blake3::Hasher,
    values: Option<[f64; 2]>,
) -> Result<(), opto_archive::ArchiveError> {
    fingerprint_value(writer, &values.map(|pair| pair.map(canonical_f64)))
}

fn canonical_f64(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}
