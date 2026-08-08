// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use hashbrown::{HashMap, HashSet};
use opto_ir::{BitVal, ConstBits, logic::Lit, word};
use std::collections::BTreeMap;

use super::symbolic::{SymbolicError, WordLogicEncoder};
use crate::word::signal_driver::SignalDriverIndex;
use materialize::materialize_plans;

mod materialize;

const MAX_STATES: usize = 64;
const MAX_STATE_WIDTH: u32 = 128;
const MAX_TRANSITION_VALUES: usize = 4096;

#[derive(Debug, Clone)]
struct DerivedFsm {
    register_operation: word::OpId,
    register: word::RegisterOp,
    register_result: word::ValueId,
    state_signal: word::SignalId,
    state_type: word::WordType,
    source: word::SourceSpan,
    states: Vec<ConstBits>,
    state_classes: Box<[usize]>,
    representatives: Box<[usize]>,
    reset_values: Box<[ConstBits]>,
    transition_order: Box<[word::ValueId]>,
    constant_values: Box<[(word::ValueId, ConstBits)]>,
}

#[derive(Debug)]
struct FsmCatalog {
    machines: Box<[DerivedFsm]>,
}

#[derive(Debug, Clone)]
struct FsmAnalysisCandidate {
    machine: DerivedFsm,
    reset_states: Box<[usize]>,
    allow_state_merging: bool,
}

#[derive(Debug, Clone, Copy)]
struct FsmObservationCandidate {
    value: word::ValueId,
    excluded_for: Option<word::ValueId>,
}

struct FsmObservationRoots {
    values: Box<[word::ValueId]>,
    supports_state_merging: bool,
}

#[derive(Debug, Clone, Copy)]
struct FsmDependencyUser {
    value: word::ValueId,
    crosses_unresolved_signal_driver: bool,
}

#[derive(Debug)]
struct FsmDependencyIndex {
    users: opto_core::PackedRows<FsmDependencyUser>,
    signal_references: opto_core::PackedRows<word::ValueId>,
}

struct SymbolicBehavior<'module> {
    encoder: WordLogicEncoder<'module>,
    observations: Vec<Vec<Lit>>,
    next_values: Vec<Vec<Lit>>,
    successors: Vec<Box<[usize]>>,
}

#[derive(Debug, Clone)]
struct FsmPlan {
    machine: DerivedFsm,
    codes: Vec<ConstBits>,
    encoded_type: word::WordType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FsmObjective {
    Area,
    Timing,
}

pub(crate) fn optimize_derived_fsms_in_regions(
    module: &mut word::WordModule,
    operation_regions: &[Option<crate::RegionRowId>],
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<usize, crate::SynthError> {
    if operation_regions.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "regional FSM optimization has incomplete operation ownership",
        ));
    }
    let mut catalog = derive_catalog(module, runtime)?;
    catalog.machines = catalog
        .machines
        .into_vec()
        .into_iter()
        .filter(|machine| machine_is_region_private(module, machine, operation_regions))
        .collect();
    let plans = plan_catalog(catalog, |machine| {
        machine_objective(module, machine, timing, port_bindings)
    })?;
    materialize_plans(module, &plans)?;
    Ok(plans.len())
}

fn machine_is_region_private(
    module: &word::WordModule,
    machine: &DerivedFsm,
    operation_regions: &[Option<crate::RegionRowId>],
) -> bool {
    let owner = operation_regions[machine.register_operation.index()];
    owner.is_some()
        && machine.transition_order.iter().all(|&value| {
            let Some(word::ValueKind::Operation(operation)) =
                module.value(value).map(|value| &value.kind)
            else {
                return true;
            };
            operation_regions[operation.index()] == owner
        })
}

#[cfg(test)]
fn optimize_with_objective(
    module: &mut word::WordModule,
    objective: FsmObjective,
) -> Result<usize, crate::SynthError> {
    let catalog = derive_catalog(module, crate::test_runtime())?;
    let plans = plan_catalog(catalog, |_| objective)?;
    materialize_plans(module, &plans)?;
    module.compact_netlist().map_err(crate::SynthError::from)?;
    module.validate().map_err(crate::SynthError::from)?;
    Ok(plans.len())
}

fn derive_catalog(
    module: &word::WordModule,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<FsmCatalog, crate::SynthError> {
    let uses = crate::word::uses::value_use_counts(module)?;
    let signal_drivers = SignalDriverIndex::new(module)?;
    let dependency_index = FsmDependencyIndex::build(module, &signal_drivers)?;
    let mut facts = word::KnownBitsAnalysis::new(module);
    let mut driver_counts = vec![0u32; module.signals().len()];
    for connect in module.connects() {
        let count = driver_counts
            .get_mut(connect.target.signal.index())
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "connect targets unknown signal {:?}",
                    connect.target.signal
                ))
            })?;
        *count = count.checked_add(1).ok_or_else(|| {
            crate::SynthError::capacity("signal driver count exceeds 32-bit capacity")
        })?;
    }
    let observation_candidates = fsm_observation_candidates(module, &uses)?;
    let mut candidates = Vec::new();
    for connect in module.connects() {
        if connect.target.range.is_some() || connect.target.dynamic.is_some() {
            continue;
        }
        let Some(value) = module.value(connect.value) else {
            return Err(crate::SynthError::invariant(format!(
                "FSM candidate connect references unknown value {:?}",
                connect.value
            )));
        };
        let word::ValueKind::Operation(operation_id) = value.kind else {
            continue;
        };
        let Some(operation) = module.operation(operation_id) else {
            return Err(crate::SynthError::invariant(format!(
                "FSM candidate references unknown operation {operation_id:?}"
            )));
        };
        let word::OpKind::Register(register) = &operation.kind else {
            continue;
        };
        let Some(signal) = module.signal(connect.target.signal) else {
            return Err(crate::SynthError::invariant(format!(
                "FSM candidate targets unknown signal {:?}",
                connect.target.signal
            )));
        };
        let state_target = match signal.kind {
            word::SignalKind::Wire | word::SignalKind::Register => signal.name.is_some(),
            word::SignalKind::Port(port) => module
                .port(port)
                .is_some_and(|port| port.direction == word::PortDirection::Output),
            word::SignalKind::ProcessLocal => false,
        };
        if !state_target
            || signal.ty != value.ty
            || signal.ty.width() > MAX_STATE_WIDTH
            || register.resets.is_empty()
            || uses.get(connect.value.index()).copied() != Some(1)
            || driver_counts[connect.target.signal.index()] != 1
        {
            continue;
        }
        if !transition_cone_within_budget(module, register.d)? {
            continue;
        }

        let mut states = Vec::new();
        let mut transition_order = Vec::new();
        let mut constant_values = Vec::new();
        if !collect_states(
            module,
            register.d,
            connect.target.signal,
            &mut facts,
            TransitionCollection {
                states: &mut states,
                order: &mut transition_order,
                constants: &mut constant_values,
            },
        )? {
            continue;
        }
        let mut reset_values = Vec::with_capacity(register.resets.len());
        for reset in &register.resets {
            let Some(bits) = facts.constant(module, reset.reset_value) else {
                states.clear();
                break;
            };
            if !is_boolean(&bits) {
                states.clear();
                break;
            }
            push_unique(&mut states, bits.clone());
            reset_values.push(bits);
        }
        if states.is_empty()
            || states.len() > MAX_STATES
            || reset_values.len() != register.resets.len()
        {
            continue;
        }
        order_states(&mut states, &reset_values[0]);
        let reset_states = reset_values
            .iter()
            .map(|reset| {
                states
                    .iter()
                    .position(|state| state == reset)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "FSM reset state is absent from its finite state set",
                        )
                    })
            })
            .collect::<Result<Box<[_]>, crate::SynthError>>()?;
        candidates.push(FsmAnalysisCandidate {
            machine: DerivedFsm {
                register_operation: operation_id,
                register: register.clone(),
                register_result: operation.result,
                state_signal: connect.target.signal,
                state_type: signal.ty,
                source: operation.source.clone(),
                states,
                state_classes: Box::new([]),
                representatives: Box::new([]),
                reset_values: reset_values.into_boxed_slice(),
                transition_order: transition_order.into_boxed_slice(),
                constant_values: constant_values.into_boxed_slice(),
            },
            reset_states,
            allow_state_merging: !matches!(
                signal.kind,
                word::SignalKind::Port(port)
                    if module.port(port).is_some_and(|port| matches!(
                        port.direction,
                        word::PortDirection::Output | word::PortDirection::Inout
                    ))
            ),
        });
    }
    let analyzed = runtime.analyze_indexed(candidates.len(), |index| {
        let candidate = &candidates[index];
        let mut machine = candidate.machine.clone();
        let observation_roots = fsm_observation_roots(
            module,
            machine.state_signal,
            machine.register_result,
            &observation_candidates,
            &dependency_index,
        )?;
        let mut behavior =
            derive_symbolic_behavior(module, &machine, &observation_roots.values, &signal_drivers)?;
        if let Some(current) = &behavior {
            let retained = retain_reset_reachable_states(
                &mut machine.states,
                &candidate.reset_states,
                &current.successors,
            )?;
            if !retained {
                return Ok::<_, crate::SynthError>(None);
            }
            if machine.states.len() != current.next_values.len() {
                behavior = derive_symbolic_behavior(
                    module,
                    &machine,
                    &observation_roots.values,
                    &signal_drivers,
                )?;
            }
        }
        minimize_states(
            &mut machine,
            behavior.as_mut(),
            candidate.allow_state_merging && observation_roots.supports_state_merging,
        )?;
        Ok::<_, crate::SynthError>(Some(machine))
    })?;
    Ok(FsmCatalog {
        machines: analyzed.into_iter().flatten().collect(),
    })
}

fn transition_cone_within_budget(
    module: &word::WordModule,
    root: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let mut visited = HashSet::new();
    let mut pending = vec![root];
    while let Some(value_id) = pending.pop() {
        if !visited.insert(value_id) {
            continue;
        }
        if visited.len() > MAX_TRANSITION_VALUES {
            return Ok(false);
        }
        let value = module.value(value_id).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "FSM transition cone references unknown value {value_id:?}"
            ))
        })?;
        if let word::ValueKind::Operation(operation_id) = value.kind {
            let operation = module.operation(operation_id).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "FSM transition cone references unknown operation {operation_id:?}"
                ))
            })?;
            pending.extend(crate::word::operation_inputs(&operation.kind));
        }
    }
    Ok(true)
}

fn fsm_observation_candidates(
    module: &word::WordModule,
    uses: &[u32],
) -> Result<Box<[FsmObservationCandidate]>, crate::SynthError> {
    let estimated = module
        .connects()
        .len()
        .saturating_add(module.operations().len());
    let mut candidates = Vec::<FsmObservationCandidate>::with_capacity(estimated);
    let mut candidate_indices = HashMap::<word::ValueId, usize>::with_capacity(estimated);
    let mut add_candidate = |value: word::ValueId, excluded_for: Option<word::ValueId>| {
        if let Some(&index) = candidate_indices.get(&value) {
            let candidate = &mut candidates[index];
            if candidate.excluded_for != excluded_for {
                candidate.excluded_for = None;
            }
        } else {
            candidate_indices.insert(value, candidates.len());
            candidates.push(FsmObservationCandidate {
                value,
                excluded_for,
            });
        }
    };
    for connect in module.connects() {
        let signal = module.signal(connect.target.signal).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "FSM observation targets unknown signal {:?}",
                connect.target.signal
            ))
        })?;
        let observable = match signal.kind {
            word::SignalKind::Port(port) => module.port(port).is_some_and(|port| {
                matches!(
                    port.direction,
                    word::PortDirection::Output | word::PortDirection::Inout
                )
            }),
            word::SignalKind::Wire
            | word::SignalKind::Register
            | word::SignalKind::ProcessLocal => false,
        };
        if observable {
            add_candidate(connect.value, Some(connect.value));
            if let Some(dynamic) = connect.target.dynamic {
                add_candidate(dynamic.offset, Some(connect.value));
            }
        }
    }
    for connection in module
        .instances()
        .iter()
        .flat_map(|instance| &instance.connections)
    {
        add_candidate(connection.value, None);
    }
    for port in module.memory_read_ports() {
        add_candidate(port.address, None);
        if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = port.timing {
            add_candidate(clock.value, None);
            if let Some(enable) = enable {
                add_candidate(enable.value, None);
            }
        }
    }
    for port in module.memory_write_ports() {
        for value in [port.address, port.data, port.clock.value] {
            add_candidate(value, None);
        }
        if let Some(enable) = port.enable {
            add_candidate(enable.value, None);
        }
        if let Some(mask) = port.mask {
            add_candidate(mask.value, None);
        }
    }
    for operation in module.operations() {
        if uses.get(operation.result.index()).copied().unwrap_or(0) == 0 {
            continue;
        }
        match &operation.kind {
            word::OpKind::Register(register) => {
                for value in crate::word::operation_inputs(&operation.kind) {
                    add_candidate(value, Some(operation.result));
                }
                add_candidate(register.clock, None);
            }
            word::OpKind::Latch(_) => {
                for value in crate::word::operation_inputs(&operation.kind) {
                    add_candidate(value, None);
                }
            }
            _ => {}
        }
    }
    Ok(candidates.into_boxed_slice())
}

fn fsm_observation_roots(
    module: &word::WordModule,
    state: word::SignalId,
    register_result: word::ValueId,
    candidates: &[FsmObservationCandidate],
    dependencies: &FsmDependencyIndex,
) -> Result<FsmObservationRoots, crate::SynthError> {
    let mut states = vec![0u8; module.values().len()];
    let mut pending = Vec::new();
    for &reference in dependencies.signal_references.row(state.index()) {
        states[reference.index()] = 1;
        pending.push(reference);
    }
    while let Some(value) = pending.pop() {
        let state = states[value.index()];
        for user in dependencies.users.row(value.index()) {
            let next_state = if state == 2 || user.crosses_unresolved_signal_driver {
                2
            } else {
                1
            };
            let current = states.get_mut(user.value.index()).ok_or_else(|| {
                crate::SynthError::invariant("FSM dependency user is outside the value arena")
            })?;
            if next_state > *current {
                *current = next_state;
                pending.push(user.value);
            }
        }
    }
    let mut roots = Vec::new();
    let mut supports_state_merging = true;
    for candidate in candidates {
        if candidate.excluded_for == Some(register_result) {
            continue;
        }
        let root = candidate.value;
        let dependency = states.get(root.index()).copied().ok_or_else(|| {
            crate::SynthError::invariant("FSM observation root is outside the value arena")
        })?;
        if dependency != 0 {
            roots.push(root);
            supports_state_merging &= dependency != 2;
        }
    }
    Ok(FsmObservationRoots {
        values: roots.into_boxed_slice(),
        supports_state_merging,
    })
}

impl FsmDependencyIndex {
    fn build(
        module: &word::WordModule,
        signal_drivers: &SignalDriverIndex,
    ) -> Result<Self, crate::SynthError> {
        let mut user_entries = Vec::new();
        let mut reference_entries = Vec::new();
        for (index, value) in module.values().iter().enumerate() {
            let value_id = word::ValueId::from_index(index)
                .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
            match &value.kind {
                word::ValueKind::Constant(_) => {}
                word::ValueKind::Operation(operation) => {
                    let operation = module.operation(*operation).ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "FSM dependency index references unknown operation {operation:?}"
                        ))
                    })?;
                    if !matches!(
                        operation.kind,
                        word::OpKind::Register(_) | word::OpKind::Latch(_)
                    ) {
                        user_entries.extend(
                            crate::word::operation_inputs(&operation.kind)
                                .into_iter()
                                .map(|input| {
                                    (
                                        input.index(),
                                        FsmDependencyUser {
                                            value: value_id,
                                            crosses_unresolved_signal_driver: false,
                                        },
                                    )
                                }),
                        );
                    }
                }
                word::ValueKind::Signal(reference) => {
                    reference_entries.push((reference.signal.index(), value_id));
                    if let Some(drivers) = signal_drivers.reference_drivers(*reference) {
                        user_entries.extend(drivers.into_iter().map(|driver| {
                            (
                                driver.index(),
                                FsmDependencyUser {
                                    value: value_id,
                                    crosses_unresolved_signal_driver: false,
                                },
                            )
                        }));
                    } else {
                        user_entries.extend(signal_drivers.values(reference.signal).map(
                            |driver| {
                                (
                                    driver.index(),
                                    FsmDependencyUser {
                                        value: value_id,
                                        crosses_unresolved_signal_driver: true,
                                    },
                                )
                            },
                        ));
                    }
                }
            }
        }
        let users = opto_core::PackedRows::try_from_entries(module.values().len(), user_entries)
            .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
        let signal_references =
            opto_core::PackedRows::try_from_entries(module.signals().len(), reference_entries)
                .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
        Ok(Self {
            users,
            signal_references,
        })
    }
}

fn minimize_states(
    machine: &mut DerivedFsm,
    behavior: Option<&mut SymbolicBehavior<'_>>,
    allow_merging: bool,
) -> Result<(), crate::SynthError> {
    let state_count = machine.states.len();
    let mut classes = (0..state_count).collect::<Vec<_>>();
    if allow_merging
        && state_count > 1
        && let Some(behavior) = behavior
    {
        classes = group_signatures(behavior.observations.iter().cloned());
        loop {
            let signatures = behavior
                .next_values
                .iter()
                .map(|next| behavior.encoder.partition(next, &machine.states, &classes))
                .collect::<Result<Vec<_>, _>>();
            let signatures = match signatures {
                Ok(signatures) => signatures,
                Err(SymbolicError::Unsupported) => {
                    classes = (0..state_count).collect();
                    break;
                }
                Err(SymbolicError::Synthesis(error)) => return Err(error),
            };
            let refined =
                group_signatures(classes.iter().copied().zip(signatures).collect::<Vec<_>>());
            if refined == classes {
                break;
            }
            classes = refined;
        }
    }
    let class_count = classes
        .iter()
        .copied()
        .max()
        .map_or(0, |maximum| maximum + 1);
    let mut representatives = vec![usize::MAX; class_count];
    for (state, &class) in classes.iter().enumerate() {
        let representative = representatives.get_mut(class).ok_or_else(|| {
            crate::SynthError::invariant("FSM minimization produced an invalid class")
        })?;
        if *representative == usize::MAX {
            *representative = state;
        }
    }
    machine.state_classes = classes.into_boxed_slice();
    machine.representatives = representatives.into_boxed_slice();
    Ok(())
}

fn derive_symbolic_behavior<'module>(
    module: &'module word::WordModule,
    machine: &DerivedFsm,
    observations: &[word::ValueId],
    signal_drivers: &'module SignalDriverIndex,
) -> Result<Option<SymbolicBehavior<'module>>, crate::SynthError> {
    let mut encoder = WordLogicEncoder::with_signal_drivers(module, signal_drivers);
    let mut observation_values = Vec::with_capacity(machine.states.len());
    let mut next_values = Vec::with_capacity(machine.states.len());
    for state in &machine.states {
        if let Err(error) = encoder.begin_state(machine.state_signal, state) {
            return symbolic_eligibility(error);
        }
        match encoder.values(observations) {
            Ok(values) => observation_values.push(values),
            Err(error) => return symbolic_eligibility(error),
        }
        match encoder.register_next(&machine.register, machine.state_signal) {
            Ok(values) => next_values.push(values),
            Err(error) => return symbolic_eligibility(error),
        }
    }
    let mut successors = Vec::with_capacity(next_values.len());
    for next in &next_values {
        let mut targets = Vec::new();
        for (target, state) in machine.states.iter().enumerate() {
            match encoder.equals_constant(next, state) {
                Ok(Lit::FALSE) => {}
                Ok(_) => targets.push(target),
                Err(error) => return symbolic_eligibility(error),
            }
        }
        successors.push(targets.into_boxed_slice());
    }
    Ok(Some(SymbolicBehavior {
        encoder,
        observations: observation_values,
        next_values,
        successors,
    }))
}

fn symbolic_eligibility<T>(error: SymbolicError) -> Result<Option<T>, crate::SynthError> {
    match error {
        SymbolicError::Unsupported => Ok(None),
        SymbolicError::Synthesis(error) => Err(error),
    }
}

fn group_signatures<K: Ord>(signatures: impl IntoIterator<Item = K>) -> Vec<usize> {
    let mut classes = BTreeMap::new();
    signatures
        .into_iter()
        .map(|signature| {
            let next_class = classes.len();
            *classes.entry(signature).or_insert(next_class)
        })
        .collect()
}

fn plan_catalog(
    catalog: FsmCatalog,
    mut objective: impl FnMut(&DerivedFsm) -> FsmObjective,
) -> Result<Vec<FsmPlan>, crate::SynthError> {
    let mut plans = Vec::new();
    for machine in catalog.machines.into_vec() {
        let objective = objective(&machine);
        let class_states = machine
            .representatives
            .iter()
            .map(|&state| machine.states[state].clone())
            .collect::<Vec<_>>();
        let (encoded_width, codes) =
            choose_encoding(&class_states, machine.state_type.width(), objective)?;
        if encoded_width >= machine.state_type.width()
            && machine.representatives.len() == machine.states.len()
        {
            continue;
        }
        let encoded_type = word::WordType::new(encoded_width, false, machine.state_type.state())
            .map_err(crate::SynthError::from)?;
        plans.push(FsmPlan {
            machine,
            codes,
            encoded_type,
        });
    }
    Ok(plans)
}

fn machine_objective(
    module: &word::WordModule,
    machine: &DerivedFsm,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
) -> FsmObjective {
    let clock_signal = module
        .value(machine.register.clock)
        .and_then(|value| match value.kind {
            word::ValueKind::Signal(reference) if reference.width() == 1 => Some(reference.signal),
            _ => None,
        });
    if clock_signal
        .and_then(|signal| module.ports().iter().position(|port| port.signal == signal))
        .and_then(|port| port_bindings.get(port))
        .is_some_and(|port| timing.minimum_clock_period_on(port).is_some())
    {
        FsmObjective::Timing
    } else {
        FsmObjective::Area
    }
}

fn retain_reset_reachable_states(
    states: &mut Vec<ConstBits>,
    reset_states: &[usize],
    successors: &[Box<[usize]>],
) -> Result<bool, crate::SynthError> {
    if successors.len() != states.len() {
        return Err(crate::SynthError::invariant(
            "symbolic FSM transition relation has the wrong source-state count",
        ));
    }
    let mut reachable = vec![false; states.len()];
    let mut pending = Vec::new();
    for &reset in reset_states {
        let Some(reachable) = reachable.get_mut(reset) else {
            return Err(crate::SynthError::invariant(
                "FSM reset references an unknown finite state",
            ));
        };
        if !*reachable {
            *reachable = true;
            pending.push(reset);
        }
    }
    while let Some(state) = pending.pop() {
        let targets = successors.get(state).ok_or_else(|| {
            crate::SynthError::invariant("symbolic FSM transition relation has no source-state row")
        })?;
        for &target in targets {
            let Some(reachable) = reachable.get_mut(target) else {
                return Err(crate::SynthError::invariant(
                    "symbolic FSM transition relation references an unknown target state",
                ));
            };
            if !*reachable {
                *reachable = true;
                pending.push(target);
            }
        }
    }
    *states = std::mem::take(states)
        .into_iter()
        .zip(reachable)
        .filter_map(|(state, reachable)| reachable.then_some(state))
        .collect();
    Ok(!states.is_empty())
}

struct TransitionCollection<'a> {
    states: &'a mut Vec<ConstBits>,
    order: &'a mut Vec<word::ValueId>,
    constants: &'a mut Vec<(word::ValueId, ConstBits)>,
}

fn collect_states(
    module: &word::WordModule,
    value_id: word::ValueId,
    state_signal: word::SignalId,
    facts: &mut word::KnownBitsAnalysis,
    collection: TransitionCollection<'_>,
) -> Result<bool, crate::SynthError> {
    let TransitionCollection {
        states,
        order,
        constants,
    } = collection;
    let mut visited = HashSet::new();
    let mut active = HashSet::new();
    let mut pending = vec![(value_id, false)];
    while let Some((value_id, exiting)) = pending.pop() {
        if exiting {
            if active.remove(&value_id) {
                visited.insert(value_id);
                order.push(value_id);
            }
            continue;
        }
        if visited.contains(&value_id) {
            continue;
        }
        if !active.insert(value_id) {
            return Ok(false);
        }
        if visited.len().saturating_add(active.len()) > MAX_TRANSITION_VALUES {
            return Ok(false);
        }
        let Some(value) = module.value(value_id) else {
            return Err(crate::SynthError::invariant(format!(
                "FSM transition references unknown value {value_id:?}"
            )));
        };
        if let word::ValueKind::Constant(bits) = &value.kind {
            if !is_boolean(bits) {
                return Ok(false);
            }
            push_unique(states, bits.clone());
            constants.push((value_id, bits.clone()));
            active.remove(&value_id);
            visited.insert(value_id);
            order.push(value_id);
            if states.len() > MAX_STATES {
                return Ok(false);
            }
            continue;
        }
        if matches!(
            value.kind,
            word::ValueKind::Signal(reference)
                if reference.signal == state_signal
                    && reference.lsb == 0
                    && reference.width() == value.ty.width()
        ) {
            active.remove(&value_id);
            visited.insert(value_id);
            order.push(value_id);
            continue;
        }
        let word::ValueKind::Operation(operation_id) = value.kind else {
            return Ok(false);
        };
        let Some(operation) = module.operation(operation_id) else {
            return Err(crate::SynthError::invariant(format!(
                "FSM transition references unknown operation {operation_id:?}"
            )));
        };
        let children = match &operation.kind {
            word::OpKind::Mux {
                then_value,
                else_value,
                ..
            } => [Some(*then_value), Some(*else_value)],
            word::OpKind::Cast {
                value: operand,
                target,
                ..
            } => {
                let Some(operand_value) = module.value(*operand) else {
                    return Err(crate::SynthError::invariant(format!(
                        "FSM cast references unknown value {operand:?}"
                    )));
                };
                if target.width() != operand_value.ty.width() {
                    return Ok(false);
                }
                [Some(*operand), None]
            }
            _ => {
                let Some(bits) = facts.constant(module, value_id) else {
                    return Ok(false);
                };
                if !is_boolean(&bits) {
                    return Ok(false);
                }
                push_unique(states, bits.clone());
                constants.push((value_id, bits));
                active.remove(&value_id);
                visited.insert(value_id);
                order.push(value_id);
                if states.len() > MAX_STATES {
                    return Ok(false);
                }
                continue;
            }
        };
        pending.push((value_id, true));
        pending.extend(
            children
                .into_iter()
                .rev()
                .flatten()
                .map(|child| (child, false)),
        );
    }
    Ok(true)
}

fn order_states(states: &mut [ConstBits], primary_reset: &ConstBits) {
    states.sort_by(|left, right| {
        left.as_slice()
            .iter()
            .copied()
            .map(bit_rank)
            .cmp(right.as_slice().iter().copied().map(bit_rank))
    });
    if let Some(index) = states.iter().position(|state| state == primary_reset) {
        states.swap(0, index);
    }
}

fn choose_encoding(
    states: &[ConstBits],
    original_width: u32,
    objective: FsmObjective,
) -> Result<(u32, Vec<ConstBits>), crate::SynthError> {
    let zero = states.iter().position(is_zero);
    let one_hot_width = u32::try_from(states.len() - usize::from(zero.is_some()))
        .map_err(|_| crate::SynthError::capacity("FSM state count exceeds 32-bit capacity"))?
        .max(1);
    let binary_width = usize::BITS - states.len().saturating_sub(1).leading_zeros();
    let binary_width = binary_width.max(1);
    let use_one_hot = objective == FsmObjective::Timing && one_hot_width < original_width;
    let width = if use_one_hot {
        one_hot_width
    } else {
        binary_width
    };
    let mut next_hot_bit = 0u32;
    let mut codes = Vec::with_capacity(states.len());
    for (index, state) in states.iter().enumerate() {
        let bits = if use_one_hot {
            if is_zero(state) {
                boolean_constant(width, None)?
            } else {
                let bits = boolean_constant(width, Some(next_hot_bit))?;
                next_hot_bit += 1;
                bits
            }
        } else {
            binary_constant(width, index)?
        };
        codes.push(bits);
    }
    Ok((width, codes))
}

fn push_unique(states: &mut Vec<ConstBits>, state: ConstBits) {
    if !states.contains(&state) {
        states.push(state);
    }
}

fn is_boolean(bits: &ConstBits) -> bool {
    bits.as_slice()
        .iter()
        .all(|bit| matches!(bit, BitVal::Zero | BitVal::One))
}

fn is_zero(bits: &ConstBits) -> bool {
    bits.as_slice().iter().all(|&bit| bit == BitVal::Zero)
}

fn bit_rank(bit: BitVal) -> u8 {
    match bit {
        BitVal::Zero => 0,
        BitVal::One => 1,
        BitVal::X => 2,
        BitVal::Z => 3,
    }
}

fn binary_constant(width: u32, value: usize) -> Result<ConstBits, crate::SynthError> {
    let bits = (0..width)
        .rev()
        .map(|bit| {
            if value & (1usize << bit) == 0 {
                BitVal::Zero
            } else {
                BitVal::One
            }
        })
        .collect();
    ConstBits::from_bits(bits).map_err(crate::SynthError::from)
}

fn boolean_constant(width: u32, hot_bit: Option<u32>) -> Result<ConstBits, crate::SynthError> {
    let bits = (0..width)
        .rev()
        .map(|bit| {
            if hot_bit == Some(bit) {
                BitVal::One
            } else {
                BitVal::Zero
            }
        })
        .collect();
    ConstBits::from_bits(bits).map_err(crate::SynthError::from)
}

#[cfg(test)]
mod tests;
