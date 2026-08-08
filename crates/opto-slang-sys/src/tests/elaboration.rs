// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_compile_unrolls_constant_procedural_for_loops() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin y = '0; for (int i = 0; i < 4; i++) y[i] = a[3-i]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 5);
    for (index, effect) in effects.iter().skip(1).enumerate() {
        let lhs = effect.lhs().unwrap();
        let rhs = effect.rhs().unwrap();
        let SlangExpressionKind::Signal(lhs) = lhs.kind().unwrap() else {
            panic!("expected unrolled signal target");
        };
        let index = u32::try_from(index).expect("test unrolled index fits u32");
        assert_eq!(lhs.range.unwrap().msb, index);
        let SlangExpressionKind::Signal(rhs) = rhs.kind().unwrap() else {
            panic!("expected selected input bit");
        };
        assert_eq!(rhs.range.unwrap().msb, 3 - index);
    }
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
