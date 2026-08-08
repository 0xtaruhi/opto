// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    AreaCellKind, AreaReportContext, PowerReportKind, ReportPowerOptions, report_mapped_area,
    report_power, write_mapped_verilog,
};
use opto_db::{DesignId, ObjectUid, PortId};
use opto_ir::RevisionId;
use opto_ir::mapped::{ConnectionSignal, MappedBuilder, PortDirection};
use opto_library::{LibrarySelection, LibraryStore, parse_liberty};
use opto_power::{ActivityAnnotations, PowerAnalysis, PowerEngine};
use opto_runtime::ExecutionContext;
use opto_timing::{DelayType, TimingContext, TimingEngine, TimingModel};
use std::collections::BTreeMap;
use std::sync::Arc;

fn mapped_fixture() -> opto_ir::mapped::MappedNetlist {
    let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
    let a = builder.add_net(Some("a")).unwrap();
    let y = builder.add_net(Some("y")).unwrap();
    builder.add_port("a", PortDirection::Input, &[a]).unwrap();
    builder.add_port("y", PortDirection::Output, &[y]).unwrap();
    builder
        .add_cell(
            "U1",
            "INVX1",
            Some(0),
            &[
                ("A".to_string(), Some(0), ConnectionSignal::Net(a)),
                ("Y".to_string(), Some(1), ConnectionSignal::Net(y)),
            ],
        )
        .unwrap();
    builder.freeze().unwrap()
}

fn stable_timestamp(report: &str) -> String {
    report
        .lines()
        .map(|line| {
            if line.starts_with("Date:") {
                "Date: <timestamp>"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn snapshot_body(snapshot: &'static str) -> &'static str {
    snapshot
        .split_once("\n---\n")
        .expect("committed report golden has an insta metadata header")
        .1
        .trim_end_matches('\n')
}

fn power_analysis_fixture() -> (Arc<TimingModel>, PowerAnalysis) {
    let mut libraries = LibraryStore::default();
    libraries
        .append(vec![
            parse_liberty(
                r#"
library(demo) {
  time_unit : "1ns";
  capacitive_load_unit(1, pf);
  voltage_unit : "1V";
  leakage_power_unit : "1nW";
  nom_voltage : 1.0;
  cell(INVX1) {
    area : 1.25;
    cell_leakage_power : 2.0;
    pin(A) { direction : input; capacitance : 0.1; }
    pin(Y) {
      direction : output;
      function : "!A";
      timing() {
        related_pin : "A";
        timing_sense : negative_unate;
        cell_rise(t) { values("0.1"); }
        cell_fall(t) { values("0.1"); }
      }
    }
  }
}
"#,
                "demo.lib",
            )
            .unwrap(),
        ])
        .unwrap();
    let library = libraries
        .current()
        .timing_library(&LibrarySelection::parse("demo"))
        .unwrap();
    let uid = |raw| ObjectUid::from_raw(raw).unwrap();
    let model = Arc::new(
        TimingModel::from_mapped(
            &mapped_fixture(),
            DesignId::from_uid(uid(1)),
            &opto_timing::PortBindings::new([PortId::from_uid(uid(2)), PortId::from_uid(uid(3))]),
            library,
        )
        .unwrap(),
    );
    let runtime = ExecutionContext::default();
    let electrical = TimingEngine::new(runtime.clone())
        .electrical_snapshot(
            &TimingContext::default(),
            Arc::clone(&model),
            DelayType::Max,
        )
        .unwrap();
    let annotations = ActivityAnnotations::new(model.generation(), []).unwrap();
    let analysis = PowerEngine::new()
        .analyze(&runtime, Arc::clone(&model), electrical, annotations)
        .unwrap();
    (model, analysis)
}

#[test]
fn mapped_verilog_snapshot() {
    let mut output = Vec::new();
    write_mapped_verilog(&mut output, &mapped_fixture()).unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap().trim_end_matches('\n'),
        snapshot_body(include_str!(
            "snapshots/opto_formats__snapshot_tests__mapped_verilog_snapshot.snap"
        ))
    );
}

#[test]
fn area_report_snapshot() {
    let context = AreaReportContext {
        library_cell_area: BTreeMap::from([("INVX1".to_string(), 1.25)]),
        library_cell_kind: BTreeMap::from([("INVX1".to_string(), AreaCellKind::BufferInverter)]),
        libraries: vec![crate::AreaLibrary {
            name: "demo".to_string(),
            source: "demo.lib".to_string(),
        }],
    };
    assert_eq!(
        stable_timestamp(&report_mapped_area(&mapped_fixture(), &context).render_plain()),
        snapshot_body(include_str!(
            "snapshots/opto_formats__snapshot_tests__area_report_snapshot.snap"
        ))
    );
}

#[test]
fn power_report_snapshot() {
    let (model, analysis) = power_analysis_fixture();
    assert_eq!(
        stable_timestamp(
            &report_power(
                &model,
                &analysis,
                &ReportPowerOptions {
                    kind: PowerReportKind::Summary,
                    ..ReportPowerOptions::default()
                },
            )
            .unwrap()
            .render_plain(),
        ),
        snapshot_body(include_str!(
            "snapshots/opto_formats__snapshot_tests__power_report_snapshot.snap"
        ))
    );
}
