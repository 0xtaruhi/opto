// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn selects_wire_tree_from_the_default_operating_condition() {
    for (tree, expected) in [
        ("balanced_tree", crate::WireLoadTree::Balanced),
        ("worst_case_tree", crate::WireLoadTree::WorstCase),
        ("best_case_tree", crate::WireLoadTree::BestCase),
    ] {
        let source = format!(
            r#"library(demo) {{
            operating_conditions(unselected) {{ tree_type : worst_case_tree; }}
            default_operating_conditions : selected;
            operating_conditions(selected) {{ tree_type : "{tree}"; }}
        }}"#
        );
        let imported = parse_liberty(&source, "tree.lib").unwrap();
        assert_eq!(imported.wire_load_tree, expected);
        let mut store = crate::LibraryStore::default();
        store.append(vec![imported]).unwrap();
        let revision = store.current();
        let library = revision
            .timing_library(&revision.all_libraries(false))
            .unwrap();
        assert_eq!(library.wire_load_tree, expected);
    }
    for body in [
        "",
        "operating_conditions(unselected) { tree_type : worst_case_tree; }",
        "default_operating_conditions : selected; operating_conditions(selected) {}",
    ] {
        let source = format!("library(demo) {{ {body} }}");
        assert_eq!(
            parse_liberty(&source, "tree.lib").unwrap().wire_load_tree,
            crate::WireLoadTree::Balanced
        );
    }
}

#[test]
fn rejects_invalid_wire_tree_selection() {
    for body in [
        "default_operating_conditions : missing;",
        "operating_conditions(op) { tree_type : unknown; }",
        "operating_conditions(op) { tree_type : balanced_tree; tree_type : best_case_tree; }",
        "operating_conditions(op) {} operating_conditions(op) {}",
        "operating_conditions(op) { tree_type(balanced_tree); }",
    ] {
        let source = format!("library(demo) {{ {body} }}");
        assert!(matches!(
            parse_liberty(&source, "tree.lib"),
            Err(LibraryError::UnsupportedConstruct { .. })
        ));
    }
}

const LIB: &str = r#"
library(demo) {
  time_unit : "1ns";
  capacitive_load_unit (1, ff);
  pulling_resistance_unit : "1kohm";
  default_fanout_load : 2.0;
  default_operating_conditions : slow;
  operating_conditions(slow) { tree_type : balanced_tree; }
  default_wire_load : wl;
  default_wire_load_mode : enclosed;
  wire_load(wl) {
    capacitance : 0.2;
    resistance : 0.1;
    slope : 2.0;
    fanout_length(1, 3.0);
    fanout_length(2, 5.0);
  }
  cell(INVX1) {
    area : 1.25;
    pin(A) {
      direction : input;
      capacitance : 0.02;
      rise_capacitance : 0.021;
      fall_capacitance : 0.019;
      fanout_load : 0.5;
    }
    pin(Y) {
      direction : output;
      function : "!A";
      timing() {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_rise(scalar) { values("0.1"); }
        cell_fall(scalar) { values("0.2"); }
      }
    }
  }
  cell(TBUFX1) {
    area : 1.5;
    pin(A) { direction : input; }
    pin(OE) { direction : input; }
    pin(Y) {
      direction : output;
      function : "A";
      three_state : "!OE";
    }
  }
  cell(DLYX1) {
    area : 2.5;
    dont_use : true;
    pin(A) { direction : input; }
    pin(Y) { direction : output; function : "A"; }
  }
}
"#;

#[test]
fn imports_typed_liberty_model() {
    let library = parse_liberty(LIB, "inline.lib").unwrap();

    assert_eq!(library.name, "demo");
    assert_eq!(library.cell_count, 3);
    assert_eq!(library.pin_count, 7);
    assert_eq!(library.units.time_seconds, Some(1e-9));
    assert_eq!(library.units.capacitance_farads, Some(1e-15));
    assert_eq!(library.units.resistance_ohms, Some(1e3));
    assert!((library.units.normalize_resistance(1.0) - 1e-3).abs() < 1e-15);
    assert_eq!(
        library
            .target_cells()
            .iter()
            .map(crate::TargetCellRef::name)
            .collect::<Vec<_>>(),
        ["INVX1", "TBUFX1", "DLYX1"]
    );
    let inv = library.target_cells.get(0).unwrap();
    let inv_a = inv.pins().next().unwrap();
    let inv_y = inv.pins().nth(1).unwrap();
    let tbuf = library.target_cells.get(1).unwrap();
    let tbuf_a = tbuf.pins().next().unwrap();
    let tbuf_y = tbuf.pins().nth(2).unwrap();
    assert!(!inv.dont_use());
    assert!(library.target_cells.get(2).unwrap().dont_use());
    assert_eq!(inv_a.capacitance(), Some(0.02));
    assert_eq!(inv_a.capacitance_at(crate::TimingEdge::Rise), Some(0.021));
    assert_eq!(inv_a.capacitance_at(crate::TimingEdge::Fall), Some(0.019));
    assert_eq!(inv_a.fanout_load(), Some(0.5));
    assert_eq!(tbuf_a.fanout_load(), Some(2.0));
    let wire_load = &library.wire_loads["wl"];
    assert!((wire_load.capacitance_at(1.0) - 0.6).abs() < 1e-12);
    assert!((wire_load.resistance_at(3.0) - 0.7).abs() < 1e-12);
    assert_eq!(
        inv_y
            .function()
            .and_then(crate::BooleanFunctionRef::as_literal),
        Some(("A", false))
    );
    assert_eq!(
        inv_y.timing_arcs().next().unwrap().default_delay(),
        Some(0.2)
    );
    assert_eq!(
        tbuf_y
            .three_state()
            .and_then(crate::BooleanFunctionRef::as_literal),
        Some(("OE", false))
    );
}

#[test]
fn imports_special_cell_usage() {
    let library = parse_liberty(
        r#"
library(demo) {
  cell(ISO) {
    is_isolation_cell : true;
    pin(A) { direction : input; }
    pin(EN) { direction : input; }
    pin(Y) { direction : output; function : "A & EN"; }
  }
  cell(LS) {
    is_level_shifter : true;
    pin(A) { direction : input; }
    pin(Y) { direction : output; function : "A"; }
  }
	  cell(ICG) {
	    clock_gating_integrated_cell : latch_posedge;
	    pin(CK) { direction : input; }
	    pin(GCK) { direction : output; function : "CK"; }
	  }
	  cell(AON) {
	    always_on : true;
	    pin(A) { direction : input; }
	    pin(Y) { direction : output; function : "A"; }
	  }
	}
"#,
        "special.lib",
    )
    .unwrap();

    assert_eq!(
        library.target_cells.get(0).unwrap().usage(),
        crate::TargetCellUsage::ISOLATION
    );
    assert_eq!(
        library.target_cells.get(1).unwrap().usage(),
        crate::TargetCellUsage::LEVEL_SHIFTER
    );
    assert_eq!(
        library.target_cells.get(2).unwrap().usage(),
        crate::TargetCellUsage::INTEGRATED_CLOCK_GATING
    );
    assert_eq!(
        library.target_cells.get(3).unwrap().usage(),
        crate::TargetCellUsage::ALWAYS_ON
    );
    assert!(
        library
            .target_cells
            .iter()
            .all(|cell| !cell.is_synthesis_eligible())
    );
}

#[test]
fn imports_level_sensitive_latch_semantics() {
    let library = parse_liberty(
        r#"
library(demo) {
  cell(LHQD1) {
    area : 1.8;
    latch(IQ, IQN) {
      data_in : "D";
      enable : "E";
      clear : "!RN";
    }
    pin(D) { direction : input; nextstate_type : data; }
    pin(E) { direction : input; }
    pin(RN) { direction : input; nextstate_type : clear; }
    pin(Q) { direction : output; function : "IQ"; }
  }
}
"#,
        "latch.lib",
    )
    .unwrap();

    let sequential = library
        .target_cells
        .get(0)
        .unwrap()
        .sequential()
        .next()
        .unwrap();
    assert_eq!(sequential.kind(), crate::TargetSequentialKind::Latch);
    assert_eq!(
        sequential
            .next_state()
            .and_then(crate::BooleanFunctionRef::as_literal),
        Some(("D", true))
    );
    assert_eq!(
        sequential
            .enable()
            .and_then(crate::BooleanFunctionRef::as_literal),
        Some(("E", true))
    );
    assert_eq!(
        sequential
            .clear()
            .and_then(crate::BooleanFunctionRef::as_literal),
        Some(("RN", false))
    );
}

#[test]
fn rejects_non_lib_inputs_without_archive_fallback() {
    let error = read_lib_input(Path::new("cells.tar.gz")).unwrap_err();
    assert!(error.to_string().contains("expected a Liberty .lib file"));
}

#[test]
fn retains_valid_timing_types_outside_the_current_sta_scope() {
    let library = parse_liberty(
        r#"
library(demo) {
  cell(DFF) {
    pin(CP) { direction : input; }
    pin(D) {
      direction : input;
      timing() {
        related_pin : "CP";
        timing_type : recovery_rising;
        rise_constraint(scalar) { values("0.1"); }
      }
    }
  }
}
"#,
        "unsupported.lib",
    )
    .unwrap();

    assert_eq!(
        library
            .target_cells
            .get(0)
            .unwrap()
            .pins()
            .nth(1)
            .unwrap()
            .timing_arcs()
            .next()
            .unwrap()
            .timing_type(),
        crate::TargetTimingType::Recovery(crate::TimingEdge::Rise)
    );
}

#[test]
fn imports_ccs_waveforms_with_scalar_timing_tables() {
    let library = parse_liberty(
        r#"
library(demo) {
  time_unit : "1s";
  current_unit : "1A";
  voltage_unit : "1V";
  capacitive_load_unit (1, F);
  nom_voltage : 1;
  input_threshold_pct_rise : 50;
  output_threshold_pct_rise : 50;
  slew_lower_threshold_pct_rise : 20;
  slew_upper_threshold_pct_rise : 80;
  output_current_template(ccs_template) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    variable_3 : time;
  }
  lu_table_template(cap_template) {
    index_1("1");
    index_2("1");
  }
  cell(BUF) {
    pin(A) { direction : input; capacitance : 1; }
    pin(Y) {
      direction : output;
      function : "A";
      timing() {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise(cap_template) { values("0.7"); }
        rise_transition(cap_template) { values("0.9"); }
        receiver_capacitance1_rise(cap_template) { values("2"); }
        receiver_capacitance2_rise(cap_template) { values("4"); }
        output_current_rise() {
          vector(ccs_template) {
            reference_time : 0;
            index_1(1);
            index_2(1);
            index_3("0, 1");
            values("1, 1");
          }
        }
      }
    }
  }
}
"#,
        "ccs.lib",
    )
    .unwrap();

    let arc = library
        .target_cells
        .get(0)
        .unwrap()
        .pins()
        .nth(1)
        .unwrap()
        .timing_arcs()
        .next()
        .unwrap();
    assert_eq!(library.timing_models.ccs, 1);
    assert_eq!(library.units.time_seconds, Some(1.0));
    assert_eq!(library.units.capacitance_farads, Some(1.0));
    assert_eq!(
        arc.delay_model().map(crate::ArcDelayModel::kind),
        Some(crate::TimingModelKind::Ccs)
    );
    assert_eq!(
        arc.delay_at(crate::TimingEdge::Rise, Some(1.0), Some(1.0)),
        Some(0.7)
    );
    assert_eq!(
        arc.transition_at(crate::TimingEdge::Rise, Some(1.0), Some(1.0)),
        Some(0.9)
    );
    assert_eq!(
        arc.receiver_capacitance_at(
            crate::TimingEdge::Rise,
            crate::TimingEdge::Rise,
            Some(1.0),
            Some(1.0),
        ),
        Some(3.0)
    );
}

#[test]
fn imports_ecsm_waveforms_with_scalar_timing_tables() {
    let library = parse_liberty(
        r#"
library(demo) {
  output_threshold_pct_rise : 50;
  slew_lower_threshold_pct_rise : 20;
  slew_upper_threshold_pct_rise : 80;
  lu_table_template(timing_template) {
    variable_1 : input_net_transition;
    variable_2 : total_output_net_capacitance;
    index_1("1");
    index_2("1");
  }
  cell(BUF) {
    pin(A) { direction : input; capacitance : 1; }
    pin(Y) {
      direction : output;
      function : "A";
      timing() {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise(timing_template) { values("0.7"); }
        rise_transition(timing_template) {
          values("0.9");
          ecsm_waveform("0") {
            index_1 : "0, 0.5, 1";
            values : "0, 0.4, 1";
          }
          ecsm_capacitance(rise) { values : "2"; }
        }
      }
    }
  }
}
"#,
        "ecsm.lib",
    )
    .unwrap();

    let arc = library
        .target_cells
        .get(0)
        .unwrap()
        .pins()
        .nth(1)
        .unwrap()
        .timing_arcs()
        .next()
        .unwrap();
    assert_eq!(library.timing_models.ecsm, 1);
    assert_eq!(
        arc.delay_model().map(crate::ArcDelayModel::kind),
        Some(crate::TimingModelKind::Ecsm)
    );
    assert_eq!(
        arc.delay_at(crate::TimingEdge::Rise, Some(1.0), Some(1.0)),
        Some(0.7)
    );
    assert_eq!(
        arc.transition_at(crate::TimingEdge::Rise, Some(1.0), Some(1.0)),
        Some(0.9)
    );
    assert_eq!(
        arc.receiver_capacitance_at(
            crate::TimingEdge::Rise,
            crate::TimingEdge::Rise,
            Some(1.0),
            Some(1.0),
        ),
        Some(2.0)
    );
}

#[test]
fn imports_ecsm_waveform_sets() {
    let library = parse_liberty(
        r#"
library(demo) {
  output_threshold_pct_rise : 50;
  slew_lower_threshold_pct_rise : 20;
  slew_upper_threshold_pct_rise : 80;
  lu_table_template(timing_template) {
    index_1("1");
    index_2("1");
  }
  ecsm_lut_template(waveform_template) {
    index_1("0, 0.5, 1");
  }
  cell(BUF) {
    pin(A) { direction : input; }
    pin(Y) {
      direction : output;
      timing() {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise(timing_template) { values("0.7"); }
        rise_transition(timing_template) {
          values("0.9");
          ecsm_waveform_set(waveform_template) {
            values("0, 0.4, 1");
          }
        }
      }
    }
  }
}
"#,
        "ecsm-set.lib",
    )
    .unwrap();

    let arc = library
        .target_cells
        .get(0)
        .unwrap()
        .pins()
        .nth(1)
        .unwrap()
        .timing_arcs()
        .next()
        .unwrap();
    assert_eq!(
        arc.delay_at(crate::TimingEdge::Rise, Some(1.0), Some(1.0)),
        Some(0.7)
    );
    assert_eq!(
        arc.transition_at(crate::TimingEdge::Rise, Some(1.0), Some(1.0)),
        Some(0.9)
    );
}

#[test]
fn imports_pin_level_ccs_and_ecsm_receiver_models() {
    let library = parse_liberty(
        r#"
library(demo) {
  lu_table_template(receiver_template) { index_1("1"); }
  cell(SINK) {
    pin(A) {
      direction : input;
      capacitance : 1;
      receiver_capacitance() {
        receiver_capacitance1_rise(receiver_template) { values("2"); }
        receiver_capacitance2_rise(receiver_template) { values("4"); }
      }
    }
    pin(B) {
      direction : input;
      capacitance : 1;
      ecsm_capacitance(rise) { index_1 : "1"; values : "2"; }
      ecsm_capacitance(rise) { index_1 : "1"; values : "3"; }
      ecsm_capacitance(fall) { index_1 : "1"; values : "4"; }
    }
  }
}
"#,
        "pin-receiver.lib",
    )
    .unwrap();

    let cell = library.target_cells.get(0).unwrap();
    let ccs = cell
        .pins()
        .find(|pin| pin.name() == "A")
        .and_then(crate::TargetPinRef::receiver_capacitance)
        .unwrap();
    assert_eq!(
        ccs.capacitance_at(crate::TimingEdge::Rise, Some(1.0)),
        Some(3.0)
    );
    let ecsm = cell
        .pins()
        .find(|pin| pin.name() == "B")
        .and_then(crate::TargetPinRef::receiver_capacitance)
        .unwrap();
    assert_eq!(
        ecsm.capacitance_at(crate::TimingEdge::Rise, Some(1.0)),
        Some(3.0)
    );
    assert_eq!(
        ecsm.capacitance_at(crate::TimingEdge::Fall, Some(1.0)),
        Some(4.0)
    );
}

#[test]
fn rejects_incomplete_advanced_waveform_grids() {
    let error = parse_liberty(
        r#"
library(demo) {
  lu_table_template(timing_template) {
    index_1("1");
    index_2("1, 2");
  }
  cell(BUF) {
    pin(A) { direction : input; }
    pin(Y) {
      direction : output;
      timing() {
        related_pin : "A";
        rise_transition(timing_template) {
          ecsm_waveform("0") {
            index_1 : "0, 1";
            values : "0, 1";
          }
        }
      }
    }
  }
}
"#,
        "broken-ecsm.lib",
    )
    .unwrap_err();

    assert!(error.to_string().contains("waveform index 1 is missing"));
}

#[test]
fn rejects_advanced_waveforms_without_scalar_timing_anchors() {
    let error = parse_liberty(
        r#"
library(demo) {
  lu_table_template(timing_template) {
    index_1("1");
    index_2("1");
  }
  cell(BUF) {
    pin(A) { direction : input; }
    pin(Y) {
      direction : output;
      timing() {
        related_pin : "A";
        rise_transition(timing_template) {
          ecsm_waveform("0") {
            index_1 : "0, 1";
            values : "0, 1";
          }
        }
      }
    }
  }
}
"#,
        "missing-scalar-anchor.lib",
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("requires both scalar cell delay and transition tables")
    );
}

#[test]
fn imports_liberty_leakage_and_internal_power() {
    let library = parse_liberty(
        r#"
library(power_demo) {
  time_unit : "1ns";
  capacitive_load_unit(1, pf);
  voltage_unit : "1V";
  current_unit : "1mA";
  leakage_power_unit : "1nW";
  nom_voltage : 1.0;
  power_lut_template(power_template) {
    variable_1 : input_transition_time;
    variable_2 : total_output_net_capacitance;
    index_1("0.1");
    index_2("0.2");
  }
  cell(BUF) {
    cell_leakage_power : 2.0;
    leakage_power() { when : "A"; value : 3.0; }
    leakage_power() { when : "!A"; value : 1.0; }
    pin(A) { direction : input; capacitance : 0.1; }
    pin(Y) {
      direction : output;
      function : "A";
      internal_power() {
        related_pin : "A";
        rise_power(power_template) { values("4.0"); }
        fall_power(power_template) { values("6.0"); }
      }
    }
  }
}
"#,
        "power.lib",
    )
    .unwrap();

    assert_eq!(library.power_units.time_seconds, Some(1e-9));
    assert_eq!(library.power_units.capacitance_farads, Some(1e-12));
    assert_eq!(library.power_units.voltage_volts, Some(1.0));
    assert_eq!(library.power_units.leakage_power_watts, Some(1e-9));
    let cell = &library.power_cells[0];
    assert_eq!(cell.cell_leakage_power, Some(2.0));
    assert_eq!(cell.leakage_power.len(), 2);
    assert_eq!(cell.pins[0].name, "Y");
    let internal = &cell.pins[0].internal_power[0];
    assert_eq!(internal.related_pin.as_deref(), Some("A"));
    assert_eq!(
        internal
            .rise_power
            .as_ref()
            .and_then(crate::LookupTable::default_value),
        Some(4.0)
    );
    assert_eq!(
        internal
            .fall_power
            .as_ref()
            .and_then(crate::LookupTable::default_value),
        Some(6.0)
    );
}

#[test]
fn tolerates_complex_attributes_without_semicolons_across_lines() {
    let library = parse_liberty(
        r#"
library(demo) {
  wire_load("ZeroWireload") {
    resistance : 0.00001 ;
    capacitance : 1 ;
    area : 0
    slope : 0 ;
    fanout_length(1,0.0000)
    fanout_length(2,0.0000)
    fanout_length(3,0.0000)
  }
}
"#,
        "tolerant.lib",
    )
    .unwrap();

    let model = &library.wire_loads["ZeroWireload"];
    assert!(model.capacitance_at(2.0).abs() <= f64::EPSILON);
}

#[test]
fn rejects_unterminated_complex_attributes_on_one_line() {
    let error = parse_liberty(
        r"
library(demo) {
  wire_load(wl) {
    fanout_length(1,0.0) fanout_length(2,0.0);
  }
}
",
        "strict.lib",
    )
    .unwrap_err();

    assert!(error.to_string().contains("'{' or ';'"));
}

#[test]
fn imports_standard_liberty_memory_shape_and_bus_port_contracts() {
    let library = parse_liberty(
        r#"
library(memory_demo) {
  bus_naming_style : "%s[%d]";
  type(addr_t) { base_type : array; data_type : bit; bit_width : 2; }
  type(data_t) { base_type : array; data_type : bit; bit_from : 7; bit_to : 0; }
  cell(RAM4X8) {
    area : 42.0;
    memory() { type : ram; address_width : 2; word_width : 8; }
    bus(A) { bus_type : addr_t; direction : input; }
    bus(D) {
      bus_type : data_t;
      direction : input;
      memory_write() { address : A; clocked_on : CLK; enable : WE; }
    }
    bus(Q) {
      bus_type : data_t;
      direction : output;
      memory_read() { address : A; }
      timing() {
        related_pin : "A[0]";
        cell_rise(scalar) { values("0.25"); }
        cell_fall(scalar) { values("0.25"); }
      }
    }
    pin(CLK) { direction : input; }
    pin(WE) { direction : input; }
  }
}
"#,
        "memory.lib",
    )
    .unwrap();

    library.target_cells.validate_for_synthesis().unwrap();
    let cell = library.target_cells.get(0).unwrap();
    let memory = cell.memory().unwrap();
    assert_eq!(memory.kind, crate::TargetMemoryKind::Ram);
    assert_eq!((memory.depth, memory.word_width), (4, 8));
    assert_eq!(memory.read_ports.len(), 1);
    assert_eq!(memory.write_ports.len(), 1);
    assert_eq!(memory.read_ports[0].address_pins, ["A[0]", "A[1]"]);
    assert_eq!(memory.read_ports[0].data_pins[0], "Q[0]");
    assert_eq!(memory.write_ports[0].data_pins[7], "D[7]");
    assert_eq!(memory.write_ports[0].clock.pin, "CLK");
    assert_eq!(memory.write_ports[0].enable.as_ref().unwrap().pin, "WE");
    assert!(
        cell.pins()
            .filter(|pin| pin.name().starts_with("Q["))
            .all(|pin| pin.timing_arcs().count() == 1)
    );
}
