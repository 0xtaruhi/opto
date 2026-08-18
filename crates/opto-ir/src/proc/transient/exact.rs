// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Conservative exact-value domain for transient procedural analysis.

use super::{ProcExprKind, TransientProcModule};
use crate::proc::{ProcExprId, ProcLocalId};
use crate::word::{
    BinaryOp, CastKind, KnownBit, OpKind, UnaryOp, ValueId, ValueKind, WordModule, WordType,
};
use crate::{BitVal, ConstBits};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ExactValue {
    width: usize,
    // Two inline words cover the common scalar and integer cases without a
    // per-value allocation. Unused high bits are always zero.
    words: SmallVec<[u64; 2]>,
}

impl ExactValue {
    pub(super) fn from_lsb_bits(bits: impl IntoIterator<Item = bool>) -> Self {
        let bits = bits.into_iter();
        let (lower, _) = bits.size_hint();
        let mut value = Self {
            width: 0,
            words: SmallVec::with_capacity(lower.div_ceil(u64::BITS as usize)),
        };
        for bit in bits {
            value.push_bit(bit);
        }
        value
    }

    fn push_bit(&mut self, bit: bool) {
        let index = self.width;
        if index.is_multiple_of(u64::BITS as usize) {
            self.words.push(0);
        }
        if bit {
            self.words[index / u64::BITS as usize] |= 1u64 << (index % u64::BITS as usize);
        }
        self.width += 1;
    }

    pub(super) fn bit(&self, index: usize) -> Option<bool> {
        (index < self.width).then(|| {
            (self.words[index / u64::BITS as usize] & (1u64 << (index % u64::BITS as usize))) != 0
        })
    }

    fn set_bit(&mut self, index: usize, bit: bool) -> Option<()> {
        if index >= self.width {
            return None;
        }
        let mask = 1u64 << (index % u64::BITS as usize);
        let word = self.words.get_mut(index / u64::BITS as usize)?;
        if bit {
            *word |= mask;
        } else {
            *word &= !mask;
        }
        Some(())
    }

    pub(super) fn bits(&self) -> impl DoubleEndedIterator<Item = bool> + ExactSizeIterator + '_ {
        (0..self.width).map(|index| {
            self.bit(index)
                .expect("exact-value iterator remains within its width")
        })
    }

    pub(super) fn from_constant(value: &ConstBits, expected: WordType) -> Option<Self> {
        if value.width() != expected.width() {
            return None;
        }
        let mut exact = Self {
            width: 0,
            words: SmallVec::with_capacity((value.width() as usize).div_ceil(u64::BITS as usize)),
        };
        for bit in 0..value.width() {
            exact.push_bit(match value.bit_lsb(bit)? {
                BitVal::Zero => Some(false),
                BitVal::One => Some(true),
                BitVal::X | BitVal::Z => None,
            }?);
        }
        Some(exact)
    }

    pub(super) fn truth(&self) -> bool {
        self.words.iter().any(|word| *word != 0)
    }

    pub(super) fn width(&self) -> usize {
        self.width
    }

    pub(super) fn to_constant(&self) -> Option<ConstBits> {
        ConstBits::from_bits(
            self.bits()
                .rev()
                .map(|bit| if bit { BitVal::One } else { BitVal::Zero })
                .collect(),
        )
        .ok()
    }

    pub(super) fn unsigned_usize(&self) -> Option<usize> {
        let mut value = 0usize;
        for (index, bit) in self.bits().enumerate() {
            if bit {
                value |= 1usize.checked_shl(u32::try_from(index).ok()?)?;
            }
        }
        Some(value)
    }

    pub(super) fn unsigned_u128(&self) -> Option<u128> {
        if self.width > u128::BITS as usize {
            return None;
        }
        let mut value = 0u128;
        for (index, bit) in self.bits().enumerate() {
            if bit {
                value |= 1u128.checked_shl(u32::try_from(index).ok()?)?;
            }
        }
        Some(value)
    }

    pub(super) fn signed_i128(&self) -> Option<i128> {
        let width = u32::try_from(self.width).ok()?;
        if width == 0 || width > i128::BITS {
            return None;
        }
        let unsigned = self.unsigned_u128()?;
        let shift = i128::BITS - width;
        Some((unsigned << shift).cast_signed() >> shift)
    }

    pub(super) fn assign_slice(
        &mut self,
        offset: usize,
        replacement: &Self,
        reverse: bool,
    ) -> Option<()> {
        if offset.checked_add(replacement.width())? > self.width {
            return None;
        }
        for (index, bit) in replacement.bits().enumerate() {
            let destination = if reverse {
                offset + replacement.width() - 1 - index
            } else {
                offset + index
            };
            self.set_bit(destination, bit)?;
        }
        Some(())
    }

    pub(super) fn resized(&self, width: usize, signed: bool) -> Self {
        let extension = signed && self.bit(self.width.saturating_sub(1)).unwrap_or(false);
        Self::from_lsb_bits((0..width).map(|index| self.bit(index).unwrap_or(extension)))
    }

    fn zero(width: usize) -> Self {
        Self::from_lsb_bits(std::iter::repeat_n(false, width))
    }

    pub(super) fn one_bit(value: bool) -> Self {
        Self::from_lsb_bits([value])
    }

    fn compare_unsigned(&self, other: &Self) -> std::cmp::Ordering {
        for (left, right) in self.bits().zip(other.bits()).rev() {
            match left.cmp(&right) {
                std::cmp::Ordering::Equal => {}
                ordering => return ordering,
            }
        }
        self.width().cmp(&other.width())
    }

    fn compare(&self, other: &Self, signed: bool) -> Option<std::cmp::Ordering> {
        if self.width() != other.width() {
            return None;
        }
        if signed {
            let left_negative = self.bit(self.width.checked_sub(1)?)?;
            let right_negative = other.bit(other.width.checked_sub(1)?)?;
            if left_negative != right_negative {
                return Some(if left_negative {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                });
            }
        }
        Some(self.compare_unsigned(other))
    }

    fn add(&self, other: &Self, subtract: bool) -> Option<Self> {
        if self.width() != other.width() {
            return None;
        }
        let mut result = Self::zero(self.width());
        let mut carry = subtract;
        for (index, (left, right)) in self.bits().zip(other.bits()).enumerate() {
            let right = right ^ subtract;
            result.set_bit(index, left ^ right ^ carry)?;
            carry = (left && right) || (carry && (left || right));
        }
        Some(result)
    }

    fn multiply(&self, other: &Self) -> Option<Self> {
        if self.width() != other.width() {
            return None;
        }
        let mut result = Self::zero(self.width());
        for (shift, enabled) in other.bits().enumerate() {
            if !enabled {
                continue;
            }
            let mut row = Self::zero(self.width());
            for (index, bit) in self.bits().enumerate() {
                if let Some(destination) =
                    index.checked_add(shift).filter(|bit| *bit < self.width())
                {
                    row.set_bit(destination, bit)?;
                }
            }
            result = result.add(&row, false)?;
        }
        Some(result)
    }

    fn divide(&self, divisor: &Self) -> Option<(Self, Self)> {
        if self.width() != divisor.width() || !divisor.truth() {
            return None;
        }
        let width = self.width();
        let mut quotient = Self::zero(width);
        let mut remainder = Self::zero(width);
        for index in (0..width).rev() {
            for bit in (1..width).rev() {
                remainder.set_bit(bit, remainder.bit(bit - 1)?)?;
            }
            remainder.set_bit(0, self.bit(index)?)?;
            if remainder.compare_unsigned(divisor) != std::cmp::Ordering::Less {
                remainder = remainder.add(divisor, true)?;
                quotient.set_bit(index, true)?;
            }
        }
        Some((quotient, remainder))
    }

    fn divide_signed(&self, divisor: &Self) -> Option<(Self, Self)> {
        if self.width() != divisor.width() || !divisor.truth() {
            return None;
        }
        let dividend_negative = self.bit(self.width.checked_sub(1)?)?;
        let divisor_negative = divisor.bit(divisor.width.checked_sub(1)?)?;
        let dividend = if dividend_negative {
            Self::zero(self.width()).add(self, true)?
        } else {
            self.clone()
        };
        let divisor = if divisor_negative {
            Self::zero(divisor.width()).add(divisor, true)?
        } else {
            divisor.clone()
        };
        let (mut quotient, mut remainder) = dividend.divide(&divisor)?;
        if dividend_negative != divisor_negative {
            quotient = Self::zero(quotient.width()).add(&quotient, true)?;
        }
        if dividend_negative {
            remainder = Self::zero(remainder.width()).add(&remainder, true)?;
        }
        Some((quotient, remainder))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ExactState(Arc<[Option<ExactValue>]>);

impl ExactState {
    pub(super) fn get(&self, index: usize) -> Option<&Option<ExactValue>> {
        self.0.get(index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Option<ExactValue>> {
        Arc::make_mut(&mut self.0).get_mut(index)
    }
}

#[derive(Clone, Copy)]
pub(super) struct TypedExact<'a> {
    pub(super) value: &'a ExactValue,
    pub(super) ty: WordType,
}

pub(super) struct ExactEvaluator<'a> {
    graph: &'a TransientProcModule,
    word: &'a WordModule,
    known_bits: RefCell<crate::word::KnownBitsAnalysis>,
}

impl<'a> ExactEvaluator<'a> {
    pub(super) fn new(graph: &'a TransientProcModule, word: &'a WordModule) -> Self {
        Self {
            graph,
            word,
            known_bits: RefCell::new(crate::word::KnownBitsAnalysis::new(word)),
        }
    }

    pub(super) fn evaluate(
        &self,
        expression: ProcExprId,
        state: &ExactState,
    ) -> Option<ExactValue> {
        let expression = self.graph.expressions.get(expression.index())?;
        let value = match &expression.kind {
            ProcExprKind::ModuleValue(value) => {
                let stored = self.word.value(*value)?;
                let bits = match &stored.kind {
                    ValueKind::Constant(bits) => bits.clone(),
                    ValueKind::Signal(_) | ValueKind::Operation(_) => {
                        self.known_bits.borrow_mut().constant(self.word, *value)?
                    }
                };
                ExactValue::from_constant(&bits, stored.ty)?
            }
            ProcExprKind::Constant(bits) => ExactValue::from_constant(bits, expression.ty)?,
            ProcExprKind::LocalRead(local) => state.get(local.index())?.clone()?,
            ProcExprKind::Unary { op, arg } => {
                let arg = self.evaluate(*arg, state)?;
                match op {
                    UnaryOp::LogicalNot => ExactValue::one_bit(!arg.truth()),
                    UnaryOp::BitNot => ExactValue::from_lsb_bits(arg.bits().map(|bit| !bit)),
                    UnaryOp::ReductionAnd => ExactValue::one_bit(arg.bits().all(|bit| bit)),
                    UnaryOp::ReductionOr => ExactValue::one_bit(arg.truth()),
                    UnaryOp::ReductionXor => {
                        ExactValue::one_bit(arg.bits().fold(false, |sum, bit| sum ^ bit))
                    }
                }
            }
            ProcExprKind::Binary { op, left, right } => {
                let left_expression = self.graph.expressions.get(left.index())?;
                let right_expression = self.graph.expressions.get(right.index())?;
                let left = self.evaluate(*left, state);
                let right = self.evaluate(*right, state);
                match op {
                    BinaryOp::LogicalAnd
                        if left.as_ref().is_some_and(|value| !value.truth())
                            || right.as_ref().is_some_and(|value| !value.truth()) =>
                    {
                        return Some(ExactValue::one_bit(false));
                    }
                    BinaryOp::LogicalOr
                        if left.as_ref().is_some_and(ExactValue::truth)
                            || right.as_ref().is_some_and(ExactValue::truth) =>
                    {
                        return Some(ExactValue::one_bit(true));
                    }
                    _ => {}
                }
                let left = left?;
                let right = right?;
                Self::binary(
                    *op,
                    TypedExact {
                        value: &left,
                        ty: left_expression.ty,
                    },
                    TypedExact {
                        value: &right,
                        ty: right_expression.ty,
                    },
                    expression.ty,
                )?
            }
            ProcExprKind::Mux {
                condition,
                then_value,
                else_value,
            } => {
                let condition = self.evaluate(*condition, state)?;
                self.evaluate(
                    if condition.truth() {
                        *then_value
                    } else {
                        *else_value
                    },
                    state,
                )?
            }
            ProcExprKind::MemoryRead { .. } | ProcExprKind::TriState { .. } => return None,
            ProcExprKind::Concat(parts) => {
                let mut bits = Vec::new();
                for part in parts.iter().rev() {
                    bits.extend(self.evaluate(*part, state)?.bits());
                }
                ExactValue::from_lsb_bits(bits)
            }
            ProcExprKind::Extract { value, lsb, width } => {
                let value = self.evaluate(*value, state)?;
                let start = usize::try_from(*lsb).ok()?;
                let end = start.checked_add(width.get() as usize)?;
                ExactValue::from_lsb_bits(
                    (start..end)
                        .map(|index| value.bit(index))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            ProcExprKind::DynamicExtract {
                value,
                offset,
                width,
            } => {
                let value = self.evaluate(*value, state)?;
                let start = self.evaluate(*offset, state)?.unsigned_usize()?;
                let end = start.checked_add(width.get() as usize)?;
                ExactValue::from_lsb_bits(
                    (start..end)
                        .map(|index| value.bit(index))
                        .collect::<Option<Vec<_>>>()?,
                )
            }
            ProcExprKind::Insert {
                value,
                lsb,
                replacement,
            } => {
                let mut value = self.evaluate(*value, state)?;
                let replacement = self.evaluate(*replacement, state)?;
                value.assign_slice(*lsb as usize, &replacement, false)?;
                value
            }
            ProcExprKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                let mut value = self.evaluate(*value, state)?;
                let offset = self.evaluate(*offset, state)?.unsigned_usize()?;
                let replacement = self.evaluate(*replacement, state)?;
                value.assign_slice(offset, &replacement, false)?;
                value
            }
            ProcExprKind::Cast { kind, value } => {
                let value_expression = self.graph.expressions.get(value.index())?;
                let value = self.evaluate(*value, state)?;
                match kind {
                    CastKind::ZeroExtend | CastKind::Truncate => {
                        value.resized(expression.ty.width() as usize, false)
                    }
                    CastKind::SignExtend => value.resized(
                        expression.ty.width() as usize,
                        value_expression.ty.is_signed(),
                    ),
                }
            }
        };
        (value.width() == expression.ty.width() as usize).then_some(value)
    }

    /// Returns a Boolean fact even when the complete value is not exact.
    ///
    /// Comparisons use conservative extrema derived from exact locals and
    /// module known-bit facts. This proves, for example, that an unsigned
    /// maximum induction value cannot be less than any value of a same-width
    /// runtime bound, without choosing a particular runtime input.
    pub(super) fn evaluate_truth(
        &self,
        expression: ProcExprId,
        state: &ExactState,
    ) -> Option<bool> {
        if let Some(value) = self.evaluate(expression, state) {
            return Some(value.truth());
        }
        let stored = self.graph.expressions.get(expression.index())?;
        match &stored.kind {
            ProcExprKind::Unary {
                op: UnaryOp::LogicalNot,
                arg,
            } => self.evaluate_truth(*arg, state).map(|truth| !truth),
            ProcExprKind::Binary { op, left, right } => match op {
                BinaryOp::LogicalAnd => {
                    let left = self.evaluate_truth(*left, state);
                    let right = self.evaluate_truth(*right, state);
                    if left == Some(false) || right == Some(false) {
                        Some(false)
                    } else if left == Some(true) && right == Some(true) {
                        Some(true)
                    } else {
                        None
                    }
                }
                BinaryOp::LogicalOr => {
                    let left = self.evaluate_truth(*left, state);
                    let right = self.evaluate_truth(*right, state);
                    if left == Some(true) || right == Some(true) {
                        Some(true)
                    } else if left == Some(false) && right == Some(false) {
                        Some(false)
                    } else {
                        None
                    }
                }
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge => self.comparison_truth(*op, *left, *right, state),
                _ => self.truth_from_facts(expression, state),
            },
            ProcExprKind::Mux {
                condition,
                then_value,
                else_value,
            } => match self.evaluate_truth(*condition, state) {
                Some(true) => self.evaluate_truth(*then_value, state),
                Some(false) => self.evaluate_truth(*else_value, state),
                None => {
                    let then_truth = self.evaluate_truth(*then_value, state);
                    (then_truth == self.evaluate_truth(*else_value, state))
                        .then_some(then_truth)
                        .flatten()
                }
            },
            _ => self.truth_from_facts(expression, state),
        }
    }

    pub(super) fn expression_bounds(
        &self,
        expression: ProcExprId,
        state: &ExactState,
    ) -> Option<ValueBounds> {
        let ty = self.graph.expressions.get(expression.index())?.ty;
        self.bounds(expression, state, ty.width() as usize, ty.is_signed())
    }

    fn truth_from_facts(&self, expression: ProcExprId, state: &ExactState) -> Option<bool> {
        let facts = self.bit_facts(expression, state)?;
        if facts.contains(&Some(true)) {
            Some(true)
        } else if facts.iter().all(Option::is_some) {
            Some(false)
        } else {
            None
        }
    }

    fn comparison_truth(
        &self,
        op: BinaryOp,
        left: ProcExprId,
        right: ProcExprId,
        state: &ExactState,
    ) -> Option<bool> {
        use std::cmp::Ordering;

        let left_ty = self.graph.expressions.get(left.index())?.ty;
        let right_ty = self.graph.expressions.get(right.index())?.ty;
        let width = left_ty.width().max(right_ty.width()) as usize;
        let signed = left_ty.is_signed() && right_ty.is_signed();
        let left_bounds = self.bounds(left, state, width, signed)?;
        let right_bounds = self.bounds(right, state, width, signed)?;
        let compare = |left: &ExactValue, right: &ExactValue| left.compare(right, signed);
        let disjoint = compare(&left_bounds.maximum, &right_bounds.minimum)? == Ordering::Less
            || compare(&right_bounds.maximum, &left_bounds.minimum)? == Ordering::Less;
        let both_singleton = left_bounds.minimum == left_bounds.maximum
            && right_bounds.minimum == right_bounds.maximum;
        match op {
            BinaryOp::Eq if disjoint => Some(false),
            BinaryOp::Eq if both_singleton => Some(left_bounds.minimum == right_bounds.minimum),
            BinaryOp::Ne if disjoint => Some(true),
            BinaryOp::Ne if both_singleton => Some(left_bounds.minimum != right_bounds.minimum),
            BinaryOp::Lt
                if compare(&left_bounds.maximum, &right_bounds.minimum)? == Ordering::Less =>
            {
                Some(true)
            }
            BinaryOp::Lt
                if compare(&left_bounds.minimum, &right_bounds.maximum)? != Ordering::Less =>
            {
                Some(false)
            }
            BinaryOp::Le
                if compare(&left_bounds.maximum, &right_bounds.minimum)? != Ordering::Greater =>
            {
                Some(true)
            }
            BinaryOp::Le
                if compare(&left_bounds.minimum, &right_bounds.maximum)? == Ordering::Greater =>
            {
                Some(false)
            }
            BinaryOp::Gt
                if compare(&left_bounds.minimum, &right_bounds.maximum)? == Ordering::Greater =>
            {
                Some(true)
            }
            BinaryOp::Gt
                if compare(&left_bounds.maximum, &right_bounds.minimum)? != Ordering::Greater =>
            {
                Some(false)
            }
            BinaryOp::Ge
                if compare(&left_bounds.minimum, &right_bounds.maximum)? != Ordering::Less =>
            {
                Some(true)
            }
            BinaryOp::Ge
                if compare(&left_bounds.maximum, &right_bounds.minimum)? == Ordering::Less =>
            {
                Some(false)
            }
            _ => None,
        }
    }

    fn bounds(
        &self,
        expression: ProcExprId,
        state: &ExactState,
        width: usize,
        signed: bool,
    ) -> Option<ValueBounds> {
        let stored = self.graph.expressions.get(expression.index())?;
        if let ProcExprKind::Cast { kind, value } = &stored.kind {
            let source_ty = self.graph.expressions.get(value.index())?.ty;
            match kind {
                CastKind::SignExtend if signed && source_ty.is_signed() => {
                    let source_bounds =
                        self.bounds(*value, state, source_ty.width() as usize, true)?;
                    return Some(ValueBounds {
                        minimum: source_bounds.minimum.resized(width, true),
                        maximum: source_bounds.maximum.resized(width, true),
                    });
                }
                CastKind::ZeroExtend => {
                    let source_bounds =
                        self.bounds(*value, state, source_ty.width() as usize, false)?;
                    return Some(ValueBounds {
                        minimum: source_bounds.minimum.resized(width, false),
                        maximum: source_bounds.maximum.resized(width, false),
                    });
                }
                CastKind::SignExtend | CastKind::Truncate => {}
            }
        }
        if let ProcExprKind::ModuleValue(value) = &stored.kind
            && let Some(bounds) = self.module_value_bounds(*value, width, signed)
        {
            return Some(bounds);
        }
        Self::bounds_from_facts(self.bit_facts(expression, state)?, width, signed)
    }

    fn module_value_bounds(
        &self,
        value: ValueId,
        width: usize,
        signed: bool,
    ) -> Option<ValueBounds> {
        let stored = self.word.value(value)?;
        if let ValueKind::Operation(operation) = &stored.kind
            && let OpKind::Cast {
                kind,
                value: source,
                ..
            } = &self.word.operation(*operation)?.kind
        {
            let source_ty = self.word.value(*source)?.ty;
            match kind {
                CastKind::SignExtend if signed && source_ty.is_signed() => {
                    let source_bounds =
                        self.module_value_bounds(*source, source_ty.width() as usize, true)?;
                    return Some(ValueBounds {
                        minimum: source_bounds.minimum.resized(width, true),
                        maximum: source_bounds.maximum.resized(width, true),
                    });
                }
                CastKind::ZeroExtend => {
                    let source_bounds =
                        self.module_value_bounds(*source, source_ty.width() as usize, false)?;
                    return Some(ValueBounds {
                        minimum: source_bounds.minimum.resized(width, false),
                        maximum: source_bounds.maximum.resized(width, false),
                    });
                }
                CastKind::SignExtend | CastKind::Truncate => {}
            }
        }
        let facts = (0..stored.ty.width())
            .map(
                |index| match self.known_bits.borrow_mut().bit(self.word, value, index) {
                    KnownBit::Zero => Some(false),
                    KnownBit::One => Some(true),
                    KnownBit::Unknown => None,
                },
            )
            .collect();
        Self::bounds_from_facts(facts, width, signed)
    }

    fn bounds_from_facts(
        mut facts: Vec<Option<bool>>,
        width: usize,
        signed: bool,
    ) -> Option<ValueBounds> {
        let extension = if signed {
            facts.last().copied().flatten()
        } else {
            Some(false)
        };
        facts.resize(width, extension);
        facts.truncate(width);
        let mut minimum = facts
            .iter()
            .map(|bit| bit.unwrap_or(false))
            .collect::<Vec<_>>();
        let mut maximum = facts
            .iter()
            .map(|bit| bit.unwrap_or(true))
            .collect::<Vec<_>>();
        if signed {
            let sign = facts.last().copied().flatten();
            *minimum.last_mut()? = sign.unwrap_or(true);
            *maximum.last_mut()? = sign.unwrap_or(false);
        }
        Some(ValueBounds {
            minimum: ExactValue::from_lsb_bits(minimum),
            maximum: ExactValue::from_lsb_bits(maximum),
        })
    }

    fn bit_facts(&self, expression: ProcExprId, state: &ExactState) -> Option<Vec<Option<bool>>> {
        if let Some(value) = self.evaluate(expression, state) {
            return Some(value.bits().map(Some).collect());
        }
        let expression = self.graph.expressions.get(expression.index())?;
        let unknown = || vec![None; expression.ty.width() as usize];
        let facts = match &expression.kind {
            ProcExprKind::ModuleValue(value) => {
                let stored = self.word.value(*value)?;
                (0..stored.ty.width())
                    .map(
                        |index| match self.known_bits.borrow_mut().bit(self.word, *value, index) {
                            KnownBit::Zero => Some(false),
                            KnownBit::One => Some(true),
                            KnownBit::Unknown => None,
                        },
                    )
                    .collect()
            }
            ProcExprKind::Constant(bits) => (0..bits.width())
                .map(|index| match bits.bit_lsb(index) {
                    Some(BitVal::Zero) => Some(false),
                    Some(BitVal::One) => Some(true),
                    Some(BitVal::X | BitVal::Z) | None => None,
                })
                .collect(),
            ProcExprKind::LocalRead(local) => state
                .get(local.index())?
                .as_ref()
                .map_or_else(unknown, |value| value.bits().map(Some).collect()),
            ProcExprKind::Concat(parts) => {
                let mut bits = Vec::with_capacity(expression.ty.width() as usize);
                for part in parts.iter().rev() {
                    bits.extend(self.bit_facts(*part, state)?);
                }
                bits
            }
            ProcExprKind::Extract { value, lsb, width } => {
                let value = self.bit_facts(*value, state)?;
                let start = *lsb as usize;
                let end = start.checked_add(width.get() as usize)?;
                value.get(start..end)?.to_vec()
            }
            ProcExprKind::DynamicExtract {
                value,
                offset,
                width,
            } => {
                let value = self.bit_facts(*value, state)?;
                let start = self.evaluate(*offset, state)?.unsigned_usize()?;
                let end = start.checked_add(width.get() as usize)?;
                value.get(start..end)?.to_vec()
            }
            ProcExprKind::Cast { kind, value } => {
                let value_expression = self.graph.expressions.get(value.index())?;
                let mut bits = self.bit_facts(*value, state)?;
                let extension = match kind {
                    CastKind::SignExtend if value_expression.ty.is_signed() => {
                        bits.last().copied().flatten()
                    }
                    CastKind::ZeroExtend | CastKind::SignExtend | CastKind::Truncate => Some(false),
                };
                bits.resize(expression.ty.width() as usize, extension);
                bits.truncate(expression.ty.width() as usize);
                bits
            }
            ProcExprKind::Mux {
                condition,
                then_value,
                else_value,
            } => match self.evaluate_truth(*condition, state) {
                Some(true) => self.bit_facts(*then_value, state)?,
                Some(false) => self.bit_facts(*else_value, state)?,
                None => self
                    .bit_facts(*then_value, state)?
                    .into_iter()
                    .zip(self.bit_facts(*else_value, state)?)
                    .map(|(then_bit, else_bit)| {
                        (then_bit == else_bit).then_some(then_bit).flatten()
                    })
                    .collect(),
            },
            _ => unknown(),
        };
        (facts.len() == expression.ty.width() as usize).then_some(facts)
    }

    pub(super) fn binary(
        op: BinaryOp,
        left: TypedExact<'_>,
        right: TypedExact<'_>,
        result_ty: WordType,
    ) -> Option<ExactValue> {
        use std::cmp::Ordering;

        let result_width = result_ty.width() as usize;
        let common_signed = left.ty.is_signed() && right.ty.is_signed();
        let left_signed = left.ty.is_signed();
        let computation_width = match op {
            BinaryOp::LogicalAnd
            | BinaryOp::LogicalOr
            | BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => left.value.width().max(right.value.width()),
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr => left.value.width(),
            _ => result_width,
        };
        let shift = if matches!(op, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr) {
            Some(right.value.unsigned_usize()?)
        } else {
            None
        };
        let left = left.value.resized(computation_width, common_signed);
        let right = right.value.resized(computation_width, common_signed);
        Some(match op {
            BinaryOp::Add => left.add(&right, false)?,
            BinaryOp::Sub => left.add(&right, true)?,
            BinaryOp::Mul => left.multiply(&right)?,
            BinaryOp::Div if common_signed => left.divide_signed(&right)?.0,
            BinaryOp::Mod if common_signed => left.divide_signed(&right)?.1,
            BinaryOp::Div => left.divide(&right)?.0,
            BinaryOp::Mod => left.divide(&right)?.1,
            BinaryOp::BitAnd => ExactValue::from_lsb_bits(
                left.bits()
                    .zip(right.bits())
                    .map(|(left, right)| left && right),
            ),
            BinaryOp::BitOr => ExactValue::from_lsb_bits(
                left.bits()
                    .zip(right.bits())
                    .map(|(left, right)| left || right),
            ),
            BinaryOp::BitXor => ExactValue::from_lsb_bits(
                left.bits()
                    .zip(right.bits())
                    .map(|(left, right)| left ^ right),
            ),
            BinaryOp::LogicalAnd => ExactValue::one_bit(left.truth() && right.truth()),
            BinaryOp::LogicalOr => ExactValue::one_bit(left.truth() || right.truth()),
            BinaryOp::Eq => ExactValue::one_bit(left == right),
            BinaryOp::Ne => ExactValue::one_bit(left != right),
            BinaryOp::Lt => {
                ExactValue::one_bit(left.compare(&right, common_signed)? == Ordering::Less)
            }
            BinaryOp::Le => {
                ExactValue::one_bit(left.compare(&right, common_signed)? != Ordering::Greater)
            }
            BinaryOp::Gt => {
                ExactValue::one_bit(left.compare(&right, common_signed)? == Ordering::Greater)
            }
            BinaryOp::Ge => {
                ExactValue::one_bit(left.compare(&right, common_signed)? != Ordering::Less)
            }
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr => {
                let shift = shift.expect("shift operations computed an exact shift amount");
                let fill = op == BinaryOp::Ashr
                    && left_signed
                    && left.bit(left.width().saturating_sub(1)).unwrap_or(false);
                let mut result = ExactValue::from_lsb_bits(std::iter::repeat_n(fill, result_width));
                for index in 0..result_width {
                    let source = match op {
                        BinaryOp::Shl => index.checked_sub(shift),
                        BinaryOp::Shr | BinaryOp::Ashr => index.checked_add(shift),
                        _ => unreachable!(),
                    };
                    if let Some(source) = source.filter(|source| *source < result_width) {
                        result.set_bit(index, left.bit(source)?)?;
                    } else if op == BinaryOp::Shl || op == BinaryOp::Shr {
                        result.set_bit(index, false)?;
                    }
                }
                result
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ValueBounds {
    pub(super) minimum: ExactValue,
    pub(super) maximum: ExactValue,
}

pub(super) fn unknown_state(local_count: usize) -> ExactState {
    ExactState(vec![None; local_count].into())
}

pub(super) fn local_slot(
    state: &mut ExactState,
    local: ProcLocalId,
) -> Option<&mut Option<ExactValue>> {
    state.get_mut(local.index())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word::LogicStateKind;

    #[test]
    fn exact_constant_round_trip_preserves_display_bit_order() {
        let ty = WordType::new(4, false, LogicStateKind::FourState).unwrap();
        let bits = ConstBits::from_bin_str("1010").unwrap();
        let exact = ExactValue::from_constant(&bits, ty).unwrap();
        assert_eq!(exact.to_constant().unwrap(), bits);
        assert_eq!(exact.unsigned_usize(), Some(10));
    }

    #[test]
    fn exact_signed_division_truncates_toward_zero() {
        let ty = WordType::new(8, true, LogicStateKind::TwoState).unwrap();
        let dividend =
            ExactValue::from_constant(&ConstBits::from_bin_str("11111001").unwrap(), ty).unwrap();
        let divisor =
            ExactValue::from_constant(&ConstBits::from_bin_str("00000011").unwrap(), ty).unwrap();

        let (quotient, remainder) = dividend.divide_signed(&divisor).unwrap();

        assert_eq!(quotient.to_constant().unwrap().to_string(), "11111110");
        assert_eq!(remainder.to_constant().unwrap().to_string(), "11111111");
    }

    #[test]
    fn common_exact_values_stay_inline() {
        let integer = ExactValue::from_lsb_bits(std::iter::repeat_n(true, 32));
        let wide_scalar = ExactValue::from_lsb_bits(std::iter::repeat_n(true, 128));

        assert!(!integer.words.spilled());
        assert!(!wide_scalar.words.spilled());
    }

    #[test]
    fn exact_state_clones_share_storage_until_mutation() {
        let mut original = unknown_state(4);
        *local_slot(&mut original, ProcLocalId::from_index(0).unwrap()).unwrap() =
            Some(ExactValue::one_bit(false));
        let mut clone = original.clone();
        assert!(Arc::ptr_eq(&original.0, &clone.0));

        *local_slot(&mut clone, ProcLocalId::from_index(0).unwrap()).unwrap() =
            Some(ExactValue::one_bit(true));
        assert!(!Arc::ptr_eq(&original.0, &clone.0));
        assert_ne!(original, clone);
    }
}
