// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use opto_ir::proc::{
    AssignmentMode, ProcBuilder, ProcTarget, ProcedureKind, SensitivityEvent, SwitchArmSpec,
};
use opto_ir::word::{PortDirection, SourceSpan, WordModule, WordType};

fn bit() -> WordType {
    WordType::bits(1).unwrap()
}

fn span() -> SourceSpan {
    SourceSpan::stable("frontend test")
}

fn port(module: &mut WordModule, name: &str, direction: PortDirection) -> word::SignalId {
    let port = module.add_port(name, direction, bit(), span()).unwrap();
    module.port(port).unwrap().signal
}

fn input(module: &mut WordModule, name: &str) -> word::SignalId {
    port(module, name, PortDirection::Input)
}

fn output(module: &mut WordModule, name: &str) -> word::SignalId {
    port(module, name, PortDirection::Output)
}

fn read(module: &mut WordModule, signal: word::SignalId) -> word::ValueId {
    module.read_signal(signal, span()).unwrap()
}

fn sensitivity(
    module: &mut WordModule,
    signal: word::SignalId,
    edge: word::Edge,
) -> SensitivityEvent {
    SensitivityEvent {
        value: read(module, signal),
        edge,
        iff: None,
    }
}

fn reads_signal(module: &WordModule, value: word::ValueId, signal: word::SignalId) -> bool {
    matches!(
        module.value(value).map(|value| &value.kind),
        Some(word::ValueKind::Signal(reference))
            if reference.signal == signal
                && reference.lsb == 0
                && reference.width() == module.signal(signal).unwrap().ty.width()
    )
}

fn depends_on_signal(module: &WordModule, root: word::ValueId, signal: word::SignalId) -> bool {
    let mut pending = vec![root];
    let mut visited = vec![false; module.values().len()];
    while let Some(value) = pending.pop() {
        if visited[value.index()] {
            continue;
        }
        visited[value.index()] = true;
        match &module.value(value).unwrap().kind {
            word::ValueKind::Constant(_) => {}
            word::ValueKind::Signal(reference) => {
                if reference.signal == signal {
                    return true;
                }
            }
            word::ValueKind::Operation(operation) => {
                pending.extend(crate::word::operation_inputs(
                    &module.operation(*operation).unwrap().kind,
                ));
            }
        }
    }
    false
}

fn lower(module: WordModule, builder: ProcBuilder) -> Result<WordModule, crate::SynthError> {
    lower_procedures(
        RtlModule::new(module, builder.seal().unwrap()).unwrap(),
        crate::test_runtime(),
        &mut |_| {},
    )
}

fn independent_procedures() -> RtlModule {
    let mut module = WordModule::new("parallel_frontend");
    let input_a = input(&mut module, "a");
    let input_b = input(&mut module, "b");
    let output_a = output(&mut module, "y_a");
    let output_b = output(&mut module, "y_b");
    let value_a = read(&mut module, input_a);
    let value_b = read(&mut module, input_b);
    let mut builder = ProcBuilder::new();
    for (target, value) in [(output_a, value_a), (output_b, value_b)] {
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let block = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                block,
                AssignmentMode::Blocking,
                ProcTarget::signal(target),
                value,
                span(),
            )
            .unwrap();
        builder.terminate_return(block, span()).unwrap();
    }
    RtlModule::new(module, builder.seal().unwrap()).unwrap()
}

#[test]
fn parallel_cfg_analysis_preserves_serial_word_ir() {
    let serial_runtime =
        opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 1 })
            .unwrap();
    let parallel_runtime =
        opto_runtime::ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 4 })
            .unwrap();

    let serial = lower_procedures(independent_procedures(), &serial_runtime, &mut |_| {}).unwrap();
    let parallel =
        lower_procedures(independent_procedures(), &parallel_runtime, &mut |_| {}).unwrap();

    assert_eq!(serial, parallel);
}

#[test]
fn unreachable_flop_write_has_explicit_dont_care_producer() {
    let mut module = WordModule::new("unreachable_flop_write");
    let clock = input(&mut module, "clock");
    let data = input(&mut module, "data");
    let storage = module.add_wire("storage", bit(), span()).unwrap();
    let observed = output(&mut module, "observed");
    let storage_value = read(&mut module, storage);
    module
        .connect(word::LValue::signal(observed), storage_value, span())
        .unwrap();
    let data = read(&mut module, data);
    let never = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), span())
        .unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure([sensitivity(&mut module, clock, word::Edge::Pos)], span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let update = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_branch(entry, never, update, exit, span())
        .unwrap();
    cfg.assign(
        update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(storage),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(update, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower_to_validated_word(
        RtlModule::new(module, cfg.seal().unwrap()).unwrap(),
        &crate::ReferencePortMap::new(),
        crate::test_runtime(),
        &mut |_| {},
    )
    .unwrap();

    let driver = lowered
        .connects()
        .iter()
        .find(|connect| connect.target.signal == storage)
        .expect("unreachable procedural target has an SSA producer");
    assert!(matches!(
        lowered.value(driver.value).unwrap().kind,
        word::ValueKind::Constant(ref bits) if bits.as_slice() == [opto_ir::BitVal::X]
    ));
    assert!(
        lowered
            .operations()
            .iter()
            .all(|operation| !matches!(operation.kind, word::OpKind::Register(_)))
    );
}

#[test]
fn latch_procedure_supports_per_target_assignment_scheduling() {
    let mut module = WordModule::new("mixed_latch_scheduling");
    let gate_signal = input(&mut module, "gate");
    let data_signal = input(&mut module, "data");
    let blocking_target = output(&mut module, "blocking_target");
    let nonblocking_target = output(&mut module, "nonblocking_target");
    let gate = read(&mut module, gate_signal);
    let data = read(&mut module, data_signal);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::CombinationalOrLatch, span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let update = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_branch(entry, gate, update, exit, span())
        .unwrap();
    cfg.assign(
        update,
        AssignmentMode::Blocking,
        ProcTarget::signal(blocking_target),
        data,
        span(),
    )
    .unwrap();
    cfg.assign(
        update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(nonblocking_target),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(update, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();

    assert_eq!(
        lowered
            .operations()
            .iter()
            .filter(|operation| matches!(operation.kind, word::OpKind::Latch(_)))
            .count(),
        2
    );
}

#[test]
fn inherited_assignment_ignores_later_branch_guards() {
    let mut module = WordModule::new("top");
    let decode_signal = input(&mut module, "decode");
    let feedback_signal = input(&mut module, "feedback");
    let later_signal = input(&mut module, "later");
    let y = output(&mut module, "y");
    let side = output(&mut module, "side");
    let decode = read(&mut module, decode_signal);
    let feedback = read(&mut module, feedback_signal);
    let later = read(&mut module, later_signal);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), span())
        .unwrap();
    let one = module
        .constant(ConstBits::from_bin_str("1").unwrap(), bit(), span())
        .unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let decode_block = cfg.add_block(procedure, span()).unwrap();
    let feedback_true = cfg.add_block(procedure, span()).unwrap();
    let feedback_false = cfg.add_block(procedure, span()).unwrap();
    let decode_join = cfg.add_block(procedure, span()).unwrap();
    let later_true = cfg.add_block(procedure, span()).unwrap();
    let later_false = cfg.add_block(procedure, span()).unwrap();
    let later_join = cfg.add_block(procedure, span()).unwrap();
    let idle_block = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();

    cfg.assign(
        entry,
        AssignmentMode::Blocking,
        ProcTarget::signal(y),
        zero,
        span(),
    )
    .unwrap();
    cfg.assign(
        entry,
        AssignmentMode::Blocking,
        ProcTarget::signal(side),
        zero,
        span(),
    )
    .unwrap();
    cfg.terminate_branch(entry, decode, decode_block, idle_block, span())
        .unwrap();

    cfg.assign(
        decode_block,
        AssignmentMode::Blocking,
        ProcTarget::signal(y),
        one,
        span(),
    )
    .unwrap();
    cfg.terminate_branch(
        decode_block,
        feedback,
        feedback_true,
        feedback_false,
        span(),
    )
    .unwrap();
    cfg.assign(
        feedback_true,
        AssignmentMode::Blocking,
        ProcTarget::signal(side),
        one,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(feedback_true, decode_join, span())
        .unwrap();
    cfg.terminate_jump(feedback_false, decode_join, span())
        .unwrap();
    cfg.terminate_branch(decode_join, later, later_true, later_false, span())
        .unwrap();
    cfg.assign(
        later_true,
        AssignmentMode::Blocking,
        ProcTarget::signal(side),
        one,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(later_true, later_join, span()).unwrap();
    cfg.terminate_jump(later_false, later_join, span()).unwrap();
    cfg.terminate_jump(later_join, exit, span()).unwrap();
    cfg.terminate_jump(idle_block, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let y_value = lowered
        .connects()
        .iter()
        .find(|connect| connect.target.signal == y)
        .unwrap()
        .value;
    assert!(depends_on_signal(&lowered, y_value, decode_signal));
    assert!(!depends_on_signal(&lowered, y_value, feedback_signal));
    assert!(!depends_on_signal(&lowered, y_value, later_signal));
}

#[test]
fn joins_materialize_one_deterministic_phi() {
    let build = || {
        let mut module = WordModule::new("top");
        let select_signal = input(&mut module, "select");
        let a_signal = input(&mut module, "a");
        let b_signal = input(&mut module, "b");
        let y = output(&mut module, "y");
        let select = read(&mut module, select_signal);
        let a = read(&mut module, a_signal);
        let b = read(&mut module, b_signal);
        let mut cfg = ProcBuilder::new();
        let procedure = cfg
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let entry = cfg.add_block(procedure, span()).unwrap();
        let then_block = cfg.add_block(procedure, span()).unwrap();
        let else_block = cfg.add_block(procedure, span()).unwrap();
        let join = cfg.add_block(procedure, span()).unwrap();
        let exit = cfg.add_block(procedure, span()).unwrap();
        cfg.terminate_branch(entry, select, then_block, else_block, span())
            .unwrap();
        cfg.assign(
            then_block,
            AssignmentMode::Blocking,
            ProcTarget::signal(y),
            a,
            span(),
        )
        .unwrap();
        cfg.terminate_jump(then_block, join, span()).unwrap();
        cfg.assign(
            else_block,
            AssignmentMode::Blocking,
            ProcTarget::signal(y),
            b,
            span(),
        )
        .unwrap();
        cfg.terminate_jump(else_block, join, span()).unwrap();
        cfg.terminate_jump(join, exit, span()).unwrap();
        cfg.terminate_return(exit, span()).unwrap();
        lower(module, cfg).unwrap()
    };

    let first = build();
    let second = build();
    assert_eq!(first, second);
    let connect = first
        .connects()
        .iter()
        .find(|connect| connect.target.signal == first.signal_id("y").unwrap())
        .unwrap();
    assert!(matches!(
        first.value(connect.value).unwrap().kind,
        word::ValueKind::Operation(operation)
            if matches!(first.operation(operation).unwrap().kind, word::OpKind::Mux { .. })
    ));
}

#[test]
fn exhaustive_binary_switch_has_no_reachable_default() {
    let mut module = WordModule::new("top");
    let selector_ty = WordType::bits(2).unwrap();
    let selector_port = module
        .add_port("selector", PortDirection::Input, selector_ty, span())
        .unwrap();
    let data = input(&mut module, "data");
    let y = output(&mut module, "y");
    let selector_signal = module.port(selector_port).unwrap().signal;
    let selector = read(&mut module, selector_signal);
    let data = read(&mut module, data);
    let patterns = ["00", "01", "10", "11"]
        .map(|bits| {
            module
                .constant(ConstBits::from_bin_str(bits).unwrap(), selector_ty, span())
                .unwrap()
        })
        .to_vec();
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let arms = patterns
        .into_iter()
        .map(|pattern| {
            let block = cfg.add_block(procedure, span()).unwrap();
            (pattern, block)
        })
        .collect::<Vec<_>>();
    let default = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_switch(
        entry,
        selector,
        arms.iter().map(|&(pattern, target)| SwitchArmSpec {
            pattern,
            target,
            source: span(),
        }),
        default,
        span(),
    )
    .unwrap();
    for &(_, block) in &arms {
        cfg.assign(
            block,
            AssignmentMode::Blocking,
            ProcTarget::signal(y),
            data,
            span(),
        )
        .unwrap();
        cfg.terminate_jump(block, exit, span()).unwrap();
    }
    cfg.terminate_jump(default, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    assert_eq!(lower(module, cfg).unwrap().connects().len(), 1);
}

#[test]
fn process_locals_are_consumed_at_the_phase_boundary() {
    let mut module = WordModule::new("top");
    let a = input(&mut module, "a");
    let y = output(&mut module, "y");
    let temporary = module
        .add_process_local_signal("temporary", bit(), span())
        .unwrap();
    let a = read(&mut module, a);
    let temporary_value = read(&mut module, temporary);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(temporary),
        a,
        span(),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(y),
        temporary_value,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    assert!(
        lowered
            .signals()
            .iter()
            .all(|signal| signal.kind != word::SignalKind::ProcessLocal)
    );
    assert!(lowered.signal_id("temporary").is_none());
}

#[test]
fn deep_cfg_is_iterative_and_final_seal_rejects_cycles() {
    let mut module = WordModule::new("top");
    let a_signal = input(&mut module, "a");
    let y = output(&mut module, "y");
    let a = read(&mut module, a_signal);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let blocks = (0..1024)
        .map(|_| cfg.add_block(procedure, span()).unwrap())
        .collect::<Vec<_>>();
    for pair in blocks.windows(2) {
        cfg.terminate_jump(pair[0], pair[1], span()).unwrap();
    }
    cfg.assign(
        *blocks.last().unwrap(),
        AssignmentMode::Blocking,
        ProcTarget::signal(y),
        a,
        span(),
    )
    .unwrap();
    cfg.terminate_return(*blocks.last().unwrap(), span())
        .unwrap();
    assert_eq!(lower(module, cfg).unwrap().connects().len(), 1);

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let loop_block = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_jump(entry, loop_block, span()).unwrap();
    cfg.terminate_jump(loop_block, loop_block, span()).unwrap();
    let error = cfg.seal().unwrap_err();
    assert!(error.to_string().contains("control-flow cycle"), "{error}");
}

#[test]
fn selected_clock_values_reach_register_operations() {
    let mut module = WordModule::new("selected_clocks");
    let clocks_port = module
        .add_port(
            "clocks",
            PortDirection::Input,
            WordType::bits(4).unwrap(),
            span(),
        )
        .unwrap();
    let index_port = module
        .add_port(
            "index",
            PortDirection::Input,
            WordType::bits(2).unwrap(),
            span(),
        )
        .unwrap();
    let clocks = module.port(clocks_port).unwrap().signal;
    let index = module.port(index_port).unwrap().signal;
    let d = input(&mut module, "d");
    let q_static = output(&mut module, "q_static");
    let q_dynamic = output(&mut module, "q_dynamic");
    let static_clock = module.read_signal_slice(clocks, 2, 1, span()).unwrap();
    let clocks_value = read(&mut module, clocks);
    let index_value = read(&mut module, index);
    let dynamic_clock = module
        .dynamic_extract(clocks_value, index_value, 1, span())
        .unwrap();
    let data = read(&mut module, d);
    let mut cfg = ProcBuilder::new();
    for (clock, edge, target) in [
        (static_clock, word::Edge::Pos, q_static),
        (dynamic_clock, word::Edge::Neg, q_dynamic),
    ] {
        let procedure = cfg
            .add_clocked_procedure(
                [SensitivityEvent {
                    value: clock,
                    edge,
                    iff: None,
                }],
                span(),
            )
            .unwrap();
        let block = cfg.add_block(procedure, span()).unwrap();
        cfg.assign(
            block,
            AssignmentMode::Nonblocking,
            ProcTarget::signal(target),
            data,
            span(),
        )
        .unwrap();
        cfg.terminate_return(block, span()).unwrap();
    }

    let lowered = lower(module, cfg).unwrap();
    let registers = lowered
        .operations()
        .iter()
        .filter_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(registers.len(), 2);
    assert!(registers.iter().any(|register| {
        register.edge == word::Edge::Pos
            && matches!(
                lowered.value(register.clock).map(|value| &value.kind),
                Some(word::ValueKind::Signal(reference))
                    if reference.signal == clocks && reference.lsb == 2 && reference.width() == 1
            )
    }));
    assert!(registers.iter().any(|register| {
        register.edge == word::Edge::Neg
            && matches!(
                lowered.value(register.clock).map(|value| &value.kind),
                Some(word::ValueKind::Operation(operation))
                    if matches!(
                        lowered.operation(*operation).map(|operation| &operation.kind),
                        Some(word::OpKind::DynamicExtract { width, .. }) if width.get() == 1
                    )
            )
    }));
}

#[test]
fn duplicate_clock_events_or_their_independent_iff_qualifiers() {
    let mut module = WordModule::new("qualified_clock");
    let clocks_port = module
        .add_port(
            "clocks",
            PortDirection::Input,
            WordType::bits(4).unwrap(),
            span(),
        )
        .unwrap();
    let index_port = module
        .add_port(
            "index",
            PortDirection::Input,
            WordType::bits(2).unwrap(),
            span(),
        )
        .unwrap();
    let clocks = module.port(clocks_port).unwrap().signal;
    let index = module.port(index_port).unwrap().signal;
    let enable_a = input(&mut module, "enable_a");
    let enable_b = input(&mut module, "enable_b");
    let data_signal = input(&mut module, "data");
    let q = output(&mut module, "q");
    let clock_a = {
        let value = read(&mut module, clocks);
        let offset = read(&mut module, index);
        module.dynamic_extract(value, offset, 1, span()).unwrap()
    };
    let clock_b = {
        let value = read(&mut module, clocks);
        let offset = read(&mut module, index);
        module.dynamic_extract(value, offset, 1, span()).unwrap()
    };
    let qualifier_a = read(&mut module, enable_a);
    let qualifier_b = read(&mut module, enable_b);
    let data = read(&mut module, data_signal);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [(clock_a, qualifier_a), (clock_b, qualifier_b)].map(|(value, iff)| SensitivityEvent {
                value,
                edge: word::Edge::Pos,
                iff: Some(iff),
            }),
            span(),
        )
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let register = lowered
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .unwrap();
    let enable = register.enable.unwrap();
    assert!(enable.active_high);
    assert!(depends_on_signal(&lowered, enable.value, enable_a));
    assert!(depends_on_signal(&lowered, enable.value, enable_b));
}

#[test]
fn blocking_is_visible_but_nonblocking_is_only_scheduled() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let data_signal = input(&mut module, "d");
    let q = output(&mut module, "q");
    let r = output(&mut module, "r");
    let data = read(&mut module, data_signal);
    let q_old = read(&mut module, q);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure([sensitivity(&mut module, clock, word::Edge::Pos)], span())
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(r),
        q_old,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();
    let lowered = lower(module, cfg).unwrap();
    let r_register = lowered
        .connects()
        .iter()
        .find(|connect| connect.target.signal == r)
        .and_then(|connect| lowered.value(connect.value))
        .and_then(|value| match value.kind {
            word::ValueKind::Operation(operation) => lowered.operation(operation),
            _ => None,
        })
        .and_then(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .unwrap();
    assert!(reads_signal(&lowered, r_register.d, q));
}

#[test]
fn prioritized_async_set_clear_matches_every_sensitivity_event() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let clear_signal = input(&mut module, "clear");
    // Allocate clear first while keeping preset first in the sensitivity list;
    // reset matching must be independent of arena and source event order.
    let preset_signal = input(&mut module, "preset");
    let enable_signal = input(&mut module, "enable");
    let data_signal = input(&mut module, "data");
    let q = output(&mut module, "q");
    let preset = read(&mut module, preset_signal);
    let clear = read(&mut module, clear_signal);
    let enable = read(&mut module, enable_signal);
    let data = read(&mut module, data_signal);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), span())
        .unwrap();
    let one = module
        .constant(ConstBits::from_bin_str("1").unwrap(), bit(), span())
        .unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [
                sensitivity(&mut module, clock, word::Edge::Pos),
                sensitivity(&mut module, preset_signal, word::Edge::Pos),
                sensitivity(&mut module, clear_signal, word::Edge::Pos),
            ],
            span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let preset_block = cfg.add_block(procedure, span()).unwrap();
    let test_clear = cfg.add_block(procedure, span()).unwrap();
    let clear_block = cfg.add_block(procedure, span()).unwrap();
    let test_enable = cfg.add_block(procedure, span()).unwrap();
    let data_block = cfg.add_block(procedure, span()).unwrap();
    let hold_block = cfg.add_block(procedure, span()).unwrap();
    let enable_join = cfg.add_block(procedure, span()).unwrap();
    let clear_join = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();

    cfg.terminate_branch(entry, preset, preset_block, test_clear, span())
        .unwrap();
    cfg.assign(
        preset_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        one,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(preset_block, exit, span()).unwrap();
    cfg.terminate_branch(test_clear, clear, clear_block, test_enable, span())
        .unwrap();
    cfg.assign(
        clear_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        zero,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(clear_block, clear_join, span()).unwrap();
    cfg.terminate_branch(test_enable, enable, data_block, hold_block, span())
        .unwrap();
    cfg.assign(
        data_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(data_block, enable_join, span()).unwrap();
    cfg.terminate_jump(hold_block, enable_join, span()).unwrap();
    cfg.terminate_jump(enable_join, clear_join, span()).unwrap();
    cfg.terminate_jump(clear_join, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let register = lowered
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .unwrap();
    assert_eq!(register.resets.len(), 2);
    assert!(reads_signal(
        &lowered,
        register.resets[0].value,
        preset_signal
    ));
    assert!(reads_signal(
        &lowered,
        register.resets[1].value,
        clear_signal
    ));
}

#[test]
fn factors_async_reset_through_an_implied_outer_enable_guard() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let reset_signal = input(&mut module, "reset_n");
    let enable_signal = input(&mut module, "enable");
    let data_signal = input(&mut module, "data");
    let q = output(&mut module, "q");
    let reset_n = read(&mut module, reset_signal);
    let enable = read(&mut module, enable_signal);
    let data = read(&mut module, data_signal);
    let reset_asserted = module
        .unary(word::UnaryOp::LogicalNot, reset_n, span())
        .unwrap();
    let reset_or_enable = module
        .binary(word::BinaryOp::LogicalOr, reset_asserted, enable, span())
        .unwrap();
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), span())
        .unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [
                sensitivity(&mut module, clock, word::Edge::Pos),
                sensitivity(&mut module, reset_signal, word::Edge::Neg),
            ],
            span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let reset_test = cfg.add_block(procedure, span()).unwrap();
    let reset_block = cfg.add_block(procedure, span()).unwrap();
    let data_block = cfg.add_block(procedure, span()).unwrap();
    let hold_block = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_branch(entry, reset_or_enable, reset_test, hold_block, span())
        .unwrap();
    cfg.terminate_branch(reset_test, reset_asserted, reset_block, data_block, span())
        .unwrap();
    cfg.assign(
        reset_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        zero,
        span(),
    )
    .unwrap();
    cfg.assign(
        data_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(reset_block, exit, span()).unwrap();
    cfg.terminate_jump(data_block, exit, span()).unwrap();
    cfg.terminate_jump(hold_block, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let register = lowered
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .unwrap();
    assert_eq!(register.resets.len(), 1);
    assert!(!register.resets[0].active_high);
    assert!(reads_signal(
        &lowered,
        register.resets[0].value,
        reset_signal
    ));
    assert!(register.enable.is_some_and(|enable| depends_on_signal(
        &lowered,
        enable.value,
        enable_signal
    )));
}

#[test]
fn dual_edge_state_uses_phase_banks_with_explicit_hold_feedback() {
    let mut module = WordModule::new("dual_edge");
    let clock = input(&mut module, "clock");
    let reset_signal = input(&mut module, "reset");
    let enable_signal = input(&mut module, "enable");
    let data_signal = input(&mut module, "data");
    let q = output(&mut module, "q");
    let reset = read(&mut module, reset_signal);
    let enable = read(&mut module, enable_signal);
    let data = read(&mut module, data_signal);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), span())
        .unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [
                sensitivity(&mut module, clock, word::Edge::Pos),
                sensitivity(&mut module, clock, word::Edge::Neg),
            ],
            span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let reset_block = cfg.add_block(procedure, span()).unwrap();
    let enable_test = cfg.add_block(procedure, span()).unwrap();
    let update = cfg.add_block(procedure, span()).unwrap();
    let hold = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_branch(entry, reset, reset_block, enable_test, span())
        .unwrap();
    cfg.assign(
        reset_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        zero,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(reset_block, exit, span()).unwrap();
    cfg.terminate_branch(enable_test, enable, update, hold, span())
        .unwrap();
    cfg.assign(
        update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(update, exit, span()).unwrap();
    cfg.terminate_jump(hold, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let registers = lowered
        .operations()
        .iter()
        .filter_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(registers.len(), 2);
    assert_eq!(
        registers
            .iter()
            .map(|register| register.edge)
            .collect::<Vec<_>>(),
        [word::Edge::Pos, word::Edge::Neg]
    );
    assert!(
        registers
            .iter()
            .all(|register| register.enable.is_none() && register.resets.is_empty())
    );
    assert!(
        registers
            .iter()
            .all(|register| depends_on_signal(&lowered, register.d, q))
    );
    assert!(
        registers
            .iter()
            .all(|register| depends_on_signal(&lowered, register.d, reset_signal))
    );
    let q_value = lowered
        .connects()
        .iter()
        .find(|connect| connect.target.signal == q)
        .map(|connect| connect.value)
        .unwrap();
    assert!(matches!(
        lowered.value(q_value).unwrap().kind,
        word::ValueKind::Operation(operation)
            if matches!(
                lowered.operation(operation).unwrap().kind,
                word::OpKind::Mux { cond, .. } if reads_signal(&lowered, cond, clock)
            )
    ));
}

#[test]
fn unknown_value_is_still_a_constant_async_reset() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let reset_signal = input(&mut module, "reset");
    let data_signal = input(&mut module, "data");
    let q = output(&mut module, "q");
    let reset = read(&mut module, reset_signal);
    let data = read(&mut module, data_signal);
    let unknown = module
        .constant(ConstBits::from_bin_str("x").unwrap(), bit(), span())
        .unwrap();
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [
                sensitivity(&mut module, clock, word::Edge::Pos),
                sensitivity(&mut module, reset_signal, word::Edge::Pos),
            ],
            span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let reset_block = cfg.add_block(procedure, span()).unwrap();
    let data_block = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_branch(entry, reset, reset_block, data_block, span())
        .unwrap();
    cfg.assign(
        reset_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        unknown,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(reset_block, exit, span()).unwrap();
    cfg.assign(
        data_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(data_block, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let register = lowered
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register),
            _ => None,
        })
        .unwrap();

    assert_eq!(register.resets.len(), 1);
    assert!(matches!(
        lowered.value(register.resets[0].reset_value).unwrap().kind,
        word::ValueKind::Constant(ref bits) if bits.as_slice() == [opto_ir::BitVal::X]
    ));
}

#[test]
fn synthesis_boundary_materializes_self_xor_as_zero() {
    let mut module = WordModule::new("self_xor");
    let data = input(&mut module, "data");
    let left = read(&mut module, data);
    let right = read(&mut module, data);
    let xor = module
        .binary(word::BinaryOp::BitXor, left, right, span())
        .unwrap();
    let mut analysis = word::KnownBitsAnalysis::new(&module);

    let constant =
        materialize_synthesis_constant(&mut module, &mut analysis, xor, &span()).unwrap();

    assert!(matches!(
        module.value(constant).unwrap().kind,
        word::ValueKind::Constant(ref bits) if bits.as_slice() == [opto_ir::BitVal::Zero]
    ));
}

#[test]
fn typed_event_fact_proves_unreset_target_holds_during_async_reset() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let reset_signal = input(&mut module, "reset");
    let enable_signal = input(&mut module, "enable");
    let data_signal = input(&mut module, "data");
    let q = output(&mut module, "q");
    let r = output(&mut module, "r");
    let reset = read(&mut module, reset_signal);
    let enable = read(&mut module, enable_signal);
    let data = read(&mut module, data_signal);
    let zero = module
        .constant(ConstBits::from_bin_str("0").unwrap(), bit(), span())
        .unwrap();

    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure(
            [
                sensitivity(&mut module, clock, word::Edge::Pos),
                sensitivity(&mut module, reset_signal, word::Edge::Pos),
            ],
            span(),
        )
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let reset_block = cfg.add_block(procedure, span()).unwrap();
    let data_phase = cfg.add_block(procedure, span()).unwrap();
    let update_r = cfg.add_block(procedure, span()).unwrap();
    let hold_r = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();

    cfg.terminate_branch(entry, reset, reset_block, data_phase, span())
        .unwrap();
    cfg.assign(
        reset_block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        zero,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(reset_block, exit, span()).unwrap();
    cfg.assign(
        data_phase,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_branch(data_phase, enable, update_r, hold_r, span())
        .unwrap();
    cfg.assign(
        update_r,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(r),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(update_r, exit, span()).unwrap();
    cfg.terminate_jump(hold_r, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let register_for = |signal| {
        lowered
            .connects()
            .iter()
            .find(|connect| connect.target.signal == signal)
            .and_then(|connect| lowered.value(connect.value))
            .and_then(|value| match value.kind {
                word::ValueKind::Operation(operation) => lowered.operation(operation),
                word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
            })
            .and_then(|operation| match &operation.kind {
                word::OpKind::Register(register) => Some(register),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(register_for(q).resets.len(), 1);
    assert!(register_for(r).resets.is_empty());
    assert!(register_for(r).enable.is_some());
}

#[test]
fn dynamic_target_uses_the_latest_blocking_base() {
    let mut module = WordModule::new("top");
    let vector = WordType::bits(4).unwrap();
    let index_ty = WordType::bits(2).unwrap();
    let base_port = module
        .add_port("base", PortDirection::Input, vector, span())
        .unwrap();
    let index_port = module
        .add_port("index", PortDirection::Input, index_ty, span())
        .unwrap();
    let patch_signal = input(&mut module, "patch");
    let y_port = module
        .add_port("y", PortDirection::Output, vector, span())
        .unwrap();
    let base_signal = module.port(base_port).unwrap().signal;
    let index_signal = module.port(index_port).unwrap().signal;
    let base = read(&mut module, base_signal);
    let index = read(&mut module, index_signal);
    let patch = read(&mut module, patch_signal);
    let y = module.port(y_port).unwrap().signal;
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(y),
        base,
        span(),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(y).with_select(proc::TargetSelect::Dynamic {
            offset: index,
            width: NonZeroU32::MIN,
        }),
        patch,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();
    let lowered = lower(module, cfg).unwrap();
    assert!(lowered.operations().iter().any(|operation| {
        matches!(operation.kind, word::OpKind::DynamicInsert { value, .. } if value == base)
    }));
}

#[test]
fn bounded_dynamic_target_does_not_claim_disjoint_signal_bits() {
    let mut module = WordModule::new("bounded_dynamic_target");
    let lower_port = module
        .add_port(
            "lower",
            PortDirection::Input,
            WordType::bits(4).unwrap(),
            span(),
        )
        .unwrap();
    let output_port = module
        .add_port(
            "y",
            PortDirection::Output,
            WordType::bits(8).unwrap(),
            span(),
        )
        .unwrap();
    let lower_signal = module.port(lower_port).unwrap().signal;
    let output_signal = module.port(output_port).unwrap().signal;
    let lower_value = read(&mut module, lower_signal);
    module
        .connect(
            word::LValue::signal(output_signal).with_range(word::BitRange { msb: 3, lsb: 0 }),
            lower_value,
            span(),
        )
        .unwrap();
    let selector_signal = input(&mut module, "selector");
    let enable_signal = input(&mut module, "enable");
    let data_signal = input(&mut module, "data");
    let selector = read(&mut module, selector_signal);
    let enable = read(&mut module, enable_signal);
    let data = read(&mut module, data_signal);
    let offset_base = module
        .constant(
            ConstBits::from_bin_str("10").unwrap(),
            WordType::bits(2).unwrap(),
            span(),
        )
        .unwrap();
    let offset = module.concat(vec![offset_base, selector], span()).unwrap();
    let upper_default = module
        .constant(
            ConstBits::from_bin_str("0000").unwrap(),
            WordType::bits(4).unwrap(),
            span(),
        )
        .unwrap();
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let update = cfg.add_block(procedure, span()).unwrap();
    let bypass = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        entry,
        AssignmentMode::Blocking,
        ProcTarget::signal(output_signal).with_select(proc::TargetSelect::Static(word::BitRange {
            msb: 7,
            lsb: 4,
        })),
        upper_default,
        span(),
    )
    .unwrap();
    cfg.terminate_branch(entry, enable, update, bypass, span())
        .unwrap();
    cfg.assign(
        update,
        AssignmentMode::Blocking,
        ProcTarget::signal(output_signal).with_select(proc::TargetSelect::Dynamic {
            offset,
            width: NonZeroU32::MIN,
        }),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(update, exit, span()).unwrap();
    cfg.terminate_jump(bypass, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    assert!(lowered.connects().iter().any(|connect| {
        connect.target.signal == output_signal
            && matches!(connect.target.range, Some(range) if range.lsb == 0 && range.msb == 3)
    }));
    assert!(lowered.connects().iter().any(|connect| {
        connect.target.signal == output_signal
            && matches!(connect.target.range, Some(range) if range.lsb == 4 && range.msb == 7)
    }));
}

#[test]
fn signed_whole_assignment_splits_into_unsigned_partial_state() {
    let mut module = WordModule::new("top");
    let signed = WordType::new(4, true, word::LogicStateKind::FourState).unwrap();
    let output_port = module
        .add_port("y", PortDirection::Output, signed, span())
        .unwrap();
    let output_signal = module.port(output_port).unwrap().signal;
    let base = module
        .constant(ConstBits::from_bin_str("1010").unwrap(), signed, span())
        .unwrap();
    let patch = module
        .constant(ConstBits::from_bin_str("1").unwrap(), bit(), span())
        .unwrap();
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(output_signal),
        base,
        span(),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(output_signal).with_select(proc::TargetSelect::Static(word::BitRange {
            msb: 0,
            lsb: 0,
        })),
        patch,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let upper = lowered
        .connects()
        .iter()
        .find(|connect| {
            connect.target.signal == output_signal
                && matches!(connect.target.range, Some(range) if range.lsb == 1 && range.msb == 3)
        })
        .expect("upper signed target fragment is committed");
    let ty = lowered.value(upper.value).unwrap().ty;
    assert_eq!(ty.width(), 3);
    assert!(!ty.is_signed());
}

#[test]
fn reversed_static_target_preserves_assignment_bit_order() {
    let mut module = WordModule::new("top");
    let vector = WordType::bits(4).unwrap();
    let input_port = module
        .add_port("a", PortDirection::Input, vector, span())
        .unwrap();
    let output_port = module
        .add_port("y", PortDirection::Output, vector, span())
        .unwrap();
    let input_signal = module.port(input_port).unwrap().signal;
    let output_signal = module.port(output_port).unwrap().signal;
    let value = read(&mut module, input_signal);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Combinational, span())
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::signal(output_signal).with_select(proc::TargetSelect::Static(word::BitRange {
            msb: 0,
            lsb: 3,
        })),
        value,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();

    let lowered = lower(module, cfg).unwrap();
    let assigned = lowered
        .connects()
        .iter()
        .find(|connect| connect.target.signal == output_signal)
        .and_then(|connect| lowered.value(connect.value))
        .and_then(|value| match &value.kind {
            word::ValueKind::Operation(operation) => lowered.operation(*operation),
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
        })
        .and_then(|operation| match &operation.kind {
            word::OpKind::Concat { parts } => Some(parts),
            _ => None,
        })
        .expect("reversed assignment is represented by one bit-ordering concatenation");
    assert_eq!(assigned.len(), 4);
    for (&part, expected_lsb) in assigned.iter().zip(0..4) {
        assert!(matches!(
            lowered.value(part).map(|value| &value.kind),
            Some(word::ValueKind::Operation(operation))
                if matches!(
                    lowered.operation(*operation).map(|operation| &operation.kind),
                    Some(word::OpKind::Extract { value: source, lsb, width })
                        if *source == value && *lsb == expected_lsb && width.get() == 1
                )
        ));
    }
}

#[test]
fn latch_preserves_conditional_enable() {
    let mut module = WordModule::new("top");
    let enable_signal = input(&mut module, "en");
    let data_signal = input(&mut module, "d");
    let q = output(&mut module, "q");
    let enable = read(&mut module, enable_signal);
    let data = read(&mut module, data_signal);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_combinational_procedure(ProcedureKind::Latch, span())
        .unwrap();
    let entry = cfg.add_block(procedure, span()).unwrap();
    let update = cfg.add_block(procedure, span()).unwrap();
    let hold = cfg.add_block(procedure, span()).unwrap();
    let exit = cfg.add_block(procedure, span()).unwrap();
    cfg.terminate_branch(entry, enable, update, hold, span())
        .unwrap();
    cfg.assign(
        update,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        data,
        span(),
    )
    .unwrap();
    cfg.terminate_jump(update, exit, span()).unwrap();
    cfg.terminate_jump(hold, exit, span()).unwrap();
    cfg.terminate_return(exit, span()).unwrap();
    let lowered = lower(module, cfg).unwrap();
    let latch = lowered
        .operations()
        .iter()
        .find_map(|operation| match &operation.kind {
            word::OpKind::Latch(latch) => Some(latch),
            _ => None,
        })
        .unwrap();
    assert!(reads_signal(&lowered, latch.d, data_signal));
    assert!(latch.enable.active_high && reads_signal(&lowered, latch.enable.value, enable_signal));
}

#[test]
fn blocking_memory_write_forwards_and_ports_keep_source_priority() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let address_signal = input(&mut module, "addr");
    let first_signal = input(&mut module, "first");
    let second_signal = input(&mut module, "second");
    let q = output(&mut module, "q");
    let address = read(&mut module, address_signal);
    let first = read(&mut module, first_signal);
    let second = read(&mut module, second_signal);
    let memory = module
        .add_memory("mem", bit(), NonZeroU32::new(2).unwrap(), span())
        .unwrap();
    let read_data = module.add_wire("mem_read", bit(), span()).unwrap();
    module
        .add_memory_read_port(word::MemoryReadPort {
            memory,
            address,
            data: read_data,
            timing: word::MemoryReadTiming::Asynchronous,
            read_during_write: word::ReadDuringWrite::NewData,
            source: span(),
        })
        .unwrap();
    let read_value = read(&mut module, read_data);
    let mut cfg = ProcBuilder::new();
    let procedure = cfg
        .add_clocked_procedure([sensitivity(&mut module, clock, word::Edge::Pos)], span())
        .unwrap();
    let block = cfg.add_block(procedure, span()).unwrap();
    cfg.assign(
        block,
        AssignmentMode::Blocking,
        ProcTarget::memory(memory, address),
        first,
        span(),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Nonblocking,
        ProcTarget::signal(q),
        read_value,
        span(),
    )
    .unwrap();
    cfg.assign(
        block,
        AssignmentMode::Nonblocking,
        ProcTarget::memory(memory, address),
        second,
        span(),
    )
    .unwrap();
    cfg.terminate_return(block, span()).unwrap();
    let mut lowered = lower(module, cfg).unwrap();
    assert_eq!(
        lowered
            .memory_write_ports()
            .iter()
            .map(|port| port.priority)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    let q_d = lowered
        .connects()
        .iter()
        .find(|connect| connect.target.signal == q)
        .and_then(|connect| lowered.value(connect.value))
        .and_then(|value| match value.kind {
            word::ValueKind::Operation(operation) => lowered.operation(operation),
            _ => None,
        })
        .and_then(|operation| match &operation.kind {
            word::OpKind::Register(register) => Some(register.d),
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        lowered.value(q_d).unwrap().kind,
        word::ValueKind::Operation(operation)
            if matches!(lowered.operation(operation).unwrap().kind, word::OpKind::Mux { .. })
    ));

    crate::planning::memory::lower_memories_to_register_banks(&mut lowered).unwrap();
    assert!(lowered.memories().is_empty());
    assert!(lowered.memory_read_ports().is_empty());
    assert!(lowered.memory_write_ports().is_empty());
    assert!(lowered.signal_id("mem$0").is_some());
    assert!(lowered.signal_id("mem$1").is_some());
}

#[test]
fn same_clock_procedures_form_deterministic_memory_write_ports() {
    let mut module = WordModule::new("top");
    let clock = input(&mut module, "clk");
    let address_a = input(&mut module, "address_a");
    let address_b = input(&mut module, "address_b");
    let data_a = input(&mut module, "data_a");
    let data_b = input(&mut module, "data_b");
    let address_a = read(&mut module, address_a);
    let address_b = read(&mut module, address_b);
    let data_a = read(&mut module, data_a);
    let data_b = read(&mut module, data_b);
    let memory = module
        .add_memory("mem", bit(), NonZeroU32::new(2).unwrap(), span())
        .unwrap();
    let mut cfg = ProcBuilder::new();
    for (address, data) in [(address_a, data_a), (address_b, data_b)] {
        let procedure = cfg
            .add_clocked_procedure([sensitivity(&mut module, clock, word::Edge::Pos)], span())
            .unwrap();
        let block = cfg.add_block(procedure, span()).unwrap();
        cfg.assign(
            block,
            AssignmentMode::Nonblocking,
            ProcTarget::memory(memory, address),
            data,
            span(),
        )
        .unwrap();
        cfg.terminate_return(block, span()).unwrap();
    }

    let mut lowered = lower(module, cfg).unwrap();
    let ports = lowered.memory_write_ports();
    assert_eq!(
        ports.iter().map(|port| port.priority).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_ne!(ports[0].clock.value, ports[1].clock.value);
    assert_eq!(
        lowered.value(ports[0].clock.value).unwrap().kind,
        lowered.value(ports[1].clock.value).unwrap().kind
    );

    crate::planning::memory::lower_memories_to_register_banks(&mut lowered).unwrap();
    assert!(lowered.memories().is_empty());
    assert!(lowered.memory_write_ports().is_empty());
}
