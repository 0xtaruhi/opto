// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn read_hdl_requires_explicit_elaboration() {
    let source = temp_script_path("opto-read-hdl.sv");
    std::fs::write(
        &source,
        "module child(input logic a, output logic y); assign y = a; endmodule\nmodule top(input logic a, output logic y); child u_child(.a(a), .y(y)); endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    runtime
        .eval(&format!("read_hdl {{{}}}", source.display()))
        .unwrap();
    let before = runtime.eval("get_db current_design").unwrap();
    let after = runtime
        .eval("elaborate top; list [get_db [get_db current_design] .name] [get_db [get_db designs] .name]")
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(matches!(before, EvalResult::Complete(value) if value.is_empty()));
    assert!(matches!(after, EvalResult::Complete(value) if value == "top {child top}"));
}

#[test]
fn write_hdl_uses_a_single_unambiguous_form() {
    let source = temp_script_path("opto-write-hdl-source.sv");
    let output = temp_script_path("opto-write-hdl-output.v");
    std::fs::write(
        &source,
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    runtime
        .eval(&format!(
            "read_hdl {{{}}}; elaborate top; write_hdl {{{}}}",
            source.display(),
            output.display()
        ))
        .unwrap();
    let text = std::fs::read_to_string(&output).unwrap();
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(output).unwrap();

    assert!(text.contains("module top"));
}

#[test]
fn save_and_resume_restore_a_session_across_runtimes() {
    let source = temp_script_path("opto-checkpoint-source.sv");
    let checkpoint = temp_script_path("opto-checkpoint.ock");
    std::fs::write(
        &source,
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut writer = Runtime::new(Session::new()).unwrap();
    writer.register_commands().unwrap();
    writer
        .eval(&format!(
            "read_hdl {{{}}}; elaborate top; set_db synth_effort high; set_db clock_gating true; save {{{}}}",
            source.display(),
            checkpoint.display()
        ))
        .unwrap();
    assert_eq!(
        std::fs::read(&checkpoint).unwrap().get(..8),
        Some(b"OPTOCKPT".as_slice())
    );

    let mut reader = Runtime::new(Session::new()).unwrap();
    reader.register_commands().unwrap();
    let restored = reader
        .eval(&format!(
            "resume {{{}}}; list [get_db [get_db current_design] .name] [get_db [get_db designs] .name] [get_db synth_effort] [get_db clock_gating]",
            checkpoint.display()
        ))
        .unwrap();
    assert!(matches!(restored, EvalResult::Complete(value) if value == "top top high true"));

    std::fs::write(&checkpoint, b"invalid-opto-database").unwrap();
    let error = reader
        .eval(&format!("resume {{{}}}", checkpoint.display()))
        .unwrap_err();
    assert!(error.to_string().contains("not an Opto checkpoint"));

    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(checkpoint).unwrap();
}
