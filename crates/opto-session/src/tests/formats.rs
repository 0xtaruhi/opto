// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn write_hdl_emits_requested_hierarchy() {
    let mut child = WordModule::new("child");
    child
        .add_port(
            "a",
            PortDirection::Input,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();

    let mut top = WordModule::new("top");
    let top_a = top
        .add_port(
            "a",
            PortDirection::Input,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let top_a_value = top
        .read_signal(top.port(top_a).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_child",
        "child",
        vec![("a".to_string(), top_a_value, SourceSpan::default())],
        SourceSpan::default(),
    )
    .unwrap();

    let mut session = Session::new();
    session
        .apply_db_update(DbUpdate {
            modules: vec![rtl(top), rtl(child)],
            top: Some("top".to_string()),
            diagnostics: Vec::new(),
        })
        .unwrap();

    let path = temp_file("hier.v");
    let message = session.write_hdl_file(&path, true).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();

    assert!(message.contains("Wrote HDL file"));
    assert!(text.contains("module top"));
    assert!(text.contains("module child"));
    assert_eq!(session.current_design(), Some("top"));
    assert_eq!(session.state.designs.keys().count(), 2);
}
