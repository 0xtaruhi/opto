// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn report_area_uses_active_library_cell_area() {
    let dir = temp_dir("report-area-liberty");
    let path = dir.join("demo.lib");
    std::fs::write(
        &path,
        r"
library (demo) {
  cell (INVX1) {
    area : 3.25;
    pin (A) { direction : input; }
    pin (Y) { direction : output; }
  }
}
",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![rtl_module_with_instance("top", "INVX1")],
                top: Some("top".to_string()),
                diagnostics: Vec::new(),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    let report = session.report_area().unwrap();
    assert!(report.contains("Version: opto 0.1.0"));
    assert!(report.contains("Date: "));
    assert!(report.contains("Information: Updating design information..."));
    assert!(!report.contains("UID-85"));
    assert!(report.contains("## Libraries"));
    assert!(report.contains("| demo"));
    assert!(report.contains(&format!("| {}", path.display())));
    assert!(report.contains("Number of cells: 1"));
    assert!(report.contains("Number of combinational cells: 1"));
    assert!(report.contains("Number of macros/black boxes: 0"));
    assert!(report.contains("Combinational area: 3.250000"));
    assert!(report.contains("Total cell area: 3.250000"));
}

#[test]
fn synthesize_maps_unary_inverter_from_mapping_library() {
    let dir = temp_dir("synthesis-target-library");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (INVX1) {
    area : 3.25;
    pin (A) { direction : input; }
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
    let cell = design.cell(0).unwrap();
    assert_eq!(cell.name, "U1");
    assert_eq!(cell.reference, "INVX1");
    let pins = cell
        .connections()
        .map(|connection| connection.port)
        .collect::<Vec<_>>();
    assert_eq!(pins, ["A", "Y"]);

    let out_path = dir.join("mapped.v");
    session
        .write_hdl_file(Some(out_path.clone()), &[], false)
        .unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    let area = session.report_area().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert!(mapped.contains("INVX1 U1(.A(a), .Y(y));"));
    assert!(area.contains("Number of buf/inv: 1"));
    assert!(area.contains("Buf/Inv area: 3.250000"));
    assert!(area.contains("Total cell area: 3.250000"));
}

#[test]
fn synthesize_maps_basic_logic_from_mapping_library() {
    let dir = temp_dir("synthesis-basic-target-library");
    let lib_path = dir.join("demo.lib");
    std::fs::write(
        &lib_path,
        r#"
library (demo) {
  cell (AND2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A B"; }
  }
  cell (OR2) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A+B"; }
  }
  cell (NAND2) {
    area : 1.5;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "!(A B)"; }
  }
  cell (XOR2) {
    area : 4.0;
    pin (A) { direction : input; }
    pin (B) { direction : input; }
    pin (Y) { direction : output; function : "A^B"; }
  }
  cell (MUX2) {
    area : 5.0;
    pin (S) { direction : input; }
    pin (I0) { direction : input; }
    pin (I1) { direction : input; }
    pin (Z) { direction : output; function : "((S I1) + (!S I0))"; }
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
  input logic s,
  output logic y_and,
  output logic y_or,
  output logic y_nand,
  output logic y_xor,
  output logic y_mux
);
  assign y_and = a & b;
  assign y_or = a | b;
  assign y_nand = ~(a & b);
  assign y_xor = a ^ b;
  assign y_mux = s ? a : b;
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
    let out_path = dir.join("basic-mapped.v");
    session
        .write_hdl_file(Some(out_path.clone()), &[], false)
        .unwrap();
    let mapped = std::fs::read_to_string(out_path).unwrap();
    let area = session.report_area().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    for output in ["y_and", "y_or", "y_nand", "y_xor", "y_mux"] {
        assert!(mapped.contains(&format!("({output})")), "{mapped}");
    }
    assert!(area.contains("Number of macros/black boxes: 0"));
    let total = area
        .lines()
        .find_map(|line| line.strip_prefix("Total cell area:"))
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap();
    assert!(total <= 14.5 + 1e-9, "{area}");
}

#[test]
fn library_source_names_include_exact_path_file_and_stem() {
    let names = library_source_names("/pdk/stdcells/slow.lib");

    assert!(names.contains("/pdk/stdcells/slow.lib"));
    assert!(names.contains("slow.lib"));
    assert!(names.contains("slow"));
    assert!(!names.contains("slow.db"));
}

#[test]
fn read_libs_resolves_inputs_through_search_path() {
    let dir = temp_dir("read-lib-search-path");
    let path = dir.join("demo.lib");
    std::fs::write(
        &path,
        r"
library (demo) {
  cell (INVX1) {
    area : 1.0;
    pin (A) { direction : input; }
    pin (Y) { direction : output; }
  }
}
",
    )
    .unwrap();

    let mut session = Session::new();
    session.set_lib_search_path(vec![PathBuf::from(dir.display().to_string())]);
    let message = session.read_libs(&[PathBuf::from("demo.lib")]).unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    assert_eq!(session.liberty_library_count(), 1);
    assert!(message.contains("1 Liberty libraries"));
}

#[test]
fn hierarchy_resolution_searches_design_memory_before_loaded_libraries() {
    let session = Session::new();

    assert_eq!(
        session.resolution_library_selection().selectors(),
        [opto_library::LibrarySelector::DesignMemory]
    );
}

#[test]
fn session_library_selection_is_stable_first_match_wins() {
    let mut session = Session::new();
    session
        .process
        .libraries
        .append(vec![
            opto_library::parse_liberty(
                "library(first) { cell(SHARED) { area : 1; } }",
                "first.lib",
            )
            .unwrap(),
            opto_library::parse_liberty(
                "library(second) { cell(SHARED) { area : 2; } }",
                "second.lib",
            )
            .unwrap(),
        ])
        .unwrap();

    let first = session.synthesis_options().unwrap().target_cells;
    assert_eq!(first.len(), 1);
    assert_eq!(first.get(0).unwrap().area(), Some(1.0));

    let second = session.synthesis_options().unwrap().target_cells;
    assert_eq!(second.len(), 1);
    assert_eq!(second.get(0).unwrap().area(), Some(1.0));

    let plan = session.active_link_plan().unwrap();
    assert_eq!(plan.providers().len(), 3);
}

#[test]
fn design_definition_before_loaded_library_shadows_the_cell() {
    let mut session = Session::new();
    session
        .process
        .libraries
        .append(vec![
            opto_library::parse_liberty(
                "library(cells) { cell(child) { area : 1; } }",
                "cells.lib",
            )
            .unwrap(),
        ])
        .unwrap();
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![
                    empty_rtl_module("child"),
                    rtl_module_with_instance("top", "child"),
                ],
                top: Some("top".to_string()),
                diagnostics: Vec::new(),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();

    let graph = session.definition_graph("test").unwrap();
    let opto_db::LinkBinding::Design {
        provider,
        definition,
    } = graph.instance(graph.root(), 0).unwrap().binding()
    else {
        panic!("*-before-library must bind the in-memory design");
    };
    assert_eq!(
        graph.provider(provider).kind(),
        opto_db::LinkProviderKind::Definitions
    );
    assert_eq!(graph.definition_name(definition), "child");
    assert_eq!(
        session
            .collect_design_modules("test", &["top".to_string()], true)
            .unwrap(),
        ["top", "child"]
    );
}

#[test]
fn repeated_library_cell_bindings_are_first_wins() {
    let mut session = Session::new();
    session
        .process
        .libraries
        .append(vec![
            opto_library::parse_liberty(
                "library(first) { cell(SHARED) { area : 1; } }",
                "first.lib",
            )
            .unwrap(),
            opto_library::parse_liberty(
                "library(second) { cell(SHARED) { area : 2; } }",
                "second.lib",
            )
            .unwrap(),
        ])
        .unwrap();
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![rtl_module_with_instance("top", "SHARED")],
                top: Some("top".to_string()),
                diagnostics: Vec::new(),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();

    let graph = session.definition_graph("test").unwrap();
    let opto_db::LinkBinding::External { provider } =
        graph.instance(graph.root(), 0).unwrap().binding()
    else {
        panic!("library cell must bind externally");
    };
    assert_eq!(graph.provider(provider).label(), "first (first.lib)");
}
