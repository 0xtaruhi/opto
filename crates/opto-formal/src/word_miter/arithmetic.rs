// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{CnfEncoder, FormalError, Lit, word};

impl CnfEncoder<'_> {
    pub(super) fn add_sub(
        &mut self,
        left: &[Lit],
        left_ty: word::WordType,
        right: &[Lit],
        right_ty: word::WordType,
        result_ty: word::WordType,
        subtract: bool,
    ) -> Result<Vec<Lit>, FormalError> {
        let left = self.resize(left, left_ty, result_ty)?;
        let right = self.resize(right, right_ty, result_ty)?;
        let mut carry = self.constant(subtract);
        let mut result = Vec::with_capacity(left.len());
        for (left, mut right) in left.into_iter().zip(right) {
            if subtract {
                right = !right;
            }
            let partial = self.xor(left, right);
            result.push(self.xor(partial, carry));
            let first = self.and(left, right);
            let second = self.and(left, carry);
            let third = self.and(right, carry);
            let remaining = self.or(second, third);
            carry = self.or(first, remaining);
        }
        Ok(result)
    }

    pub(super) fn multiply(
        &mut self,
        left: &[Lit],
        left_ty: word::WordType,
        right: &[Lit],
        right_ty: word::WordType,
        result_ty: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
        let left = self.resize(left, left_ty, result_ty)?;
        let right = self.resize(right, right_ty, result_ty)?;
        let zero = self.constant(false);
        let mut product = vec![zero; left.len()];
        for (right_index, right_bit) in right.into_iter().enumerate() {
            let row = (0..left.len())
                .map(|output_index| {
                    if output_index < right_index {
                        zero
                    } else {
                        self.and(left[output_index - right_index], right_bit)
                    }
                })
                .collect::<Vec<_>>();
            product = self.add_bits(product, row, zero)?;
        }
        Ok(product)
    }

    pub(super) fn divide(
        &mut self,
        left: &[Lit],
        left_ty: word::WordType,
        right: &[Lit],
        right_ty: word::WordType,
        result_ty: word::WordType,
        remainder: bool,
    ) -> Result<Vec<Lit>, FormalError> {
        let left = self.resize(left, left_ty, result_ty)?;
        let right = self.resize(right, right_ty, result_ty)?;
        let (dividend, divisor, negative) = if result_ty.is_signed() {
            let left_sign = *left
                .last()
                .ok_or_else(|| FormalError::invalid("signed division has no dividend sign"))?;
            let right_sign = *right
                .last()
                .ok_or_else(|| FormalError::invalid("signed division has no divisor sign"))?;
            let dividend = self.conditional_negate_bits(&left, left_sign)?;
            let divisor = self.conditional_negate_bits(&right, right_sign)?;
            let negative = if remainder {
                left_sign
            } else {
                self.xor(left_sign, right_sign)
            };
            (dividend, divisor, Some(negative))
        } else {
            (left, right, None)
        };
        let (quotient, modulus) = self.unsigned_divide_bits(&dividend, &divisor)?;
        let mut result = if remainder { modulus } else { quotient };
        if let Some(negative) = negative {
            result = self.conditional_negate_bits(&result, negative)?;
        }
        let nonzero = self.reduce_or(&divisor);
        let zero = self.constant(false);
        Ok(result
            .into_iter()
            .map(|bit| self.select(nonzero, bit, zero))
            .collect())
    }

    pub(super) fn unsigned_divide_bits(
        &mut self,
        dividend: &[Lit],
        divisor: &[Lit],
    ) -> Result<(Vec<Lit>, Vec<Lit>), FormalError> {
        if dividend.len() != divisor.len() || dividend.is_empty() {
            return Err(FormalError::invalid(
                "division operands must have equal nonzero widths",
            ));
        }
        let zero = self.constant(false);
        let mut partial = vec![zero; dividend.len() + 1];
        let mut divisor_extended = divisor.to_vec();
        divisor_extended.push(zero);
        let mut quotient = vec![zero; dividend.len()];
        for index in (0..dividend.len()).rev() {
            partial.rotate_right(1);
            partial[0] = dividend[index];
            let (difference, no_borrow) = self.subtract_bits(&partial, &divisor_extended)?;
            for (partial, difference) in partial.iter_mut().zip(difference) {
                *partial = self.select(no_borrow, difference, *partial);
            }
            quotient[index] = no_borrow;
        }
        partial.pop();
        Ok((quotient, partial))
    }

    pub(super) fn subtract_bits(
        &mut self,
        left: &[Lit],
        right: &[Lit],
    ) -> Result<(Vec<Lit>, Lit), FormalError> {
        if left.len() != right.len() || left.is_empty() {
            return Err(FormalError::invalid(
                "subtractor operands must have equal nonzero widths",
            ));
        }
        let mut carry = self.constant(true);
        let mut result = Vec::with_capacity(left.len());
        for (&left, &right) in left.iter().zip(right) {
            let right = !right;
            let propagate = self.xor(left, right);
            result.push(self.xor(propagate, carry));
            let generate = self.and(left, right);
            let propagated = self.and(propagate, carry);
            carry = self.or(generate, propagated);
        }
        Ok((result, carry))
    }

    pub(super) fn conditional_negate_bits(
        &mut self,
        value: &[Lit],
        negative: Lit,
    ) -> Result<Vec<Lit>, FormalError> {
        if value.is_empty() {
            return Err(FormalError::invalid("cannot negate an empty value"));
        }
        let mut carry = negative;
        let mut result = Vec::with_capacity(value.len());
        for &bit in value {
            let inverted = self.xor(bit, negative);
            result.push(self.xor(inverted, carry));
            carry = self.and(inverted, carry);
        }
        Ok(result)
    }

    pub(super) fn compare(
        &mut self,
        op: word::BinaryOp,
        left: &[Lit],
        left_ty: word::WordType,
        right: &[Lit],
        right_ty: word::WordType,
        result_ty: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
        let width = left_ty.width().max(right_ty.width());
        let signed = left_ty.is_signed() && right_ty.is_signed();
        let compare_ty =
            word::WordType::new(width, signed, result_ty.state()).map_err(FormalError::Word)?;
        let left = self.resize(left, left_ty, compare_ty)?;
        let right = self.resize(right, right_ty, compare_ty)?;
        let mut equal = self.constant(true);
        let mut less = self.constant(false);
        for (&left_bit, &right_bit) in left.iter().zip(&right).rev() {
            let different = self.xor(left_bit, right_bit);
            let less_here = self.and(!left_bit, right_bit);
            let first_difference_is_less = self.and(equal, less_here);
            less = self.or(less, first_difference_is_less);
            equal = self.and(equal, !different);
        }
        if signed {
            let left_sign = *left.last().ok_or_else(|| {
                FormalError::invalid("equivalence proof comparison has no sign bit")
            })?;
            let right_sign = *right.last().ok_or_else(|| {
                FormalError::invalid("equivalence proof comparison has no sign bit")
            })?;
            let signs_differ = self.xor(left_sign, right_sign);
            less = self.select(signs_differ, left_sign, less);
        }
        let result = match op {
            word::BinaryOp::Lt => less,
            word::BinaryOp::Le => self.or(less, equal),
            word::BinaryOp::Gt => !self.or(less, equal),
            word::BinaryOp::Ge => !less,
            _ => {
                return Err(FormalError::invalid(format!(
                    "equivalence proof received non-comparison operation {op:?}"
                )));
            }
        };
        Ok(vec![result])
    }
}
