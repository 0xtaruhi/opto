// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable region-graph and boundary-identity contracts.
//!
//! These tests own partition atoms, deterministic region/port identities, and
//! hard state boundaries. They do not assert cover choice or post-map repair.

use super::*;
use opto_ir::word::{
    BinaryOp, Edge, LValue, MemoryReadPort, MemoryReadTiming, OpId, PortDirection, ReadDuringWrite,
    RegisterOp, SourceSpan, UnaryOp, ValueId, ValueKind, WordModule, WordType,
};
use std::collections::BTreeSet;
use std::num::NonZeroU32;

fn test_span() -> SourceSpan {
    SourceSpan::stable("test")
}

fn input(module: &mut WordModule, name: &str) -> ValueId {
    input_with_type(module, name, WordType::bits(1).unwrap())
}

fn input_with_type(module: &mut WordModule, name: &str, ty: WordType) -> ValueId {
    let port = module
        .add_port(name, PortDirection::Input, ty, test_span())
        .unwrap();
    module
        .read_signal(module.port(port).unwrap().signal, test_span())
        .unwrap()
}

fn output(module: &mut WordModule, name: &str, value: ValueId) {
    let port = module
        .add_port(
            name,
            PortDirection::Output,
            WordType::bits(1).unwrap(),
            test_span(),
        )
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(port).unwrap().signal),
            value,
            test_span(),
        )
        .unwrap();
}

fn operation(module: &WordModule, value: ValueId) -> OpId {
    match module.value(value).unwrap().kind {
        ValueKind::Operation(operation) => operation,
        _ => panic!("test value must be produced by an operation"),
    }
}

#[test]
fn physical_tri_state_shell_is_not_owned_by_a_boolean_region() {
    let mut module = WordModule::new("tri_state_boundary");
    let a = input(&mut module, "a");
    let e = input(&mut module, "e");
    let data = module.unary(UnaryOp::BitNot, a, test_span()).unwrap();
    let enable = module.unary(UnaryOp::LogicalNot, e, test_span()).unwrap();
    let pad_port = module
        .add_port(
            "pad",
            PortDirection::Inout,
            WordType::bits(1).unwrap(),
            test_span(),
        )
        .unwrap();
    let pad = module.port(pad_port).unwrap().signal;
    module
        .set_signal_resolution(pad, opto_ir::word::SignalResolution::TriState)
        .unwrap();
    let driver = module
        .tri_state(
            data,
            opto_ir::word::Enable {
                value: enable,
                active_high: true,
            },
            test_span(),
        )
        .unwrap();
    module
        .connect(LValue::signal(pad), driver, test_span())
        .unwrap();
    let observed = module.read_signal(pad, test_span()).unwrap();
    output(&mut module, "observed", observed);

    let reachable = super::partition::synthesis_reachable_operations(&module).unwrap();

    assert!(reachable[operation(&module, data).index()]);
    assert!(reachable[operation(&module, enable).index()]);
    assert!(!reachable[operation(&module, driver).index()]);
}

#[test]
fn deterministic_split_materializes_typed_cross_region_ports() {
    let mut module = WordModule::new("chain");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let c = input(&mut module, "c");
    let first = module.binary(BinaryOp::BitAnd, a, b, test_span()).unwrap();
    let second = module
        .binary(BinaryOp::BitXor, first, c, test_span())
        .unwrap();
    output(&mut module, "y", second);

    let graph =
        super::partition::build(&module, RegionPartitionPolicy::with_target_work(2)).unwrap();

    assert_eq!(graph.regions().len(), 2);
    let edge_count = graph
        .regions()
        .iter()
        .map(|region| {
            graph
                .successors(*region)
                .iter()
                .filter(|&&successor| successor != region.row())
                .count()
        })
        .sum::<usize>();
    assert_eq!(edge_count, 1);
    let internal = graph
        .regions()
        .iter()
        .flat_map(|region| graph.output_ports(*region))
        .filter_map(|&port| graph.port(port))
        .find(|port| port.peer().is_some())
        .unwrap();
    assert_eq!(internal.direction(), RegionPortDirection::Output);
    assert_eq!(internal.ty(), WordType::bits(1).unwrap());
}

#[test]
fn packed_crossings_freeze_each_bit_at_its_semantic_producer() {
    let mut module = WordModule::new("packed_bit_producers");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let low = module.unary(UnaryOp::BitNot, a, test_span()).unwrap();
    let high = module.unary(UnaryOp::BitNot, b, test_span()).unwrap();
    let pair = WordType::bits(2).unwrap();
    let bus = module.add_wire("bus", pair, test_span()).unwrap();
    module
        .connect(
            LValue::signal(bus).with_range(opto_ir::word::BitRange { msb: 0, lsb: 0 }),
            low,
            test_span(),
        )
        .unwrap();
    module
        .connect(
            LValue::signal(bus).with_range(opto_ir::word::BitRange { msb: 1, lsb: 1 }),
            high,
            test_span(),
        )
        .unwrap();
    let packed = module.read_signal(bus, test_span()).unwrap();
    let result = module.unary(UnaryOp::BitNot, packed, test_span()).unwrap();
    let out = module
        .add_port("y", PortDirection::Output, pair, test_span())
        .unwrap();
    module
        .connect(
            LValue::signal(module.port(out).unwrap().signal),
            result,
            test_span(),
        )
        .unwrap();

    let graph =
        super::partition::build(&module, RegionPartitionPolicy::with_target_work(1)).unwrap();
    let consumer = graph
        .operation_owner(operation(&module, result))
        .unwrap()
        .row();
    for producer in [low, high] {
        let owner = graph.operation_owner(operation(&module, producer)).unwrap();
        assert!(graph.bit_flows(owner).iter().any(|flow| {
            flow.value() == producer && flow.bit() == 0 && flow.consumer() == Some(consumer)
        }));
        assert!(
            graph
                .predecessors(graph.region(consumer).unwrap())
                .contains(&owner.row())
        );
    }
}

#[test]
fn repeated_reads_share_one_physical_boundary() {
    let mut module = WordModule::new("read_aliases");
    let first = input(&mut module, "a");
    let signal = match module.value(first).unwrap().kind {
        ValueKind::Signal(reference) => reference.signal,
        _ => panic!("input helper must return a signal read"),
    };
    let second = module.read_signal(signal, test_span()).unwrap();
    let result = module
        .binary(BinaryOp::BitXor, first, second, test_span())
        .unwrap();
    output(&mut module, "y", result);

    let graph = super::partition::build(&module, RegionPartitionPolicy::default()).unwrap();

    assert_eq!(graph.regions().len(), 1);
    assert_eq!(graph.input_ports(graph.regions()[0]).len(), 1);
}

#[test]
fn small_disconnected_output_cones_share_one_region() {
    let mut module = WordModule::new("small_siblings");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let c = input(&mut module, "c");
    let d = input(&mut module, "d");
    let left = module.binary(BinaryOp::BitAnd, a, b, test_span()).unwrap();
    let right = module.binary(BinaryOp::BitXor, c, d, test_span()).unwrap();
    output(&mut module, "y", left);
    output(&mut module, "z", right);

    let graph = super::partition::build(&module, RegionPartitionPolicy::default()).unwrap();

    assert_eq!(graph.regions().len(), 1);
    assert_eq!(graph.operations(graph.regions()[0]).len(), 2);
}

#[test]
fn fanout_edges_have_unique_pairwise_semantic_keys() {
    let mut module = WordModule::new("fanout_edges");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let c = input(&mut module, "c");
    let source = module.unary(UnaryOp::BitNot, a, test_span()).unwrap();
    let left = module
        .binary(BinaryOp::BitXor, source, b, test_span())
        .unwrap();
    let right = module
        .binary(BinaryOp::BitAnd, source, c, test_span())
        .unwrap();
    output(&mut module, "y", left);
    output(&mut module, "z", right);

    let graph =
        super::partition::build(&module, RegionPartitionPolicy::with_target_work(1)).unwrap();
    let source_row = graph
        .regions()
        .iter()
        .find(|region| {
            graph
                .operations(**region)
                .contains(&operation(&module, source))
        })
        .unwrap()
        .row();
    let ports = graph
        .output_ports(graph.region(source_row).unwrap())
        .iter()
        .filter_map(|&port| graph.port(port))
        .filter(|port| port.value() == source && port.peer().is_some())
        .collect::<Vec<_>>();

    assert_eq!(ports.len(), 2);
    assert_ne!(ports[0].semantic_key(), ports[1].semantic_key());
    for port in ports {
        let peer = port.peer().unwrap();
        let matching = graph
            .input_ports(graph.region(peer).unwrap())
            .iter()
            .filter_map(|&input| graph.port(input))
            .filter(|input| input.semantic_key() == port.semantic_key())
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].peer(), Some(source_row));
    }
}

#[test]
fn word_level_slice_cycle_is_one_atomic_architecture_region() {
    let mut module = WordModule::new("slice_cycle");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let pair = WordType::bits(2).unwrap();
    let x = module.add_wire("x", pair, test_span()).unwrap();
    let y = module.add_wire("y", pair, test_span()).unwrap();
    let y_low = module.read_signal_slice(y, 0, 1, test_span()).unwrap();
    let x_high = module.read_signal_slice(x, 1, 1, test_span()).unwrap();
    let x_value = module.concat(vec![a, y_low], test_span()).unwrap();
    let y_value = module.concat(vec![x_high, b], test_span()).unwrap();
    module
        .connect(LValue::signal(x), x_value, test_span())
        .unwrap();
    module
        .connect(LValue::signal(y), y_value, test_span())
        .unwrap();
    let out = module
        .add_port("out", PortDirection::Output, pair, test_span())
        .unwrap();
    let x_read = module.read_signal(x, test_span()).unwrap();
    module
        .connect(
            LValue::signal(module.port(out).unwrap().signal),
            x_read,
            test_span(),
        )
        .unwrap();

    let graph =
        super::partition::build(&module, RegionPartitionPolicy::with_target_work(1)).unwrap();

    assert_eq!(graph.regions().len(), 1);
    assert_eq!(graph.operations(graph.regions()[0]).len(), 2);
    assert!(graph.predecessors(graph.regions()[0]).is_empty());
    assert!(graph.successors(graph.regions()[0]).is_empty());
}

#[test]
fn word_level_slice_cycle_through_connect_aliases_is_one_region() {
    let mut module = WordModule::new("aliased_slice_cycle");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let pair = WordType::bits(2).unwrap();
    let x = module.add_wire("x", pair, test_span()).unwrap();
    let x_alias = module.add_wire("x_alias", pair, test_span()).unwrap();
    let y = module.add_wire("y", pair, test_span()).unwrap();
    let y_alias = module.add_wire("y_alias", pair, test_span()).unwrap();
    let y_alias_low = module
        .read_signal_slice(y_alias, 0, 1, test_span())
        .unwrap();
    let x_alias_high = module
        .read_signal_slice(x_alias, 1, 1, test_span())
        .unwrap();
    let x_value = module.concat(vec![a, y_alias_low], test_span()).unwrap();
    let y_value = module.concat(vec![x_alias_high, b], test_span()).unwrap();
    module
        .connect(LValue::signal(x), x_value, test_span())
        .unwrap();
    module
        .connect(LValue::signal(y), y_value, test_span())
        .unwrap();
    let x_read = module.read_signal(x, test_span()).unwrap();
    let y_read = module.read_signal(y, test_span()).unwrap();
    module
        .connect(LValue::signal(x_alias), x_read, test_span())
        .unwrap();
    module
        .connect(LValue::signal(y_alias), y_read, test_span())
        .unwrap();
    let out = module
        .add_port("out", PortDirection::Output, pair, test_span())
        .unwrap();
    let output_value = module.read_signal(x_alias, test_span()).unwrap();
    module
        .connect(
            LValue::signal(module.port(out).unwrap().signal),
            output_value,
            test_span(),
        )
        .unwrap();

    let graph =
        super::partition::build(&module, RegionPartitionPolicy::with_target_work(1)).unwrap();

    assert_eq!(graph.regions().len(), 1);
    assert_eq!(graph.operations(graph.regions()[0]).len(), 2);
    assert!(graph.predecessors(graph.regions()[0]).is_empty());
    assert!(graph.successors(graph.regions()[0]).is_empty());
}

#[test]
fn unrelated_component_preserves_existing_region_identity() {
    let mut module = WordModule::new("stable");
    let a = input(&mut module, "a");
    let b = input(&mut module, "b");
    let first = module.binary(BinaryOp::BitAnd, a, b, test_span()).unwrap();
    output(&mut module, "y", first);
    let before = SynthesisRegionGraph::build(&module).unwrap();
    let before_ids = before
        .regions()
        .iter()
        .map(|region| region.id())
        .collect::<BTreeSet<_>>();

    let c = input(&mut module, "c");
    let d = input(&mut module, "d");
    let second = module.binary(BinaryOp::BitOr, c, d, test_span()).unwrap();
    output(&mut module, "z", second);
    let after = SynthesisRegionGraph::build(&module).unwrap();
    let after_ids = after
        .regions()
        .iter()
        .map(|region| region.id())
        .collect::<BTreeSet<_>>();

    assert!(before_ids.is_subset(&after_ids));
    assert_ne!(before.revision(), after.revision());
}

#[test]
fn state_is_a_hard_region_boundary() {
    let mut module = WordModule::new("state");
    let data = input(&mut module, "d");
    let clock = input(&mut module, "clk");
    let inverted = module
        .unary(opto_ir::word::UnaryOp::BitNot, data, test_span())
        .unwrap();
    let state = module
        .register(
            RegisterOp {
                name: None,
                d: inverted,
                clock,
                edge: Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    output(&mut module, "q", state);

    let graph = SynthesisRegionGraph::build(&module).unwrap();
    let state_owner = graph.operation_owner(operation(&module, state)).unwrap();
    let logic_owner = graph.operation_owner(operation(&module, inverted)).unwrap();
    assert_eq!(state_owner, logic_owner);
    assert_eq!(
        graph
            .regions()
            .iter()
            .filter(|region| region.kind() == SynthesisRegionKind::State)
            .count(),
        1
    );
}

#[test]
fn state_fanout_consumers_share_one_downstream_region() {
    let mut module = WordModule::new("state_fanout");
    let data = input(&mut module, "d");
    let other = input(&mut module, "other");
    let clock = input(&mut module, "clk");
    let state = module
        .register(
            RegisterOp {
                name: None,
                d: data,
                clock,
                edge: Edge::Pos,
                enable: None,
                resets: Vec::new(),
            },
            test_span(),
        )
        .unwrap();
    let state_signal = module
        .add_wire("state", WordType::bits(1).unwrap(), test_span())
        .unwrap();
    module
        .connect(LValue::signal(state_signal), state, test_span())
        .unwrap();
    let decoded_state = module.read_signal(state_signal, test_span()).unwrap();
    let next_state = module.read_signal(state_signal, test_span()).unwrap();
    let decoded = module
        .binary(BinaryOp::BitAnd, decoded_state, other, test_span())
        .unwrap();
    let next = module
        .binary(BinaryOp::BitXor, next_state, other, test_span())
        .unwrap();
    output(&mut module, "decoded", decoded);
    output(&mut module, "next", next);

    let graph = SynthesisRegionGraph::build(&module).unwrap();
    let decoded_owner = graph.operation_owner(operation(&module, decoded)).unwrap();
    let next_owner = graph.operation_owner(operation(&module, next)).unwrap();
    assert_eq!(decoded_owner, next_owner);
}

#[test]
fn star_fragments_are_absorbed_without_mutual_nomination() {
    let mut module = WordModule::new("state_star");
    let data = input(&mut module, "d");
    let clock = input(&mut module, "clk");
    let shared = module.unary(UnaryOp::BitNot, data, test_span()).unwrap();
    for _ in 0..12 {
        module
            .register(
                RegisterOp {
                    name: None,
                    d: shared,
                    clock,
                    edge: Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                test_span(),
            )
            .unwrap();
    }

    let graph = super::partition::build(
        &module,
        RegionPartitionPolicy::with_work_limits(1, 8, 16, 64),
    )
    .unwrap();

    assert_eq!(graph.regions().len(), 1);
    assert_eq!(graph.operations(graph.regions()[0]).len(), 13);
}

#[test]
fn inconsistent_work_policy_is_rejected() {
    let mut module = WordModule::new("policy");
    let data = input(&mut module, "d");
    let inverted = module.unary(UnaryOp::BitNot, data, test_span()).unwrap();
    output(&mut module, "q", inverted);
    let policies = [
        RegionPartitionPolicy::with_work_limits(0, 1, 1, 1),
        RegionPartitionPolicy::with_work_limits(1, 0, 1, 1),
        RegionPartitionPolicy::with_work_limits(1, 1, 0, 1),
        RegionPartitionPolicy::with_work_limits(1, 1, 16, 8),
        RegionPartitionPolicy::with_work_limits(1, 32, 16, 64),
    ];

    for policy in policies {
        let error = super::partition::build(&module, policy).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("region work policy is inconsistent")
        );
    }
}

#[test]
fn final_partition_treats_structural_owners_as_indivisible_atoms() {
    let mut module = WordModule::new("owner_atoms");
    let source = input(&mut module, "a");
    let first = module.unary(UnaryOp::BitNot, source, test_span()).unwrap();
    let middle = module.unary(UnaryOp::BitNot, source, test_span()).unwrap();
    let last = module.unary(UnaryOp::BitNot, source, test_span()).unwrap();
    output(&mut module, "x", first);
    output(&mut module, "y", middle);
    output(&mut module, "z", last);
    let first_owner = RegionRowId::from_index(0).unwrap();
    let middle_owner = RegionRowId::from_index(1).unwrap();
    let ownership = crate::regional::StructuralOwnershipProvenance::from_owners_for_test(
        &module,
        vec![Some(first_owner), Some(middle_owner), Some(first_owner)],
    )
    .unwrap();

    let graph = super::partition::build_with_ownership(
        &module,
        RegionPartitionPolicy::with_target_work(1),
        &ownership,
    )
    .unwrap();

    let first_region = graph.operation_owner(operation(&module, first)).unwrap();
    let middle_region = graph.operation_owner(operation(&module, middle)).unwrap();
    let last_region = graph.operation_owner(operation(&module, last)).unwrap();
    assert_eq!(first_region, last_region);
    assert_ne!(first_region, middle_region);
    assert_eq!(graph.regions().len(), 2);
}

#[test]
fn stable_identity_resolves_a_deep_forward_reference_chain() {
    let mut module = WordModule::new("forward");
    let source = input(&mut module, "a");
    let values = (0..2_048)
        .map(|_| module.unary(UnaryOp::BitNot, source, test_span()).unwrap())
        .collect::<Vec<_>>();
    for pair in values.windows(2) {
        let ValueKind::Operation(operation) = module.value(pair[0]).unwrap().kind else {
            unreachable!("unary result must reference its operation");
        };
        let opto_ir::word::OpKind::Unary { arg, .. } =
            &mut module.operation_mut(operation).unwrap().kind
        else {
            unreachable!("unary result must reference a unary operation");
        };
        *arg = pair[1];
    }
    output(&mut module, "y", values[0]);

    let graph = SynthesisRegionGraph::build(&module).unwrap();

    assert!(!graph.regions().is_empty());
}

#[test]
fn unreachable_operations_have_no_region_owner() {
    let mut module = WordModule::new("root_closure");
    let input = input(&mut module, "a");
    let live = module.unary(UnaryOp::BitNot, input, test_span()).unwrap();
    let dead = module
        .binary(BinaryOp::BitAnd, input, input, test_span())
        .unwrap();
    output(&mut module, "y", live);

    let graph = SynthesisRegionGraph::build(&module).unwrap();

    assert!(graph.operation_owner(operation(&module, live)).is_some());
    assert!(graph.operation_owner(operation(&module, dead)).is_none());
    assert_eq!(
        graph
            .regions()
            .iter()
            .map(|region| graph.operations(*region).len())
            .sum::<usize>(),
        1
    );
}

#[test]
fn memory_read_address_operations_have_a_region_owner() {
    let mut module = WordModule::new("memory_read_address");
    let address_type = WordType::bits(2).unwrap();
    let address = input_with_type(&mut module, "address", address_type);
    let upper_bound = module
        .constant(
            opto_ir::ConstBits::from_bin_str("11").unwrap(),
            address_type,
            test_span(),
        )
        .unwrap();
    let translated_address = module
        .binary(BinaryOp::Sub, upper_bound, address, test_span())
        .unwrap();
    let memory = module
        .add_memory(
            "memory",
            WordType::bits(1).unwrap(),
            NonZeroU32::new(4).unwrap(),
            test_span(),
        )
        .unwrap();
    let read_data = module
        .add_wire("read_data", WordType::bits(1).unwrap(), test_span())
        .unwrap();
    module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address: translated_address,
            data: read_data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::OldData,
            source: test_span(),
        })
        .unwrap();

    let graph = SynthesisRegionGraph::build(&module).unwrap();

    assert!(
        graph
            .operation_owner(operation(&module, translated_address))
            .is_some()
    );
}

#[test]
fn stable_port_identity_is_separate_from_value_revision() {
    let build = |op| {
        let mut module = WordModule::new("identity");
        let left = input(&mut module, "a");
        let right = input(&mut module, "b");
        let value = module.binary(op, left, right, test_span()).unwrap();
        output(&mut module, "y", value);
        let graph =
            super::partition::build(&module, RegionPartitionPolicy::with_target_work(1)).unwrap();
        let region = graph.regions()[0];
        let port = graph.port(graph.output_ports(region)[0]).unwrap();
        (
            region.id(),
            region.revision(),
            port.stable_id(),
            port.value_revision(),
        )
    };

    let add = build(BinaryOp::Add);
    let subtract = build(BinaryOp::Sub);

    assert_eq!(add.0, subtract.0);
    assert_ne!(add.1, subtract.1);
    assert_eq!(add.2, subtract.2);
    assert_ne!(add.3, subtract.3);
}

#[test]
fn operation_anchor_ignores_unrelated_arena_insertions() {
    let build = |insert_unrelated| {
        let mut module = WordModule::new("anchors");
        let input = input(&mut module, "a");
        if insert_unrelated {
            let unrelated = module
                .unary(UnaryOp::BitNot, input, SourceSpan::stable("unrelated"))
                .unwrap();
            output(&mut module, "unused_output", unrelated);
        }
        let target = module
            .unary(UnaryOp::BitNot, input, SourceSpan::stable("target"))
            .unwrap();
        output(&mut module, "y", target);
        let operation = operation(&module, target);
        let graph = SynthesisRegionGraph::build(&module).unwrap();
        (
            graph.operation_anchor(operation).unwrap(),
            graph.operation_owner(operation).unwrap().id(),
        )
    };

    assert_eq!(build(false), build(true));
}

#[test]
fn one_source_construct_assigns_distinct_operation_roles() {
    let mut module = WordModule::new("roles");
    let input = input(&mut module, "a");
    let first = module
        .unary(UnaryOp::BitNot, input, SourceSpan::stable("expression"))
        .unwrap();
    let second = module
        .unary(UnaryOp::BitNot, input, SourceSpan::stable("expression"))
        .unwrap();
    output(&mut module, "y", first);
    output(&mut module, "z", second);
    let graph = SynthesisRegionGraph::build(&module).unwrap();

    assert_ne!(
        graph.operation_anchor(operation(&module, first)),
        graph.operation_anchor(operation(&module, second))
    );
}

#[test]
fn partition_rejects_operations_without_source_identity() {
    let mut module = WordModule::new("unanchored");
    let input = input(&mut module, "a");
    let value = module
        .unary(UnaryOp::BitNot, input, SourceSpan::default())
        .unwrap();
    output(&mut module, "y", value);

    let error = SynthesisRegionGraph::build(&module).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no stable frontend source identity"),
        "{error}"
    );
}
