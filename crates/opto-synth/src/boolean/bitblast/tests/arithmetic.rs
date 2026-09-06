// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const ARITHMETIC_REGION_RECIPES: [&str; 12] = [
    "serial-csa-ripple",
    "serial-csa-brent-kung",
    "balanced-csa-ripple",
    "balanced-csa-brent-kung",
    "wallace-csa-ripple",
    "wallace-csa-brent-kung",
    "dadda-csa-ripple",
    "dadda-csa-brent-kung",
    "serial-csa-kogge-stone",
    "balanced-csa-kogge-stone",
    "wallace-csa-kogge-stone",
    "dadda-csa-kogge-stone",
];
const PRODUCT_REGION_RECIPES: [&str; 16] = [
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

#[test]
fn exhaustively_lowers_four_bit_add_and_subtract() {
    for recipe in [
        "ripple-carry",
        "brent-kung",
        "kogge-stone",
        "hybrid-brent-kung-balanced",
    ] {
        for (op, expected) in [
            (word::BinaryOp::Add, wrapping_add as fn(u64, u64) -> u64),
            (word::BinaryOp::Sub, wrapping_sub as fn(u64, u64) -> u64),
        ] {
            let (mut module, a, b, y) = binary_module(op, 4, 4, false);
            let mut plan =
                crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
            select_recipe(&mut plan, recipe);
            let mut provenance =
                crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
            bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
            assert_eq!(module.connects().len(), 4);
            for left in 0..16 {
                for right in 0..16 {
                    assert_eq!(
                        evaluate_output(&module, y, &[(a, left), (b, right)]),
                        expected(left, right) & 0xf,
                        "recipe={recipe}, op={op:?}, left={left}, right={right}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_nested_add_sub_regions_with_carry_save() {
    for recipe in ARITHMETIC_REGION_RECIPES {
        let mut module = word::WordModule::new("additive_region");
        let ports = ["a", "b", "c", "d"].map(|name| add_input(&mut module, name, 4));
        let values = ports.map(|port| read_port(&mut module, port));
        let difference = module
            .binary(
                word::BinaryOp::Sub,
                values[1],
                values[2],
                word::SourceSpan::default(),
            )
            .unwrap();
        let difference = module
            .binary(
                word::BinaryOp::Sub,
                values[0],
                difference,
                word::SourceSpan::default(),
            )
            .unwrap();
        let sum = module
            .binary(
                word::BinaryOp::Add,
                difference,
                values[3],
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = add_output(&mut module, "y", 4, sum);
        let mut plan =
            crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
        assert_eq!(plan.operators().len(), 1);
        let operator = plan.operators()[0];
        assert_eq!(operator.kind(), crate::OperatorKind::Sum);
        assert_eq!(operator.term_count(), 4);
        assert_eq!(plan.source_operations(operator.id()).len(), 3);
        select_recipe(&mut plan, recipe);

        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        let signals = ports.map(|port| module_signal(&module, port));
        for a in 0u64..16 {
            for b in 0u64..16 {
                for c in 0u64..16 {
                    for d in 0u64..16 {
                        let expected = a.wrapping_sub(b.wrapping_sub(c)).wrapping_add(d) & 0xf;
                        assert_eq!(
                            evaluate_output(
                                &module,
                                output,
                                &[
                                    (signals[0], a),
                                    (signals[1], b),
                                    (signals[2], c),
                                    (signals[3], d),
                                ],
                            ),
                            expected,
                            "recipe={recipe}, a={a}, b={b}, c={c}, d={d}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn dadda_reduces_dense_five_term_regions() {
    for recipe in ["dadda-csa-ripple", "dadda-csa-brent-kung"] {
        let mut module = word::WordModule::new("dense_dadda_region");
        let ports = ["a", "b", "c", "d", "e"].map(|name| add_input(&mut module, name, 3));
        let values = ports.map(|port| read_port(&mut module, port));
        let sum = values[1..]
            .iter()
            .try_fold(values[0], |sum, &value| {
                module.binary(word::BinaryOp::Add, sum, value, word::SourceSpan::default())
            })
            .unwrap();
        let output = add_output(&mut module, "y", 3, sum);
        let mut plan =
            crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
        assert_eq!(plan.operators()[0].term_count(), 5);
        select_recipe(&mut plan, recipe);

        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        let signals = ports.map(|port| module_signal(&module, port));
        for packed in 0u32..(1 << 15) {
            let operands =
                std::array::from_fn::<_, 5, _>(|index| u64::from((packed >> (index * 3)) & 7));
            let inputs = std::array::from_fn::<_, 5, _>(|index| (signals[index], operands[index]));
            assert_eq!(
                evaluate_output(&module, output, &inputs),
                operands.into_iter().sum::<u64>() & 7,
                "recipe={recipe}, operands={operands:?}"
            );
        }
    }
}

#[test]
fn arithmetic_regions_absorb_a_single_bit_carry_without_changing_sum_semantics() {
    for recipe in ARITHMETIC_REGION_RECIPES {
        let mut module = word::WordModule::new("carry_input");
        let ports = [
            add_input(&mut module, "a", 4),
            add_input(&mut module, "b", 4),
            add_input(&mut module, "cin", 1),
        ];
        let values = ports.map(|port| read_port(&mut module, port));
        let carry = module
            .cast(
                word::CastKind::ZeroExtend,
                values[2],
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let pair = module
            .binary(
                word::BinaryOp::Add,
                values[0],
                values[1],
                word::SourceSpan::default(),
            )
            .unwrap();
        let sum = module
            .binary(
                word::BinaryOp::Add,
                pair,
                carry,
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = add_output(&mut module, "y", 4, sum);
        let mut plan =
            crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
        assert_eq!(plan.operators().len(), 1);
        select_recipe(&mut plan, recipe);
        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        let signals = ports.map(|port| module_signal(&module, port));
        for a in 0..16 {
            for b in 0..16 {
                for carry in 0..2 {
                    assert_eq!(
                        evaluate_output(
                            &module,
                            output,
                            &[(signals[0], a), (signals[1], b), (signals[2], carry)]
                        ),
                        (a + b + carry) & 15,
                        "recipe={recipe}, a={a}, b={b}, carry={carry}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_late_carry_enters_after_the_prefix_scan() {
    let mut module = word::WordModule::new("late_carry");
    let a = add_input(&mut module, "a", 16);
    let b = add_input(&mut module, "b", 16);
    let cin = add_input(&mut module, "cin", 1);
    let left = read_port(&mut module, a);
    let right = read_port(&mut module, b);
    let mut carry = read_port(&mut module, cin);
    for index in 0..24 {
        let port = add_input(&mut module, &format!("late{index}"), 1);
        let bit = read_port(&mut module, port);
        carry = module
            .binary(
                word::BinaryOp::BitXor,
                carry,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
    }
    let carry = module
        .cast(
            word::CastKind::ZeroExtend,
            carry,
            word::WordType::bits(16).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let pair = module
        .binary(
            word::BinaryOp::Add,
            left,
            right,
            word::SourceSpan::default(),
        )
        .unwrap();
    let sum = module
        .binary(
            word::BinaryOp::Add,
            pair,
            carry,
            word::SourceSpan::default(),
        )
        .unwrap();
    add_output(&mut module, "y", 16, sum);
    let mut plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    select_recipe(&mut plan, "serial-csa-kogge-stone");
    let sources = plan
        .operators()
        .iter()
        .map(|operator| plan.source_operations(operator.id()).into())
        .collect::<Vec<Box<[word::OpId]>>>();
    let operators = crate::DurableOperatorArena::capture(&module, &plan, &sources, |operation| {
        let mut anchor = [0; 32];
        anchor[..4].copy_from_slice(&operation.raw().to_le_bytes());
        Ok(crate::OperationAnchorId::from_bytes_for_test(anchor))
    })
    .unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::for_regional_candidate(&module);
    let lowered = lower_local_region_boolean(
        &mut module,
        LocalRegionBooleanRequest {
            plan: &plan,
            operators: &operators,
            provenance: &mut provenance,
            owner: crate::RegionRowId::from_index(0).unwrap(),
            boundary_inputs: &[],
            roots: &[sum],
            tracked_values: &[sum],
        },
    )
    .unwrap();
    let depth = lowered
        .ownership
        .lowered_bits(sum)
        .unwrap()
        .iter()
        .map(|value| {
            let index = lowered
                .subject
                .value_nodes
                .binary_search_by_key(value, |&(value, _)| value)
                .unwrap();
            lowered
                .subject
                .network
                .level(lowered.subject.value_nodes[index].1)
        })
        .max()
        .unwrap();
    // AND, OR, XOR after the late carry; an early-seeded carry would traverse
    // the full prefix scan and exceed this structural path bound.
    assert!(depth <= 27, "late carry incurred prefix depth: {depth}");
}

#[test]
fn carry_save_regions_preserve_signed_leaf_extension() {
    for recipe in ARITHMETIC_REGION_RECIPES {
        let mut module = word::WordModule::new("signed_additive_region");
        let state = word::LogicStateKind::FourState;
        let types = [
            word::WordType::new(2, true, state).unwrap(),
            word::WordType::new(4, true, state).unwrap(),
            word::WordType::new(4, true, state).unwrap(),
        ];
        let mut ports = Vec::new();
        let mut values = Vec::new();
        for (name, ty) in ["a", "b", "c"].into_iter().zip(types) {
            let port = module
                .add_port(
                    name,
                    word::PortDirection::Input,
                    ty,
                    word::SourceSpan::default(),
                )
                .unwrap();
            values.push(read_port(&mut module, port));
            ports.push(port);
        }
        let first = module
            .binary(
                word::BinaryOp::Add,
                values[0],
                values[1],
                word::SourceSpan::default(),
            )
            .unwrap();
        let sum = module
            .binary(
                word::BinaryOp::Add,
                first,
                values[2],
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = module
            .add_port(
                "y",
                word::PortDirection::Output,
                module.value(sum).unwrap().ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = module_signal(&module, output);
        module
            .connect(
                word::LValue::signal(output),
                sum,
                word::SourceSpan::default(),
            )
            .unwrap();
        let mut plan =
            crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
        assert_eq!(plan.operators()[0].kind(), crate::OperatorKind::Sum);
        select_recipe(&mut plan, recipe);

        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        let signals = ports
            .into_iter()
            .map(|port| module_signal(&module, port))
            .collect::<Vec<_>>();
        for a in 0u64..4 {
            for b in 0u64..16 {
                for c in 0u64..16 {
                    let expected = (signed_value(a, 2) + signed_value(b, 4) + signed_value(c, 4))
                        .cast_unsigned()
                        & 0xf;
                    assert_eq!(
                        evaluate_output(
                            &module,
                            output,
                            &[(signals[0], a), (signals[1], b), (signals[2], c)],
                        ),
                        expected,
                        "recipe={recipe}, a={a}, b={b}, c={c}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_multi_product_arithmetic_regions() {
    for recipe in PRODUCT_REGION_RECIPES {
        for signed in [false, true] {
            for subtract_product in [false, true] {
                let mut module = word::WordModule::new("multi_product_region");
                let ty = word::WordType::new(2, signed, word::LogicStateKind::FourState).unwrap();
                let mut signals = Vec::new();
                let mut values = Vec::new();
                for name in ["a", "b", "c", "d", "e", "f"] {
                    let port = module
                        .add_port(
                            name,
                            word::PortDirection::Input,
                            ty,
                            word::SourceSpan::default(),
                        )
                        .unwrap();
                    signals.push(module_signal(&module, port));
                    values.push(read_port(&mut module, port));
                }
                let first = module
                    .binary(
                        word::BinaryOp::Mul,
                        values[0],
                        values[1],
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let second = module
                    .binary(
                        word::BinaryOp::Mul,
                        values[2],
                        values[3],
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let sum = module
                    .binary(
                        if subtract_product {
                            word::BinaryOp::Sub
                        } else {
                            word::BinaryOp::Add
                        },
                        first,
                        second,
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let sum = module
                    .binary(
                        word::BinaryOp::Add,
                        sum,
                        values[4],
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let sum = module
                    .binary(
                        word::BinaryOp::Sub,
                        sum,
                        values[5],
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let output = module
                    .add_port(
                        "y",
                        word::PortDirection::Output,
                        ty,
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let output = module_signal(&module, output);
                module
                    .connect(
                        word::LValue::signal(output),
                        sum,
                        word::SourceSpan::default(),
                    )
                    .unwrap();
                let mut plan =
                    crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
                assert_eq!(plan.operators().len(), 1);
                let region = plan.operators()[0];
                assert_eq!(region.kind(), crate::OperatorKind::Sum);
                assert_eq!(region.term_count(), 4);
                assert_eq!(region.product_term_count(), 2);
                assert_eq!(plan.source_operations(region.id()).len(), 5);
                assert_eq!(plan.operator_inputs(region).collect::<Vec<_>>(), values);
                select_recipe(&mut plan, recipe);

                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                for packed in 0u32..(1 << 12) {
                    let operands = std::array::from_fn::<_, 6, _>(|index| {
                        u64::from((packed >> (index * 2)) & 3)
                    });
                    let values = operands.map(|value| {
                        if signed {
                            signed_value(value, 2)
                        } else {
                            i64::try_from(value).unwrap()
                        }
                    });
                    let first_product = values[0].wrapping_mul(values[1]);
                    let second_product = values[2].wrapping_mul(values[3]);
                    let products = if subtract_product {
                        first_product.wrapping_sub(second_product)
                    } else {
                        first_product.wrapping_add(second_product)
                    };
                    let expected = products
                        .wrapping_add(values[4])
                        .wrapping_sub(values[5])
                        .cast_unsigned()
                        & 3;
                    let inputs =
                        std::array::from_fn::<_, 6, _>(|index| (signals[index], operands[index]));
                    assert_eq!(
                        evaluate_output(&module, output, &inputs),
                        expected,
                        "recipe={recipe}, signed={signed}, subtract_product={subtract_product}, operands={operands:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn combines_constant_products_before_csd_recoding() {
    for recipe in ARITHMETIC_REGION_RECIPES {
        let mut module = word::WordModule::new("combined_coefficients");
        let ty = word::WordType::bits(5).unwrap();
        let x = module
            .add_port(
                "x",
                word::PortDirection::Input,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let y = module
            .add_port(
                "y",
                word::PortDirection::Input,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let x_value = read_port(&mut module, x);
        let y_value = read_port(&mut module, y);
        let seven = module
            .constant(
                ConstBits::from_bin_str("00111").unwrap(),
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let three = module
            .constant(
                ConstBits::from_bin_str("00011").unwrap(),
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let seven_x = module
            .binary(
                word::BinaryOp::Mul,
                x_value,
                seven,
                word::SourceSpan::default(),
            )
            .unwrap();
        let three_x = module
            .binary(
                word::BinaryOp::Mul,
                three,
                x_value,
                word::SourceSpan::default(),
            )
            .unwrap();
        let difference = module
            .binary(
                word::BinaryOp::Sub,
                seven_x,
                three_x,
                word::SourceSpan::default(),
            )
            .unwrap();
        let sum = module
            .binary(
                word::BinaryOp::Add,
                difference,
                y_value,
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = module
            .add_port(
                "out",
                word::PortDirection::Output,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let output = module_signal(&module, output);
        module
            .connect(
                word::LValue::signal(output),
                sum,
                word::SourceSpan::default(),
            )
            .unwrap();
        let mut plan =
            crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
        assert_eq!(plan.operators().len(), 1);
        assert_eq!(plan.operators()[0].product_term_count(), 2);
        assert_eq!(plan.operators()[0].variable_product_term_count(), 0);
        select_recipe(&mut plan, recipe);
        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        let x = module_signal(&module, x);
        let y = module_signal(&module, y);
        for x_value in 0..32 {
            for y_value in 0..32 {
                assert_eq!(
                    evaluate_output(&module, output, &[(x, x_value), (y, y_value)]),
                    (4 * x_value + y_value) & 31,
                    "recipe={recipe}, x={x_value}, y={y_value}"
                );
            }
        }
    }
}

#[test]
fn lowers_arithmetic_regions_wider_than_native_constant_words() {
    let mut module = word::WordModule::new("wide_arithmetic_region");
    let ty = word::WordType::new(129, true, word::LogicStateKind::FourState).unwrap();
    let x = module
        .add_port(
            "x",
            word::PortDirection::Input,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let y = module
        .add_port(
            "y",
            word::PortDirection::Input,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let x_value = read_port(&mut module, x);
    let y_value = read_port(&mut module, y);
    let minus_one = module
        .constant(
            ConstBits::from_bin_str(&"1".repeat(129)).unwrap(),
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let product = module
        .binary(
            word::BinaryOp::Mul,
            x_value,
            minus_one,
            word::SourceSpan::default(),
        )
        .unwrap();
    let sum = module
        .binary(
            word::BinaryOp::Add,
            product,
            y_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "out",
            word::PortDirection::Output,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module_signal(&module, output);
    module
        .connect(
            word::LValue::signal(output),
            sum,
            word::SourceSpan::default(),
        )
        .unwrap();
    let mut plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    assert_eq!(plan.operators()[0].product_term_count(), 1);
    select_recipe(&mut plan, "dadda-csa-ripple");

    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
    assert_eq!(module.connects().len(), 129);
}

#[test]
fn lowers_only_the_observable_arithmetic_prefix() {
    let mut module = word::WordModule::new("truncated_add");
    let a = add_input(&mut module, "a", 8);
    let b = add_input(&mut module, "b", 8);
    let left = read_port(&mut module, a);
    let right = read_port(&mut module, b);
    let sum = module
        .binary(
            word::BinaryOp::Add,
            left,
            right,
            word::SourceSpan::default(),
        )
        .unwrap();
    let low = module
        .extract(sum, 0, 3, word::SourceSpan::default())
        .unwrap();
    let y = add_output(&mut module, "y", 3, low);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();

    assert_eq!(plan.operators()[0].semantic_width(), 8);
    assert_eq!(plan.operators()[0].width(), 3);

    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
    let a = module_signal(&module, a);
    let b = module_signal(&module, b);
    for left in 0..=u8::MAX {
        for right in 0..=u8::MAX {
            assert_eq!(
                evaluate_output(&module, y, &[(a, u64::from(left)), (b, u64::from(right))]),
                u64::from(left.wrapping_add(right) & 0x7)
            );
        }
    }
}

#[test]
fn omits_demanded_high_bits_proven_zero() {
    let mut module = word::WordModule::new("range_limited_add");
    let a = add_input(&mut module, "a", 4);
    let b = add_input(&mut module, "b", 4);
    let wide = word::WordType::bits(8).unwrap();
    let a_value = read_port(&mut module, a);
    let left = module
        .cast(
            word::CastKind::ZeroExtend,
            a_value,
            wide,
            word::SourceSpan::default(),
        )
        .unwrap();
    let b_value = read_port(&mut module, b);
    let right = module
        .cast(
            word::CastKind::ZeroExtend,
            b_value,
            wide,
            word::SourceSpan::default(),
        )
        .unwrap();
    let sum = module
        .binary(
            word::BinaryOp::Add,
            left,
            right,
            word::SourceSpan::default(),
        )
        .unwrap();
    let y = add_output(&mut module, "y", 8, sum);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();

    assert_eq!(plan.operators()[0].semantic_width(), 8);
    assert_eq!(plan.operators()[0].width(), 5);

    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
    let a = module_signal(&module, a);
    let b = module_signal(&module, b);
    for left in 0..16 {
        for right in 0..16 {
            assert_eq!(
                evaluate_output(&module, y, &[(a, left), (b, right)]),
                left + right
            );
        }
    }
}

#[test]
fn discards_unobservable_arithmetic_without_an_implementation() {
    let mut module = word::WordModule::new("dead_add");
    let a = add_input(&mut module, "a", 8);
    let b = add_input(&mut module, "b", 8);
    let left = read_port(&mut module, a);
    let right = read_port(&mut module, b);
    let _dead_sum = module
        .binary(
            word::BinaryOp::Add,
            left,
            right,
            word::SourceSpan::default(),
        )
        .unwrap();
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();

    assert!(plan.operators().is_empty());

    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
}

#[test]
fn recognizes_and_lowers_increment_and_decrement_resources() {
    for (op, kind, expected) in [
        (
            word::BinaryOp::Add,
            crate::OperatorKind::Increment,
            wrapping_add as fn(u64, u64) -> u64,
        ),
        (
            word::BinaryOp::Sub,
            crate::OperatorKind::Decrement,
            wrapping_sub as fn(u64, u64) -> u64,
        ),
    ] {
        let mut module = word::WordModule::new("step");
        let input = add_input(&mut module, "a", 4);
        let value = read_port(&mut module, input);
        let one = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0001").unwrap(),
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let result = module
            .binary(op, value, one, word::SourceSpan::default())
            .unwrap();
        let output = add_output(&mut module, "y", 4, result);
        let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();

        assert_eq!(plan.operators()[0].kind(), kind);
        assert_eq!(plan.candidates(plan.operators()[0].id()).len(), 1);

        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        let input = module_signal(&module, input);
        for value in 0..16 {
            assert_eq!(
                evaluate_output(&module, output, &[(input, value)]),
                expected(value, 1) & 0xf
            );
        }
    }
}

#[test]
fn exhaustively_lowers_arbitrary_constant_add_and_subtract_operands() {
    for op in [word::BinaryOp::Add, word::BinaryOp::Sub] {
        for constant_left in [false, true] {
            for constant in 0..64 {
                let (mut module, input, output) =
                    constant_add_sub_module(op, constant, constant_left);
                let plan =
                    crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
                let operator = plan.operators()[0].id();
                assert_eq!(
                    plan.candidate_recipe_name(plan.candidates(operator)[0].id()),
                    Some(
                        if constant == 1 && (op == word::BinaryOp::Add || !constant_left) {
                            if op == word::BinaryOp::Add {
                                "increment-ripple"
                            } else {
                                "decrement-ripple"
                            }
                        } else {
                            "constant-ripple"
                        }
                    )
                );
                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                for value in 0..64 {
                    let (left, right) = if constant_left {
                        (constant, value)
                    } else {
                        (value, constant)
                    };
                    let expected = match op {
                        word::BinaryOp::Add => left + right,
                        word::BinaryOp::Sub => left.wrapping_sub(right),
                        _ => unreachable!(),
                    } & 0x3f;
                    assert_eq!(
                        evaluate_output(&module, output, &[(input, value)]),
                        expected,
                        "op={op:?}, constant_left={constant_left}, constant={constant}, value={value}"
                    );
                }
            }
        }
    }
}

#[test]
fn exhaustively_lowers_constant_brent_kung_adders() {
    for op in [word::BinaryOp::Add, word::BinaryOp::Sub] {
        for constant_left in [false, true] {
            for constant in [0, 2, 3, 21, 42, 63] {
                let (mut module, input, output) =
                    constant_add_sub_module(op, constant, constant_left);
                let mut plan =
                    crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
                select_recipe(&mut plan, "constant-brent-kung");
                let mut provenance =
                    crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
                bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
                for value in 0u64..64 {
                    let (left, right) = if constant_left {
                        (constant, value)
                    } else {
                        (value, constant)
                    };
                    let expected = match op {
                        word::BinaryOp::Add => left + right,
                        word::BinaryOp::Sub => left.wrapping_sub(right),
                        _ => unreachable!(),
                    } & 0x3f;
                    assert_eq!(
                        evaluate_output(&module, output, &[(input, value)]),
                        expected
                    );
                }
            }
        }
    }
}

fn constant_add_sub_module(
    op: word::BinaryOp,
    constant: u64,
    constant_left: bool,
) -> (word::WordModule, word::SignalId, word::SignalId) {
    let mut module = word::WordModule::new("constant_add_sub");
    let ty = word::WordType::bits(6).unwrap();
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
        .binary(op, left, right, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 6, result);
    let input = module_signal(&module, input_port);
    (module, input, output)
}

#[test]
fn exhaustively_lowers_area_hybrid_adders() {
    let width = 5;
    let mask = (1u64 << width) - 1;
    for (op, expected) in [
        (word::BinaryOp::Add, wrapping_add as fn(u64, u64) -> u64),
        (word::BinaryOp::Sub, wrapping_sub as fn(u64, u64) -> u64),
    ] {
        let (mut module, a, b, y) = binary_module(op, width, width, false);
        let mut plan =
            crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
        select_recipe(&mut plan, "hybrid-brent-kung-area");
        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
        bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
        for left in 0..=mask {
            for right in 0..=mask {
                assert_eq!(
                    evaluate_output(&module, y, &[(a, left), (b, right)]),
                    expected(left, right) & mask,
                    "op={op:?}, left={left}, right={right}"
                );
            }
        }
    }
}

#[test]
fn exhaustively_lowers_non_power_of_two_brent_kung_adders() {
    for width in [2, 3, 5, 6] {
        let mask = (1u64 << width) - 1;
        for (op, expected) in [
            (word::BinaryOp::Add, wrapping_add as fn(u64, u64) -> u64),
            (word::BinaryOp::Sub, wrapping_sub as fn(u64, u64) -> u64),
        ] {
            let (mut module, a, b, y) = binary_module(op, width, width, false);
            let mut plan =
                crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
            select_recipe(&mut plan, "brent-kung");
            let mut provenance =
                crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
            bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();
            for left in 0..=mask {
                for right in 0..=mask {
                    assert_eq!(
                        evaluate_output(&module, y, &[(a, left), (b, right)]),
                        expected(left, right) & mask,
                        "width={width}, op={op:?}, left={left}, right={right}"
                    );
                }
            }
        }
    }
}
