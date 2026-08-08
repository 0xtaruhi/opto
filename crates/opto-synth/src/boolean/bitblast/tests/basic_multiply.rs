// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn exhaustively_lowers_four_bit_multiply() {
    let (mut module, a, b, y) = binary_module(word::BinaryOp::Mul, 4, 4, false);
    bitblast_area(&mut module).unwrap();
    assert_eq!(module.signal(y).unwrap().ty.width(), 4);
    for left in 0..16 {
        for right in 0..16 {
            assert_eq!(
                evaluate_output(&module, y, &[(a, left), (b, right)]),
                (left * right) & 0xf
            );
        }
    }
}

#[test]
fn exhaustively_lowers_signed_four_bit_multiply() {
    let (mut module, a, b, y) = binary_module(word::BinaryOp::Mul, 4, 4, true);
    bitblast_area(&mut module).unwrap();
    for left in 0..16 {
        for right in 0..16 {
            let expected = signed_value(left, 4) * signed_value(right, 4);
            assert_eq!(
                evaluate_output(&module, y, &[(a, left), (b, right)]),
                expected.cast_unsigned() & 0xf,
                "left={}, right={}",
                signed_value(left, 4),
                signed_value(right, 4)
            );
        }
    }
}

#[test]
fn exhaustively_lowers_signed_and_unsigned_constant_multipliers() {
    for signed in [false, true] {
        for constant_left in [false, true] {
            for constant in 0..64 {
                let (mut module, input, output) =
                    constant_multiply_module(constant, constant_left, signed);
                let plan =
                    crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
                if constant == 0 {
                    assert!(plan.operators().is_empty());
                } else {
                    let operator = plan.operators()[0].id();
                    assert_eq!(
                        plan.candidate_recipe_name(plan.candidates(operator)[0].id()),
                        Some("constant-csd-wallace")
                    );
                }
                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                for value in 0..64 {
                    let expected = if signed {
                        (signed_value(value, 6) * signed_value(constant, 6)).cast_unsigned()
                    } else {
                        value * constant
                    } & 0x3f;
                    assert_eq!(
                        evaluate_output(&module, output, &[(input, value)]),
                        expected,
                        "signed={signed}, constant_left={constant_left}, constant={constant}, value={value}"
                    );
                }
            }
        }
    }
}

fn constant_multiply_module(
    constant: u64,
    constant_left: bool,
    signed: bool,
) -> (word::WordModule, word::SignalId, word::SignalId) {
    let mut module = word::WordModule::new("constant_multiply");
    let ty = word::WordType::new(6, signed, word::LogicStateKind::FourState).unwrap();
    let input_port = module
        .add_port(
            "a",
            word::PortDirection::Input,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = read_port(&mut module, input_port);
    let constant = module
        .constant(
            ConstBits::from_bin_str(&format!("{constant:06b}")).unwrap(),
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let (left, right) = if constant_left {
        (constant, input)
    } else {
        (input, constant)
    };
    let result = module
        .binary(
            word::BinaryOp::Mul,
            left,
            right,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output_port = module
        .add_port(
            "y",
            word::PortDirection::Output,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module_signal(&module, output_port);
    module
        .connect(
            word::LValue::signal(output),
            result,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module_signal(&module, input_port);
    (module, input, output)
}
