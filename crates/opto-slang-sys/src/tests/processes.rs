// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_compile_normalizes_compound_procedural_assignments() {
    let source = NativeTestSource::new(
        "module top(input logic [4:0] a, output logic [4:0] y); always_comb begin y = a; y -= 5'd1; y |= 5'd2; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 3);
    assert!(matches!(
        effects[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::Sub,
            ..
        }
    ));
    assert!(matches!(
        effects[2].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Binary {
            op: SlangBinaryOp::BitOr,
            ..
        }
    ));
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
fn native_compile_lowers_procedural_variable_initializers() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); always_comb begin logic temporary = a; y = temporary; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 2);
    assert!(is_signal(effects[0].rhs().unwrap(), "a"));
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
fn native_compile_unrolls_predeclared_for_loops_with_break() {
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
    assert!(
        nets.iter()
            .any(|(name, _)| name.starts_with("__opto_loop_") && name.ends_with("_broken"))
    );
    assert!(procedure_effects(module.procedures().next().unwrap()).len() >= 6);
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
fn unsupported_runtime_loops_fail_explicitly() {
    let source = NativeTestSource::new(
        "module top(input logic enable, output logic y); always_comb while (enable) y = 1; endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unsupported statement 'WhileLoop'")
    );
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
            "ReplicatedAssignmentPattern",
            "Dist",
            "NewArray",
            "NewClass",
            "NewCovergroup",
            "CopyClass",
            "MinTypMax",
            "ClockingEvent",
            "AssertionInstance",
            "TaggedUnion",
        ],
    );
}
