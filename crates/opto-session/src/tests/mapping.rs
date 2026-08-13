// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn synthesize_selects_area_efficient_prefix_adder_to_meet_max_delay() {
    let dir = temp_dir("timing-driven-adder");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (AND2) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A B";
      timing () { related_pin : "A"; cell_rise (t) { values ( "1.0" ); } }
      timing () { related_pin : "B"; cell_rise (t) { values ( "1.0" ); } }
    }
  }
  cell (OR2) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A + B";
      timing () { related_pin : "A"; cell_rise (t) { values ( "1.0" ); } }
      timing () { related_pin : "B"; cell_rise (t) { values ( "1.0" ); } }
    }
  }
  cell (XOR2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A ^ B";
      timing () { related_pin : "A"; cell_rise (t) { values ( "1.0" ); } }
      timing () { related_pin : "B"; cell_rise (t) { values ( "1.0" ); } }
    }
  }
}
"#,
    )
    .unwrap();
    let sv_path = dir.join("top.sv");
    std::fs::write(
        &sv_path,
        "module top(input logic [7:0] a, b, output logic [7:0] y); assign y = a + b; endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&sv_path), &FrontendOptions::default())
        .unwrap();
    let from = session
        .resolve_timing_endpoints("set_max_delay", &["a".to_string(), "b".to_string()])
        .unwrap();
    let to = session
        .resolve_timing_endpoints("set_max_delay", &["y".to_string()])
        .unwrap();
    session.set_max_delay(12.0, from, to).unwrap();

    session.synthesize().unwrap();

    let synthesis = session
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap();
    assert!(
        !synthesis.implementation_db().regions()[0]
            .recipe()
            .is_empty()
    );
    let implementation = &synthesis.implementation_db().regions()[0];
    let operator = implementation.operator();
    assert!(!implementation.mapped_cells().is_empty());
    assert!(implementation.mapped_cells().iter().all(|&cell| {
        synthesis.mapped().is_live_cell(cell)
            && synthesis.implementation_db().operators_for_cell(cell)
                == Some(std::slice::from_ref(&operator))
    }));
    assert!(synthesis.timing().unwrap().slack.unwrap() >= 0.0);
    let qor = session.report_qor().unwrap();
    assert!(qor.contains("Timing paths: 1"), "{qor}");
    assert!(qor.contains("Critical Path Length:"));
    assert!(qor.contains("Critical Path Slack:"));
    assert!(qor.contains("Total Negative Slack: 0.000000"));
    assert!(qor.contains("No. of Violating Paths: 0"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn synthesize_maps_always_ff_to_target_dff() {
    let dir = temp_dir("synthesis-target-dff");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (DFD1) {
    area : 2.142;
    ff (IQ, IQN) {
      clocked_on : "CP";
      next_state : "D";
    }
    pin (CP) { direction : input; clock : true; }
    pin (D) { direction : input; }
    pin (Q) { direction : output; function : "IQ"; }
  }
}
"#,
    )
    .unwrap();
    let sv_path = dir.join("top.sv");
    std::fs::write(
            &sv_path,
            "module top(input logic clk, input logic d, output logic q); always_ff @(posedge clk) q <= d; endmodule\n",
        )
        .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&sv_path), &FrontendOptions::default())
        .unwrap();

    let message = session.synthesize().unwrap();

    assert_eq!(message, "1");
    let design = session.current().unwrap();
    assert_eq!(design.cell_count(), 1);
    let cell = design.cell(0).unwrap();
    assert_eq!(cell.name, "q_reg");
    assert_eq!(cell.reference, "DFD1");

    let out_path = dir.join("mapped.v");
    session.write_hdl_file(&out_path, false).unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    let area = session.report_area().unwrap();
    let qor = session.report_qor().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(mapped.contains("DFD1 q_reg(.D(d), .CP(clk), .Q(q));"));
    assert!(area.contains("Number of sequential cells: 1"));
    assert!(area.contains("Noncombinational area: 2.142000"));
    assert!(qor.contains("Combinational cells: 0"));
    assert!(qor.contains("Sequential cells: 1"));
}

#[test]
fn synthesize_prefers_lower_input_capacitance_for_equal_area_cells() {
    let dir = temp_dir("synthesis-low-input-cap-target-library");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (A_INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 10.0; }
    pin (Y) { direction : output; function : "!A"; }
  }
  cell (Z_INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 1.0; }
    pin (Y) { direction : output; function : "!A"; }
  }
}
"#,
    )
    .unwrap();
    let sv_path = dir.join("top.sv");
    std::fs::write(
        &sv_path,
        "module top(input logic a, output logic y); assign y = ~a; endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&sv_path), &FrontendOptions::default())
        .unwrap();

    let message = session.synthesize().unwrap();

    assert_eq!(message, "1");
    let design = session.current().unwrap();
    assert_eq!(design.cell_count(), 1);
    assert_eq!(design.cell(0).unwrap().reference, "Z_INV");

    let out_path = dir.join("mapped.v");
    session.write_hdl_file(&out_path, false).unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(mapped.contains("Z_INV U1(.A(a), .Y(y));"));
}

#[test]
fn synthesize_prefers_lower_default_delay_for_equal_area_cells() {
    let dir = temp_dir("synthesis-low-delay-target-library");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (A_INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 1.0; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "10.0" ); }
        cell_fall (delay_template) { values ( "10.0" ); }
      }
    }
  }
  cell (Z_INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 10.0; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.1" ); }
        cell_fall (delay_template) { values ( "0.1" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    let sv_path = dir.join("top.sv");
    std::fs::write(
        &sv_path,
        "module top(input logic a, output logic y); assign y = ~a; endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&sv_path), &FrontendOptions::default())
        .unwrap();

    let message = session.synthesize().unwrap();

    assert_eq!(message, "1");
    let design = session.current().unwrap();
    assert_eq!(design.cell_count(), 1);
    assert_eq!(design.cell(0).unwrap().reference, "Z_INV");

    let out_path = dir.join("mapped.v");
    session.write_hdl_file(&out_path, false).unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(mapped.contains("Z_INV U1(.A(a), .Y(y));"));
}

#[test]
fn synthesize_prefers_lower_default_transition_for_equal_delay_cells() {
    let dir = temp_dir("synthesis-low-transition-target-library");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (A_INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 1.0; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.1" ); }
        cell_fall (delay_template) { values ( "0.1" ); }
        rise_transition (delay_template) { values ( "10.0" ); }
        fall_transition (delay_template) { values ( "10.0" ); }
      }
    }
  }
  cell (Z_INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 10.0; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.1" ); }
        cell_fall (delay_template) { values ( "0.1" ); }
        rise_transition (delay_template) { values ( "1.0" ); }
        fall_transition (delay_template) { values ( "1.0" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    let sv_path = dir.join("top.sv");
    std::fs::write(
        &sv_path,
        "module top(input logic a, output logic y); assign y = ~a; endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&sv_path), &FrontendOptions::default())
        .unwrap();

    let message = session.synthesize().unwrap();

    assert_eq!(message, "1");
    let design = session.current().unwrap();
    assert_eq!(design.cell_count(), 1);
    assert_eq!(design.cell(0).unwrap().reference, "Z_INV");

    let out_path = dir.join("mapped.v");
    session.write_hdl_file(&out_path, false).unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(mapped.contains("Z_INV U1(.A(a), .Y(y));"));
}

#[test]
fn synthesize_selects_lower_area_decomposition_over_direct_cell() {
    let dir = temp_dir("synthesis-costed-covering-target-library");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (AND2) {
    area : 5.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) { direction : output; function : "!A"; }
  }
  cell (NAND2) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "!(A B)"; }
  }
}
"#,
    )
    .unwrap();
    let sv_path = dir.join("top.sv");
    std::fs::write(
        &sv_path,
        r"
module top(
  input logic a,
  input logic b,
  output logic y
);
  assign y = a & b;
endmodule
",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&sv_path), &FrontendOptions::default())
        .unwrap();

    let message = session.synthesize().unwrap();

    assert_eq!(message, "1");
    let out_path = dir.join("costed-covering.v");
    session.write_hdl_file(&out_path, false).unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    let area = session.report_area().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(
        mapped.contains("NAND2 U1(.A(a), .B(b), .Y(n1));"),
        "{mapped}"
    );
    assert!(mapped.contains("INV U2(.A(n1), .Y(y));"));
    assert!(!mapped.contains("\n  AND2 "));
    assert!(area.contains("Number of combinational cells: 2"));
    assert!(area.contains("Number of buf/inv: 1"));
    assert!(area.contains("Total cell area: 2.000000"));
}
