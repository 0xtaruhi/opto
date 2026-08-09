// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn read_hdl_stores_templates_until_explicit_elaboration() {
    let path = temp_file("read-hdl-deferred.sv");
    std::fs::write(
        &path,
        "package support; localparam int W = 2; endpackage\nmodule invalid_by_default #(parameter type T = logic) (output T y); assign y.member = 1'b0; endmodule\nmodule top(input logic a, output logic y); assign y = a; endmodule\n",
    )
    .unwrap();

    let mut session = Session::new();
    let initial_revision = session.revision();
    assert_eq!(
        session
            .ingest_verilog(std::slice::from_ref(&path), &FrontendOptions::default())
            .unwrap(),
        "1"
    );
    std::fs::remove_file(path).unwrap();

    assert_eq!(session.current_design(), None);
    assert!(session.state.designs.keys().next().is_none());
    assert!(session.state.hdl_catalog.definitions.contains("top"));
    assert!(session.state.hdl_catalog.packages.contains("support"));
    assert_eq!(session.revision(), initial_revision.next().unwrap());

    assert_eq!(session.elaborate("top").unwrap(), "1");
    assert_eq!(session.current_design(), Some("top"));
    assert!(session.state.designs.contains_key("top"));
}

#[test]
fn internal_import_selects_the_first_loaded_design() {
    let path = temp_file("first-imported-design.v");
    std::fs::write(
            &path,
            "module child(input a, output y); assign y = a; endmodule\nmodule top(input a, output y); child u_child(.a(a), .y(y)); endmodule\n",
        )
        .unwrap();

    let mut session = Session::new();
    let message = session
        .import_verilog(std::slice::from_ref(&path), &FrontendOptions::default())
        .unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(message, "child top");
    assert_eq!(session.current_design(), Some("child"));
    assert_eq!(session.state.designs.keys().count(), 2);
}

#[test]
fn synthesize_updates_implementation_and_object_index_for_always_comb_processes() {
    let mut word = WordModule::new("top");
    let a = word
        .add_port(
            "a",
            PortDirection::Input,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::stable("frontend-test/input-a"),
        )
        .unwrap();
    let y = word
        .add_port(
            "y",
            PortDirection::Output,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::stable("frontend-test/output-y"),
        )
        .unwrap();
    let a_value = word
        .read_signal(
            word.port(a).unwrap().signal,
            SourceSpan::stable("frontend-test/read-a"),
        )
        .unwrap();
    let mut procedures = opto_ir::proc::ProcBuilder::new();
    let procedure = procedures
        .add_combinational_procedure(
            opto_ir::proc::ProcedureKind::Combinational,
            SourceSpan::stable("frontend-test/always-comb"),
        )
        .unwrap();
    let block = procedures
        .add_block(
            procedure,
            SourceSpan::stable("frontend-test/always-comb/entry"),
        )
        .unwrap();
    procedures
        .assign(
            block,
            opto_ir::proc::AssignmentMode::Blocking,
            opto_ir::proc::ProcTarget::signal(word.port(y).unwrap().signal),
            a_value,
            SourceSpan::stable("frontend-test/always-comb/assignment"),
        )
        .unwrap();
    procedures
        .terminate_return(
            block,
            SourceSpan::stable("frontend-test/always-comb/return"),
        )
        .unwrap();
    let module = RtlModule::new(word, procedures.seal().unwrap()).unwrap();

    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .apply_db_update(
            DbUpdate {
                modules: vec![module],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();
    assert_eq!(
        session
            .state
            .designs
            .get("top")
            .unwrap()
            .source
            .procedures()
            .procedures()
            .len(),
        1
    );

    let message = session.synthesize().unwrap();
    assert_eq!(message, "1");
    let synthesized = session
        .state
        .designs
        .get("top")
        .unwrap()
        .synthesized
        .as_ref()
        .unwrap();
    assert_eq!(synthesized.mapped().cell_count(), 0);
    assert_eq!(synthesized.mapped().net_count(), 1);
    assert_eq!(
        session
            .state
            .designs
            .get("top")
            .unwrap()
            .source
            .procedures()
            .procedures()
            .len(),
        1
    );
    assert_eq!(session.synthesize().unwrap(), "1");

    let path = temp_file("always-comb-synthesized.v");
    session
        .write_hdl_file(Some(path.clone()), &[], false)
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(text.contains("assign y = a;"));
}

#[test]
fn same_clock_dual_port_memory_synthesizes_from_first_class_resources() {
    let path = temp_file("dual-port-memory.sv");
    std::fs::write(
        &path,
        "module top(input logic clk, we_a, we_b, input logic [1:0] address_a, address_b, \
         input logic [7:0] data_a, data_b, output logic [7:0] result); \
         logic [7:0] memory[4]; \
         always_ff @(posedge clk) if (we_a) memory[address_a] <= data_a; \
         always_ff @(posedge clk) if (we_b) memory[address_b] <= data_b; \
         assign result = memory[address_a]; endmodule\n",
    )
    .unwrap();
    let mut session = Session::new();
    install_test_mapping_library(&mut session);
    session
        .import_verilog(std::slice::from_ref(&path), &FrontendOptions::default())
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let source = &session.state.designs.get("top").unwrap().source;
    assert_eq!(source.word().memories().len(), 1);
    assert_eq!(source.procedures().procedures().len(), 2);

    assert_eq!(session.synthesize().unwrap(), "1");
    let record = session.state.designs.get("top").unwrap();
    assert_eq!(record.source.word().memories().len(), 1);
    assert!(record.synthesized.is_some());
}

#[test]
fn read_libs_stores_libraries() {
    let path = temp_file("session.lib");
    std::fs::write(
        &path,
        r"
library (demo) {
  cell (BUF) {
    area : 2.0;
    pin (A) { direction : input; }
    pin (Y) { direction : output; }
  }
}
",
    )
    .unwrap();

    let mut session = Session::new();
    let message = session.read_libs(std::slice::from_ref(&path)).unwrap();
    std::fs::remove_file(path).unwrap();

    assert_eq!(session.liberty_library_count(), 1);
    assert!(message.contains("1 Liberty libraries"));
    assert!(message.contains("1 cells"));
    assert!(message.contains("2 pins"));
}
