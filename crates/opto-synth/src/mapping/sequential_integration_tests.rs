// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-stage tests owned by sequential target mapping and materialization.

use crate::test_support::*;

fn mux_target_cell(area: f64) -> TargetCell {
    target_cell(
        "MUX2",
        area,
        &[
            ("I0", TargetPinDirection::Input, None),
            ("I1", TargetPinDirection::Input, None),
            ("S", TargetPinDirection::Input, None),
            ("Z", TargetPinDirection::Output, Some("(!S*I0)+(S*I1)")),
        ],
    )
}

#[test]
fn regional_target_mapping_is_deterministic_across_worker_counts() {
    fn synthesize_with_threads(max_threads: usize) -> String {
        let runtime =
            opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads })
                .unwrap();
        let source =
            opto_ir::rtl::RtlModule::structural(module_with_inverted_flop_output()).unwrap();
        let result = synthesize_rtl_module(
            source,
            SynthesisOptions {
                target_cells: vec![
                    simple_dff_target_cell(),
                    target_cell(
                        "INV",
                        1.0,
                        &[
                            ("A", TargetPinDirection::Input, None),
                            ("Z", TargetPinDirection::Output, Some("!A")),
                        ],
                    ),
                ]
                .into(),
            },
            &runtime,
        )
        .unwrap();
        let (_, _, mapped) = result.into_module_and_report();
        let mut output = Vec::new();
        opto_formats::write_mapped_verilog(&mut output, &mapped).unwrap();
        String::from_utf8(output).unwrap()
    }

    let serial = synthesize_with_threads(1);
    assert!(serial.contains("DFD1"), "{serial}");
    assert!(serial.contains("INV"), "{serial}");
    assert_eq!(serial, synthesize_with_threads(4));
}

#[test]
fn synthesize_maps_simple_flop_to_target_dff() {
    let mut module = module_with_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![simple_dff_target_cell()].into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();

    assert_eq!(report.report.cells, 1);
    let text = report.mapped_verilog();
    assert!(text.contains("DFD1 q_reg(.D(d), .CP(clk), .Q(q));"));
    assert!(!text.contains("always @"));
    let mut area = AreaReportContext::default();
    area.library_cell_kind
        .insert("DFD1".to_string(), AreaCellKind::Sequential);
    let qor = report_qor(&module, &area, None).render_plain();
    assert!(qor.contains("Combinational cells: 0"));
    assert!(qor.contains("Sequential cells: 1"));
}

#[test]
fn synthesize_maps_level_sensitive_latch_to_target_cell() {
    let mut module = module_with_latch_process();
    let options = SynthesisOptions {
        target_cells: vec![simple_latch_target_cell()].into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();

    assert_eq!(report.report.cells, 1);
    let text = report.mapped_verilog();
    assert!(text.contains("LHQD1 q_reg(.D(d), .E(en), .Q(q));"));
    assert!(!text.contains("always @"));
}

#[test]
fn synthesize_maps_latch_asynchronous_clear_to_target_cell() {
    let mut module = module_with_reset_latch_process();
    let options = SynthesisOptions {
        target_cells: vec![clear_latch_target_cell()].into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 1);
    assert!(text.contains("LHQD1R q_reg(.D(d), .E(en), .R(reset), .Q(q));"));
}

#[test]
fn synthesize_bitblasts_vector_latch_to_scalar_target_cells() {
    let mut module = WordModule::new("top");
    let enable = module
        .add_port("en", PortDirection::Input, bit(), test_span())
        .unwrap();
    let vector = WordType::bits(4).unwrap();
    let data = module
        .add_port("d", PortDirection::Input, vector, test_span())
        .unwrap();
    let q = module
        .add_port("q", PortDirection::Output, vector, test_span())
        .unwrap();
    let enable = read_port(&mut module, enable);
    let data = read_port(&mut module, data);
    let target = module.port(q).unwrap().signal;
    let mut module = conditional_assignment(
        module,
        ProcedureKind::Latch,
        None,
        enable,
        target,
        data,
        AssignmentMode::Nonblocking,
    );

    let report = synthesize_test_module(
        &mut module,
        SynthesisOptions {
            target_cells: vec![simple_latch_target_cell()].into(),
        },
    )
    .unwrap();

    assert_eq!(report.report.cells, 4);
    let text = report.mapped_verilog();
    assert_eq!(text.matches("LHQD1 ").count(), 4, "{text}");
}

#[test]
fn synthesize_prefers_q_only_dff_without_inversion_demand() {
    let mut module = module_with_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![simple_dff_target_cell(), dual_output_dff_target_cell()].into(),
    };

    let synthesized = synthesize_test_module(&mut module, options).unwrap();
    let text = synthesized.mapped_verilog();

    assert!(text.contains("DFD1 q_reg(.D(d), .CP(clk), .Q(q));"));
    assert!(!text.contains("DFDQN"));
}

#[test]
fn synthesize_rejects_dual_output_dff_more_expensive_than_inverter() {
    let mut module = module_with_inverted_flop_output();
    let mut dual = dual_output_dff_target_cell();
    dual.area = Some(4.0);
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            dual,
            target_cell(
                "INV",
                1.0,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("ZN", TargetPinDirection::Output, Some("!A")),
                ],
            ),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 2);
    assert!(text.contains("DFD1 q_reg(.D(d), .CP(clk), .Q(q));"));
    assert!(text.contains("INV"));
    assert!(!text.contains("DFDQN"));
}

#[test]
fn synthesize_maps_semantic_clock_enable_to_mux_and_dff() {
    let mut module = module_with_enable_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![simple_dff_target_cell(), mux_target_cell(1.0)].into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 2);
    assert!(
        text.contains("MUX2 U1(.I0(q), .I1(d), .S(en), .Z(n1));"),
        "{text}"
    );
    assert!(
        text.contains("DFD1 q_reg(.D(n1), .CP(clk), .Q(q));"),
        "{text}"
    );
    assert!(!text.contains("always @"));
}

#[test]
fn synthesize_maps_semantic_clock_enable_to_an_enable_dff_pin() {
    let mut module = module_with_enable_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            enable_dff_target_cell(),
            mux_target_cell(1.0),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 1, "{text}");
    assert!(
        text.contains("EDFD1 q_reg(.D(d), .DE(en), .CP(clk), .Q(q));"),
        "{text}"
    );
    assert!(!text.contains("MUX2"), "{text}");
}

#[test]
fn synthesize_covers_enable_polarity_in_the_boolean_network() {
    let mut module = module_with_enable_flop_process();
    let mut enabled = enable_dff_target_cell();
    enabled.name = "EDFND1".to_string();
    enabled.sequential[0].next_state = Some(BooleanFunction::parse("(D*!DE)+(IQ*DE)").unwrap());
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            enabled,
            target_cell(
                "INV",
                0.1,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("Z", TargetPinDirection::Output, Some("!A")),
                ],
            ),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 2, "{text}");
    assert!(text.contains("INV"), "{text}");
    assert!(text.contains("EDFND1"), "{text}");
    assert!(text.contains(".DE(n1)"), "{text}");
}

#[test]
fn synthesize_keeps_a_lowered_feedback_mux_as_ordinary_logic() {
    let mut module = module_with_lowered_feedback_mux_flop();
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            enable_dff_target_cell(),
            mux_target_cell(1.0),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 2, "{text}");
    assert!(
        text.contains("MUX2 U1(.I0(q), .I1(d), .S(en), .Z(n1));"),
        "{text}"
    );
    assert!(
        text.contains("DFD1 q_reg(.D(n1), .CP(clk), .Q(q));"),
        "{text}"
    );
    assert!(!text.contains("EDFD1"), "{text}");
}

#[test]
fn synthesize_composes_sync_reset_with_a_retained_enable_pin() {
    let mut module = module_with_sync_reset_enable_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            enable_dff_target_cell(),
            target_cell(
                "OR2",
                0.5,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("B", TargetPinDirection::Input, None),
                    ("Z", TargetPinDirection::Output, Some("A+B")),
                ],
            ),
            target_cell(
                "ANR2",
                0.5,
                &[
                    ("D", TargetPinDirection::Input, None),
                    ("R", TargetPinDirection::Input, None),
                    ("Z", TargetPinDirection::Output, Some("D*!R")),
                ],
            ),
            mux_target_cell(1.0),
            target_cell(
                "INV",
                0.5,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("ZN", TargetPinDirection::Output, Some("!A")),
                ],
            ),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 3, "{text}");
    assert!(
        text.contains("OR2 U1(.A(en), .B(reset), .Z(n2));")
            || text.contains("OR2 U1(.A(reset), .B(en), .Z(n2));"),
        "{text}"
    );
    assert!(
        text.contains("ANR2 U2(.D(d), .R(reset), .Z(n1));"),
        "{text}"
    );
    assert!(
        text.contains("EDFD1 q_reg(.D(n1), .DE(n2), .CP(clk), .Q(q));"),
        "{text}"
    );
    assert!(!text.contains("MUX2"), "{text}");
}

#[test]
fn synthesize_maps_semantic_sync_reset_through_logic_planner() {
    let mut module = module_with_sync_reset_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            target_cell(
                "ANR2",
                0.5,
                &[
                    ("D", TargetPinDirection::Input, None),
                    ("R", TargetPinDirection::Input, None),
                    ("Z", TargetPinDirection::Output, Some("D*!R")),
                ],
            ),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 2);
    assert!(
        text.contains("ANR2 U1(.D(d), .R(reset), .Z(n1));"),
        "{text}"
    );
    assert!(
        text.contains("DFD1 q_reg(.D(n1), .CP(clk), .Q(q));"),
        "{text}"
    );
}

#[test]
fn synthesize_assigns_input_phases_across_register_control_logic() {
    let mut module = module_with_sync_reset_enable_flop_process();
    let options = SynthesisOptions {
        target_cells: vec![
            simple_dff_target_cell(),
            mux_target_cell(2.0),
            target_cell(
                "MUX2N",
                1.0,
                &[
                    ("I0", TargetPinDirection::Input, None),
                    ("I1", TargetPinDirection::Input, None),
                    ("S", TargetPinDirection::Input, None),
                    ("ZN", TargetPinDirection::Output, Some("!((!S*I0)+(S*I1))")),
                ],
            ),
            target_cell(
                "NR2",
                0.5,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("B", TargetPinDirection::Input, None),
                    ("ZN", TargetPinDirection::Output, Some("!(A+B)")),
                ],
            ),
            target_cell(
                "INR2",
                2.0,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("B", TargetPinDirection::Input, None),
                    ("ZN", TargetPinDirection::Output, Some("A*!B")),
                ],
            ),
            target_cell(
                "INV",
                1.0,
                &[
                    ("A", TargetPinDirection::Input, None),
                    ("ZN", TargetPinDirection::Output, Some("!A")),
                ],
            ),
        ]
        .into(),
    };

    let report = synthesize_test_module(&mut module, options).unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 3);
    assert_eq!(text.matches("  MUX2N ").count(), 1, "{text}");
    assert_eq!(text.matches("  NR2 ").count(), 1, "{text}");
    assert!(!text.contains("  INV "), "{text}");
    assert!(!text.contains("  INR2 "), "{text}");
}

#[test]
fn synthesize_bitblasts_vector_flop_to_target_dffs() {
    let mut module = module_with_vector_flop_process(4);

    let report = synthesize_test_module(
        &mut module,
        SynthesisOptions {
            target_cells: vec![simple_dff_target_cell()].into(),
        },
    )
    .unwrap();
    let text = report.mapped_verilog();

    assert_eq!(report.report.cells, 4);
    assert!(
        text.contains("DFD1 q_reg_0_(.D(d[0]), .CP(clk), .Q(q[0]));"),
        "{text}"
    );
    assert!(text.contains("DFD1 q_reg_3_(.D(d[3]), .CP(clk), .Q(q[3]));"));
}

#[test]
fn synthesize_preserves_reconstructed_state_and_controls() {
    let mut module = WordModule::new("top");
    let inputs = ["clk", "reset", "enable", "d0", "d1"].map(|name| {
        module
            .add_port(name, PortDirection::Input, bit(), test_span())
            .unwrap()
    });
    let output = module
        .add_port(
            "q",
            PortDirection::Output,
            WordType::bits(2).unwrap(),
            test_span(),
        )
        .unwrap();
    let [clock, reset, enable, d0, d1] = inputs.map(|port| read_port(&mut module, port));
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), test_span())
        .unwrap();
    let states = [
        (
            d0,
            Some(word::Enable {
                value: enable,
                active_high: true,
            }),
            Vec::new(),
        ),
        (
            d1,
            None,
            vec![word::Reset {
                kind: word::ResetKind::Sync,
                value: reset,
                active_high: true,
                reset_value: zero,
            }],
        ),
    ]
    .map(|(d, enable, resets)| {
        module
            .register(
                word::RegisterOp {
                    name: None,
                    d,
                    clock,
                    edge: Edge::Pos,
                    enable,
                    resets,
                },
                test_span(),
            )
            .unwrap()
    });
    let reconstructed = module
        .concat(states.into_iter().rev().collect(), test_span())
        .unwrap();
    connect_port(&mut module, output, reconstructed);
    let reset_gate = target_cell(
        "ANR2",
        1.0,
        &[
            ("A", TargetPinDirection::Input, None),
            ("B", TargetPinDirection::Input, None),
            ("Z", TargetPinDirection::Output, Some("A*!B")),
        ],
    );

    let report = synthesize_test_module(
        &mut module,
        SynthesisOptions {
            target_cells: vec![simple_dff_target_cell(), reset_gate, mux_target_cell(1.0)].into(),
        },
    )
    .unwrap();
    let text = report.mapped_verilog();

    assert_eq!(text.matches("  DFD1 ").count(), 2, "{text}");
    assert_eq!(text.matches("  ANR2 ").count(), 1, "{text}");
    assert_eq!(text.matches("  MUX2 ").count(), 1, "{text}");
    // The generated instance index is an emission detail; the reconstructed
    // enable is the property under test, so match the connectivity only.
    assert!(
        text.contains("(.I0(q[0]), .I1(d0), .S(enable), .Z(n1));"),
        "{text}"
    );
    assert!(
        text.contains("DFD1 q_reg_0_(.D(n1), .CP(clk), .Q(q[0]));"),
        "{text}"
    );
}

#[test]
fn synthesize_shares_equivalent_registers_inside_a_small_design_region() {
    let mut module = WordModule::new("top");
    let clock = module
        .add_port("clk", PortDirection::Input, bit(), test_span())
        .unwrap();
    let data = module
        .add_port("d", PortDirection::Input, bit(), test_span())
        .unwrap();
    let mask = module
        .add_port("mask", PortDirection::Input, bit(), test_span())
        .unwrap();
    let y0 = module
        .add_port("y0", PortDirection::Output, bit(), test_span())
        .unwrap();
    let y1 = module
        .add_port("y1", PortDirection::Output, bit(), test_span())
        .unwrap();
    let q0 = module.add_wire("q0", bit(), test_span()).unwrap();
    let q1 = module.add_wire("q1", bit(), test_span()).unwrap();
    let clock = read_port(&mut module, clock);
    let data = read_port(&mut module, data);
    let first = module
        .register(
            word::RegisterOp {
                name: None,
                d: data,
                clock,
                edge: Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    let second = module
        .register(
            word::RegisterOp {
                name: None,
                d: data,
                clock,
                edge: Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    module
        .connect(LValue::signal(q0), first, test_span())
        .unwrap();
    module
        .connect(LValue::signal(q1), second, test_span())
        .unwrap();
    let q0_value = module.read_signal(q0, test_span()).unwrap();
    let mask = read_port(&mut module, mask);
    let masked = module
        .binary(BinaryOp::BitAnd, q0_value, mask, test_span())
        .unwrap();
    connect_port(&mut module, y0, masked);
    let q1_value = module.read_signal(q1, test_span()).unwrap();
    let y1_value = read_port(&mut module, y1);
    module
        .add_instance(
            "u_inv",
            "INV",
            vec![
                ("A".to_string(), q1_value, test_span()),
                ("Z".to_string(), y1_value, test_span()),
            ],
            test_span(),
        )
        .unwrap();

    let synthesized = synthesize_test_module(
        &mut module,
        SynthesisOptions {
            target_cells: vec![
                simple_dff_target_cell(),
                target_cell(
                    "AND2",
                    1.0,
                    &[
                        ("A", TargetPinDirection::Input, None),
                        ("B", TargetPinDirection::Input, None),
                        ("Z", TargetPinDirection::Output, Some("A B")),
                    ],
                ),
                target_cell(
                    "INV",
                    1.0,
                    &[
                        ("A", TargetPinDirection::Input, None),
                        ("Z", TargetPinDirection::Output, Some("!A")),
                    ],
                ),
            ]
            .into(),
        },
    )
    .unwrap();
    let text = synthesized.mapped_verilog();

    assert_eq!(text.matches("  DFD1 ").count(), 1, "{text}");
    assert!(text.contains(".Q(q0)"), "{text}");
    assert!(text.contains("INV u_inv(.A(q0), .Z(y1));"), "{text}");
}

#[test]
fn synthesize_assigns_unique_inferred_register_instance_name() {
    let mut module = module_with_flop_process();
    module
        .add_instance("q_reg", "DFD1", Vec::new(), test_span())
        .unwrap();

    let synthesized = synthesize_test_module(
        &mut module,
        SynthesisOptions {
            target_cells: vec![simple_dff_target_cell()].into(),
        },
    )
    .unwrap();

    let text = synthesized.mapped_verilog();
    assert!(text.contains("DFD1 q_reg_1("), "{text}");
}
