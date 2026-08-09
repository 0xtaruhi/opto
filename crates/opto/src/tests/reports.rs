// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn zero_argument_design_and_report_commands_reject_extra_words() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    for command in [
        "check_design unexpected",
        "report_area unexpected",
        "report_qor unexpected",
        "report_clock unexpected",
        "check_timing unexpected",
    ] {
        let error = runtime.eval(command).unwrap_err();
        assert!(
            error.to_string().contains("wrong number of arguments"),
            "{command}: {error}"
        );
    }
}

#[test]
fn report_timing_requires_current_design() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let err = runtime.eval("report_timing").unwrap_err();
    assert!(
        err.to_string()
            .contains("no current design; use elaborate or set_db current_design")
    );
}

#[test]
fn report_resources_tracks_synthesis_state_and_source_location() {
    let source = temp_script_path("report-resources.sv");
    std::fs::write(
        &source,
        "module top(\n  input logic [3:0] a, b,\n  output logic [3:0] y\n);\n  assign y = a + b;\nendmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    runtime.eval(&test_target_setup()).unwrap();

    let before = runtime
        .eval(&format!(
            "read_hdl {}; elaborate top; report_resources",
            tcl_path_word(&source)
        ))
        .unwrap();
    let after = runtime.eval("synth; report_resources").unwrap();
    std::fs::remove_file(&source).unwrap();

    match before {
        EvalResult::Complete(report) => {
            assert!(report.contains("Synth the design before reporting resources"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
    match after {
        EvalResult::Complete(report) => {
            assert!(report.contains("DW01_add"));
            assert!(report.contains(&tcl_path_text(&source)));
            assert!(report.contains("add_5"));
            assert!(report.contains("cla"));
            assert!(report.contains("| Resource"));
            assert!(report.contains("| Width"));
            let modern =
                crate::presentation::render_report(&report, Theme::Dark.palette(), false, Some(56));
            assert!(modern.contains("Resources report"), "{modern}");
            assert!(modern.contains("DW01_add"), "{modern}");
            assert!(modern.contains("Width"), "{modern}");
            assert!(modern.contains('4'), "{modern}");
            assert!(modern.contains('─'), "{modern}");
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn report_resources_accepts_design_lists_and_hierarchy() {
    let source = temp_script_path("report-resources-hierarchy.sv");
    std::fs::write(
        &source,
        "module child(input logic [1:0] a, b, output logic [1:0] y); assign y = a + b; endmodule\nmodule top(input logic [1:0] a, b, output logic [1:0] y); child u_child(.a(a), .b(b), .y(y)); endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    runtime.eval(&test_target_setup()).unwrap();
    runtime
        .eval(&format!(
            "read_hdl {}; elaborate top; synth",
            tcl_path_word(&source)
        ))
        .unwrap();

    let hierarchy = runtime.eval("report_resources -hierarchy").unwrap();
    let design_list = runtime.eval("report_resources {child top}").unwrap();
    let ignored_hierarchy = runtime.eval("report_resources -hierarchy {top}").unwrap();
    let context_error = runtime.eval("report_resources -context").unwrap_err();
    std::fs::remove_file(&source).unwrap();

    for result in [hierarchy, design_list] {
        match result {
            EvalResult::Complete(report) => {
                assert_eq!(report.matches("# Resources report").count(), 1);
                assert!(report.contains("## top"));
                assert!(report.contains("## child"));
                assert!(report.contains("Design: top"));
                assert!(report.contains("Design: child"));
            }
            EvalResult::Exit(code) => panic!("unexpected exit {code}"),
        }
    }
    match ignored_hierarchy {
        EvalResult::Complete(report) => {
            assert_eq!(report.matches("# Resources report").count(), 1);
            assert!(!report.contains("## top"));
            assert!(report.contains("Design: top"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
    assert!(
        context_error
            .to_string()
            .contains("option '-context' is not implemented yet")
    );
}

#[test]
fn report_timing_accepts_documented_delay_type_selectors() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    for command in [
        "report_timing -delay min",
        "report_timing -delay_type min",
        "report_timing -significant_digits 6",
    ] {
        let err = runtime.eval(command).unwrap_err();
        assert!(
            err.to_string()
                .contains("no current design; use elaborate or set_db current_design")
        );
    }
    let err = runtime
        .eval("report_timing -delay_type earliest")
        .unwrap_err();
    assert!(err.to_string().contains("must be max or min"));

    let err = runtime
        .eval("report_timing -delay_type min_max")
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("report_timing: -delay_type min_max is not implemented yet")
    );
}

#[test]
fn get_clocks_returns_collection_handle() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval("create_clock -period 10 -name sys_clk; llength [get_clocks sys*]")
        .unwrap();

    match result {
        EvalResult::Complete(count) => assert_eq!(count, "1"),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn set_clock_transition_accepts_edge_and_delay_options() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(
            "create_clock -period 10 -name sys_clk; set_clock_transition -rise -max 0.05 [get_clocks sys_clk]",
        )
        .unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
}

#[test]
fn design_rule_commands_use_documented_names_and_object_lists() {
    let source = temp_script_path("opto-design-rules.sv");
    std::fs::write(
        &source,
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    runtime
        .eval(&format!(
            "read_hdl {}; elaborate top",
            tcl_path_word(&source)
        ))
        .unwrap();

    for command in [
        "set_max_transition 0.2 [get_db current_design]",
        "set_max_capacitance 1.5 [get_db current_design]",
        "set_max_fanout 8 [get_db current_design]",
        "set_max_transition 0.2 [get_ports y]",
        "set_max_capacitance 1.5 [get_ports y]",
    ] {
        let result = runtime.eval(command).unwrap();
        assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
    }

    let scoped = runtime
        .eval(
            "create_clock -period 10 -name sys_clk; set_max_transition 0.1 -data_path [get_clocks sys_clk]; set_max_capacitance 0.2 -clock_path [get_clocks sys_clk]",
        )
        .unwrap();
    assert!(matches!(scoped, EvalResult::Complete(value) if value == "1"));

    let fanout_option = runtime
        .eval("set_max_fanout 2 -data_path [get_clocks sys_clk]")
        .unwrap_err();
    assert!(
        fanout_option
            .to_string()
            .contains("unsupported option '-data_path'")
    );
    let fanout_output = runtime.eval("set_max_fanout 2 [get_ports y]").unwrap_err();
    assert!(
        fanout_output
            .to_string()
            .contains("object class 'port' is not valid")
    );
    let _ = std::fs::remove_file(source);
}
