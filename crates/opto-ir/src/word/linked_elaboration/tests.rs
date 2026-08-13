// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn bits(width: u32) -> WordType {
    WordType::bits(width).unwrap()
}

fn leaf() -> WordModule {
    let mut module = WordModule::new("leaf");
    let a = module
        .add_port("a", PortDirection::Input, bits(2), SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bits(2), SourceSpan::default())
        .unwrap();
    let a = module
        .read_signal(module.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(y).unwrap().signal),
            a,
            SourceSpan::default(),
        )
        .unwrap();
    module
}

#[test]
fn recursively_elaborates_linked_design_and_preserves_library_leaves() {
    let leaf = leaf();
    let mut middle = WordModule::new("middle");
    let a = middle
        .add_port("a", PortDirection::Input, bits(2), SourceSpan::default())
        .unwrap();
    let y = middle
        .add_port("y", PortDirection::Output, bits(2), SourceSpan::default())
        .unwrap();
    let a = middle
        .read_signal(middle.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y = middle
        .read_signal(middle.port(y).unwrap().signal, SourceSpan::default())
        .unwrap();
    middle
        .add_instance(
            "u_leaf",
            "leaf",
            vec![
                ("a".to_string(), a, SourceSpan::default()),
                ("y".to_string(), y, SourceSpan::default()),
            ],
            SourceSpan::default(),
        )
        .unwrap();
    middle
        .add_instance("u_lib", "BUF", Vec::new(), SourceSpan::default())
        .unwrap();

    let mut top = WordModule::new("top");
    let a = top
        .add_port("a", PortDirection::Input, bits(2), SourceSpan::default())
        .unwrap();
    let y = top
        .add_port("y", PortDirection::Output, bits(2), SourceSpan::default())
        .unwrap();
    let a = top
        .read_signal(top.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y = top
        .read_signal(top.port(y).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_middle",
        "middle",
        vec![
            ("a".to_string(), a, SourceSpan::default()),
            ("y".to_string(), y, SourceSpan::default()),
        ],
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &middle, &leaf]).unwrap();
    assert_eq!(flat.ports().len(), 2);
    assert_eq!(flat.instances().len(), 1);
    assert_eq!(flat.name_str(flat.instances()[0].name), "u_middle/u_lib");
    assert_eq!(flat.name_str(flat.instances()[0].module), "BUF");
    assert!(flat.signal_id("u_middle/u_leaf/a").is_some());
    assert!(flat.signal_id("u_middle/u_leaf/y").is_some());
    assert_eq!(flat.connects().len(), 5);
}

#[test]
fn preserves_explicit_black_boxes_as_hierarchy_leaves() {
    let mut blackbox = WordModule::new("macro");
    blackbox.set_definition_kind(DefinitionKind::BlackBox);
    blackbox
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    blackbox
        .add_port("y", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();

    let mut top = WordModule::new("top");
    let a = top
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    let y = top
        .add_port("y", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();
    let a = top
        .read_signal(top.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y = top
        .read_signal(top.port(y).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_macro",
        "macro",
        vec![
            ("a".to_string(), a, SourceSpan::default()),
            ("y".to_string(), y, SourceSpan::default()),
        ],
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &blackbox]).unwrap();

    assert_eq!(flat.instances().len(), 1);
    assert_eq!(flat.name_str(flat.instances()[0].module), "macro");
    assert!(flat.signal_id("u_macro/y").is_none());
}

#[test]
fn elaborates_empty_synthesizable_definitions_instead_of_guessing_black_boxes() {
    let mut empty = WordModule::new("empty");
    empty
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();

    let mut top = WordModule::new("top");
    let a = top
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    let a = top
        .read_signal(top.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_empty",
        "empty",
        vec![("a".to_string(), a, SourceSpan::default())],
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &empty]).unwrap();

    assert!(flat.instances().is_empty());
    assert!(flat.signal_id("u_empty/a").is_some());
}

#[test]
fn unconnected_inlined_input_becomes_a_care_free_ssa_value() {
    let mut child = WordModule::new("child");
    let a = child
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    let y = child
        .add_port("y", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();
    let a = child
        .read_signal(child.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    child
        .connect(
            LValue::signal(child.port(y).unwrap().signal),
            a,
            SourceSpan::default(),
        )
        .unwrap();

    let mut top = WordModule::new("top");
    let y = top
        .add_port("y", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();
    let y = top
        .read_signal(top.port(y).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_child",
        "child",
        vec![("y".to_string(), y, SourceSpan::default())],
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &child]).unwrap();
    let input = flat.signal_id("u_child/a").unwrap();
    let driver = flat
        .connects()
        .iter()
        .find(|connect| connect.target.signal == input)
        .unwrap();

    assert!(matches!(
        &flat.value(driver.value).unwrap().kind,
        ValueKind::Constant(value) if value.bit_lsb(0) == Some(BitVal::X)
    ));
}

#[test]
fn preserves_hierarchy_selected_by_typed_directives() {
    let mut child = leaf();
    child
        .set_synthesis_directive(
            AnnotationTarget::Module,
            SynthesisDirectiveKind::Ungroup,
            false,
            SourceSpan::construct("keep_hierarchy"),
        )
        .unwrap();

    let mut top = WordModule::new("top");
    let a = top
        .add_port("a", PortDirection::Input, bits(2), SourceSpan::default())
        .unwrap();
    let y = top
        .add_port("y", PortDirection::Output, bits(2), SourceSpan::default())
        .unwrap();
    let a = top
        .read_signal(top.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y = top
        .read_signal(top.port(y).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_leaf",
        "leaf",
        vec![
            ("a".to_string(), a, SourceSpan::default()),
            ("y".to_string(), y, SourceSpan::default()),
        ],
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &child]).unwrap();
    let instance = flat.instance_id("u_leaf").unwrap();
    assert_eq!(flat.instances().len(), 1);
    assert!(flat.signal_id("u_leaf/a").is_none());
    assert_eq!(
        flat.synthesis_directive(
            AnnotationTarget::Instance(instance),
            SynthesisDirectiveKind::Ungroup,
        ),
        Some(false)
    );
}

#[test]
fn remaps_annotations_for_inlined_objects_and_external_instances() {
    let mut child = WordModule::new("child");
    let child_port = child
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    child
        .add_annotation(
            AnnotationTarget::Port(child_port),
            "port_tag",
            AnnotationValueSpec::String("child".to_string()),
            SourceSpan::default(),
        )
        .unwrap();

    let mut top = WordModule::new("top");
    let a = top
        .add_port("a", PortDirection::Input, bits(1), SourceSpan::default())
        .unwrap();
    let a_value = top
        .read_signal(top.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    top.add_instance(
        "u_child",
        "child",
        vec![("a".to_string(), a_value, SourceSpan::default())],
        SourceSpan::default(),
    )
    .unwrap();
    let external = top
        .add_instance("u_external", "LIB", Vec::new(), SourceSpan::default())
        .unwrap();
    top.add_annotation(
        AnnotationTarget::Instance(external),
        "instance_tag",
        AnnotationValueSpec::Integer {
            bits: crate::ConstBits::from_bin_str("1").unwrap(),
            signed: false,
        },
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &child]).unwrap();
    let child_signal = flat.signal_id("u_child/a").unwrap();
    let external = flat.instance_id("u_external").unwrap();

    assert!(flat.annotations().iter().any(|annotation| {
        annotation.target == AnnotationTarget::Signal(child_signal)
            && flat.name_str(annotation.name) == "port_tag"
    }));
    assert!(flat.annotations().iter().any(|annotation| {
        annotation.target == AnnotationTarget::Instance(external)
            && flat.name_str(annotation.name) == "instance_tag"
    }));
}

#[test]
fn output_concatenation_is_connected_in_lsb_order() {
    let leaf = leaf();
    let mut top = WordModule::new("top");
    let a = top
        .add_port("a", PortDirection::Input, bits(2), SourceSpan::default())
        .unwrap();
    let y0 = top
        .add_port("y0", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();
    let y1 = top
        .add_port("y1", PortDirection::Output, bits(1), SourceSpan::default())
        .unwrap();
    let a = top
        .read_signal(top.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y0 = top
        .read_signal(top.port(y0).unwrap().signal, SourceSpan::default())
        .unwrap();
    let y1 = top
        .read_signal(top.port(y1).unwrap().signal, SourceSpan::default())
        .unwrap();
    let output = top.concat(vec![y1, y0], SourceSpan::default()).unwrap();
    top.add_instance(
        "u_leaf",
        "leaf",
        vec![
            ("a".to_string(), a, SourceSpan::default()),
            ("y".to_string(), output, SourceSpan::default()),
        ],
        SourceSpan::default(),
    )
    .unwrap();

    let flat = elaborate_linked_root(&top, [&top, &leaf]).unwrap();
    let output_signals = [flat.signal_id("y0").unwrap(), flat.signal_id("y1").unwrap()];
    let output_connects = flat
        .connects()
        .iter()
        .filter(|connect| output_signals.contains(&connect.target.signal))
        .collect::<Vec<_>>();
    assert_eq!(output_connects.len(), 2);
    assert_eq!(output_connects[0].target.signal, output_signals[0]);
    assert_eq!(output_connects[1].target.signal, output_signals[1]);
}
