// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn allocation_model_charges_payload_slack_and_metadata() {
    assert_eq!(allocation_bytes(0), 0);
    assert_eq!(
        allocation_bytes(100),
        100 + 25 + std::mem::size_of::<usize>() * 2
    );
}

#[test]
fn allocation_model_saturates_instead_of_wrapping() {
    assert_eq!(allocation_bytes(usize::MAX), usize::MAX);
}

#[test]
fn slice_bytes_scales_with_the_element_layout() {
    assert_eq!(
        slice_bytes::<u32>(4),
        allocation_bytes(4 * std::mem::size_of::<u32>())
    );
}

#[test]
fn the_name_table_wire_marker_is_stable() {
    assert_eq!(NAME_TABLE_WIRE_NAME, "opto.NameTable");
}
