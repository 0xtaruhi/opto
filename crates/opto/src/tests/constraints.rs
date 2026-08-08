// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_session::{EdgeSelection, ExceptionCorner, PathExceptionKind};

#[test]
fn read_sdc_evaluates_clock_constraints() {
    let script = temp_script_path("opto-clock.sdc");
    std::fs::write(
        &script,
        "create_clock -period 10 -waveform {0 5} -name sys_clk\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!("read_sdc {}; report_clock", script.display()))
        .unwrap();
    std::fs::remove_file(script).unwrap();

    match result {
        EvalResult::Complete(report) => {
            assert!(report.starts_with("# Clock report"));
            assert!(report.contains("| sys_clk"));
            assert!(report.contains("| 10.000"));
            assert!(report.contains("| {0.000 5.000}"));
            assert!(report.contains("| <virtual>"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn generated_clock_derives_period_and_waveform() {
    let source = temp_script_path("opto-generated-clock.sv");
    std::fs::write(
        &source,
        "module top(input logic clk, input logic gclk, output logic y); assign y = gclk; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_hdl {}; elaborate top; \
             create_clock -name master -period 10 [get_ports clk]; \
             create_generated_clock -name divided -source [get_ports clk] \
               -master_clock [get_clocks master] -divide_by 2 [get_ports gclk]; \
             report_clock",
            source.display()
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    match result {
        EvalResult::Complete(report) => {
            assert!(report.contains("| master"));
            assert!(report.contains("| divided"));
            assert!(report.contains("| 20.000"));
            assert!(report.contains("| {0.000 10.000}"));
            assert!(report.contains("| gclk"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }

    let sizes = runtime
        .eval(
            "list [llength [all_inputs]] \
                  [llength [all_inputs -no_clocks]] \
                  [llength [all_outputs]] \
                  [llength [all_clocks]]",
        )
        .unwrap();
    match sizes {
        EvalResult::Complete(sizes) => assert_eq!(sizes, "2 0 1 2"),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }

    runtime
        .eval("create_clock -name companion -period 4 -add [get_ports clk]")
        .unwrap();
    let written = temp_script_path("opto-written.sdc");
    runtime
        .eval(&format!("write_sdc {}", written.display()))
        .unwrap();
    let contents = std::fs::read_to_string(&written).unwrap();
    std::fs::remove_file(written).unwrap();
    assert!(contents.contains("create_clock"));
    assert!(contents.contains("create_generated_clock"));
    assert!(
        contents
            .lines()
            .any(|line| line.contains("companion") && line.contains(" -add "))
    );
    crate::validate_sdc_syntax(&contents).unwrap();

    let result = runtime
        .eval("delete_generated_clock [get_clocks divided]; report_clock")
        .unwrap();
    match result {
        EvalResult::Complete(report) => {
            assert!(report.contains("| master"));
            assert!(!report.contains("| divided"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn typed_sdc_commands_execute_through_the_shell() {
    let source = temp_script_path("opto-typed-sdc-commands.sv");
    std::fs::write(
        &source,
        "module top(input logic clk, input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    runtime
        .eval(&format!(
            "read_hdl {}; elaborate top; \
             create_clock -name c1 -period 10 [get_ports clk]; \
             create_clock -name c2 -period 12",
            source.display()
        ))
        .unwrap();

    for command in [
        "set_clock_transition -rise -max 0.2 [get_clocks c1]",
        "unset_clock_transition [get_clocks c1]",
        "set_clock_latency -source -early 0.3 [get_clocks c1]",
        "unset_clock_latency -source [get_clocks c1]",
        "set_clock_uncertainty -setup 0.1 [get_clocks c1]",
        "unset_clock_uncertainty -setup [get_clocks c1]",
        "set_clock_uncertainty -rise_from [get_clocks c1] -fall_to [get_clocks c2] 0.2",
        "unset_clock_uncertainty -rise_from [get_clocks c1] -fall_to [get_clocks c2]",
        "set_clock_groups -asynchronous -name async -group [get_clocks c1] -group [get_clocks c2]",
        "unset_clock_groups -asynchronous -name async",
        "set_case_analysis rise [get_ports a]",
        "unset_case_analysis [get_ports a]",
        "set_logic_zero [get_ports a]",
        "set_logic_one [get_ports a]",
        "set_logic_dc [get_ports a]",
        "set_disable_timing [get_ports a]",
        "unset_disable_timing [get_ports a]",
        "set_timing_derate -early -rise -data -cell_delay 1.05",
        "unset_timing_derate",
        "set_propagated_clock [get_clocks c1] [get_clocks c2]",
        "unset_propagated_clock [get_clocks c1] [get_clocks c2]",
        "set_resistance -max 0.4 [get_nets a]",
        "set_input_delay -clock [get_clocks c1] -max 1.0 [get_ports a]",
        "unset_input_delay -clock [get_clocks c1] -max [get_ports a]",
        "set_output_delay -clock [get_clocks c1] -max 2.0 [get_ports y]",
        "unset_output_delay -clock [get_clocks c1] -max [get_ports y]",
        "set_false_path -from [get_ports a] -to [get_ports y]",
        "unset_path_exceptions -from [get_ports a] -to [get_ports y]",
        "report_clock",
        "delete_clock [get_clocks c2]",
    ] {
        runtime
            .eval(command)
            .unwrap_or_else(|error| panic!("{command}: {error}"));
    }
    assert!(
        runtime
            .eval("check_timing")
            .unwrap_err()
            .to_string()
            .contains("no Liberty timing arcs found")
    );

    std::fs::remove_file(source).unwrap();
}

#[test]
fn all_registers_returns_sequential_target_instances_by_kind() {
    let lib = temp_script_path("opto-all-registers.lib");
    let source = temp_script_path("opto-all-registers.sv");
    std::fs::write(
        &lib,
        r#"
library (demo) {
  cell (DFF) {
    ff (IQ, IQN) { clocked_on : "CP"; next_state : "D"; }
    pin (CP) { direction : input; clock : true; }
    pin (D) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
  cell (DLAT) {
    latch (IQ, IQN) { data_in : "D"; enable : "G"; }
    pin (G) { direction : input; clock : true; }
    pin (D) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        &source,
        "module top(input logic clk, gate, d, output logic q1, q2); \
         DFF u_ff(.CP(clk), .D(d), .Q(q1)); \
         DLAT u_latch(.G(gate), .D(d), .Q(q2)); \
         endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_libs {}; \
             read_hdl {}; elaborate top; \
             list [get_db [all_registers] .name] \
                  [get_db [all_registers -edge_triggered] .name] \
                  [get_db [all_registers -level_sensitive] .name]",
            lib.display(),
            source.display()
        ))
        .unwrap();
    std::fs::remove_file(lib).unwrap();
    std::fs::remove_file(source).unwrap();

    match result {
        EvalResult::Complete(names) => assert_eq!(names, "{u_ff u_latch} u_ff u_latch"),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn read_sdc_evaluates_load_and_transition_constraints() {
    let lib = temp_script_path("opto-timing-constraints.lib");
    let source = temp_script_path("opto-timing-constraints.sv");
    let sdc = temp_script_path("opto-timing-constraints.sdc");
    std::fs::write(
        &lib,
        r#"
library (demo) {
  cell (AND2) {
area : 1.0;
pin (A) { direction : input; }
pin (B) { direction : input; capacitance : 10.0; }
pin (Y) {
  direction : output;
  function : "A B";
  timing () {
    related_pin : "A";
    cell_rise (delay_template) { values ( "0.10" ); }
  }
  timing () {
    related_pin : "B";
    cell_rise (delay_template) {
      index_1 ("0.0, 1.0");
      index_2 ("0.0, 10.0");
      values ( \
        "0.10, 0.20", \
        "0.30, 0.40" \
      );
    }
  }
  }
}

}

"#,
    )
    .unwrap();
    std::fs::write(
        &source,
        "module top(input logic a, input logic b, output logic y); assign y = a & b; endmodule\n",
    )
    .unwrap();
    std::fs::write(
        &sdc,
        "set_input_transition 1.0 [get_ports b]\nset_load 10.0 [get_ports y]\nset_drive 0.1 [get_ports b]\nset_resistance 0.1 [get_nets b]\nset_max_delay 0.3 -from [get_ports b] -to [get_ports y]\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_libs {}; read_hdl {}; elaborate top; synth; read_sdc {}; report_timing -from [get_ports b] -to [get_ports y]",
            lib.display(),
            source.display(),
            sdc.display()
        ))
        .unwrap();

    match result {
        EvalResult::Complete(report) => {
            assert!(report.contains("Startpoint: b (input port)"));
            assert!(report.contains("Endpoint: y (output port)"));
            assert!(report.contains("2.400"));
            assert!(report.contains("Status: VIOLATED"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }

    let written = temp_script_path("opto-timing-roundtrip.sdc");
    runtime
        .eval(&format!("write_sdc {}", written.display()))
        .unwrap();
    let contents = std::fs::read_to_string(&written).unwrap();
    crate::validate_sdc_syntax(&contents).unwrap();

    let mut roundtrip = Runtime::new(Session::new()).unwrap();
    roundtrip.register_commands().unwrap();
    let result = roundtrip
        .eval(&format!(
            "read_libs {}; \
             read_hdl {}; elaborate top; synth; read_sdc {}; \
             report_timing -from [get_ports b] -to [get_ports y]",
            lib.display(),
            source.display(),
            written.display()
        ))
        .unwrap();
    match result {
        EvalResult::Complete(report) => assert!(report.contains("2.400")),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }

    std::fs::remove_file(lib).unwrap();
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(sdc).unwrap();
    std::fs::remove_file(written).unwrap();
}

#[test]
fn sdc_io_delays_drive_arrival_and_required_times() {
    let lib = temp_script_path("opto-io-delay.lib");
    let source = temp_script_path("opto-io-delay.sv");
    std::fs::write(
        &lib,
        r#"
library (demo) {
  cell (BUF) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.4" ); }
        cell_fall (delay_template) { values ( "0.4" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        &source,
        "module top(input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let result = runtime
        .eval(&format!(
            "read_libs {}; \
             read_hdl {}; elaborate top; synth; \
             create_clock -name vclk -period 10; \
             set_input_delay -clock [get_clocks vclk] -max 1.0 [get_ports a]; \
             set_output_delay -clock [get_clocks vclk] -max 2.0 [get_ports y]; \
             report_timing -from [get_ports a] -to [get_ports y]",
            lib.display(),
            source.display()
        ))
        .unwrap();
    std::fs::remove_file(lib).unwrap();
    std::fs::remove_file(source).unwrap();

    match result {
        EvalResult::Complete(report) => {
            assert!(report.contains("Startpoint: a (input port)"));
            assert!(report.contains("Endpoint: y (output port)"));
            assert!(report.contains("1.000"));
            assert!(report.contains("8.000"));
            assert!(report.contains("7.000"));
            assert!(report.contains("Type: Output delay"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
}

#[test]
fn dc_path_exception_commands_preserve_typed_ordered_points() {
    let source = temp_script_path("opto-path-exceptions.sv");
    std::fs::write(
        &source,
        r"
module leaf(input logic A, output logic Y);
  assign Y = A;
endmodule
module top(input logic a, output logic y);
  logic n;
  leaf U1(.A(a), .Y(n));
  assign y = n;
endmodule
",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();
    runtime
        .eval(&format!(
            "read_hdl {}; elaborate top; \
             set_false_path -setup -from [get_ports a] -through [get_pins U1/A] -through [get_cells U1] -through [get_pins U1/Y] -to [get_ports y] -comment ordered; \
             set_max_delay 0.4 -rise_from [get_ports a] -through [get_nets n] -fall_to [get_ports y] -ignore_clock_latency; \
             set_min_delay 0.2 -fall -from [get_ports a] -to [get_ports y]; \
             set_multicycle_path 3 -hold -start -from [get_ports a] -to [get_ports y]",
            source.display()
        ))
        .unwrap();
    std::fs::remove_file(source).unwrap();

    let session = runtime.state.session.borrow();
    let rows = session.path_exceptions().iter().collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    assert!(matches!(rows[0].kind, PathExceptionKind::FalsePath));
    assert_eq!(rows[0].through.len(), 3);
    assert_eq!(rows[0].comment, "ordered");
    assert!(matches!(
        rows[1].kind,
        PathExceptionKind::MaxDelay { delay } if (delay - 0.4).abs() <= f64::EPSILON
    ));
    assert!(rows[1].ignore_clock_latency);
    assert_eq!(rows[1].edges.from, EdgeSelection::Rise);
    assert_eq!(rows[1].edges.to, EdgeSelection::Fall);
    assert_eq!(rows[1].edges.end, EdgeSelection::Both);
    assert!(matches!(
        rows[2].kind,
        PathExceptionKind::MinDelay { delay } if (delay - 0.2).abs() <= f64::EPSILON
    ));
    assert_eq!(rows[2].edges.to, EdgeSelection::Both);
    assert_eq!(rows[2].edges.end, EdgeSelection::Fall);
    assert!(matches!(
        rows[3].kind,
        PathExceptionKind::MultiCycle {
            cycles: 3,
            use_end_clock: false
        }
    ));
    assert_eq!(rows[3].corner, ExceptionCorner::Hold);
}

#[test]
fn set_dont_use_excludes_library_cells_from_synthesis() {
    let lib = temp_script_path("opto-dont-use.lib");
    let source = temp_script_path("opto-dont-use.sv");
    std::fs::write(
        &lib,
        r#"
library (demo) {
  cell (AND2S) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
  cell (AND2B) {
    area : 5.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        &source,
        "module top(input logic a, input logic b, output logic y); assign y = a & b; endmodule\n",
    )
    .unwrap();
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let setup = format!(
        "read_libs {}; read_hdl {}; elaborate top",
        lib.display(),
        source.display()
    );
    runtime.eval(&setup).unwrap();
    let baseline = runtime.eval("synth; report_area").unwrap();
    let marked = runtime
        .eval("set_db [get_db lib_cells AND2S] .dont_use true")
        .unwrap();
    let excluded = runtime.eval("synth; report_area").unwrap();
    let missing = runtime
        .eval("set_db [get_db lib_cells NO_SUCH_CELL] .dont_use true")
        .unwrap();
    std::fs::remove_file(lib).unwrap();
    std::fs::remove_file(source).unwrap();

    match baseline {
        EvalResult::Complete(report) => assert!(report.contains("Total cell area:")),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
    match marked {
        EvalResult::Complete(result) => assert_eq!(result, "1"),
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
    match excluded {
        EvalResult::Complete(report) => {
            assert!(report.contains("5.000000"));
            assert!(!report.contains("1.000000"));
        }
        EvalResult::Exit(code) => panic!("unexpected exit {code}"),
    }
    assert!(matches!(missing, EvalResult::Complete(value) if value == "0"));
}
