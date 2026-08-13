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

#[test]
fn database_query_schema_rejects_unsupported_forms_precisely() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    for (script, expected) in [
        (
            "get_db not_a_class",
            "unknown root property or object class",
        ),
        (
            "get_db -of bogus libraries",
            "libraries does not support -of",
        ),
        (
            "get_db -if {.name == BUF} lib_cells",
            "lib_cells currently supports name patterns only",
        ),
        (
            "get_db -if {.name == high} synth_effort",
            "'synth_effort' is a root property",
        ),
        (
            "get_db synth_effort high",
            "root property 'synth_effort' does not accept name pattern 'high'",
        ),
    ] {
        let error = runtime.eval(script).expect_err("query must be rejected");
        assert!(error.to_string().contains(expected), "{script}: {error}");
    }
}
