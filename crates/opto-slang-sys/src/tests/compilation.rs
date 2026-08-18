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
fn native_compile_flattens_explicit_modport_expressions() {
    let source = NativeTestSource::new(
        "interface bus_if; logic hi; logic lo; logic signed [1:0] signed_value; modport source(input .pair({hi, lo}), .signed_value(signed_value)); modport sink(output .pair({hi, lo})); endinterface\nmodule reader(bus_if.source bus, output logic [1:0] y); assign y = bus.pair ^ bus.signed_value; endmodule\nmodule writer(bus_if.sink bus, input logic [1:0] d); assign bus.pair = d; endmodule\nmodule top(input logic [1:0] d, output logic [1:0] y); bus_if link(); assign link.signed_value = d; writer u_writer(.bus(link), .d(d)); reader u_reader(.bus(link), .y(y)); endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();

    let reader = module_named(&compilation, "reader");
    assert_eq!(
        reader
            .ports()
            .map(|port| (
                port.name().unwrap(),
                port.direction().unwrap(),
                port.width()
            ))
            .collect::<Vec<_>>(),
        [
            ("bus.pair", SlangPortDirection::Input, 2),
            ("bus.signed_value", SlangPortDirection::Input, 2),
            ("y", SlangPortDirection::Output, 2),
        ]
    );
    assert!(
        reader
            .ports()
            .find(|port| port.name().unwrap() == "bus.signed_value")
            .unwrap()
            .is_signed()
    );
    let writer = module_named(&compilation, "writer");
    assert_eq!(
        writer
            .ports()
            .map(|port| (
                port.name().unwrap(),
                port.direction().unwrap(),
                port.width()
            ))
            .collect::<Vec<_>>(),
        [
            ("bus.pair", SlangPortDirection::Output, 2),
            ("d", SlangPortDirection::Input, 2),
        ]
    );
    let top = module_named(&compilation, "top");
    let pair_connections = top
        .instances()
        .flat_map(SlangInstance::connections)
        .filter(|connection| connection.port().unwrap() == "bus.pair")
        .collect::<Vec<_>>();
    assert_eq!(pair_connections.len(), 2);
    assert!(pair_connections.iter().all(|connection| matches!(
        connection.expression().unwrap().kind().unwrap(),
        SlangExpressionKind::Concat(_)
    )));
}

#[test]
fn native_compile_flattens_explicit_modport_inout_lvalue() {
    let source = NativeTestSource::new(
        "interface pad_if; wire pad; modport device(inout .pin(pad)); endinterface\nmodule transceiver(pad_if.device bus, input logic d, output logic q); assign bus.pin = d; assign q = bus.pin; endmodule\nmodule top(input logic d, output logic q); pad_if link(); transceiver u_transceiver(.bus(link), .d(d), .q(q)); endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();

    let transceiver = module_named(&compilation, "transceiver");
    let pin = transceiver
        .ports()
        .find(|port| port.name().unwrap() == "bus.pin")
        .unwrap();
    assert_eq!(pin.direction().unwrap(), SlangPortDirection::Inout);
    assert_eq!(pin.width(), 1);
    let top = module_named(&compilation, "top");
    let connection = top
        .instances()
        .next()
        .unwrap()
        .connections()
        .find(|connection| connection.port().unwrap() == "bus.pin")
        .unwrap();
    assert!(matches!(
        connection.expression().unwrap().kind().unwrap(),
        SlangExpressionKind::Signal(_)
    ));
}

#[test]
fn native_compile_rejects_non_lvalue_modport_output_expression() {
    let source = NativeTestSource::new(
        "interface bus_if; logic a; logic b; modport bad(output .value(a & b)); endinterface\nmodule child(bus_if.bad bus, input logic d); assign bus.value = d; endmodule\nmodule top(input logic d); bus_if bus(); child u_child(.bus(bus), .d(d)); endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .expect_err("an output modport expression must be an lvalue");
    let SlangError::Diagnostics(diagnostics) = error else {
        panic!("expected structured Slang diagnostics, got {error}");
    };
    let invalid = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("not assignable"))
        .expect("non-lvalue output diagnostic");
    assert!(invalid.location.is_some());
    assert!(invalid.stable_code().starts_with("OPT-HDL-S"));
}

#[test]
fn native_compile_flattens_nested_interface_modport_expression() {
    let source = NativeTestSource::new(
        "interface leaf_if; logic value; endinterface\ninterface outer_if(leaf_if nested); modport view(input .nested_value(nested.value)); endinterface\nmodule child(outer_if.view bus, output logic y); assign y = bus.nested_value; endmodule\nmodule top(input logic value, output logic y); leaf_if leaf(); outer_if outer(leaf); assign leaf.value = value; child u_child(.bus(outer), .y(y)); endmodule\n",
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
            .map(|port| (port.name().unwrap(), port.direction().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("bus.nested_value", SlangPortDirection::Input),
            ("y", SlangPortDirection::Output),
        ]
    );
}

#[test]
fn native_compile_inlines_imported_modport_subroutines() {
    let source = NativeTestSource::new(
        "interface math_if; function automatic logic invert(input logic value); return ~value; endfunction task automatic copy(input logic value, output logic result); result = value; endtask modport user(import invert, copy); endinterface\nmodule child(math_if.user api, input logic a, output logic y, output logic z); assign y = api.invert(a); always_comb api.copy(a, z); endmodule\nmodule top(input logic a, output logic y, output logic z); math_if api(); child u_child(.api(api), .a(a), .y(y), .z(z)); endmodule\n",
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
        ["a", "y", "z"]
    );
    assert!(!child.assigns().collect::<Vec<_>>().is_empty());
    assert!(!child.procedures().collect::<Vec<_>>().is_empty());
}

#[test]
fn native_compile_flattens_imported_method_state_dependencies() {
    let source = NativeTestSource::new(
        "interface state_if; logic observed; logic updated; function automatic logic read_helper(); return observed; endfunction function automatic logic read_state(); return read_helper(); endfunction task automatic write_helper(input logic value); updated = value; endtask task automatic write_state(input logic value); write_helper(value); endtask modport user(import read_state, write_state); endinterface\nmodule child(state_if.user api, input logic d, output logic y); assign y = api.read_state(); always_comb api.write_state(d); endmodule\nmodule top(input logic d, input logic observed, output logic y, output logic updated); state_if api(); assign api.observed = observed; assign updated = api.updated; child u_child(.api(api), .d(d), .y(y)); endmodule\n",
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
            .map(|port| (port.name().unwrap(), port.direction().unwrap()))
            .collect::<Vec<_>>(),
        [
            (
                "api.__opto_method_read_state.observed",
                SlangPortDirection::Input,
            ),
            (
                "api.__opto_method_write_state.updated",
                SlangPortDirection::Ref,
            ),
            ("d", SlangPortDirection::Input),
            ("y", SlangPortDirection::Output),
        ]
    );
}

#[test]
fn native_compile_inlines_unambiguous_exported_modport_function() {
    let source = NativeTestSource::new(
        "interface callback_if(input logic value); extern function logic transform(input logic source); logic result; always_comb result = transform(value); modport implementation(input value, output result, export transform); endinterface\nmodule provider(callback_if.implementation api); function logic api.transform(input logic source); return ~source; endfunction endmodule\nmodule top(input logic value, output logic result); callback_if api(value); provider u_provider(.api(api)); assign result = api.result; endmodule\n",
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
    assert!(!top.procedures().collect::<Vec<_>>().is_empty());
    let provider = module_named(&compilation, "provider");
    assert_eq!(
        provider
            .ports()
            .map(|port| port.name().unwrap())
            .collect::<Vec<_>>(),
        ["api.value"]
    );
}

#[test]
fn native_compile_rejects_ambiguous_exported_modport_implementations() {
    let source = NativeTestSource::new(
        "interface callback_if; extern function logic transform(input logic source); modport implementation(export transform); endinterface\nmodule first(callback_if.implementation api); function logic api.transform(input logic source); return source; endfunction endmodule\nmodule second(callback_if.implementation api); function logic api.transform(input logic source); return ~source; endfunction endmodule\nmodule top; callback_if api(); first u_first(.api(api)); second u_second(.api(api)); endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .expect_err("multiple exported implementations must be rejected");
    let SlangError::Diagnostics(diagnostics) = error else {
        panic!("expected structured Slang diagnostics, got {error}");
    };
    let duplicate = diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic
                .message
                .contains("more than one implementation provided for extern")
        })
        .expect("duplicate export diagnostic");
    assert!(duplicate.location.is_some());
    assert!(duplicate.stable_code().starts_with("OPT-HDL-S"));
}

#[test]
fn native_compile_rejects_nonsynthesizable_modport_method_with_typed_failure() {
    let source = NativeTestSource::new(
        "interface callback_if; import \"DPI-C\" function logic callback(input logic source); modport user(import callback); endinterface\nmodule child(callback_if.user api, input logic source, output logic result); assign result = api.callback(source); endmodule\nmodule top(input logic source, output logic result); callback_if api(); child u_child(.api(api), .source(source), .result(result)); endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .expect_err("DPI modport method must remain outside the synthesis profile");
    let SlangError::LoweringFailed(failure) = error else {
        panic!("expected a typed lowering failure, got {error}");
    };
    assert_eq!(
        failure.category,
        SlangLoweringFailureCategory::UnsupportedProfile
    );
    assert_eq!(failure.stable_code(), "OPT-HDL-LP-0002");
    assert!(failure.message.contains("synthesizable implementation"));
    assert!(failure.location.is_some());
}

#[test]
fn native_compile_rejects_exported_modport_method_external_capture() {
    let source = NativeTestSource::new(
        "interface callback_if(input logic value); extern function logic transform(input logic source); logic result; always_comb result = transform(value); modport implementation(input value, output result, export transform); endinterface\nmodule provider(callback_if.implementation api, input logic mask); function logic api.transform(input logic source); return source ^ mask; endfunction endmodule\nmodule top(input logic value, input logic mask, output logic result); callback_if api(value); provider u_provider(.api(api), .mask(mask)); assign result = api.result; endmodule\n",
    );
    let error = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .expect_err("exported implementation state outside the interface must be rejected");
    let SlangError::LoweringFailed(failure) = error else {
        panic!("expected a typed lowering failure, got {error}");
    };
    assert_eq!(
        failure.category,
        SlangLoweringFailureCategory::UnsupportedProfile
    );
    assert_eq!(failure.stable_code(), "OPT-HDL-LP-0006");
    assert!(failure.message.contains("outside its interface"));
    assert!(failure.location.is_some());
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
fn native_compile_preserves_module_reference_ports_as_exact_aliases() {
    let source = NativeTestSource::new(
        "module child(ref logic value, input logic d); always_comb value = d; endmodule\nmodule top(input logic d, output logic y); logic shared; child u_child(.value(shared), .d(d)); assign y = shared; endmodule\n",
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
            .map(|port| (port.name().unwrap(), port.direction().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("value", SlangPortDirection::Ref),
            ("d", SlangPortDirection::Input),
        ]
    );
    let top = module_named(&compilation, "top");
    let connection = top
        .instances()
        .next()
        .unwrap()
        .connections()
        .find(|connection| connection.port().unwrap() == "value")
        .unwrap();
    let SlangExpressionKind::Signal(actual) = connection.expression().unwrap().kind().unwrap()
    else {
        panic!("reference-port actual should remain one signal alias");
    };
    assert_eq!(actual.name, "shared");
}

#[test]
fn native_compile_lowers_named_legacy_port_concatenations() {
    let source = NativeTestSource::new(
        "module legacy(.incoming({high, low}), .outgoing({upper[0:3], lower[7:4]})); input [3:0] high; input [0:3] low; output [0:3] upper; output [7:0] lower; assign upper = low; assign lower = {high, low}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);

    assert_eq!(
        module
            .ports()
            .map(|port| (
                port.name().unwrap(),
                port.direction().unwrap(),
                port.width()
            ))
            .collect::<Vec<_>>(),
        [
            ("incoming", SlangPortDirection::Input, 8),
            ("outgoing", SlangPortDirection::Output, 8),
        ]
    );
    assert_eq!(
        module
            .nets()
            .map(|net| net.name().unwrap())
            .collect::<Vec<_>>(),
        ["high", "low", "upper", "lower"]
    );

    let assignments = module.assigns().collect::<Vec<_>>();
    let incoming = assignments
        .iter()
        .filter_map(|assignment| {
            let lhs = assignment.lhs().ok()?.kind().ok()?;
            let SlangExpressionKind::Signal(signal) = lhs else {
                return None;
            };
            matches!(signal.name, "high" | "low").then(|| assignment.rhs().unwrap())
        })
        .collect::<Vec<_>>();
    assert_eq!(incoming.len(), 2);
    assert!(matches!(
        incoming[0].kind().unwrap(),
        SlangExpressionKind::Extract {
            lsb: 4,
            width: 4,
            ..
        }
    ));
    assert!(matches!(
        incoming[1].kind().unwrap(),
        SlangExpressionKind::Extract {
            lsb: 0,
            width: 4,
            ..
        }
    ));

    let outgoing = assignments
        .iter()
        .find(|assignment| is_signal(assignment.lhs().unwrap(), "outgoing"))
        .expect("external output projection should be driven");
    let SlangExpressionKind::Concat(parts) = outgoing.rhs().unwrap().kind().unwrap() else {
        panic!("external output should preserve concatenation order");
    };
    let parts = parts
        .parts()
        .map(|part| part.unwrap().kind().unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        parts[0],
        SlangExpressionKind::Signal(SlangSignalRef {
            name: "upper",
            range: Some(SlangBitRange { msb: 3, lsb: 0 })
        })
    ));
    assert!(matches!(
        parts[1],
        SlangExpressionKind::Signal(SlangSignalRef {
            name: "lower",
            range: Some(SlangBitRange { msb: 7, lsb: 4 })
        })
    ));

    let source =
        NativeTestSource::new("module exact(.shared(shared)); inout [3:0] shared; endmodule\n");
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    assert_eq!(
        module
            .ports()
            .map(|port| (
                port.name().unwrap(),
                port.direction().unwrap(),
                port.width()
            ))
            .collect::<Vec<_>>(),
        [("shared", SlangPortDirection::Inout, 4)]
    );
    assert_eq!(module.nets().count(), 0);
}

#[test]
fn native_compile_rejects_noninvertible_legacy_port_projections() {
    let cases = [
        (
            "module bad(.p({a[3:0], a[2:0]})); output [3:0] a; endmodule\n",
            "overlapping internal bit mappings",
        ),
        (
            "module bad(.p({a, b})); inout a, b; endmodule\n",
            "exact whole-signal inout or ref mapping",
        ),
        (
            "module bad(.p({a, b})); input a; output b; assign b = a; endmodule\n",
            "mixes input and output component directions",
        ),
    ];

    for (text, expected) in cases {
        let source = NativeTestSource::new(text);
        let error = compile(
            std::slice::from_ref(&source.path),
            &SlangCompileOptions::default(),
        )
        .expect_err("invalid external port projection should fail lowering");
        let SlangError::LoweringFailed(failure) = error else {
            panic!("expected structured lowering failure, got {error}");
        };
        assert_eq!(
            failure.category,
            SlangLoweringFailureCategory::InvalidProjection
        );
        assert_eq!(failure.stable_code(), "OPT-HDL-LR-0001");
        assert!(failure.message.contains(expected), "{}", failure.message);
        let location = failure
            .location
            .expect("external port should have a source span");
        assert_eq!(location.path, source.path);
        assert_eq!(location.line, 1);
        assert!(location.column > 0);
    }
}

#[test]
fn native_compile_flattens_reference_modport_members_as_aliases() {
    let source = NativeTestSource::new(
        "interface bus_if; logic [7:0] data; modport alias_port(ref data); endinterface\nmodule child(bus_if.alias_port bus, input logic [7:0] d); always_comb bus.data = d; endmodule\nmodule top(input logic [7:0] d, output logic [7:0] y); bus_if link(); child u_child(.bus(link), .d(d)); assign y = link.data; endmodule\n",
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
            .map(|port| (port.name().unwrap(), port.direction().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("bus.data", SlangPortDirection::Ref),
            ("d", SlangPortDirection::Input),
        ]
    );
}

#[test]
fn native_compile_accepts_dynamic_unpacked_reference_port_actuals() {
    let source = NativeTestSource::new(
        "module child(ref logic [7:0] value, input logic [7:0] d); always_comb value = d; endmodule\nmodule top(input logic [7:0] d, input logic [1:0] index, output logic [7:0] y); logic [7:0] values [0:3]; child u_child(.value(values[index]), .d(d)); assign y = values[index]; endmodule\n",
    );
    let compilation = compile(
        std::slice::from_ref(&source.path),
        &SlangCompileOptions {
            top: Some("top".to_string()),
            ..SlangCompileOptions::default()
        },
    )
    .unwrap();
    assert_eq!(module_named(&compilation, "child").ports().len(), 2);
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
fn native_compile_lowers_constant_streaming_with_ranges_in_declared_order() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] descending [3:0], input logic [7:0] ascending [0:3], output logic [23:0] desc_right, asc_right, desc_left); assign desc_right = {>>{descending with [2:0]}}; assign asc_right = {>>{ascending with [1:3]}}; assign desc_left = {<<10{descending with [2:0]}}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    for assignment in &assignments[..2] {
        let SlangExpressionKind::Cast {
            value,
            width: 24,
            signed: false,
            ..
        } = assignment.rhs().unwrap().kind().unwrap()
        else {
            panic!("expected unsigned streaming result conversion");
        };
        let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
            panic!("expected selected streaming elements");
        };
        let ranges = parts
            .parts()
            .map(|part| {
                let SlangExpressionKind::Signal(signal) = part.unwrap().kind().unwrap() else {
                    panic!("expected a statically selected array element");
                };
                signal.range.unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ranges,
            [
                SlangBitRange { msb: 15, lsb: 8 },
                SlangBitRange { msb: 23, lsb: 16 },
                SlangBitRange { msb: 31, lsb: 24 },
            ]
        );
    }

    let SlangExpressionKind::Cast {
        value, width: 24, ..
    } = assignments[2].rhs().unwrap().kind().unwrap()
    else {
        panic!("expected left-streaming result conversion");
    };
    let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
        panic!("expected slice-size reordering");
    };
    let slices = parts
        .parts()
        .map(|part| match part.unwrap().kind().unwrap() {
            SlangExpressionKind::Extract { lsb, width, .. } => (lsb, width),
            other => panic!("expected a reordered slice, got {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(slices, [(0, 10), (10, 10), (20, 4)]);
}

#[test]
fn native_compile_streams_constant_indexed_unpacked_struct_elements() {
    let source = NativeTestSource::new(
        "typedef struct { logic [3:0] high; bit [3:0] low; } item_t; module top(input item_t values [0:2], output logic [15:0] y); assign y = {>>{values with [0 +: 2]}}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let rhs = module.assigns().next().unwrap().rhs().unwrap();
    let SlangExpressionKind::Cast {
        value, width: 16, ..
    } = rhs.kind().unwrap()
    else {
        panic!("expected unpacked-element streaming conversion");
    };
    let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
        panic!("expected flattened unpacked struct elements");
    };
    let mut ranges = Vec::new();
    for element in parts.parts() {
        let SlangExpressionKind::Concat(fields) = element.unwrap().kind().unwrap() else {
            panic!("expected one bitstream concatenation per unpacked struct element");
        };
        for field in fields.parts() {
            let SlangExpressionKind::Signal(signal) = field.unwrap().kind().unwrap() else {
                panic!("expected an unpacked struct field slice");
            };
            ranges.push(signal.range.unwrap());
        }
    }
    assert_eq!(
        ranges,
        [
            SlangBitRange { msb: 7, lsb: 4 },
            SlangBitRange { msb: 3, lsb: 0 },
            SlangBitRange { msb: 15, lsb: 12 },
            SlangBitRange { msb: 11, lsb: 8 },
        ]
    );
}

#[test]
fn native_compile_fills_streaming_with_oob_elements_and_left_aligns_widening() {
    let source = NativeTestSource::new(
        "module top(input logic [7:0] four [3:0], input bit [7:0] two [0:3], output logic [15:0] four_fill, output logic signed [31:0] two_fill); assign four_fill = {>>{four with [4:3]}}; assign two_fill = {>>{two with [3:4]}}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    let SlangExpressionKind::Cast {
        value, width: 16, ..
    } = assignments[0].rhs().unwrap().kind().unwrap()
    else {
        panic!("expected four-state streaming conversion");
    };
    let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
        panic!("expected four-state fill concatenation");
    };
    let parts = parts.parts().map(Result::unwrap).collect::<Vec<_>>();
    assert!(matches!(
        parts[0].kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(8),
            bits: "xxxxxxxx",
            ..
        })
    ));
    assert!(matches!(
        parts[1].kind().unwrap(),
        SlangExpressionKind::Signal(SlangSignalRef {
            range: Some(SlangBitRange { msb: 7, lsb: 0 }),
            ..
        })
    ));

    let SlangExpressionKind::Cast {
        value,
        width: 32,
        signed: true,
        ..
    } = assignments[1].rhs().unwrap().kind().unwrap()
    else {
        panic!("expected signed widened streaming conversion");
    };
    let SlangExpressionKind::Concat(aligned) = value.kind().unwrap() else {
        panic!("expected a left-aligned widened stream");
    };
    let aligned = aligned.parts().map(Result::unwrap).collect::<Vec<_>>();
    assert!(matches!(
        aligned[1].kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(16),
            bits: "0000000000000000",
            ..
        })
    ));
    let SlangExpressionKind::Concat(selected) = aligned[0].kind().unwrap() else {
        panic!("expected selected two-state elements");
    };
    let selected = selected.parts().map(Result::unwrap).collect::<Vec<_>>();
    assert!(matches!(
        selected[1].kind().unwrap(),
        SlangExpressionKind::Constant(SlangLogicConstant {
            width: Some(8),
            bits: "00000000",
            ..
        })
    ));
}

#[test]
fn native_compile_lowers_runtime_streaming_with_bases_for_all_orientations() {
    fn contains_signed_unit_constant(expression: SlangExpression<'_>, negative: bool) -> bool {
        match expression.kind().unwrap() {
            SlangExpressionKind::Constant(constant) if constant.signed => {
                if negative {
                    constant.bits.bytes().all(|bit| bit == b'1')
                } else {
                    !constant.bits.is_empty()
                        && constant.bits.ends_with('1')
                        && constant.bits[..constant.bits.len() - 1]
                            .bytes()
                            .all(|bit| bit == b'0')
                }
            }
            SlangExpressionKind::Binary { left, right, .. } => {
                contains_signed_unit_constant(left, negative)
                    || contains_signed_unit_constant(right, negative)
            }
            SlangExpressionKind::Cast { value, .. } => {
                contains_signed_unit_constant(value, negative)
            }
            _ => false,
        }
    }

    let source = NativeTestSource::new(
        "module top(input logic signed [4:0] base, input logic [7:0] ascending [10:13], input logic [7:0] descending [13:10], output logic [15:0] asc_up, asc_down, desc_up, desc_down); assign asc_up = {>>{ascending with [base +: 2]}}; assign asc_down = {>>{ascending with [base -: 2]}}; assign desc_up = {>>{descending with [base +: 2]}}; assign desc_down = {>>{descending with [base -: 2]}}; endmodule\n",
    );
    let compilation = compile_source(&source);
    let module = first_module(&compilation);
    let assignments = module.assigns().collect::<Vec<_>>();

    for assignment in &assignments {
        let SlangExpressionKind::Cast {
            value, width: 16, ..
        } = assignment.rhs().unwrap().kind().unwrap()
        else {
            panic!("expected runtime streaming conversion");
        };
        let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
            panic!("expected two runtime-selected elements");
        };
        let parts = parts.parts().map(Result::unwrap).collect::<Vec<_>>();
        assert_eq!(parts.len(), 2);
        for part in parts {
            let SlangExpressionKind::Mux {
                condition,
                then_value,
                else_value,
            } = part.kind().unwrap()
            else {
                panic!("expected out-of-range fill mux");
            };
            assert!(matches!(
                condition.kind().unwrap(),
                SlangExpressionKind::Binary {
                    op: SlangBinaryOp::LogicalAnd,
                    ..
                }
            ));
            assert!(matches!(
                then_value.kind().unwrap(),
                SlangExpressionKind::DynamicExtract { width: 8, .. }
            ));
            assert!(matches!(
                else_value.kind().unwrap(),
                SlangExpressionKind::Constant(SlangLogicConstant {
                    width: Some(8),
                    bits: "xxxxxxxx",
                    ..
                })
            ));
        }
    }

    for (assignment_index, part_index, negative) in
        [(0, 1, false), (1, 0, true), (2, 0, false), (3, 1, true)]
    {
        let SlangExpressionKind::Cast { value, .. } =
            assignments[assignment_index].rhs().unwrap().kind().unwrap()
        else {
            unreachable!();
        };
        let SlangExpressionKind::Concat(parts) = value.kind().unwrap() else {
            unreachable!();
        };
        let part = parts.parts().nth(part_index).unwrap().unwrap();
        let SlangExpressionKind::Mux { then_value, .. } = part.kind().unwrap() else {
            unreachable!();
        };
        let SlangExpressionKind::DynamicExtract { offset, .. } = then_value.kind().unwrap() else {
            unreachable!();
        };
        assert!(contains_signed_unit_constant(offset, negative));
    }
}

#[test]
fn native_compile_reports_structured_streaming_with_profile_failures() {
    for (text, expected) in [
        (
            "module top(input logic [7:0] values [0:3], input logic [1:0] left, right, output logic [31:0] y); assign y = {>>{values with [left:right]}}; endmodule\n",
            "streaming simple with-range requires constant bounds",
        ),
        (
            "typedef union { logic [7:0] first; logic [7:0] second; } item_t; module top(input item_t value, output logic [7:0] y); assign y = {>>{value}}; endmodule\n",
            "streaming aggregate form 'UnpackedUnionType'",
        ),
    ] {
        let source = NativeTestSource::new(text);
        let error = compile(
            std::slice::from_ref(&source.path),
            &SlangCompileOptions::default(),
        )
        .expect_err("unsupported streaming shape should fail during lowering");
        let SlangError::LoweringFailed(failure) = error else {
            panic!("expected a structured lowering failure, got {error}");
        };
        assert_eq!(
            failure.category,
            SlangLoweringFailureCategory::UnsupportedProfile
        );
        assert!(failure.message.contains(expected), "{failure:?}");
        let location = failure
            .location
            .expect("with-clause failure has a source span");
        assert_eq!(location.path, source.path);
        assert_eq!(location.line, 1);
        assert!(location.column > 0);
    }
}

#[test]
fn native_compile_bounds_streaming_bitstream_expansion() {
    for text in [
        "module top(input bit values [0:65536], output bit [65536:0] y); assign y = {>>{values}}; endmodule\n",
        "module top(input bit [65536:0] value, output bit [65536:0] y); assign y = {<<1{value}}; endmodule\n",
    ] {
        let source = NativeTestSource::new(text);
        let error = compile(
            std::slice::from_ref(&source.path),
            &SlangCompileOptions::default(),
        )
        .expect_err("oversized streaming expansion should fail during lowering");
        let SlangError::LoweringFailed(failure) = error else {
            panic!("expected a structured lowering failure, got {error}");
        };
        assert_eq!(failure.category, SlangLoweringFailureCategory::Capacity);
        assert!(
            failure
                .message
                .contains("deterministic expansion limit of 65536 parts"),
            "{failure:?}"
        );
        assert!(failure.location.is_some());
    }
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
