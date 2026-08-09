// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn add_test_port(design: &mut DesignIndex, name: &str, direction: Direction, width: u32) {
    let name = design.intern_name(name).unwrap();
    design.ports.push(Port {
        name,
        direction,
        width,
    });
}

fn add_test_cell(
    design: &mut DesignIndex,
    name: &str,
    reference: &str,
    connections: &[(&str, &str)],
) {
    let name = design.intern_name(name).unwrap();
    let reference = design.intern_name(reference).unwrap();
    let mut cell = Cell::new(name, reference);
    for &(port, signal) in connections {
        let port = design.intern_name(port).unwrap();
        let signal = design.intern_name(signal).unwrap();
        cell = cell.with_connection(port, signal);
    }
    design.add_cell(cell);
}

#[test]
fn collection_handles_resolve_inside_session() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    add_test_port(&mut design, "clk", Direction::Input, 1);
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();

    let collection = session.get_ports("*").unwrap();
    let handle = session.collection_handles(collection).join(" ");

    assert_eq!(session.collection_len(&handle).unwrap(), 1);
    assert_eq!(
        session.collection_first_object_name(&handle).unwrap(),
        Some("clk".to_string())
    );
    let members = session.collection_members(&handle).unwrap();
    assert_eq!(members.len(), 1);
    let member_handle = session.collection_member_handle(members[0]);
    assert_eq!(
        session.collection_object_names(&member_handle).unwrap(),
        vec!["clk".to_string()]
    );
    assert!(session.collection_len("_obj1_p999").is_err());
}

#[test]
fn object_name_visiting_is_read_only_and_uses_current_design() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    add_test_port(&mut design, "clk", Direction::Input, 1);
    add_test_cell(&mut design, "u0", "BUF", &[("A", "clk")]);
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();

    let revision = session.revision();
    let mut ports = Vec::new();
    let mut cells = Vec::new();
    let mut pins = Vec::new();
    session.visit_object_names(ObjectClass::Port, |name| ports.push(name.to_string()));
    session.visit_object_names(ObjectClass::Cell, |name| cells.push(name.to_string()));
    session.visit_object_names(ObjectClass::Pin, |name| pins.push(name.to_string()));

    assert_eq!(ports, ["clk"]);
    assert_eq!(cells, ["u0"]);
    assert_eq!(pins, ["u0/A"]);
    assert_eq!(session.revision(), revision);
}

#[test]
fn fresh_design_install_invalidates_same_named_collection_objects() {
    let mut session = Session::new();
    let mut original = DesignIndex::new("top");
    add_test_port(&mut original, "clk", Direction::Input, 1);
    install_test_design(&mut session, original);
    session.set_current_design("top").unwrap();
    let old_ports = session.get_ports("*").unwrap();
    let old_handle = session.collection_handles(old_ports).join(" ");
    let old_object = session.collection_members(&old_handle).unwrap()[0];
    let old_member = session.collection_member_handle(old_object);

    let mut replacement = DesignIndex::new("top");
    add_test_port(&mut replacement, "clk", Direction::Input, 1);
    install_test_design(&mut session, replacement);
    let new_ports = session.get_ports("*").unwrap();
    let new_handle = session.collection_handles(new_ports).join(" ");
    let new_object = session.collection_members(&new_handle).unwrap()[0];
    let new_member = session.collection_member_handle(new_object);

    assert!(session.collection_len(&old_handle).is_err());
    assert!(session.collection_len(&old_member).is_err());
    assert_eq!(session.collection_len(&new_handle).unwrap(), 1);
    assert_eq!(session.collection_len(&new_member).unwrap(), 1);
    assert_ne!(old_object, new_object);
    assert_ne!(old_member, new_member);
}

#[test]
fn fresh_design_install_interns_internal_objects_on_demand() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    add_test_cell(&mut design, "u0", "BUF_X1", &[]);
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();
    let locator = ObjectLocator::Cell {
        design: "top".to_string(),
        name: "u0".to_string(),
    };

    assert!(session.state.objects.get(&locator).is_none());
    let cells = session.get_cells("u0").unwrap();
    assert_eq!(cells.len(), 1);
    assert!(session.state.objects.get(&locator).is_some());
}

#[test]
fn design_update_preserves_live_uids_and_removes_deleted_objects() {
    let mut session = Session::new();
    let mut original = DesignIndex::new("top");
    add_test_port(&mut original, "clk", Direction::Input, 1);
    add_test_cell(&mut original, "old_cell", "BUF_X1", &[]);
    install_test_design(&mut session, original);
    session.set_current_design("top").unwrap();
    let ports = session.get_ports("*").unwrap();
    let old_port_handle = session.collection_handles(ports).join(" ");
    let cells = session.get_cells("*").unwrap();
    let old_cell_handle = session.collection_handles(cells).join(" ");

    let mut updated = DesignIndex::new("top");
    add_test_port(&mut updated, "clk", Direction::Input, 1);
    add_test_cell(&mut updated, "new_cell", "BUF_X1", &[]);
    session.update_design_preserving_objects(updated).unwrap();
    let ports = session.get_ports("*").unwrap();
    let new_port_handle = session.collection_handles(ports).join(" ");

    assert_eq!(session.collection_len(&old_port_handle).unwrap(), 1);
    assert!(session.collection_len(&old_cell_handle).is_err());
    assert_eq!(old_port_handle, new_port_handle);
}

#[test]
fn design_rule_constraints_do_not_rebind_to_recreated_objects() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    add_test_port(&mut design, "a", Direction::Input, 1);
    session
        .install_design_fresh(empty_rtl_module("top"), RevisionId::INITIAL, design)
        .unwrap();
    session.state.current_design = Some("top".to_string());

    let object = session
        .resolve_design_rule_objects("set_max_fanout", &["a".to_string()])
        .unwrap()
        .pop()
        .unwrap();
    let old_uid = object.object_id().uid();
    session.set_max_fanout(4.0, &[object]).unwrap();

    session
        .update_design_preserving_objects(DesignIndex::new("top"))
        .unwrap();
    assert!(
        session
            .state
            .timing
            .design_rule_constraints(opto_timing::DesignRuleKind::MaxFanout)
            .is_empty()
    );

    let mut recreated = DesignIndex::new("top");
    add_test_port(&mut recreated, "a", Direction::Input, 1);
    session.update_design_preserving_objects(recreated).unwrap();
    let new_object = session
        .resolve_design_rule_objects("set_max_fanout", &["a".to_string()])
        .unwrap()
        .pop()
        .unwrap();
    assert_ne!(new_object.object_id().uid(), old_uid);
}

#[test]
fn current_design_can_be_returned_as_collection_handle() {
    let mut session = Session::new();
    install_test_design(&mut session, DesignIndex::new("top"));
    session.set_current_design("top").unwrap();

    let handle = session.store_current_design_collection().unwrap();

    assert_eq!(
        session.collection_object_names(&handle).unwrap(),
        vec!["top".to_string()]
    );
}

#[test]
fn collection_attributes_come_from_db_objects() {
    let mut session = Session::new();
    let mut design = DesignIndex::new("top");
    add_test_port(&mut design, "clk", Direction::Input, 1);
    add_test_port(&mut design, "y", Direction::Output, 4);
    install_test_design(&mut session, design);
    session.set_current_design("top").unwrap();

    let handle = {
        let collection = session.get_ports("*").unwrap();
        session.collection_handles(collection).join(" ")
    };

    assert_eq!(
        session
            .collection_attribute_values(&handle, "direction")
            .unwrap(),
        vec!["in".to_string(), "out".to_string()]
    );
    assert_eq!(
        session
            .collection_attribute_values(&handle, "bit_width")
            .unwrap(),
        vec!["1".to_string(), "4".to_string()]
    );
}

#[test]
fn pin_collections_use_instance_pin_full_names() {
    let mut session = Session::new();
    let mut child = DesignIndex::new("child");
    add_test_port(&mut child, "a", Direction::Input, 1);
    add_test_port(&mut child, "y", Direction::Output, 1);
    let mut top = DesignIndex::new("top");
    add_test_cell(&mut top, "u_child", "child", &[("a", "a"), ("y", "y")]);
    install_test_design(&mut session, child);
    install_test_design(&mut session, top);
    session.set_current_design("top").unwrap();

    let pins = {
        let collection = session.get_pins("u_child/*").unwrap();
        session.collection_handles(collection).join(" ")
    };

    assert_eq!(
        session.collection_object_names(&pins).unwrap(),
        vec!["u_child/a".to_string(), "u_child/y".to_string()]
    );
    assert_eq!(
        session
            .collection_attribute_values(&pins, "direction")
            .unwrap(),
        vec!["in".to_string(), "out".to_string()]
    );
    assert_eq!(
        session.collection_attribute_values(&pins, "name").unwrap(),
        vec!["a".to_string(), "y".to_string()]
    );
}

#[test]
fn object_navigation_preserves_database_relationships() {
    let mut session = Session::new();
    let mut child = DesignIndex::new("child");
    add_test_port(&mut child, "a", Direction::Input, 1);
    add_test_port(&mut child, "y", Direction::Output, 1);

    let mut top = DesignIndex::new("top");
    add_test_port(&mut top, "clk", Direction::Input, 1);
    add_test_port(&mut top, "a", Direction::Input, 1);
    add_test_port(&mut top, "y", Direction::Output, 1);
    add_test_cell(&mut top, "u_child", "child", &[("a", "a"), ("y", "y")]);

    install_test_design(&mut session, child);
    install_test_design(&mut session, top);
    session.set_current_design("top").unwrap();

    let cells = {
        let collection = session.get_cells("u_child").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let pins = {
        let collection = session.get_pins_of_objects(&cells, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let nets = {
        let collection = session.get_nets_of_objects(&pins, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let pins_of_nets = {
        let collection = session.get_pins_of_objects(&nets, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let all_nets = {
        let collection = session.get_nets("*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let a_ports = {
        let collection = session.get_ports("a").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let ports_of_ports = {
        let collection = session.get_ports_of_objects(&a_ports, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let ports_of_nets = {
        let collection = session.get_ports_of_objects(&all_nets, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let ports_of_cells = {
        let collection = session.get_ports_of_objects(&cells, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let cells_of_ports = {
        let collection = session.get_cells_of_objects(&a_ports, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let cells_of_pins = {
        let collection = session.get_cells_of_objects(&pins, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };
    let cells_of_cells = {
        let collection = session.get_cells_of_objects(&cells, "*").unwrap();
        session.collection_handles(collection).join(" ")
    };

    assert_eq!(
        session.collection_object_names(&pins).unwrap(),
        vec!["u_child/a".to_string(), "u_child/y".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&nets).unwrap(),
        vec!["a".to_string(), "y".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&pins_of_nets).unwrap(),
        vec!["u_child/a".to_string(), "u_child/y".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&all_nets).unwrap(),
        vec!["a".to_string(), "y".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&ports_of_ports).unwrap(),
        vec!["a".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&ports_of_nets).unwrap(),
        vec!["a".to_string(), "y".to_string()]
    );
    assert!(
        session
            .collection_object_names(&ports_of_cells)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        session.collection_object_names(&cells_of_ports).unwrap(),
        vec!["u_child".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&cells_of_pins).unwrap(),
        vec!["u_child".to_string()]
    );
    assert_eq!(
        session.collection_object_names(&cells_of_cells).unwrap(),
        vec!["u_child".to_string()]
    );
    assert_eq!(
        session
            .collection_attribute_values(&nets, "object_class")
            .unwrap(),
        vec!["net".to_string(), "net".to_string()]
    );
}

#[test]
fn frontend_update_builds_one_canonical_design_record() {
    let mut module = WordModule::new("top");
    module
        .add_port(
            "a",
            PortDirection::Input,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let y = module
        .add_port(
            "y",
            PortDirection::Output,
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let n = module
        .add_wire(
            "n",
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    let n_value = module.read_signal(n, SourceSpan::default()).unwrap();
    module
        .add_instance(
            "u_child",
            "child",
            vec![("a".to_string(), n_value, SourceSpan::default())],
            SourceSpan::default(),
        )
        .unwrap();
    let one = module
        .constant(
            ConstBits::from_bin_str("1").unwrap(),
            WordType::new(1, false, LogicStateKind::FourState).unwrap(),
            SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            opto_ir::word::LValue::signal(module.port(y).unwrap().signal),
            one,
            SourceSpan::default(),
        )
        .unwrap();

    let mut session = Session::new();
    let message = session
        .apply_db_update(
            DbUpdate {
                modules: vec![rtl(module)],
                top: Some("top".to_string()),
            },
            CurrentDesignPolicy::ElaboratedTop,
        )
        .unwrap();

    assert_eq!(message, "top");
    assert_eq!(session.current_design(), Some("top"));
    assert!(session.state.hdl_catalog.definitions.contains("top"));
    assert_eq!(session.elaborate("top").unwrap(), "1");
    assert_eq!(session.current_design(), Some("top"));
    assert!(session.state.designs.contains_key("top"));
    let design = session.current().unwrap();
    assert_eq!(design.port_count(), 2);
    assert!(design.net(0).unwrap().name.eq_str("n"));
    assert_eq!(design.cell(0).unwrap().reference, "child");
    assert!(design.used_signal_names().any(|name| name == "y"));
}
