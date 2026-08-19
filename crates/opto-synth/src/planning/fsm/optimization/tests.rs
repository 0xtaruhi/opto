// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn constant(module: &mut word::WordModule, text: &str) -> word::ValueId {
    let bits = ConstBits::from_bin_str(text).unwrap();
    module
        .constant(
            bits,
            word::WordType::bits(u32::try_from(text.len()).unwrap()).unwrap(),
            word::SourceSpan::default(),
        )
        .unwrap()
}

fn sparse_fsm(with_reset: bool) -> (word::WordModule, word::SignalId) {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let state_type = word::WordType::bits(8).unwrap();
    let clock_port = module
        .add_port(
            "clock",
            word::PortDirection::Input,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let clock = module.port(clock_port).unwrap().signal;
    let reset = module
        .add_wire("reset", bit, word::SourceSpan::default())
        .unwrap();
    let select = module
        .add_wire("select", bit, word::SourceSpan::default())
        .unwrap();
    let state = module
        .add_wire("state", state_type, word::SourceSpan::default())
        .unwrap();
    let clock = module
        .read_signal(clock, word::SourceSpan::default())
        .unwrap();
    let reset = module
        .read_signal(reset, word::SourceSpan::default())
        .unwrap();
    let select = module
        .read_signal(select, word::SourceSpan::default())
        .unwrap();
    let state_read = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let idle = constant(&mut module, "00000000");
    let first = constant(&mut module, "00010000");
    let second = constant(&mut module, "00100000");
    let active = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            second,
            word::SourceSpan::default(),
        )
        .unwrap();
    let active_port = module
        .add_port(
            "active",
            word::PortDirection::Output,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(active_port).unwrap().signal),
            active,
            word::SourceSpan::default(),
        )
        .unwrap();
    let first_or_hold = module
        .mux(select, first, state_read, word::SourceSpan::default())
        .unwrap();
    let next = module
        .mux(select, second, first_or_hold, word::SourceSpan::default())
        .unwrap();
    let resets = with_reset
        .then_some(word::Reset {
            kind: word::ResetKind::Sync,
            value: reset,
            active_high: true,
            reset_value: idle,
        })
        .into_iter()
        .collect();
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: next,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets,
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(state),
            register,
            word::SourceSpan::default(),
        )
        .unwrap();
    (module, state)
}

fn replace_state_register_data(
    module: &mut word::WordModule,
    state: word::SignalId,
    next: word::ValueId,
) {
    let register = module
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register.clone()),
            _ => None,
        })
        .unwrap();
    let replacement = module
        .register(
            word::RegisterOp {
                d: next,
                ..register
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    let connects = module.take_connects();
    for mut connect in connects {
        if connect.target.signal == state {
            connect.value = replacement;
        }
        module
            .connect(connect.target, connect.value, connect.source)
            .unwrap();
    }
    module.compact_netlist().unwrap();
}

#[allow(
    clippy::similar_names,
    reason = "the A0/A1/B0/B1 bindings intentionally mirror the encoded FSM state names"
)]
fn mergeable_fsm(split_successors: bool) -> word::WordModule {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let state_type = word::WordType::bits(8).unwrap();
    let phase_type = word::WordType::bits(3).unwrap();
    let input_ports = ["clock", "reset", "select"].map(|name| {
        module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let [clock, reset, select] = input_ports.map(|port| {
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let state = module
        .add_wire("state", state_type, word::SourceSpan::default())
        .unwrap();
    let phase_port = module
        .add_port(
            "phase",
            word::PortDirection::Output,
            phase_type,
            word::SourceSpan::default(),
        )
        .unwrap();
    let state_read = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let idle = constant(&mut module, "00000000");
    let a0 = constant(&mut module, "00010000");
    let a1 = constant(&mut module, "00100000");
    let b0 = constant(&mut module, "01000000");
    let b1 = constant(&mut module, "10000000");
    let is_idle = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            idle,
            word::SourceSpan::default(),
        )
        .unwrap();
    let is_a0 = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            a0,
            word::SourceSpan::default(),
        )
        .unwrap();
    let is_a1 = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            a1,
            word::SourceSpan::default(),
        )
        .unwrap();
    let is_b0 = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            b0,
            word::SourceSpan::default(),
        )
        .unwrap();
    let is_b1 = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            b1,
            word::SourceSpan::default(),
        )
        .unwrap();
    let not_select = module
        .unary(word::UnaryOp::BitNot, select, word::SourceSpan::default())
        .unwrap();
    let select_a0 = module
        .binary(
            word::BinaryOp::BitAnd,
            is_idle,
            select,
            word::SourceSpan::default(),
        )
        .unwrap();
    let select_a1 = module
        .binary(
            word::BinaryOp::BitAnd,
            is_idle,
            not_select,
            word::SourceSpan::default(),
        )
        .unwrap();
    let in_a = module
        .binary(
            word::BinaryOp::BitOr,
            is_a0,
            is_a1,
            word::SourceSpan::default(),
        )
        .unwrap();
    let in_b = module
        .binary(
            word::BinaryOp::BitOr,
            is_b0,
            is_b1,
            word::SourceSpan::default(),
        )
        .unwrap();
    let phase = if split_successors {
        module
            .concat(vec![in_a, is_b0, is_b1], word::SourceSpan::default())
            .unwrap()
    } else {
        module
            .concat(vec![in_a, in_b, in_b], word::SourceSpan::default())
            .unwrap()
    };
    module
        .connect(
            word::LValue::signal(module.port(phase_port).unwrap().signal),
            phase,
            word::SourceSpan::default(),
        )
        .unwrap();
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: idle,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: [
                    (reset, idle),
                    (select_a0, a0),
                    (select_a1, a1),
                    (is_a0, b0),
                    (is_a1, b1),
                ]
                .map(|(value, reset_value)| word::Reset {
                    kind: word::ResetKind::Sync,
                    value,
                    active_high: true,
                    reset_value,
                })
                .into(),
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(state),
            register,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
}

fn refinement_chain_fsm(state_count: usize) -> word::WordModule {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let state_type = word::WordType::bits(8).unwrap();
    let [clock, reset] = ["clock", "reset"].map(|name| {
        let port = module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let active_port = module
        .add_port(
            "active",
            word::PortDirection::Output,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let state = module
        .add_wire("state", state_type, word::SourceSpan::default())
        .unwrap();
    let state_read = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let states = (0..state_count)
        .map(|state| constant(&mut module, &format!("{state:08b}")))
        .collect::<Vec<_>>();
    let active = module
        .binary(
            word::BinaryOp::Eq,
            state_read,
            states[0],
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(active_port).unwrap().signal),
            active,
            word::SourceSpan::default(),
        )
        .unwrap();
    let mut next = states[0];
    for source in 1..state_count {
        let selected = module
            .binary(
                word::BinaryOp::Eq,
                state_read,
                states[source],
                word::SourceSpan::default(),
            )
            .unwrap();
        next = module
            .mux(
                selected,
                states[source - 1],
                next,
                word::SourceSpan::default(),
            )
            .unwrap();
    }
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: next,
                clock,
                edge: word::Edge::Pos,
                enable: None,
                resets: [word::Reset {
                    kind: word::ResetKind::Sync,
                    value: reset,
                    active_high: true,
                    reset_value: states[state_count - 1],
                }]
                .into(),
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(state),
            register,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
}

#[test]
fn merges_states_with_equal_observations_and_successor_classes() {
    let mut module = mergeable_fsm(false);
    let catalog = derive_catalog(&module, crate::test_runtime()).unwrap();

    assert_eq!(catalog.machines.len(), 1);
    assert_eq!(catalog.machines[0].states.len(), 5);
    assert_eq!(catalog.machines[0].representatives.len(), 3);
    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        1
    );
    let width = module
        .operations()
        .iter()
        .find_map(|operation| {
            matches!(operation.kind, word::OpKind::Register(_))
                .then(|| module.value(operation.result).unwrap().ty.width())
        })
        .unwrap();
    assert_eq!(width, 2);
}

#[test]
fn skips_dead_state_before_fsm_analysis() {
    let (mut module, state) = sparse_fsm(true);
    let connects = module.take_connects();
    for connect in connects {
        if connect.target.signal != state {
            module
                .connect(connect.target, connect.value, connect.source)
                .unwrap();
        }
    }

    let catalog = derive_catalog(&module, crate::test_runtime()).unwrap();

    assert!(catalog.machines.is_empty());
}

#[test]
fn keeps_lookalike_states_when_successor_classes_differ() {
    let mut module = mergeable_fsm(true);
    let catalog = derive_catalog(&module, crate::test_runtime()).unwrap();

    assert_eq!(catalog.machines.len(), 1);
    assert_eq!(catalog.machines[0].representatives.len(), 5);
    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        1
    );
    let width = module
        .operations()
        .iter()
        .find_map(|operation| {
            matches!(operation.kind, word::OpKind::Register(_))
                .then(|| module.value(operation.result).unwrap().ty.width())
        })
        .unwrap();
    assert_eq!(width, 3);
}

#[test]
fn refinement_scaling_is_bounded_by_structural_signatures() {
    let module = refinement_chain_fsm(MAX_STATES);
    let catalog = derive_catalog(&module, crate::test_runtime()).unwrap();

    assert_eq!(catalog.machines.len(), 1);
    assert_eq!(catalog.machines[0].states.len(), MAX_STATES);
    assert_eq!(catalog.machines[0].representatives.len(), MAX_STATES);
}

#[test]
fn removes_unreachable_states_before_reencoding() {
    let (mut module, state) = sparse_fsm(true);

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Timing).unwrap(),
        1
    );

    let registers = module
        .operations()
        .iter()
        .filter_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some((operation, register)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(registers.len(), 1);
    assert_eq!(module.value(registers[0].0.result).unwrap().ty.width(), 1);
    assert_eq!(registers[0].1.resets.len(), 1);
    assert_eq!(
        module
            .value(registers[0].1.resets[0].reset_value)
            .unwrap()
            .ty
            .width(),
        1
    );
    let state_driver = module
        .connects()
        .iter()
        .find(|connect| connect.target.signal == state)
        .unwrap();
    assert_eq!(module.value(state_driver.value).unwrap().ty.width(), 8);
    let encoded_target = module
        .connects()
        .iter()
        .find(|connect| connect.value == registers[0].0.result)
        .unwrap()
        .target
        .signal;
    assert!(module.signal(encoded_target).unwrap().name.is_none());
    assert_eq!(
        module
            .operations()
            .iter()
            .filter(|operation| matches!(
                operation.kind,
                word::OpKind::Binary {
                    op: word::BinaryOp::Eq,
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn preserves_an_observable_state_encoding() {
    let (mut module, state) = sparse_fsm(true);
    let state_type = module.signal(state).unwrap().ty;
    let output = module
        .add_port(
            "state_o",
            word::PortDirection::Output,
            state_type,
            word::SourceSpan::default(),
        )
        .unwrap();
    let state_read = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let output_signal = module.port(output).unwrap().signal;
    module
        .connect(
            word::LValue::signal(output_signal),
            state_read,
            word::SourceSpan::default(),
        )
        .unwrap();

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        0
    );
    let register_width = module
        .operations()
        .iter()
        .find_map(|operation| {
            matches!(operation.kind, word::OpKind::Register(_))
                .then(|| module.value(operation.result).unwrap().ty.width())
        })
        .unwrap();
    assert_eq!(register_width, 8);
}

#[test]
fn leaves_unreset_state_space_unconstrained() {
    let (mut module, _) = sparse_fsm(false);

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        0
    );
}

#[test]
fn leaves_arithmetic_state_transitions_outside_fsm_reencoding() {
    let (mut module, state) = sparse_fsm(true);
    let state_read = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let one = constant(&mut module, "00000001");
    let next = module
        .binary(
            word::BinaryOp::Add,
            state_read,
            one,
            word::SourceSpan::default(),
        )
        .unwrap();
    replace_state_register_data(&mut module, state, next);

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        0
    );
}

#[test]
fn bounds_transition_extraction_without_recursive_descent() {
    let (mut module, state) = sparse_fsm(true);
    let bit = word::WordType::bits(1).unwrap();
    let select = module
        .add_wire("deep_select", bit, word::SourceSpan::default())
        .unwrap();
    let select = module
        .read_signal(select, word::SourceSpan::default())
        .unwrap();
    let state_read = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let first = constant(&mut module, "00010000");
    let mut next = state_read;
    for _ in 0..=MAX_TRANSITION_VALUES {
        next = module
            .mux(select, first, next, word::SourceSpan::default())
            .unwrap();
    }
    replace_state_register_data(&mut module, state, next);

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        0
    );
}

#[test]
fn rebuilds_area_transitions_in_the_compact_state_domain() {
    let (mut module, _) = sparse_fsm(true);

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        1
    );
    assert_eq!(
        module
            .operations()
            .iter()
            .filter(|operation| matches!(
                operation.kind,
                word::OpKind::Binary {
                    op: word::BinaryOp::Eq,
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn selects_compact_and_one_hot_codes_from_the_synthesis_objective() {
    let states = ["00000000", "00010000", "00100000", "01000000"]
        .map(|text| ConstBits::from_bin_str(text).unwrap());

    let (area_width, _) = choose_encoding(&states, 8, FsmObjective::Area).unwrap();
    let (timing_width, codes) = choose_encoding(&states, 8, FsmObjective::Timing).unwrap();

    assert_eq!(area_width, 2);
    assert_eq!(timing_width, 3);
    assert!(is_zero(&codes[0]));
    assert!(codes[1..].iter().all(|code| {
        code.as_slice()
            .iter()
            .filter(|&&bit| bit == BitVal::One)
            .count()
            == 1
    }));
}

#[test]
fn only_a_constraint_on_the_machine_clock_selects_timing_encoding() {
    let (module, _) = sparse_fsm(true);
    let catalog = derive_catalog(&module, crate::test_runtime()).unwrap();
    let machine = &catalog.machines[0];
    let clock_port = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(1).unwrap());
    let state_port = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(2).unwrap());
    let bindings = opto_timing::PortBindings::new([clock_port, state_port]);
    let mut timing = opto_timing::TimingContext::new();

    assert_eq!(
        machine_objective(&module, machine, &timing, &bindings),
        FsmObjective::Area
    );

    timing
        .create_clock(
            opto_timing::ClockId::from_uid(opto_core::ObjectUid::from_raw(3).unwrap()),
            opto_timing::ClockSpec::new("clock", 1.0, vec![clock_port], None).unwrap(),
        )
        .unwrap();
    assert_eq!(
        machine_objective(&module, machine, &timing, &bindings),
        FsmObjective::Timing
    );
}

#[test]
fn keeps_states_observed_through_signal_aliases() {
    let mut module = word::WordModule::new("top");
    let bit = word::WordType::bits(1).unwrap();
    let [clock, reset, enable] = ["clock", "reset", "enable"].map(|name| {
        let port = module
            .add_port(
                name,
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    });
    let output = module
        .add_port(
            "output",
            word::PortDirection::Output,
            bit,
            word::SourceSpan::default(),
        )
        .unwrap();
    let state = module
        .add_wire("state", bit, word::SourceSpan::default())
        .unwrap();
    let alias = module
        .add_wire("state_alias", bit, word::SourceSpan::default())
        .unwrap();
    let state_value = module
        .read_signal(state, word::SourceSpan::default())
        .unwrap();
    let alias_value = module
        .read_signal(alias, word::SourceSpan::default())
        .unwrap();
    let zero = constant(&mut module, "0");
    let one = constant(&mut module, "1");
    let register = module
        .register(
            word::RegisterOp {
                name: None,
                d: zero,
                clock,
                edge: word::Edge::Pos,
                enable: Some(word::Enable {
                    value: enable,
                    active_high: true,
                }),
                resets: vec![word::Reset {
                    kind: word::ResetKind::Sync,
                    value: reset,
                    active_high: true,
                    reset_value: one,
                }],
            },
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(state),
            register,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(alias),
            state_value,
            word::SourceSpan::default(),
        )
        .unwrap();
    module
        .connect(
            word::LValue::signal(module.port(output).unwrap().signal),
            alias_value,
            word::SourceSpan::default(),
        )
        .unwrap();

    assert_eq!(
        optimize_with_objective(&mut module, FsmObjective::Area).unwrap(),
        0
    );
}
