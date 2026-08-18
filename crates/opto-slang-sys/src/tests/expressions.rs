// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn expression_references_signal(expression: SlangExpression<'_>, expected: &str) -> bool {
    match expression.kind().unwrap() {
        SlangExpressionKind::Signal(signal) => signal.name == expected,
        SlangExpressionKind::Unary { arg, .. } => expression_references_signal(arg, expected),
        SlangExpressionKind::Binary { left, right, .. } => {
            expression_references_signal(left, expected)
                || expression_references_signal(right, expected)
        }
        SlangExpressionKind::Mux {
            condition,
            then_value,
            else_value,
        } => {
            expression_references_signal(condition, expected)
                || expression_references_signal(then_value, expected)
                || expression_references_signal(else_value, expected)
        }
        SlangExpressionKind::Concat(parts) => parts
            .parts()
            .any(|part| expression_references_signal(part.unwrap(), expected)),
        SlangExpressionKind::Cast { value, .. } | SlangExpressionKind::Extract { value, .. } => {
            expression_references_signal(value, expected)
        }
        SlangExpressionKind::DynamicExtract { value, offset, .. } => {
            expression_references_signal(value, expected)
                || expression_references_signal(offset, expected)
        }
        SlangExpressionKind::Constant(_) => false,
    }
}

#[test]
fn native_compile_expands_nested_and_procedural_let_invocations() {
    let source = NativeTestSource::new(
        "module top(input logic signed [3:0] a, input logic [5:0] b, input logic select, output logic [3:0] y, output logic [3:0] z); let step(logic signed [3:0] value) = value + 4'sd1; let nested = step(a) + 4'sd1; assign y = nested; always_comb begin let choose(logic pick, logic [5:0] first, second) = pick ? first : second; z = choose(select, y, b); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let rhs = module.assigns().next().unwrap().rhs().unwrap();
    assert!(expression_references_signal(rhs, "a"));
    assert!(!expression_references_signal(rhs, "value"));
    let effect = first_effect(module.procedures().next().unwrap());
    let rhs = effect.rhs().unwrap();
    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Mux { .. } | SlangExpressionKind::Cast { .. }
    ));
    assert!(expression_references_signal(rhs, "select"));
    assert!(expression_references_signal(rhs, "y"));
    assert!(expression_references_signal(rhs, "b"));
}

#[test]
fn native_compile_expands_interface_scope_let_invocations() {
    let source = NativeTestSource::new(
        "interface helper_if; logic [3:0] data; let invert(value) = ~value; function automatic logic [3:0] transformed(); transformed = invert(data); endfunction endinterface module top(input logic [3:0] a, output logic [3:0] y); helper_if bus(); assign bus.data = a; assign y = bus.transformed(); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(module.procedures().any(|procedure| {
        procedure_effects(procedure).iter().any(|effect| {
            effect
                .rhs()
                .is_ok_and(|rhs| expression_references_signal(rhs, "bus.data"))
        })
    }));
}

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
fn native_compile_preserves_division_by_zero_for_dont_care_lowering() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, output logic [7:0] quotient, remainder); assign quotient = value / 8'd0; assign remainder = value % 8'd0; endmodule\n",
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
fn native_compile_expands_runtime_base_with_constant_exponent() {
    fn multiplication_count(expression: SlangExpression<'_>) -> usize {
        match expression.kind().unwrap() {
            SlangExpressionKind::Binary { op, left, right } => {
                usize::from(op == SlangBinaryOp::Mul)
                    + multiplication_count(left)
                    + multiplication_count(right)
            }
            SlangExpressionKind::Cast { value, .. }
            | SlangExpressionKind::Extract { value, .. } => multiplication_count(value),
            _ => 0,
        }
    }

    let source = NativeTestSource::new(
        "module top(input logic [7:0] base, output logic [7:0] value); assign value = base ** 3; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert_eq!(multiplication_count(rhs), 2);
}

#[test]
fn native_compile_lowers_runtime_bit_vector_system_functions() {
    fn contains_binary(expression: SlangExpression<'_>, expected: SlangBinaryOp) -> bool {
        match expression.kind().unwrap() {
            SlangExpressionKind::Binary { op, left, right } => {
                op == expected
                    || contains_binary(left, expected)
                    || contains_binary(right, expected)
            }
            SlangExpressionKind::Cast { value, .. }
            | SlangExpressionKind::Extract { value, .. }
            | SlangExpressionKind::Unary { arg: value, .. } => contains_binary(value, expected),
            _ => false,
        }
    }

    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, output logic [31:0] count, output logic exactly_one, at_most_one); assign count = $countones(value); assign exactly_one = $onehot(value); assign at_most_one = $onehot0(value); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert!(contains_binary(
        assignments[0].rhs().unwrap(),
        SlangBinaryOp::Add
    ));
    assert!(contains_binary(
        assignments[1].rhs().unwrap(),
        SlangBinaryOp::LogicalAnd
    ));
    assert!(contains_binary(
        assignments[2].rhs().unwrap(),
        SlangBinaryOp::Eq
    ));
}

#[test]
fn native_compile_lowers_runtime_clog2_to_a_balanced_priority_encoder() {
    fn mux_depth(expression: SlangExpression<'_>) -> usize {
        match expression.kind().unwrap() {
            SlangExpressionKind::Mux {
                condition,
                then_value,
                else_value,
            } => {
                1 + mux_depth(condition)
                    .max(mux_depth(then_value))
                    .max(mux_depth(else_value))
            }
            SlangExpressionKind::Binary { left, right, .. } => {
                mux_depth(left).max(mux_depth(right))
            }
            SlangExpressionKind::Cast { value, .. }
            | SlangExpressionKind::Extract { value, .. }
            | SlangExpressionKind::Unary { arg: value, .. } => mux_depth(value),
            _ => 0,
        }
    }

    fn contains_subtraction(expression: SlangExpression<'_>) -> bool {
        match expression.kind().unwrap() {
            SlangExpressionKind::Binary { op, left, right } => {
                op == SlangBinaryOp::Sub
                    || contains_subtraction(left)
                    || contains_subtraction(right)
            }
            SlangExpressionKind::Mux {
                condition,
                then_value,
                else_value,
            } => {
                contains_subtraction(condition)
                    || contains_subtraction(then_value)
                    || contains_subtraction(else_value)
            }
            SlangExpressionKind::Cast { value, .. }
            | SlangExpressionKind::Extract { value, .. }
            | SlangExpressionKind::Unary { arg: value, .. } => contains_subtraction(value),
            _ => false,
        }
    }

    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, input logic signed [7:0] signed_value, output logic signed [31:0] magnitude, signed_magnitude); assign magnitude = $clog2(value); assign signed_magnitude = $clog2(signed_value); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    for assignment in assignments {
        let expression = assignment.rhs().unwrap();
        assert!(contains_subtraction(expression));
        assert!(mux_depth(expression) <= 5);
    }
}

#[test]
fn native_compile_lowers_runtime_countbits_for_binary_controls() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, output logic [31:0] zeros, binary); assign zeros = $countbits(value, 1'b0); assign binary = $countbits(value, 1'b0, 1'b1); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    assert!(matches!(
        assignments[0].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Sub,
            ..
        } | SlangExpressionKind::Cast { .. }
    ));
    assert!(matches!(
        assignments[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Constant(_) | SlangExpressionKind::Cast { .. }
    ));
}

#[test]
fn runtime_isunknown_fails_with_profile_diagnostic() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value, output logic unknown); assign unknown = $isunknown(value); endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires runtime X/Z observability")
    );
}

#[test]
fn native_compile_lowers_lossless_extended_equalities() {
    fn contains_binary(expression: SlangExpression<'_>, expected: SlangBinaryOp) -> bool {
        match expression.kind().unwrap() {
            SlangExpressionKind::Binary { op, left, right } => {
                op == expected
                    || contains_binary(left, expected)
                    || contains_binary(right, expected)
            }
            SlangExpressionKind::Cast { value, .. }
            | SlangExpressionKind::Extract { value, .. } => contains_binary(value, expected),
            _ => false,
        }
    }

    let source = NativeTestSource::new(
        "module top(input bit [3:0] a, b, output logic exact, different, masked, masked_different); assign exact = a === b; assign different = a !== b; assign masked = a ==? 4'b1x0z; assign masked_different = a !=? 4'b0z1x; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expressions = module
        .assigns()
        .map(|assignment| assignment.rhs().unwrap())
        .collect::<Vec<_>>();

    assert!(contains_binary(expressions[0], SlangBinaryOp::Eq));
    assert!(contains_binary(expressions[1], SlangBinaryOp::Ne));
    assert!(contains_binary(expressions[2], SlangBinaryOp::Eq));
    assert!(contains_binary(expressions[3], SlangBinaryOp::Ne));
}

#[test]
fn runtime_four_state_case_equality_fails_explicitly() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, b, output logic exact); assign exact = a === b; endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires runtime X/Z observability")
    );
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
fn native_compile_inlines_function_returns_from_cyclic_loops() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [1:0] y); function automatic logic [1:0] first_set(input logic [3:0] value); for (int i = 0; i < 4; i++) if (value[i]) return i; endfunction assign y = first_set(value); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(module.procedures().len(), 1);
    assert_eq!(module.nets().len(), 3);
    let procedure = module.procedures().next().unwrap();
    assert_eq!(procedure.loop_regions().len(), 1);
    assert!(procedure_effects(procedure).len() >= 3);
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
fn native_compile_preserves_exact_ref_argument_aliasing() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [3:0] y); task automatic mutate(ref logic [3:0] first, ref logic [3:0] second); first = 4'hc; second = first ^ 4'h3; endtask always_comb begin y = value; mutate(y, y); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 3);
    assert!(is_signal(effects[1].lhs().unwrap(), "y"));
    assert!(is_signal(effects[2].lhs().unwrap(), "y"));
    assert!(matches!(
        effects[2].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitXor,
            left,
            ..
        } if is_signal(left, "y")
    ));
    assert!(
        module
            .nets()
            .filter_map(|net| net.name().ok())
            .all(|name| !name.contains("_mutate_first") && !name.contains("_mutate_second"))
    );
}

#[test]
fn native_compile_freezes_dynamic_ref_selectors_at_call_entry() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value [0:3], input logic [1:0] index, output logic [7:0] y [0:3], output logic [1:0] next); task automatic update(ref logic [7:0] item, ref logic [1:0] selector); selector = selector + 2'd1; item = 8'ha5; endtask logic [7:0] working [0:3]; logic [1:0] chosen; always_comb begin working = value; chosen = index; update(working[chosen], chosen); y = working; next = chosen; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let selector_name = module
        .nets()
        .find_map(|net| {
            let name = net.name().ok()?;
            name.contains("_item_ref_selector")
                .then(|| (name.to_string(), net.is_process_local()))
        })
        .expect("dynamic ref argument should snapshot its selector");
    assert!(selector_name.1);

    let effects = procedure_effects(module.procedures().next().unwrap());
    let dynamic_lhs = effects
        .iter()
        .find_map(|effect| match effect.lhs().ok()?.kind().ok()? {
            SlangExpressionKind::DynamicExtract {
                offset, width: 8, ..
            } => Some(offset),
            _ => None,
        })
        .expect("ref assignment should retain a dynamic target");
    assert!(is_signal(dynamic_lhs, &selector_name.0));
    let snapshot = effects
        .iter()
        .position(|effect| {
            effect
                .lhs()
                .is_ok_and(|lhs| is_signal(lhs, &selector_name.0))
        })
        .expect("selector snapshot assignment should exist");
    let target_write = effects
        .iter()
        .position(|effect| {
            matches!(
                effect.lhs().and_then(SlangExpression::kind),
                Ok(SlangExpressionKind::DynamicExtract { width: 8, .. })
            )
        })
        .expect("dynamic ref target write should exist");
    assert!(snapshot < target_write);
}

#[test]
fn native_compile_rejects_packed_select_ref_actuals() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, input logic [1:0] index, output logic [3:0] y); task automatic update(ref logic item); item = ~item; endtask always_comb begin y = value; update(y[index]); end endmodule\n",
    );
    let SlangError::Diagnostics(diagnostics) = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .expect_err("packed select ref actual must fail language legality") else {
        panic!("frontend error did not preserve structured diagnostics");
    };

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == SlangDiagnosticSeverity::Error
            && diagnostic
                .message
                .contains("invalid expression for pass by reference")
    }));
}

#[test]
fn native_compile_inlines_value_returning_functions_with_ref_arguments() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [3:0] result, output logic [3:0] updated); function automatic logic [3:0] bump(ref logic [3:0] target); target = target + 4'd1; return target; endfunction logic [3:0] working; always_comb begin working = value; result = bump(working); updated = working; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert!(effects.iter().any(|effect| {
        is_signal(effect.lhs().unwrap(), "working")
            && matches!(
                effect.rhs().unwrap().kind().unwrap(),
                SlangExpressionKind::Binary {
                    op: SlangBinaryOp::Add,
                    left,
                    ..
                } if is_signal(left, "working")
            )
    }));
    assert!(
        module
            .nets()
            .filter_map(|net| net.name().ok())
            .all(|name| !name.contains("_bump_target"))
    );
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
