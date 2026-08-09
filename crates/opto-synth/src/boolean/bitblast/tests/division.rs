// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn exhaustively_lowers_four_bit_variable_division_and_modulo() {
    for signed in [false, true] {
        for op in [word::BinaryOp::Div, word::BinaryOp::Mod] {
            let (mut module, a, b, y) = binary_module(op, 4, 4, signed);
            bitblast_area(&mut module).unwrap();
            for left in 0..16 {
                for right in 0..16 {
                    let expected = expected(op, left, right, 4, signed);
                    assert_eq!(
                        evaluate_output(&module, y, &[(a, left), (b, right)]),
                        expected,
                        "signed={signed}, op={op:?}, left={left}, right={right}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_six_bit_constant_divisors() {
    for signed in [false, true] {
        for op in [word::BinaryOp::Div, word::BinaryOp::Mod] {
            for divisor in 0..64 {
                let (mut module, input, output) =
                    constant_operand_module(op, 6, divisor, false, signed);
                bitblast_area(&mut module).unwrap();
                for dividend in 0..64 {
                    assert_eq!(
                        evaluate_output(&module, output, &[(input, dividend)]),
                        expected(op, dividend, divisor, 6, signed),
                        "signed={signed}, op={op:?}, dividend={dividend}, divisor={divisor}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_six_bit_constant_dividends() {
    for signed in [false, true] {
        for op in [word::BinaryOp::Div, word::BinaryOp::Mod] {
            for dividend in 0..64 {
                let (mut module, input, output) =
                    constant_operand_module(op, 6, dividend, true, signed);
                bitblast_area(&mut module).unwrap();
                for divisor in 0..64 {
                    assert_eq!(
                        evaluate_output(&module, output, &[(input, divisor)]),
                        expected(op, dividend, divisor, 6, signed),
                        "signed={signed}, op={op:?}, dividend={dividend}, divisor={divisor}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_magic_multiply_high_division() {
    for op in [word::BinaryOp::Div, word::BinaryOp::Mod] {
        let (mut module, input, output) = constant_operand_module(op, 8, 33, false, false);
        bitblast_area(&mut module).unwrap();
        for dividend in 0..=u8::MAX {
            assert_eq!(
                evaluate_output(&module, output, &[(input, u64::from(dividend))]),
                expected(op, u64::from(dividend), 33, 8, false),
                "op={op:?}, dividend={dividend}"
            );
        }
    }
}

fn constant_operand_module(
    op: word::BinaryOp,
    width: u32,
    constant: u64,
    constant_left: bool,
    signed: bool,
) -> (word::WordModule, word::SignalId, word::SignalId) {
    let mut module = word::WordModule::new("constant_division");
    let ty = word::WordType::new(width, signed, word::LogicStateKind::FourState).unwrap();
    let input_port = module
        .add_port(
            "a",
            word::PortDirection::Input,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .read_signal(
            module.port(input_port).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let text = format!("{constant:0width$b}", width = width as usize);
    let constant = module
        .constant(
            ConstBits::from_bin_str(&text).unwrap(),
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
        .binary(op, left, right, word::SourceSpan::default())
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

fn expected(op: word::BinaryOp, left: u64, right: u64, width: u32, signed: bool) -> u64 {
    let mask = (1u64 << width) - 1;
    if right == 0 {
        return 0;
    }
    if signed {
        let left = signed_value(left, width);
        let right = signed_value(right, width);
        if right == 0 {
            return 0;
        }
        let value = match op {
            word::BinaryOp::Div => left / right,
            word::BinaryOp::Mod => left % right,
            _ => unreachable!(),
        };
        value.cast_unsigned() & mask
    } else {
        (match op {
            word::BinaryOp::Div => left / right,
            word::BinaryOp::Mod => left % right,
            _ => unreachable!(),
        }) & mask
    }
}
