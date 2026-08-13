// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn install_hierarchy(runtime: &mut Runtime, name: &str) -> crate::test_support::TestPath {
    let source = temp_script_path(name);
    std::fs::write(
        &source,
        "module child(input logic a, output logic y); assign y = a; endmodule\nmodule top(input logic a, input logic b, input logic clk, output logic y); logic n; child u_child(.a(a), .y(n)); assign y = n & b; endmodule\n",
    )
    .unwrap();
    runtime
        .eval(&format!(
            "read_hdl {}; elaborate top",
            tcl_path_word(&source)
        ))
        .unwrap();
    source
}

#[test]
fn object_queries_return_native_tcl_lists_of_stable_handles() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    let source = install_hierarchy(&mut runtime, "opto-object-lists.sv");

    let result = runtime
        .eval("set ports [get_ports *]; set names {}; foreach port $ports {lappend names [get_db $port .name]}; list [llength $ports] $names")
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "4 {a b clk y}"));
}

#[test]
fn projections_preserve_names_containing_whitespace() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval("create_clock -period 2 -name {clock one}; set names [get_db [get_clocks *] .name]; list [llength $names] [lindex $names 0]")
        .unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1 {clock one}"));
}

#[test]
fn object_queries_support_inline_filters() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    let source = install_hierarchy(&mut runtime, "opto-object-filters.sv");

    let result = runtime
        .eval("create_clock -period 10 -name clk1 [get_ports clk]; list [get_db [get_ports -filter {.direction == in} *] .name] [get_db [get_db -if {.ref_name == child} insts *] .name] [get_db [get_db -if {.period == 10} clocks *] .name]")
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(
        matches!(result, EvalResult::Complete(ref value) if value == "{a b clk} u_child clk1"),
        "{result:?}"
    );
}

#[test]
fn related_object_queries_use_handle_lists() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    let source = install_hierarchy(&mut runtime, "opto-related-objects.sv");

    let result = runtime
        .eval("set inst [get_db insts u_child]; set pins [get_db -of $inst pins]; set nets [get_db -of $pins nets]; list [get_db $pins .name] [get_db $nets .name]")
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(
        matches!(result, EvalResult::Complete(ref value) if value == "{a y} {a n}"),
        "{result:?}"
    );
}
