// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::ShareCandidate;
use super::activation::Activation;
use crate::planning::dataflow::rewrite_operation_inputs;
use opto_ir::word;

pub(super) fn materialize_groups(
    module: &mut word::WordModule,
    groups: Vec<Vec<ShareCandidate>>,
) -> Result<Box<[super::OperationRewrite]>, crate::SynthError> {
    let mut replacements = vec![None; module.values().len()];
    let mut rewrites = Vec::with_capacity(groups.len());
    for group in groups {
        let first = module.operations().len();
        let shared = materialize_group(module, &group)?;
        rewrites.push(super::OperationRewrite {
            created: first..module.operations().len(),
            replaced: group.iter().map(|candidate| candidate.operation).collect(),
        });
        for candidate in &group {
            replacements[candidate.result.index()] = Some(shared);
        }
    }
    rewrite_uses(module, &replacements)?;
    Ok(rewrites.into_boxed_slice())
}

fn materialize_group(
    module: &mut word::WordModule,
    group: &[ShareCandidate],
) -> Result<word::ValueId, crate::SynthError> {
    let last = group
        .last()
        .ok_or_else(|| crate::SynthError::invariant("empty arithmetic sharing group"))?;
    let result_ty = value_type(module, last.result)?;
    let source = module
        .operation(group[0].operation)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "unknown shared operation {:?}",
                group[0].operation
            ))
        })?
        .source
        .clone();
    let mut selected = [
        resize_operand(module, last.inputs[0], result_ty, &source)?,
        resize_operand(module, last.inputs[1], result_ty, &source)?,
    ];
    for candidate in group[..group.len() - 1].iter().rev() {
        let guard = materialize_activation(module, &candidate.activation, &source)?;
        for (side, input) in candidate.inputs.iter().copied().enumerate() {
            let input = resize_operand(module, input, result_ty, &source)?;
            selected[side] = module
                .mux(guard, input, selected[side], source.clone())
                .map_err(crate::SynthError::from)?;
        }
    }
    module
        .binary(last.kind.binary(), selected[0], selected[1], source)
        .map_err(crate::SynthError::from)
}

fn materialize_activation(
    module: &mut word::WordModule,
    activation: &Activation,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let mut terms = Vec::with_capacity(activation.literals().len());
    for literal in activation.literals() {
        let term = if literal.positive {
            literal.condition
        } else {
            module
                .unary(word::UnaryOp::BitNot, literal.condition, source.clone())
                .map_err(crate::SynthError::from)?
        };
        terms.push(term);
    }
    let mut terms = terms.into_iter();
    let mut result = terms.next().ok_or_else(|| {
        crate::SynthError::invariant("arithmetic sharing requires a guarded activation")
    })?;
    for term in terms {
        result = module
            .binary(word::BinaryOp::BitAnd, result, term, source.clone())
            .map_err(crate::SynthError::from)?;
    }
    Ok(result)
}

fn resize_operand(
    module: &mut word::WordModule,
    value: word::ValueId,
    target: word::WordType,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let ty = value_type(module, value)?;
    if ty == target {
        return Ok(value);
    }
    let kind = if ty.width() < target.width() {
        if target.is_signed() {
            word::CastKind::SignExtend
        } else {
            word::CastKind::ZeroExtend
        }
    } else {
        word::CastKind::Truncate
    };
    module
        .cast(kind, value, target, source.clone())
        .map_err(crate::SynthError::from)
}

fn rewrite_uses(
    module: &mut word::WordModule,
    replacements: &[Option<word::ValueId>],
) -> Result<(), crate::SynthError> {
    for index in 0..module.operations().len() {
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        let mut kind = module
            .operation(operation)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!("unknown operation {operation:?}"))
            })?
            .kind
            .clone();
        rewrite_operation_inputs(&mut kind, |value| resolve_replacement(replacements, value))?;
        module
            .operation_mut(operation)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!("unknown operation {operation:?}"))
            })?
            .kind = kind;
    }
    let connects = module.take_connects();
    for mut connect in connects {
        connect.value = resolve_replacement(replacements, connect.value)?;
        module
            .connect(connect.target, connect.value, connect.source)
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

fn resolve_replacement(
    replacements: &[Option<word::ValueId>],
    value: word::ValueId,
) -> Result<word::ValueId, crate::SynthError> {
    let mut current = value;
    for _ in 0..=replacements.len() {
        let Some(next) = replacements.get(current.index()).copied().flatten() else {
            return Ok(current);
        };
        current = next;
    }
    Err(crate::SynthError::invariant(
        "cyclic resource-sharing replacement",
    ))
}

fn value_type(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<word::WordType, crate::SynthError> {
    module
        .value(value)
        .map(|value| value.ty)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {value:?}")))
}
