// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn substrate_test_cell() -> opto_library::TargetCell {
    let pin = |name: &str, direction: opto_library::TargetPinDirection| opto_library::TargetPin {
        name: name.to_string(),
        direction,
        function: None,
        three_state: None,
        capacitance: None,
        rise_capacitance: None,
        fall_capacitance: None,
        receiver_capacitance: None,
        fanout_load: None,
        next_state_type: None,
        clock_gate_role: None,
        timing_arcs: Vec::new(),
    };
    opto_library::TargetCell {
        name: "ICG".to_string(),
        area: Some(1.0),
        dont_use: false,
        usage: opto_library::TargetCellUsage::INTEGRATED_CLOCK_GATING,
        pins: vec![
            pin("CLK", opto_library::TargetPinDirection::Input),
            pin("GCLK", opto_library::TargetPinDirection::Output),
        ],
        sequential: Vec::new(),
        clock_gate: None,
        memory: None,
    }
}

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
fn artifact_pins_do_not_escape_into_word_provenance() {
    let value = word::ValueId::from_index(0).unwrap();
    let pin = SequentialPinKey {
        state: opto_ir::design::CellId::from_bytes([1; 32]),
        role: SequentialPinRole::StateOutput,
        bit: 0,
    };
    let binding = RegionPlanBinding {
        inputs: vec![RegionPlanValueBinding::ArtifactPinBit {
            pin: pin.into(),
            value,
        }]
        .into(),
        outputs: vec![RegionPlanValueBinding::ArtifactPinBit {
            pin: pin.into(),
            value,
        }]
        .into(),
    };
    let lowering = crate::boolean::bitblast::LoweredRegionBinding::new(1);

    assert!(binding.resolve_inputs(&lowering).unwrap().is_empty());
    assert!(binding.resolve_outputs(&lowering).unwrap().is_empty());
}

#[test]
fn memory_logic_ownership_is_always_a_cover_output() {
    let combinational = word::ValueId::from_index(0).unwrap();
    let memory = word::MemoryId::from_index(0).unwrap();
    let mut outputs = BindingMap::new();

    bind_artifact_output(
        &mut outputs,
        combinational,
        RegionPlanValueBinding::MemoryLogicBit {
            memory,
            ordinal: 0,
            bit: 0,
        },
    );

    assert!(outputs.contains_key(&combinational));
}

#[test]
fn source_publication_drives_a_generated_target_sink() {
    let ty = word::WordType::bits(1).unwrap();
    let span = word::SourceSpan::default();
    let mut source = word::WordModule::new("source");
    let source_clock_port = source
        .add_port("clk", word::PortDirection::Input, ty, span.clone())
        .unwrap();
    let source_clock = source
        .read_signal(source.port(source_clock_port).unwrap().signal, span.clone())
        .unwrap();
    let mut local = word::WordModule::new("local");
    let local_clock_port = local
        .add_port("clk", word::PortDirection::Input, ty, span.clone())
        .unwrap();
    let local_clock = local
        .read_signal(local.port(local_clock_port).unwrap().signal, span.clone())
        .unwrap();
    let gated_port = local
        .add_port("gclk", word::PortDirection::Output, ty, span.clone())
        .unwrap();
    let gated_clock = local
        .read_signal(local.port(gated_port).unwrap().signal, span.clone())
        .unwrap();
    local
        .add_instance(
            "gate",
            "ICG",
            vec![
                ("CLK".to_string(), local_clock, span.clone()),
                ("GCLK".to_string(), gated_clock, span),
            ],
            word::SourceSpan::default(),
        )
        .unwrap();
    let target_cells: opto_library::TargetCellSet = vec![substrate_test_cell()].into();
    let mut region_binding =
        crate::boolean::bitblast::LoweredRegionBinding::new(local.values().len());
    region_binding.bind_identity_for_test(local_clock);
    region_binding.bind_identity_for_test(gated_clock);
    let candidate = build_candidate_binding(
        CandidateBindingDomain {
            source_module: &source,
            local_module: &local,
            source_to_local: &std::collections::BTreeMap::from([(source_clock, local_clock)]),
            boundary_bindings: &[],
            owned_memory_logic: &[],
            memory_states: &[],
            source_cells: &std::collections::BTreeMap::new(),
            sequential_operations: &[],
            root_bindings: &[(source_clock, local.port(gated_port).unwrap().signal)],
            region_binding: &region_binding,
            region: crate::RegionAnchorId::from_bytes_for_test([2; 32]),
            target_cells: &target_cells,
            substrate_instances: &["gate".into()],
        },
        &[gated_clock],
        [std::slice::from_ref(&local_clock)],
    )
    .unwrap();

    assert_eq!(candidate.output_widths.as_ref(), &[1]);
    assert!(matches!(
        candidate.binding.outputs[0],
        RegionPlanValueBinding::SourceBit {
            value,
            bit: 0
        } if value == source_clock
    ));
    assert!(matches!(
        candidate.binding.inputs[0],
        RegionPlanValueBinding::ArtifactPinBit { .. }
    ));
    assert_eq!(candidate.substrate.len(), 1);
    assert_eq!(candidate.substrate[0].connections.len(), 2);
    assert!(matches!(
        candidate.substrate[0].connections[0].endpoint,
        RegionalEndpoint::SourceBit {
            value,
            bit: 0
        } if value == source_clock
    ));
    assert!(matches!(
        candidate.substrate[0].connections[1].endpoint,
        RegionalEndpoint::Pin(RegionalPinKey::Substrate(_))
    ));
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
    let region_binding = crate::boolean::bitblast::LoweredRegionBinding::new(local.values().len());

    let candidate = build_candidate_binding(
        CandidateBindingDomain {
            source_module: &source,
            local_module: &local,
            source_to_local: &source_to_local,
            boundary_bindings: &[(source_input, local_input)],
            owned_memory_logic: &[],
            memory_states: &[],
            source_cells: &std::collections::BTreeMap::new(),
            sequential_operations: &[],
            root_bindings: &[(source_root, root_signal)],
            region_binding: &region_binding,
            region: crate::RegionAnchorId::from_bytes_for_test([1; 32]),
            target_cells: &opto_library::TargetCellSet::default(),
            substrate_instances: &[],
        },
        &[local_input],
        [std::slice::from_ref(&local_input)],
    )
    .unwrap();

    assert_eq!(candidate.output_widths.as_ref(), &[1]);
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
fn only_frozen_boundary_identity_becomes_a_cover_input() {
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
        (port_value, local_input),
        (implementation_input, local_input),
        (observation, local_input),
        (source_root, local_input),
    ]);
    let region_binding = crate::boolean::bitblast::LoweredRegionBinding::new(local.values().len());

    let candidate = build_candidate_binding(
        CandidateBindingDomain {
            source_module: &source,
            local_module: &local,
            source_to_local: &source_to_local,
            boundary_bindings: &[(port_value, local_input)],
            owned_memory_logic: &[],
            memory_states: &[],
            source_cells: &std::collections::BTreeMap::new(),
            sequential_operations: &[],
            root_bindings: &[(source_root, root_signal)],
            region_binding: &region_binding,
            region: crate::RegionAnchorId::from_bytes_for_test([1; 32]),
            target_cells: &opto_library::TargetCellSet::default(),
            substrate_instances: &[],
        },
        &[local_input],
        [std::slice::from_ref(&local_input)],
    )
    .unwrap();

    assert_eq!(candidate.output_widths.as_ref(), &[1]);
    assert_eq!(
        candidate.binding.inputs.as_ref(),
        &[RegionPlanValueBinding::SourceBit {
            value: port_value,
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
fn root_publication_replaces_the_private_memory_state_handle() {
    let ty = word::WordType::bits(1).unwrap();
    let span = word::SourceSpan::default();
    let mut source = word::WordModule::new("source");
    let source_root = source
        .constant(
            opto_ir::ConstBits::from_bin_str("0").unwrap(),
            ty,
            span.clone(),
        )
        .unwrap();
    let mut local = word::WordModule::new("local");
    let root_port = local
        .add_port("root", word::PortDirection::Output, ty, span.clone())
        .unwrap();
    let root_signal = local.port(root_port).unwrap().signal;
    let local_root = local
        .constant(opto_ir::ConstBits::from_bin_str("0").unwrap(), ty, span)
        .unwrap();
    let memory = word::MemoryId::from_index(0).unwrap();
    let memory_binding = RegionPlanValueBinding::MemoryStateBit {
        memory,
        ordinal: 0,
        bit: 0,
    };
    let mut outputs = BindingMap::from([(local_root, vec![memory_binding])]);

    bind_root_outputs(
        &source,
        &local,
        &std::collections::BTreeMap::from([(source_root, local_root)]),
        &[(source_root, root_signal)],
        &crate::boolean::bitblast::LoweredRegionBinding::new(local.values().len()),
        &mut outputs,
    )
    .unwrap();

    assert_eq!(
        outputs.get(&local_root).unwrap(),
        &[RegionPlanValueBinding::SourceBit {
            value: source_root,
            bit: 0,
        }]
    );
}
