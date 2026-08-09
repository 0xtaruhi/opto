// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_ir::word::{
    AnnotationTarget, AnnotationValueSpec, ArrayKind, BinaryOp, DefinitionKind, IndexRange,
    LogicStateKind, PortDirection, SourceSpan, SynthesisDirectiveKind, TypeLayoutSpec, UnaryOp,
    WordModule, WordType,
};

fn rtl(module: WordModule) -> RtlModule {
    RtlModule::structural(module).unwrap()
}

fn logic_module(invert_left: bool) -> WordModule {
    let mut module = WordModule::new("top");
    let bit = WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bit, SourceSpan::default())
        .unwrap();
    let mut left = module
        .read_signal(module.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let right = module
        .read_signal(module.port(b).unwrap().signal, SourceSpan::default())
        .unwrap();
    if invert_left {
        left = module
            .unary(UnaryOp::BitNot, left, SourceSpan::default())
            .unwrap();
    }
    let output = module
        .binary(BinaryOp::BitAnd, left, right, SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(y).unwrap().signal),
            output,
            SourceSpan::default(),
        )
        .unwrap();
    module
}

fn two_cone_module(invert_left: bool) -> WordModule {
    let mut module = WordModule::new("top");
    let bit = WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let a = module
        .add_port("a", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let b = module
        .add_port("b", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let y = module
        .add_port("y", PortDirection::Output, bit, SourceSpan::default())
        .unwrap();
    let z = module
        .add_port("z", PortDirection::Output, bit, SourceSpan::default())
        .unwrap();
    let a = module
        .read_signal(module.port(a).unwrap().signal, SourceSpan::default())
        .unwrap();
    let b = module
        .read_signal(module.port(b).unwrap().signal, SourceSpan::default())
        .unwrap();
    let selected = if invert_left {
        module
            .unary(UnaryOp::BitNot, a, SourceSpan::default())
            .unwrap()
    } else {
        a
    };
    let first = module
        .binary(BinaryOp::BitAnd, selected, b, SourceSpan::default())
        .unwrap();
    let second = module
        .binary(BinaryOp::BitXor, a, b, SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(y).unwrap().signal),
            first,
            SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(z).unwrap().signal),
            second,
            SourceSpan::default(),
        )
        .unwrap();
    module
}

fn interface_module(width: u32) -> WordModule {
    let mut module = WordModule::new("unit");
    let ty = WordType::new(width, false, LogicStateKind::FourState).unwrap();
    module
        .add_port("value", PortDirection::Input, ty, SourceSpan::default())
        .unwrap();
    module
}

fn procedural_module(assign_right: bool) -> RtlModule {
    let mut module = WordModule::new("top");
    let bit = WordType::new(1, false, LogicStateKind::FourState).unwrap();
    let left = module
        .add_port("a", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let right = module
        .add_port("b", PortDirection::Input, bit, SourceSpan::default())
        .unwrap();
    let output = module
        .add_port("y", PortDirection::Output, bit, SourceSpan::default())
        .unwrap();
    let selected = if assign_right { right } else { left };
    let value = module
        .read_signal(module.port(selected).unwrap().signal, SourceSpan::default())
        .unwrap();
    let mut procedures = proc::ProcBuilder::new();
    let procedure = procedures
        .add_combinational_procedure(proc::ProcedureKind::Combinational, SourceSpan::default())
        .unwrap();
    let entry = procedures
        .add_block(procedure, SourceSpan::default())
        .unwrap();
    procedures
        .assign(
            entry,
            proc::AssignmentMode::Blocking,
            proc::ProcTarget::signal(module.port(output).unwrap().signal),
            value,
            SourceSpan::default(),
        )
        .unwrap();
    procedures
        .terminate_return(entry, SourceSpan::default())
        .unwrap();
    RtlModule::new(module, procedures.seal().unwrap()).unwrap()
}

#[test]
fn identical_source_has_an_empty_dirty_cone() {
    let module = rtl(logic_module(false));
    let previous = SourceSnapshot::capture(&module, crate::SynthesisEffort::Medium);
    let current = SourceSnapshot::capture(&module, crate::SynthesisEffort::Medium);

    let changes = current.changes_from(Some(&previous));

    assert_eq!(changes.changed_values, 0);
    assert_eq!(changes.changed_operations, 0);
    assert_eq!(changes.changed_boundaries, 0);
}

#[test]
fn checkpoint_snapshot_requires_the_module_boundary() {
    let snapshot = SourceSnapshot {
        effort: crate::SynthesisEffort::Medium,
        semantic_fingerprint: SourceFingerprint([0; 32]),
        value_fingerprints: Box::new([]),
        operation_fingerprints: Box::new([]),
        boundary_fingerprints: Box::new([]),
        region_fingerprints: Box::new([]),
    };

    assert!(
        snapshot
            .validate_checkpoint()
            .unwrap_err()
            .to_string()
            .contains("module boundary")
    );
}

#[test]
fn snapshots_from_different_synthesis_representations_do_not_share_a_dirty_cone() {
    let module = rtl(logic_module(false));
    let previous = SourceSnapshot::capture(&module, crate::SynthesisEffort::Medium);
    let current = SourceSnapshot::capture(&module, crate::SynthesisEffort::High);

    let changes = current.changes_from(Some(&previous));

    assert_eq!(changes.changed_values, changes.values);
    assert_eq!(changes.changed_operations, changes.operations);
    assert_eq!(changes.changed_boundaries, changes.boundaries);
}

#[test]
fn definition_semantics_and_annotations_change_source_fingerprints() {
    let original = interface_module(1);
    let mut annotated = original.clone();
    annotated
        .add_annotation(
            AnnotationTarget::Module,
            "vendor_hint",
            AnnotationValueSpec::String("balanced".to_string()),
            SourceSpan::default(),
        )
        .unwrap();
    let mut black_box = original.clone();
    black_box.set_definition_kind(DefinitionKind::BlackBox);
    let mut directed = original.clone();
    directed
        .set_synthesis_directive(
            AnnotationTarget::Module,
            SynthesisDirectiveKind::DontTouch,
            true,
            SourceSpan::default(),
        )
        .unwrap();

    let original = SourceFingerprint::capture(&rtl(original));
    assert_ne!(original, SourceFingerprint::capture(&rtl(annotated)));
    assert_ne!(original, SourceFingerprint::capture(&rtl(black_box)));
    assert_ne!(original, SourceFingerprint::capture(&rtl(directed)));
}

#[test]
fn changed_leaf_marks_its_structural_fanout_dirty() {
    let previous =
        SourceSnapshot::capture(&rtl(logic_module(false)), crate::SynthesisEffort::Medium);
    let current = SourceSnapshot::capture(&rtl(logic_module(true)), crate::SynthesisEffort::Medium);

    let changes = current.changes_from(Some(&previous));

    assert!(changes.changed_values > 0);
    assert!(changes.changed_values < changes.values);
    assert!(changes.changed_operations > 0);
    assert!(changes.changed_boundaries > 0);
}

#[test]
fn semantic_regions_survive_unrelated_arena_insertions() {
    let previous =
        SourceSnapshot::capture(&rtl(two_cone_module(false)), crate::SynthesisEffort::Medium);
    let current =
        SourceSnapshot::capture(&rtl(two_cone_module(true)), crate::SynthesisEffort::Medium);

    let changes = current.changes_from(Some(&previous));

    assert_eq!(changes.regions, 3);
    assert_eq!(changes.reused_regions, 1);
    assert_eq!(changes.rebuilt_regions, 2);
}

#[test]
fn semantic_fingerprint_tracks_logic_content() {
    let module = rtl(logic_module(false));
    let original = SourceFingerprint::capture(&module);
    let repeated = SourceFingerprint::capture(&rtl(logic_module(false)));
    let changed = SourceFingerprint::capture(&rtl(logic_module(true)));

    assert_eq!(original, repeated);
    assert_eq!(
        original,
        SourceSnapshot::capture(&module, crate::SynthesisEffort::Medium).semantic_fingerprint()
    );
    assert_ne!(original, changed);
}

#[test]
fn hierarchy_fingerprint_tracks_stable_stream_and_occurrences() {
    let mut leaf = logic_module(false);
    leaf.rename("leaf").unwrap();
    let leaf = rtl(leaf);
    let mut top = WordModule::new("top");
    top.add_instance("u_leaf", "leaf", Vec::new(), SourceSpan::default())
        .unwrap();
    let top = rtl(top);

    let original = SourceFingerprint::capture_hierarchy("top", [(&leaf, 1), (&top, 1)]);
    let repeated = SourceFingerprint::capture_hierarchy("top", [(&leaf, 1), (&top, 1)]);
    let repeated_leaf = SourceFingerprint::capture_hierarchy("top", [(&leaf, 2), (&top, 1)]);

    assert_eq!(original, repeated);
    assert_ne!(original, repeated_leaf);
}

#[test]
fn hierarchy_fingerprint_tracks_child_body_and_topology() {
    let mut original_leaf = logic_module(false);
    original_leaf.rename("leaf").unwrap();
    let original_leaf = rtl(original_leaf);
    let mut changed_leaf = logic_module(true);
    changed_leaf.rename("leaf").unwrap();
    let changed_leaf = rtl(changed_leaf);
    let mut original_top = WordModule::new("top");
    original_top
        .add_instance("u_leaf", "leaf", Vec::new(), SourceSpan::default())
        .unwrap();
    let original_top = rtl(original_top);
    let mut changed_top = WordModule::new("top");
    changed_top
        .add_instance("renamed", "leaf", Vec::new(), SourceSpan::default())
        .unwrap();
    let changed_top = rtl(changed_top);

    let original =
        SourceFingerprint::capture_hierarchy("top", [(&original_leaf, 1), (&original_top, 1)]);
    assert_ne!(
        original,
        SourceFingerprint::capture_hierarchy("top", [(&changed_leaf, 1), (&original_top, 1)],)
    );
    assert_ne!(
        original,
        SourceFingerprint::capture_hierarchy("top", [(&original_leaf, 1), (&changed_top, 1)],)
    );
}

#[test]
fn semantic_fingerprint_tracks_procedural_cfg_content() {
    let left = procedural_module(false);
    let repeated = procedural_module(false);
    let right = procedural_module(true);

    assert_eq!(
        SourceFingerprint::capture(&left),
        SourceFingerprint::capture(&repeated)
    );
    assert_ne!(
        SourceFingerprint::capture(&left),
        SourceFingerprint::capture(&right)
    );
}

#[test]
fn semantic_fingerprint_tracks_module_identity_and_type_layout() {
    let original = logic_module(false);
    let mut renamed = original.clone();
    renamed.rename("renamed").unwrap();
    assert_ne!(
        SourceFingerprint::capture(&rtl(original.clone())),
        SourceFingerprint::capture(&rtl(renamed))
    );

    let input = original.ports()[0].signal;
    let mut packed = original.clone();
    packed
        .set_signal_type_layout(
            input,
            &TypeLayoutSpec::Array {
                kind: ArrayKind::Packed,
                range: IndexRange { left: 0, right: 0 },
                element: Box::new(TypeLayoutSpec::Scalar),
            },
        )
        .unwrap();
    let mut unpacked = original;
    unpacked
        .set_signal_type_layout(
            input,
            &TypeLayoutSpec::Array {
                kind: ArrayKind::Unpacked,
                range: IndexRange { left: 0, right: 0 },
                element: Box::new(TypeLayoutSpec::Scalar),
            },
        )
        .unwrap();
    assert_ne!(
        SourceFingerprint::capture(&rtl(packed)),
        SourceFingerprint::capture(&rtl(unpacked))
    );
}

#[test]
fn interface_fingerprint_ignores_body_but_tracks_port_contract() {
    assert_eq!(
        InterfaceFingerprint::capture(&rtl(logic_module(false))),
        InterfaceFingerprint::capture(&rtl(logic_module(true)))
    );
    assert_ne!(
        InterfaceFingerprint::capture(&rtl(interface_module(1))),
        InterfaceFingerprint::capture(&rtl(interface_module(2)))
    );

    let mut packed = interface_module(1);
    let signal = packed.ports()[0].signal;
    packed
        .set_signal_type_layout(
            signal,
            &TypeLayoutSpec::Array {
                kind: ArrayKind::Packed,
                range: IndexRange { left: 0, right: 0 },
                element: Box::new(TypeLayoutSpec::Scalar),
            },
        )
        .unwrap();
    let mut unpacked = interface_module(1);
    unpacked
        .set_signal_type_layout(
            signal,
            &TypeLayoutSpec::Array {
                kind: ArrayKind::Unpacked,
                range: IndexRange { left: 0, right: 0 },
                element: Box::new(TypeLayoutSpec::Scalar),
            },
        )
        .unwrap();
    assert_ne!(
        InterfaceFingerprint::capture(&rtl(packed)),
        InterfaceFingerprint::capture(&rtl(unpacked))
    );
}
