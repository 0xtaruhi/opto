// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn synthesis_directives_are_database_properties() {
    let source = temp_script_path("opto-synthesis-directives.sv");
    std::fs::write(
        &source,
        "module child(input logic a, output logic y); assign y = a; endmodule\n\
         module top(input logic a, output logic y); logic n; child u_child(.a(a), .y(n)); assign y = n; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_hdl {{{}}}; elaborate top; set cell [get_cells u_child]; set net [get_nets n]; set_db $cell .dont_touch true; set_db $cell .ungroup false; set_db $net .dont_touch false; list [get_db $cell .dont_touch] [get_db $cell .ungroup] [get_db $net .dont_touch]",
            source.display()
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "true false false"));
}

#[test]
fn synthesis_database_properties_reject_invalid_values_and_object_classes() {
    let source = temp_script_path("opto-synthesis-directive-errors.sv");
    std::fs::write(
        &source,
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    runtime
        .eval(&format!("read_hdl {{{}}}; elaborate top", source.display()))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    let boolean = runtime
        .eval("set_db [get_db designs top] .dont_touch maybe")
        .unwrap_err();
    assert!(boolean.to_string().contains("expected boolean"));
    let object = runtime
        .eval("set_db [get_ports a] .dont_touch true")
        .unwrap_err();
    assert!(object.to_string().contains("do not support"));
}
