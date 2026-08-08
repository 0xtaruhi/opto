// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_compile_lowers_net_declaration_assignments() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, b, output logic [3:0] y); wire [4:0] sum = {1'b0, a} + {1'b0, b}; assign y = sum[3:0]; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert_eq!(assignments.len(), 2);
    assert!(is_signal(assignments[0].lhs().unwrap(), "sum"));
    assert!(matches!(
        assignments[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Add,
            ..
        }
    ));
}

#[test]
fn native_compile_preserves_wired_net_resolution() {
    let source = NativeTestSource::new(
        "module top(input logic a, b, output wor y); wand resolved; assign resolved = a; assign resolved = b; assign y = resolved; assign y = a; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let resolved = module
        .nets()
        .find(|net| net.name().unwrap() == "resolved")
        .unwrap();
    let y = module
        .ports()
        .find(|port| port.name().unwrap() == "y")
        .unwrap();

    assert_eq!(resolved.resolution().unwrap(), SlangNetResolution::WiredAnd);
    assert_eq!(y.resolution().unwrap(), SlangNetResolution::WiredOr);
}

#[test]
fn native_compile_rejects_unmodeled_declaration_and_driver_semantics() {
    let cases = [
        (
            "module top(output logic y); logic initialized = 1'b1; assign y = initialized; endmodule\n",
            "module variable initializer for 'initialized' is not supported for synthesis",
        ),
        (
            "module top(input logic a, output logic y); wire #1 delayed = a; assign y = delayed; endmodule\n",
            "delay on net 'delayed' is not supported for synthesis",
        ),
        (
            "module top(input logic a, output logic y); assign #1 y = a; endmodule\n",
            "delay on continuous assignment is not supported for synthesis",
        ),
        (
            "module top(input logic a, output logic y); wire (strong1, pull0) driven = a; assign y = driven; endmodule\n",
            "drive strength on net 'driven' is not supported for synthesis",
        ),
        (
            "module top(input logic a, output wire y); assign (strong1, pull0) y = a; endmodule\n",
            "drive strength on continuous assignment is not supported for synthesis",
        ),
        (
            "module top(output logic y); trireg (small) stored; assign y = stored; endmodule\n",
            "net type 'trireg' on net 'stored' is not supported for synthesis",
        ),
        (
            "module top(output logic y); tri0 pulled; assign y = pulled; endmodule\n",
            "net type 'tri0' on net 'pulled' is not supported for synthesis",
        ),
    ];

    for (source_text, expected) in cases {
        let source = NativeTestSource::new(source_text);
        let error = compile(
            std::slice::from_ref(&source.path),
            &SlangCompileOptions::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn native_compile_decomposes_continuous_lvalue_concatenations() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, output logic [2:0] high, output logic [4:0] low); assign {high, low} = value; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert_eq!(assignments.len(), 2);
    assert!(is_signal(assignments[0].lhs().unwrap(), "high"));
    assert!(is_signal(assignments[1].lhs().unwrap(), "low"));
    assert!(matches!(
        assignments[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef {
            name: "value",
            range: Some(SlangBitRange { msb: 7, lsb: 5 })
        })
    ));
    assert!(matches!(
        assignments[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef {
            name: "value",
            range: Some(SlangBitRange { msb: 4, lsb: 0 })
        })
    ));
}

#[test]
fn native_compile_retypes_signed_lvalue_concatenation_leaves() {
    let source = NativeTestSource::new(
        "module top(input logic [9:0] value, output logic signed [9:0] signed_value, output logic flag); assign {signed_value, flag} = {value, 1'b0}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let signed_rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        signed_rhs.kind().unwrap(),
        SlangExpressionKind::Cast {
            width: 10,
            signed: true,
            ..
        }
    ));
}

#[test]
fn native_compile_snapshots_blocking_lvalue_concatenations() {
    let source = NativeTestSource::new(
        "module top(input logic select, output logic a, b); always_comb begin a = select; b = ~select; {a, b} = {b, a}; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 5);
    assert!(
        module
            .nets()
            .any(|net| net.name().unwrap().starts_with("__opto_lvalue_"))
    );
    let snapshot_lhs = effects[2].lhs().unwrap();
    let first_rhs = effects[3].rhs().unwrap();

    assert!(matches!(
        snapshot_lhs.kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef { name, range: None })
            if name.starts_with("__opto_lvalue_")
    ));
    assert!(matches!(
        first_rhs.kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef { name, range: Some(_) })
            if name.starts_with("__opto_lvalue_")
    ));
}

#[test]
fn native_compile_flattens_loop_generate_assignments() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); for (genvar i = 0; i < 4; i++) begin : g_reverse assign y[i] = a[3-i]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.assigns().len(), 4);
    for (index, assign) in module.assigns().enumerate() {
        let lhs = assign.lhs().unwrap();
        let rhs = assign.rhs().unwrap();
        let SlangExpressionKind::Signal(lhs) = lhs.kind().unwrap() else {
            panic!("expected generated assignment signal target");
        };
        assert_eq!(
            lhs.range.unwrap().msb,
            u32::try_from(index).expect("test assignment index fits u32")
        );
        assert!(matches!(
            rhs.kind().unwrap(),
            SlangExpressionKind::Signal(SlangSignalRef {
                name: "a",
                range: Some(_),
            })
        ));
    }
}

#[test]
fn native_compile_excludes_uninstantiated_generate_branches() {
    let source = NativeTestSource::new(
        "module top #(parameter bit DIRECT = 1) (input logic a, output logic y); if (DIRECT) begin : g_direct assign y = a; end else begin : g_invert assign y = ~a; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.assigns().len(), 1);
    assert!(is_signal(
        module.assigns().next().unwrap().rhs().unwrap(),
        "a"
    ));
}

#[test]
fn native_compile_preserves_expression_source_location() {
    let source = NativeTestSource::new(
        "module top(\n  input logic [3:0] a, b,\n  output logic [3:0] y\n);\n  assign y = a + b;\nendmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Binary { .. }
    ));
    assert_eq!(
        std::fs::canonicalize(rhs.source().unwrap().file.unwrap()).unwrap(),
        std::fs::canonicalize(&source.path).unwrap()
    );
    assert_eq!(rhs.source().unwrap().line, Some(5));
    assert!(rhs.source().unwrap().column.is_some());
}

#[test]
fn native_compile_lowers_conditional_expression() {
    let source = NativeTestSource::new(
        "module top(input logic sel, input logic a, input logic b, output logic y); assign y = sel ? a : b; endmodule\n",
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
        } if is_signal(condition, "sel")
            && is_signal(then_value, "a")
            && is_signal(else_value, "b")
    ));
}
