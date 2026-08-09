// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn epoch_snapshot_shares_immutable_binding_arenas() {
    let value = word::ValueId::from_index(0).unwrap();
    let binding = RegionPlanBinding {
        inputs: vec![RegionPlanValueBinding::Lowered(value)].into(),
        outputs: vec![vec![RegionPlanValueBinding::Lowered(value)].into()].into(),
    };

    let snapshot = binding.clone();

    assert!(Arc::ptr_eq(&binding.inputs, &snapshot.inputs));
    assert!(Arc::ptr_eq(&binding.outputs, &snapshot.outputs));
    assert!(Arc::ptr_eq(&binding.outputs[0], &snapshot.outputs[0]));
}
