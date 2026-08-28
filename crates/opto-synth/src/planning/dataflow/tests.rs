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

    resolve_static_connect_aliases(&mut module).unwrap();
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
    assert!(
        module
            .connects()
            .iter()
            .all(|connect| connect.target.signal != shared)
    );
}

#[test]
fn preserves_tri_state_net_reads_as_physical_boundaries() {
    let mut module = word::WordModule::new("tri_state_feedback");
    let source = word::SourceSpan::default();
    let bit = word::WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let data = module
        .add_port("data", word::PortDirection::Input, bit, source.clone())
        .unwrap();
    let enable = module
        .add_port("enable", word::PortDirection::Input, bit, source.clone())
        .unwrap();
    let pad = module
        .add_port("pad", word::PortDirection::Inout, bit, source.clone())
        .unwrap();
    let output = module
        .add_port("observed", word::PortDirection::Output, bit, source.clone())
        .unwrap();
    let data = module
        .read_signal(module.port(data).unwrap().signal, source.clone())
        .unwrap();
    let enable = module
        .read_signal(module.port(enable).unwrap().signal, source.clone())
        .unwrap();
    let pad = module.port(pad).unwrap().signal;
    module
        .set_signal_resolution(pad, word::SignalResolution::TriState)
        .unwrap();
    let driver = module
        .tri_state(
            data,
            word::Enable {
                value: enable,
                active_high: true,
            },
            source.clone(),
        )
        .unwrap();
    module
        .connect(word::LValue::signal(pad), driver, source.clone())
        .unwrap();
    let pad_read = module.read_signal(pad, source.clone()).unwrap();
    let observed = module
        .unary(word::UnaryOp::BitNot, pad_read, source.clone())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            observed,
            source,
        )
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();

    let word::ValueKind::Operation(observed) = module.value(observed).unwrap().kind else {
        panic!("observed value must remain operation-backed");
    };
    assert!(matches!(
        module.operation(observed).unwrap().kind,
        word::OpKind::Unary { arg, .. } if arg == pad_read
    ));
    assert!(
        module
            .connects()
            .iter()
            .any(|connect| connect.target.signal == pad && connect.value == driver)
    );
}

#[test]
fn exact_vector_aliases_reject_reverse_and_multiple_drivers() {
    let mut module = word::WordModule::new("top");
    let wide = word::WordType::bits(4).unwrap();
    let source = word::SourceSpan::default();
    let inputs = ["a", "b"].map(|name| {
        let port = module
            .add_port(name, word::PortDirection::Input, wide, source.clone())
            .unwrap();
        module
            .read_signal(module.port(port).unwrap().signal, source.clone())
            .unwrap()
    });

    let reversed = module.add_wire("reversed", wide, source.clone()).unwrap();
    module
        .connect(
            word::LValue::signal(reversed).with_range(word::BitRange { msb: 0, lsb: 3 }),
            inputs[0],
            source.clone(),
        )
        .unwrap();
    let reversed_read = module.read_signal(reversed, source.clone()).unwrap();

    let multiple = module.add_wire("multiple", wide, source.clone()).unwrap();
    for &input in &inputs {
        module
            .connect(word::LValue::signal(multiple), input, source.clone())
            .unwrap();
    }
    let multiple_read = module.read_signal(multiple, source).unwrap();
    let mixed = module
        .add_wire("mixed", wide, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(mixed),
            inputs[0],
            word::SourceSpan::default(),
        )
        .unwrap();
    let low = module
        .extract(inputs[1], 0, 2, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(mixed).with_range(word::BitRange { msb: 1, lsb: 0 }),
            low,
            word::SourceSpan::default(),
        )
        .unwrap();
    let mixed_read = module
        .read_signal(mixed, word::SourceSpan::default())
        .unwrap();
    for (name, value) in [
        ("reversed_output", reversed_read),
        ("multiple_output", multiple_read),
        ("mixed_output", mixed_read),
    ] {
        let output = module
            .add_port(
                name,
                word::PortDirection::Output,
                wide,
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

    let changes = resolve_static_connect_aliases(&mut module).unwrap();

    assert_eq!(
        changes.representatives()[reversed_read.index()],
        reversed_read
    );
    assert_eq!(
        changes.representatives()[multiple_read.index()],
        multiple_read
    );
    assert_eq!(changes.representatives()[mixed_read.index()], mixed_read);
    assert_eq!(
        module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == reversed)
            .count(),
        1
    );
    assert_eq!(
        module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == multiple)
            .count(),
        2
    );
    assert_eq!(
        module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == mixed)
            .count(),
        2
    );
}

#[test]
fn exact_vector_aliases_preserve_explicit_signal_identity() {
    let mut module = word::WordModule::new("top");
    let wide = word::WordType::bits(4).unwrap();
    let source = word::SourceSpan::default();
    let input = module
        .add_port("a", word::PortDirection::Input, wide, source.clone())
        .unwrap();
    let input = module
        .read_signal(module.port(input).unwrap().signal, source.clone())
        .unwrap();
    let kept = module.add_wire("kept", wide, source.clone()).unwrap();
    module
        .set_synthesis_directive(
            word::AnnotationTarget::Signal(kept),
            word::SynthesisDirectiveKind::KeepSignal,
            true,
            source.clone(),
        )
        .unwrap();
    module
        .connect(word::LValue::signal(kept), input, source.clone())
        .unwrap();
    let kept_read = module.read_signal(kept, source).unwrap();

    let changes = resolve_static_connect_aliases(&mut module).unwrap();

    assert_eq!(changes.representatives()[kept_read.index()], kept_read);
    assert!(
        module
            .connects()
            .iter()
            .any(|connect| connect.target.signal == kept)
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
fn folds_variable_extract_from_a_known_zero_aggregate() {
    let mut module = word::WordModule::new("dynamic_known_bits");
    let data_ty = word::WordType::new(64, false, LogicStateKind::FourState).unwrap();
    let offset_ty = word::WordType::new(6, false, LogicStateKind::FourState).unwrap();
    let offset = module
        .add_port(
            "offset",
            word::PortDirection::Input,
            offset_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset = module
        .read_signal(
            module.port(offset).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let zero = module
        .constant(
            ConstBits::from_bin_str(&"0".repeat(64)).unwrap(),
            data_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let extracted = module
        .dynamic_extract(zero, offset, 1, word::SourceSpan::default())
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();

    assert!(matches!(
        module.value(extracted).map(|stored| &stored.kind),
        Some(word::ValueKind::Constant(bits)) if bits == &ConstBits::from_bin_str("0").unwrap()
    ));
}

#[test]
fn folds_variable_extract_with_a_known_invalid_offset() {
    let mut module = word::WordModule::new("dynamic_invalid_offset");
    let data_ty = word::WordType::new(64, false, LogicStateKind::FourState).unwrap();
    let offset_ty = word::WordType::new(7, false, LogicStateKind::FourState).unwrap();
    let data = module
        .constant(
            ConstBits::from_bin_str(&"1".repeat(64)).unwrap(),
            data_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset = module
        .constant(
            ConstBits::from_bin_str("1000000").unwrap(),
            offset_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let extracted = module
        .dynamic_extract(data, offset, 1, word::SourceSpan::default())
        .unwrap();

    optimize_combinational_dataflow(&mut module).unwrap();

    assert!(matches!(
        module.value(extracted).map(|stored| &stored.kind),
        Some(word::ValueKind::Constant(bits)) if bits == &ConstBits::from_bin_str("0").unwrap()
    ));
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
fn priority_rebalancing_repartitions_generated_operations_from_word_ir() {
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
    for condition in &conditions {
        selected = module
            .mux(*condition, one, selected, source.clone())
            .unwrap();
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

    let original_operations = module.operations().len();

    optimize_priority_dataflow_in_regions(&mut module).unwrap();
    assert!(module.operations().len() > original_operations);
    let graph = crate::regional::region_graph::partition::build(
        &module,
        crate::regional::region_graph::RegionPartitionPolicy::with_target_work(1),
    )
    .unwrap();
    let reachable =
        crate::regional::region_graph::partition::synthesis_reachable_operations(&module).unwrap();
    assert!(
        reachable[original_operations..]
            .iter()
            .zip(&graph.operation_owner_rows()[original_operations..])
            .any(|(&reachable, owner)| reachable && owner.is_some())
    );
}

#[test]
fn keeps_a_driver_for_a_wire_read_only_by_an_instance() {
    let source = word::SourceSpan::default();
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let input = module
        .add_port("a", word::PortDirection::Input, bit, source.clone())
        .unwrap();
    let read = module
        .read_signal(module.port(input).unwrap().signal, source.clone())
        .unwrap();
    let driver = module
        .unary(word::UnaryOp::BitNot, read, source.clone())
        .unwrap();
    let wire = module.add_wire("enable", bit, source.clone()).unwrap();
    module
        .connect(word::LValue::signal(wire), driver, source.clone())
        .unwrap();
    let enable = module.read_signal(wire, source.clone()).unwrap();
    module
        .add_instance(
            "gate",
            "ICG",
            vec![("GATE".to_string(), enable, source.clone())],
            source,
        )
        .unwrap();

    let word::ValueKind::Operation(operation) = module.value(driver).unwrap().kind else {
        panic!("test driver must be operation-backed");
    };
    let mut owners = vec![None; module.operations().len()];
    owners[operation.index()] = Some(crate::RegionRowId::from_index(0).unwrap());
    optimize_region_combinational_dataflow(&mut module, &owners).unwrap();

    // The wire's only reader was the instance, so dropping its connect is
    // legitimate only if the instance was substituted onto the driver too.
    for connection in module
        .instances()
        .iter()
        .flat_map(|instance| &instance.connections)
    {
        let word::ValueKind::Signal(reference) = module.value(connection.value).unwrap().kind
        else {
            continue;
        };
        assert!(
            module
                .connects()
                .iter()
                .any(|connect| connect.target.signal == reference.signal),
            "instance connection reads a wire with no driver"
        );
    }
}
