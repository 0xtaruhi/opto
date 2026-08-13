// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! End-to-end command-line behavior for the public `opto` executable.

#[path = "../support/tcl.rs"]
mod test_tcl;

use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Condvar, Mutex, OnceLock};

use test_tcl::tcl_path_word;

fn bare_opto() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_opto"));
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|key| key.starts_with("OPTO_")) {
            command.env_remove(key);
        }
    }
    command
}

const MAX_CONCURRENT_PRODUCT_PROCESSES: usize = 4;
static PRODUCT_PROCESS_SLOTS: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();

#[derive(Debug)]
struct ProductProcessSlot;

impl ProductProcessSlot {
    fn acquire() -> Self {
        let (active, changed) =
            PRODUCT_PROCESS_SLOTS.get_or_init(|| (Mutex::new(0), Condvar::new()));
        let mut active = active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *active >= MAX_CONCURRENT_PRODUCT_PROCESSES {
            active = changed
                .wait(active)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *active += 1;
        Self
    }
}

impl Drop for ProductProcessSlot {
    fn drop(&mut self) {
        let (active, changed) =
            PRODUCT_PROCESS_SLOTS.get_or_init(|| (Mutex::new(0), Condvar::new()));
        let mut active = active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.checked_sub(1).expect("product process slot balance");
        changed.notify_one();
    }
}

#[derive(Debug)]
struct TestCommand {
    command: Command,
    _slot: ProductProcessSlot,
}

impl Deref for TestCommand {
    type Target = Command;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

impl DerefMut for TestCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}

/// Build a resource-bounded product process for parallel libtest execution.
///
/// The CLI suite owns process behavior, not worker-count determinism. Without
/// this bound, every concurrently running test inherits all host CPUs and the
/// suite can create thousands of worker threads on a large CI runner.
fn opto() -> TestCommand {
    let mut command = bare_opto();
    command.args(["--threads", "1"]);
    TestCommand {
        command,
        _slot: ProductProcessSlot::acquire(),
    }
}

fn output_text(output: &Output) -> String {
    format!(
        "status={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn normalized_stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn test_mapping_library() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../qualification/libraries/opto_test.lib")
}

fn test_target_setup() -> String {
    let library = test_mapping_library();
    format!("read_libs [list {}];", tcl_path_word(&library))
}

#[test]
fn color_and_theme_flags_preserve_batch_output() {
    let output = opto()
        .args([
            "--color",
            "always",
            "--theme",
            "light",
            "--no-init",
            "-x",
            "echo ok",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn piped_stdin_is_promptless_and_supports_multiline_tcl() {
    let mut command = opto();
    let mut child = command
        .arg("--no-init")
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
fn x_evaluates_tcl_without_loading_init_files() {
    let output = opto()
        .args(["--no-init", "-x", "echo ok"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn threads_accepts_a_positive_worker_limit() {
    let output = bare_opto()
        .args(["--no-init", "--threads", "1", "-x", "echo ok"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
}

#[test]
fn threads_rejects_zero() {
    let output = bare_opto()
        .args(["--no-init", "--threads", "0", "-x", "echo ok"])
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
            "--no-init",
            "-x",
            "list [info patchlevel] $tcl_library [file exists opto:/tcl8.6/init.tcl]",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "8.6.18 opto:/tcl8.6 1\n"
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

    let no_home = run(&["--no-home-init"], "set ::opto_setup_order");
    assert!(no_home.status.success(), "{}", output_text(&no_home));
    assert_eq!(String::from_utf8_lossy(&no_home.stdout), "local\n");

    let no_local = run(&["--no-local-init"], "set ::opto_setup_order");
    assert!(no_local.status.success(), "{}", output_text(&no_local));
    assert_eq!(String::from_utf8_lossy(&no_local.stdout), "home\n");

    let none = run(&["--no-init"], "info exists ::opto_setup_order");
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
        .arg("--no-init")
        .arg("-f")
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(normalized_stdout(&output), "cli_script_ok\n");
}

#[test]
fn f_reports_tcl_errors_with_the_source_line() {
    let script = temp_tcl(
        "cli-script-error.tcl",
        "set ready 1\nunknown_command argument\n",
    );
    let output = opto()
        .arg("--no-init")
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!("read_hdl {}", tcl_path_word(&source)))
        .output()
        .unwrap();
    std::fs::remove_file(&source).unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("expected expression"), "{stderr}");
    let source_name = source.file_name().unwrap().to_string_lossy();
    assert!(stderr.contains(&format!("{source_name}:2:14")), "{stderr}");
    assert!(stderr.contains("assign y = ;"), "{stderr}");
}

#[test]
fn successful_verilog_frontend_warnings_are_visible_and_nonfatal() {
    let source = temp_sv(
        "cli-source-warning.sv",
        "module top(output logic [3:0] y); assign y = 8'hff; endmodule\n",
    );
    let output = opto()
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top",
            tcl_path_word(&source)
        ))
        .output()
        .unwrap();
    std::fs::remove_file(&source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(normalized_stdout(&output), "1\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning[OPT-HDL-S"), "{stderr}");
    assert!(stderr.contains("changes value"), "{stderr}");
    assert!(stderr.contains("cli-source-warning.sv:1:"), "{stderr}");
    assert!(stderr.contains("assign y = 8'hff"), "{stderr}");
}

#[test]
fn f_runs_database_configuration_script() {
    let script = temp_tcl(
        "cli-database-settings.tcl",
        "set_db hdl_search_path [list rtl include]\nputs [get_db hdl_search_path]\n",
    );
    let output = opto()
        .arg("--no-init")
        .arg("-f")
        .arg(&script)
        .output()
        .unwrap();
    std::fs::remove_file(script).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(normalized_stdout(&output), "rtl include\n");
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
            "read_hdl {}\nelaborate top\nwrite_hdl {}\n",
            tcl_path_word(&source),
            tcl_path_word(&output_path)
        ),
    );
    let output = opto()
        .arg("--no-init")
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
        .args(["--no-init", "-x"])
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl -define {{WIDTH=4 DEBUG}} {}; elaborate top; get_db [get_db current_design] .name",
            tcl_path_word(&source)
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; puts [get_db [get_db current_design] .name]; puts [get_db [get_db designs] .name]",
            tcl_path_word(&source)
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(normalized_stdout(&output), "top\nchild top\n");
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "{} read_hdl {}; elaborate top; synth; report_qor",
            test_target_setup(),
            tcl_path_word(&source)
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
        .filter(|line| {
            !line.starts_with("Date:")
                && !line.starts_with("Design:")
                && !line.starts_with("Synthesis stage: elapsed=")
                && !line.starts_with("Optimization: elapsed=")
        })
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; puts [get_db [get_db current_design] .name]; check_design",
            tcl_path_word(&source)
        ))
        .output()
        .unwrap();
    std::fs::remove_file(source).unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    assert_eq!(normalized_stdout(&output), "top\n1\n");
}

#[test]
fn synthesis_drives_logic_in_every_unpacked_array_element() {
    let source = temp_sv(
        "cli-unpacked-array-driver.sv",
        concat!(
            "module top(input wire [6:0] a, b, c, input wire [1:0] sel, output wire [6:0] y);\n",
            "  wire [6:0] values [0:3];\n",
            "  assign values[0] = a;\n",
            "  assign values[1] = b;\n",
            "  assign values[2] = c;\n",
            "  assign values[3] = a ^ b ^ c;\n",
            "  assign y = values[sel];\n",
            "endmodule\n",
        ),
    );
    let mapped = temp_path("cli-unpacked-array-driver-mapped.v");
    let output = opto()
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "{} read_hdl {}; elaborate top; synth; write_hdl {}",
            test_target_setup(),
            tcl_path_word(&source),
            tcl_path_word(&mapped),
        ))
        .output()
        .unwrap();
    let netlist = std::fs::read_to_string(&mapped).unwrap_or_default();
    std::fs::remove_file(source).unwrap();
    let _ = std::fs::remove_file(mapped);

    assert!(output.status.success(), "{}", output_text(&output));
    assert!(!netlist.contains("values"), "{netlist}");
    assert!(netlist.matches("XOR2_X1 ").count() >= 7, "{netlist}");
}

#[test]
fn synthesize_requires_a_mapping_library() {
    let source = temp_sv(
        "cli-synthesis-requires-target-library.sv",
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    );
    let output = opto()
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; synth",
            tcl_path_word(&source)
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
            .arg("--no-init")
            .arg("-x")
            .arg(format!(
                "{target_setup} read_hdl {}; elaborate top; {command}",
                tcl_path_word(&source)
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
            tcl_path_word(&source)
        ),
    );
    let output = opto()
        .args(["--color", "never", "--no-init", "-f"])
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
            .arg("--no-init")
            .arg("-x")
            .arg(format!(
                "{target_setup} read_hdl {}; elaborate top; {command}",
                tcl_path_word(&source)
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
            .arg("--no-init")
            .arg("-x")
            .arg(format!(
                "{target_setup} read_hdl {}; elaborate top; {command}",
                tcl_path_word(&source)
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
            .arg("--no-init")
            .arg("-x")
            .arg(format!(
                "read_libs [list {0}]; read_hdl {1}; elaborate top; {command}",
                tcl_path_word(&library),
                tcl_path_word(&source)
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_libs {}; read_hdl {}; elaborate top; check_design",
            tcl_path_word(&library),
            tcl_path_word(&source)
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_libs {}; read_hdl {}; elaborate top; check_design",
            tcl_path_word(&library),
            tcl_path_word(&source)
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; check_design",
            tcl_path_word(&source)
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
        .arg("--no-init")
        .arg("-x")
        .arg(format!(
            "read_hdl {}; elaborate top; check_design",
            tcl_path_word(&source)
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
        .args(["--no-init", "-x", "read_hdl -bad_option rtl/top.sv"])
        .output()
        .unwrap();

    assert!(!output.status.success(), "{}", output_text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("read_hdl: unsupported option '-bad_option'"));
    assert!(stderr.contains("error[OPT-CLI-001]"));
    assert!(stderr.contains("help read_hdl"));
}

#[test]
fn command_help_explains_usage_preconditions_and_an_example() {
    let output = opto()
        .args(["--no-init", "-x", "help read_hdl"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = normalized_stdout(&output);
    for expected in [
        "Summary:",
        "Usage:\n  read_hdl",
        "Options:",
        "Requires:",
        "Example:\n  read_hdl",
    ] {
        assert!(stdout.contains(expected), "{}", output_text(&output));
    }
}

#[test]
fn get_clocks_behaves_like_a_collection_command() {
    let output = opto()
        .args([
            "--no-init",
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
