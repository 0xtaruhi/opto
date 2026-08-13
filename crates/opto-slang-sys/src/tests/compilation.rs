// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn compile_requires_inputs() {
    let err = compile(&[], &SlangCompileOptions::default()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "slang compile requires at least one input file"
    );
}

#[test]
fn compilation_is_safe_to_share_for_parallel_lowering() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<SlangCompilation>();
}

#[test]
fn borrowed_leaf_views_remain_compact() {
    assert_eq!(
        std::mem::size_of::<SlangExpression<'_>>(),
        std::mem::size_of::<usize>()
    );
    assert!(std::mem::size_of::<SlangEffect<'_>>() <= 6 * std::mem::size_of::<usize>());
    assert_eq!(
        std::mem::size_of::<SlangTypeLayout<'_>>(),
        std::mem::size_of::<usize>()
    );
    assert!(std::mem::size_of::<SlangExpressionKind<'_>>() <= 5 * std::mem::size_of::<usize>());
}

#[test]
fn materialized_module_guards_share_and_release_native_storage() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = compilation.modules().next().unwrap();

    let first = module.materialize().unwrap();
    let second = module.materialize().unwrap();
    assert_eq!(first.ports().len(), 2);
    drop(first);
    assert_eq!(second.assigns().len(), 1);
    drop(second);

    let rematerialized = module.materialize().unwrap();
    assert_eq!(rematerialized.ports().next().unwrap().name().unwrap(), "a");
}

#[test]
fn native_snapshot_preserves_source_type_layouts() {
    let source = NativeTestSource::new(
        "typedef struct packed { logic [3:1] payload; logic flag; } item_t; module top(input logic [31:1] addr, output logic y); item_t state [2:0]; assign y = addr[1] ^ state[0].flag; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    let addr = module.ports().next().unwrap();
    let addr_layout = addr.type_layout().unwrap();
    assert_eq!(addr_layout.kind().unwrap(), SlangTypeLayoutKind::Array);
    assert_eq!(addr_layout.array_kind().unwrap(), SlangArrayKind::Packed);
    assert_eq!(
        addr_layout.array_range().unwrap(),
        SlangIndexRange { left: 31, right: 1 }
    );

    let state = module
        .nets()
        .find(|net| net.name().unwrap() == "state")
        .unwrap();
    let state_layout = state.type_layout().unwrap().unwrap();
    assert_eq!(state_layout.kind().unwrap(), SlangTypeLayoutKind::Array);
    assert_eq!(state_layout.array_kind().unwrap(), SlangArrayKind::Unpacked);
    assert_eq!(
        state_layout.array_range().unwrap(),
        SlangIndexRange { left: 2, right: 0 }
    );
    let item = state_layout.array_element().unwrap();
    assert_eq!(item.kind().unwrap(), SlangTypeLayoutKind::Struct);
    let fields = item.fields().unwrap().collect::<Vec<_>>();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].name().unwrap(), "payload");
    assert_eq!(fields[0].bit_offset(), 1);
    assert_eq!(fields[1].name().unwrap(), "flag");
    assert_eq!(fields[1].bit_offset(), 0);
}

#[test]
fn successful_compilation_preserves_structured_warnings() {
    let source =
        NativeTestSource::new("module top(output logic [3:0] y); assign y = 8'hff; endmodule\n");

    let compilation = compile_source(&source);
    let warning = compilation
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.severity == SlangDiagnosticSeverity::Warning)
        .expect("width truncation should remain a successful warning");

    assert_eq!(warning.option_name.as_deref(), Some("constant-conversion"));
    assert!(warning.message.contains("changes value"));
    assert!(warning.stable_code().starts_with("OPT-HDL-S"));
    let location = warning.location.as_ref().expect("warning source location");
    assert_eq!(location.path, source.path);
    assert_eq!(location.line, 1);
    assert!(location.column > 0);
}

#[test]
fn rejected_compilation_returns_structured_source_diagnostics() {
    let source = NativeTestSource::new("module top(output logic y); assign y = ; endmodule\n");

    let SlangError::Diagnostics(diagnostics) = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .expect_err("invalid expression must fail with typed diagnostics") else {
        panic!("frontend error did not preserve structured diagnostics");
    };
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == SlangDiagnosticSeverity::Error)
        .expect("typed error diagnostic");

    assert!(!diagnostic.message.is_empty());
    assert!(diagnostic.stable_code().starts_with("OPT-HDL-S"));
    let location = diagnostic.location.as_ref().expect("error source location");
    assert_eq!(location.path, source.path);
    assert_eq!(location.line, 1);
}

#[test]
fn native_analysis_defers_default_parameter_elaboration() {
    let source = NativeTestSource::new(
        "module invalid_by_default #(parameter type T = logic) (output T y); assign y.member = 1'b0; endmodule\nmodule chosen(output logic y); assign y = 1'b1; endmodule\n",
    );
    let unit = SlangSourceUnit {
        files: vec![source.snapshot()],
        dependencies: Vec::new(),
        include_paths: Vec::new(),
        defines: Vec::new(),
        language: SlangLanguage::SystemVerilog2017,
    };

    let analysis = analyze(std::slice::from_ref(&unit), None).unwrap();
    assert_eq!(analysis.definitions, ["chosen", "invalid_by_default"]);
    assert!(analysis.packages.is_empty());

    let compilation = compile_units(std::slice::from_ref(&unit), "chosen", None).unwrap();
    assert_eq!(compilation.top().unwrap(), Some("chosen"));
    assert_eq!(compilation.module_count(), 1);
}

#[test]
fn native_verilog_2005_rejects_systemverilog_port_shortcuts() {
    for text in [
        "module top; integer [31:0] value; endmodule\n",
        "module top; task run(input integer [3:0] value); endtask endmodule\n",
        "module top; task run(input [3] value); endtask endmodule\n",
        "module top; wire [3] value; endmodule\n",
        "module top; input value [2:0]; endmodule\n",
        "module top(input wire x = 1'b0); endmodule\n",
        "module top; parameter integer [2:0] value = 0; endmodule\n",
        "interface iface; endinterface module top(iface x = 1'b0); endmodule\n",
        "module top #(width = 1) (); endmodule\n",
    ] {
        let source = NativeTestSource::new(text);
        let unit = SlangSourceUnit {
            files: vec![source.snapshot()],
            dependencies: Vec::new(),
            include_paths: Vec::new(),
            defines: Vec::new(),
            language: SlangLanguage::Verilog2005,
        };

        assert!(analyze(std::slice::from_ref(&unit), None).is_err());
    }
}

#[test]
fn native_verilog_2005_accepts_legal_integer_ranges_and_scalar_ports() {
    let source = NativeTestSource::new(
        "module top(input value, output [3:0] y); integer index; wire [3:0] data; assign y = data ^ {4{value}}; endmodule\n",
    );
    let unit = SlangSourceUnit {
        files: vec![source.snapshot()],
        dependencies: Vec::new(),
        include_paths: Vec::new(),
        defines: Vec::new(),
        language: SlangLanguage::Verilog2005,
    };

    assert_eq!(
        analyze(std::slice::from_ref(&unit), None)
            .unwrap()
            .definitions,
        ["top"]
    );
}

#[test]
fn native_source_units_preserve_independent_macro_scopes() {
    let two = NativeTestSource::new(
        "module width_two(input logic [`WIDTH-1:0] a, output logic [`WIDTH-1:0] y); assign y = a; endmodule\n",
    );
    let three = NativeTestSource::new(
        "module width_three(input logic [`WIDTH-1:0] a, output logic [`WIDTH-1:0] y); assign y = a; endmodule\n",
    );
    let top = NativeTestSource::new(
        "module top(input logic [1:0] a, input logic [2:0] b, output logic [1:0] y, output logic [2:0] z); width_two u_two(.a(a), .y(y)); width_three u_three(.a(b), .y(z)); endmodule\n",
    );
    let units = [
        source_unit(&two, "WIDTH", "2"),
        source_unit(&three, "WIDTH", "3"),
        SlangSourceUnit {
            files: vec![top.snapshot()],
            dependencies: Vec::new(),
            include_paths: Vec::new(),
            defines: Vec::new(),
            language: SlangLanguage::SystemVerilog2017,
        },
    ];

    let analysis = analyze(&units, None).unwrap();
    assert_eq!(analysis.definitions, ["top", "width_three", "width_two"]);
    let compilation = compile_units(&units, "top", None).unwrap();
    let widths = compilation
        .modules()
        .map(|module| {
            let name = module.name().unwrap().to_string();
            let module = module.materialize().unwrap();
            (name, module.ports().next().unwrap().width())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(widths["width_two"], 2);
    assert_eq!(widths["width_three"], 3);
}

#[test]
fn native_compile_keeps_synthesis_blackboxes_as_interfaces() {
    let source = NativeTestSource::new(
        "(* blackbox = 1 *) module macro(input logic clk, input logic a, output logic y); logic state; always_ff @(posedge clk) state <= a; assign y = state; endmodule\nmodule top(input logic clk, input logic a, output logic y); macro u_macro(.clk(clk), .a(a), .y(y)); endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    let blackbox = module_named(&compilation, "macro");

    let attributes = blackbox.attributes().collect::<Vec<_>>();
    assert_eq!(attributes.len(), 1);
    assert_eq!(attributes[0].name().unwrap(), "blackbox");
    assert!(attributes[0].is_true());
    assert_eq!(
        attributes[0].value().unwrap(),
        crate::SlangAttributeValue::Integer {
            bits: "00000000000000000000000000000001",
            width: 32,
            signed: true,
        }
    );
    assert_eq!(blackbox.ports().len(), 3);
    assert_eq!(blackbox.nets().len(), 0);
    assert_eq!(blackbox.instances().len(), 0);
    assert_eq!(blackbox.assigns().len(), 0);
    assert_eq!(blackbox.procedures().len(), 0);
}

#[test]
fn native_compile_exposes_structural_object_attributes() {
    let source = NativeTestSource::new(
        "module child(input logic a, output logic y); assign y = a; endmodule\n\
         (* module_tag = \"top\" *) module top(a, y);\n\
           (* port_tag = 2 *) input logic a;\n\
           output logic y;\n\
           (* net_tag = \"middle\" *) logic n;\n\
           (* instance_tag *) child u_child(.a(a), .y(n));\n\
           assign y = n;\n\
         endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    let top = module_named(&compilation, "top");

    let attribute = top.attributes().next().unwrap();
    assert_eq!(attribute.name().unwrap(), "module_tag");
    assert_eq!(
        attribute.value().unwrap(),
        SlangAttributeValue::String("top")
    );
    let input = top
        .ports()
        .find(|port| port.name().unwrap() == "a")
        .unwrap();
    assert_eq!(
        input.attributes().next().unwrap().name().unwrap(),
        "port_tag"
    );
    let net = top.nets().find(|net| net.name().unwrap() == "n").unwrap();
    let attribute = net.attributes().next().unwrap();
    assert_eq!(attribute.name().unwrap(), "net_tag");
    assert_eq!(
        attribute.value().unwrap(),
        SlangAttributeValue::String("middle")
    );
    let instance = top.instances().next().unwrap();
    let attribute = instance.attributes().next().unwrap();
    assert_eq!(attribute.name().unwrap(), "instance_tag");
    assert!(attribute.is_true());
}

#[test]
fn native_compile_flattens_interface_arrays_and_modports() {
    let source = NativeTestSource::new(
        "interface bus_if; logic valid; logic ready; logic unused; modport master(output valid, input ready, input unused); modport slave(input valid, output ready, output unused); endinterface\nmodule child(bus_if.slave bus[1]); assign bus[0].ready = bus[0].valid; endmodule\nmodule middle(bus_if bus[1]); child u_child(.bus(bus)); endmodule\nmodule top(input logic a, output logic y); bus_if buses[1](); assign buses[0].valid = a; assign y = buses[0].ready; middle u_middle(.bus(buses)); endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    let child = module_named(&compilation, "child");
    assert_eq!(
        child
            .ports()
            .map(|port| port.name().unwrap())
            .collect::<Vec<_>>(),
        ["bus.valid", "bus.ready"]
    );
    let middle = module_named(&compilation, "middle");
    assert_eq!(
        middle
            .ports()
            .map(|port| (port.name().unwrap(), port.direction().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("bus.valid", SlangPortDirection::Input),
            ("bus.ready", SlangPortDirection::Output),
        ]
    );

    let top = module_named(&compilation, "top");
    assert_eq!(
        top.nets()
            .map(|net| net.name().unwrap())
            .collect::<Vec<_>>(),
        ["buses[0].valid", "buses[0].ready", "buses[0].unused"]
    );
    assert_eq!(
        top.instances()
            .next()
            .unwrap()
            .connections()
            .map(|connection| connection.port().unwrap())
            .collect::<Vec<_>>(),
        ["bus.valid", "bus.ready"]
    );
}

#[test]
fn native_compile_keeps_local_interface_storage_connected_to_modport() {
    let source = NativeTestSource::new(
        "typedef struct packed { logic x; logic y; } payload_t; interface bus_if; logic valid; payload_t data; logic ready; modport master(output valid, output data, input ready); endinterface\nmodule bit_source(input logic a, output logic q); assign q = a; endmodule\nmodule producer(input logic a, bus_if.master bus); bit_source u_valid(.a(a), .q(bus.valid)); bit_source u_data(.a(a), .q(bus.data.x)); assign bus.data.y = a; endmodule\nmodule top(input logic a, output logic y); bus_if link(); producer u_producer(.a(a), .bus(link)); assign link.ready = 1'b1; assign y = link.data.x; endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    let top = module_named(&compilation, "top");
    assert_eq!(
        top.nets()
            .map(|net| net.name().unwrap())
            .collect::<Vec<_>>(),
        ["link.valid", "link.data", "link.ready"]
    );
    let producer = module_named(&compilation, "producer");
    assert_eq!(
        producer
            .ports()
            .map(|port| port.name().unwrap())
            .collect::<Vec<_>>(),
        ["a", "bus.valid", "bus.data", "bus.ready"]
    );
}

#[test]
fn native_compile_resolves_direct_child_input_port_references() {
    let source = NativeTestSource::new(
        "module child(input logic a); endmodule\nmodule top(input logic a, output logic y); child u_child(.a(a)); assign y = u_child.a; endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    let top = module_named(&compilation, "top");
    let rhs = top.assigns().next().unwrap().rhs().unwrap();
    assert!(is_signal(rhs, "a"));
}

#[test]
fn native_compile_honors_synthesis_translate_regions() {
    let source = NativeTestSource::new(
        "module leaf(input logic a); endmodule\nmodule middle(input logic a); leaf u_leaf(.a(a)); endmodule\nmodule top(input logic a, output logic y); middle u_middle(.a(a)); // pragma translate_off\nlogic trace; assign trace = u_middle.u_leaf.a; // pragma translate_on\nassign y = a; endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    let top = module_named(&compilation, "top");

    assert_eq!(top.nets().len(), 0);
    assert_eq!(top.assigns().len(), 1);
    assert!(is_signal(top.assigns().next().unwrap().rhs().unwrap(), "a"));
}

#[test]
fn native_compile_short_circuits_dominated_invalid_branches() {
    let source = NativeTestSource::new(
        "module top(input logic enable, input logic [31:0] value, output logic y); always_comb begin y = 1'b0; if (enable && 1'b0 && |value[31:34]) y = 1'b1; end endmodule\n",
    );
    let compilation = compile_source(&source);

    let module = first_module(&compilation);
    assert_eq!(module.procedures().len(), 1);
}

#[test]
fn native_compile_short_circuits_struct_parameter_members() {
    let source = NativeTestSource::new(
        "typedef struct { bit is_wide; } config_t; \
         module top #(parameter config_t Config = '{default: 1'b0}) \
         (input logic enable, input logic [31:0] value, output logic y); \
         always_comb begin y = 1'b0; \
         if (enable && Config.is_wide && |value[31:34]) y = 1'b1; \
         end endmodule\n",
    );
    let compilation = compile_source(&source);

    let module = first_module(&compilation);
    assert_eq!(module.procedures().len(), 1);
}

#[test]
fn native_compile_rejects_undominated_invalid_branches() {
    let source = NativeTestSource::new(
        "module top(input logic enable, input logic [31:0] value, output logic y); always_comb begin y = 1'b0; if (enable && |value[31:34]) y = 1'b1; end endmodule\n",
    );
    let err = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions::default(),
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("range select is outside its declared range"),
        "{err}"
    );
}

#[test]
fn native_compile_elides_static_conditional_branches() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] value, output logic y); localparam bit ENABLE = 1'b0; assign y = ENABLE ? value[2] : 1'b0; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();

    assert!(matches!(
        rhs.kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant { bits: "0", .. })
    ));
}

#[test]
fn native_compile_normalizes_multibit_boolean_contexts() {
    let source = NativeTestSource::new(
        "module top(input logic [1:0] select, input logic a, b, output logic y_mux, y_if); assign y_mux = select ? a : b; always_comb begin y_if = 1'b0; if (select) y_if = a; end endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let SlangExpressionKind::Mux { condition, .. } = module
        .assigns()
        .next()
        .unwrap()
        .rhs()
        .unwrap()
        .kind()
        .unwrap()
    else {
        panic!("expected conditional expression");
    };
    let (branch_condition, _, _) = first_branch(module.procedures().next().unwrap());

    assert!(matches!(
        condition.kind().unwrap(),
        SlangExpressionKind::Unary {
            op: SlangUnaryOp::ReductionOr,
            ..
        }
    ));
    assert!(matches!(
        branch_condition.kind().unwrap(),
        SlangExpressionKind::Unary {
            op: SlangUnaryOp::ReductionOr,
            ..
        }
    ));
}

#[test]
fn native_compile_lowers_fixed_streaming_concatenations() {
    let source = NativeTestSource::new(
        "module top(input logic [31:0] value, output logic [31:0] swapped); assign swapped = {<<8{value}}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();
    let SlangExpressionKind::Cast {
        value, width: 32, ..
    } = rhs.kind().unwrap()
    else {
        panic!("expected streaming result conversion");
    };
    let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
        panic!("expected reordered streaming concatenation");
    };
    let ranges = parts
        .parts()
        .map(|part| {
            let SlangExpressionKind::Signal(signal) = part.unwrap().kind().unwrap() else {
                panic!("expected signal slices");
            };
            signal.range.unwrap()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        ranges,
        [
            SlangBitRange { msb: 7, lsb: 0 },
            SlangBitRange { msb: 15, lsb: 8 },
            SlangBitRange { msb: 23, lsb: 16 },
            SlangBitRange { msb: 31, lsb: 24 },
        ]
    );
}

#[test]
fn native_compile_elides_zero_count_replication_in_concatenation() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); assign y = {{0{1'b0}}, a}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();
    let SlangExpressionKind::Concat(parts) = rhs.kind().unwrap() else {
        panic!("expected concatenation");
    };

    assert_eq!(parts.parts().len(), 1);
    assert!(is_signal(parts.parts().next().unwrap().unwrap(), "a"));
}

#[test]
fn native_compile_lowers_simple_assign() {
    let source = NativeTestSource::new(
        "module top(input logic a, output logic y); logic n; assign n = ~a; assign y = n; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(compilation.top().unwrap(), Some("top"));
    assert_eq!(compilation.module_count(), 1);
    assert_eq!(module.ports().len(), 2);
    assert_eq!(module.nets().len(), 1);
    assert_eq!(module.assigns().len(), 2);
    assert_eq!(module.procedures().len(), 0);
}
