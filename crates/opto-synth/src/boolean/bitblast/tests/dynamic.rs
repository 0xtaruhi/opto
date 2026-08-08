// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn exhaustively_lowers_bounded_dynamic_extracts() {
    let mut module = word::WordModule::new("dynamic_extract");
    let value_port = add_input(&mut module, "value", 10);
    let offset_port = add_input(&mut module, "offset", 3);
    let value = read_port(&mut module, value_port);
    let offset = read_port(&mut module, offset_port);
    let selected = module
        .dynamic_extract(value, offset, 3, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 3, selected);
    let value_signal = module_signal(&module, value_port);
    let offset_signal = module_signal(&module, offset_port);

    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let extract = plan
        .operators()
        .iter()
        .find(|operator| operator.kind() == crate::OperatorKind::DynamicExtract)
        .unwrap();
    assert_eq!(plan.candidates(extract.id()).len(), 1);
    assert_eq!(
        plan.candidate_recipe_name(plan.candidates(extract.id())[0].id()),
        Some("mux-barrel")
    );

    bitblast_area(&mut module).unwrap();

    for input in 0..1024 {
        for offset in 0..8 {
            assert_eq!(
                evaluate_output(
                    &module,
                    output,
                    &[(value_signal, input), (offset_signal, offset)]
                ),
                (input >> offset) & 0b111,
                "input={input}, offset={offset}"
            );
        }
    }
}

#[test]
fn zero_fills_out_of_range_dynamic_extracts() {
    let mut module = word::WordModule::new("out_of_range_dynamic_extract");
    let value_port = add_input(&mut module, "value", 8);
    let offset_port = add_input(&mut module, "offset", 3);
    let value = read_port(&mut module, value_port);
    let offset = read_port(&mut module, offset_port);
    let selected = module
        .dynamic_extract(value, offset, 2, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 2, selected);
    let value_signal = module_signal(&module, value_port);
    let offset_signal = module_signal(&module, offset_port);

    bitblast_area(&mut module).unwrap();

    for input in 0..256 {
        for offset in 0..8 {
            let expected = if offset <= 6 {
                (input >> offset) & 0b11
            } else {
                0
            };
            assert_eq!(
                evaluate_output(
                    &module,
                    output,
                    &[(value_signal, input), (offset_signal, offset)]
                ),
                expected,
                "input={input}, offset={offset}"
            );
        }
    }
}

#[test]
fn zero_fills_signed_scalar_dynamic_extracts_with_the_exact_type() {
    let mut module = word::WordModule::new("signed_out_of_range_dynamic_extract");
    let value_port = module
        .add_port(
            "value",
            word::PortDirection::Input,
            word::WordType::new(1, true, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset_port = add_input(&mut module, "offset", 1);
    let value = read_port(&mut module, value_port);
    let offset = read_port(&mut module, offset_port);
    let selected = module
        .dynamic_extract(value, offset, 1, word::SourceSpan::default())
        .unwrap();
    let output = module
        .add_port(
            "y",
            word::PortDirection::Output,
            word::WordType::new(1, true, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module_signal(&module, output);
    module
        .connect(
            word::LValue::signal(output),
            selected,
            word::SourceSpan::default(),
        )
        .unwrap();
    let value_signal = module_signal(&module, value_port);
    let offset_signal = module_signal(&module, offset_port);

    bitblast_area(&mut module).unwrap();

    for input in 0..2 {
        assert_eq!(
            evaluate_output(
                &module,
                output,
                &[(value_signal, input), (offset_signal, 0)]
            ),
            input
        );
        assert_eq!(
            evaluate_output(
                &module,
                output,
                &[(value_signal, input), (offset_signal, 1)]
            ),
            0
        );
    }
}

#[test]
fn exhaustively_lowers_scaled_dynamic_extract_offsets() {
    let mut module = word::WordModule::new("scaled_dynamic_extract");
    let value_port = add_input(&mut module, "value", 32);
    let index_port = add_input(&mut module, "index", 2);
    let value = read_port(&mut module, value_port);
    let index = read_port(&mut module, index_port);
    let widened_index = module
        .cast(
            word::CastKind::ZeroExtend,
            index,
            word::WordType::bits(5).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let scale = module
        .constant(
            ConstBits::from_bin_str("01000").unwrap(),
            word::WordType::bits(5).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset = module
        .binary(
            word::BinaryOp::Mul,
            widened_index,
            scale,
            word::SourceSpan::default(),
        )
        .unwrap();
    let selected = module
        .dynamic_extract(value, offset, 8, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 8, selected);
    let value_signal = module_signal(&module, value_port);
    let index_signal = module_signal(&module, index_port);

    bitblast_area(&mut module).unwrap();

    let input = 0x4433_2211;
    for index in 0..4 {
        assert_eq!(
            evaluate_output(
                &module,
                output,
                &[(value_signal, input), (index_signal, index)]
            ),
            (input >> (index * 8)) & 0xff,
            "index={index}"
        );
    }
}

#[test]
fn wide_sparse_scaled_dynamic_extract_uses_one_hot_decode_and_is_exhaustive() {
    let mut module = word::WordModule::new("wide_scaled_dynamic_extract");
    let inputs = (0..4)
        .map(|index| add_input(&mut module, &format!("value{index}"), 64))
        .collect::<Vec<_>>();
    let index_port = add_input(&mut module, "index", 2);
    let parts = inputs
        .iter()
        .rev()
        .map(|&input| read_port(&mut module, input))
        .collect::<Vec<_>>();
    let value = module.concat(parts, word::SourceSpan::default()).unwrap();
    let index = read_port(&mut module, index_port);
    let widened_index = module
        .cast(
            word::CastKind::ZeroExtend,
            index,
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let scale = module
        .constant(
            ConstBits::from_bin_str("01000000").unwrap(),
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset = module
        .binary(
            word::BinaryOp::Mul,
            widened_index,
            scale,
            word::SourceSpan::default(),
        )
        .unwrap();
    let element = module
        .dynamic_extract(value, offset, 64, word::SourceSpan::default())
        .unwrap();
    let selected = module
        .extract(element, 0, 32, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 32, selected);
    let input_signals = inputs
        .iter()
        .map(|&port| module_signal(&module, port))
        .collect::<Vec<_>>();
    let index_signal = module_signal(&module, index_port);

    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let extract = plan
        .operators()
        .iter()
        .find(|operator| operator.kind() == crate::OperatorKind::DynamicExtract)
        .unwrap();
    let recipes = plan
        .candidates(extract.id())
        .iter()
        .map(|candidate| plan.candidate_recipe_name(candidate.id()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(recipes, ["shared-one-hot", "mux-barrel"]);

    let mut barrel_module = module.clone();
    let mut barrel_plan =
        crate::planning::operator::ArchitectureDecisions::for_module(&barrel_module).unwrap();
    let barrel_operator = barrel_plan
        .operators()
        .iter()
        .find(|operator| operator.kind() == crate::OperatorKind::DynamicExtract)
        .unwrap()
        .id();
    let barrel = barrel_plan
        .candidates(barrel_operator)
        .iter()
        .find(|candidate| barrel_plan.candidate_recipe_name(candidate.id()) == Some("mux-barrel"))
        .unwrap()
        .id();
    barrel_plan.select_candidate(barrel).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&barrel_module, &barrel_plan).unwrap();
    bitblast_module_with_plan(&mut barrel_module, &barrel_plan, &mut provenance).unwrap();
    crate::planning::dataflow::optimize_combinational_dataflow(&mut barrel_module).unwrap();
    assert!(
        barrel_module
            .operations()
            .iter()
            .any(|operation| matches!(operation.kind, word::OpKind::Mux { .. })),
        "the explicit barrel candidate must retain its staged mux architecture"
    );

    bitblast_area(&mut module).unwrap();
    crate::planning::dataflow::optimize_combinational_dataflow(&mut module).unwrap();
    assert!(
        module
            .operations()
            .iter()
            .all(|operation| !matches!(operation.kind, word::OpKind::Mux { .. })),
        "sparse aligned extraction should share a one-hot decode instead of building a mux barrel"
    );

    let values = [
        0x1111_1111_aaaa_0000,
        0x2222_2222_bbbb_0001,
        0x3333_3333_cccc_0002,
        0x4444_4444_dddd_0003,
    ];
    for index in 0..4 {
        let mut assignments = input_signals
            .iter()
            .copied()
            .zip(values)
            .collect::<Vec<_>>();
        assignments.push((index_signal, index));
        assert_eq!(
            evaluate_output(&module, output, &assignments),
            values[usize::try_from(index).unwrap()] & 0xffff_ffff,
            "index={index}"
        );
    }
}

#[test]
fn one_hot_decode_shares_selector_prefixes() {
    let mut module = word::WordModule::new("shared_one_hot_prefixes");
    let inputs = (0..16)
        .map(|index| add_input(&mut module, &format!("value{index}"), 16))
        .collect::<Vec<_>>();
    let index_port = add_input(&mut module, "index", 4);
    let parts = inputs
        .iter()
        .rev()
        .map(|&input| read_port(&mut module, input))
        .collect();
    let value = module.concat(parts, word::SourceSpan::default()).unwrap();
    let index = read_port(&mut module, index_port);
    let widened_index = module
        .cast(
            word::CastKind::ZeroExtend,
            index,
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let scale = module
        .constant(
            ConstBits::from_bin_str("00010000").unwrap(),
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset = module
        .binary(
            word::BinaryOp::Mul,
            widened_index,
            scale,
            word::SourceSpan::default(),
        )
        .unwrap();
    let selected = module
        .dynamic_extract(value, offset, 4, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 4, selected);
    let input_signals = inputs
        .iter()
        .map(|&port| module_signal(&module, port))
        .collect::<Vec<_>>();
    let index_signal = module_signal(&module, index_port);

    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let extract = plan
        .operators()
        .iter()
        .find(|operator| operator.kind() == crate::OperatorKind::DynamicExtract)
        .unwrap();
    assert_eq!(
        plan.candidate_recipe_name(plan.selected_candidate(extract.id()).unwrap().id()),
        Some("shared-one-hot")
    );
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    bitblast_module_with_plan(&mut module, &plan, &mut provenance).unwrap();

    let bit_and_count = module
        .operations()
        .iter()
        .filter(|operation| {
            matches!(
                operation.kind,
                word::OpKind::Binary {
                    op: word::BinaryOp::BitAnd,
                    ..
                }
            )
        })
        .count();
    // 28 shared decoder-prefix terms, 64 data terms, and the scaled-offset
    // implementation currently account for at most 106 AND operations. A
    // per-tap decoder would require 20 additional ANDs for the same shape.
    assert!(bit_and_count <= 106, "bit-and count={bit_and_count}");

    for index in 0..16u64 {
        let mut assignments = input_signals
            .iter()
            .copied()
            .enumerate()
            .map(|(position, signal)| (signal, 0x1200 + position as u64))
            .collect::<Vec<_>>();
        assignments.push((index_signal, index));
        assert_eq!(
            evaluate_output(&module, output, &assignments),
            index,
            "index={index}"
        );
    }
}

#[test]
fn exhaustively_lowers_dynamic_inserts_with_out_of_range_bits() {
    let mut module = word::WordModule::new("dynamic_insert");
    let value_port = add_input(&mut module, "value", 8);
    let offset_port = add_input(&mut module, "offset", 3);
    let replacement_port = add_input(&mut module, "replacement", 2);
    let value = read_port(&mut module, value_port);
    let offset = read_port(&mut module, offset_port);
    let replacement = read_port(&mut module, replacement_port);
    let updated = module
        .dynamic_insert(value, offset, replacement, word::SourceSpan::default())
        .unwrap();
    let output = add_output(&mut module, "y", 8, updated);
    let value_signal = module_signal(&module, value_port);
    let offset_signal = module_signal(&module, offset_port);
    let replacement_signal = module_signal(&module, replacement_port);

    bitblast_area(&mut module).unwrap();

    for input in 0..256 {
        for offset in 0..8 {
            for replacement in 0..4 {
                let mask = (0b11u64 << offset) & 0xff;
                let expected = (input & !mask) | ((replacement << offset) & 0xff);
                assert_eq!(
                    evaluate_output(
                        &module,
                        output,
                        &[
                            (value_signal, input),
                            (offset_signal, offset),
                            (replacement_signal, replacement),
                        ]
                    ),
                    expected,
                    "input={input}, offset={offset}, replacement={replacement}"
                );
            }
        }
    }
}
