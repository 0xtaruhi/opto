// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn report_power_matches_the_dc_scalar_golden_and_survives_checkpoint() {
    let library = temp_script_path("opto-power.lib");
    let verilog = temp_script_path("opto-power.v");
    let checkpoint = temp_script_path("opto-power.ock");
    std::fs::write(&library, power_library()).unwrap();
    std::fs::write(
        &verilog,
        "module top(input a, output z); BUF U1(.A(a), .Y(z)); endmodule\n",
    )
    .unwrap();

    let configure = format!(
        "read_libs {}; read_hdl {}; elaborate top; synth",
        library.display(),
        verilog.display(),
    );
    let mut writer = Runtime::new(Session::new()).unwrap();
    writer.register_commands().unwrap();
    writer.eval(&configure).unwrap();
    writer
        .eval("set_switching_activity -static_probability 0.25 -toggle_rate 0.2 -period 10 [get_ports a]")
        .unwrap();

    let summary = complete(writer.eval("report_power").unwrap(), "report_power summary");
    assert!(summary.starts_with("# Power report"));
    assert!(summary.contains("Cell internal power: 100.0000 uW (100%)"));
    assert!(summary.contains("Cell leakage power: 1.5000 nW"));
    assert!(summary.contains(&format!("| {}", library.display())));
    assert!(summary.contains("Operating conditions: nom_pvt"));
    assert!(summary.contains("Wire load model mode: top"));
    assert!(summary.contains("| combinational"));
    let modern_summary =
        crate::presentation::render_report(&summary, Theme::Dark.palette(), false, Some(72));
    assert!(modern_summary.contains("Power report"), "{modern_summary}");
    assert!(modern_summary.contains("(100%)"), "{modern_summary}");
    let cells = complete(
        writer.eval("report_power -cell -flat").unwrap(),
        "report_power -cell",
    );
    assert!(cells.contains("View: Cell"));
    assert!(cells.contains("| Cell"));
    assert!(cells.contains("| Dynamic"));
    assert!(cells.contains("U1"));
    assert!(cells.contains("0.1000"));
    let modern_cells =
        crate::presentation::render_report(&cells, Theme::Dark.palette(), false, Some(64));
    assert!(modern_cells.contains("U1"), "{modern_cells}");
    assert!(modern_cells.contains('─'), "{modern_cells}");
    let nets = complete(
        writer
            .eval("report_power -net -flat -include_input_nets")
            .unwrap(),
        "report_power -net",
    );
    assert!(nets.contains("View: Net"));
    assert!(nets.contains("| Net"));
    assert!(nets.contains('a'));
    assert!(nets.contains("0.0010"));
    assert!(nets.contains("0.0200"));
    assert!(nets.contains("0.250"));

    writer
        .eval("set_switching_activity -static_probability 0.75 [get_ports a]")
        .unwrap();
    let partial_update = complete(
        writer
            .eval("report_power -net -flat -include_input_nets")
            .unwrap(),
        "report_power after partial activity update",
    );
    assert!(partial_update.contains("0.750"));
    assert!(partial_update.contains("0.0200"));
    writer
        .eval("reset_switching_activity [get_ports a]")
        .unwrap();
    let reset = complete(
        writer
            .eval("report_power -net -flat -include_input_nets")
            .unwrap(),
        "report_power after activity reset",
    );
    assert!(reset.contains("| a"));
    assert!(reset.contains("| 0.100"));
    assert!(reset.contains("| 0.500"));
    assert!(reset.contains("| 0.0000"));
    assert!(reset.contains("| d"));
    writer
        .eval("set_switching_activity -static_probability 0.25 -toggle_rate 0.2 -period 10 [get_ports a]")
        .unwrap();

    writer
        .eval(&format!("save {}", checkpoint.display()))
        .unwrap();
    let mut reader = Runtime::new(Session::new()).unwrap();
    reader.register_commands().unwrap();
    let restored = complete(
        reader
            .eval(&format!(
                "read_libs {}; resume {}; report_power",
                library.display(),
                checkpoint.display()
            ))
            .unwrap(),
        "restored report_power",
    );
    assert!(restored.contains("Cell internal power: 100.0000 uW (100%)"));
    assert!(restored.contains("Cell leakage power: 1.5000 nW"));

    std::fs::remove_file(library).unwrap();
    std::fs::remove_file(verilog).unwrap();
    std::fs::remove_file(checkpoint).unwrap();
}

#[test]
fn power_commands_reject_unimplemented_dc_options_explicitly() {
    let mut runtime = Runtime::new(Session::new()).unwrap();
    runtime.register_commands().unwrap();

    let effort = runtime
        .eval("report_power -analysis_effort high")
        .unwrap_err();
    assert!(effort.to_string().contains("not implemented yet"));
    let kind = runtime.eval("report_power -cell -net").unwrap_err();
    assert!(
        kind.to_string()
            .contains("-cell and -net are mutually exclusive")
    );
    let saif_scope = runtime
        .eval("set_switching_activity -toggle_rate 1 -path_sources source")
        .unwrap_err();
    assert!(saif_scope.to_string().contains("not implemented yet"));
}

fn complete(result: EvalResult, operation: &str) -> String {
    match result {
        EvalResult::Complete(text) => text,
        EvalResult::Exit(code) => panic!("{operation}: unexpected exit {code}"),
    }
}

fn power_library() -> &'static str {
    r#"
library (power_demo) {
  delay_model : table_lookup;
  time_unit : "1ns";
  capacitive_load_unit (1, pf);
  voltage_unit : "1V";
  current_unit : "1mA";
  leakage_power_unit : "1nW";
  nom_voltage : 1.0;
  power_lut_template (power_template) {
    variable_1 : input_transition_time;
    variable_2 : total_output_net_capacitance;
    index_1 ("0.1");
    index_2 ("0.2");
  }
  cell (BUF) {
    area : 1.0;
    cell_leakage_power : 2.0;
    leakage_power () { when : "A"; value : 3.0; }
    leakage_power () { when : "!A"; value : 1.0; }
    pin (A) { direction : input; capacitance : 0.1; }
    pin (Y) {
      direction : output;
      function : "A";
      internal_power () {
        related_pin : "A";
        rise_power (power_template) { values ("4.0"); }
        fall_power (power_template) { values ("6.0"); }
      }
      timing () {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise (scalar) { values ("0.1"); }
        cell_fall (scalar) { values ("0.1"); }
        rise_transition (scalar) { values ("0.1"); }
        fall_transition (scalar) { values ("0.1"); }
      }
    }
  }
}
"#
}
