// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

mod multiplier;

fn bitblast_area(module: &mut word::WordModule) -> Result<(), crate::SynthError> {
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(module)?;
    let mut provenance = crate::artifact::provenance::ProvenanceBuilder::new(module, &plan)?;
    bitblast_module_with_plan(module, &plan, &mut provenance)
}

fn select_recipe(plan: &mut crate::planning::operator::ArchitectureDecisions, recipe: &str) {
    let operator = plan.operators()[0].id();
    let candidate = plan
        .candidates(operator)
        .iter()
        .find(|candidate| plan.candidate_recipe_name(candidate.id()) == Some(recipe))
        .unwrap()
        .id();
    plan.select_candidate(candidate).unwrap();
}

mod arithmetic;
mod basic_multiply;
mod division;
mod dynamic;
mod operations;

#[test]
fn axm_eliminates_care_free_operands_without_creating_logic() {
    let mut module = word::WordModule::new("care_free_axm_operand");
    let input = add_input(&mut module, "input", 1);
    let input = read_port(&mut module, input);
    let dont_care = module
        .constant(
            ConstBits::from_bits(vec![BitVal::X]).unwrap(),
            word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let mut backend = AxmBackend::default();

    let input_bit = backend.import_word(&module, input);
    let dont_care_bit = backend.import_word(&module, dont_care);
    let (result, generated) = backend
        .emit_binary(
            &mut module,
            word::BinaryOp::BitXor,
            input_bit,
            dont_care_bit,
            &word::SourceSpan::default(),
        )
        .unwrap();
    let (network, inputs) = backend.finish();

    assert_eq!(result, input_bit);
    assert_eq!(generated, None);
    assert_eq!(inputs.as_ref(), &[input]);
    assert_eq!(network.node_count(), 2);
}

#[test]
fn regional_boolean_lowering_builds_axm_without_scalar_boolean_word_ops() {
    let mut module = word::WordModule::new("regional_axm");
    let a = add_input(&mut module, "a", 8);
    let b = add_input(&mut module, "b", 8);
    let a = read_port(&mut module, a);
    let b = read_port(&mut module, b);
    let result = module
        .binary(word::BinaryOp::BitXor, a, b, word::SourceSpan::default())
        .unwrap();
    let scalar = module
        .extract(a, 0, 1, word::SourceSpan::default())
        .unwrap();
    let scalar_cast = module
        .cast(
            word::CastKind::SignExtend,
            scalar,
            word::WordType::new(1, true, word::LogicStateKind::TwoState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let shifted = module
        .binary(
            word::BinaryOp::Ashr,
            scalar_cast,
            b,
            word::SourceSpan::default(),
        )
        .unwrap();
    add_output(&mut module, "y", 8, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_private_region(
        &module,
        &[shifted],
        implementation_providers().into(),
    )
    .unwrap();
    let operators = crate::DurableOperatorArena::capture(&module, &plan, &[], |_| {
        Err(crate::SynthError::invariant(
            "unexpected arithmetic operator",
        ))
    })
    .unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::for_regional_candidate(&module);
    let original_operations = module.operations().len();
    let owner = crate::RegionRowId::from_index(0).unwrap();

    let lowered = lower_local_region_boolean(
        &mut module,
        LocalRegionBooleanRequest {
            plan: &plan,
            operators: &operators,
            provenance: &mut provenance,
            owner,
            boundary_inputs: &[a, b],
            roots: &[shifted],
            tracked_values: &[a, b, result, scalar_cast, shifted],
        },
    )
    .unwrap();

    assert_eq!(lowered.subject.inputs.len(), 16);
    assert_eq!(lowered.ownership.lowered_bits(result).unwrap().len(), 8);
    assert_eq!(lowered.ownership.lowered_bits(shifted).unwrap(), &[shifted]);
    assert!(lowered.subject.network.node_count() > lowered.subject.inputs.len());
    assert!(
        module.operations()[original_operations..]
            .iter()
            .all(|operation| matches!(operation.kind, word::OpKind::Extract { .. }))
    );
}

#[test]
fn regional_boolean_lowering_resolves_dont_care_at_publication_boundary() {
    let mut module = word::WordModule::new("regional_dont_care_root");
    let root = module
        .constant(
            ConstBits::from_bits(vec![BitVal::X]).unwrap(),
            word::WordType::new(1, false, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let plan = crate::planning::operator::ArchitectureDecisions::for_private_region(
        &module,
        &[root],
        implementation_providers().into(),
    )
    .unwrap();
    let operators = crate::DurableOperatorArena::capture(&module, &plan, &[], |_| {
        Err(crate::SynthError::invariant(
            "unexpected arithmetic operator",
        ))
    })
    .unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::for_regional_candidate(&module);
    let owner = crate::RegionRowId::from_index(0).unwrap();

    let lowered = lower_local_region_boolean(
        &mut module,
        LocalRegionBooleanRequest {
            plan: &plan,
            operators: &operators,
            provenance: &mut provenance,
            owner,
            boundary_inputs: &[],
            roots: &[root],
            tracked_values: &[],
        },
    )
    .unwrap();

    let [published] = lowered.ownership.lowered_bits(root).unwrap() else {
        panic!("one-bit don't-care root must retain one physical binding");
    };
    let value = module.value(*published).unwrap();
    assert!(matches!(
        &value.kind,
        word::ValueKind::Constant(bits) if bits.bit_lsb(0) == Some(BitVal::Zero)
    ));
    assert_eq!(lowered.subject.value_nodes.len(), 1);
    assert_eq!(
        lowered.subject.value_nodes[0],
        (
            *published,
            crate::boolean::logic::network::LogicGraph::constant(false)
        )
    );
    assert_eq!(lowered.subject.dont_care_values.as_ref(), &[*published]);
}

#[test]
fn frozen_ownership_follows_static_signal_drivers() {
    let mut module = word::WordModule::new("owned_connectivity");
    let bit = word::WordType::bits(1).unwrap();
    let input = module
        .add_port(
            "a",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let input = module
        .read_signal(
            module.port(input).unwrap().signal,
            word::SourceSpan::default(),
        )
        .unwrap();
    let produced = module
        .unary(word::UnaryOp::BitNot, input, word::SourceSpan::default())
        .unwrap();
    let first = module
        .add_wire("first", bit, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(first),
            produced,
            word::SourceSpan::default(),
        )
        .unwrap();
    let first = module
        .read_signal(first, word::SourceSpan::default())
        .unwrap();
    let second = module
        .add_wire("second", bit, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(second),
            first,
            word::SourceSpan::default(),
        )
        .unwrap();
    let second = module
        .read_signal(second, word::SourceSpan::default())
        .unwrap();

    let owner = crate::RegionRowId::from_index(0).unwrap();
    let mut ownership = LoweredRegionOwnership::new(module.values().len());
    ownership.set(produced, owner).unwrap();
    ownership.infer_unowned(&module).unwrap();

    assert_eq!(ownership.owner(first), Some(owner));
    assert_eq!(ownership.owner(second), Some(owner));
    assert_eq!(ownership.owner(input), None);
}

#[test]
fn preserves_care_free_x_constants_during_bitblast() {
    let mut module = word::WordModule::new("top");
    let ty = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::located("x_constant.sv", Some(7), Some(11), "constant");
    let value = module
        .constant(ConstBits::from_bin_str("x").unwrap(), ty, source)
        .unwrap();
    let output = add_output(&mut module, "y", 1, value);

    bitblast_area(&mut module).unwrap();

    let connect = module
        .connects()
        .iter()
        .find(|connect| connect.target.signal == output)
        .unwrap();
    let word::ValueKind::Constant(bits) = &module.value(connect.value).unwrap().kind else {
        panic!("care-free X must remain a constant");
    };
    assert_eq!(bits.bit_lsb(0), Some(BitVal::X));
}

#[test]
fn rejects_tri_state_constants_during_bitblast() {
    let mut module = word::WordModule::new("top");
    let ty = word::WordType::bits(1).unwrap();
    let source = word::SourceSpan::located("z_constant.sv", Some(9), Some(13), "constant");
    let value = module
        .constant(ConstBits::from_bin_str("z").unwrap(), ty, source)
        .unwrap();
    add_output(&mut module, "y", 1, value);

    let error = bitblast_area(&mut module).unwrap_err();

    assert!(error.to_string().contains("tri-state constant"));
    assert!(error.to_string().contains("z_constant.sv"));
}

#[test]
fn rejects_unresolved_tri_state_driver_at_boolean_boundary() {
    let mut module = word::WordModule::new("unresolved_tri_state");
    let data = add_input(&mut module, "data", 1);
    let enable = add_input(&mut module, "enable", 1);
    let data = read_port(&mut module, data);
    let enable = read_port(&mut module, enable);
    let source = word::SourceSpan::located("tri_state.sv", Some(11), Some(7), "tri-state");
    let driver = module
        .tri_state(
            data,
            word::Enable {
                value: enable,
                active_high: true,
            },
            source,
        )
        .unwrap();
    add_output(&mut module, "y", 1, driver);

    let error = bitblast_area(&mut module).unwrap_err();

    assert!(error.to_string().contains("tri-state driver"));
    assert!(error.to_string().contains("physical tri-state lowering"));
    assert!(error.to_string().contains("tri_state.sv"));
}

#[test]
fn regional_shell_preserves_a_validated_physical_tri_state_connect() {
    let mut module = word::WordModule::new("regional_tri_state_shell");
    let data_port = add_input(&mut module, "data", 1);
    let enable_port = add_input(&mut module, "enable", 1);
    let data = read_port(&mut module, data_port);
    let enable = read_port(&mut module, enable_port);
    let pad_port = module
        .add_port(
            "pad",
            word::PortDirection::Inout,
            word::WordType::bits(1).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let pad = module.port(pad_port).unwrap().signal;
    module
        .set_signal_resolution(pad, word::SignalResolution::TriState)
        .unwrap();
    let driver = module
        .tri_state(
            data,
            word::Enable {
                value: enable,
                active_high: true,
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(pad),
            driver,
            word::SourceSpan::default(),
        )
        .unwrap();
    let plan = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&module);
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();

    bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &[None],
        &[],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    assert_eq!(module.connects().len(), 1);
    assert_ne!(module.connects()[0].value, driver);
    assert_eq!(module.connects()[0].target.signal, pad);
    let lowered = module.value(module.connects()[0].value).unwrap();
    let word::ValueKind::Operation(lowered) = lowered.kind else {
        panic!("lowered tri-state driver is not an operation");
    };
    assert!(matches!(
        module.operation(lowered).unwrap().kind,
        word::OpKind::TriState {
            data: lowered_data,
            enable: word::Enable {
                value: lowered_enable,
                active_high: true,
            },
        } if lowered_data == data && lowered_enable == enable
    ));
}

#[test]
fn regional_shell_cuts_owned_combinational_cones_without_rewriting_source_values() {
    let mut module = word::WordModule::new("regional_shell");
    let input = add_input(&mut module, "a", 4);
    let input = read_port(&mut module, input);
    let result = module
        .unary(word::UnaryOp::BitNot, input, word::SourceSpan::default())
        .unwrap();
    add_output(&mut module, "y", 4, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    let region = crate::RegionRowId::from_index(0).unwrap();

    let ownership = bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &[Some(region)],
        &[],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    assert_eq!(ownership.lowered_bits(result).unwrap().len(), 4);
    assert!(matches!(
        module.value(result).unwrap().kind,
        word::ValueKind::Operation(_)
    ));
    module.compact_netlist().unwrap();
    module.validate().unwrap();
    assert!(module.operations().is_empty());
    assert_eq!(module.connects().len(), 4);
    assert!(module.connects().iter().all(|connect| matches!(
        module.value(connect.value).unwrap().kind,
        word::ValueKind::Signal(_)
    )));
}

#[test]
fn regional_shell_freezes_full_domain_constant_bits() {
    let mut module = word::WordModule::new("regional_constants");
    let input = add_input(&mut module, "a", 4);
    let input = read_port(&mut module, input);
    let wide = module
        .cast(
            word::CastKind::ZeroExtend,
            input,
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let result = module
        .binary(word::BinaryOp::Add, wide, wide, word::SourceSpan::default())
        .unwrap();
    add_output(&mut module, "y", 8, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    let region = crate::RegionRowId::from_index(0).unwrap();
    let owners = vec![Some(region); module.operations().len()];

    let ownership = bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &owners,
        &[],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    let bits = ownership.lowered_bits(result).unwrap();
    assert_eq!(bits.len(), 8);
    for &bit in &bits[5..] {
        let word::ValueKind::Constant(value) = &module.value(bit).unwrap().kind else {
            panic!("proven upper result bit is not frozen as a constant");
        };
        assert_eq!(value.bit_lsb(0), Some(BitVal::Zero));
    }
}

#[test]
fn regional_shell_rejects_a_producer_claim_for_a_full_domain_constant() {
    let mut module = word::WordModule::new("regional_constant_publication_claim");
    let input = module
        .constant(
            ConstBits::from_bits(vec![BitVal::Zero]).unwrap(),
            word::WordType::bits(1).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let result = module
        .unary(word::UnaryOp::BitNot, input, word::SourceSpan::default())
        .unwrap();
    add_output(&mut module, "y", 1, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    let region = crate::RegionRowId::from_index(0).unwrap();

    let error = bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &[Some(region)],
        &[],
        &[RegionalPublicationBit {
            target: result,
            bit: 0,
            producer: region,
        }],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap_err();

    assert!(error.to_string().contains("claims full-domain constant"));
}

#[test]
fn regional_shell_freezes_unowned_proven_constant_operations() {
    let mut module = word::WordModule::new("unowned_proven_constant");
    let zero = module
        .constant(
            ConstBits::from_bits(vec![BitVal::Zero; 8]).unwrap(),
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let offset = add_input(&mut module, "offset", 3);
    let offset = read_port(&mut module, offset);
    let result = module
        .dynamic_extract(zero, offset, 1, word::SourceSpan::default())
        .unwrap();
    let shell = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&module);
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &shell).unwrap();

    let ownership = bitblast_module_with_regions(
        &mut module,
        &shell,
        &mut provenance,
        &[None],
        &[result],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    let [lowered] = ownership.lowered_bits(result).unwrap() else {
        panic!("scalar result must retain one lowered value");
    };
    let word::ValueKind::Constant(bits) = &module.value(*lowered).unwrap().kind else {
        panic!("proven unowned result was not frozen as a constant");
    };
    assert_eq!(bits.bit_lsb(0), Some(BitVal::Zero));
}

#[test]
fn regional_shell_drops_unowned_arithmetic_on_a_dead_connect() {
    let mut module = word::WordModule::new("unowned_arithmetic");
    let data = add_input(&mut module, "data", 8);
    let data = read_port(&mut module, data);
    let one = module
        .constant(
            ConstBits::from_bin_str("00000001").unwrap(),
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let result = module
        .binary(word::BinaryOp::Sub, data, one, word::SourceSpan::default())
        .unwrap();
    let dead = module
        .add_wire(
            "dead",
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(dead),
            result,
            word::SourceSpan::default(),
        )
        .unwrap();
    let shell = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&module);
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &shell).unwrap();

    bitblast_module_with_regions(
        &mut module,
        &shell,
        &mut provenance,
        &[None],
        &[],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    assert!(module.connects().is_empty());
}

#[test]
fn regional_shell_keeps_connects_needed_by_an_explicit_lowering_root() {
    let mut module = word::WordModule::new("required_internal_value");
    let data = add_input(&mut module, "data", 8);
    let data = read_port(&mut module, data);
    let inverted = module
        .unary(word::UnaryOp::BitNot, data, word::SourceSpan::default())
        .unwrap();
    let internal = module
        .add_wire(
            "required",
            word::WordType::bits(8).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(internal),
            inverted,
            word::SourceSpan::default(),
        )
        .unwrap();
    let required = module
        .read_signal(internal, word::SourceSpan::default())
        .unwrap();
    let shell = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&module);
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &shell).unwrap();
    let region = crate::RegionRowId::from_index(0).unwrap();

    bitblast_module_with_regions(
        &mut module,
        &shell,
        &mut provenance,
        &[Some(region)],
        &[required],
        &[],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap();

    assert_eq!(module.connects().len(), 8);
    assert!(
        module
            .connects()
            .iter()
            .all(|connect| connect.target.signal == internal)
    );
}

#[test]
fn regional_shell_freezes_constants_reached_through_connects() {
    let mut module = word::WordModule::new("regional_connected_constant");
    let bit = word::WordType::bits(1).unwrap();
    let zero = module
        .constant(
            ConstBits::from_bits(vec![BitVal::Zero]).unwrap(),
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let wire = module
        .add_wire("constant_wire", bit, word::SourceSpan::default())
        .unwrap();
    module
        .connect(
            word::LValue::signal(wire),
            zero,
            word::SourceSpan::default(),
        )
        .unwrap();
    let wire = module
        .read_signal(wire, word::SourceSpan::default())
        .unwrap();
    let result = module
        .unary(word::UnaryOp::BitNot, wire, word::SourceSpan::default())
        .unwrap();
    add_output(&mut module, "y", 1, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    let region = crate::RegionRowId::from_index(0).unwrap();

    let error = bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &[Some(region)],
        &[],
        &[RegionalPublicationBit {
            target: result,
            bit: 0,
            producer: region,
        }],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap_err();

    assert!(error.to_string().contains("claims full-domain constant"));
}

#[test]
fn regional_shell_rejects_a_claim_from_the_wrong_producer() {
    let mut module = word::WordModule::new("regional_wrong_producer");
    let input = add_input(&mut module, "a", 1);
    let input = read_port(&mut module, input);
    let result = module
        .unary(word::UnaryOp::BitNot, input, word::SourceSpan::default())
        .unwrap();
    add_output(&mut module, "y", 1, result);
    let plan = crate::planning::operator::ArchitectureDecisions::for_module(&module).unwrap();
    let mut provenance =
        crate::artifact::provenance::ProvenanceBuilder::new(&module, &plan).unwrap();
    let owner = crate::RegionRowId::from_index(0).unwrap();
    let claimant = crate::RegionRowId::from_index(1).unwrap();

    let error = bitblast_module_with_regions(
        &mut module,
        &plan,
        &mut provenance,
        &[Some(owner)],
        &[],
        &[RegionalPublicationBit {
            target: result,
            bit: 0,
            producer: claimant,
        }],
        GlobalBitblastScope::RegionalShell,
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("but the source operation belongs to")
    );
}

#[test]
fn bit_splitting_publishes_deterministic_async_reset_values() {
    let mut module = word::WordModule::new("deterministic_async_reset");
    let data_port = add_input(&mut module, "d", 2);
    let clock_port = add_input(&mut module, "clock", 1);
    let reset_port = add_input(&mut module, "reset", 1);
    let data = read_port(&mut module, data_port);
    let clock = read_port(&mut module, clock_port);
    let reset = read_port(&mut module, reset_port);
    let reset_value = module
        .constant(
            ConstBits::from_bits(vec![BitVal::X, BitVal::X]).unwrap(),
            word::WordType::new(2, false, word::LogicStateKind::FourState).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let result = module
        .register(
            word::RegisterOp {
                name: None,
                d: data,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: vec![word::Reset {
                    kind: word::ResetKind::Async,
                    value: reset,
                    active_high: true,
                    reset_value,
                }],
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    add_output(&mut module, "q", 2, result);

    bitblast_area(&mut module).unwrap();

    let reset_values = module.operations().iter().filter_map(|operation| {
        let word::OpKind::Register(register) = &operation.kind else {
            return None;
        };
        if module.value(operation.result).unwrap().ty.width() != 1 {
            return None;
        }
        register.resets.first().map(|reset| reset.reset_value)
    });
    let reset_values = reset_values.collect::<Vec<_>>();
    assert_eq!(reset_values.len(), 2);
    for reset_value in reset_values {
        let word::ValueKind::Constant(bits) = &module.value(reset_value).unwrap().kind else {
            panic!("split reset value is not constant");
        };
        assert_eq!(bits.bit_lsb(0), Some(BitVal::Zero));
    }
}

fn wrapping_add(left: u64, right: u64) -> u64 {
    left.wrapping_add(right)
}

fn wrapping_sub(left: u64, right: u64) -> u64 {
    left.wrapping_sub(right)
}

fn signed_value(value: u64, width: u32) -> i64 {
    let shift = u64::BITS - width;
    (value << shift).cast_signed() >> shift
}

fn binary_module(
    op: word::BinaryOp,
    left_width: u32,
    right_width: u32,
    signed: bool,
) -> (
    word::WordModule,
    word::SignalId,
    word::SignalId,
    word::SignalId,
) {
    let mut module = word::WordModule::new("top");
    let state = word::LogicStateKind::FourState;
    let left_ty = word::WordType::new(left_width, signed, state).unwrap();
    let right_ty = word::WordType::new(right_width, signed, state).unwrap();
    let a = module
        .add_port(
            "a",
            word::PortDirection::Input,
            left_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let b = module
        .add_port(
            "b",
            word::PortDirection::Input,
            right_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let left = module
        .read_signal(module.port(a).unwrap().signal, word::SourceSpan::default())
        .unwrap();
    let right = module
        .read_signal(module.port(b).unwrap().signal, word::SourceSpan::default())
        .unwrap();
    let result = module
        .binary(op, left, right, word::SourceSpan::default())
        .unwrap();
    let result_ty = module.value(result).unwrap().ty;
    let y = module
        .add_port(
            "y",
            word::PortDirection::Output,
            result_ty,
            word::SourceSpan::default(),
        )
        .unwrap();
    let y_signal = module.port(y).unwrap().signal;
    module
        .connect(
            word::LValue::signal(y_signal),
            result,
            word::SourceSpan::default(),
        )
        .unwrap();
    let a_signal = module_signal(&module, a);
    let b_signal = module_signal(&module, b);
    (module, a_signal, b_signal, y_signal)
}

fn add_input(module: &mut word::WordModule, name: &str, width: u32) -> word::PortId {
    module
        .add_port(
            name,
            word::PortDirection::Input,
            word::WordType::bits(width).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap()
}

fn add_output(
    module: &mut word::WordModule,
    name: &str,
    width: u32,
    value: word::ValueId,
) -> word::SignalId {
    let port = module
        .add_port(
            name,
            word::PortDirection::Output,
            word::WordType::bits(width).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap();
    let signal = module_signal(module, port);
    module
        .connect(
            word::LValue::signal(signal),
            value,
            word::SourceSpan::default(),
        )
        .unwrap();
    signal
}

fn read_port(module: &mut word::WordModule, port: word::PortId) -> word::ValueId {
    module
        .read_signal(module_signal(module, port), word::SourceSpan::default())
        .unwrap()
}

fn module_signal(module: &word::WordModule, port: word::PortId) -> word::SignalId {
    module.port(port).unwrap().signal
}

fn evaluate_output(
    module: &word::WordModule,
    output: word::SignalId,
    inputs: &[(word::SignalId, u64)],
) -> u64 {
    let mut result = 0u64;
    let mut memo = vec![None; module.values().len()];
    for connect in module
        .connects()
        .iter()
        .filter(|connect| connect.target.signal == output)
    {
        let bit = connect.target.range.map_or(0, |range| {
            assert_eq!(range.msb, range.lsb);
            range.lsb
        });
        if evaluate_value(module, connect.value, inputs, &mut memo) {
            result |= 1u64 << bit;
        }
    }
    result
}

fn evaluate_value(
    module: &word::WordModule,
    value_id: word::ValueId,
    inputs: &[(word::SignalId, u64)],
    memo: &mut [Option<bool>],
) -> bool {
    if let Some(value) = memo[value_id.index()] {
        return value;
    }
    let value = module.value(value_id).unwrap();
    assert_eq!(value.ty.width(), 1);
    let result = match &value.kind {
        word::ValueKind::Signal(reference) => {
            assert_eq!(reference.width(), 1);
            let value = inputs
                .iter()
                .find(|(signal, _)| *signal == reference.signal)
                .unwrap()
                .1;
            ((value >> reference.lsb) & 1) != 0
        }
        word::ValueKind::Constant(bits) => match bits.bit_lsb(0).unwrap() {
            BitVal::Zero => false,
            BitVal::One => true,
            BitVal::X | BitVal::Z => panic!("test evaluator received unknown constant"),
        },
        word::ValueKind::Operation(operation_id) => {
            let operation = module.operation(*operation_id).unwrap();
            match &operation.kind {
                word::OpKind::Unary { op, arg } => match op {
                    word::UnaryOp::LogicalNot | word::UnaryOp::BitNot => {
                        !evaluate_value(module, *arg, inputs, memo)
                    }
                    word::UnaryOp::ReductionAnd
                    | word::UnaryOp::ReductionOr
                    | word::UnaryOp::ReductionXor => evaluate_value(module, *arg, inputs, memo),
                },
                word::OpKind::Binary { op, left, right } => {
                    let left = evaluate_value(module, *left, inputs, memo);
                    let right = evaluate_value(module, *right, inputs, memo);
                    match op {
                        word::BinaryOp::BitAnd | word::BinaryOp::LogicalAnd => left & right,
                        word::BinaryOp::BitOr | word::BinaryOp::LogicalOr => left | right,
                        word::BinaryOp::BitXor | word::BinaryOp::Ne => left ^ right,
                        word::BinaryOp::Eq => left == right,
                        _ => panic!("non-scalarized binary op {op:?}"),
                    }
                }
                word::OpKind::Mux {
                    cond,
                    then_value,
                    else_value,
                } => {
                    if evaluate_value(module, *cond, inputs, memo) {
                        evaluate_value(module, *then_value, inputs, memo)
                    } else {
                        evaluate_value(module, *else_value, inputs, memo)
                    }
                }
                word::OpKind::Cast { value, .. } => evaluate_value(module, *value, inputs, memo),
                kind => panic!("non-scalarized operation {kind:?}"),
            }
        }
    };
    memo[value_id.index()] = Some(result);
    result
}
