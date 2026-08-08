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
            "read_hdl -define WIDTH=8 -incdir . {{{}}}",
            source.display()
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(result, EvalResult::Complete(value) if value == "1"));
}

#[test]
fn read_hdl_rejects_unknown_options() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    let err = runtime.eval("read_hdl -bad_option rtl/top.sv").unwrap_err();

    assert!(err.to_string().contains("unsupported option '-bad_option'"));
}
