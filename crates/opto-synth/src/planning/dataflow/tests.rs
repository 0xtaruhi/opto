// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_ir::{ConstBits, word::LogicStateKind};

#[test]
fn canonicalizes_word_values_before_bit_lowering() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let inputs = ["a", "b"].map(|name| {
        module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let outputs = ["y", "z", "selected"].map(|name| {
        module
            .add_port(
                name,
                word::PortDirection::Output,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let values = inputs.map(|port| {
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let first = module
        .binary(
            word::BinaryOp::BitAnd,
            values[0],
            values[1],
            word::SourceSpan::default(),
        )
        .unwrap();
    let duplicate = module
        .binary(
            word::BinaryOp::BitAnd,
            values[1],
            values[0],
            word::SourceSpan::default(),
        )
        .unwrap();
    let one = module
        .constant(
            ConstBits::from_bin_str("1").unwrap(),
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let selected = module
        .mux(one, duplicate, values[0], word::SourceSpan::default())
        .unwrap();
    for (port, value) in outputs.into_iter().zip([first, duplicate, selected]) {
        module
            .connect(
                word::LValue::signal(module.port(port).unwrap().signal),
                value,
                word::SourceSpan::default(),
            )
            .unwrap();
    }

    optimize_combinational_dataflow(&mut module).unwrap();
    module.compact_netlist().unwrap();
    module.validate().unwrap();

    assert_eq!(module.operations().len(), 1);
    assert!(
        module
            .connects()
            .windows(2)
            .all(|pair| pair[0].value == pair[1].value)
    );
}

#[test]
fn protects_entire_canonical_equivalence_class() {
    let mut module = word::WordModule::new("guarded");
    let bit = word::WordType::bits(1).unwrap();
    let inputs = ["a", "b"].map(|name| {
        module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let values = inputs.map(|port| {
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let duplicates = [values, [values[1], values[0]]].map(|operands| {
        module
            .binary(
                word::BinaryOp::BitAnd,
                operands[0],
                operands[1],
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    for (index, value) in duplicates.into_iter().enumerate() {
        let output = module
            .add_port(
                format!("y{index}"),
                word::PortDirection::Output,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(module.port(output).unwrap().signal),
                value,
                word::SourceSpan::default(),
            )
            .unwrap();
    }

    let mut protected = vec![false; module.values().len()];
    protected[duplicates[0].index()] = true;
    let changes =
        optimize_combinational_dataflow_by_preserving_classes(&mut module, &protected, |_, _| true)
            .unwrap();
    module.compact_netlist().unwrap();
    module.validate().unwrap();

    assert!(
        changes
            .representatives()
            .iter()
            .enumerate()
            .all(|(index, value)| value.index() == index)
    );
    assert_eq!(module.operations().len(), 2);
    assert_ne!(module.connects()[0].value, module.connects()[1].value);
}

#[test]
fn resolves_exact_static_vector_aliases_before_region_freeze() {
    let mut module = word::WordModule::new("top");
    let ty = word::WordType::new(32, false, LogicStateKind::FourState).unwrap();
    let ports = ["a", "b", "c"].map(|name| {
        module
            .add_port(
                name,
                word::PortDirection::Input,
                ty,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let inputs = ports.map(|port| {
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let shared = module
        .add_wire("shared", ty, word::SourceSpan::default())
        .unwrap();
    let xor = module
        .binary(
            word::BinaryOp::BitXor,
            inputs[0],
            inputs[1],
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(shared),
            xor,
            word::SourceSpan::default(),
        )
        .unwrap();
    let shared_value = module
        .read_signal(shared, word::SourceSpan::default())
        .unwrap();
    let and = module
        .binary(
            word::BinaryOp::BitAnd,
            shared_value,
            inputs[2],
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
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            and,
            word::SourceSpan::default(),
        )
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();
    module.validate().unwrap();

    let and_operation = module
        .operations()
        .iter()
        .find(|operation| {
            matches!(
                operation.kind,
                word::OpKind::Binary {
                    op: word::BinaryOp::BitAnd,
                    ..
                }
            )
        })
        .unwrap();
    assert!(
        crate::word::operation_inputs(&and_operation.kind)
            .into_iter()
            .any(|input| input == xor)
    );
}

#[test]
fn coalesces_complete_static_wire_slices_before_region_freeze() {
    let mut module = word::WordModule::new("top");
    let byte = word::WordType::bits(8).unwrap();
    let wide = word::WordType::bits(32).unwrap();
    let inputs = (0..4)
        .map(|index| {
            let port = module
                .add_port(
                    format!("a{index}"),
                    word::PortDirection::Input,
                    byte,
                    word::SourceSpan::stable(format!("input {index}")),
                )
                .unwrap();
            module
                .read_signal(
                    module.port(port).unwrap().signal,
                    word::SourceSpan::stable(format!("input {index}")),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let aggregate = module
        .add_wire("aggregate", wide, word::SourceSpan::stable("aggregate"))
        .unwrap();
    for (index, &value) in inputs.iter().enumerate() {
        let lsb = u32::try_from(index).unwrap() * 8;
        module
            .connect(
                word::LValue::signal(aggregate).with_range(word::BitRange { msb: lsb + 7, lsb }),
                value,
                word::SourceSpan::stable(format!("slice {index}")),
            )
            .unwrap();
    }

    coalesce_static_wire_drivers(&mut module).unwrap();
    module.validate().unwrap();

    let connects = module
        .connects()
        .iter()
        .filter(|connect| connect.target.signal == aggregate)
        .collect::<Vec<_>>();
    assert_eq!(connects.len(), 1);
    assert!(connects[0].target.range.is_none());
    assert!(matches!(
        module.value(connects[0].value).unwrap().kind,
        word::ValueKind::Operation(operation)
            if matches!(module.operation(operation).unwrap().kind, word::OpKind::Concat { .. })
    ));
}

#[test]
fn leaves_incomplete_static_wire_slices_separate() {
    let mut module = word::WordModule::new("top");
    let byte = word::WordType::bits(8).unwrap();
    let input = module
        .add_port(
            "a",
            word::PortDirection::Input,
            byte,
            word::SourceSpan::stable("input"),
        )
        .unwrap();
    let value = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::stable("input"),
        )
        .unwrap();
    let aggregate = module
        .add_wire(
            "aggregate",
            word::WordType::bits(24).unwrap(),
            word::SourceSpan::stable("aggregate"),
        )
        .unwrap();
    for lsb in [0, 16] {
        module
            .connect(
                word::LValue::signal(aggregate).with_range(word::BitRange { msb: lsb + 7, lsb }),
                value,
                word::SourceSpan::stable(format!("slice {lsb}")),
            )
            .unwrap();
    }

    coalesce_static_wire_drivers(&mut module).unwrap();

    assert_eq!(
        module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == aggregate)
            .count(),
        2
    );
}

#[test]
fn folds_results_proven_constant_by_partial_bit_facts() {
    let mut module = word::WordModule::new("known_bits");
    let ty = word::WordType::new(8, false, LogicStateKind::FourState).unwrap();
    let input = module
        .add_port(
            "a",
            word::PortDirection::Input,
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let zero = module
        .constant(
            ConstBits::from_bin_str("00000000").unwrap(),
            ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let masked = module
        .binary(
            word::BinaryOp::BitAnd,
            input,
            zero,
            word::SourceSpan::default(),
        )
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();

    let word::ValueKind::Constant(bits) = &module.value(masked).unwrap().kind else {
        panic!("known-zero mask result was not folded");
    };
    assert_eq!(bits, &ConstBits::from_bin_str("00000000").unwrap());
}

#[test]
fn preserves_alias_connect_when_its_driver_follows_an_operation_use() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let alias = module
        .add_wire("alias", bit, word::SourceSpan::default())
        .unwrap();
    let alias_value = module
        .read_signal(alias, word::SourceSpan::default())
        .unwrap();
    let inverted = module
        .unary(
            word::UnaryOp::BitNot,
            alias_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .add_port(
            "input",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input_value = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "output",
            word::PortDirection::Output,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(alias),
            input_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            inverted,
            word::SourceSpan::default(),
        )
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();
    module.validate().unwrap();

    assert!(
        module
            .connects()
            .iter()
            .any(|connect| connect.target.signal == alias)
    );
}

#[test]
fn preserves_signal_alias_selected_by_synthesis_directive() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::default();
    let input = module
        .add_port("input", word::PortDirection::Input, bit, source.clone())
        .unwrap();
    let output = module
        .add_port("output", word::PortDirection::Output, bit, source.clone())
        .unwrap();
    let kept = module.add_wire("kept", bit, source.clone()).unwrap();
    module
        .set_synthesis_directive(
            word::AnnotationTarget::Signal(kept),
            word::SynthesisDirectiveKind::KeepSignal,
            true,
            source.clone(),
        )
        .unwrap();
    let input_value = module
        .read_signal(module.port(input).unwrap().signal, source.clone())
        .unwrap();
    module
        .connect(word::LValue::signal(kept), input_value, source.clone())
        .unwrap();
    let kept_value = module.read_signal(kept, source.clone()).unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            kept_value,
            source,
        )
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();
    module.validate().unwrap();

    assert_eq!(module.connects().len(), 2);
    let output_driver = module
        .connects()
        .iter()
        .find(|connect| connect.target.signal == module.port(output).unwrap().signal)
        .unwrap();
    assert!(matches!(
        module.value(output_driver.value).unwrap().kind,
        word::ValueKind::Signal(reference) if reference.signal == kept
    ));
}

#[test]
fn preserves_alias_chain_when_its_resolved_driver_follows_an_operation_use() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let upstream = module
        .add_wire("upstream", bit, word::SourceSpan::default())
        .unwrap();
    let upstream_value = module
        .read_signal(upstream, word::SourceSpan::default())
        .unwrap();
    let alias = module
        .add_wire("alias", bit, word::SourceSpan::default())
        .unwrap();
    let alias_value = module
        .read_signal(alias, word::SourceSpan::default())
        .unwrap();
    let inverted = module
        .unary(
            word::UnaryOp::BitNot,
            alias_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .add_port(
            "input",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input_value = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let output = module
        .add_port(
            "output",
            word::PortDirection::Output,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(upstream),
            input_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(alias),
            upstream_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            inverted,
            word::SourceSpan::default(),
        )
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();
    module.validate().unwrap();

    assert!(
        module
            .connects()
            .iter()
            .any(|connect| connect.target.signal == alias)
    );
}

#[test]
fn priority_rebalancing_assigns_generated_operations_to_the_chain_owner() {
    let mut module = word::WordModule::new("priority_owner");
    let bit = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::stable("priority assignment");
    let inputs = (0..4)
        .map(|index| {
            let port = module
                .add_port(
                    format!("condition_{index}"),
                    word::PortDirection::Input,
                    bit,
                    source.clone(),
                )
                .unwrap();
            module
                .read_signal(module.port(port).unwrap().signal, source.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit, source.clone())
        .unwrap();
    let one = module
        .constant(ConstBits::from_bin_str("1").unwrap(), bit, source.clone())
        .unwrap();
    let conditions = inputs
        .into_iter()
        .map(|input| {
            module
                .binary(word::BinaryOp::BitAnd, input, one, source.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut selected = zero;
    let mut chain_nodes = Vec::new();
    for condition in &conditions {
        selected = module
            .mux(*condition, one, selected, source.clone())
            .unwrap();
        chain_nodes.push(selected);
    }
    let output = module
        .add_port("selected", word::PortDirection::Output, bit, source.clone())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            selected,
            source,
        )
        .unwrap();

    let boundary_owner = crate::RegionRowId::from_index(0).unwrap();
    let chain_owner = crate::RegionRowId::from_index(1).unwrap();
    let mut owners = vec![None; module.operations().len()];
    for (values, owner) in [(&conditions, boundary_owner), (&chain_nodes, chain_owner)] {
        for value in values {
            let word::ValueKind::Operation(operation) = module.value(*value).unwrap().kind else {
                panic!("test value must be operation-backed");
            };
            owners[operation.index()] = Some(owner);
        }
    }
    let original_operations = owners.len();

    optimize_owned_priority_dataflow(&mut module, &mut owners).unwrap();
    assert_eq!(owners.len(), module.operations().len());
    assert!(owners.len() > original_operations);
    assert!(
        owners[original_operations..]
            .iter()
            .all(|owner| *owner == Some(chain_owner))
    );
}
