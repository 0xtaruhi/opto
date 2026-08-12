// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Session-to-timing wiring, publication, cache, and report-boundary tests.
//!
//! Scalar STA and incremental edit algorithms remain owned by `opto-timing`.

use super::*;

#[test]
fn report_timing_analyzes_mapped_register_setup_and_hold() {
    let dir = temp_dir("report-timing-register-path");
    std::fs::write(
        dir.join("demo.lib"),
        r#"
library (demo) {
  default_operating_conditions : typical;
  cell (DFF) {
    area : 2.0;
    ff (IQ, IQN) { clocked_on : "CP"; next_state : "D"; }
    pin (CP) { direction : input; clock : true; capacitance : 0.02; }
    pin (D) {
      direction : input;
      capacitance : 0.01;
      timing () {
        related_pin : "CP";
        timing_type : setup_rising;
        rise_constraint (constraint_template) { values ( "0.02" ); }
        fall_constraint (constraint_template) { values ( "0.02" ); }
      }
      timing () {
        related_pin : "CP";
        timing_type : hold_rising;
        rise_constraint (constraint_template) { values ( "0.01" ); }
        fall_constraint (constraint_template) { values ( "0.01" ); }
      }
    }
    pin (Q) {
      direction : output;
      function : "IQ";
      timing () {
        related_pin : "CP";
        timing_type : rising_edge;
        timing_sense : non_unate;
        cell_rise (delay_template) { values ( "0.06" ); }
        cell_fall (delay_template) { values ( "0.06" ); }
        rise_transition (delay_template) { values ( "0.04" ); }
        fall_transition (delay_template) { values ( "0.04" ); }
      }
    }
  }
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 0.01; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_rise (delay_template) { values ( "0.01" ); }
        cell_fall (delay_template) { values ( "0.01" ); }
        rise_transition (delay_template) { values ( "0.03" ); }
        fall_transition (delay_template) { values ( "0.03" ); }
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
        r"
module top(input logic clk, input logic d, output logic q);
  logic launch;
  logic capture;
  always_ff @(posedge clk) launch <= d;
  always_ff @(posedge clk) capture <= ~launch;
  assign q = capture;
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
    let clock_port = session
        .resolve_port_ids("create_clock", &["clk".to_string()])
        .unwrap();
    session
        .create_clock("clk", 1.0, clock_port.clone(), None)
        .unwrap();
    session.set_input_transition(0.05, &clock_port).unwrap();
    session.synthesize().unwrap();

    let setup = session
        .report_timing(&ReportTimingOptions::default())
        .unwrap();
    let hold = session
        .report_timing(&ReportTimingOptions {
            delay_type: DelayType::Min,
            ..ReportTimingOptions::default()
        })
        .unwrap();
    let checks = session.check_timing().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(setup.contains("Startpoint: launch_reg"));
    assert!(setup.contains("Endpoint: capture_reg"));
    assert!(setup.contains("Path group: clk"));
    assert!(setup.contains("Library setup time"));
    assert!(setup.contains("Data required time"));
    assert!(hold.contains("Path type: min"));
    assert!(hold.contains("Library hold time"));
    assert!(checks.contains("launch_reg/D"));
    assert!(checks.contains('q'));
    assert!(!checks.contains("capture_reg/D"));
}

#[test]
fn report_timing_analyzes_mapped_latch_transparency_and_borrowing() {
    let dir = temp_dir("report-timing-latch-path");
    std::fs::write(
        dir.join("demo.lib"),
        r#"
library (demo) {
  default_operating_conditions : typical;
  cell (DFF) {
    area : 2.0;
    ff (IQ, IQN) { clocked_on : "CP"; next_state : "D"; }
    pin (CP) { direction : input; clock : true; }
    pin (D) { direction : input; }
    pin (Q) {
      direction : output;
      function : "IQ";
      timing () {
        related_pin : "CP";
        timing_type : rising_edge;
        timing_sense : non_unate;
        cell_rise (delay_template) { values ( "0.06" ); }
        cell_fall (delay_template) { values ( "0.06" ); }
      }
    }
  }
  cell (LATCH_H) {
    area : 1.0;
    latch (IQ, IQN) { data_in : "D"; enable : "G"; }
    pin (D) {
      direction : input;
      timing () {
        related_pin : "G";
        timing_type : setup_falling;
        rise_constraint (constraint_template) { values ( "0.02" ); }
        fall_constraint (constraint_template) { values ( "0.02" ); }
      }
      timing () {
        related_pin : "G";
        timing_type : hold_falling;
        rise_constraint (constraint_template) { values ( "0.01" ); }
        fall_constraint (constraint_template) { values ( "0.01" ); }
      }
    }
    pin (G) { direction : input; clock : true; }
    pin (Q) {
      direction : output;
      function : "IQ";
      timing () {
        related_pin : "D";
        timing_sense : positive_unate;
        cell_rise (delay_template) { values ( "0.04" ); }
        cell_fall (delay_template) { values ( "0.04" ); }
      }
      timing () {
        related_pin : "G";
        timing_type : rising_edge;
        timing_sense : non_unate;
        cell_rise (delay_template) { values ( "0.04" ); }
        cell_fall (delay_template) { values ( "0.04" ); }
      }
    }
  }
  cell (LATCH_L) {
    area : 1.0;
    latch (IQ, IQN) { data_in : "D"; enable : "!G"; }
    pin (D) {
      direction : input;
      timing () {
        related_pin : "G";
        timing_type : setup_rising;
        rise_constraint (constraint_template) { values ( "0.02" ); }
        fall_constraint (constraint_template) { values ( "0.02" ); }
      }
      timing () {
        related_pin : "G";
        timing_type : hold_rising;
        rise_constraint (constraint_template) { values ( "0.01" ); }
        fall_constraint (constraint_template) { values ( "0.01" ); }
      }
    }
    pin (G) { direction : input; clock : true; }
    pin (Q) {
      direction : output;
      function : "IQ";
      timing () {
        related_pin : "D";
        timing_sense : positive_unate;
        cell_rise (delay_template) { values ( "0.04" ); }
        cell_fall (delay_template) { values ( "0.04" ); }
      }
      timing () {
        related_pin : "G";
        timing_type : falling_edge;
        timing_sense : non_unate;
        cell_rise (delay_template) { values ( "0.04" ); }
        cell_fall (delay_template) { values ( "0.04" ); }
      }
    }
  }
  cell (INV) {
    area : 0.5;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_rise (delay_template) { values ( "0.30" ); }
        cell_fall (delay_template) { values ( "0.30" ); }
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
        r"
module top(input logic clk, input logic d, output logic q);
  logic launch;
  logic high;
  logic low;
  always_ff @(posedge clk) launch <= d;
  always_latch if (clk) high <= ~launch;
  always_latch if (!clk) low <= ~high;
  assign q = low;
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
    let clock_port = session
        .resolve_port_ids("create_clock", &["clk".to_string()])
        .unwrap();
    session.create_clock("clk", 1.0, clock_port, None).unwrap();
    session.synthesize().unwrap();

    let setup = session
        .report_timing(&ReportTimingOptions {
            to: vec!["low_reg/D".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    let checkpoint = dir.join("top.ock");
    session.write_checkpoint_file(&checkpoint).unwrap();
    let mut restored = Session::new();
    restored.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    restored.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    restored.read_checkpoint_file(&checkpoint).unwrap();
    let restored_setup = restored
        .report_timing(&ReportTimingOptions {
            to: vec!["low_reg/D".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert_eq!(restored_setup, setup);
    assert!(setup.contains("Startpoint: launch_reg"), "{setup}");
    assert!(setup.contains("Endpoint: low_reg"), "{setup}");
    assert!(setup.contains("high_reg/Q (LATCH_H)"), "{setup}");
    assert!(
        setup.contains("falling level-sensitive latch enabled by clk"),
        "{setup}"
    );
    assert!(setup.contains("Time borrowed: 0.200"), "{setup}");
    assert!(setup.contains("0.200"), "{setup}");
}

#[test]
fn report_timing_uses_liberty_arcs_on_synthesized_design() {
    let dir = temp_dir("report-timing-liberty-arcs");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  default_operating_conditions : typical;
  default_wire_load : "ZeroWireload";
  default_wire_load_mode : segmented;
  cell (AND2) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A B";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.10" ); }
        cell_fall (delay_template) { values ( "0.12" ); }
      }
      timing () {
        related_pin : "B";
        cell_rise (delay_template) { values ( "0.25" ); }
        cell_fall (delay_template) { values ( "0.20" ); }
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
    session.synthesize().unwrap();

    let first_model = session.current_timing_model().unwrap();
    let second_model = session.current_timing_model().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first_model, &second_model));

    let report = session
        .report_timing(&ReportTimingOptions::default())
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(report.starts_with("# Timing report"));
    assert!(report.contains("Startpoint: b (input port)"));
    assert!(report.contains("Endpoint: y (output port)"));
    assert!(report.contains("Operating conditions: typical"));
    assert!(report.contains("Library: demo"));
    assert!(report.contains("Wire load mode: segmented"));
    assert!(report.contains("Wire load model: ZeroWireload"));
    assert!(report.contains("input external delay"));
    assert!(report.contains("b (in)"));
    assert!(report.contains("U1/Y (AND2)"));
    assert!(
        report.lines().any(|line| line.starts_with("| y ")),
        "{report}"
    );
    assert!(report.contains("0.250"));
}

#[test]
fn timing_model_cache_tracks_semantic_generation_and_library_replacement() {
    let dir = temp_dir("timing-model-generation");
    std::fs::write(
        dir.join("demo.lib"),
        r#"
library (demo) {
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_rise (t) { values ( "0.2" ); }
        cell_fall (t) { values ( "0.2" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    let rtl = dir.join("top.sv");
    std::fs::write(
        &rtl,
        "module top(input logic a, output logic y); assign y = ~a; endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&rtl), &FrontendOptions::default())
        .unwrap();
    session.synthesize().unwrap();

    let first = session.current_timing_model().unwrap();
    let hit = session.current_timing_model().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &hit));
    drop(hit);

    session
        .apply_db_update(
            DbUpdate {
                modules: vec![hierarchy_leaf("unrelated", 1, false)],
                top: None,
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    let unaffected = session.current_timing_model().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &unaffected));
    drop(unaffected);

    session
        .mark_library_cells_unavailable(&["INV".to_string()])
        .unwrap();
    assert!(session.process.timing_model_cache.borrow().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn report_timing_uses_sdc_transition_and_load_constraints() {
    let dir = temp_dir("report-timing-sdc-loads");
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
    session.synthesize().unwrap();
    let b = session
        .resolve_port_ids("set_input_transition", &["b".to_string()])
        .unwrap();
    let y = session
        .resolve_port_ids("set_load", &["y".to_string()])
        .unwrap();
    session.set_input_transition(1.0, &b).unwrap();
    session.set_load(10.0, &y).unwrap();

    let report = session
        .report_timing(&ReportTimingOptions::default())
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(report.contains("Startpoint: b (input port)"));
    assert!(report.contains("U1/Y (AND2)"));
    assert!(report.contains("0.400"));
}

#[test]
fn report_timing_uses_liberty_pin_capacitance_for_fanout_load() {
    let dir = temp_dir("report-timing-fanout-cap");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (SRC) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 1.0; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) {
          index_1 ("0.0");
          index_2 ("0.0, 20.0");
          values ( "0.10, 0.50" );
        }
      }
    }
  }
  cell (SINK) {
    area : 1.0;
    pin (A) { direction : input; capacitance : 10.0; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.10" ); }
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
        r"
module top(
  input logic a,
  output logic y1,
  output logic y2
);
  logic n;
  SRC U1(.A(a), .Y(n));
  SINK U2(.A(n), .Y(y1));
  SINK U3(.A(n), .Y(y2));
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

    let report = session
        .report_timing(&ReportTimingOptions {
            from: vec!["a".to_string()],
            to: vec!["y1".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(report.contains("Startpoint: a (input port)"));
    assert!(report.contains("Endpoint: y1 (output port)"));
    assert!(report.contains("U1/Y (SRC)"));
    assert!(report.contains("0.500"));
}

#[test]
fn report_timing_uses_liberty_transition_tables() {
    let dir = temp_dir("report-timing-transition-tables");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (SRC) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) { values ( "0.10" ); }
        rise_transition (delay_template) { values ( "1.00" ); }
      }
    }
  }
  cell (SINK) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        cell_rise (delay_template) {
          index_1 ("0.0, 1.0");
          index_2 ("0.0");
          values ( "0.10", "0.40" );
        }
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
        r"
module top(
  input logic a,
  output logic y
);
  logic n;
  SRC U1(.A(a), .Y(n));
  SINK U2(.A(n), .Y(y));
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

    let report = session
        .report_timing(&ReportTimingOptions {
            from: vec!["a".to_string()],
            to: vec!["y".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(report.contains("Startpoint: a (input port)"));
    assert!(report.contains("Endpoint: y (output port)"));
    assert!(report.contains("U1/Y (SRC)"));
    assert!(report.contains("U2/Y (SINK)"));
    assert!(report.contains("0.500"));
}

#[test]
fn report_timing_uses_liberty_negative_unate_sense() {
    let dir = temp_dir("report-timing-negative-unate");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_fall (delay_template) { values ( "0.30" ); }
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
        r"
module top(
  input logic a,
  output logic y
);
  INV U1(.A(a), .Y(y));
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

    let report = session
        .report_timing(&ReportTimingOptions {
            from: vec!["a".to_string()],
            to: vec!["y".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(report.contains("U1/Y (INV)"));
    assert!(report.contains("0.300"));
    assert!(
        report
            .lines()
            .any(|line| line.starts_with("| y ") && line.contains("| f"))
    );
}

#[test]
fn hierarchical_timing_keeps_a_full_domain_artifact_across_the_boundary() {
    let dir = temp_dir("hierarchical-timing");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (INV) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) {
      direction : output;
      function : "!A";
      timing () {
        related_pin : "A";
        cell_rise (t) { values ( "0.2" ); }
        cell_fall (t) { values ( "0.2" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    let rtl_path = dir.join("hierarchy.sv");
    std::fs::write(
        &rtl_path,
        "module child(input logic a, output logic y, output logic unused); assign y = ~a; assign unused = a; endmodule\n\
         module top(input logic a, output logic y); logic n; assign n = ~a; child u_child(.a(n), .y(y)); endmodule\n",
    )
    .unwrap();
    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .import_verilog(std::slice::from_ref(&rtl_path), &FrontendOptions::default())
        .unwrap();
    session.set_current_design("top").unwrap();
    session.synthesize().unwrap();

    let report = session
        .report_timing(&ReportTimingOptions {
            from: vec!["a".to_string()],
            to: vec!["y".to_string()],
            ..ReportTimingOptions::default()
        })
        .unwrap();
    let area = session.report_area().unwrap();
    let qor = session.report_qor().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(!report.contains("u_child/U1"), "{report}");
    assert!(report.contains("Data arrival time: 0.400"), "{report}");
    assert!(area.contains("Number of ports: 2"), "{area}");
    assert!(area.contains("Number of combinational cells: 2"), "{area}");
    assert!(area.contains("Total cell area: 2.000000"), "{area}");
    assert!(qor.contains("Timing paths: 0"), "{qor}");
    assert!(!qor.contains("Critical Path Length:"), "{qor}");
}

#[test]
fn create_clock_tracks_timing_state() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    let clk = design.intern_name("clk").unwrap();
    design.ports.push(Port {
        name: clk,
        direction: Direction::Input,
        width: 1,
    });
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();
    let sources = session
        .resolve_port_ids("create_clock", &["clk".to_string()])
        .unwrap();
    let message = session
        .create_clock("sys_clk", 10.0, sources, Some((0.0, 5.0)))
        .unwrap();

    assert_eq!(message, "Created clock 'sys_clk' period 10.000");
    assert!(session.report_clock().contains("sys_clk"));
    assert!(session.report_clock().contains("clk"));
    assert!(session.check_timing().is_err());
}

#[test]
fn clock_collection_filters_by_pattern() {
    let mut session = Session::new();
    session
        .create_clock("sys_clk", 10.0, Vec::new(), None)
        .unwrap();
    session
        .create_clock("scan_clk", 20.0, Vec::new(), None)
        .unwrap();

    let clocks = session.get_clocks("sys*").unwrap();
    let handle = session.collection_handles(clocks).join(" ");
    assert_eq!(
        session.collection_object_names(&handle).unwrap(),
        ["sys_clk".to_string()]
    );
}

#[test]
fn spef_elmore_and_incremental_state_survive_a_new_process() {
    let dir = temp_dir("spef-elmore-checkpoint");
    let library = dir.join("demo.lib");
    let verilog = dir.join("top.v");
    let spef = dir.join("top.spef");
    let checkpoint = dir.join("top.ock");
    std::fs::write(
        &library,
        r#"
library (demo) {
  time_unit : "1ps";
  capacitive_load_unit (1, ff);
  default_wire_load : wl;
  wire_load (wl) {
    capacitance : 0.2;
    resistance : 0.1;
    slope : 0.0;
    fanout_length (1, 1.0);
  }
  cell (BUF) {
    area : 1.0;
    pin (A) {
      direction : input;
      capacitance : 0.1;
      rise_capacitance : 0.2;
      fall_capacitance : 0.3;
    }
    pin (Y) {
      direction : output;
      function : "A";
      timing () {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise (scalar) { values ( "0.1" ); }
        cell_fall (scalar) { values ( "0.1" ); }
        rise_transition (scalar) { values ( "0.1" ); }
        fall_transition (scalar) { values ( "0.1" ); }
      }
    }
  }
}
"#,
    )
    .unwrap();
    std::fs::write(
        &verilog,
        "module top(input a, output z); wire n; BUF U1(.A(a), .Y(n)); BUF U2(.A(n), .Y(z)); endmodule\n",
    )
    .unwrap();
    std::fs::write(
        &spef,
        r#"
*SPEF "IEEE 1481-1998"
*DESIGN "top"
*DIVIDER /
*DELIMITER :
*C_UNIT 1 FF
*R_UNIT 1 OHM
*D_NET n 1.0
*CONN
*I U1:Y O
*I U2:A I
*CAP
1 U2:A 1.0
*RES
1 U1:Y U2:A 1000.0
*END
"#,
    )
    .unwrap();

    let configure = |session: &mut Session| {
        session.read_libs(std::slice::from_ref(&library)).unwrap();
    };
    let mut original = Session::new();
    original.set_synth_effort(SynthesisEffort::Low);
    configure(&mut original);
    original
        .import_verilog(std::slice::from_ref(&verilog), &FrontendOptions::default())
        .unwrap();
    original.synthesize().unwrap();
    original
        .read_parasitics(
            std::slice::from_ref(&spef),
            &ReadParasiticsOptions {
                delay_model: ParasiticDelayModel::Elmore,
                ..ReadParasiticsOptions::default()
            },
        )
        .unwrap();
    original.synthesize().unwrap();
    let options = ReportTimingOptions {
        significant_digits: 6,
        ..ReportTimingOptions::default()
    };
    let expected = original.report_timing(&options).unwrap();
    assert!(expected.contains("U2/A (BUF)"), "{expected}");
    assert!(expected.contains("1.100000"), "{expected}");
    assert!(expected.contains("U1/A (BUF)"), "{expected}");
    assert!(expected.contains("z (net)"), "{expected}");
    assert!(
        expected.contains("Data arrival time: 1.370000"),
        "{expected}"
    );
    original.write_checkpoint_file(&checkpoint).unwrap();

    let mut restored = Session::new();
    configure(&mut restored);
    restored.read_checkpoint_file(&checkpoint).unwrap();
    let mut events = Vec::new();
    restored
        .synthesize_observed(SynthesisEffort::Low, &mut |event| events.push(event))
        .unwrap();
    assert_eq!(
        events,
        [SynthesisEvent::Completed {
            design: "top".to_string(),
            synthesized: false,
        }]
    );
    assert_eq!(restored.report_timing(&options).unwrap(), expected);
    std::fs::remove_dir_all(dir).unwrap();
}
