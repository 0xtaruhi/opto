// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const MULTIPLY_RECIPES: [&str; 2] = ["radix4-wallace", "array-wallace"];
const ARITHMETIC_PRODUCT_RECIPES: [&str; 16] = [
    "serial-radix4-ripple",
    "serial-radix4-brent-kung",
    "balanced-radix4-ripple",
    "balanced-radix4-brent-kung",
    "wallace-radix4-ripple",
    "wallace-radix4-brent-kung",
    "dadda-radix4-ripple",
    "dadda-radix4-brent-kung",
    "serial-array-ripple",
    "serial-array-brent-kung",
    "balanced-array-ripple",
    "balanced-array-brent-kung",
    "wallace-array-ripple",
    "wallace-array-brent-kung",
    "dadda-array-ripple",
    "dadda-array-brent-kung",
];

fn plan_with_recipe(
    module: &word::WordModule,
    recipe: &str,
) -> crate::planning::operator::ArchitectureDecisions {
    let mut plan = crate::planning::operator::ArchitectureDecisions::for_module(module).unwrap();
    let candidate = plan
        .operators()
        .iter()
        .flat_map(|operator| plan.candidates(operator.id()))
        .copied()
        .find(|candidate| plan.candidate_recipe_name(candidate.id()) == Some(recipe))
        .unwrap();
    plan.select_candidate(candidate.id()).unwrap();
    plan
}

#[test]
fn exhaustively_lowers_widened_booth_multipliers() {
    for recipe in MULTIPLY_RECIPES {
        for signed in [false, true] {
            for (left_width, right_width) in [(1, 1), (2, 3), (3, 4), (4, 5), (5, 3)] {
                let (mut module, left, right, output) =
                    widened_multiply_module(left_width, right_width, signed);
                let plan = plan_with_recipe(&module, recipe);
                let operator = plan.operators()[0];
                assert_eq!(operator.kind(), crate::OperatorKind::Multiply);
                assert_eq!(
                    operator.input_types().map(word::WordType::width),
                    [left_width, right_width]
                );

                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                let output_width = left_width + right_width;
                let mask = (1u64 << output_width) - 1;
                for left_value in 0..(1u64 << left_width) {
                    for right_value in 0..(1u64 << right_width) {
                        let expected = multiply_model(
                            left_value,
                            left_width,
                            right_value,
                            right_width,
                            signed,
                        ) & mask;
                        assert_eq!(
                            evaluate_output(
                                &module,
                                output,
                                &[(left, left_value), (right, right_value)]
                            ),
                            expected,
                            "recipe={recipe}, signed={signed}, widths={left_width}x{right_width}, left={left_value}, right={right_value}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn lowers_ibex_sized_signed_multiplier() {
    let (mut module, left, right, output) = widened_multiply_module(17, 17, true);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    assert_eq!(
        plan.operators()[0].input_types().map(word::WordType::width),
        [17, 17]
    );
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();

    let vectors = [0, 1, 2, 0x0ffff, 0x10000, 0x10001, 0x1fffe, 0x1ffff];
    let mask = (1u64 << 34) - 1;
    for left_value in vectors {
        for right_value in vectors {
            let expected = multiply_model(left_value, 17, right_value, 17, true) & mask;
            assert_eq!(
                evaluate_output(&module, output, &[(left, left_value), (right, right_value)]),
                expected,
                "left={}, right={}",
                signed_value(left_value, 17),
                signed_value(right_value, 17)
            );
        }
    }
}

#[test]
fn exhaustively_lowers_fused_multiply_accumulate() {
    for recipe in ARITHMETIC_PRODUCT_RECIPES {
        for signed in [false, true] {
            for (left_width, right_width, addend_width) in
                [(2, 2, 3), (3, 2, 5), (3, 3, 6), (2, 4, 4)]
            {
                let output_width = left_width + right_width;
                let (mut module, left, right, addend, output) =
                    mac_module(left_width, right_width, addend_width, signed);
                let plan = plan_with_recipe(&module, recipe);
                let region = plan
                    .operators()
                    .iter()
                    .find(|operator| operator.kind() == crate::OperatorKind::Sum)
                    .copied()
                    .unwrap();
                assert_eq!(region.product_term_count(), 1);
                assert_eq!(region.term_count(), 2);

                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                let mask = (1u64 << output_width) - 1;
                for left_value in 0..(1u64 << left_width) {
                    for right_value in 0..(1u64 << right_width) {
                        for addend_value in 0..(1u64 << addend_width) {
                            let product = multiply_model(
                                left_value,
                                left_width,
                                right_value,
                                right_width,
                                signed,
                            );
                            let extended_addend = if signed {
                                sign_extend(addend_value, addend_width)
                            } else {
                                addend_value
                            };
                            let expected = product.wrapping_add(extended_addend) & mask;
                            assert_eq!(
                                evaluate_output(
                                    &module,
                                    output,
                                    &[
                                        (left, left_value),
                                        (right, right_value),
                                        (addend, addend_value)
                                    ]
                                ),
                                expected,
                                "recipe={recipe}, signed={signed}, {left_width}x{right_width}+{addend_width}, {left_value}*{right_value}+{addend_value}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_truncated_multiply_accumulate() {
    for recipe in ARITHMETIC_PRODUCT_RECIPES {
        for signed in [false, true] {
            for (left_width, right_width, output_width) in
                [(3, 3, 4), (4, 4, 5), (3, 4, 6), (4, 3, 3), (5, 5, 6)]
            {
                let (mut module, left, right, addend, output) =
                    truncated_mac_module(left_width, right_width, output_width, signed);
                let plan = plan_with_recipe(&module, recipe);
                let region = plan
                    .operators()
                    .iter()
                    .find(|operator| operator.kind() == crate::OperatorKind::Sum)
                    .copied()
                    .unwrap();
                assert_eq!(region.product_term_count(), 1);
                assert_eq!(region.term_count(), 2);

                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                let mask = (1u64 << output_width) - 1;
                for left_value in 0..(1u64 << left_width) {
                    for right_value in 0..(1u64 << right_width) {
                        for addend_value in 0..(1u64 << output_width) {
                            let a = if signed {
                                sign_extend(left_value, left_width)
                            } else {
                                left_value
                            };
                            let b = if signed {
                                sign_extend(right_value, right_width)
                            } else {
                                right_value
                            };
                            let expected = a.wrapping_mul(b).wrapping_add(addend_value) & mask;
                            assert_eq!(
                                evaluate_output(
                                    &module,
                                    output,
                                    &[
                                        (left, left_value),
                                        (right, right_value),
                                        (addend, addend_value)
                                    ]
                                ),
                                expected,
                                "recipe={recipe}, signed={signed}, {left_width}x{right_width}->{output_width}, \
                             {left_value}*{right_value}+{addend_value}"
                            );
                        }
                    }
                }
            }
        }
    }
}

fn truncated_mac_module(
    left_width: u32,
    right_width: u32,
    output_width: u32,
    signed: bool,
) -> (
    word::WordModule,
    word::SignalId,
    word::SignalId,
    word::SignalId,
    word::SignalId,
) {
    let mut module = word::WordModule::new("truncated_mac");
    let state = word::LogicStateKind::FourState;
    let left_ty = word::WordType::new(left_width, signed, state).unwrap();
    let right_ty = word::WordType::new(right_width, signed, state).unwrap();
    let result_ty = word::WordType::new(output_width, signed, state).unwrap();
    let (left_signal, left_value, right_signal, right_value, addend_signal, addend_value) = {
        let mut input = |name: &str, ty: word::WordType| {
            let port = module
                .add_port(
                    name,
                    word::PortDirection::Input,
                    ty,
                    word::SourceSpan::default(),
                )
                .unwrap();
            let signal = module.port(port).unwrap().signal;
            let value = module
                .read_signal(signal, word::SourceSpan::default())
                .unwrap();
            (signal, value)
        };
        let (left_signal, left_value) = input("left", left_ty);
        let (right_signal, right_value) = input("right", right_ty);
        let (addend_signal, addend_value) = input("addend", result_ty);
        (
            left_signal,
            left_value,
            right_signal,
            right_value,
            addend_signal,
            addend_value,
        )
    };
    let cast = if signed {
        word::CastKind::SignExtend
    } else {
        word::CastKind::ZeroExtend
    };
    let resize = |module: &mut word::WordModule, value: word::ValueId, width: u32| {
        let ty = word::WordType::new(width, signed, state).unwrap();
        let value_width = module.value(value).unwrap().ty.width();
        let kind = if width < value_width {
            word::CastKind::Truncate
        } else {
            cast
        };
        module
            .cast(kind, value, ty, word::SourceSpan::default())
            .unwrap()
    };
    let left_wide = resize(&mut module, left_value, output_width);
    let right_wide = resize(&mut module, right_value, output_width);
    let product = module
        .binary(
            word::BinaryOp::Mul,
            left_wide,
            right_wide,
            word::SourceSpan::default(),
        )
        .unwrap();
    let sum = module
        .binary(
            word::BinaryOp::Add,
            product,
            addend_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    let out_port = module
        .add_port(
            "out",
            word::PortDirection::Output,
            result_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let out_signal = module.port(out_port).unwrap().signal;
    module
        .connect(
            word::LValue::signal(out_signal),
            sum,
            word::SourceSpan::default(),
        )
        .unwrap();
    (module, left_signal, right_signal, addend_signal, out_signal)
}

fn sign_extend(value: u64, width: u32) -> u64 {
    if width == 0 || width >= 64 {
        return value;
    }
    let sign = 1u64 << (width - 1);
    if value & sign != 0 {
        value | !((1u64 << width) - 1)
    } else {
        value
    }
}

fn mac_module(
    left_width: u32,
    right_width: u32,
    addend_width: u32,
    signed: bool,
) -> (
    word::WordModule,
    word::SignalId,
    word::SignalId,
    word::SignalId,
    word::SignalId,
) {
    let mut module = word::WordModule::new("mac");
    let state = word::LogicStateKind::FourState;
    let result_width = left_width + right_width;
    let left_ty = word::WordType::new(left_width, signed, state).unwrap();
    let right_ty = word::WordType::new(right_width, signed, state).unwrap();
    let addend_ty = word::WordType::new(addend_width, signed, state).unwrap();
    let result_ty = word::WordType::new(result_width, signed, state).unwrap();
    let (left_signal, left_value, right_signal, right_value, addend_signal, addend_value) = {
        let mut input = |name: &str, ty: word::WordType| {
            let port = module
                .add_port(
                    name,
                    word::PortDirection::Input,
                    ty,
                    word::SourceSpan::default(),
                )
                .unwrap();
            let signal = module.port(port).unwrap().signal;
            let value = module
                .read_signal(signal, word::SourceSpan::default())
                .unwrap();
            (signal, value)
        };
        let (left_signal, left_value) = input("left", left_ty);
        let (right_signal, right_value) = input("right", right_ty);
        let (addend_signal, addend_value) = input("addend", addend_ty);
        (
            left_signal,
            left_value,
            right_signal,
            right_value,
            addend_signal,
            addend_value,
        )
    };
    let cast = if signed {
        word::CastKind::SignExtend
    } else {
        word::CastKind::ZeroExtend
    };
    let left_wide = module
        .cast(cast, left_value, result_ty, word::SourceSpan::default())
        .unwrap();
    let right_wide = module
        .cast(cast, right_value, result_ty, word::SourceSpan::default())
        .unwrap();
    let addend_wide = module
        .cast(cast, addend_value, result_ty, word::SourceSpan::default())
        .unwrap();
    let product = module
        .binary(
            word::BinaryOp::Mul,
            left_wide,
            right_wide,
            word::SourceSpan::default(),
        )
        .unwrap();
    let sum = module
        .binary(
            word::BinaryOp::Add,
            product,
            addend_wide,
            word::SourceSpan::default(),
        )
        .unwrap();
    let out_port = module
        .add_port(
            "out",
            word::PortDirection::Output,
            result_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let out_signal = module.port(out_port).unwrap().signal;
    module
        .connect(
            word::LValue::signal(out_signal),
            sum,
            word::SourceSpan::default(),
        )
        .unwrap();
    (module, left_signal, right_signal, addend_signal, out_signal)
}

fn widened_multiply_module(
    left_width: u32,
    right_width: u32,
    signed: bool,
) -> (
    word::WordModule,
    word::SignalId,
    word::SignalId,
    word::SignalId,
) {
    let mut module = word::WordModule::new("widened_multiply");
    let state = word::LogicStateKind::FourState;
    let left_ty = word::WordType::new(left_width, signed, state).unwrap();
    let right_ty = word::WordType::new(right_width, signed, state).unwrap();
    let result_ty = word::WordType::new(left_width + right_width, signed, state).unwrap();
    let left_port = module
        .add_port(
            "left",
            word::PortDirection::Input,
            left_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let right_port = module
        .add_port(
            "right",
            word::PortDirection::Input,
            right_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let left = module
        .read_signal(
            module.port(left_port).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let right = module
        .read_signal(
            module.port(right_port).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let cast = if signed {
        word::CastKind::SignExtend
    } else {
        word::CastKind::ZeroExtend
    };
    let left = module
        .cast(cast, left, result_ty, word::SourceSpan::default())
        .unwrap();
    let right = module
        .cast(cast, right, result_ty, word::SourceSpan::default())
        .unwrap();
    let product = module
        .binary(
            word::BinaryOp::Mul,
            left,
            right,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "product",
            word::PortDirection::Output,
            result_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let left_signal = module_signal(&module, left_port);
    let right_signal = module_signal(&module, right_port);
    let output = module_signal(&module, output);
    module
        .connect(
            word::LValue::signal(output),
            product,
            word::SourceSpan::default(),
        )
        .unwrap();
    (module, left_signal, right_signal, output)
}

fn multiply_model(left: u64, left_width: u32, right: u64, right_width: u32, signed: bool) -> u64 {
    if signed {
        let product = i128::from(signed_value(left, left_width))
            * i128::from(signed_value(right, right_width));
        u64::try_from(product.cast_unsigned() & u128::from(u64::MAX)).unwrap()
    } else {
        left * right
    }
}
