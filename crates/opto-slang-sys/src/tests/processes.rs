// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn assert_cyclic_region_count(procedure: SlangProcedure<'_>, expected: usize) {
    let regions = procedure.loop_regions().collect::<Vec<_>>();
    assert_eq!(regions.len(), expected);
    for region in regions {
        let latch = procedure.block(region.latch()).unwrap();
        assert!(match latch.terminator().kind().unwrap() {
            SlangTerminatorKind::Jump(edge) => edge.block == region.header(),
            SlangTerminatorKind::Branch { then_edge, .. } => {
                then_edge.block == region.header()
            }
            _ => false,
        });
    }
}

#[test]
fn native_compile_normalizes_compound_procedural_assignments() {
    let source = NativeTestSource::new(
        "module top(input logic [4:0] a, output logic [4:0] y); always_comb begin y = a; y -= 5'd1; y |= 5'd2; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 5);
    assert!(matches!(
        effects[2].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Sub,
            ..
        }
    ));
    assert!(matches!(
        effects[4].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitOr,
            ..
        }
    ));
}

#[test]
fn native_compile_sequences_blocking_assignment_expressions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] p, q, r, output logic [3:0] a, b, y); always_comb begin a = p; b = q; y = ((a = b) + (b = r)); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 7);
    let SlangExpressionKind::Signal(first_result) = effects[2].lhs().unwrap().kind().unwrap()
    else {
        panic!("first assignment expression should have a result temporary");
    };
    let SlangExpressionKind::Signal(second_result) = effects[4].lhs().unwrap().kind().unwrap()
    else {
        panic!("second assignment expression should have a result temporary");
    };
    assert!(first_result.name.starts_with("__opto_assignment_"));
    assert!(second_result.name.starts_with("__opto_assignment_"));
    assert!(is_signal(effects[2].rhs().unwrap(), "b"));
    assert!(is_signal(effects[3].lhs().unwrap(), "a"));
    assert!(is_signal(effects[3].rhs().unwrap(), first_result.name));
    assert!(is_signal(effects[4].rhs().unwrap(), "r"));
    assert!(is_signal(effects[5].lhs().unwrap(), "b"));
    assert!(is_signal(effects[5].rhs().unwrap(), second_result.name));
    assert!(matches!(
        effects[6].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Add,
            left,
            right,
        } if is_signal(left, first_result.name)
            && is_signal(right, second_result.name)
    ));
}

#[test]
fn native_compile_sequences_compound_assignment_expressions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] p, q, output logic [3:0] a, y); always_comb begin a = p; y = (a += q); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 5);
    let SlangExpressionKind::Signal(snapshot) = effects[1].lhs().unwrap().kind().unwrap() else {
        panic!("compound assignment should snapshot its old lvalue");
    };
    let SlangExpressionKind::Signal(result) = effects[2].lhs().unwrap().kind().unwrap() else {
        panic!("compound assignment expression should have a result temporary");
    };
    assert!(snapshot.name.starts_with("__opto_compound_"));
    assert!(is_signal(effects[1].rhs().unwrap(), "a"));
    assert!(matches!(
        effects[2].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Add,
            left,
            right,
        } if is_signal(left, snapshot.name) && is_signal(right, "q")
    ));
    assert!(is_signal(effects[3].lhs().unwrap(), "a"));
    assert!(is_signal(effects[3].rhs().unwrap(), result.name));
    assert!(is_signal(effects[4].rhs().unwrap(), result.name));
}

#[test]
fn native_compile_sequences_compound_lvalue_concatenations() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] p, q, r, output logic [3:0] a, b, output logic [7:0] y); always_comb begin a = p; b = q; y = ({a, b} += {4'b0, (a = r)}); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    let snapshot = effects
        .iter()
        .position(|effect| {
            effect.lhs().is_ok_and(|lhs| {
                matches!(lhs.kind(), Ok(SlangExpressionKind::Signal(signal))
                    if signal.name.starts_with("__opto_compound_"))
            })
        })
        .expect("compound concatenation should snapshot its complete old value");
    let nested_update = effects
        .iter()
        .enumerate()
        .skip(snapshot + 1)
        .find_map(|(index, effect)| {
            effect
                .lhs()
                .is_ok_and(|lhs| is_signal(lhs, "a"))
                .then_some(index)
        })
        .expect("compound RHS should retain its nested assignment");
    let distributed_write = effects
        .iter()
        .rposition(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "b")))
        .expect("compound concatenation should distribute its low result slice");

    let SlangExpressionKind::Concat(parts) = effects[snapshot].rhs().unwrap().kind().unwrap()
    else {
        panic!("compound concatenation snapshot should read one concatenated value");
    };
    let parts = parts.parts().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(parts.len(), 2);
    assert!(is_signal(parts[0], "a"));
    assert!(is_signal(parts[1], "b"));
    assert!(snapshot < nested_update);
    assert!(nested_update < distributed_write);
}

#[test]
fn native_compile_freezes_every_compound_concatenation_selector_before_the_rhs() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] base [0:3], input logic index, output logic [3:0] memory [0:3], output logic next, output logic [7:0] assigned); logic [3:0] working [0:3]; logic chosen; always_comb begin working = base; chosen = index; assigned = ({working[chosen], working[chosen + 2'd1]} += {7'd0, (chosen = ~chosen)}); memory = working; next = chosen; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let selectors = module
        .nets()
        .filter_map(|net| {
            let name = net.name().ok()?;
            name.ends_with("_selector").then(|| name.to_string())
        })
        .collect::<Vec<_>>();
    assert_eq!(selectors.len(), 2);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let chosen_update = effects
        .iter()
        .rposition(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "chosen")))
        .expect("compound RHS should update its selector source");
    for selector in selectors {
        let snapshot = effects
            .iter()
            .position(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, &selector)))
            .expect("each dynamic concatenation leaf should snapshot its selector");
        let write = effects
            .iter()
            .rposition(|effect| {
                matches!(
                    effect.lhs().and_then(SlangExpression::kind),
                    Ok(SlangExpressionKind::DynamicExtract { offset, width: 4, .. })
                        if is_signal(offset, &selector)
                )
            })
            .expect("each dynamic concatenation leaf should write through its frozen selector");
        assert!(snapshot < chosen_update);
        assert!(chosen_update < write);
    }
}

#[test]
fn native_compile_guards_conditional_assignment_expression_effects() {
    let source = NativeTestSource::new(
        "module top(input logic select, input logic [3:0] p, q, r, output logic [3:0] a, b, y); always_comb begin a = p; b = q; y = select ? (a = r) : (b = r); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (condition, _, _) = first_branch(procedure);
    let effects = procedure_effects(procedure);

    assert!(is_signal(condition, "select"));
    assert_eq!(effects.len(), 9);
    assert!(
        effects
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "a")))
    );
    assert!(
        effects
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "b")))
    );
    assert!(module.nets().any(|net| {
        net.name()
            .is_ok_and(|name| name.starts_with("__opto_conditional_"))
    }));
}

#[test]
fn native_compile_short_circuits_assignment_expression_effects() {
    let source = NativeTestSource::new(
        "module top(input logic enable, input logic p, q, output logic a, y); always_comb begin a = p; y = enable && (a = q); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (condition, _, _) = first_branch(procedure);

    assert!(is_signal(condition, "enable"));
    assert!(module.nets().any(|net| {
        net.name()
            .is_ok_and(|name| name.starts_with("__opto_short_circuit_"))
    }));
    assert!(
        procedure_effects(procedure)
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "a")))
    );
}

#[test]
fn native_compile_freezes_dynamic_assignment_expression_targets() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value [0:3], input logic [1:0] index, output logic [7:0] y [0:3], output logic [1:0] next, output logic [7:0] assigned); logic [7:0] working [0:3]; logic [1:0] chosen; always_comb begin working = value; chosen = index; assigned = (working[chosen] = (chosen = chosen + 2'd1)); y = working; next = chosen; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let selector = module
        .nets()
        .find_map(|net| {
            let name = net.name().ok()?;
            name.ends_with("_selector").then(|| name.to_string())
        })
        .expect("dynamic assignment expression should snapshot its selector");
    let effects = procedure_effects(module.procedures().next().unwrap());
    let snapshot = effects
        .iter()
        .position(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, &selector)))
        .expect("selector snapshot effect should exist");
    let chosen_update = effects
        .iter()
        .rposition(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "chosen")))
        .expect("nested assignment should update its target");
    let dynamic_update = effects
        .iter()
        .position(|effect| {
            matches!(
                effect.lhs().and_then(SlangExpression::kind),
                Ok(SlangExpressionKind::DynamicExtract { offset, width: 8, .. })
                    if is_signal(offset, &selector)
            )
        })
        .expect("outer assignment should use the frozen selector");

    assert!(snapshot < chosen_update);
    assert!(chosen_update < dynamic_update);
}

#[test]
fn native_compile_leaves_pure_dynamic_statement_targets_in_source_order() {
    let source = NativeTestSource::new(
        "module top(input logic clk, input logic [1:0] index, input logic [7:0] value, output logic [7:0] memory [0:3]); always_ff @(posedge clk) memory[index] <= value; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(
        !module
            .nets()
            .any(|net| { net.name().is_ok_and(|name| name.ends_with("_selector")) })
    );
    assert!(
        procedure_effects(module.procedures().next().unwrap())
            .iter()
            .any(|effect| matches!(
                effect.lhs().and_then(SlangExpression::kind),
                Ok(SlangExpressionKind::DynamicExtract { width: 8, .. })
            ))
    );
}

#[test]
fn native_compile_freezes_dynamic_statement_targets_before_rhs_effects() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] value [0:3], input logic [1:0] index, output logic [7:0] memory [0:3], output logic [1:0] next); logic [1:0] chosen; always_comb begin memory = value; chosen = index; memory[chosen] = (chosen = chosen + 2'd1); next = chosen; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let selector = module
        .nets()
        .find_map(|net| {
            let name = net.name().ok()?;
            name.ends_with("_selector").then(|| name.to_string())
        })
        .expect("side-effecting RHS should snapshot its dynamic target selector");
    let effects = procedure_effects(module.procedures().next().unwrap());
    let snapshot = effects
        .iter()
        .position(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, &selector)))
        .unwrap();
    let chosen_update = effects
        .iter()
        .rposition(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "chosen")))
        .unwrap();
    assert!(snapshot < chosen_update);
}

#[test]
fn native_compile_preserves_prefix_and_postfix_update_values() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] p, output logic [3:0] a, y); always_comb begin a = p; y = (a++) + (++a); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 6);
    let SlangExpressionKind::Signal(postfix_result) = effects[1].lhs().unwrap().kind().unwrap()
    else {
        panic!("postfix update should snapshot its old value");
    };
    let SlangExpressionKind::Signal(prefix_result) = effects[3].lhs().unwrap().kind().unwrap()
    else {
        panic!("prefix update should materialize its new value");
    };
    assert!(is_signal(effects[1].rhs().unwrap(), "a"));
    assert!(matches!(
        effects[2].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Add,
            left,
            ..
        } if is_signal(left, postfix_result.name)
    ));
    assert!(is_signal(effects[4].rhs().unwrap(), prefix_result.name));
    assert!(matches!(
        effects[5].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Add,
            left,
            right,
        } if is_signal(left, postfix_result.name)
            && is_signal(right, prefix_result.name)
    ));
}

#[test]
fn native_compile_lowers_struct_pattern_conditions_and_captures() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] opcode, input logic [3:0] payload, input logic [3:0] fallback, output logic hit, output logic [3:0] selected); typedef struct packed { logic [1:0] opcode; logic [3:0] payload; } packet_t; packet_t packet; always_comb begin packet = '{opcode: opcode, payload: payload}; hit = 1'b0; if (packet matches '{opcode: 2'b01, payload: .captured} &&& captured[3]) hit = 1'b1; selected = packet matches '{opcode: 2'b10, payload: .value} ? value : fallback; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert!(module.nets().any(|net| {
        net.is_process_local()
            && net
                .name()
                .is_ok_and(|name| name.starts_with("__opto_pattern_"))
    }));
    assert!(procedure_effects(procedure).len() >= 6);
    let _ = first_branch(procedure);
}

#[test]
fn native_compile_lowers_struct_pattern_case_priority_and_filters() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] opcode, input logic [3:0] payload, output logic [3:0] y); typedef struct packed { logic [1:0] opcode; logic [3:0] payload; } packet_t; packet_t packet; always_comb begin packet = '{opcode: opcode, payload: payload}; case (packet) matches '{opcode: 2'b00, payload: .value} &&& value[3]: y = value; '{opcode: 2'b01, payload: .*}: y = 4'ha; default: y = 4'h5; endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert!(procedure_effects(procedure).len() >= 5);
    let _ = first_branch(procedure);
}

#[test]
fn native_compile_rejects_exact_runtime_unknown_pattern_bits() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] value, output logic y); always_comb begin if (value matches 2'bx1) y = 1'b1; else y = 1'b0; end endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(error.to_string().contains(
        "pattern matching against runtime X/Z state is not synthesizable in the Opto ASIC profile"
    ));
}

#[test]
fn native_compile_lowers_casez_pattern_masks() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic y); always_comb begin casez (value) matches 4'b1z0z: y = 1'b1; default: y = 1'b0; endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let _ = first_branch(module.procedures().next().unwrap());
}

#[test]
fn native_compile_lowers_packed_and_unpacked_tagged_unions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] small_data, input logic [7:0] large_data, output logic [7:0] unpacked_value, output logic packed_hit); typedef union tagged { void Empty; logic [3:0] Small; logic [7:0] Large; } unpacked_t; typedef union tagged packed { void Empty; logic [3:0] Small; logic [7:0] Large; } packed_t; unpacked_t unpacked_union; packed_t packed_union; always_comb begin unpacked_union = tagged Large large_data; packed_union = tagged Small small_data; unpacked_value = '0; packed_hit = 1'b0; if (unpacked_union matches tagged Large .value) unpacked_value = value; if (packed_union matches tagged Small .value) packed_hit = value[0]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let widths = module
        .nets()
        .filter_map(|net| net.name().ok().map(|name| (name.to_string(), net.width())))
        .collect::<std::collections::HashMap<_, _>>();

    assert_eq!(widths["unpacked_union"], 10);
    assert_eq!(widths["packed_union"], 10);
    assert!(module.procedures().next().unwrap().blocks().count() >= 5);
}

#[test]
fn native_compile_lowers_nested_tagged_union_patterns() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] data, output logic [3:0] y); typedef union tagged { logic [3:0] Nibble; logic [7:0] Byte; } inner_t; typedef union tagged { void None; inner_t Some; } outer_t; outer_t value; always_comb begin value = tagged Some (tagged Nibble data); y = '0; if (value matches tagged Some (tagged Nibble .captured)) y = captured; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let value = module
        .nets()
        .find(|net| net.name().is_ok_and(|name| name == "value"))
        .unwrap();

    assert_eq!(value.width(), 10);
    let _ = first_branch(module.procedures().next().unwrap());
}

#[test]
fn native_compile_lowers_replicated_assignment_patterns_in_evaluation_order() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, b, output logic [3:0] values [0:3], output logic [15:0] packed_bits, output logic [3:0] final_state); typedef struct packed { logic [3:0] first; logic [3:0] second; logic [3:0] third; logic [3:0] fourth; } packed_t; packed_t packed_value; integer state; always_comb begin state = 0; values = '{2{state++, state++}}; packed_value = '{2{a, b}}; packed_bits = packed_value; final_state = state; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    assert_eq!(
        module
            .nets()
            .filter(|net| net
                .name()
                .is_ok_and(|name| name.starts_with("__opto_update_")))
            .count(),
        4
    );
}

#[test]
fn native_compile_normalizes_increment_statements() {
    let source = NativeTestSource::new(
        "module top(input logic enable, output logic [3:0] count); always_comb begin count = '0; if (enable) count++; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (_, then_edge, _) = first_branch(procedure);
    let increment = procedure
        .block(then_edge.block)
        .unwrap()
        .effects()
        .next()
        .unwrap();
    let rhs = increment.rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Add,
            ..
        }
    ));
}

#[test]
fn native_compile_normalizes_procedural_assignment_widths() {
    let source = NativeTestSource::new(
        "module top(input logic [11:0] value, output logic [3:0] y); always_comb y = value; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = first_effect(module.procedures().next().unwrap())
        .rhs()
        .unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Cast {
            kind: SlangCastKind::Truncate,
            width: 4,
            ..
        }
    ));
}

#[test]
fn native_compile_lowers_automatic_procedural_variable_initializers() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); always_comb begin automatic logic temporary = a; y = temporary; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 2);
    assert!(is_signal(effects[0].rhs().unwrap(), "a"));
}

#[test]
fn native_compile_rejects_static_procedural_declaration_initialization() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); always_comb begin logic temporary = a; y = temporary; end endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .expect_err("a static declaration initializer is time-zero state");

    let SlangError::LoweringFailed(failure) = error else {
        panic!("expected a structured module-lowering failure, got {error}");
    };
    assert_eq!(
        failure.category,
        SlangLoweringFailureCategory::UnsupportedProfile
    );
    assert_eq!(failure.stable_code(), "OPT-HDL-LP-0001");
    assert!(
        failure
            .message
            .contains("static procedural declaration initialization for 'temporary'")
    );
    let location = failure.location.expect("rejecting declaration has a span");
    assert_eq!(location.path, source.path);
    assert_eq!(location.line, 1);
    assert!(location.column > 0);
}

#[test]
fn native_compile_preserves_procedural_variable_lifetimes() {
    let source = NativeTestSource::new(
        "module top(input logic clk, d, output logic q); always @(posedge clk) begin: state logic stored; automatic logic next; next = d; stored = next; q <= stored; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let locals = module
        .nets()
        .filter_map(|net| {
            let name = net.name().ok()?;
            (name.ends_with("_stored") || name.ends_with("_next"))
                .then_some((name.to_string(), net.is_process_local()))
        })
        .collect::<Vec<_>>();

    assert_eq!(locals.len(), 2);
    assert!(
        locals
            .iter()
            .any(|(name, local)| name.ends_with("_stored") && !local)
    );
    assert!(
        locals
            .iter()
            .any(|(name, local)| name.ends_with("_next") && *local)
    );
}

#[test]
fn native_compile_keeps_same_named_block_locals_distinct() {
    let source = NativeTestSource::new(
        "module top(input logic sel, input logic [3:0] narrow, input logic [11:0] wide, output logic [3:0] y_narrow, output logic [11:0] y_wide); always_comb begin y_narrow = '0; y_wide = '0; case (sel) 1'b0: begin automatic logic [3:0] index = narrow; y_narrow = index; end 1'b1: begin automatic logic [11:0] index = wide; y_wide = index; end endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let locals = module
        .nets()
        .filter_map(|net| {
            let name = net.name().unwrap();
            name.ends_with("_index")
                .then(|| (name.to_string(), net.width()))
        })
        .collect::<Vec<_>>();

    assert_eq!(locals.len(), 2);
    assert_ne!(locals[0].0, locals[1].0);
    assert_eq!(
        locals.iter().map(|(_, width)| *width).collect::<Vec<_>>(),
        [4, 12]
    );
}

#[test]
fn native_compile_lowers_always_comb_if_else() {
    let source = NativeTestSource::new(
        "module top(input logic sel, input logic a, input logic b, output logic y); always_comb begin if (sel) y = a; else y = b; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (_, then_edge, else_edge) = first_branch(procedure);
    assert_eq!(procedure.block(then_edge.block).unwrap().effects().len(), 1);
    assert_eq!(procedure.block(else_edge.block).unwrap().effects().len(), 1);
}

#[test]
fn native_compile_lowers_always_comb_case() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] sel, input logic a, b, c, output logic y); always_comb begin case (sel) 2'b00: y = a; 2'b01, 2'b10: y = b; default: y = c; endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (_, arms, default) = first_switch(procedure);
    let arms = arms.iter().collect::<Vec<_>>();
    assert_eq!(arms.len(), 3);
    assert_eq!(arms[1].edge().unwrap().block, arms[2].edge().unwrap().block);
    assert_eq!(procedure.block(default.block).unwrap().effects().len(), 1);
}

#[test]
fn native_compile_normalizes_casez_to_masked_priority_conditions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] sel, input logic a, b, output logic y); always_comb begin casez (sel) 4'b1???: y = a; 4'b01?0: y = b; default: y = 1'b0; endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (condition, _, else_edge) = first_branch(procedure);
    let SlangExpressionKind::Binary {
        op: SlangBinaryOp::Eq,
        left,
        ..
    } = condition.kind().unwrap()
    else {
        panic!("expected masked casez equality");
    };
    assert!(matches!(
        left.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitAnd,
            ..
        }
    ));
    assert!(matches!(
        procedure
            .block(else_edge.block)
            .unwrap()
            .terminator()
            .kind()
            .unwrap(),
        SlangTerminatorKind::Branch { .. }
    ));
}

#[test]
fn native_compile_normalizes_case_inside_to_priority_conditions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] sel, input logic a, b, output logic y); always_comb begin case (sel) inside 4'b1???: y = a; [4'd2:4'd5]: y = b; default: y = 1'b0; endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (first_condition, _, else_edge) = first_branch(procedure);
    let SlangExpressionKind::Binary {
        op: SlangBinaryOp::Eq,
        left,
        ..
    } = first_condition.kind().unwrap()
    else {
        panic!("expected wildcard equality");
    };
    assert!(matches!(
        left.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitAnd,
            ..
        }
    ));
    let nested = procedure.block(else_edge.block).unwrap().terminator();
    let SlangTerminatorKind::Branch { condition, .. } = nested.kind().unwrap() else {
        panic!("expected second priority branch");
    };
    assert!(matches!(
        condition.kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::LogicalAnd,
            ..
        }
    ));
}

#[test]
fn native_compile_lowers_simple_always_ff() {
    let source = NativeTestSource::new(
        "module top(input logic clk, input logic d, output logic q); always_ff @(posedge clk) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();
    let event = process.events().next().unwrap();

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Flop);
    assert_eq!(event.edge().unwrap(), SlangEdge::Pos);
    assert_eq!(event.signal().unwrap().name, "clk");
    assert_eq!(
        first_effect(process).mode(),
        SlangAssignmentMode::Nonblocking
    );
}

#[test]
fn native_compile_lowers_always_latch() {
    let source = NativeTestSource::new(
        "module top(input logic en, d, output logic q); always_latch if (en) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();

    assert_eq!(process.kind().unwrap(), SlangProcedureKind::Latch);
    assert_eq!(process.events().len(), 0);
    let _ = first_branch(process);
}

#[test]
fn native_compile_lowers_clock_enable_control_flow() {
    let source = NativeTestSource::new(
        "module top(input logic clk, en, d, output logic q); always_ff @(posedge clk) begin if (en) q <= d; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let (_, then_edge, else_edge) = first_branch(procedure);
    assert_eq!(procedure.block(then_edge.block).unwrap().effects().len(), 1);
    assert_eq!(procedure.block(else_edge.block).unwrap().effects().len(), 0);
}

#[test]
fn native_compile_preserves_single_event_iff_on_its_event_identity() {
    let source = NativeTestSource::new(
        "module top(input logic clk, en, d, output logic q); always_ff @(posedge clk iff en) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let events = procedure.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 1);
    assert!(is_signal(events[0].qualifier().unwrap().unwrap(), "en"));
    assert_eq!(
        procedure
            .blocks()
            .map(|block| block.effects().len())
            .sum::<usize>(),
        1
    );
}

#[test]
fn native_compile_preserves_static_and_dynamic_selected_clock_expressions() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] clocks, input logic [1:0] index, d, output logic q_static, q_dynamic); always_ff @(posedge clocks[2]) q_static <= d; always_ff @(negedge clocks[index]) q_dynamic <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedures = module.procedures().collect::<Vec<_>>();
    let static_event = procedures[0].events().next().unwrap();
    let dynamic_event = procedures[1].events().next().unwrap();

    assert!(matches!(
        static_event.expression().unwrap().kind().unwrap(),
        SlangExpressionKind::Signal(signal)
            if signal.name == "clocks"
                && signal.range.is_some_and(|range| range.msb == 2 && range.lsb == 2)
    ));
    assert!(matches!(
        dynamic_event.expression().unwrap().kind().unwrap(),
        SlangExpressionKind::DynamicExtract { value, offset, width }
            if width == 1
                && is_signal(value, "clocks")
                && matches!(
                    offset.kind().unwrap(),
                    SlangExpressionKind::Cast { value, .. } if is_signal(value, "index")
                )
    ));
}

#[test]
fn native_compile_does_not_fold_a_different_bit_as_a_selected_clock_self_qualifier() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] clocks, d, output logic q); always_ff @(posedge clocks[2] iff clocks[1]) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let event = module.procedures().next().unwrap().events().next().unwrap();

    assert!(matches!(
        event.qualifier().unwrap().unwrap().kind().unwrap(),
        SlangExpressionKind::Signal(signal)
            if signal.name == "clocks"
                && signal.range.is_some_and(|range| range.msb == 1 && range.lsb == 1)
    ));
}

#[test]
fn native_compile_lowers_clock_iff_with_unqualified_async_reset() {
    let source = NativeTestSource::new(
        "module top(input logic clk, rst_n, en, d, output logic q); always_ff @(posedge clk iff en or negedge rst_n) if (!rst_n) q <= 1'b0; else q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let events = procedure.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert!(is_signal(events[0].qualifier().unwrap().unwrap(), "en"));
    assert!(events[1].qualifier().unwrap().is_none());
    let _ = first_branch(procedure);
}

#[test]
fn native_compile_canonicalizes_constant_event_iff_qualifiers() {
    let source = NativeTestSource::new(
        "module top(input logic clk, rst_n, ignored, en, d, output logic q); always_ff @(posedge clk iff en or negedge rst_n iff 1'b1 or posedge ignored iff 1'b0) if (!rst_n) q <= 1'b0; else q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let events = procedure.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].signal().unwrap().name, "clk");
    assert_eq!(events[1].signal().unwrap().name, "rst_n");
    assert!(is_signal(events[0].qualifier().unwrap().unwrap(), "en"));
    assert!(events[1].qualifier().unwrap().is_none());
    let _ = first_branch(procedure);
}

#[test]
fn native_compile_canonicalizes_post_edge_event_iff_qualifiers() {
    let source = NativeTestSource::new(
        "module top(input logic clk, rst_n, ignored, en, d, output logic q); always_ff @(posedge clk iff en or negedge rst_n iff !rst_n or posedge ignored iff !ignored) if (!rst_n) q <= 1'b0; else q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let events = procedure.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].signal().unwrap().name, "clk");
    assert_eq!(events[1].signal().unwrap().name, "rst_n");
    assert!(is_signal(events[0].qualifier().unwrap().unwrap(), "en"));
    assert!(events[1].qualifier().unwrap().is_none());
    let _ = first_branch(procedure);
}

#[test]
fn native_compile_rejects_an_event_list_eliminated_by_constant_iff() {
    let source = NativeTestSource::new(
        "module top(input logic clk, d, output logic q); always_ff @(posedge clk iff 1'b0) q <= d; endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("no reachable event after constant iff qualification"),
        "{error}"
    );
}

#[test]
fn native_compile_preserves_multiple_event_specific_iff_qualifiers() {
    let source = NativeTestSource::new(
        "module top(input logic clk_a, clk_b, en_a, en_b, d, output logic q); always_ff @(posedge clk_a iff en_a or negedge clk_b iff en_b) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let events = module
        .procedures()
        .next()
        .unwrap()
        .events()
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].signal().unwrap().name, "clk_a");
    assert!(is_signal(events[0].qualifier().unwrap().unwrap(), "en_a"));
    assert_eq!(events[1].signal().unwrap().name, "clk_b");
    assert!(is_signal(events[1].qualifier().unwrap().unwrap(), "en_b"));
}

#[test]
fn native_compile_lowers_independently_qualified_dual_edges() {
    let source = NativeTestSource::new(
        "module top(input logic clk, pos_enable, neg_enable, d, output logic q); always_ff @(posedge clk iff pos_enable or negedge clk iff neg_enable) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let events = procedure.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert!(is_signal(
        events[0].qualifier().unwrap().unwrap(),
        "pos_enable"
    ));
    assert!(is_signal(
        events[1].qualifier().unwrap().unwrap(),
        "neg_enable"
    ));
}

#[test]
fn native_compile_lowers_predeclared_for_loops_with_break_to_cyclic_cfg() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [2:0] index, output logic missed); always_comb begin int i; index = '0; for (i = 0; i < 4; i++) begin if (value[i]) begin index = i; break; end end missed = i == 4; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let nets = module
        .nets()
        .map(|net| (net.name().unwrap().to_string(), net.is_signed()))
        .collect::<Vec<_>>();

    assert!(
        nets.iter()
            .any(|(name, signed)| name.starts_with("__opto_local_")
                && name.ends_with("_i")
                && *signed)
    );
    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert!(!nets.iter().any(|(name, _)| name.ends_with("_broken")));
    assert!(procedure.blocks().any(|block| matches!(
        block.terminator().kind().unwrap(),
        SlangTerminatorKind::Jump(edge)
            if edge.block == procedure.loop_regions().next().unwrap().exit()
    )));
}

#[test]
fn native_compile_lowers_for_loops_with_omitted_clauses() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] data, skip, output logic [3:0] mask, checksum, output logic [2:0] count); always_comb begin integer i; i = 0; mask = '0; checksum = '0; for (; i < 4;) begin i++; if (skip[i - 1]) continue; mask[i - 1] = data[i - 1]; end for (i = 0; i < 4; i++, checksum += i) begin end count = i[2:0]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert!(
        effects
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "mask")))
            .count()
            >= 1
    );
    assert_eq!(
        effects
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "checksum")))
            .count(),
        2,
        "the initializer and source step are each lowered once"
    );
    assert_cyclic_region_count(module.procedures().next().unwrap(), 2);
}

#[test]
fn native_compile_proves_for_loop_without_stop_condition() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] stop, data, output logic [3:0] mask, output logic [2:0] count); always_comb begin integer i; i = 0; mask = '0; for (i = 0;; i++) begin if (stop[i] || i == 4) break; mask[i] = data[i]; end count = i[2:0]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_cyclic_region_count(module.procedures().next().unwrap(), 1);
    assert!(
        !module
            .nets()
            .any(|net| net.name().is_ok_and(|name| name.ends_with("_broken")))
    );

    let expression_free = NativeTestSource::new(
        "module top(input logic value, output logic y); always_comb begin for (;;) break; y = value; end endmodule\n",
    );
    let compilation = compile_source(&expression_free);
    let module = first_module(&compilation);
    assert!(
        procedure_effects(module.procedures().next().unwrap())
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
    );
}

#[test]
fn native_compile_tracks_multiple_for_induction_variables() {
    let source = NativeTestSource::new(
        "module top(output logic [3:0] result); always_comb begin integer i; integer j; i = 0; j = 4; for (i = 0; i < j; i++, j--) begin end result = i + j; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(
        procedure_effects(module.procedures().next().unwrap())
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "result")))
    );
}

#[test]
fn native_compile_preserves_runtime_for_initializer_effects() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] data, input logic [4:0] seed, output logic [4:0] result); always_comb begin integer i; logic [4:0] sum; for (i = 0, sum = seed; i < 4; i++) sum += data[i]; result = sum; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(
        effects
            .iter()
            .filter(|effect| {
                effect.lhs().is_ok_and(|lhs| {
                    matches!(lhs.kind(), Ok(SlangExpressionKind::Signal(signal)) if signal.name.contains("sum"))
                })
            })
            .count(),
        2,
        "the native adapter emits the runtime initializer and source body once; Rust owns copy-back"
    );
    assert_cyclic_region_count(module.procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_a_for_continue_path_for_rust_boundedness_analysis() {
    let source = NativeTestSource::new(
        "module top(output logic y); always_comb begin integer i; i = 0; y = 0; for (; i < 4;) begin continue; i++; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_lowers_constant_repeat_body_once() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin y = a; repeat (3) y = y + 1'b1; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let procedure = module.procedures().next().unwrap();
    let regions = procedure.loop_regions().collect::<Vec<_>>();
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].form().unwrap(), SlangLoopForm::PreTest);
    assert_eq!(regions[0].parent(), None);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
            .count(),
        2,
        "the source assignment must occur once in the cyclic body"
    );
    assert!(!module.nets().any(|net| {
        net.name()
            .is_ok_and(|name| name.starts_with("__opto_repeat_"))
    }));
}

#[test]
fn native_compile_nests_cyclic_repeat_regions_without_relowering_bodies() {
    let source = NativeTestSource::new(
        "module top(output logic [3:0] y); always_comb begin y = 0; repeat (2) repeat (3) y++; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let regions = procedure.loop_regions().collect::<Vec<_>>();

    assert_eq!(regions.len(), 2);
    assert_eq!(regions[0].parent(), None);
    assert_eq!(regions[1].parent().unwrap().index(), 0);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
            .count(),
        2,
        "the nested source body must still be lowered only once"
    );
}

#[test]
fn native_compile_lowers_break_in_constant_repeat_loops() {
    let source = NativeTestSource::new(
        "module top(input logic stop, output logic [2:0] y); always_comb begin y = 0; repeat (3) begin y = y + 1'b1; if (stop && y[0]) break; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(!module.nets().any(|net| {
        let name = net.name().unwrap();
        name.starts_with("__opto_loop_") && name.ends_with("_broken")
    }));
    let procedure = module.procedures().next().unwrap();
    let region = procedure.loop_regions().next().unwrap();
    assert_eq!(procedure.loop_regions().len(), 1);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
            .count(),
        2,
        "the source assignment must occur once in the cyclic body"
    );
    assert!(procedure.blocks().any(|block| {
        matches!(
            block.terminator().kind().unwrap(),
            SlangTerminatorKind::Jump(edge) if edge.block == region.exit()
        )
    }));
}

#[test]
fn native_compile_lowers_continue_in_constant_repeat_loops() {
    let source = NativeTestSource::new(
        "module top(input logic skip, output logic [3:0] y); always_comb begin y = 0; repeat (3) begin y = y + 1'b1; if (skip && y[0]) continue; y = y + 2'd2; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(!module.nets().any(|net| {
        let name = net.name().unwrap();
        name.starts_with("__opto_loop_") && name.ends_with("_continued")
    }));
    let procedure = module.procedures().next().unwrap();
    let region = procedure.loop_regions().next().unwrap();
    assert_eq!(procedure.loop_regions().len(), 1);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
            .count(),
        3,
        "each source assignment must occur once in the cyclic body"
    );
    assert!(procedure.blocks().any(|block| matches!(
        block.terminator().kind().unwrap(),
        SlangTerminatorKind::Branch { then_edge, .. }
            if then_edge.block != region.exit()
                && then_edge.block != region.header()
                && then_edge.block != region.body()
                && then_edge.block != region.latch()
    )));
}

#[test]
fn native_compile_lowers_lexical_named_block_disable() {
    let source = NativeTestSource::new(
        "module top(input logic stop_inner, stop_outer, output logic [7:0] y); always_comb begin y = 0; begin : outer y = y + 1; begin : inner y = y + 2; if (stop_inner) disable inner; y = y + 4; if (stop_outer) disable outer; y = y + 8; end y = y + 16; end y = y + 32; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let controls = module
        .nets()
        .filter_map(|net| net.name().ok())
        .filter(|name| name.starts_with("__opto_disable_"))
        .collect::<Vec<_>>();

    assert_eq!(controls.len(), 2);
    let effects = procedure_effects(module.procedures().next().unwrap());
    assert!(effects.iter().any(|effect| {
        effect.lhs().is_ok_and(|lhs| is_signal(lhs, controls[0]))
            && matches!(
                effect.rhs().and_then(SlangExpression::kind),
                Ok(SlangExpressionKind::Constant(value)) if value.bits == "1"
            )
    }));
    assert!(effects.iter().any(|effect| {
        effect.lhs().is_ok_and(|lhs| is_signal(lhs, controls[1]))
            && matches!(
                effect.rhs().and_then(SlangExpression::kind),
                Ok(SlangExpressionKind::Constant(value)) if value.bits == "1"
            )
    }));
}

#[test]
fn native_compile_preserves_outer_disable_in_a_single_cyclic_body() {
    let source = NativeTestSource::new(
        "module top(input logic stop, output logic [7:0] y); always_comb begin y = 0; begin : outer for (int i = 0; i < 4; i++) begin y = y + 1; if (stop && i == 1) disable outer; y = y + 2; end y = y + 16; end y = y + 32; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(
        module
            .nets()
            .filter_map(|net| net.name().ok())
            .filter(|name| name.starts_with("__opto_disable_"))
            .count(),
        1
    );
    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
            .count(),
        5,
        "source assignments are lowered once before Rust clones iterations"
    );
}

#[test]
fn native_compile_lowers_statically_terminating_forever_loops_to_cyclic_cfg() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] value, output logic [3:0] y, output logic [2:0] count); always_comb begin integer i; i = 0; y = '0; forever begin if (i == 4) break; y[i] = value[i]; i++; end count = i[2:0]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert!(
        !module
            .nets()
            .any(|net| net.name().is_ok_and(|name| name.ends_with("_broken")))
    );
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| matches!(
                lhs.kind(),
                Ok(SlangExpressionKind::DynamicExtract { .. })
            )))
            .count(),
        1
    );
}

#[test]
fn native_compile_preserves_runtime_early_break_in_bounded_forever_loops() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] stop, output logic [3:0] mask, output logic [2:0] count); always_comb begin integer i; i = 0; mask = '0; forever begin if (stop[i] || i == 4) break; mask[i] = 1'b1; i++; end count = i[2:0]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_cyclic_region_count(procedure, 1);
    assert!(
        !module
            .nets()
            .any(|net| net.name().is_ok_and(|name| name.ends_with("_broken")))
    );
    assert!(
        procedure_effects(procedure)
            .iter()
            .any(|effect| { effect.lhs().is_ok_and(|lhs| is_signal(lhs, "count")) })
    );
}

#[test]
fn native_compile_lowers_single_iteration_forever_loop() {
    let source = NativeTestSource::new(
        "module top(input logic value, output logic y); always_comb forever begin y = value; break; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(
        procedure_effects(module.procedures().next().unwrap())
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
    );

    let empty_loop = NativeTestSource::new(
        "module top(input logic value, output logic y); always_comb begin forever break; y = value; end endmodule\n",
    );
    let compilation = compile_source(&empty_loop);
    let module = first_module(&compilation);
    assert!(
        procedure_effects(module.procedures().next().unwrap())
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
    );
}

#[test]
fn native_compile_proves_all_runtime_forever_branches_break() {
    let source = NativeTestSource::new(
        "module top(input logic select, value, output logic y); always_comb forever begin y = value; if (select) break; else break; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert!(
        procedure_effects(module.procedures().next().unwrap())
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
    );
}

#[test]
fn native_compile_proves_all_runtime_forever_case_branches_break() {
    let sources = [
        "module top(input logic [1:0] select, output logic y); always_comb forever begin y = 1'b1; case (select) 2'b00: break; 2'b01: break; default: break; endcase end endmodule\n",
        "module top(input logic [1:0] select, output logic y); always_comb forever begin y = 1'b1; casez (select) 2'b0?: break; default: break; endcase end endmodule\n",
        "module top(input logic [1:0] select, output logic y); always_comb forever begin y = 1'b1; case (select) inside [0:1]: break; default: break; endcase end endmodule\n",
        "module top(input logic opcode, payload, output logic y); typedef struct packed { logic opcode; logic payload; } packet_t; packet_t packet; always_comb begin packet = '{opcode: opcode, payload: payload}; forever begin y = 1'b1; case (packet) matches '{opcode: 1'b0, payload: .*}: break; default: break; endcase end end endmodule\n",
    ];

    for text in sources {
        let source = NativeTestSource::new(text);
        let compilation = compile_source(&source);
        let module = first_module(&compilation);
        assert!(
            procedure_effects(module.procedures().next().unwrap())
                .iter()
                .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
        );
    }
}

#[test]
fn native_compile_proves_current_function_returns_complete_unbounded_loops() {
    let sources = [
        "module top(input logic select, output logic [3:0] y); function automatic logic [3:0] choose(input logic pick); forever begin if (pick) return 4'd3; else return 4'd5; end endfunction always_comb y = choose(select); endmodule\n",
        "module top(input logic select, output logic [3:0] y); function automatic logic [3:0] choose(input logic pick); for (;;) begin if (pick) return 4'd7; else return 4'd9; end endfunction always_comb y = choose(select); endmodule\n",
        "module top(input logic select, output logic [3:0] y); function automatic logic [3:0] choose(input logic pick); forever begin for (;;) begin if (pick) return 4'd11; else return 4'd13; end end endfunction always_comb y = choose(select); endmodule\n",
        "module top(input logic select, output logic [3:0] y); function automatic logic [3:0] choose(input logic pick); while (pick) return 4'd2; return 4'd4; endfunction always_comb y = choose(select); endmodule\n",
        "module top(input logic select, output logic [3:0] y); function automatic logic [3:0] choose(input logic pick); forever do begin if (pick) return 4'd6; else return 4'd8; end while (pick); endfunction always_comb y = choose(select); endmodule\n",
    ];

    for text in sources {
        let source = NativeTestSource::new(text);
        let compilation = compile_source(&source);
        let module = first_module(&compilation);
        assert!(
            procedure_effects(module.procedures().next().unwrap())
                .iter()
                .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
        );
    }
}

#[test]
fn native_compile_proves_enclosing_disable_completes_forever_loop() {
    let source = NativeTestSource::new(
        "module top(input logic select, output logic [4:0] y); always_comb begin y = 0; begin : outer forever begin y = y + 1'b1; if (select) disable outer; else disable outer; end y = y + 4'd8; end y = y + 5'd16; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(
        module
            .nets()
            .filter_map(|net| net.name().ok())
            .filter(|name| name.starts_with("__opto_disable_"))
            .count(),
        1
    );
}

#[test]
fn inner_activation_exits_do_not_prove_outer_forever_termination() {
    let sources = [
        "module top(input logic select, output logic [3:0] y); function automatic logic [3:0] choose(input logic pick); if (pick) return 4'd1; return 4'd2; endfunction always_comb forever y = choose(select); endmodule\n",
        "module top(output logic y); always_comb forever begin : inner y = 1'b1; disable inner; end endmodule\n",
    ];

    for text in sources {
        let source = NativeTestSource::new(text);
        let error = compile(
            std::slice::from_ref(&source.path),
            &SlangCompileOptions::default(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("procedural forever loop requires a lexically contained"),
            "{error}"
        );
    }
}

#[test]
fn native_compile_preserves_non_exhaustive_forever_case_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(input logic select, output logic y); always_comb forever begin y = 1'b1; case (select) 1'b0: break; endcase end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_forever_case_continue_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(input logic select, output logic y); always_comb forever begin y = 1'b1; case (select) 1'b0: continue; default: break; endcase break; end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_runtime_only_forever_break_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(input logic stop, output logic y); always_comb begin y = 0; forever begin if (stop) break; y = 1; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_repeating_forever_state_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(output logic y); always_comb begin integer i; i = 0; y = 0; forever begin if (i == 3) break; y = ~y; i = i ^ 1; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_unreachable_forever_break_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(input logic value, output logic y); always_comb forever begin y = value; continue; break; end endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("procedural loop has no structurally reachable exit"),
        "{error}"
    );
}

#[test]
fn native_compile_copies_task_outputs_after_self_disable() {
    let source = NativeTestSource::new(
        "module top(input logic stop, output logic [3:0] y); task automatic leave(output logic [3:0] target, input logic halt); target = 1; if (halt) disable leave; target = 2; endtask always_comb leave(y, stop); endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    assert!(module.nets().any(|net| {
        net.name()
            .is_ok_and(|name| name.starts_with("__opto_disable_"))
    }));
    let effects = procedure_effects(module.procedures().next().unwrap());
    let copy_out = effects
        .iter()
        .find(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
        .expect("task output must be copied after the task activation exits");
    assert!(matches!(
        copy_out.rhs().and_then(SlangExpression::kind),
        Ok(SlangExpressionKind::Signal(signal)) if signal.name.contains("_leave_target")
    ));
}

#[test]
fn native_compile_rejects_hierarchical_disable() {
    let source = NativeTestSource::new(
        "module top(output logic y); task automatic leave; y = 1; endtask always_comb begin y = 0; disable top.leave; end endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("hierarchical disable is not supported in synthesizable procedures"),
        "{error}"
    );
}

#[test]
fn native_compile_lowers_static_foreach_body_once() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin y = '0; foreach (a[i]) y[i] = a[i]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_cyclic_region_count(module.procedures().next().unwrap(), 1);
    assert_eq!(
        effects
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| matches!(
                lhs.kind(),
                Ok(SlangExpressionKind::DynamicExtract { .. })
            )))
            .count(),
        1,
        "foreach preserves one dynamic source body for Rust elimination"
    );
}

#[test]
fn native_compile_lowers_statically_terminating_while_body_once() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin integer i; i = 0; y = '0; while (i < 4) begin y[i] = a[i]; i++; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_cyclic_region_count(module.procedures().next().unwrap(), 1);
    assert_eq!(
        effects
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| matches!(
                lhs.kind(),
                Ok(SlangExpressionKind::DynamicExtract { .. })
            )))
            .count(),
        1
    );
}

#[test]
fn native_compile_lowers_while_inside_runtime_branch_once() {
    let source = NativeTestSource::new(
        "module top(input logic enable, input logic [3:0] a, output logic [3:0] y); always_comb begin y = '0; if (enable) begin integer i; i = 0; while (i < 4) begin y[i] = a[i]; i++; end end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")
                || matches!(lhs.kind(), Ok(SlangExpressionKind::DynamicExtract { .. }))))
            .count(),
        2
    );
}

#[test]
fn native_compile_lowers_statically_terminating_do_while_body_once() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [3:0] y); always_comb begin integer i; i = 0; y = '0; do begin y[i] = a[i]; i++; end while (i < 4); end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert_eq!(
        procedure.loop_regions().next().unwrap().form().unwrap(),
        SlangLoopForm::PostTest
    );
}

#[test]
fn native_compile_expands_module_scope_loop_state() {
    let sources = [
        "module top(input logic [3:0] a, keep, output logic [3:0] y, output logic [2:0] count); integer i; integer limit; always_comb begin i = 0; limit = 4; y = '0; while (i < limit && keep[i]) begin y[i] = a[i]; i++; end count = i; end endmodule\n",
        "module top(input logic [3:0] a, keep, output logic [3:0] y, output logic [2:0] count); integer i; integer remaining; always_comb begin i = 0; remaining = 4; y = '0; do begin y[i] = a[i]; i++; remaining--; end while (remaining > 0 && keep[i]); count = i; end endmodule\n",
        "module top(input logic [3:0] a, keep, output logic [3:0] y, output logic [2:0] count); integer i; integer limit; always_comb begin i = 0; limit = 4; y = '0; for (; i < limit; i++) begin if (!keep[i]) break; y[i] = a[i]; end count = i; end endmodule\n",
        "module top(input logic [3:0] a, keep, output logic [3:0] y, output logic [2:0] count); integer i; integer limit; always_comb begin i = 0; limit = 4; y = '0; forever begin if (i == limit || !keep[i]) break; y[i] = a[i]; i++; end count = i; end endmodule\n",
        "module top(input logic [3:0] a, output logic [2:0] count); integer i; always_comb begin i = a; i = 0; while (i < 4) i++; count = i; end endmodule\n",
        "module top(input logic [3:0] a, output logic [2:0] count); integer i; integer runtime_limit; always_comb begin i = 0; runtime_limit = 4; runtime_limit = a; while (i < 4 && runtime_limit != 0) i++; count = i; end endmodule\n",
    ];

    for text in sources {
        let source = NativeTestSource::new(text);
        let compilation = compile_source(&source);
        let module = first_module(&compilation);
        let procedure = module.procedures().next().unwrap();
        let effects = procedure_effects(procedure);
        assert!(
            effects
                .iter()
                .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "count")))
        );
        assert!(effects.iter().any(|effect| {
            effect.lhs().is_ok_and(|lhs| {
                matches!(lhs.kind(), Ok(SlangExpressionKind::Signal(signal)) if signal.name == "i" || signal.name == "remaining")
            })
        }));
    }
}

#[test]
fn native_compile_isolates_shared_module_loop_variables_per_procedure() {
    let source = NativeTestSource::new(
        "module top(input logic clk, input logic [3:0] a, output logic [3:0] x, y); integer k; always @(posedge clk) begin for (k = 0; k < 2; k++) x[k] <= a[k]; x[3:2] <= k; end always @* begin for (k = 0; k < 4; k++) y[k] = a[k]; y[3:2] = k; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedures = module.procedures().collect::<Vec<_>>();

    assert_eq!(procedures.len(), 2);
    for procedure in procedures {
        let effects = procedure_effects(procedure);
        assert!(
            effects
                .iter()
                .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "k")))
        );
    }
}

#[test]
fn native_compile_keeps_mutated_function_arguments_as_runtime_loop_data() {
    let sources = [
        "module top(input logic [3:0] value, output logic [2:0] y); function automatic logic [2:0] count_while(input logic [3:0] remaining); integer i = 0; while (i < 4 && remaining != 0) begin i++; remaining--; end return i[2:0]; endfunction always_comb y = count_while(value); endmodule\n",
        "module top(input logic [3:0] value, output logic [2:0] y); function automatic logic [2:0] count_for(input logic [3:0] remaining); integer result = 0; for (integer i = 0; i < 4 && remaining != 0; i++, remaining--) result++; return result[2:0]; endfunction always_comb y = count_for(value); endmodule\n",
    ];

    for text in sources {
        let source = NativeTestSource::new(text);
        let compilation = compile_source(&source);
        let module = first_module(&compilation);
        assert!(
            procedure_effects(module.procedures().next().unwrap())
                .iter()
                .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
        );
    }
}

#[test]
fn native_compile_ends_static_state_lifetime_after_each_loop_range() {
    let sources = [
        "module top(input logic [4:0] data, output logic [4:0] y); integer i; always_comb begin i = 0; while (i < 4) i++; i = data; y = i[4:0]; end endmodule\n",
        "module top(input logic [4:0] data, output logic [4:0] y); integer i; always_comb begin i = 0; while (i < 4) i++; i += data; y = i[4:0]; end endmodule\n",
        "module top(input logic bit_value, output logic [4:0] y); integer i; always_comb begin i = 0; while (i < 4) i++; i[0] = bit_value; y = i[4:0]; end endmodule\n",
        "module top(input logic [4:0] data, output logic [4:0] first, runtime_value, second); integer i; always_comb begin i = 0; while (i < 2) i++; first = i[4:0]; i = data; runtime_value = i[4:0]; i = 0; while (i < 3) i++; second = i[4:0]; end endmodule\n",
    ];

    for text in sources {
        let source = NativeTestSource::new(text);
        let compilation = compile_source(&source);
        let module = first_module(&compilation);
        assert!(!procedure_effects(module.procedures().next().unwrap()).is_empty());
    }
}

#[test]
fn native_compile_preserves_bounded_runtime_while_condition() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] keep, output logic [2:0] count); always_comb begin integer i; i = 0; while (i < 4 && keep[i]) i++; count = i; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_cyclic_region_count(procedure, 1);
    assert!(procedure.blocks().any(|block| matches!(
        block.terminator().kind().unwrap(),
        SlangTerminatorKind::Branch { .. }
    )));
}

#[test]
fn native_compile_preserves_bounded_runtime_do_while_condition() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] keep, output logic [2:0] count); always_comb begin integer i; i = 0; do i++; while (i < 4 && keep[i]); count = i; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_cyclic_region_count(procedure, 1);
    assert_eq!(
        procedure.loop_regions().next().unwrap().form().unwrap(),
        SlangLoopForm::PostTest
    );
}

#[test]
fn native_compile_preserves_bounded_runtime_for_condition_and_continue() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] keep, skip, output logic [2:0] count, output logic [3:0] mask); always_comb begin integer i; i = 0; mask = '0; for (i = 0; i < 4 && keep[i]; i++) begin if (skip[i]) continue; mask[i] = 1'b1; end count = i; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_cyclic_region_count(procedure, 1);
    assert!(
        !module
            .nets()
            .any(|net| net.name().is_ok_and(|name| name.ends_with("_continued")))
    );
}

#[test]
fn native_compile_materializes_while_induction_state_for_runtime_break() {
    let source = NativeTestSource::new(
        "module top(input logic stop, output logic [2:0] y); always_comb begin integer i; i = 0; while (i < 4) begin if (stop) break; i++; end y = i; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert!(
        !module
            .nets()
            .any(|net| net.name().is_ok_and(|name| name.ends_with("_broken")))
    );
    assert!(
        effects
            .iter()
            .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
    );
}

#[test]
fn native_compile_lowers_continue_after_a_static_while_transition() {
    let source = NativeTestSource::new(
        "module top(input logic skip, output logic [3:0] y); always_comb begin integer i; i = 0; y = '0; while (i < 4) begin i++; if (skip) continue; y[i - 1] = 1'b1; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_cyclic_region_count(module.procedures().next().unwrap(), 1);
    assert!(
        !module
            .nets()
            .any(|net| net.name().is_ok_and(|name| name.ends_with("_continued")))
    );
}

#[test]
fn native_compile_preserves_while_continue_without_progress_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(input logic skip, output logic [3:0] y); always_comb begin integer i; i = 0; y = '0; while (i < 4) begin if (skip) continue; i++; y[i - 1] = 1'b1; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_runtime_controlled_while_transition() {
    let source = NativeTestSource::new(
        "module top(input logic choose, output logic [2:0] y); always_comb begin integer i; i = 0; while (i < 4) begin if (choose) i++; else i += 2; end y = i; end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_preserves_repeating_while_state_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(output logic y); always_comb begin integer i; i = 0; y = 0; while (i < 2) begin y = ~y; i ^= 1; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn native_compile_propagates_nested_updates_to_outer_induction_state() {
    let source = NativeTestSource::new(
        "module top(output logic [2:0] while_value, for_value); always_comb begin integer i; i = 0; while (i < 4) begin while (i < 2) i++; i++; end while_value = i; begin integer k; k = 0; for (; k < 3;) begin for (integer j = 0; j < 1; j++) k++; end for_value = k; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_cyclic_region_count(module.procedures().next().unwrap(), 4);
    for output in ["while_value", "for_value"] {
        assert!(
            effects
                .iter()
                .any(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, output)))
        );
    }
}

#[test]
fn native_compile_preserves_bounded_runtime_repeat_counts() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] initial_count, output logic [3:0] y); always_comb begin logic [3:0] count; count = initial_count; y = 0; repeat (count) begin y++; count = 0; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let procedure = module.procedures().next().unwrap();
    assert_cyclic_region_count(procedure, 1);
    assert_eq!(
        procedure_effects(procedure)
            .iter()
            .filter(|effect| effect.lhs().is_ok_and(|lhs| is_signal(lhs, "y")))
            .count(),
        2
    );
}

#[test]
fn native_compile_leaves_wide_runtime_repeat_size_policy_to_rust() {
    let source = NativeTestSource::new(
        "module top(input logic [31:0] count, output logic y); always_comb begin y = 0; repeat (count) y = ~y; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_cyclic_region_count(procedure, 1);
}

#[test]
fn native_compile_does_not_apply_a_second_repeat_expansion_limit() {
    let source = NativeTestSource::new(
        "module top(output logic y); always_comb begin y = 0; repeat (65537) y = ~y; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_cyclic_region_count(procedure, 1);
}

#[test]
fn native_compile_lowers_verilog_always_forms() {
    let source = NativeTestSource::new(
        "module top(input wire clk, input wire d, input wire a, output reg q, y); always @(posedge clk) q <= d; always @* y = a; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let processes = module.procedures().collect::<Vec<_>>();

    assert_eq!(processes[0].kind().unwrap(), SlangProcedureKind::Flop);
    assert_eq!(
        processes[0].events().next().unwrap().edge().unwrap(),
        SlangEdge::Pos
    );
    assert_eq!(
        processes[1].kind().unwrap(),
        SlangProcedureKind::CombOrLatch
    );
    assert_eq!(processes[1].events().len(), 0);
}

#[test]
fn native_compile_preserves_async_reset_event_lists() {
    let source = NativeTestSource::new(
        "module top(input logic clk, input logic rst_n, input logic d, output logic q); always_ff @(posedge clk or negedge rst_n) begin if (!rst_n) q <= 1'b0; else q <= d; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let process = module.procedures().next().unwrap();
    let events = process.events().collect::<Vec<_>>();

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].edge().unwrap(), SlangEdge::Pos);
    assert_eq!(events[0].signal().unwrap().name, "clk");
    assert_eq!(events[1].edge().unwrap(), SlangEdge::Neg);
    assert_eq!(events[1].signal().unwrap().name, "rst_n");
    let _ = first_branch(process);
}

#[test]
fn native_compile_normalizes_nonblocking_always_comb_assignment() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); always_comb begin y <= a; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    assert_eq!(
        first_effect(module.procedures().next().unwrap()).mode(),
        SlangAssignmentMode::Blocking
    );
}

#[test]
fn native_compile_ignores_synthesis_assignment_delays() {
    let source = NativeTestSource::new(
        "module top(input logic clk, input logic d, output logic q); always_ff @(posedge clk) q <= #1 d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    assert_eq!(
        first_effect(module.procedures().next().unwrap()).mode(),
        SlangAssignmentMode::Nonblocking
    );
}

#[test]
fn cfg_if_branches_rejoin_the_same_block() {
    let source = NativeTestSource::new(
        "module top(input logic sel, a, b, output logic y); always_comb if (sel) y = a; else y = b; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let entry = entry_block(procedure);
    let SlangTerminatorKind::Branch {
        then_edge,
        else_edge,
        ..
    } = entry.terminator().kind().unwrap()
    else {
        panic!("entry should branch");
    };
    let jump_target = |edge: SlangEdgeTarget<'_>| {
        let block = procedure.block(edge.block).unwrap();
        let SlangTerminatorKind::Jump(join) = block.terminator().kind().unwrap() else {
            panic!("branch body should jump to its join");
        };
        join.block
    };

    assert_eq!(jump_target(then_edge), jump_target(else_edge));
}

#[test]
fn cfg_switch_arms_preserve_source_order() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] sel, output logic [1:0] y); always_comb case (sel) 2'b00: y = 0; 2'b01, 2'b10: y = 1; default: y = 2; endcase endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let (_, arms, _) = first_switch(module.procedures().next().unwrap());
    let patterns = arms
        .iter()
        .map(|arm| match arm.pattern().unwrap().kind().unwrap() {
            SlangExpressionKind::Constant(value) => value.bits.to_string(),
            _ => panic!("switch pattern should be constant"),
        })
        .collect::<Vec<_>>();

    assert_eq!(patterns, ["00", "01", "10"]);
}

#[test]
fn cyclic_loops_publish_only_region_described_backedges() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, output logic [7:0] y); always_comb begin integer i; y = '0; for (i = 0; i < 2; i++) y += a; repeat (2) y++; foreach (a[j]) y[j] = a[j]; i = 0; while (i < 2) begin y++; i++; end i = 0; do begin y++; i++; end while (i < 2); forever begin y++; break; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();
    let backedges = procedure
        .loop_regions()
        .map(|region| (region.latch().index(), region.header().index()))
        .collect::<Vec<_>>();
    assert_eq!(backedges.len(), 5, "every nontrivial source loop is cyclic");
    let block_count = procedure.blocks().len();
    let mut successors = vec![Vec::new(); block_count];
    let mut indegree = vec![0_usize; block_count];

    for block in procedure.blocks() {
        let targets = match block.terminator().kind().unwrap() {
            SlangTerminatorKind::Return => Vec::new(),
            SlangTerminatorKind::Jump(edge) => vec![edge.block],
            SlangTerminatorKind::Branch {
                then_edge,
                else_edge,
                ..
            } => vec![then_edge.block, else_edge.block],
            SlangTerminatorKind::Switch { arms, default, .. } => arms
                .iter()
                .map(|arm| arm.edge().unwrap().block)
                .chain(std::iter::once(default.block))
                .collect(),
        };
        for target in targets {
            assert!(target.index() < block_count, "CFG edge target must exist");
            if backedges.contains(&(block.id().index(), target.index())) {
                continue;
            }
            successors[block.id().index()].push(target.index());
            indegree[target.index()] += 1;
        }
    }

    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(block, degree)| (*degree == 0).then_some(block))
        .collect::<Vec<_>>();
    let mut visited = 0_usize;
    while let Some(block) = ready.pop() {
        visited += 1;
        for &target in &successors[block] {
            indegree[target] -= 1;
            if indegree[target] == 0 {
                ready.push(target);
            }
        }
    }

    assert_eq!(
        visited, block_count,
        "removing declared loop-region backedges must leave an acyclic CFG"
    );
}

#[test]
fn cfg_views_carry_source_locations() {
    let source = NativeTestSource::new(
        "module top(input logic clk, d, output logic q); always_ff @(posedge clk) q <= d; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert!(procedure.source().unwrap().line.is_some());
    assert!(entry_block(procedure).source().unwrap().line.is_some());
    assert!(first_effect(procedure).source().unwrap().line.is_some());
    assert!(
        procedure
            .events()
            .next()
            .unwrap()
            .source()
            .unwrap()
            .line
            .is_some()
    );
}

#[test]
fn deeply_nested_control_materializes_as_flat_blocks() {
    // Slang accepts this source shape up to 128 nested blocks.
    const DEPTH: usize = 128;
    let mut text = String::from(
        "module top(input logic enable, input logic value, output logic y); always_comb begin y = 0; ",
    );
    text.push_str(&"if (enable) begin ".repeat(DEPTH));
    text.push_str("y = value;");
    text.push_str(&" end".repeat(DEPTH));
    text.push_str(" end endmodule\n");
    let source = NativeTestSource::new(&text);
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let procedure = module.procedures().next().unwrap();

    assert_eq!(procedure.blocks().len(), 1 + DEPTH * 2 + 1);
    assert_eq!(procedure_effects(procedure).len(), 2);
}

#[test]
fn native_compile_preserves_unbounded_runtime_while_for_rust_proof() {
    let source = NativeTestSource::new(
        "module top(input logic enable, output logic y); always_comb while (enable) y = 1; endmodule\n",
    );
    let compilation = compile_source(&source);
    assert_cyclic_region_count(first_module(&compilation).procedures().next().unwrap(), 1);
}

#[test]
fn expression_kind_inventory_covers_every_slang_node() {
    assert_semantic_inventory(
        ast_visitor_case_kinds("switch (expr->kind)"),
        &[
            "Invalid",
            "IntegerLiteral",
            "UnbasedUnsizedIntegerLiteral",
            "NamedValue",
            "HierarchicalValue",
            "UnaryOp",
            "BinaryOp",
            "ConditionalOp",
            "Inside",
            "Assignment",
            "Concatenation",
            "Replication",
            "Streaming",
            "ElementSelect",
            "RangeSelect",
            "MemberAccess",
            "Call",
            "Conversion",
            "LValueReference",
            "SimpleAssignmentPattern",
            "StructuredAssignmentPattern",
            "ReplicatedAssignmentPattern",
            "TaggedUnion",
            // These only appear inside a surrounding lowered expression.
            "EmptyArgument",
            "ValueRange",
        ],
        &[
            "RealLiteral",
            "TimeLiteral",
            "NullLiteral",
            "UnboundedLiteral",
            "StringLiteral",
            "DataType",
            "TypeReference",
            "ArbitrarySymbol",
            "Dist",
            "NewArray",
            "NewClass",
            "NewCovergroup",
            "CopyClass",
            "MinTypMax",
            "ClockingEvent",
            "AssertionInstance",
        ],
    );
}
