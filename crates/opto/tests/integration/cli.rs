// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn opto() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_opto"));
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("OPTO_")) {
            command.env_remove(key);
        }
    }
    command
}

fn output_text(output: &Output) -> String {
    format!(
        "status={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn test_mapping_library() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../qualification/libraries/opto_test.lib")
}

fn test_target_setup() -> String {
    let library = test_mapping_library();
    format!("read_libs [list {}];", library.display())
}

#[test]
fn dc_style_version_flag_is_supported() {
    let output = opto().arg("-version").output().unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn color_and_theme_flags_preserve_batch_output() {
    let output = opto()
        .args([
            "--color", "always", "--theme", "light", "-no_init", "-x", "echo ok",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn piped_stdin_is_promptless_and_supports_multiline_tcl() {
    let mut child = opto()
        .arg("-no_init")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"if {1} {\n  echo ok\n}\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn x_evaluates_tcl_while_skipping_dc_setup_files() {
    let output = opto().args(["-no_init", "-x", "echo ok"]).output().unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn threads_accepts_a_positive_worker_limit() {
    let output = opto()
        .args(["-no_init", "--threads", "1", "-x", "echo ok"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn threads_rejects_zero() {
    let output = opto()
        .args(["-no_init", "--threads", "0", "-x", "echo ok"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("invalid value '0'"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn embedded_tcl_reports_the_pinned_patchlevel_and_library() {
    let output = opto()
        .args([
            "-no_init",
            "-x",
            "list [info patchlevel] $tcl_library [file exists opto:/tcl8.6/init.tcl]",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "8.6.11 opto:/tcl8.6 1\n"
    );
}

#[test]
fn opto_init_file_controls_match_the_startup_surface() {
    let root = temp_path("opto-init-files");
    let home = root.join("home");
    let local = root.join("work");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(home.join(".opto.tcl"), "lappend ::opto_setup_order home\n").unwrap();
    std::fs::write(
        local.join(".opto.tcl"),
        "lappend ::opto_setup_order local\n",
    )
    .unwrap();

    let run = |flags: &[&str], command: &str| {
        let mut process = opto();
        process
            .env("HOME", &home)
            .current_dir(&local)
            .args(flags)
            .args(["-x", command]);
        process.output().unwrap()
    };

    let all = run(&[], "set ::opto_setup_order");
    assert!(all.status.success(), "{}", output_text(&all));
    assert_eq!(String::from_utf8_lossy(&all.stdout), "home local\n");

    let no_home = run(&["-no_home_init"], "set ::opto_setup_order");
    assert!(no_home.status.success(), "{}", output_text(&no_home));
    assert_eq!(String::from_utf8_lossy(&no_home.stdout), "local\n");

    let no_local = run(&["-no_local_init"], "set ::opto_setup_order");
    assert!(no_local.status.success(), "{}", output_text(&no_local));
    assert_eq!(String::from_utf8_lossy(&no_local.stdout), "home\n");

    let none = run(&["-no_init"], "info exists ::opto_setup_order");
    assert!(none.status.success(), "{}", output_text(&none));
    assert_eq!(String::from_utf8_lossy(&none.stdout), "0\n");

    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn opto_does_not_depend_on_a_dynamic_tcl_library() {
    let output = Command::new("ldd")
        .arg(env!("CARGO_BIN_EXE_opto"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", output_text(&output));
    let libraries = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    assert!(!libraries.contains("libtcl"), "{libraries}");
}

#[test]
fn f_runs_tcl_script() {
    let script = temp_tcl("cli-script.tcl", "puts cli_script_ok\n");
    let output = opto()
        .arg("-no_init")
        .arg("-f")
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "cli_script_ok\n");
}

#[test]
fn f_reports_tcl_errors_with_the_source_line() {
    let script = temp_tcl(
        "cli-script-error.tcl",
        "set ready 1\nunknown_command argument\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-f")
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(&script).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("error:"), "{stderr}");
    assert!(!stderr.contains("OPT-"), "{stderr}");
    assert!(
        stderr.contains(&format!("{}:2:1", script.display())),
        "{stderr}"
    );
    assert!(stderr.contains("unknown_command argument"), "{stderr}");
    assert!(!output.stderr.contains(&0x1b), "{stderr}");
}

#[test]
fn verilog_syntax_errors_point_at_the_exact_column() {
    let source = temp_sv(
        "cli-source-diagnostic.sv",
        "module top(input logic a, output logic y);\n  assign y = ;\nendmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!("read_hdl {}", source.display()))
        .output()
        .unwrap();
    std::fs::remove_file(&source).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("expected expression"), "{stderr}");
    assert!(
        stderr.contains(&format!("{}:2:14", source.display())),
        "{stderr}"
    );
    assert!(stderr.contains("assign y = ;"), "{stderr}");
}

#[test]
fn f_runs_database_configuration_script() {
    let script = temp_tcl(
        "cli-database-settings.tcl",
        "set_db hdl_search_path [list rtl include]\nputs [get_db hdl_search_path]\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-f")
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "rtl include\n");
}

#[test]
fn f_runs_write_command_script() {
    let source = temp_sv(
        "cli-script-write-source.v",
        "module top(input wire a, output wire y); assign y = a; endmodule\n",
    );
    let output_path = temp_path("cli-script-write.v");
    let script = temp_tcl(
        "cli-write.tcl",
        &format!(
            "read_hdl {{{}}}\nelaborate top\nwrite_hdl {{{}}}\n",
            source.display(),
            output_path.display()
        ),
    );
    let output = opto()
        .arg("-no_init")
        .arg("-f")
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(script).unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert!(output_path.is_file());
    std::fs::remove_file(output_path).unwrap();
}

#[test]
fn x_round_trips_database_search_paths() {
    let output = opto()
        .args(["-no_init", "-x"])
        .arg("set_db lib_search_path [list lib slow]; get_db lib_search_path")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "lib slow\n");
}

#[test]
fn read_hdl_flow_uses_native_slang_bridge() {
    let source = temp_sv(
        "cli-native-bridge.sv",
        "module top(input logic [`WIDTH-1:0] a, output logic [`WIDTH-1:0] y); assign y = a; endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_hdl -define {{WIDTH=4 DEBUG}} {}; elaborate top; get_db [get_db current_design] .name",
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "top\n");
}

#[test]
fn read_hdl_loads_designs_without_implicit_current_design() {
    let source = temp_sv(
        "cli-read-file-verilog.v",
        "module child(input a, output y); assign y = a; endmodule\nmodule top(input a, output y); child u_child(.a(a), .y(y)); endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; puts [get_db [get_db current_design] .name]; puts [get_db [get_db designs] .name]",
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "top\nchild top\n");
}

const REMOVED_SYNTHESIS_OVERRIDES: [(&str, &str); 5] = [
    ("OPTO_MUL_ARCH", "array"),
    ("OPTO_NO_REWRITE", "1"),
    ("OPTO_NO_ARITHMETIC_FUSION", "1"),
    ("OPTO_AREA_EVAL_BUDGET", "0"),
    ("OPTO_TIMING_EVAL_BUDGET", "1"),
];

fn qor_report(name: &str, overrides: &[(&str, &str)]) -> String {
    let source = temp_sv(
        name,
        "module top(input [3:0] a, input [3:0] b, output [7:0] y); assign y = a * b; endmodule\n",
    );
    let mut command = opto();
    for (key, value) in overrides {
        command.env(key, value);
    }
    let output = command
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "{} read_hdl {}; elaborate top; synth; report_qor",
            test_target_setup(),
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn removed_synthesis_overrides_are_neither_accepted_nor_honored() {
    let baseline = qor_report("cli-qor-baseline.v", &[]);
    let overridden = qor_report("cli-qor-overridden.v", &REMOVED_SYNTHESIS_OVERRIDES);

    assert_eq!(
        strip_volatile_report_fields(&overridden),
        strip_volatile_report_fields(&baseline),
        "a removed override changed the result"
    );
}

#[test]
fn a_removed_override_never_appears_in_a_report() {
    let report = qor_report("cli-qor-no-controls.v", &REMOVED_SYNTHESIS_OVERRIDES);

    for (key, _) in REMOVED_SYNTHESIS_OVERRIDES {
        assert!(!report.contains(key), "{report}");
    }
    assert!(!report.contains("multiplier"), "{report}");
    assert!(!report.contains("budget"), "{report}");
}

fn strip_volatile_report_fields(report: &str) -> String {
    report
        .lines()
        .filter(|line| !line.starts_with("Date:") && !line.starts_with("Design:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn read_hdl_always_comb_flow_uses_native_slang_bridge() {
    let source = temp_sv(
        "cli-native-always-comb.sv",
        "module top(input logic a, output logic y); always_comb begin y = a; end endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; puts [get_db [get_db current_design] .name]; check_design",
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "top\n1\n");
}

#[test]
fn synthesize_requires_a_mapping_library() {
    let source = temp_sv(
        "cli-synthesis-requires-target-library.sv",
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; synth",
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("synthesis requires a non-empty target library"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn rejects_multiple_continuous_drivers_before_netlist_aliasing() {
    let source = temp_sv(
        "cli-multiple-continuous-drivers.sv",
        "module top(input logic a, b, output wire y); assign y = a; assign y = b; endmodule\n",
    );
    let target_setup = test_target_setup();
    for command in ["check_design", "synth"] {
        let output = opto()
            .arg("-no_init")
            .arg("-x")
            .arg(format!(
                "{target_setup} read_hdl {}; elaborate top; {command}",
                source.display()
            ))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(
            output_text(&output).contains("signal 'y' bit 0 has multiple drivers"),
            "{}",
            output_text(&output)
        );
    }
    std::fs::remove_file(source).unwrap();
}

#[test]
fn combinational_cycle_reports_hdl_before_the_synthesis_invocation() {
    let source = temp_sv(
        "cli-combinational-cycle.sv",
        concat!(
            "module top(input logic a, output logic y);\n",
            "  assign y = y & a;\n",
            "endmodule\n",
        ),
    );
    let script = temp_tcl(
        "cli-combinational-cycle.tcl",
        &format!(
            "{}\nread_hdl {}\nelaborate top\nsynth\n",
            test_target_setup(),
            source.display()
        ),
    );
    let output = opto()
        .args(["--color", "never", "-no_init", "-f"])
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(&source).unwrap();
    std::fs::remove_file(&script).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error[OPT-SYN-001]: combinational loop detected in module 'top'"),
        "{}",
        output_text(&output)
    );
    assert!(
        stderr.contains("assign y = y & a"),
        "{}",
        output_text(&output)
    );
    assert!(
        stderr.contains("feedback path enters a bitwise AND expression"),
        "{}",
        output_text(&output)
    );
    assert!(
        stderr.contains("note: command invocation"),
        "{}",
        output_text(&output)
    );
    assert!(stderr.contains("synth"), "{}", output_text(&output));
    assert!(!stderr.contains("ValueId"), "{}", output_text(&output));
    assert!(!stderr.contains("OpKind"), "{}", output_text(&output));
    assert!(
        stderr.find("assign y = y & a").unwrap() < stderr.find("note: command invocation").unwrap(),
        "{}",
        output_text(&output)
    );
}

#[test]
fn rejects_internal_drive_of_externally_driven_input_port() {
    let source = temp_sv(
        "cli-internal-input-driver.sv",
        "module top(input wire a, b, output wire y); assign a = b; assign y = a; endmodule\n",
    );
    let target_setup = test_target_setup();
    for command in ["check_design", "synth"] {
        let output = opto()
            .arg("-no_init")
            .arg("-x")
            .arg(format!(
                "{target_setup} read_hdl {}; elaborate top; {command}",
                source.display()
            ))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(
            output_text(&output).contains("signal 'a' bit 0 has multiple drivers"),
            "{}",
            output_text(&output)
        );
    }
    std::fs::remove_file(source).unwrap();
}

#[test]
fn rejects_hierarchical_output_and_continuous_driver_collision() {
    let source = temp_sv(
        "cli-hierarchical-output-driver-collision.sv",
        "module child(input logic a, output wire y); assign y = ~a; endmodule module top(input logic a, output wire y); child u_child(.a(a), .y(y)); assign y = a; endmodule\n",
    );
    let target_setup = test_target_setup();
    for command in ["check_design", "synth"] {
        let output = opto()
            .arg("-no_init")
            .arg("-x")
            .arg(format!(
                "{target_setup} read_hdl {}; elaborate top; {command}",
                source.display()
            ))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(
            output_text(&output).contains("signal 'y' bit 0 has multiple drivers"),
            "{}",
            output_text(&output)
        );
    }
    std::fs::remove_file(source).unwrap();
}

#[test]
fn rejects_library_output_and_continuous_driver_collision() {
    let library = temp_path("cli-library-output-driver-collision.lib");
    std::fs::write(
        &library,
        "library (demo) { cell (BUF_X1) { pin (A) { direction : input; } pin (Y) { direction : output; function : \"A\"; } } }\n",
    )
    .unwrap();
    let source = temp_sv(
        "cli-library-output-driver-collision.sv",
        "module top(input logic a, output wire y); BUF_X1 u_buffer(.A(a), .Y(y)); assign y = a; endmodule\n",
    );
    for command in ["check_design", "synth"] {
        let output = opto()
            .arg("-no_init")
            .arg("-x")
            .arg(format!(
                "read_libs [list {0}]; read_hdl {1}; elaborate top; {command}",
                library.display(),
                source.display()
            ))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", output_text(&output));
        assert!(
            output_text(&output).contains("signal 'y' bit 0 has multiple drivers"),
            "{}",
            output_text(&output)
        );
    }
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(library).unwrap();
}

#[test]
fn rejects_unknown_library_instance_port() {
    let library = temp_path("cli-unknown-library-port.lib");
    std::fs::write(
        &library,
        "library (demo) { cell (BUF_X1) { pin (A) { direction : input; } pin (Y) { direction : output; function : \"A\"; } } }\n",
    )
    .unwrap();
    let source = temp_sv(
        "cli-unknown-library-port.sv",
        "module top(input logic a, output wire y); BUF_X1 u_buffer(.A(a), .Z(y)); endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_libs {}; read_hdl {}; elaborate top; check_design",
            library.display(),
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(library).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("instance 'u_buffer' references unknown port 'BUF_X1.Z'"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn rejects_vector_connection_to_scalar_library_pin() {
    let library = temp_path("cli-vector-library-pin.lib");
    std::fs::write(
        &library,
        "library (demo) { cell (BUF_X1) { pin (A) { direction : input; } pin (Y) { direction : output; function : \"A\"; } } }\n",
    )
    .unwrap();
    let source = temp_sv(
        "cli-vector-library-pin.sv",
        "module top(input logic [1:0] a, output wire y); BUF_X1 u_buffer(.A(a), .Y(y)); endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_libs {}; read_hdl {}; elaborate top; check_design",
            library.display(),
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(library).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("instance 'u_buffer' port 'BUF_X1.A' expects width 1, got 2"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn check_design_rejects_unresolved_instance_reference() {
    let source = temp_sv(
        "cli-unresolved-instance-reference.sv",
        "module top(input logic a, output wire y); MISSING u_missing(.A(a), .Y(y)); endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; check_design",
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("unresolved reference 'MISSING'"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn rejects_multiple_procedural_drivers() {
    let source = temp_sv(
        "cli-multiple-procedural-drivers.sv",
        "module top(input wire a, b, output reg y); always @* y = a; always @* y = b; endmodule\n",
    );
    let output = opto()
        .arg("-no_init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; check_design",
            source.display()
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        output_text(&output).contains("signal 'y' bit 0 has multiple drivers"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn read_hdl_rejects_unknown_options_before_bridge() {
    let output = opto()
        .args(["-no_init", "-x", "read_hdl -bad_option rtl/top.sv"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("read_hdl: unsupported option '-bad_option'"));
}

#[test]
fn get_clocks_behaves_like_a_collection_command() {
    let output = opto()
        .args([
            "-no_init",
            "-x",
            "create_clock -period 10 -name sys_clk; llength [get_clocks sys*]",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "1\n");
}

fn temp_sv(name: &str, text: &str) -> PathBuf {
    let path = temp_path(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn temp_tcl(name: &str, text: &str) -> PathBuf {
    let path = temp_path(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn temp_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("opto-cli-{}-{name}", std::process::id()));
    path
}
