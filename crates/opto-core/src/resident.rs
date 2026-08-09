// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic accounting for allocations already owned by compact data
//! structures.
//!
//! These helpers describe retained logical storage for diagnostics and
//! benchmarks. They do not predict future allocations or participate in task
//! scheduling.

use std::mem::size_of;

const ALLOCATION_METADATA_BYTES: usize = size_of::<usize>() * 2;
const ALLOCATOR_MARGIN_DIVISOR: usize = 4;

/// Stable serde marker used by the checkpoint decoder.
pub const NAME_TABLE_WIRE_NAME: &str = "opto.NameTable";

/// Account one logical allocation with deterministic allocator slack.
#[must_use]
pub fn allocation_bytes(payload_bytes: usize) -> usize {
    if payload_bytes == 0 {
        return 0;
    }
    payload_bytes
        .saturating_add(payload_bytes.div_ceil(ALLOCATOR_MARGIN_DIVISOR))
        .saturating_add(ALLOCATION_METADATA_BYTES)
}

/// Account one contiguous logical allocation containing `len` values.
#[must_use]
pub fn slice_bytes<T>(len: usize) -> usize {
    allocation_bytes(size_of::<T>().saturating_mul(len))
}

#[cfg(test)]
mod tests;
