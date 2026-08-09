// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn native_compile_serializes_packed_struct_parameters() {
    let source = NativeTestSource::new(
        "package p; typedef struct packed { logic irq_int; logic irq_ext; logic [4:0] cause; } cause_t; localparam cause_t IRQ = '{irq_int: 1'b0, irq_ext: 1'b1, cause: 5'd31}; endpackage module top(input logic select, output logic [6:0] all, output logic [4:0] cause); assign all = select ? p::IRQ : '0; assign cause = p::IRQ.cause; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    let SlangExpressionKind::Mux { then_value, .. } = assignments[0].rhs().unwrap().kind().unwrap()
    else {
        panic!("expected conditional packed parameter");
    };
    assert!(matches!(
        then_value.kind().unwrap(),
        SlangExpressionKind::Constant(constant)
            if constant.width == Some(7) && constant.bits == "0111111"
    ));
    assert!(matches!(
        assignments[1].rhs().unwrap().kind().unwrap(),
        SlangExpressionKind::Constant(constant)
            if constant.width == Some(5) && constant.bits == "11111"
    ));
}

#[test]
fn native_compile_sizes_unbased_unsized_literals() {
    let source = NativeTestSource::new(
        "module top(output logic [3:0] zero, one); assign zero = '0; assign one = '1; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let constants = module
        .assigns()
        .map(|assign| assign.rhs().unwrap().kind().unwrap())
        .collect::<Vec<_>>();

    assert!(matches!(
        constants[0],
        SlangExpressionKind::Constant(constant)
            if constant.width == Some(4) && constant.bits == "0000"
    ));
    assert!(matches!(
        constants[1],
        SlangExpressionKind::Constant(constant)
            if constant.width == Some(4) && constant.bits == "1111"
    ));
}

#[test]
fn native_compile_flattens_unpacked_array_patterns_and_selects() {
    let source = NativeTestSource::new(
        "module top(input logic [3:0] a, b, output logic [3:0] y0, y1); logic [3:0] values[2]; assign values = '{a, b}; assign y0 = values[0]; assign y1 = values[1]; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    let SlangExpressionKind::Concat(parts) = assignments[0].rhs().unwrap().kind().unwrap() else {
        panic!("expected flattened assignment pattern");
    };
    let parts = parts.parts().map(Result::unwrap).collect::<Vec<_>>();
    assert_eq!(parts.len(), 2);
    assert!(is_signal(parts[0], "b"));
    assert!(is_signal(parts[1], "a"));

    for (assignment, expected) in [
        (&assignments[1], SlangBitRange { msb: 3, lsb: 0 }),
        (&assignments[2], SlangBitRange { msb: 7, lsb: 4 }),
    ] {
        let SlangExpressionKind::Signal(selected) = assignment.rhs().unwrap().kind().unwrap()
        else {
            panic!("expected flattened array element select");
        };
        assert_eq!(selected.name, "values");
        assert_eq!(selected.range.unwrap(), expected);
    }
}

#[test]
fn native_compile_flattens_packed_struct_member_access() {
    let source = NativeTestSource::new(
        "package p; typedef struct packed { logic [3:0] current_pc; logic [1:0] code; } dump_t; endpackage module top(input logic [3:0] pc, input logic [1:0] code, output p::dump_t dump); assign dump.current_pc = pc; assign dump.code = code; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    let ranges = assignments
        .iter()
        .map(|assignment| {
            let SlangExpressionKind::Signal(signal) = assignment.lhs().unwrap().kind().unwrap()
            else {
                panic!("expected packed member signal target");
            };
            signal.range.unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges[0], SlangBitRange { msb: 5, lsb: 2 });
    assert_eq!(ranges[1], SlangBitRange { msb: 1, lsb: 0 });
}

#[test]
fn native_compile_composes_nested_packed_member_offsets() {
    let source = NativeTestSource::new(
        "package p; typedef struct packed { logic [1:0] low; logic high; } inner_t; typedef struct packed { logic lead; inner_t payload; } outer_t; endpackage module top(input p::outer_t value, output logic [1:0] y); assign y = value.payload.low; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::Signal(signal) = rhs.kind().unwrap() else {
        panic!("expected flattened packed member signal");
    };
    assert_eq!(signal.range.unwrap(), SlangBitRange { msb: 2, lsb: 1 });
}

#[test]
fn native_compile_translates_ascending_indexed_part_selects() {
    let source = NativeTestSource::new(
        "module top(input logic [4:7] a, output logic [1:0] y); assign y = a[5 +: 2]; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::Signal(signal) = rhs.kind().unwrap() else {
        panic!("expected flattened part select");
    };
    assert_eq!(signal.range.unwrap(), SlangBitRange { msb: 2, lsb: 1 });
}

#[test]
fn native_compile_preserves_runtime_packed_element_selects() {
    let source = NativeTestSource::new(
        "module top(input logic [31:0] data, input logic [4:0] index, output logic y); assign y = data[index]; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expression = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::DynamicExtract {
        value,
        offset,
        width,
    } = expression.kind().unwrap()
    else {
        panic!("expected dynamic extract");
    };
    assert_eq!(width, 1);
    assert!(is_signal(value, "data"));
    assert!(matches!(
        offset.kind().unwrap(),
        SlangExpressionKind::Cast { value, width: 5, signed: false, .. }
            if is_signal(value, "index")
    ));
}

#[test]
fn native_compile_preserves_runtime_indexed_part_selects() {
    let source = NativeTestSource::new(
        "module top(input logic [31:0] data, input logic [4:0] index, output logic [31:0] y); always_comb begin y = '0; y[index +: 8] = data[index +: 8]; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let lhs = effects[1].lhs().unwrap();
    let rhs = effects[1].rhs().unwrap();

    assert!(matches!(
        lhs.kind().unwrap(),
        SlangExpressionKind::DynamicExtract { width: 8, .. }
    ));
    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::DynamicExtract { width: 8, .. }
    ));
}

#[test]
fn native_compile_preserves_runtime_unpacked_element_selects() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] index, output logic [7:0] y, output logic [7:0] first); logic [7:0] values[4]; assign y = values[index]; assign first = values[0]; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expression = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::DynamicExtract {
        value,
        offset,
        width,
    } = expression.kind().unwrap()
    else {
        panic!("expected dynamic extract");
    };
    assert_eq!(width, 8);
    assert!(is_signal(value, "values"));
    let SlangExpressionKind::Binary {
        op: SlangBinaryOp::Mul,
        left,
        right,
    } = offset.kind().unwrap()
    else {
        panic!("expected scaled dynamic offset");
    };
    assert!(matches!(
        left.kind().unwrap(),
        SlangExpressionKind::Cast {
            width: 5,
            signed: false,
            value,
            ..
        } if matches!(
            value.kind().unwrap(),
            SlangExpressionKind::Cast {
                width: 2,
                signed: false,
                value,
                ..
            } if is_signal(value, "index")
        )
    ));
    assert!(matches!(
        right.kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(5),
            bits: "01000",
            signed: false,
        })
    ));

    let first = module.assigns().nth(1).unwrap().rhs().unwrap();
    let SlangExpressionKind::Signal(first) = first.kind().unwrap() else {
        panic!("expected static unpacked element signal");
    };
    assert_eq!(first.name, "values");
    assert_eq!(first.range, Some(SlangBitRange { msb: 7, lsb: 0 }));
}

#[test]
fn native_compile_reports_unpacked_element_signedness() {
    let source = NativeTestSource::new("module top; logic signed [7:0] memory[4]; endmodule\n");
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let memory = module
        .nets()
        .find(|net| net.name().unwrap() == "memory")
        .unwrap();

    assert!(memory.element_is_signed());
}

#[test]
fn native_compile_preserves_wide_runtime_unpacked_element_offsets() {
    let source = NativeTestSource::new(
        "module top(input logic [4:0] index, output logic [63:0] y); logic [63:0] values[32]; assign y = values[index]; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let expression = module.assigns().next().unwrap().rhs().unwrap();

    let SlangExpressionKind::DynamicExtract { offset, width, .. } = expression.kind().unwrap()
    else {
        panic!("expected dynamic extract");
    };
    assert_eq!(width, 64);
    let SlangExpressionKind::Binary {
        op: SlangBinaryOp::Mul,
        left,
        right,
    } = offset.kind().unwrap()
    else {
        panic!("expected scaled dynamic offset");
    };
    assert!(matches!(
        left.kind().unwrap(),
        SlangExpressionKind::Cast {
            width: 11,
            signed: false,
            ..
        }
    ));
    assert!(matches!(
        right.kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(11),
            bits: "00001000000",
            signed: false,
        })
    ));
}

#[test]
fn native_compile_preserves_runtime_procedural_assignment_targets() {
    let source = NativeTestSource::new(
        "module top(input logic [2:0] index, output logic [7:0] we); always_comb begin we = '0; we[index] = 1'b1; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let lhs = effects[1].lhs().unwrap();
    assert!(matches!(
        lhs.kind().unwrap(),
        SlangExpressionKind::DynamicExtract { width: 1, .. }
    ));
}

#[test]
fn native_compile_composes_dynamic_element_and_struct_field_lvalues() {
    let source = NativeTestSource::new(
        "typedef struct packed { logic [2:0] payload; logic flag; } item_t; module top(input logic index, input logic value, output item_t [1:0] items); always_comb begin items = '0; items[index].flag = value; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let lhs = effects[1].lhs().unwrap();

    assert!(matches!(
        lhs.kind().unwrap(),
        SlangExpressionKind::DynamicExtract { width: 1, .. }
    ));
}

#[test]
fn native_compile_flattens_multidimensional_dynamic_lvalues() {
    let source = NativeTestSource::new(
        "typedef struct packed { logic [2:0] payload; logic flag; } item_t; module top(input logic row, input logic column, input logic value, output item_t [1:0][1:0] items); always_comb begin items = '0; items[row][column].flag = value; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let lhs = effects[1].lhs().unwrap();
    let SlangExpressionKind::DynamicExtract { value, width, .. } = lhs.kind().unwrap() else {
        panic!("expected flattened dynamic assignment target");
    };

    assert_eq!(width, 1);
    assert!(is_signal(value, "items"));
}

#[test]
fn native_compile_folds_static_slice_offsets_into_dynamic_lvalues() {
    let source = NativeTestSource::new(
        "typedef struct packed { logic [15:0] upper; logic [7:0] field; logic [7:0] lower; } data_t; module top(input logic [2:0] index, input logic value, output data_t data); always_comb begin data = '0; data.field[index] = value; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());
    let lhs = effects[1].lhs().unwrap();
    let SlangExpressionKind::DynamicExtract { value, width, .. } = lhs.kind().unwrap() else {
        panic!("expected flattened dynamic target");
    };

    assert_eq!(width, 1);
    assert!(is_signal(value, "data"));
}

#[test]
fn native_compile_folds_procedural_loop_conditions_before_lowering() {
    let source = NativeTestSource::new(
        "module top(output logic [1:0] y); always_comb begin y = '0; for (int i = 0; i < 4; i++) begin if (i >= 2) y[i - 2] = 1'b1; end end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let effects = procedure_effects(module.procedures().next().unwrap());

    assert_eq!(effects.len(), 3);
}
