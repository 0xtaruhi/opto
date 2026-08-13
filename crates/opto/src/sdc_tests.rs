// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn isolates_non_sdc_commands_before_applying_constraints() {
    let script = temp_script_path("domain.tcl");
    std::fs::write(
        &script,
        "set search_path changed\ncreate_clock -period 7 -name before_error\nsynthesis\ncreate_clock -period 3 -name after_error\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let error = runtime
        .eval(&format!("read_sdc {}", tcl_path_word(&script)))
        .expect_err("an unsupported SDC command must fail read_sdc");
    let result = runtime
        .eval("list [llength [get_clocks before_error]] [llength [get_clocks after_error]]")
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(error.to_string().contains("synthesis"));
    assert!(matches!(result, EvalResult::Complete(value) if value == "0 0"));
    assert!(
        runtime.eval("help").is_ok(),
        "shell commands were not restored"
    );
}

#[test]
fn does_not_inherit_shell_tcl_variables() {
    let script = temp_script_path("variable-isolation.tcl");
    std::fs::write(
        &script,
        "create_clock -period $PERIOD -name inherited_variable_clock\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    runtime.eval("set PERIOD 7").unwrap();
    let error = runtime
        .eval(&format!("read_sdc {}", tcl_path_word(&script)))
        .expect_err("shell variables must not leak into SDC evaluation");
    let result = runtime
        .eval("llength [get_clocks inherited_variable_clock]")
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(error.to_string().contains("PERIOD"));
    assert!(matches!(result, EvalResult::Complete(value) if value == "0"));
}

#[test]
fn syntax_only_discards_constraint_and_collection_changes() {
    let script = temp_script_path("syntax-only.tcl");
    std::fs::write(
        &script,
        "create_clock -period 5 -name transient_clock\nget_clocks transient_clock\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "list [read_sdc -syntax_only -version 2.1 {}] [llength [get_clocks transient_clock]]",
            tcl_path_word(&script)
        ))
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1 0"));
}

#[test]
fn evaluates_a_successful_sdc_file_exactly_once() {
    let script = temp_script_path("single-execution.tcl");
    let marker = temp_script_path("single-execution.marker");
    std::fs::write(
        &script,
        format!(
            "set fd [open {} a]\nputs $fd evaluated\nclose $fd\ncreate_clock -period 5 -name once\n",
            tcl_path_word(&marker)
        ),
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!("read_sdc {}", tcl_path_word(&script)))
        .unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "evaluated\n");
    std::fs::remove_file(script).unwrap();
    std::fs::remove_file(marker).unwrap();
}

#[test]
fn syntax_only_uses_a_safe_interpreter_without_external_side_effects() {
    let script = temp_script_path("safe-syntax-only.tcl");
    let marker = temp_script_path("safe-syntax-only.marker");
    std::fs::write(
        &script,
        format!(
            "set fd [open {} w]\nputs $fd unsafe\nclose $fd\n",
            tcl_path_word(&marker)
        ),
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let error = runtime
        .eval(&format!("read_sdc -syntax_only {}", tcl_path_word(&script)))
        .expect_err("unsafe commands must fail syntax-only validation");

    assert!(error.to_string().contains("open"));
    assert!(!marker.exists());
    std::fs::remove_file(script).unwrap();
}

#[test]
fn rejects_command_abbreviations_and_nested_read_sdc() {
    let nested = temp_script_path("nested-inner.tcl");
    let abbreviated = temp_script_path("abbreviated.tcl");
    let outer = temp_script_path("nested-outer.tcl");
    std::fs::write(&nested, "create_clock -period 2 -name nested_clock\n").unwrap();
    std::fs::write(
        &abbreviated,
        "create_cloc -period 2 -name abbreviated_clock\n",
    )
    .unwrap();
    std::fs::write(&outer, format!("read_sdc {}\n", tcl_path_word(&nested))).unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let abbreviated_error = runtime
        .eval(&format!("read_sdc {}", tcl_path_word(&abbreviated)))
        .expect_err("command abbreviations must fail read_sdc");
    let nested_error = runtime
        .eval(&format!("read_sdc {}", tcl_path_word(&outer)))
        .expect_err("nested read_sdc must fail");
    std::fs::remove_file(nested).unwrap();
    std::fs::remove_file(abbreviated).unwrap();
    std::fs::remove_file(outer).unwrap();

    assert!(abbreviated_error.to_string().contains("create_cloc"));
    assert!(nested_error.to_string().contains("read_sdc"));
}

#[test]
fn exit_stops_sdc_without_exiting_the_shell() {
    let script = temp_script_path("exit.tcl");
    std::fs::write(
        &script,
        "create_clock -period 4 -name before_exit\nexit\ncreate_clock -period 4 -name after_exit\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "list [read_sdc {}] [llength [get_clocks before_exit]] [llength [get_clocks after_exit]]",
            tcl_path_word(&script)
        ))
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1 1 0"));
}

#[test]
fn normal_and_safe_sdc_dispatch_share_invocation_preflight() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    for (script, expected) in [
        ("set_input_transition 0.2", "wrong number of arguments"),
        ("set_load 1.0", "wrong number of arguments"),
        ("create_clock -period", "missing value for -period"),
        ("set_max_delay 1.0 -from", "missing value for -from"),
        ("get_ports first second", "wrong number of arguments"),
    ] {
        let normal = runtime
            .eval(script)
            .expect_err("normal dispatch must reject");
        let safe = validate_sdc_syntax(script).expect_err("safe dispatch must reject");
        assert!(normal.to_string().contains(expected), "{script}: {normal}");
        assert!(safe.to_string().contains(expected), "{script}: {safe}");
    }

    for script in [
        "create_clock -period -1 -name clk",
        "set_input_transition 0.2 ports",
        "set_load 1.0 ports",
        "set_max_delay 1.0 -from ports",
        "set_min_delay 0.2 -rise_from ports -fall_to ports",
        "set_false_path -setup -from ports -through pins -to ports",
        "set_multicycle_path 2 -hold -start -from clocks -to pins",
    ] {
        validate_sdc_syntax(script).unwrap_or_else(|error| panic!("{script}: {error}"));
    }
}

#[test]
fn syntax_only_validates_commands_in_unexecuted_tcl_paths() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    for (script, expected) in [
        (
            "if {0} { create_clock -period }",
            "missing value for -period",
        ),
        (
            "foreach port {} { set_load 1.0 }",
            "wrong number of arguments",
        ),
        ("if {0} { synth }", "invalid command name \"synth\""),
        (
            "if {0} { frobnicate }",
            "invalid command name \"frobnicate\"",
        ),
        (
            "set command create_clock; if {0} { $command -period 2 }",
            "dynamic command names cannot be validated",
        ),
        (
            "if {0} { set nested [get_ports first second] }",
            "wrong number of arguments",
        ),
    ] {
        let error = validate_sdc_syntax(script).expect_err("dead Tcl path must be validated");
        assert!(error.to_string().contains(expected), "{script}: {error}");
    }
}

fn temp_script_path(name: &str) -> crate::test_support::TestPath {
    crate::test_support::TestPath::new(name)
}
