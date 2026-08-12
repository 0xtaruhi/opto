// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-stage tests owned by frontend normalization and Word-IR publication.

use crate::test_support::*;

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
fn synthesize_rejects_nonblocking_comb_processes() {
    let mut module = module_with_process(false);

    let err = synthesize_test_module(
        &mut module,
        target_options(vec![target_cell("UNUSED", 1.0, &[])]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("nonblocking assignment"));
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
