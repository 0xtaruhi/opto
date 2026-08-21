// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::FsmPlan;
use hashbrown::HashMap;
use opto_ir::word;

pub(super) fn materialize_plans(
    module: &mut word::WordModule,
    plans: &[FsmPlan],
) -> Result<Box<[(word::OpId, word::OpId)]>, crate::SynthError> {
    if plans.is_empty() {
        return Ok(Box::new([]));
    }
    let rewrites = plans
        .iter()
        .map(|plan| rewrite_candidate(module, plan))
        .collect::<Result<_, _>>()?;
    module.validate().map_err(crate::SynthError::from)?;
    Ok(rewrites)
}

fn rewrite_candidate(
    module: &mut word::WordModule,
    plan: &FsmPlan,
) -> Result<(word::OpId, word::OpId), crate::SynthError> {
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
    let implementation_register = word::RegisterOp {
        name: candidate.register.name,
        d: encoded_next,
        clock: candidate.register.clock,
        edge: candidate.register.edge,
        enable: candidate.register.enable,
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
    let operation = |value| {
        module
            .value(value)
            .and_then(|value| match value.kind {
                word::ValueKind::Operation(operation) => Some(operation),
                word::ValueKind::Constant(_) | word::ValueKind::Signal(_) => None,
            })
            .ok_or_else(|| crate::SynthError::invariant("FSM state has no operation identity"))
    };
    Ok((
        operation(candidate.register_result)?,
        operation(encoded_register)?,
    ))
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
) -> Result<word::ValueId, crate::SynthError> {
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
            module
                .constant(
                    plan.codes[candidate.state_classes[state]].clone(),
                    plan.encoded_type,
                    candidate.source.clone(),
                )
                .map_err(crate::SynthError::from)?
        } else if matches!(
            value.kind,
            word::ValueKind::Signal(reference)
                if reference.signal == candidate.state_signal
                    && reference.lsb == 0
                    && reference.width() == value.ty.width()
        ) {
            encoded_state
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
                    module
                        .mux(*cond, then_value, else_value, candidate.source.clone())
                        .map_err(crate::SynthError::from)?
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
