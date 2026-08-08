// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::collections::BTreeMap;

fn input_word(
    module: &mut word::WordModule,
    name: &str,
    width: u32,
) -> (Vec<(word::SignalId, word::ValueId)>, word::ValueId) {
    let bits = (0..width)
        .map(|bit| {
            let port = module
                .add_port(
                    format!("{name}_{bit}"),
                    word::PortDirection::Input,
                    word::WordType::bits(1).unwrap(),
                    word::SourceSpan::default(),
                )
                .unwrap();
            let signal = module.port(port).unwrap().signal;
            let value = module
                .read_signal(signal, word::SourceSpan::default())
                .unwrap();
            (signal, value)
        })
        .collect::<Vec<_>>();
    let value = module
        .concat(
            bits.iter().rev().map(|(_, value)| *value).collect(),
            word::SourceSpan::default(),
        )
        .unwrap();
    (bits, value)
}

#[test]
fn word_logic_encoding_matches_word_semantics() {
    let mut module = word::WordModule::new("symbolic");
    let (a_bits, a) = input_word(&mut module, "a", 4);
    let (b_bits, b) = input_word(&mut module, "b", 4);
    let (amount_bits, amount) = input_word(&mut module, "amount", 3);
    let (select_bits, select) = input_word(&mut module, "select", 1);
    let signed = word::WordType::new(4, true, word::LogicStateKind::FourState).unwrap();
    let signed_a = module
        .cast(
            word::CastKind::SignExtend,
            a,
            signed,
            word::SourceSpan::default(),
        )
        .unwrap();
    let signed_b = module
        .cast(
            word::CastKind::SignExtend,
            b,
            signed,
            word::SourceSpan::default(),
        )
        .unwrap();
    let replacement = module
        .extract(b, 0, 2, word::SourceSpan::default())
        .unwrap();
    let mut roots = Vec::new();
    for operation in [
        word::UnaryOp::BitNot,
        word::UnaryOp::LogicalNot,
        word::UnaryOp::ReductionAnd,
        word::UnaryOp::ReductionOr,
        word::UnaryOp::ReductionXor,
    ] {
        roots.push(
            module
                .unary(operation, a, word::SourceSpan::default())
                .unwrap(),
        );
    }
    for operation in [
        word::BinaryOp::Add,
        word::BinaryOp::Sub,
        word::BinaryOp::Mul,
        word::BinaryOp::Div,
        word::BinaryOp::Mod,
        word::BinaryOp::BitAnd,
        word::BinaryOp::BitOr,
        word::BinaryOp::BitXor,
        word::BinaryOp::LogicalAnd,
        word::BinaryOp::LogicalOr,
        word::BinaryOp::Eq,
        word::BinaryOp::Ne,
        word::BinaryOp::Lt,
        word::BinaryOp::Le,
        word::BinaryOp::Gt,
        word::BinaryOp::Ge,
    ] {
        roots.push(
            module
                .binary(operation, a, b, word::SourceSpan::default())
                .unwrap(),
        );
    }
    for operation in [
        word::BinaryOp::Add,
        word::BinaryOp::Sub,
        word::BinaryOp::Mul,
        word::BinaryOp::Div,
        word::BinaryOp::Mod,
        word::BinaryOp::Lt,
        word::BinaryOp::Le,
        word::BinaryOp::Gt,
        word::BinaryOp::Ge,
    ] {
        roots.push(
            module
                .binary(operation, signed_a, signed_b, word::SourceSpan::default())
                .unwrap(),
        );
    }
    roots.push(
        module
            .binary(
                word::BinaryOp::Ashr,
                signed_a,
                amount,
                word::SourceSpan::default(),
            )
            .unwrap(),
    );
    for operation in [
        word::BinaryOp::Shl,
        word::BinaryOp::Shr,
        word::BinaryOp::Ashr,
    ] {
        roots.push(
            module
                .binary(operation, a, amount, word::SourceSpan::default())
                .unwrap(),
        );
    }
    roots.extend([
        module
            .mux(select, a, b, word::SourceSpan::default())
            .unwrap(),
        module
            .extract(a, 1, 2, word::SourceSpan::default())
            .unwrap(),
        module
            .dynamic_extract(a, amount, 2, word::SourceSpan::default())
            .unwrap(),
        module
            .dynamic_insert(a, amount, replacement, word::SourceSpan::default())
            .unwrap(),
        module
            .cast(
                word::CastKind::ZeroExtend,
                a,
                word::WordType::bits(6).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap(),
        module
            .cast(
                word::CastKind::Truncate,
                a,
                word::WordType::bits(2).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap(),
        module
            .cast(
                word::CastKind::SignExtend,
                signed_a,
                word::WordType::new(6, true, word::LogicStateKind::FourState).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap(),
    ]);
    let combined = module
        .concat(roots.clone(), word::SourceSpan::default())
        .unwrap();
    roots.push(combined);
    let mut boundary_values = BTreeMap::new();
    for (signal, value) in a_bits
        .into_iter()
        .chain(b_bits)
        .chain(amount_bits)
        .chain(select_bits)
    {
        boundary_values.insert((signal, 0), value);
    }

    for root in roots {
        let mut encoder = WordLogicEncoder::new(&module);
        encoder.begin_unbound();
        let outputs = encoder.values(&[root]).unwrap();
        let (logic, order) = encoder.into_logic();
        let boundary = order
            .iter()
            .map(|key| boundary_values[key])
            .collect::<Vec<_>>();
        let proof = opto_formal::prove_value_against_logic_at_cut(
            &module, root, &logic, &outputs, &boundary,
        )
        .unwrap();
        if let Err(counterexample) = proof.require_proved() {
            panic!("symbolic encoding differs for {root:?}: {counterexample}");
        }
    }
}
