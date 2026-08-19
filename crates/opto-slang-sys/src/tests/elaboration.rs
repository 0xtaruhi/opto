// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_compile_lowers_constant_procedural_for_body_once() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin y = '0; for (int i = 0; i < 4; i++) y[i] = a[3-i]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    let procedure = module.procedures().next().unwrap();
    assert_eq!(procedure.loop_regions().len(), 1);
    assert_eq!(
        effects
            .iter()
            .filter(|effect| matches!(
                effect.lhs().and_then(SlangExpression::kind),
                Ok(SlangExpressionKind::DynamicExtract { .. })
            ))
            .count(),
        1,
        "the source body remains dynamic and is lowered once"
    );
}

#[test]
fn native_compile_normalizes_builtin_logic_primitives() {
    let source = NativeTestSource::new(
        "module top(input wire a, b, c, output wire y); wire n; and (n, a, b, c); nand (y, n, c); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assigns = module.assigns().collect::<Vec<_>>();

    assert_eq!(assigns.len(), 2);
    assert!(matches!(
        assigns[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitAnd,
            left,
            right,
        } if matches!(
            left.kind().unwrap(),
            SlangExpressionKind::Binary {
                op: SlangBinaryOp::BitAnd,
                ..
            }
        ) && is_signal(right, "c")
    ));
    assert!(matches!(
        assigns[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Unary {
            op: SlangUnaryOp::BitNot,
            arg,
        } if matches!(
            arg.kind().unwrap(),
            SlangExpressionKind::Binary {
                op: SlangBinaryOp::BitAnd,
                ..
            }
        )
    ));
}

#[test]
fn native_compile_preserves_tristate_primitive_semantics() {
    let source = NativeTestSource::new(
        "module top(input wire a, en, output wire y); bufif1 (y, a, en); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Mux {
            condition,
            then_value,
            else_value,
        } if is_signal(condition, "en")
            && is_signal(then_value, "a")
            && matches!(
                else_value.kind().unwrap(),
                SlangExpressionKind::Constant(SlangLogicConstant {
                    width: Some(1),
                    bits: "z",
                    ..
                })
            )
    ));
}

#[test]
fn native_compile_preserves_inverting_tristate_primitive_semantics() {
    let source = NativeTestSource::new(
        "module top(input wire a, en, output wire y); notif0 (y, a, en); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Mux {
            condition,
            then_value,
            else_value,
        } if is_signal(condition, "en")
            && matches!(
                then_value.kind().unwrap(),
                SlangExpressionKind::Constant(SlangLogicConstant {
                    width: Some(1),
                    bits: "z",
                    ..
                })
            )
            && matches!(
                else_value.kind().unwrap(),
                SlangExpressionKind::Unary {
                    op: SlangUnaryOp::BitNot,
                    arg,
                } if is_signal(arg, "a")
            )
    ));
}

#[test]
fn native_compile_lowers_pull_primitives_to_constant_drivers() {
    let source = NativeTestSource::new(
        "module top(output wire high, low); pullup (high); pulldown (low); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assigns = module.assigns().collect::<Vec<_>>();

    assert_eq!(assigns.len(), 2);
    assert!(matches!(
        assigns[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(1),
            bits: "1",
            ..
        })
    ));
    assert!(matches!(
        assigns[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(1),
            bits: "0",
            ..
        })
    ));
}

#[test]
fn native_compile_rejects_switch_level_primitives_explicitly() {
    let cases = [
        ("cmos", "z, a, en, p"),
        ("rcmos", "z, a, en, p"),
        ("nmos", "z, a, en"),
        ("pmos", "z, a, en"),
        ("rnmos", "z, a, en"),
        ("rpmos", "z, a, en"),
        ("tran", "x, y"),
        ("rtran", "x, y"),
        ("tranif0", "x, y, en"),
        ("tranif1", "x, y, en"),
        ("rtranif0", "x, y, en"),
        ("rtranif1", "x, y, en"),
    ];

    for (primitive, terminals) in cases {
        let source = NativeTestSource::new(&format!(
            "module top(input wire a, en, p, inout wire x, y, output wire z); {primitive} ({terminals}); endmodule\n"
        ));
        let error = compile(
            std::slice::from_ref(&source.path),
            &SlangCompileOptions::default(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("primitive '{primitive}'")),
            "{error}"
        );
    }
}

#[test]
fn native_compile_lowers_combinational_udp_tables() {
    let source = NativeTestSource::new(
        "primitive udp_majority(output out, input a, b, c); table 0 0 ? : 0; 0 ? 0 : 0; ? 0 0 : 0; 1 1 ? : 1; 1 ? 1 : 1; ? 1 1 : 1; endtable endprimitive module top(input logic a, b, c, output logic y); udp_majority u_majority(y, a, b, c); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assigns = module.assigns().collect::<Vec<_>>();

    assert_eq!(assigns.len(), 1);
    assert!(matches!(
        assigns[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Mux { .. }
    ));
    assert_eq!(module.instances().len(), 0);
}

#[test]
fn native_compile_lowers_level_sensitive_sequential_udp() {
    let source = NativeTestSource::new(
        "primitive udp_latch(q, d, en); output reg q; input d, en; table ? 0 : ? : -; 0 1 : ? : 0; 1 1 : ? : 1; endtable endprimitive module top(input logic d, en, output logic q); udp_latch u_latch(q, d, en); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module
        .procedures()
        .next()
        .expect("UDP should lower to Proc IR");

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::CombOrLatch);
    assert_eq!(procedure_effects(process).len(), 2);
    let _ = first_branch(process);
}

#[test]
fn native_compile_lowers_edge_sensitive_udp_to_flop_events() {
    let source = NativeTestSource::new(
        "primitive udp_dff(q, d, clk); output reg q; input d, clk; table 0 (01) : ? : 0; 1 (01) : ? : 1; ? (10) : ? : -; endtable endprimitive module top(input logic d, clk, output logic q); udp_dff u_dff(q, d, clk); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module
        .procedures()
        .next()
        .expect("UDP should lower to Proc IR");
    let events = process.events().collect::<Vec<_>>();

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Flop);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].edge().unwrap(), SlangEdge::Pos);
    assert_eq!(events[0].signal().unwrap().name, "clk");
    let effects = procedure_effects(process);
    assert_eq!(effects.len(), 2);
    assert!(
        effects
            .iter()
            .all(|effect| effect.mode() == SlangAssignmentMode::Nonblocking)
    );
}

#[test]
fn native_compile_normalizes_udp_edge_shorthands_in_the_binary_domain() {
    let source = NativeTestSource::new(
        "primitive udp_toggle(q, clk); output reg q; input clk; table p : 0 : 1; p : 1 : 0; n : 0 : 1; n : 1 : 0; endtable endprimitive module top(input logic clk, output logic q); udp_toggle u_toggle(q, clk); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();
    let events = process.events().collect::<Vec<_>>();

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Flop);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].edge().unwrap(), SlangEdge::Pos);
    assert_eq!(events[1].edge().unwrap(), SlangEdge::Neg);
    assert!(
        events
            .iter()
            .all(|event| event.signal().unwrap().name == "clk")
    );
    assert_eq!(procedure_effects(process).len(), 4);
}

#[test]
fn native_compile_lowers_udp_level_row_as_one_async_control() {
    let source = NativeTestSource::new(
        "primitive udp_dff(q, d, clk, reset_n); output reg q; input d, clk, reset_n; table ? ? 0 : ? : 0; 0 p 1 : ? : 0; 1 p 1 : ? : 1; endtable endprimitive module top(input logic d, clk, reset_n, output logic q); udp_dff u_dff(q, d, clk, reset_n); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();
    let events = process.events().collect::<Vec<_>>();

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Flop);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].edge().unwrap(), SlangEdge::Pos);
    assert_eq!(events[0].signal().unwrap().name, "clk");
    assert_eq!(events[1].edge().unwrap(), SlangEdge::Neg);
    assert_eq!(events[1].signal().unwrap().name, "reset_n");
    assert_eq!(procedure_effects(process).len(), 3);
    let _ = first_branch(process);
}

#[test]
fn native_compile_lowers_udp_edge_row_as_a_distinct_async_control_event() {
    let source = NativeTestSource::new(
        "primitive udp_dff(q, d, clk, reset); output reg q; input d, clk, reset; table ? ? r : ? : 0; ? r 1 : ? : 0; 0 r 0 : ? : 0; 1 r 0 : ? : 1; endtable endprimitive module top(input logic d, clk, reset, output logic q); udp_dff u_dff(q, d, clk, reset); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();
    let events = process.events().collect::<Vec<_>>();

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Flop);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].signal().unwrap().name, "clk");
    assert_eq!(events[0].edge().unwrap(), SlangEdge::Pos);
    assert_eq!(events[1].signal().unwrap().name, "reset");
    assert_eq!(events[1].edge().unwrap(), SlangEdge::Pos);
    assert_eq!(procedure_effects(process).len(), 4);
}

#[test]
fn native_compile_rejects_udp_updates_from_distinct_transition_inputs() {
    let source = NativeTestSource::new(
        "primitive udp_multi(q, a, b); output reg q; input a, b; table r ? : ? : 0; ? r : ? : 1; endtable endprimitive module top(input logic a, b, output logic q); udp_multi u_multi(q, a, b); endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .expect_err("distinct update-event inputs require a unique data clock");

    assert!(
        error
            .to_string()
            .contains("has no unique data-update transition input"),
        "{error}"
    );
}

#[test]
fn native_compile_identifies_one_udp_data_clock_among_transition_controls() {
    let source = NativeTestSource::new(
        "primitive udp_dff(q, d, clk, clear); output reg q; input d, clk, clear; table ? ? r : ? : 0; 0 r 0 : ? : 0; 1 r 0 : ? : 1; endtable endprimitive module top(input logic d, clk, clear, output logic q); udp_dff u(q, d, clk, clear); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module
        .procedures()
        .next()
        .expect("UDP must lower to Proc IR");
    let events = procedure.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    for name in ["clk", "clear"] {
        let event = events
            .iter()
            .find(|event| event.signal().is_ok_and(|signal| signal.name == name))
            .unwrap_or_else(|| panic!("UDP must publish the {name} transition"));
        assert_eq!(event.edge().unwrap(), SlangEdge::Pos);
    }
}

#[test]
fn native_compile_lowers_named_instance_connections_in_source_order() {
    let source = NativeTestSource::new(
        "module child(input logic a, output logic y); assign y = a; endmodule\nmodule top(input logic a, output logic y); child u_child(.a(a), .y(y)); endmodule\n",
    );
    let compilation = compile_source(&source);
    let modules = materialized_modules(&compilation);

    assert_eq!(modules[0].name().unwrap(), "child");
    assert_eq!(modules[1].name().unwrap(), "top");
    assert!(modules[0].source_order() < modules[1].source_order());
    let child_rhs = modules[0].assigns().next().unwrap().rhs().unwrap();
    assert_eq!(
        std::fs::canonicalize(child_rhs.source().unwrap().file.unwrap()).unwrap(),
        std::fs::canonicalize(&source.path).unwrap()
    );
    assert!(is_signal(child_rhs, "a"));
    let instance = modules[1].instances().next().unwrap();
    let connections = instance.connections().collect::<Vec<_>>();
    assert_eq!(connections[0].port().unwrap(), "a");
    assert_eq!(connections[1].port().unwrap(), "y");
    assert!(is_signal(connections[0].expression().unwrap(), "a"));
    assert!(is_signal(connections[1].expression().unwrap(), "y"));
}

#[test]
fn native_compile_preserves_output_connection_lvalue_width() {
    let source = NativeTestSource::new(
        "module child(output logic [3:0] y); assign y = 4'ha; endmodule\nmodule top(output logic [7:0] y); child u_child(.y(y)); endmodule\n",
    );
    let compilation = compile_source(&source);
    let modules = materialized_modules(&compilation);
    let top = modules
        .iter()
        .find(|module| module.name().unwrap() == "top")
        .unwrap();
    let connection = top
        .instances()
        .next()
        .unwrap()
        .connections()
        .next()
        .unwrap();

    assert!(matches!(
        connection.expression().unwrap().kind().unwrap(),
        SlangExpressionKind::Signal(signal) if signal.name == "y" && signal.range.is_none()
    ));
}

#[test]
fn native_compile_distinguishes_parameterized_module_specializations() {
    let source = NativeTestSource::new(
        "module leaf #(parameter int W = 1) (input logic [W-1:0] a, output logic [W-1:0] y); assign y = a; endmodule module top(input logic [1:0] a2, input logic [3:0] a4, output logic [1:0] y2, output logic [3:0] y4); leaf #(.W(2)) u2(.a(a2), .y(y2)); leaf #(.W(4)) u4(.a(a4), .y(y4)); endmodule\n",
    );
    let compilation = compile_source(&source);
    let modules = materialized_modules(&compilation);
    let top = modules
        .iter()
        .find(|module| module.name().unwrap() == "top")
        .unwrap();
    let references = top
        .instances()
        .map(|instance| instance.module_name().unwrap().to_string())
        .collect::<Vec<_>>();

    assert_eq!(modules.len(), 3);
    assert_ne!(references[0], references[1]);
    assert!(references.iter().all(|name| name.starts_with("leaf__P")));
    for reference in references {
        assert!(
            modules
                .iter()
                .any(|module| module.name().unwrap() == reference)
        );
    }
}

#[test]
fn native_compile_retains_unresolved_instances() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); INVX1 u_inv(.A(a), .Y(y)); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let instance = module.instances().next().unwrap();

    assert_eq!(instance.name().unwrap(), "u_inv");
    assert_eq!(instance.module_name().unwrap(), "INVX1");
    assert_eq!(instance.connections().len(), 2);
}

#[test]
fn native_compile_lowers_always_comb_assignment() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); always_comb begin y = a; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();
    let effect = first_effect(process);

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Comb);
    assert_eq!(effect.mode(), SlangAssignmentMode::Blocking);
    let lhs = effect.lhs().unwrap();
    let rhs = effect.rhs().unwrap();
    assert!(is_signal(lhs, "y"));
    assert!(is_signal(rhs, "a"));
}
