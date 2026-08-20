// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn read_hdl_accepts_preprocessor_options() {
    let source = temp_script_path("read-hdl-options.sv");
    std::fs::write(
        &source,
        "module top; localparam int W = `WIDTH; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_hdl -define WIDTH=8 -incdir . {}",
            tcl_path_word(&source)
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
}

#[test]
fn read_hdl_can_group_files_into_one_compilation_unit() {
    let first = temp_script_path("read-hdl-compilation-unit-first.sv");
    let second = temp_script_path("read-hdl-compilation-unit-second.sv");
    std::fs::write(&first, "`define SHARED_WIDTH 6\n").unwrap();
    std::fs::write(
        &second,
        "module top(input logic [`SHARED_WIDTH-1:0] a, output logic [`SHARED_WIDTH-1:0] y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_hdl -compilation_unit {} {}; elaborate top",
            tcl_path_word(&first),
            tcl_path_word(&second)
        ))
        .unwrap();
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(second).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
}

#[test]
fn read_hdl_rejects_unknown_options() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    let err = runtime.eval("read_hdl -bad_option rtl/top.sv").unwrap_err();

    assert!(err.to_string().contains("unsupported option '-bad_option'"));
}

#[test]
fn read_hdl_uses_verilog_keywords_for_v_files() {
    let source = temp_script_path("verilog-keyword-context.v");
    std::fs::write(
        &source,
        "module top(input wire bit, output wire y); assign y = bit; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_hdl {}; elaborate top",
            tcl_path_word(&source)
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
}

#[test]
fn read_hdl_keeps_systemverilog_keywords_for_sv_files() {
    let source = temp_script_path("systemverilog-keyword-context.sv");
    std::fs::write(
        &source,
        "module top(input bit a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_hdl {}; elaborate top",
            tcl_path_word(&source)
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
}
