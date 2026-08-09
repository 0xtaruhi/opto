// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_compile_normalizes_conditional_branch_signedness() {
    let source = NativeTestSource::new(
        "module top(input logic select, input logic signed [11:0] a, input logic [11:0] b, output logic [11:0] y); assign y = select ? a : b; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expression = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::Mux {
        then_value,
        else_value,
        ..
    } = expression.kind().unwrap()
    else {
        panic!("expected conditional mux");
    };
    assert!(matches!(
        then_value.kind().unwrap(),
        SlangExpressionKind::Cast {
            width: 12,
            signed: false,
            ..
        }
    ));
    assert!(is_signal(else_value, "b"));
}

#[test]
fn native_compile_retypes_conditional_part_select_branches() {
    let source = NativeTestSource::new(
        "module top(input logic select, input logic signed [7:0] signed_value, input logic [2:0] unsigned_value, output logic [2:0] y); assign y = select ? signed_value[2:0] : unsigned_value; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();
    let SlangExpressionKind::Mux {
        then_value,
        else_value,
        ..
    } = rhs.kind().unwrap()
    else {
        panic!("expected conditional expression");
    };

    assert!(matches!(
        then_value.kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef {
            name: "signed_value",
            range: Some(SlangBitRange { msb: 2, lsb: 0 })
        })
    ));
    assert!(is_signal(else_value, "unsigned_value"));
}

#[test]
fn native_compile_normalizes_inside_membership() {
    let source = NativeTestSource::new(
        "module top(input logic [2:0] value, output logic member); assign member = value inside {3'd1, 3'd3, 3'b1?0}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::LogicalOr,
            ..
        }
    ));
}

#[test]
fn native_compile_normalizes_inside_value_ranges() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic member); assign member = value inside {[4'd2:4'd5]}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::LogicalAnd,
            ..
        }
    ));
}

#[test]
fn native_compile_normalizes_replication_to_concatenation() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic [3:0] y); assign y = {4{a}}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expression = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::Concat(parts) = expression.kind().unwrap() else {
        panic!("expected normalized concatenation");
    };
    assert_eq!(parts.parts().len(), 4);
    assert!(parts.parts().all(|part| is_signal(part.unwrap(), "a")));
}

#[test]
fn native_compile_normalizes_extended_unary_operators() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic signed [3:0] neg, output logic nand_reduce); assign neg = -$signed(a); assign nand_reduce = ~&a; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert!(matches!(
        assignments[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Sub,
            ..
        }
    ));
    let SlangExpressionKind::Unary {
        op: SlangUnaryOp::BitNot,
        arg,
    } = assignments[1].rhs().unwrap().kind().unwrap()
    else {
        panic!("expected complemented reduction");
    };
    assert!(matches!(
        arg.kind().unwrap(),
        SlangExpressionKind::Unary {
            op: SlangUnaryOp::ReductionAnd,
            ..
        }
    ));
}

#[test]
fn native_compile_normalizes_binary_xnor() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, b, output logic [3:0] y); assign y = a ~^ b; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Unary {
            op: SlangUnaryOp::BitNot,
            arg,
        } if matches!(
            arg.kind().unwrap(),
            SlangExpressionKind::Binary {
                op: SlangBinaryOp::BitXor,
                ..
            }
        )
    ));
}

#[test]
fn native_compile_distinguishes_logical_and_arithmetic_right_shift() {
    let source = NativeTestSource::new(
        "module top(input logic signed [7:0] value, input logic [2:0] amount, output logic signed [7:0] logical_result, arithmetic_result); assign logical_result = value >> amount; assign arithmetic_result = value >>> amount; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert!(matches!(
        assignments[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Shr,
            ..
        }
    ));
    assert!(matches!(
        assignments[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Ashr,
            ..
        }
    ));
}

#[test]
fn native_compile_preserves_context_sized_binary_operands() {
    let source = NativeTestSource::new(
        "module top(input logic signed [1:0] a, input logic signed [2:0] b, output logic [3:0] y); assign y = (a + b) + 3'd0; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();
    let SlangExpressionKind::Binary { left, .. } = rhs.kind().unwrap() else {
        panic!("expected outer binary expression");
    };
    let SlangExpressionKind::Binary {
        left: inner_left,
        right: inner_right,
        ..
    } = left.kind().unwrap()
    else {
        panic!("expected inner binary expression");
    };
    let SlangExpressionKind::Cast {
        kind: left_kind,
        width: left_width,
        signed: left_signed,
        ..
    } = inner_left.kind().unwrap()
    else {
        panic!("expected conversion around the left operand");
    };
    assert_eq!((left_width, left_signed), (4, false));
    assert_eq!(left_kind, SlangCastKind::ZeroExtend);
    assert!(matches!(
        inner_right.kind().unwrap(),
        SlangExpressionKind::Cast {
            kind: SlangCastKind::ZeroExtend,
            width: 4,
            signed: false,
            ..
        }
    ));
}

#[test]
fn native_compile_preserves_invariant_division_for_synthesis_planning() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, output logic [7:0] quotient, remainder); assign quotient = value / 8'd4; assign remainder = value % 8'd4; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert!(matches!(
        assignments[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Div,
            ..
        }
    ));
    assert!(matches!(
        assignments[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Mod,
            ..
        }
    ));
}

#[test]
fn native_compile_reduces_dynamic_power_of_two_exponentiation() {
    fn contains_shift(expression: SlangExpression<'_>) -> bool {
        match expression.kind().unwrap() {
            SlangExpressionKind::Binary {
                op: SlangBinaryOp::Shl,
                ..
            } => true,
            SlangExpressionKind::Cast { value, .. }
            | SlangExpressionKind::Extract { value, .. } => contains_shift(value),
            _ => false,
        }
    }

    let source = NativeTestSource::new(
        "module top(input logic [4:0] exponent, output logic [31:0] value); assign value = 2 ** exponent; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(contains_shift(rhs));
}

#[test]
fn native_compile_lowers_integral_system_casts() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic signed [3:0] s, output logic [3:0] u); assign s = $signed(a); assign u = $unsigned(s); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expressions = module
        .assigns()
        .map(|assign| assign.rhs().unwrap().kind().unwrap())
        .collect::<Vec<_>>();

    assert!(matches!(
        expressions[0],
        SlangExpressionKind::Cast {
            signed: true,
            width: 4,
            ..
        }
    ));
    assert!(matches!(
        expressions[1],
        SlangExpressionKind::Cast {
            signed: false,
            width: 4,
            ..
        }
    ));
}

#[test]
fn native_compile_inlines_input_only_combinational_functions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [6:0] y); function automatic logic [6:0] adjust(input logic [3:0] value); case (value) 4'd0: return 7'd16; 4'd1: return 7'd32; default: return {3'b0, value}; endcase endfunction assign y = adjust(a); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(module.nets().len(), 1);
    let result = module.assigns().next().unwrap().rhs().unwrap();
    let SlangExpressionKind::Signal(result) = result.kind().unwrap() else {
        panic!("expected inlined function result signal");
    };
    assert!(result.name.starts_with("__opto_fn_"));
    let (_, arms, _) = first_switch(module.procedures().next().unwrap());
    assert_eq!(arms.len(), 2);
}

#[test]
fn native_compile_unrolls_constant_bounded_recursive_functions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] base, output logic [3:0] y); function automatic logic [3:0] power(input logic [3:0] value, input logic [3:0] exponent); begin power = 1; if (exponent > 0) power = value * power(value, exponent - 1); end endfunction assign y = power(base, 3); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(
        module
            .nets()
            .filter(|net| net.name().unwrap().contains("_return"))
            .count(),
        4
    );
}

#[test]
fn native_compile_extracts_from_function_actual_expressions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, b, output logic [1:0] y); function automatic logic [1:0] pick(input logic [3:0] value); return value[2:1]; endfunction assign y = pick(a ^ b); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let (value, lsb, width) = effects
        .into_iter()
        .find_map(|effect| match effect.rhs().ok()?.kind().ok()? {
            SlangExpressionKind::Extract { value, lsb, width } => Some((value, lsb, width)),
            _ => None,
        })
        .expect("expected extract from actual expression");
    assert_eq!((lsb, width), (1, 2));
    assert!(matches!(
        value.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitXor,
            ..
        }
    ));
}

#[test]
fn native_compile_inlines_early_function_returns() {
    let source = NativeTestSource::new(
        "module top(input logic select, a, b, output logic y); function automatic logic choose(input logic select, a, b); if (select) return a; return b; endfunction assign y = choose(select, a, b); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(module.nets().len(), 2);
    assert!(procedure_effects(module.procedures().next().unwrap()).len() >= 4);
}

#[test]
fn native_compile_inlines_function_returns_from_unrolled_loops() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [1:0] y); function automatic logic [1:0] first_set(input logic [3:0] value); for (int i = 0; i < 4; i++) if (value[i]) return i; endfunction assign y = first_set(value); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(module.nets().len(), 2);
    assert!(procedure_effects(module.procedures().next().unwrap()).len() >= 6);
}

#[test]
fn native_compile_propagates_constant_struct_function_arguments() {
    let source = NativeTestSource::new(
        "package p; typedef struct packed { logic [2:0] count; } cfg_t; function automatic logic any(cfg_t cfg, logic [3:0] value); logic result = 1'b0; for (int i = 0; i < cfg.count; i++) result = result | value[i]; return result; endfunction endpackage module top(input logic [3:0] value, output logic y); localparam p::cfg_t CFG = '{count: 3'd2}; assign y = p::any(CFG, value); endmodule\n",
    );
    let compilation = compile_source(&source);

    let module = first_module(&compilation);
    assert_eq!(module.procedures().len(), 1);
}

#[test]
fn native_compile_copies_function_arguments_that_are_written() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [3:0] y); typedef struct packed { logic [3:0] data; } request_t; function automatic request_t update(request_t request); request.data = request.data ^ 4'hf; return request; endfunction request_t source; assign source.data = value; assign y = update(source).data; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(module.nets().len(), 3);
    assert!(
        module
            .nets()
            .any(|net| net.name().unwrap().contains("_request"))
    );
}

#[test]
fn native_compile_inlines_function_expressions_at_procedural_call_sites() {
    let source = NativeTestSource::new(
        "module top(input logic [4:0] value, output logic [4:0] y); function automatic logic [4:0] identity(input logic [4:0] arg); return arg; endfunction logic [4:0] working; always_comb begin working = value; y = identity(working); working = working - 5'd1; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(
        procedure_effects(module.procedures().next().unwrap()).len(),
        5
    );
    assert!(
        module
            .nets()
            .filter(|net| net.name().unwrap().contains("__opto_fn_"))
            .all(SlangNet::is_process_local)
    );
}

#[test]
fn native_compile_inlines_void_functions_with_output_arguments() {
    let source = NativeTestSource::new(
        "module top(input logic enable, input logic [1:0] value, output logic [1:0] y); function automatic void split(output logic low, output logic high, input logic [1:0] source); low = source[0]; high = source[1]; endfunction always_comb begin y = '0; if (enable) split(y[0], y[1], value); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert!(
        module
            .nets()
            .filter_map(|net| net.name().ok())
            .filter(|name| name.contains("_split_"))
            .count()
            >= 2
    );
    let procedure = module.procedures().next().unwrap();
    let _ = first_branch(procedure);
    assert!(procedure_effects(procedure).len() >= 6);
}

#[test]
fn native_compile_inlines_synthesizable_tasks() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] value, output logic [1:0] y); task automatic copy(output logic [1:0] target, input logic [1:0] source); target = source; endtask always_comb begin copy(y, value); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert!(
        module
            .nets()
            .filter_map(|net| net.name().ok())
            .any(|name| name.contains("_copy_"))
    );
    assert!(procedure_effects(module.procedures().next().unwrap()).len() >= 3);
}

#[test]
fn native_compile_lowers_package_enum_values_as_constants() {
    let source = NativeTestSource::new(
        "package p; typedef enum logic [1:0] { ZERO, ONE, TWO } value_e; endpackage module top(output logic [1:0] y); assign y = p::TWO; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expression = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        expression.kind().unwrap(),
        SlangExpressionKind::Constant(constant)
            if constant.width == Some(2) && constant.bits == "10"
    ));
}
