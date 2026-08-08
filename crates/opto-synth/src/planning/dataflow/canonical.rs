// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use hashbrown::HashMap;
use opto_ir::word;
use opto_ir::{BitVal, ConstBits};
use std::num::NonZeroU32;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ValueKey {
    Signal(word::SignalRef, word::WordType),
    Constant(opto_ir::ConstBits, word::WordType),
    Unary(word::UnaryOp, word::ValueId, word::WordType),
    Binary(word::BinaryOp, word::ValueId, word::ValueId, word::WordType),
    Mux(word::ValueId, word::ValueId, word::ValueId, word::WordType),
    Concat(Box<[word::ValueId]>, word::WordType),
    Extract(word::ValueId, u32, NonZeroU32, word::WordType),
    DynamicExtract(word::ValueId, word::ValueId, NonZeroU32, word::WordType),
    DynamicInsert(word::ValueId, word::ValueId, word::ValueId, word::WordType),
    Cast(
        word::CastKind,
        word::ValueId,
        word::WordType,
        word::WordType,
    ),
}

pub(super) fn canonicalize_values(
    module: &mut word::WordModule,
    canonical: &mut [word::ValueId],
) -> Result<(), crate::SynthError> {
    canonicalize_values_by(module, canonical, |_| Some(1))
}

pub(super) fn canonicalize_values_by(
    module: &mut word::WordModule,
    canonical: &mut [word::ValueId],
    mut operation_scope: impl FnMut(word::OpId) -> Option<u64>,
) -> Result<(), crate::SynthError> {
    fold_constants_by(module, &mut |operation| {
        operation_scope(operation).is_some()
    })?;
    fold_known_values_by(module, &mut |operation| {
        operation_scope(operation).is_some()
    })?;
    let mut interned = HashMap::<(u64, ValueKey), word::ValueId>::new();
    for index in 0..module.values().len() {
        let id = word::ValueId::from_index(index).map_err(crate::SynthError::Word)?;
        if canonical[index] != id {
            continue;
        }
        let value = module
            .value(id)
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {id:?}")))?;
        let alias_or_key = match &value.kind {
            word::ValueKind::Signal(reference) => {
                AliasOrKey::Key(0, ValueKey::Signal(*reference, value.ty))
            }
            word::ValueKind::Constant(bits) => {
                AliasOrKey::Key(0, ValueKey::Constant(bits.clone(), value.ty))
            }
            word::ValueKind::Operation(operation_id) => {
                let operation = module.operation(*operation_id).ok_or_else(|| {
                    crate::SynthError::invariant(format!("unknown operation {operation_id:?}"))
                })?;
                let Some(scope) = operation_scope(*operation_id) else {
                    continue;
                };
                canonical_operation(
                    module,
                    &operation.kind,
                    value.ty,
                    canonical,
                    scope,
                    &interned,
                )?
            }
        };
        match alias_or_key {
            AliasOrKey::Alias(alias) => canonical[index] = alias,
            AliasOrKey::Key(scope, key) => {
                if let Some(&representative) = interned.get(&(scope, key.clone())) {
                    canonical[index] = representative;
                } else {
                    interned.insert((scope, key), id);
                }
            }
            AliasOrKey::Unique => {}
        }
    }
    Ok(())
}

enum AliasOrKey {
    Alias(word::ValueId),
    Key(u64, ValueKey),
    Unique,
}

fn canonical_operation(
    module: &word::WordModule,
    kind: &word::OpKind,
    result_ty: word::WordType,
    canonical: &[word::ValueId],
    scope: u64,
    interned: &HashMap<(u64, ValueKey), word::ValueId>,
) -> Result<AliasOrKey, crate::SynthError> {
    let value = |id: word::ValueId| {
        canonical.get(id.index()).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!("operation references unknown value {id:?}"))
        })
    };
    Ok(match kind {
        word::OpKind::Unary { op, arg } => {
            let arg = value(*arg)?;
            if *op == word::UnaryOp::BitNot
                && result_ty.state() == word::LogicStateKind::TwoState
                && let Some(inner) = unary_input(module, arg, word::UnaryOp::BitNot)
            {
                AliasOrKey::Alias(value(inner)?)
            } else {
                AliasOrKey::Key(scope, ValueKey::Unary(*op, arg, result_ty))
            }
        }
        word::OpKind::Binary { op, left, right } => {
            let mut left = value(*left)?;
            let mut right = value(*right)?;
            if binary_is_commutative(*op) && right < left {
                std::mem::swap(&mut left, &mut right);
            }
            let is_idempotent =
                left == right && matches!(op, word::BinaryOp::BitAnd | word::BinaryOp::BitOr);
            let is_zero_shift = matches!(
                op,
                word::BinaryOp::Shl | word::BinaryOp::Shr | word::BinaryOp::Ashr
            ) && constant_is_zero(module, right)
                && module
                    .value(left)
                    .is_some_and(|value| value.ty == result_ty);
            if let Some(alias) =
                boolean_binary_alias(module, *op, left, right, result_ty, canonical, interned)
            {
                AliasOrKey::Alias(alias)
            } else if is_idempotent || is_zero_shift {
                AliasOrKey::Alias(left)
            } else if matches!(
                op,
                word::BinaryOp::Add | word::BinaryOp::Sub | word::BinaryOp::Mul
            ) {
                // Resource operators retain distinct source identities until
                // architecture selection and provenance have consumed them.
                AliasOrKey::Unique
            } else {
                AliasOrKey::Key(scope, ValueKey::Binary(*op, left, right, result_ty))
            }
        }
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            let cond = value(*cond)?;
            let then_value = value(*then_value)?;
            let else_value = value(*else_value)?;
            if then_value == else_value {
                AliasOrKey::Alias(then_value)
            } else if constant_bit(module, then_value) == Some(true)
                && constant_bit(module, else_value) == Some(false)
                && module
                    .value(cond)
                    .is_some_and(|value| value.ty == result_ty)
            {
                AliasOrKey::Alias(cond)
            } else if constant_bit(module, then_value) == Some(false)
                && constant_bit(module, else_value) == Some(true)
                && let Some(inner) = unary_input(module, cond, word::UnaryOp::LogicalNot)
            {
                let inner = value(inner)?;
                if module
                    .value(inner)
                    .is_some_and(|value| value.ty == result_ty)
                {
                    AliasOrKey::Alias(inner)
                } else {
                    AliasOrKey::Key(
                        scope,
                        ValueKey::Mux(cond, then_value, else_value, result_ty),
                    )
                }
            } else {
                match constant_bit(module, cond) {
                    Some(false) => AliasOrKey::Alias(else_value),
                    Some(true) => AliasOrKey::Alias(then_value),
                    None => AliasOrKey::Key(
                        scope,
                        ValueKey::Mux(cond, then_value, else_value, result_ty),
                    ),
                }
            }
        }
        word::OpKind::Concat { parts } => {
            let parts = parts
                .iter()
                .map(|&part| value(part))
                .collect::<Result<Vec<_>, crate::SynthError>>()?;
            if let [part] = parts.as_slice()
                && module
                    .value(*part)
                    .is_some_and(|value| value.ty == result_ty)
            {
                AliasOrKey::Alias(*part)
            } else {
                AliasOrKey::Key(scope, ValueKey::Concat(parts.into_boxed_slice(), result_ty))
            }
        }
        word::OpKind::Extract {
            value: input,
            lsb,
            width,
        } => {
            let input = value(*input)?;
            if *lsb == 0
                && module
                    .value(input)
                    .is_some_and(|value| value.ty == result_ty)
            {
                AliasOrKey::Alias(input)
            } else {
                AliasOrKey::Key(scope, ValueKey::Extract(input, *lsb, *width, result_ty))
            }
        }
        word::OpKind::DynamicExtract {
            value: input,
            offset,
            width,
        } => AliasOrKey::Key(
            scope,
            ValueKey::DynamicExtract(value(*input)?, value(*offset)?, *width, result_ty),
        ),
        word::OpKind::DynamicInsert {
            value: input,
            offset,
            replacement,
        } => AliasOrKey::Key(
            scope,
            ValueKey::DynamicInsert(
                value(*input)?,
                value(*offset)?,
                value(*replacement)?,
                result_ty,
            ),
        ),
        word::OpKind::Cast {
            kind,
            value: input,
            target,
        } => {
            let input = value(*input)?;
            if module.value(input).is_some_and(|value| value.ty == *target) {
                AliasOrKey::Alias(input)
            } else {
                AliasOrKey::Key(scope, ValueKey::Cast(*kind, input, *target, result_ty))
            }
        }
        word::OpKind::Register(_) | word::OpKind::Latch(_) => AliasOrKey::Unique,
    })
}

fn unary_input(
    module: &word::WordModule,
    value: word::ValueId,
    expected: word::UnaryOp,
) -> Option<word::ValueId> {
    let word::ValueKind::Operation(operation) = module.value(value)?.kind else {
        return None;
    };
    match module.operation(operation)?.kind {
        word::OpKind::Unary { op, arg } if op == expected => Some(arg),
        _ => None,
    }
}

fn binary_is_commutative(op: word::BinaryOp) -> bool {
    matches!(
        op,
        word::BinaryOp::Add
            | word::BinaryOp::Mul
            | word::BinaryOp::BitAnd
            | word::BinaryOp::BitOr
            | word::BinaryOp::BitXor
            | word::BinaryOp::LogicalAnd
            | word::BinaryOp::LogicalOr
            | word::BinaryOp::Eq
            | word::BinaryOp::Ne
    )
}

fn constant_bit(module: &word::WordModule, value: word::ValueId) -> Option<bool> {
    let word::ValueKind::Constant(bits) = &module.value(value)?.kind else {
        return None;
    };
    if bits.width() != 1 {
        return None;
    }
    match bits.bit_lsb(0)? {
        opto_ir::BitVal::Zero => Some(false),
        opto_ir::BitVal::One => Some(true),
        opto_ir::BitVal::X | opto_ir::BitVal::Z => None,
    }
}

fn constant_is_zero(module: &word::WordModule, value: word::ValueId) -> bool {
    let Some(word::ValueKind::Constant(bits)) = module.value(value).map(|value| &value.kind) else {
        return false;
    };
    bits.as_slice()
        .iter()
        .all(|bit| *bit == opto_ir::BitVal::Zero)
}

fn boolean_binary_alias(
    module: &word::WordModule,
    op: word::BinaryOp,
    left: word::ValueId,
    right: word::ValueId,
    result_ty: word::WordType,
    canonical: &[word::ValueId],
    interned: &HashMap<(u64, ValueKey), word::ValueId>,
) -> Option<word::ValueId> {
    let alias_if_typed = |value: word::ValueId| {
        module
            .value(value)
            .is_some_and(|stored| stored.ty == result_ty)
            .then_some(value)
    };
    match op {
        word::BinaryOp::BitAnd | word::BinaryOp::LogicalAnd => {
            if constant_bit(module, left) == Some(false) {
                return alias_if_typed(left);
            }
            if constant_bit(module, right) == Some(false) {
                return alias_if_typed(right);
            }
            if constant_bit(module, left) == Some(true) {
                return alias_if_typed(right);
            }
            if constant_bit(module, right) == Some(true) {
                return alias_if_typed(left);
            }
            if logical_complements(module, left, right, canonical) {
                return interned_boolean(interned, false, result_ty);
            }
        }
        word::BinaryOp::BitOr | word::BinaryOp::LogicalOr => {
            if constant_bit(module, left) == Some(true) {
                return alias_if_typed(left);
            }
            if constant_bit(module, right) == Some(true) {
                return alias_if_typed(right);
            }
            if constant_bit(module, left) == Some(false) {
                return alias_if_typed(right);
            }
            if constant_bit(module, right) == Some(false) {
                return alias_if_typed(left);
            }
            if logical_complements(module, left, right, canonical) {
                return interned_boolean(interned, true, result_ty);
            }
        }
        _ => {}
    }
    None
}

fn logical_complements(
    module: &word::WordModule,
    left: word::ValueId,
    right: word::ValueId,
    canonical: &[word::ValueId],
) -> bool {
    let canonical_value = |value: word::ValueId| canonical.get(value.index()).copied();
    unary_input(module, left, word::UnaryOp::LogicalNot)
        .and_then(canonical_value)
        .is_some_and(|inner| inner == right)
        || unary_input(module, right, word::UnaryOp::LogicalNot)
            .and_then(canonical_value)
            .is_some_and(|inner| inner == left)
}

fn interned_boolean(
    interned: &HashMap<(u64, ValueKey), word::ValueId>,
    value: bool,
    ty: word::WordType,
) -> Option<word::ValueId> {
    let bit = if value { BitVal::One } else { BitVal::Zero };
    let bits = ConstBits::from_bits(vec![bit]).expect("one-bit constant width is valid");
    interned.get(&(0, ValueKey::Constant(bits, ty))).copied()
}

fn fold_constants_by(
    module: &mut word::WordModule,
    permit_operation: &mut impl FnMut(word::OpId) -> bool,
) -> Result<(), crate::SynthError> {
    let value_count = module.values().len();
    for index in 0..value_count {
        let value = word::ValueId::from_index(index).map_err(crate::SynthError::Word)?;
        let stored = module
            .value(value)
            .cloned()
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown value {value:?}")))?;
        let word::ValueKind::Operation(operation) = stored.kind else {
            continue;
        };
        if !permit_operation(operation) {
            continue;
        }
        let operation = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown operation {operation:?}"))
        })?;
        let Some(bits) = fold_operation(module, &operation.kind, stored.ty)? else {
            continue;
        };
        module
            .replace_operation_result_with_constant(value, bits)
            .map_err(crate::SynthError::from)?;
    }
    Ok(())
}

fn fold_known_values_by(
    module: &mut word::WordModule,
    permit_operation: &mut impl FnMut(word::OpId) -> bool,
) -> Result<(), crate::SynthError> {
    let mut facts = word::KnownBitsAnalysis::new(module);
    for index in 0..module.values().len() {
        let value = word::ValueId::from_index(index).map_err(crate::SynthError::Word)?;
        let Some(word::ValueKind::Operation(operation)) =
            module.value(value).map(|value| &value.kind)
        else {
            continue;
        };
        if !permit_operation(*operation) {
            continue;
        }
        let packed = facts.packed128(module, value);
        let bits = match packed {
            Some(packed) => packed.constant(),
            None => facts.constant(module, value),
        };
        if let Some(bits) = bits {
            module
                .replace_operation_result_with_constant(value, bits)
                .map_err(crate::SynthError::from)?;
        }
    }
    Ok(())
}

fn fold_operation(
    module: &word::WordModule,
    operation: &word::OpKind,
    result_ty: word::WordType,
) -> Result<Option<ConstBits>, crate::SynthError> {
    let constant = |value: word::ValueId| match &module.value(value)?.kind {
        word::ValueKind::Constant(bits) => Some(bits),
        word::ValueKind::Signal(_) | word::ValueKind::Operation(_) => None,
    };
    let bits = match operation {
        word::OpKind::Extract { value, lsb, width } => {
            let Some(input) = constant(*value) else {
                return Ok(None);
            };
            (0..width.get())
                .rev()
                .map(|offset| input.bit_lsb(*lsb + offset))
                .collect::<Option<Vec<_>>>()
        }
        word::OpKind::Cast {
            kind,
            value,
            target,
        } => {
            let Some(input) = constant(*value) else {
                return Ok(None);
            };
            Some(fold_cast(input, *kind, *target)?)
        }
        word::OpKind::Concat { parts } => {
            let mut result = Vec::new();
            for &part in parts {
                let Some(part) = constant(part) else {
                    return Ok(None);
                };
                result.extend_from_slice(part.as_slice());
            }
            Some(result)
        }
        word::OpKind::Unary { op, arg } => {
            let Some(input) = constant(*arg) else {
                return Ok(None);
            };
            Some(fold_unary(*op, input))
        }
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            let Some(select) = constant_bit(module, *cond) else {
                return Ok(None);
            };
            let selected = if select { *then_value } else { *else_value };
            let Some(selected) = constant(selected) else {
                return Ok(None);
            };
            Some(selected.as_slice().to_vec())
        }
        word::OpKind::Binary { .. }
        | word::OpKind::DynamicExtract { .. }
        | word::OpKind::DynamicInsert { .. }
        | word::OpKind::Register(_)
        | word::OpKind::Latch(_) => None,
    };
    let Some(bits) = bits else {
        return Ok(None);
    };
    if bits.len() != result_ty.width() as usize {
        return Err(crate::SynthError::invariant(
            "constant-folded result has the wrong width",
        ));
    }
    ConstBits::from_bits(bits)
        .map(Some)
        .map_err(crate::SynthError::from)
}

fn fold_cast(
    input: &ConstBits,
    kind: word::CastKind,
    target: word::WordType,
) -> Result<Vec<BitVal>, crate::SynthError> {
    let target_width = target.width() as usize;
    let input_width = input.as_slice().len();
    let bits = match kind {
        word::CastKind::Truncate => {
            let start = input_width.checked_sub(target_width).ok_or_else(|| {
                crate::SynthError::invariant("truncate target exceeds source width")
            })?;
            input.as_slice()[start..].to_vec()
        }
        word::CastKind::ZeroExtend => {
            let extension = target_width.checked_sub(input_width).ok_or_else(|| {
                crate::SynthError::invariant("zero-extension target is narrower than its source")
            })?;
            let mut bits = vec![BitVal::Zero; extension];
            bits.extend_from_slice(input.as_slice());
            bits
        }
        word::CastKind::SignExtend => {
            let extension = target_width.checked_sub(input_width).ok_or_else(|| {
                crate::SynthError::invariant("sign-extension target is narrower than its source")
            })?;
            let sign = input.as_slice().first().copied().ok_or_else(|| {
                crate::SynthError::invariant("cannot sign-extend an empty constant")
            })?;
            let mut bits = vec![sign; extension];
            bits.extend_from_slice(input.as_slice());
            bits
        }
    };
    Ok(bits)
}

fn fold_unary(op: word::UnaryOp, input: &ConstBits) -> Vec<BitVal> {
    match op {
        word::UnaryOp::BitNot => input
            .as_slice()
            .iter()
            .map(|bit| match bit {
                BitVal::Zero => BitVal::One,
                BitVal::One => BitVal::Zero,
                BitVal::X | BitVal::Z => BitVal::X,
            })
            .collect(),
        word::UnaryOp::LogicalNot => vec![match logical_value(input) {
            BitVal::Zero => BitVal::One,
            BitVal::One => BitVal::Zero,
            BitVal::X | BitVal::Z => BitVal::X,
        }],
        word::UnaryOp::ReductionAnd => vec![if input.as_slice().contains(&BitVal::Zero) {
            BitVal::Zero
        } else if input.as_slice().iter().all(|bit| *bit == BitVal::One) {
            BitVal::One
        } else {
            BitVal::X
        }],
        word::UnaryOp::ReductionOr => vec![logical_value(input)],
        word::UnaryOp::ReductionXor => {
            let unknown = input
                .as_slice()
                .iter()
                .any(|bit| matches!(bit, BitVal::X | BitVal::Z));
            let ones = input
                .as_slice()
                .iter()
                .filter(|bit| **bit == BitVal::One)
                .count();
            vec![if unknown {
                BitVal::X
            } else if ones % 2 == 0 {
                BitVal::Zero
            } else {
                BitVal::One
            }]
        }
    }
}

fn logical_value(input: &ConstBits) -> BitVal {
    if input.as_slice().contains(&BitVal::One) {
        BitVal::One
    } else if input.as_slice().iter().all(|bit| *bit == BitVal::Zero) {
        BitVal::Zero
    } else {
        BitVal::X
    }
}
