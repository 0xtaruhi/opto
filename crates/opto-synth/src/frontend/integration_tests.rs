// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-stage tests owned by frontend normalization and Word-IR publication.

use crate::test_support::*;

#[test]
fn mapped_vector_addition_drives_every_output_bit() {
    let timed_cell = |name, function: &str, sense| {
        let mut cell = target_cell(
            name,
            1.0,
            &[
                ("A", TargetPinDirection::Input, None),
                ("B", TargetPinDirection::Input, None),
                ("Y", TargetPinDirection::Output, Some(function)),
            ],
        );
        for related_pin in ["A", "B"] {
            cell.pins[2]
                .timing_arcs
                .push(opto_library::TargetTimingArc {
                    related_pin: related_pin.to_string(),
                    timing_type: opto_library::TargetTimingType::Combinational,
                    timing_sense: sense,
                    delay_model: Some(opto_library::ArcDelayModel::Nldm(
                        opto_library::NldmTimingModel::new(
                            Some(opto_library::LookupTable::scalar(0.1)),
                            Some(opto_library::LookupTable::scalar(0.1)),
                            None,
                            None,
                        ),
                    )),
                    rise_constraint: None,
                    fall_constraint: None,
                });
        }
        cell
    };
    let mut module = WordModule::new("top");
    let ty = WordType::bits(2).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, ty, test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, ty, test_span())
        .unwrap();
    let y = module
        .add_port(
            "y",
            PortDirection::Output,
            WordType::bits(4).unwrap(),
            test_span(),
        )
        .unwrap();
    let a = read_port(&mut module, a);
    let b = read_port(&mut module, b);
    let zero = module
        .constant(
            ConstBits::from_bin_str("00").unwrap(),
            WordType::bits(2).unwrap(),
            test_span(),
        )
        .unwrap();
    let a = module.concat(vec![a, zero], test_span()).unwrap();
    let b = module.concat(vec![b, zero], test_span()).unwrap();
    let sum = module.binary(BinaryOp::Add, a, b, test_span()).unwrap();
    connect_port(&mut module, y, sum);
    let options = target_options(vec![
        timed_cell("AND2", "A*B", opto_library::TimingSense::PositiveUnate),
        timed_cell("XOR2", "A^B", opto_library::TimingSense::NonUnate),
    ]);

    let report = synthesize_test_module(&mut module, options).unwrap();

    assert!(report.report.cells > 0);
    let (index, _) = report
        .mapped
        .ports()
        .iter()
        .enumerate()
        .find(|(_, port)| report.mapped.names().resolve(port.name) == Some("y"))
        .expect("mapped output port exists");
    let output = report
        .mapped
        .port_nets(opto_ir::mapped::PortId::from_index(index).unwrap())
        .expect("mapped output port has scalar nets");
    assert_eq!(output.len(), 4);
    for &net in &output[2..] {
        assert!(
            report
                .mapped
                .pins_on_net(net)
                .expect("mapped output net is live")
                .filter_map(|pin| report.mapped.connection(pin))
                .any(|connection| report.mapped.pin_name(connection) == Some("Y")),
            "nonconstant sum bit {net:?} has no mapped driver"
        );
    }
    for &net in &output[..2] {
        assert!(report.mapped.constant_drivers().contains(&(net, false)));
    }
}

fn target_options(cells: Vec<TargetCell>) -> SynthesisOptions {
    SynthesisOptions {
        target_cells: cells.into(),
    }
}

fn mux_target_cell() -> TargetCell {
    target_cell(
        "MUX2",
        1.0,
        &[
            ("S", TargetPinDirection::Input, None),
            ("A", TargetPinDirection::Input, None),
            ("B", TargetPinDirection::Input, None),
            ("Z", TargetPinDirection::Output, Some("(S&A)|(!S&B)")),
        ],
    )
}

#[test]
fn synthesize_lowers_comb_processes_to_structural_connects() {
    let mut module = module_with_process(true);

    let report = synthesize_test_module(
        &mut module,
        target_options(vec![target_cell("UNUSED", 1.0, &[])]),
    )
    .unwrap();

    assert_eq!(report.report.design, "top");
    assert_eq!(module.connects().len(), 1);
    assert!(write_verilog(&module).unwrap().contains("assign y = a;"));
}

#[test]
fn synthesize_lowers_comb_if_else_to_mux_connect() {
    let mut module = module_with_if_process();

    let synthesized =
        synthesize_test_module(&mut module, target_options(vec![mux_target_cell()])).unwrap();

    let text = synthesized.mapped_verilog();
    assert_eq!(synthesized.mapped.cell_count(), 1, "{text}");
    assert!(text.contains("MUX2"), "{text}");
}

#[test]
fn synthesize_accepts_read_independent_nonblocking_comb_processes() {
    let mut module = module_with_process(false);

    synthesize_test_module(
        &mut module,
        target_options(vec![target_cell("UNUSED", 1.0, &[])]),
    )
    .unwrap();
    assert_eq!(module.connects().len(), 1);
    assert!(write_verilog(&module).unwrap().contains("assign y = a;"));
}

#[test]
fn synthesize_rejects_schedule_sensitive_nonblocking_comb_processes() {
    let mut module = module_with_schedule_sensitive_nonblocking_process();

    let err = synthesize_test_module(
        &mut module,
        target_options(vec![target_cell("UNUSED", 1.0, &[])]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("nonblocking assignment to 'y'"));
    assert!(err.to_string().contains("schedule-sensitive"));
    assert!(err.to_string().contains("read during the same activation"));
    assert_eq!(module.procedures.procedures().len(), 1);
    assert!(module.connects().is_empty());
}

#[test]
fn synthesize_lowers_simple_flop_process_to_register_op() {
    let mut module = module_with_flop_process();

    synthesize_test_module(&mut module, target_options(vec![simple_dff_target_cell()])).unwrap();

    assert!(matches!(
        module.operations().last().unwrap().kind,
        word::OpKind::Register(ref register)
            if register.edge == Edge::Pos
                && register.enable.is_none()
                && register.resets.is_empty()
    ));
    let text = write_verilog(&module).unwrap();
    assert!(text.contains("output reg q;"));
    assert!(text.contains("always @(posedge clk) begin"));
    assert!(text.contains("q <= d;"));
}

#[test]
fn frontend_preserves_priority_across_constant_update_joins() {
    let module = module_with_prioritized_constant_updates();
    let module = crate::frontend::lower_to_validated_word(
        module.rtl().unwrap(),
        &ReferencePortMap::new(),
        crate::test_runtime(),
        &mut |_| {},
    )
    .unwrap();

    let text = write_verilog(&module).unwrap();
    assert!(text.contains("if (second) begin"), "{text}");
    assert!(text.contains("q <= 1'b1;"), "{text}");
    assert!(text.contains("if (first) begin"), "{text}");
    assert!(text.contains("q <= 1'b0;"), "{text}");
}

#[test]
fn frontend_canonicalizes_nested_async_clear_set_priority() {
    let module = module_with_nested_async_controls();
    let module = crate::frontend::lower_to_validated_word(
        module.rtl().unwrap(),
        &ReferencePortMap::new(),
        crate::test_runtime(),
        &mut |_| {},
    )
    .unwrap();

    let register = module
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .expect("nested asynchronous controls must lower to one register");
    assert_eq!(register.resets.len(), 2);
    assert!(
        register
            .resets
            .iter()
            .all(|reset| reset.kind == word::ResetKind::Async)
    );
    assert!(matches!(
        module.value(register.resets[0].reset_value).unwrap().kind,
        word::ValueKind::Constant(ref bits)
            if bits.bit_lsb(0) == Some(opto_ir::BitVal::One)
    ));
    assert!(matches!(
        module.value(register.resets[1].reset_value).unwrap().kind,
        word::ValueKind::Constant(ref bits)
            if bits.bit_lsb(0) == Some(opto_ir::BitVal::Zero)
    ));
}

#[test]
fn target_mapping_follows_static_slices_of_continuously_driven_wires() {
    let mut module = WordModule::new("top");
    let vector = WordType::bits(4).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, vector, test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, vector, test_span())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, vector, test_span())
        .unwrap();
    let a_value = read_port(&mut module, a);
    let b_value = read_port(&mut module, b);
    let first = module
        .binary(BinaryOp::BitXor, a_value, b_value, test_span())
        .unwrap();
    let intermediate = module
        .add_wire("intermediate", vector, test_span())
        .unwrap();
    module
        .connect(LValue::signal(intermediate), first, test_span())
        .unwrap();
    let low = module
        .read_signal_slice(intermediate, 0, 3, test_span())
        .unwrap();
    let high = module
        .read_signal_slice(intermediate, 3, 1, test_span())
        .unwrap();
    let rotated = module.concat(vec![low, high], test_span()).unwrap();
    let result = module
        .binary(BinaryOp::BitXor, rotated, a_value, test_span())
        .unwrap();
    connect_port(&mut module, y, result);
    let xor = target_cell(
        "XOR2",
        1.0,
        &[
            ("A", TargetPinDirection::Input, None),
            ("B", TargetPinDirection::Input, None),
            ("Y", TargetPinDirection::Output, Some("A^B")),
        ],
    );

    let synthesized = synthesize_test_module(
        &mut module,
        SynthesisOptions {
            target_cells: vec![xor].into(),
        },
    )
    .unwrap();

    let text = synthesized.mapped_verilog();
    assert!(synthesized.mapped.cell_count() > 0, "{text}");
    assert_eq!(
        text.matches("XOR2 ").count(),
        synthesized.mapped.cell_count(),
        "{text}"
    );
}

#[test]
fn mapped_output_preserves_concatenated_input_bits() {
    let mut module = WordModule::new("top");
    let narrow = WordType::bits(2).unwrap();
    let wide = WordType::bits(4).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, narrow, test_span())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, narrow, test_span())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, wide, test_span())
        .unwrap();
    let a = read_port(&mut module, a);
    let b = read_port(&mut module, b);
    let concatenated = module.concat(vec![a, b], test_span()).unwrap();
    connect_port(&mut module, y, concatenated);

    let synthesized = synthesize_test_module(
        &mut module,
        target_options(vec![target_cell("UNUSED", 1.0, &[])]),
    )
    .unwrap();

    let text = synthesized.mapped_verilog();
    for expected in [
        "assign y[0] = b[0];",
        "assign y[1] = b[1];",
        "assign y[2] = a[0];",
        "assign y[3] = a[1];",
    ] {
        assert!(text.contains(expected), "missing '{expected}':\n{text}");
    }
}

#[test]
fn mapped_output_preserves_a_direct_input_bit() {
    let mut module = WordModule::new("top");
    let input = module
        .add_port("a", PortDirection::Input, bit(), test_span())
        .unwrap();
    let output = module
        .add_port("y", PortDirection::Output, bit(), test_span())
        .unwrap();
    let input = read_port(&mut module, input);
    connect_port(&mut module, output, input);

    let synthesized = synthesize_test_module(
        &mut module,
        target_options(vec![target_cell("UNUSED", 1.0, &[])]),
    )
    .unwrap();

    let text = synthesized.mapped_verilog();
    assert!(text.contains("assign y = a;"), "{text}");
}
