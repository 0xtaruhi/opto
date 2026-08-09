// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn conflicting_observations_are_rejected() {
    let module = word::WordModule::new("observations");
    let value = word::ValueId::from_index(0).unwrap();
    let first = NetId::from_index(0).unwrap();
    let second = NetId::from_index(1).unwrap();

    let error = WordMappedSignals::from_observations(
        &module,
        &[value, value],
        &[Some(first), Some(second)],
    )
    .unwrap_err();

    assert!(error.to_string().contains("conflicting observations"));
}
