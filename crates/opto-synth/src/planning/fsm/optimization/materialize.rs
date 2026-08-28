// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::FsmPlan;
use hashbrown::HashMap;
use opto_ir::{ConstBits, word};

#[derive(Debug, Clone, Copy)]
struct EncodedTransition {
    data: word::ValueId,
    activity: TransitionActivity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionActivity {
    Never,
    Always,
    Value(word::ValueId),
}

pub(super) fn materialize_plans(
    module: &mut word::WordModule,
    plans: &[FsmPlan],
) -> Result<(), crate::SynthError> {
    if plans.is_empty() {
        return Ok(());
    }
    for plan in plans {
        rewrite_candidate(module, plan)?;
    }
    module.validate().map_err(crate::SynthError::from)?;
    Ok(())
}

fn rewrite_candidate(
    module: &mut word::WordModule,
    plan: &FsmPlan,
) -> Result<(), crate::SynthError> {
    let candidate = &plan.machine;
    let encoded_signal = module
        .add_generated_wire(plan.encoded_type, candidate.source.clone())
        .map_err(crate::SynthError::from)?;
    let encoded_state = module
        .read_signal(encoded_signal, candidate.source.clone())
        .map_err(crate::SynthError::from)?;
    let decoded_state = decode_state(module, encoded_state, plan)?;
    let encoded_next = encode_transition(module, encoded_state, plan)?;
    if candidate.register.resets.len() != candidate.reset_values.len() {
        return Err(crate::SynthError::invariant(
            "FSM reset controls and values differ in length",
        ));
    }
    let mut resets = Vec::with_capacity(candidate.register.resets.len());
    for (reset, reset_bits) in candidate
        .register
        .resets
        .iter()
        .zip(&candidate.reset_values)
    {
        let state = candidate
            .states
            .iter()
            .position(|state| state == reset_bits)
            .ok_or_else(|| crate::SynthError::invariant("FSM reset state is not encoded"))?;
        let class = candidate.state_classes[state];
        let encoded_reset = module
            .constant(
                plan.codes[class].clone(),
                plan.encoded_type,
                candidate.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        resets.push(word::Reset {
            reset_value: encoded_reset,
            ..*reset
        });
    }
    let (data, enable) = transition_register_inputs(
        module,
        encoded_state,
        encoded_next,
        candidate.register.enable,
        &candidate.source,
    )?;
    let implementation_register = word::RegisterOp {
        name: candidate.register.name,
        d: data,
        clock: candidate.register.clock,
        edge: candidate.register.edge,
        enable,
        resets,
    };
    let encoded_register = module
        .register(implementation_register, candidate.source.clone())
        .map_err(crate::SynthError::from)?;

    let connects = module.take_connects();
    let mut removed = false;
    for connect in connects {
        if connect.value == candidate.register_result
            && connect.target.signal == candidate.state_signal
        {
            if removed {
                return Err(crate::SynthError::invariant(
                    "FSM register has more than one state connection",
                ));
            }
            removed = true;
            continue;
        }
        module
            .connect(connect.target, connect.value, connect.source)
            .map_err(crate::SynthError::from)?;
    }
    if !removed {
        return Err(crate::SynthError::invariant(
            "FSM register connection is absent during materialization",
        ));
    }
    module
        .connect(
            word::LValue::signal(candidate.state_signal),
            decoded_state,
            candidate.source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    module
        .connect(
            word::LValue::signal(encoded_signal),
            encoded_register,
            candidate.source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    Ok(())
}

fn decode_state(
    module: &mut word::WordModule,
    encoded: word::ValueId,
    plan: &FsmPlan,
) -> Result<word::ValueId, crate::SynthError> {
    let candidate = &plan.machine;
    let first = candidate.representatives[0];
    let mut decoded = module
        .constant(
            candidate.states[first].clone(),
            candidate.state_type,
            candidate.source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    for class in 1..candidate.representatives.len() {
        let code = module
            .constant(
                plan.codes[class].clone(),
                plan.encoded_type,
                candidate.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let selected = module
            .binary(word::BinaryOp::Eq, encoded, code, candidate.source.clone())
            .map_err(crate::SynthError::from)?;
        let state = module
            .constant(
                candidate.states[candidate.representatives[class]].clone(),
                candidate.state_type,
                candidate.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        decoded = module
            .mux(selected, state, decoded, candidate.source.clone())
            .map_err(crate::SynthError::from)?;
    }
    Ok(decoded)
}

fn encode_transition(
    module: &mut word::WordModule,
    encoded_state: word::ValueId,
    plan: &FsmPlan,
) -> Result<EncodedTransition, crate::SynthError> {
    let candidate = &plan.machine;
    let mut encoded_values = HashMap::with_capacity(candidate.transition_order.len());
    for &value_id in &candidate.transition_order {
        let Some(value) = module.value(value_id) else {
            return Err(crate::SynthError::invariant(format!(
                "FSM transition plan references unknown value {value_id:?}"
            )));
        };
        let encoded = if let Some((_, bits)) = candidate
            .constant_values
            .iter()
            .find(|(constant, _)| *constant == value_id)
        {
            let state = candidate
                .states
                .iter()
                .position(|state| state == bits)
                .unwrap_or(0);
            let data = module
                .constant(
                    plan.codes[candidate.state_classes[state]].clone(),
                    plan.encoded_type,
                    candidate.source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            EncodedTransition {
                data,
                activity: TransitionActivity::Always,
            }
        } else if matches!(
            value.kind,
            word::ValueKind::Signal(reference)
                if reference.signal == candidate.state_signal
                    && reference.lsb == 0
                    && reference.width() == value.ty.width()
        ) {
            EncodedTransition {
                data: encoded_state,
                activity: TransitionActivity::Never,
            }
        } else {
            let word::ValueKind::Operation(operation_id) = value.kind else {
                return Err(crate::SynthError::invariant(format!(
                    "FSM transition plan contains unsupported value {value_id:?}"
                )));
            };
            let operation = module.operation(operation_id).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "FSM transition plan references unknown operation {operation_id:?}"
                ))
            })?;
            match &operation.kind {
                word::OpKind::Mux {
                    cond,
                    then_value,
                    else_value,
                } => {
                    let then_value = encoded_values.get(then_value).copied().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "FSM transition plan is not in dependency order",
                        )
                    })?;
                    let else_value = encoded_values.get(else_value).copied().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "FSM transition plan is not in dependency order",
                        )
                    })?;
                    select_transition(module, *cond, then_value, else_value, &candidate.source)?
                }
                word::OpKind::Cast { value, .. } => {
                    encoded_values.get(value).copied().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "FSM transition plan is not in dependency order",
                        )
                    })?
                }
                _ => {
                    return Err(crate::SynthError::invariant(format!(
                        "FSM transition plan contains unsupported operation {operation_id:?}"
                    )));
                }
            }
        };
        encoded_values.insert(value_id, encoded);
    }
    encoded_values
        .get(&candidate.register.d)
        .copied()
        .ok_or_else(|| crate::SynthError::invariant("FSM transition plan has no root value"))
}

fn select_transition(
    module: &mut word::WordModule,
    condition: word::ValueId,
    then_value: EncodedTransition,
    else_value: EncodedTransition,
    source: &word::SourceSpan,
) -> Result<EncodedTransition, crate::SynthError> {
    let activity = select_activity(
        module,
        condition,
        then_value.activity,
        else_value.activity,
        source,
    )?;
    let data = match (then_value.activity, else_value.activity) {
        (_, TransitionActivity::Never) => then_value.data,
        (TransitionActivity::Never, _) => else_value.data,
        _ if then_value.data == else_value.data => then_value.data,
        _ => module
            .mux(condition, then_value.data, else_value.data, source.clone())
            .map_err(crate::SynthError::from)?,
    };
    Ok(EncodedTransition { data, activity })
}

fn select_activity(
    module: &mut word::WordModule,
    condition: word::ValueId,
    then_value: TransitionActivity,
    else_value: TransitionActivity,
    source: &word::SourceSpan,
) -> Result<TransitionActivity, crate::SynthError> {
    if then_value == else_value {
        return Ok(then_value);
    }
    match (then_value, else_value) {
        (TransitionActivity::Always, TransitionActivity::Never) => {
            Ok(TransitionActivity::Value(condition))
        }
        (TransitionActivity::Never, TransitionActivity::Always) => module
            .unary(word::UnaryOp::BitNot, condition, source.clone())
            .map(TransitionActivity::Value)
            .map_err(crate::SynthError::from),
        (TransitionActivity::Always, TransitionActivity::Value(value)) if value == condition => {
            Ok(TransitionActivity::Value(condition))
        }
        (TransitionActivity::Value(value), TransitionActivity::Never) if value == condition => {
            Ok(TransitionActivity::Value(condition))
        }
        (then_value, else_value) => {
            let ((TransitionActivity::Value(reference), _)
            | (_, TransitionActivity::Value(reference))) = (then_value, else_value)
            else {
                return Err(crate::SynthError::invariant(
                    "materialized transition activity has no typed value",
                ));
            };
            let ty = module
                .value(reference)
                .map(|value| value.ty)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "materialized transition activity references an unknown value",
                    )
                })?;
            let then_value = materialize_activity(module, then_value, ty, source)?;
            let else_value = materialize_activity(module, else_value, ty, source)?;
            module
                .mux(condition, then_value, else_value, source.clone())
                .map(TransitionActivity::Value)
                .map_err(crate::SynthError::from)
        }
    }
}

fn materialize_activity(
    module: &mut word::WordModule,
    activity: TransitionActivity,
    ty: word::WordType,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    match activity {
        TransitionActivity::Never | TransitionActivity::Always => {
            let bit = if activity == TransitionActivity::Always {
                "1"
            } else {
                "0"
            };
            module
                .constant(
                    ConstBits::from_bin_str(bit).map_err(crate::SynthError::from)?,
                    ty,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)
        }
        TransitionActivity::Value(value) => Ok(value),
    }
}

fn transition_register_inputs(
    module: &mut word::WordModule,
    encoded_state: word::ValueId,
    transition: EncodedTransition,
    existing_enable: Option<word::Enable>,
    source: &word::SourceSpan,
) -> Result<(word::ValueId, Option<word::Enable>), crate::SynthError> {
    let derived_enable = match transition.activity {
        TransitionActivity::Never => return Ok((encoded_state, existing_enable)),
        TransitionActivity::Always => return Ok((transition.data, existing_enable)),
        TransitionActivity::Value(value) => value,
    };
    let enable = if let Some(existing) = existing_enable {
        let existing = if existing.active_high {
            existing.value
        } else {
            module
                .unary(word::UnaryOp::BitNot, existing.value, source.clone())
                .map_err(crate::SynthError::from)?
        };
        module
            .binary(
                word::BinaryOp::BitAnd,
                existing,
                derived_enable,
                source.clone(),
            )
            .map_err(crate::SynthError::from)?
    } else {
        derived_enable
    };
    Ok((
        transition.data,
        Some(word::Enable {
            value: enable,
            active_high: true,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materializes_activity_constants_in_the_value_state_domain() {
        let mut module = word::WordModule::new("top");
        let source = word::SourceSpan::default();
        let condition_signal = module
            .add_wire(
                "condition",
                word::WordType::bits(1).unwrap(),
                source.clone(),
            )
            .unwrap();
        let activity_ty = word::WordType::new(1, false, word::LogicStateKind::TwoState).unwrap();
        let activity_signal = module
            .add_wire("activity", activity_ty, source.clone())
            .unwrap();
        let condition = module
            .read_signal(condition_signal, source.clone())
            .unwrap();
        let activity = module.read_signal(activity_signal, source.clone()).unwrap();

        let TransitionActivity::Value(selected) = select_activity(
            &mut module,
            condition,
            TransitionActivity::Always,
            TransitionActivity::Value(activity),
            &source,
        )
        .unwrap() else {
            panic!("mixed transition activity should materialize a value");
        };

        assert_eq!(module.value(selected).unwrap().ty, activity_ty);
        module.validate().unwrap();
    }
}
