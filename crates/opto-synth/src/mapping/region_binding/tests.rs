// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn epoch_snapshot_shares_immutable_binding_arenas() {
    let value = word::ValueId::from_index(0).unwrap();
    let binding = RegionPlanBinding {
        inputs: vec![RegionPlanValueBinding::Lowered(value)].into(),
        outputs: vec![RegionPlanValueBinding::Lowered(value)].into(),
    };

    let snapshot = binding.clone();

    assert!(Arc::ptr_eq(&binding.inputs, &snapshot.inputs));
    assert!(Arc::ptr_eq(&binding.outputs, &snapshot.outputs));
}

#[test]
fn collapsed_root_keeps_input_and_output_identities_separate() {
    let ty = word::WordType::bits(1).unwrap();
    let source_span = word::SourceSpan::default();
    let mut source = word::WordModule::new("source");
    let input_port = source
        .add_port("input", word::PortDirection::Input, ty, source_span.clone())
        .unwrap();
    let source_input = source
        .read_signal(source.port(input_port).unwrap().signal, source_span.clone())
        .unwrap();
    let source_root = source
        .unary(word::UnaryOp::BitNot, source_input, source_span.clone())
        .unwrap();

    let mut local = word::WordModule::new("local");
    let input_port = local
        .add_port("input", word::PortDirection::Input, ty, source_span.clone())
        .unwrap();
    let local_input = local
        .read_signal(local.port(input_port).unwrap().signal, source_span.clone())
        .unwrap();
    let root_port = local
        .add_port("root", word::PortDirection::Output, ty, source_span.clone())
        .unwrap();
    let root_signal = local.port(root_port).unwrap().signal;
    local
        .connect(word::LValue::signal(root_signal), local_input, source_span)
        .unwrap();
    let source_to_local =
        std::collections::BTreeMap::from([(source_input, local_input), (source_root, local_input)]);
    let ownership = crate::boolean::bitblast::LoweredRegionOwnership::new(local.values().len());

    let candidate = build_candidate_binding(
        CandidateBindingInputs {
            source_module: &source,
            local_module: &local,
            source_to_local: &source_to_local,
            boundary_bindings: &[],
            observations: &[],
            memory_values: &[],
            operation_sources: &[],
            root_bindings: &[(source_root, root_signal)],
            ownership: &ownership,
        },
        &[local_input],
        [std::slice::from_ref(&local_input)],
    )
    .unwrap();

    assert_eq!(
        candidate.binding.inputs.as_ref(),
        &[RegionPlanValueBinding::SourceBit {
            value: source_input,
            bit: 0,
        }]
    );
    assert_eq!(
        candidate.binding.outputs.as_ref(),
        &[RegionPlanValueBinding::SourceBit {
            value: source_root,
            bit: 0,
        }]
    );
}

#[test]
fn boundary_observation_never_becomes_a_cover_input_identity() {
    let ty = word::WordType::bits(1).unwrap();
    let source_span = word::SourceSpan::default();
    let mut source = word::WordModule::new("source");
    let port = source
        .add_port("input", word::PortDirection::Input, ty, source_span.clone())
        .unwrap();
    let port_value = source
        .read_signal(source.port(port).unwrap().signal, source_span.clone())
        .unwrap();
    let implementation_input = source
        .unary(word::UnaryOp::BitNot, port_value, source_span.clone())
        .unwrap();
    let boundary = source
        .add_wire("boundary", ty, source_span.clone())
        .unwrap();
    source
        .connect(
            word::LValue::signal(boundary),
            implementation_input,
            source_span.clone(),
        )
        .unwrap();
    let observation = source.read_signal(boundary, source_span.clone()).unwrap();
    let source_root = source
        .unary(
            word::UnaryOp::LogicalNot,
            implementation_input,
            source_span.clone(),
        )
        .unwrap();

    let mut local = word::WordModule::new("local");
    let input_port = local
        .add_port("input", word::PortDirection::Input, ty, source_span.clone())
        .unwrap();
    let local_input = local
        .read_signal(local.port(input_port).unwrap().signal, source_span.clone())
        .unwrap();
    let root_port = local
        .add_port("root", word::PortDirection::Output, ty, source_span.clone())
        .unwrap();
    let root_signal = local.port(root_port).unwrap().signal;
    local
        .connect(word::LValue::signal(root_signal), local_input, source_span)
        .unwrap();
    let source_to_local = std::collections::BTreeMap::from([
        (implementation_input, local_input),
        (observation, local_input),
        (source_root, local_input),
    ]);
    let ownership = crate::boolean::bitblast::LoweredRegionOwnership::new(local.values().len());

    let candidate = build_candidate_binding(
        CandidateBindingInputs {
            source_module: &source,
            local_module: &local,
            source_to_local: &source_to_local,
            boundary_bindings: &[],
            observations: &[observation],
            memory_values: &[],
            operation_sources: &[],
            root_bindings: &[(source_root, root_signal)],
            ownership: &ownership,
        },
        &[local_input],
        [std::slice::from_ref(&local_input)],
    )
    .unwrap();

    assert_eq!(
        candidate.binding.inputs.as_ref(),
        &[RegionPlanValueBinding::SourceBit {
            value: implementation_input,
            bit: 0,
        }]
    );
    assert_eq!(
        candidate.binding.outputs.as_ref(),
        &[RegionPlanValueBinding::SourceBit {
            value: source_root,
            bit: 0,
        }]
    );
}
