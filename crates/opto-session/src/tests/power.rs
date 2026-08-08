// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn activity_update_recomputes_only_the_affected_power_cone() {
    let directory = temp_dir("incremental-power");
    let library = directory.join("power.lib");
    let verilog = directory.join("top.v");
    std::fs::write(&library, power_library()).unwrap();
    std::fs::write(
        &verilog,
        "module top(input a, input b, output za, output zb); wire na, nb; BUF U1(.A(a), .Y(na)); BUF U2(.A(na), .Y(za)); BUF U3(.A(b), .Y(nb)); BUF U4(.A(nb), .Y(zb)); endmodule\n",
    )
    .unwrap();
    let mut session = Session::new();
    session.set_synth_effort(SynthesisEffort::Low);
    session.read_libs(std::slice::from_ref(&library)).unwrap();
    session
        .import_verilog(std::slice::from_ref(&verilog), &FrontendOptions::default())
        .unwrap();
    session.synthesize().unwrap();
    let a = session
        .resolve_port_ids("set_switching_activity", &["a".to_string()])
        .unwrap()
        .into_iter()
        .map(opto_db::ObjectId::erase)
        .collect::<Vec<_>>();
    session
        .set_switching_activity(
            SwitchingActivityUpdate {
                static_probability: Some(0.25),
                toggle_rate: Some(0.02),
                rise_ratio: None,
            },
            &a,
        )
        .unwrap();

    session
        .report_power(&ReportPowerOptions::default())
        .unwrap();
    let full = session.power_engine_metrics().unwrap();
    assert_eq!(full.full_updates, 1);
    assert_eq!(full.recomputed_cells, 4);
    session
        .report_power(&ReportPowerOptions::default())
        .unwrap();
    assert_eq!(session.power_engine_metrics().unwrap().cache_hits, 1);

    session
        .set_switching_activity(
            SwitchingActivityUpdate {
                static_probability: Some(0.75),
                toggle_rate: Some(0.04),
                rise_ratio: None,
            },
            &a,
        )
        .unwrap();
    session
        .report_power(&ReportPowerOptions::default())
        .unwrap();
    let incremental = session.power_engine_metrics().unwrap();
    assert_eq!(incremental.incremental_updates, 1);
    assert_eq!(incremental.recomputed_cells - full.recomputed_cells, 2);
    assert_eq!(incremental.recomputed_nets - full.recomputed_nets, 3);

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn library_change_releases_the_complete_analysis_generation() {
    let directory = temp_dir("power-cache-invalidation");
    let library = directory.join("power.lib");
    let verilog = directory.join("top.v");
    std::fs::write(&library, power_library()).unwrap();
    std::fs::write(
        &verilog,
        "module top(input a, output y); BUF U0(.A(a), .Y(y)); endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    session.read_libs(std::slice::from_ref(&library)).unwrap();
    session
        .import_verilog(std::slice::from_ref(&verilog), &FrontendOptions::default())
        .unwrap();
    session.synthesize().unwrap();
    session
        .report_power(&ReportPowerOptions::default())
        .unwrap();
    assert!(session.process.timing_model_cache.borrow().is_some());
    let before = session.power_engine_metrics().unwrap();

    session
        .mark_library_cells_unavailable(&["BUF".to_string()])
        .unwrap();
    assert!(session.process.timing_model_cache.borrow().is_none());
    session
        .report_power(&ReportPowerOptions::default())
        .unwrap();
    let after = session.power_engine_metrics().unwrap();
    assert_eq!(after.full_updates, before.full_updates + 1);
    assert_eq!(after.cache_hits, before.cache_hits);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn repeated_occurrences_keep_independent_activity_contexts() {
    let directory = temp_dir("occurrence-power");
    let library = directory.join("power.lib");
    let verilog = directory.join("hierarchy.v");
    std::fs::write(&library, power_library()).unwrap();
    std::fs::write(
        &verilog,
        "module child(input a, output y); wire private; BUF U0(.A(a), .Y(private)); BUF U1(.A(private), .Y(y)); endmodule\n\
         module top(input a, input b, output ya, output yb); child u0(.a(a), .y(ya)); child u1(.a(b), .y(yb)); endmodule\n",
    )
    .unwrap();
    let mut session = Session::new();
    session.set_synth_effort(SynthesisEffort::Low);
    session.read_libs(std::slice::from_ref(&library)).unwrap();
    session
        .import_verilog(std::slice::from_ref(&verilog), &FrontendOptions::default())
        .unwrap();
    session.set_current_design("top").unwrap();
    session.synthesize().unwrap();

    for (name, toggle_rate) in [("a", 0.1), ("b", 0.3)] {
        let object = session
            .resolve_port_ids("set_switching_activity", &[name.to_string()])
            .unwrap()
            .into_iter()
            .map(opto_db::ObjectId::erase)
            .collect::<Vec<_>>();
        session
            .set_switching_activity(
                SwitchingActivityUpdate {
                    static_probability: Some(0.5),
                    toggle_rate: Some(toggle_rate),
                    rise_ratio: Some(0.5),
                },
                &object,
            )
            .unwrap();
    }

    let options = ReportPowerOptions {
        kind: PowerReportKind::Cell,
        flat: true,
        ..ReportPowerOptions::default()
    };
    let report = session.report_power(&options).unwrap();
    let u0 = report
        .lines()
        .find(|line| line.starts_with("| u0/U1"))
        .unwrap();
    let u1 = report
        .lines()
        .find(|line| line.starts_with("| u1/U1"))
        .unwrap();
    assert!(u0.contains("0.1000"), "{u0}");
    assert!(u1.contains("0.3000"), "{u1}");
    let full = session.power_engine_metrics().unwrap();

    let a = session
        .resolve_port_ids("set_switching_activity", &["a".to_string()])
        .unwrap()
        .into_iter()
        .map(opto_db::ObjectId::erase)
        .collect::<Vec<_>>();
    session
        .set_switching_activity(
            SwitchingActivityUpdate {
                static_probability: None,
                toggle_rate: Some(0.4),
                rise_ratio: None,
            },
            &a,
        )
        .unwrap();
    let updated = session.report_power(&options).unwrap();
    let u0 = updated
        .lines()
        .find(|line| line.starts_with("| u0/U1"))
        .unwrap();
    let u1 = updated
        .lines()
        .find(|line| line.starts_with("| u1/U1"))
        .unwrap();
    assert!(u0.contains("0.4000"), "{u0}");
    assert!(u1.contains("0.3000"), "{u1}");
    let incremental = session.power_engine_metrics().unwrap();
    assert_eq!(incremental.incremental_updates, 1);
    assert_eq!(incremental.recomputed_cells - full.recomputed_cells, 2);

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn occurrence_specific_activity_names_are_rejected_explicitly() {
    let mut session = Session::new();
    session
        .install_design_fresh(
            empty_rtl_module("top"),
            opto_db::RevisionId::INITIAL,
            opto_db::DesignIndex::new("top"),
        )
        .unwrap();
    session.set_current_design("top").unwrap();

    let error = session
        .resolve_power_objects("set_switching_activity", &["u0/net".to_string()])
        .unwrap_err();
    assert!(error.to_string().contains("occurrence-specific"));
}

fn power_library() -> &'static str {
    r#"
library(power_demo) {
  time_unit : "1ns";
  capacitive_load_unit(1, pf);
  voltage_unit : "1V";
  current_unit : "1mA";
  leakage_power_unit : "1nW";
  nom_voltage : 1.0;
  cell(BUF) {
    area : 1.0;
    cell_leakage_power : 1.0;
    pin(A) { direction : input; capacitance : 0.1; }
    pin(Y) {
      direction : output;
      function : "A";
      internal_power() {
        related_pin : "A";
        rise_power(scalar) { values("1.0"); }
        fall_power(scalar) { values("1.0"); }
      }
      timing() {
        related_pin : "A";
        timing_sense : positive_unate;
        cell_rise(scalar) { values("0.1"); }
        cell_fall(scalar) { values("0.1"); }
        rise_transition(scalar) { values("0.1"); }
        fall_transition(scalar) { values("0.1"); }
      }
    }
  }
}
"#
}
