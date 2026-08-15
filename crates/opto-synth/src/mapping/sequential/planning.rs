// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;
use std::collections::BTreeMap;

const MAX_FEEDBACK_MUX_NODES: usize = 256;

pub(crate) fn recover_feedback_enables(
    module: &mut word::WordModule,
    sequential_catalog: &super::SequentialCellCatalog,
    gating_edges: &dyn Fn(word::Edge) -> bool,
    ownership: &mut crate::regional::StructuralOwnershipProvenance,
) -> Result<(), crate::SynthError> {
    let connected = register_targets(module)?;
    let mut candidates = Vec::new();
    for (operation, target) in connected {
        let model = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "feedback-enable candidate references unknown operation {operation:?}"
            ))
        })?;
        let word::OpKind::Register(register) = &model.kind else {
            return Err(crate::SynthError::invariant(format!(
                "feedback-enable candidate {operation:?} is not a register"
            )));
        };
        let Some(reset_requests) = uniform_async_reset_requests(module, &register.resets)? else {
            continue;
        };
        if register.enable.is_some()
            || !(sequential_catalog.has_enable_cell(register.edge, &reset_requests)
                || gating_edges(register.edge))
        {
            continue;
        }
        // Recovery infers an enable by matching a hold path against a read of
        // the register's target signal. That read denotes the register's output
        // only when the clocked process assigns the signal on one branch; a
        // process with a reset branch assigns it on two, and the inferred enable
        // then comes out narrower than the design's. Registers whose enable the
        // frontend already knows do not come here at all, so declining these
        // costs only the registers whose enable is written as a mux.
        if !register.resets.is_empty() {
            continue;
        }
        candidates.push((operation, register.clone(), target, model.source.clone()));
    }

    for (operation_id, mut register, target, source) in candidates {
        let start = ownership.start(module)?;
        let q = read_target(module, &target, &source)?;
        ownership.claim_since(module, start, &[operation_id])?;
        let mut budget = MAX_FEEDBACK_MUX_NODES;
        let Some(plan) = feedback_update_plan(module, register.d, q, &mut budget)? else {
            continue;
        };
        if !plan.saw_hold || matches!(plan.enable, FeedbackEnable::Always | FeedbackEnable::Never) {
            continue;
        }
        if feedback_enable_type(module, &plan.enable).is_none() {
            continue;
        }
        let start = ownership.start(module)?;
        let enable = emit_feedback_enable(module, &plan.enable, &source)?;
        let data = emit_feedback_data(module, &plan.data, &source)?;
        let reconstructed = module
            .mux(enable, data, q, source.clone())
            .map_err(crate::SynthError::from)?;
        ownership.claim_since(module, start, &[operation_id])?;
        // The decomposition walks a next-state mux tree looking for paths that
        // hold the register's own output. Nothing about that walk guarantees the
        // enable and data it extracts reconstruct the original next state, and
        // an enable that is too narrow silently freezes the register: the design
        // keeps its reset value and only a long random-stimulus simulation
        // notices. Prove the identity, and decline the rewrite otherwise.
        if !enable_recovery_is_equivalent(module, register.d, reconstructed)? {
            continue;
        }
        register.d = data;
        register.enable = Some(word::Enable {
            value: enable,
            active_high: true,
        });
        module
            .operation_mut(operation_id)
            .expect("candidate register remains present")
            .kind = word::OpKind::Register(register);
    }
    Ok(())
}

/// Proves that a recovered enable and data reconstruct the original next state
/// for every value of the register and its inputs.
///
/// A disproved identity is a "do not rewrite this register" answer, not an
/// error: recovery is an optimization and declining it leaves a correct design.
fn enable_recovery_is_equivalent(
    module: &word::WordModule,
    next_state: word::ValueId,
    reconstructed: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let outcome = opto_formal::prove_value_equivalence_under_assumptions(
        module,
        next_state,
        reconstructed,
        &[],
    )
    .map_err(|error| {
        crate::SynthError::invariant(format!("feedback-enable equivalence proof failed: {error}"))
    })?;
    Ok(outcome.require_proved().is_ok())
}

fn register_targets(
    module: &word::WordModule,
) -> Result<BTreeMap<word::OpId, word::LValue>, crate::SynthError> {
    let mut connected = BTreeMap::<word::OpId, word::LValue>::new();
    for connect in module.connects() {
        let Some(value) = module.value(connect.value) else {
            return Err(crate::SynthError::invariant(format!(
                "unknown RTL value {:?}",
                connect.value
            )));
        };
        let word::ValueKind::Operation(operation) = value.kind else {
            continue;
        };
        if !matches!(
            module.operation(operation).map(|operation| &operation.kind),
            Some(word::OpKind::Register(_))
        ) {
            continue;
        }
        if connected
            .insert(operation, connect.target.clone())
            .is_some()
        {
            return Err(crate::SynthError::invariant(format!(
                "register operation {operation:?} drives multiple targets"
            )));
        }
    }
    Ok(connected)
}

/// Turns every enable the target cannot realize as an enabled cell into a
/// next-state mux.
///
/// This is the only site that consumes an enable into the next state. It runs
/// last, so clock gating and enabled-cell selection have already taken the
/// enables they can use, and no earlier pass has to destroy an exact control
/// that a later one would have to recover.
pub(crate) fn expand_unsupported_enables(
    module: &mut word::WordModule,
    sequential_catalog: &super::SequentialCellCatalog,
    ownership: &mut crate::regional::StructuralOwnershipProvenance,
) -> Result<(), crate::SynthError> {
    let mut candidates = Vec::new();
    for index in 0..module.operations().len() {
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        let model = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "enable candidate references unknown {operation:?}"
            ))
        })?;
        let word::OpKind::Register(register) = &model.kind else {
            continue;
        };
        let Some(enable) = register.enable else {
            continue;
        };
        let result = model.result;
        let has_enable_cell = uniform_async_reset_requests(module, &register.resets)?
            .is_some_and(|requests| sequential_catalog.has_enable_cell(register.edge, &requests));
        if has_enable_cell {
            continue;
        }
        candidates.push((
            operation,
            register.clone(),
            enable,
            result,
            model.source.clone(),
        ));
    }
    let mut generated_names = crate::mapping::word_util::GeneratedNames::new(module)?;
    for (operation, mut register, enable, result, source) in candidates {
        let start = ownership.start(module)?;
        // The held value is the register's own output, read through a wire this
        // pass owns. Reading the register's target signal instead would denote
        // whichever assignment to that signal ran last, which is not the same
        // thing when a clocked process assigns it on more than one branch.
        let held = crate::mapping::word_util::add_generated_boundary_value(
            &mut generated_names,
            module,
            result,
            &source,
        )?;
        let (then_value, else_value) = if enable.active_high {
            (register.d, held)
        } else {
            (held, register.d)
        };
        register.d = module
            .mux(enable.value, then_value, else_value, source.clone())
            .map_err(crate::SynthError::from)?;
        ownership.claim_since(module, start, &[operation])?;
        register.enable = None;
        module
            .operation_mut(operation)
            .expect("candidate register remains present")
            .kind = word::OpKind::Register(register);
    }
    Ok(())
}

fn feedback_enable_type(
    module: &word::WordModule,
    enable: &FeedbackEnable,
) -> Option<word::WordType> {
    match enable {
        FeedbackEnable::Never | FeedbackEnable::Always => None,
        FeedbackEnable::Value(value) => module.value(*value).map(|value| value.ty),
        FeedbackEnable::Not(value) => feedback_enable_type(module, value),
        FeedbackEnable::And(left, right) | FeedbackEnable::Or(left, right) => {
            let left = feedback_enable_type(module, left)?;
            (feedback_enable_type(module, right)? == left).then_some(left)
        }
        FeedbackEnable::Mux {
            then_value,
            else_value,
            ..
        } => {
            let then_value = feedback_enable_type(module, then_value)?;
            (feedback_enable_type(module, else_value)? == then_value).then_some(then_value)
        }
    }
}

#[derive(Debug, Clone)]
struct FeedbackPlan {
    enable: FeedbackEnable,
    data: FeedbackData,
    saw_hold: bool,
}

#[derive(Debug, Clone)]
enum FeedbackEnable {
    Never,
    Always,
    Value(word::ValueId),
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Mux {
        cond: word::ValueId,
        then_value: Box<Self>,
        else_value: Box<Self>,
    },
}

#[derive(Debug, Clone)]
enum FeedbackData {
    Value(word::ValueId),
    Mux {
        cond: word::ValueId,
        then_value: Box<Self>,
        else_value: Box<Self>,
    },
}

fn feedback_update_plan(
    module: &word::WordModule,
    value: word::ValueId,
    q: word::ValueId,
    budget: &mut usize,
) -> Result<Option<FeedbackPlan>, crate::SynthError> {
    if same_scalar_value(module, value, q)? {
        return Ok(Some(FeedbackPlan {
            enable: FeedbackEnable::Never,
            data: FeedbackData::Value(q),
            saw_hold: true,
        }));
    }
    let model = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {value:?}")))?;
    let word::ValueKind::Operation(operation) = model.kind else {
        return Ok(Some(FeedbackPlan {
            enable: FeedbackEnable::Always,
            data: FeedbackData::Value(value),
            saw_hold: false,
        }));
    };
    let operation = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant(format!("value {value:?} references an unknown operation"))
    })?;
    let word::OpKind::Mux {
        cond,
        then_value,
        else_value,
    } = operation.kind
    else {
        return Ok(Some(FeedbackPlan {
            enable: FeedbackEnable::Always,
            data: FeedbackData::Value(value),
            saw_hold: false,
        }));
    };
    let Some(remaining) = budget.checked_sub(1) else {
        return Ok(None);
    };
    *budget = remaining;
    let Some(then_plan) = feedback_update_plan(module, then_value, q, budget)? else {
        return Ok(None);
    };
    let Some(else_plan) = feedback_update_plan(module, else_value, q, budget)? else {
        return Ok(None);
    };
    Ok(Some(combine_feedback_plans(cond, then_plan, else_plan)))
}

fn combine_feedback_plans(
    cond: word::ValueId,
    then_plan: FeedbackPlan,
    else_plan: FeedbackPlan,
) -> FeedbackPlan {
    let then_never = matches!(then_plan.enable, FeedbackEnable::Never);
    let else_never = matches!(else_plan.enable, FeedbackEnable::Never);
    let saw_hold = then_plan.saw_hold || else_plan.saw_hold;
    let enable = mux_feedback_enable(cond, then_plan.enable, else_plan.enable);
    let data = if matches!(enable, FeedbackEnable::Never) || else_never {
        then_plan.data
    } else if then_never {
        else_plan.data
    } else {
        FeedbackData::Mux {
            cond,
            then_value: Box::new(then_plan.data),
            else_value: Box::new(else_plan.data),
        }
    };
    FeedbackPlan {
        enable,
        data,
        saw_hold,
    }
}

fn mux_feedback_enable(
    cond: word::ValueId,
    then_value: FeedbackEnable,
    else_value: FeedbackEnable,
) -> FeedbackEnable {
    match (&then_value, &else_value) {
        (FeedbackEnable::Never, FeedbackEnable::Never) => FeedbackEnable::Never,
        (FeedbackEnable::Always, FeedbackEnable::Always) => FeedbackEnable::Always,
        (FeedbackEnable::Always, FeedbackEnable::Never) => FeedbackEnable::Value(cond),
        (FeedbackEnable::Never, FeedbackEnable::Always) => {
            FeedbackEnable::Not(Box::new(FeedbackEnable::Value(cond)))
        }
        (_, FeedbackEnable::Never) => {
            FeedbackEnable::And(Box::new(FeedbackEnable::Value(cond)), Box::new(then_value))
        }
        (_, FeedbackEnable::Always) => FeedbackEnable::Or(
            Box::new(FeedbackEnable::Not(Box::new(FeedbackEnable::Value(cond)))),
            Box::new(then_value),
        ),
        (FeedbackEnable::Never, _) => FeedbackEnable::And(
            Box::new(FeedbackEnable::Not(Box::new(FeedbackEnable::Value(cond)))),
            Box::new(else_value),
        ),
        (FeedbackEnable::Always, _) => {
            FeedbackEnable::Or(Box::new(FeedbackEnable::Value(cond)), Box::new(else_value))
        }
        _ => FeedbackEnable::Mux {
            cond,
            then_value: Box::new(then_value),
            else_value: Box::new(else_value),
        },
    }
}

fn emit_feedback_enable(
    module: &mut word::WordModule,
    enable: &FeedbackEnable,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    match enable {
        FeedbackEnable::Never | FeedbackEnable::Always => Err(crate::SynthError::invariant(
            "constant feedback enable reached sequential lowering",
        )),
        FeedbackEnable::Value(value) => Ok(*value),
        FeedbackEnable::Not(value) => {
            let value = emit_feedback_enable(module, value, source)?;
            module
                .unary(word::UnaryOp::BitNot, value, source.clone())
                .map_err(crate::SynthError::from)
        }
        FeedbackEnable::And(left, right) | FeedbackEnable::Or(left, right) => {
            let op = if matches!(enable, FeedbackEnable::And(..)) {
                word::BinaryOp::BitAnd
            } else {
                word::BinaryOp::BitOr
            };
            let left = emit_feedback_enable(module, left, source)?;
            let right = emit_feedback_enable(module, right, source)?;
            module
                .binary(op, left, right, source.clone())
                .map_err(crate::SynthError::from)
        }
        FeedbackEnable::Mux {
            cond,
            then_value,
            else_value,
        } => {
            let then_value = emit_feedback_enable(module, then_value, source)?;
            let else_value = emit_feedback_enable(module, else_value, source)?;
            module
                .mux(*cond, then_value, else_value, source.clone())
                .map_err(crate::SynthError::from)
        }
    }
}

fn emit_feedback_data(
    module: &mut word::WordModule,
    data: &FeedbackData,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    match data {
        FeedbackData::Value(value) => Ok(*value),
        FeedbackData::Mux {
            cond,
            then_value,
            else_value,
        } => {
            let then_value = emit_feedback_data(module, then_value, source)?;
            let else_value = emit_feedback_data(module, else_value, source)?;
            module
                .mux(*cond, then_value, else_value, source.clone())
                .map_err(crate::SynthError::from)
        }
    }
}

fn same_scalar_value(
    module: &word::WordModule,
    left: word::ValueId,
    right: word::ValueId,
) -> Result<bool, crate::SynthError> {
    if left == right {
        return Ok(true);
    }
    let left = module
        .value(left)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {left:?}")))?;
    let right = module
        .value(right)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {right:?}")))?;
    Ok(matches!(
        (&left.kind, &right.kind),
        (word::ValueKind::Signal(left), word::ValueKind::Signal(right)) if left == right
    ))
}

/// Normalizes register and latch controls without consuming any of them.
///
/// Resets are normalized, and a synchronous reset is composed into the enable
/// and into the next state. The enable itself is retained: whether it becomes a
/// gated clock, an enabled cell, or a next-state mux is decided later by the
/// passes that own those choices.
pub(crate) fn lower_controls(
    module: &mut word::WordModule,
    ownership: &mut crate::regional::StructuralOwnershipProvenance,
) -> Result<(), crate::SynthError> {
    let mut controlled = Vec::new();
    let observability = crate::word::uses::netlist_observability(module)?;
    for (index, operation) in module.operations().iter().enumerate() {
        let word::OpKind::Register(register) = &operation.kind else {
            continue;
        };
        if register.enable.is_none() && register.resets.is_empty() {
            continue;
        }
        if !observability.observes_value(operation.result)? {
            continue;
        }
        let operation_id = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        controlled.push((
            operation_id,
            ControlledRegister {
                register: register.clone(),
                source: operation.source.clone(),
            },
        ));
    }

    for (operation_id, controlled) in controlled {
        let start = ownership.start(module)?;
        let mut data = controlled.register.d;
        let asynchronous_resets =
            normalize_async_resets(module, &controlled.register.resets, &controlled.source)?;
        let synchronous_resets = controlled
            .register
            .resets
            .iter()
            .copied()
            .filter(|reset| reset.kind == word::ResetKind::Sync)
            .collect::<Vec<_>>();
        // The enable is retained, never expanded here. Expansion is owned by
        // `expand_unsupported_enables`, which runs after clock gating has had
        // its chance to consume the enable. Folding it into the next state at
        // this point destroyed an exact control that a later pass then had to
        // recover by pattern matching, and a recovered enable is only ever as
        // good as the pattern.
        //
        // A synchronous reset is composed into the enable as well as into the
        // next state: the register must be clocked on a reset cycle, and it must
        // take the reset value on that cycle.
        let retained_enable = match controlled.register.enable {
            None => None,
            Some(enable) if synchronous_resets.is_empty() => Some(enable),
            Some(enable) => {
                let enable_active = active_high_control(
                    module,
                    enable.value,
                    enable.active_high,
                    &controlled.source,
                )?;
                let reset_active =
                    combined_reset_condition(module, &synchronous_resets, &controlled.source)?
                        .expect("non-empty synchronous reset list has a condition");
                let composed = module
                    .binary(
                        word::BinaryOp::BitOr,
                        enable_active,
                        reset_active,
                        controlled.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                Some(word::Enable {
                    value: composed,
                    active_high: true,
                })
            }
        };

        for reset in synchronous_resets.iter().rev() {
            data = if reset.active_high {
                module.mux(
                    reset.value,
                    reset.reset_value,
                    data,
                    controlled.source.clone(),
                )
            } else {
                module.mux(
                    reset.value,
                    data,
                    reset.reset_value,
                    controlled.source.clone(),
                )
            }
            .map_err(crate::SynthError::from)?;
        }

        let operation = module.operation_mut(operation_id).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "controlled register operation {operation_id:?} disappeared"
            ))
        })?;
        operation.kind = word::OpKind::Register(word::RegisterOp {
            d: data,
            enable: retained_enable,
            resets: asynchronous_resets,
            ..controlled.register
        });
        ownership.claim_since(module, start, &[operation_id])?;
    }
    Ok(())
}

pub(crate) fn normalize_sequential_controls(
    module: &mut word::WordModule,
    ownership: &mut crate::regional::StructuralOwnershipProvenance,
) -> Result<(), crate::SynthError> {
    let controls = module
        .operations()
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| match &operation.kind {
            word::OpKind::Register(register) if register.resets.len() > 1 => Some((
                index,
                register.resets.clone(),
                operation.source.clone(),
                false,
            )),
            word::OpKind::Latch(latch) if latch.resets.len() > 1 => {
                Some((index, latch.resets.clone(), operation.source.clone(), true))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, resets, source, latch) in controls {
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        let start = ownership.start(module)?;
        let async_count = resets
            .iter()
            .take_while(|reset| reset.kind == word::ResetKind::Async)
            .count();
        if resets[async_count..]
            .iter()
            .any(|reset| reset.kind != word::ResetKind::Sync)
        {
            return Err(crate::SynthError::invariant(
                "sequential resets are not ordered as asynchronous then synchronous controls",
            ));
        }
        if latch && async_count != resets.len() {
            return Err(crate::SynthError::invalid(
                "latches cannot use synchronous reset controls",
            ));
        }
        let asynchronous = normalize_async_resets(module, &resets[..async_count], &source)?;
        let synchronous = normalize_synchronous_resets(module, &resets[async_count..], &source)?;
        let stored = module
            .operation(operation)
            .ok_or_else(|| crate::SynthError::invariant("sequential operation disappeared"))?
            .clone();
        match stored.kind {
            word::OpKind::Register(mut register) => {
                if asynchronous.is_empty() {
                    register.resets = synchronous;
                } else if let Some(synchronous) = synchronous.first().copied() {
                    let active = active_high_control(
                        module,
                        synchronous.value,
                        synchronous.active_high,
                        &source,
                    )?;
                    register.d = module
                        .mux(active, synchronous.reset_value, register.d, source.clone())
                        .map_err(crate::SynthError::from)?;
                    register.enable = register
                        .enable
                        .map(|enable| {
                            let enable = active_high_control(
                                module,
                                enable.value,
                                enable.active_high,
                                &source,
                            )?;
                            Ok::<_, crate::SynthError>(word::Enable {
                                value: module
                                    .binary(
                                        word::BinaryOp::LogicalOr,
                                        enable,
                                        active,
                                        source.clone(),
                                    )
                                    .map_err(crate::SynthError::from)?,
                                active_high: true,
                            })
                        })
                        .transpose()?;
                    register.resets = asynchronous;
                } else {
                    register.resets = asynchronous;
                }
                module
                    .operation_mut(operation)
                    .expect("sequential operation remains present")
                    .kind = word::OpKind::Register(register);
            }
            word::OpKind::Latch(mut latch) => {
                latch.resets = asynchronous;
                module
                    .operation_mut(operation)
                    .expect("sequential operation remains present")
                    .kind = word::OpKind::Latch(latch);
            }
            _ => {
                return Err(crate::SynthError::invariant(
                    "sequential control target changed operation kind",
                ));
            }
        }
        ownership.claim_since(module, start, &[operation])?;
    }
    Ok(())
}

fn normalize_synchronous_resets(
    module: &mut word::WordModule,
    resets: &[word::Reset],
    source: &word::SourceSpan,
) -> Result<Vec<word::Reset>, crate::SynthError> {
    if resets.len() <= 1 {
        return Ok(resets.to_vec());
    }
    let mut reset_value = resets
        .last()
        .expect("multiple synchronous resets are non-empty")
        .reset_value;
    for reset in resets[..resets.len() - 1].iter().rev() {
        let asserted = active_high_control(module, reset.value, reset.active_high, source)?;
        reset_value = module
            .mux(asserted, reset.reset_value, reset_value, source.clone())
            .map_err(crate::SynthError::from)?;
    }
    let condition = combined_reset_condition(module, resets, source)?
        .expect("multiple synchronous resets have a condition");
    Ok(vec![word::Reset {
        kind: word::ResetKind::Sync,
        value: condition,
        active_high: true,
        reset_value,
    }])
}

fn active_high_control(
    module: &mut word::WordModule,
    value: word::ValueId,
    active_high: bool,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    if active_high {
        Ok(value)
    } else {
        module
            .unary(word::UnaryOp::BitNot, value, source.clone())
            .map_err(crate::SynthError::from)
    }
}

fn async_reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<super::AsyncResetRequests, crate::SynthError> {
    resets
        .iter()
        .copied()
        .map(|reset| async_reset_request_for(module, reset))
        .collect()
}

fn async_reset_request_for(
    module: &word::WordModule,
    reset: word::Reset,
) -> Result<super::AsyncResetRequest, crate::SynthError> {
    let value = module.value(reset.reset_value).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "unknown asynchronous reset value {:?}",
            reset.reset_value
        ))
    })?;
    let word::ValueKind::Constant(bits) = &value.kind else {
        return Err(crate::SynthError::invariant(
            "asynchronous register reset value must be constant",
        ));
    };
    let reset_value = crate::boolean::logic::logic_constant(bits).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "asynchronous register reset value must be a two-state scalar constant, got width {} and bits {bits:?}",
            value.ty.width()
        ))
    })?;
    Ok(super::AsyncResetRequest {
        active_high: reset.active_high,
        reset_value,
    })
}

fn uniform_async_reset_requests(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<Option<super::AsyncResetRequests>, crate::SynthError> {
    resets
        .iter()
        .copied()
        .map(|reset| uniform_async_reset_request_for(module, reset))
        .collect()
}

fn uniform_async_reset_request_for(
    module: &word::WordModule,
    reset: word::Reset,
) -> Result<Option<super::AsyncResetRequest>, crate::SynthError> {
    let value = module.value(reset.reset_value).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "unknown asynchronous reset value {:?}",
            reset.reset_value
        ))
    })?;
    let word::ValueKind::Constant(bits) = &value.kind else {
        return Err(crate::SynthError::invariant(
            "asynchronous register reset value must be constant",
        ));
    };
    let Some(first) = bits.as_slice().first().copied() else {
        return Err(crate::SynthError::invariant(
            "asynchronous register reset value is empty",
        ));
    };
    let reset_value = match first {
        opto_ir::BitVal::Zero => false,
        opto_ir::BitVal::One => true,
        opto_ir::BitVal::X | opto_ir::BitVal::Z => return Ok(None),
    };
    if !bits.as_slice().iter().all(|&bit| bit == first) {
        return Ok(None);
    }
    Ok(Some(super::AsyncResetRequest {
        active_high: reset.active_high,
        reset_value,
    }))
}

fn normalize_async_resets(
    module: &mut word::WordModule,
    resets: &[word::Reset],
    source: &word::SourceSpan,
) -> Result<Vec<word::Reset>, crate::SynthError> {
    let asynchronous = resets
        .iter()
        .copied()
        .filter(|reset| reset.kind == word::ResetKind::Async)
        .collect::<Vec<_>>();
    if asynchronous.len() <= 1 {
        return Ok(asynchronous);
    }

    let requests = async_reset_requests(module, &asynchronous)?;
    if requests
        .iter()
        .all(|request| request.reset_value == requests[0].reset_value)
    {
        let condition = combined_reset_condition(module, &asynchronous, source)?
            .expect("multiple asynchronous resets have a condition");
        return Ok(vec![word::Reset {
            kind: word::ResetKind::Async,
            value: condition,
            active_high: true,
            reset_value: asynchronous[0].reset_value,
        }]);
    }

    let mut blocked = None;
    let mut groups = [None, None];
    let mut group_values = [None, None];
    for (reset, request) in asynchronous.iter().zip(requests) {
        let asserted = active_high_control(module, reset.value, reset.active_high, source)?;
        let effective = if let Some(blocked) = blocked {
            let available = module
                .unary(word::UnaryOp::BitNot, blocked, source.clone())
                .map_err(crate::SynthError::from)?;
            module
                .binary(
                    word::BinaryOp::LogicalAnd,
                    asserted,
                    available,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?
        } else {
            asserted
        };
        let group = usize::from(request.reset_value);
        groups[group] = Some(match groups[group] {
            Some(existing) => module
                .binary(
                    word::BinaryOp::LogicalOr,
                    existing,
                    effective,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
            None => effective,
        });
        group_values[group].get_or_insert(reset.reset_value);
        blocked = Some(match blocked {
            Some(existing) => module
                .binary(
                    word::BinaryOp::LogicalOr,
                    existing,
                    asserted,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
            None => asserted,
        });
    }
    Ok(groups
        .into_iter()
        .zip(group_values)
        .filter_map(|(value, reset_value)| {
            Some(word::Reset {
                kind: word::ResetKind::Async,
                value: value?,
                active_high: true,
                reset_value: reset_value?,
            })
        })
        .collect())
}

fn combined_reset_condition(
    module: &mut word::WordModule,
    resets: &[word::Reset],
    source: &word::SourceSpan,
) -> Result<Option<word::ValueId>, crate::SynthError> {
    let mut combined = None;
    for reset in resets {
        let asserted = active_high_control(module, reset.value, reset.active_high, source)?;
        combined = Some(match combined {
            Some(existing) => module
                .binary(
                    word::BinaryOp::LogicalOr,
                    existing,
                    asserted,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
            None => asserted,
        });
    }
    Ok(combined)
}

fn read_target(
    module: &mut word::WordModule,
    target: &word::LValue,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    match target.range {
        None => module.read_signal(target.signal, source.clone()),
        Some(range) if range.msb >= range.lsb => {
            module.read_signal_slice(target.signal, range.lsb, range.width(), source.clone())
        }
        Some(range) => {
            return Err(crate::SynthError::invariant(format!(
                "ascending controlled register target [{}:{}] is not supported",
                range.msb, range.lsb
            )));
        }
    }
    .map_err(crate::SynthError::from)
}

struct ControlledRegister {
    register: word::RegisterOp,
    source: word::SourceSpan,
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::{BitVal, ConstBits};

    fn input(module: &mut word::WordModule, name: &str) -> word::ValueId {
        let port = module
            .add_port(
                name,
                word::PortDirection::Input,
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap()
    }

    #[test]
    fn recognizes_only_a_bounded_feedback_mux_as_an_enable() {
        let mut module = word::WordModule::new("feedback_enable");
        let enable = input(&mut module, "enable");
        let update = input(&mut module, "update");
        let q = input(&mut module, "q");
        let d = module
            .mux(enable, update, q, word::SourceSpan::default())
            .unwrap();

        let mut budget = MAX_FEEDBACK_MUX_NODES;
        let plan = feedback_update_plan(&module, d, q, &mut budget)
            .unwrap()
            .unwrap();

        assert!(plan.saw_hold);
        assert!(matches!(plan.enable, FeedbackEnable::Value(value) if value == enable));
        assert!(matches!(plan.data, FeedbackData::Value(value) if value == update));
        assert_eq!(budget, MAX_FEEDBACK_MUX_NODES - 1);
    }

    #[test]
    fn does_not_turn_general_feedback_logic_into_an_enable_search() {
        let mut module = word::WordModule::new("general_feedback");
        let update = input(&mut module, "update");
        let q = input(&mut module, "q");
        let d = module
            .binary(
                word::BinaryOp::BitXor,
                q,
                update,
                word::SourceSpan::default(),
            )
            .unwrap();

        let mut budget = MAX_FEEDBACK_MUX_NODES;
        let plan = feedback_update_plan(&module, d, q, &mut budget)
            .unwrap()
            .unwrap();

        assert!(!plan.saw_hold);
        assert!(matches!(plan.enable, FeedbackEnable::Always));
        assert_eq!(budget, MAX_FEEDBACK_MUX_NODES);
    }

    #[test]
    fn stops_feedback_mux_recursion_at_the_explicit_budget() {
        let mut module = word::WordModule::new("bounded_feedback");
        let enable = input(&mut module, "enable");
        let update = input(&mut module, "update");
        let q = input(&mut module, "q");
        let d = module
            .mux(enable, update, q, word::SourceSpan::default())
            .unwrap();

        let mut budget = 0;
        assert!(
            feedback_update_plan(&module, d, q, &mut budget)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uniform_vector_reset_has_one_catalog_request() {
        let mut module = word::WordModule::new("uniform_reset");
        let reset_value = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero; 4]).unwrap(),
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let reset = input(&mut module, "reset");
        let request = uniform_async_reset_request_for(
            &module,
            word::Reset {
                kind: word::ResetKind::Async,
                value: reset,
                active_high: true,
                reset_value,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            request,
            super::super::AsyncResetRequest {
                active_high: true,
                reset_value: false,
            }
        );
    }

    #[test]
    fn mixed_vector_reset_defers_catalog_selection_until_bitblast() {
        let mut module = word::WordModule::new("mixed_reset");
        let reset_value = module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero, BitVal::One, BitVal::Zero, BitVal::One])
                    .unwrap(),
                word::WordType::bits(4).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let reset = input(&mut module, "reset");

        assert_eq!(
            uniform_async_reset_request_for(
                &module,
                word::Reset {
                    kind: word::ResetKind::Async,
                    value: reset,
                    active_high: false,
                    reset_value,
                },
            )
            .unwrap(),
            None
        );
    }
}
