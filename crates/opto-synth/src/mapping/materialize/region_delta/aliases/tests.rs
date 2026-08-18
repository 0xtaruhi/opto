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

#[test]
fn known_constant_observations_do_not_transfer_their_aliased_net() {
    let mut module = word::WordModule::new("known_constant_observation");
    let unknown = module
        .constant(
            opto_ir::ConstBits::from_bits(vec![opto_ir::BitVal::X]).unwrap(),
            word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let zero = module
        .constant(
            opto_ir::ConstBits::from_bits(vec![opto_ir::BitVal::Zero]).unwrap(),
            word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let value = module
        .binary(
            word::BinaryOp::BitAnd,
            unknown,
            zero,
            word::SourceSpan::default(),
        )
        .unwrap();
    let aliased_net = NetId::from_index(0).unwrap();

    let signals =
        WordMappedSignals::from_observations(&module, &[value], &[Some(aliased_net)]).unwrap();

    assert_eq!(
        signals.require(value).unwrap(),
        MappedValueSignal::Constant(false)
    );
}
