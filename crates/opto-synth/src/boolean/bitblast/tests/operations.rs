// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn exhaustively_lowers_vector_bitwise_operations() {
    for op in [
        word::BinaryOp::BitAnd,
        word::BinaryOp::BitOr,
        word::BinaryOp::BitXor,
    ] {
        let (mut module, a, b, y) = binary_module(op, 4, 4, false);
        bitblast_area(&mut module).unwrap();
        for left in 0..16 {
            for right in 0..16 {
                let expected = match op {
                    word::BinaryOp::BitAnd => left & right,
                    word::BinaryOp::BitOr => left | right,
                    word::BinaryOp::BitXor => left ^ right,
                    _ => unreachable!(),
                };
                assert_eq!(
                    evaluate_output(&module, y, &[(a, left), (b, right)]),
                    expected,
                    "op={op:?}, left={left}, right={right}"
                );
            }
        }
    }
}

#[test]
fn exhaustively_lowers_unsigned_comparisons() {
    for op in [
        word::BinaryOp::Lt,
        word::BinaryOp::Le,
        word::BinaryOp::Gt,
        word::BinaryOp::Ge,
        word::BinaryOp::Eq,
        word::BinaryOp::Ne,
    ] {
        let (mut module, a, b, y) = binary_module(op, 4, 4, false);
        bitblast_area(&mut module).unwrap();
        for left in 0..16 {
            for right in 0..16 {
                let expected = match op {
                    word::BinaryOp::Lt => left < right,
                    word::BinaryOp::Le => left <= right,
                    word::BinaryOp::Gt => left > right,
                    word::BinaryOp::Ge => left >= right,
                    word::BinaryOp::Eq => left == right,
                    word::BinaryOp::Ne => left != right,
                    _ => unreachable!(),
                };
                assert_eq!(
                    evaluate_output(&module, y, &[(a, left), (b, right)]),
                    u64::from(expected),
                    "op={op:?}, left={left}, right={right}"
                );
            }
        }
    }
}

#[test]
fn lowers_wide_ordering_without_a_parallel_equality_chain() {
    const WIDTH: u32 = 34;
    for op in [
        word::BinaryOp::Lt,
        word::BinaryOp::Le,
        word::BinaryOp::Gt,
        word::BinaryOp::Ge,
    ] {
        let (mut module, _, _, _) = binary_module(op, WIDTH, WIDTH, false);

        bitblast_area(&mut module).unwrap();

        assert!(
            module.operations().len() <= 3 * WIDTH as usize + 1,
            "{op:?} unexpectedly rebuilt a separate equality prefix"
        );
    }
}

#[test]
fn exhaustively_lowers_signed_comparisons() {
    for op in [
        word::BinaryOp::Lt,
        word::BinaryOp::Le,
        word::BinaryOp::Gt,
        word::BinaryOp::Ge,
    ] {
        let (mut module, a, b, y) = binary_module(op, 4, 4, true);
        bitblast_area(&mut module).unwrap();
        for left in 0..16 {
            for right in 0..16 {
                let signed_left = signed_value(left, 4);
                let signed_right = signed_value(right, 4);
                let expected = match op {
                    word::BinaryOp::Lt => signed_left < signed_right,
                    word::BinaryOp::Le => signed_left <= signed_right,
                    word::BinaryOp::Gt => signed_left > signed_right,
                    word::BinaryOp::Ge => signed_left >= signed_right,
                    _ => unreachable!(),
                };
                assert_eq!(
                    evaluate_output(&module, y, &[(a, left), (b, right)]),
                    u64::from(expected),
                    "op={op:?}, left={signed_left}, right={signed_right}"
                );
            }
        }
    }
}

#[test]
fn exhaustively_lowers_variable_logical_shifts() {
    for op in [word::BinaryOp::Shl, word::BinaryOp::Shr] {
        let (mut module, value, amount, y) = binary_module(op, 4, 4, false);
        bitblast_area(&mut module).unwrap();
        for input in 0..16 {
            for shift in 0..16 {
                let expected = match op {
                    word::BinaryOp::Shl => (input << shift) & 0xf,
                    word::BinaryOp::Shr => input >> shift,
                    _ => unreachable!(),
                };
                assert_eq!(
                    evaluate_output(&module, y, &[(value, input), (amount, shift)]),
                    expected,
                    "op={op:?}, input={input}, shift={shift}"
                );
            }
        }
    }
}

#[test]
fn signed_scalar_left_shift_uses_a_typed_zero_fill() {
    let (mut module, value, amount, output) = binary_module(word::BinaryOp::Shl, 1, 1, true);

    bitblast_area(&mut module).unwrap();

    for input in 0..2 {
        for shift in 0..2 {
            assert_eq!(
                evaluate_output(&module, output, &[(value, input), (amount, shift)]),
                if shift == 0 { input } else { 0 },
                "input={input}, shift={shift}"
            );
        }
    }
}

#[test]
fn sign_extends_signed_scalars_into_unsigned_vectors() {
    let mut module = word::WordModule::new("signed_scalar_extension");
    let input = module
        .add_port(
            "a",
            word::PortDirection::Input,
            word::WordType::new(1, true, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let value = read_port(&mut module, input);
    let extended = module
        .cast(
            word::CastKind::SignExtend,
            value,
            word::WordType::bits(4).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = add_output(&mut module, "y", 4, extended);
    let input = module_signal(&module, input);

    bitblast_area(&mut module).unwrap();

    assert_eq!(evaluate_output(&module, output, &[(input, 0)]), 0);
    assert_eq!(evaluate_output(&module, output, &[(input, 1)]), 0b1111);
}

#[test]
fn exhaustively_lowers_variable_arithmetic_right_shifts() {
    let (mut module, value, amount, y) = binary_module(word::BinaryOp::Ashr, 4, 4, true);
    bitblast_area(&mut module).unwrap();
    for input in 0..16 {
        for shift in 0..16 {
            let expected = (signed_value(input, 4) >> shift).cast_unsigned() & 0xf;
            assert_eq!(
                evaluate_output(&module, y, &[(value, input), (amount, shift)]),
                expected,
                "signed input={}, shift={shift}",
                signed_value(input, 4)
            );
        }
    }

    let (mut module, value, amount, y) = binary_module(word::BinaryOp::Ashr, 4, 4, false);
    bitblast_area(&mut module).unwrap();
    for input in 0..16 {
        for shift in 0..16 {
            assert_eq!(
                evaluate_output(&module, y, &[(value, input), (amount, shift)]),
                input >> shift,
                "unsigned input={input}, shift={shift}"
            );
        }
    }
}

#[test]
fn logical_right_shift_of_signed_values_zero_fills() {
    let (mut module, value, amount, y) = binary_module(word::BinaryOp::Shr, 4, 4, true);
    bitblast_area(&mut module).unwrap();
    for input in 0..16 {
        for shift in 0..16 {
            assert_eq!(
                evaluate_output(&module, y, &[(value, input), (amount, shift)]),
                input >> shift,
                "input={input}, shift={shift}"
            );
        }
    }
}

#[test]
fn preserves_bit_order_through_concat_extract_cast_and_mux() {
    let mut module = word::WordModule::new("top");
    let a = add_input(&mut module, "a", 2);
    let b = add_input(&mut module, "b", 2);
    let select = add_input(&mut module, "select", 1);
    let a_value = read_port(&mut module, a);
    let b_value = read_port(&mut module, b);
    let select_value = read_port(&mut module, select);
    let ab = module
        .concat(vec![a_value, b_value], word::SourceSpan::default())
        .unwrap();
    let middle = module
        .extract(ab, 1, 2, word::SourceSpan::default())
        .unwrap();
    let zero_extended = module
        .cast(
            word::CastKind::ZeroExtend,
            middle,
            word::WordType::bits(4).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let ba = module
        .concat(vec![b_value, a_value], word::SourceSpan::default())
        .unwrap();
    let result = module
        .mux(select_value, zero_extended, ba, word::SourceSpan::default())
        .unwrap();
    let y = add_output(&mut module, "y", 4, result);

    bitblast_area(&mut module).unwrap();
    for a_bits in 0..4 {
        for b_bits in 0..4 {
            let selected = ((a_bits & 1) << 1) | ((b_bits >> 1) & 1);
            let unselected = (b_bits << 2) | a_bits;
            for select_bit in 0..2 {
                assert_eq!(
                    evaluate_output(
                        &module,
                        y,
                        &[
                            (module_signal(&module, a), a_bits,),
                            (module_signal(&module, b), b_bits,),
                            (module_signal(&module, select), select_bit,)
                        ]
                    ),
                    if select_bit == 1 {
                        selected
                    } else {
                        unselected
                    }
                );
            }
        }
    }
}
