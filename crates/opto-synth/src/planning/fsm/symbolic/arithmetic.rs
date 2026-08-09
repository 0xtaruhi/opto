// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{Lit, SymbolicResult, WordLogicEncoder, word};

impl WordLogicEncoder<'_> {
    pub(super) fn add_sub(
        &mut self,
        left: &[Lit],
        left_ty: word::WordType,
        right: &[Lit],
        right_ty: word::WordType,
        result_ty: word::WordType,
        subtract: bool,
    ) -> SymbolicResult<Vec<Lit>> {
        let left = Self::resize(left, left_ty, result_ty)?;
        let right = Self::resize(right, right_ty, result_ty)?;
        let mut carry = if subtract { Lit::TRUE } else { Lit::FALSE };
        let mut result = Vec::with_capacity(left.len());
        for (left, mut right) in left.into_iter().zip(right) {
            if subtract {
                right = right.inverted();
            }
            let partial = self.xor(left, right)?;
            result.push(self.xor(partial, carry)?);
            let generate = self.and(left, right)?;
            let propagated = self.and(partial, carry)?;
            carry = self.or(generate, propagated)?;
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
    ) -> SymbolicResult<Vec<Lit>> {
        let left = Self::resize(left, left_ty, result_ty)?;
        let right = Self::resize(right, right_ty, result_ty)?;
        let mut product = vec![Lit::FALSE; left.len()];
        for (right_index, right_bit) in right.into_iter().enumerate() {
            let row = (0..left.len())
                .map(|output_index| {
                    if output_index < right_index {
                        Ok(Lit::FALSE)
                    } else {
                        self.and(left[output_index - right_index], right_bit)
                    }
                })
                .collect::<SymbolicResult<Vec<_>>>()?;
            product = self.add_bits(product, row, Lit::FALSE)?;
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
    ) -> SymbolicResult<Vec<Lit>> {
        let left = Self::resize(left, left_ty, result_ty)?;
        let right = Self::resize(right, right_ty, result_ty)?;
        let (dividend, divisor, negative) = if result_ty.is_signed() {
            let left_sign = *left.last().ok_or_else(|| {
                crate::SynthError::invariant("symbolic signed division has no dividend sign")
            })?;
            let right_sign = *right.last().ok_or_else(|| {
                crate::SynthError::invariant("symbolic signed division has no divisor sign")
            })?;
            let dividend = self.conditional_negate_bits(&left, left_sign)?;
            let divisor = self.conditional_negate_bits(&right, right_sign)?;
            let negative = if remainder {
                left_sign
            } else {
                self.xor(left_sign, right_sign)?
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
        let nonzero = self.reduce_or(&divisor)?;
        result
            .into_iter()
            .map(|bit| self.select(nonzero, bit, Lit::FALSE))
            .collect()
    }

    fn unsigned_divide_bits(
        &mut self,
        dividend: &[Lit],
        divisor: &[Lit],
    ) -> SymbolicResult<(Vec<Lit>, Vec<Lit>)> {
        if dividend.len() != divisor.len() || dividend.is_empty() {
            return Err(crate::SynthError::invariant(
                "symbolic divider operands must have equal nonzero widths",
            )
            .into());
        }
        let mut partial = vec![Lit::FALSE; dividend.len() + 1];
        let mut divisor_extended = divisor.to_vec();
        divisor_extended.push(Lit::FALSE);
        let mut quotient = vec![Lit::FALSE; dividend.len()];
        for index in (0..dividend.len()).rev() {
            partial.rotate_right(1);
            partial[0] = dividend[index];
            let (difference, no_borrow) = self.subtract_bits(&partial, &divisor_extended)?;
            for (partial, difference) in partial.iter_mut().zip(difference) {
                *partial = self.select(no_borrow, difference, *partial)?;
            }
            quotient[index] = no_borrow;
        }
        partial.pop();
        Ok((quotient, partial))
    }

    fn subtract_bits(&mut self, left: &[Lit], right: &[Lit]) -> SymbolicResult<(Vec<Lit>, Lit)> {
        if left.len() != right.len() || left.is_empty() {
            return Err(crate::SynthError::invariant(
                "symbolic subtractor operands must have equal nonzero widths",
            )
            .into());
        }
        let mut carry = Lit::TRUE;
        let mut result = Vec::with_capacity(left.len());
        for (&left, &right) in left.iter().zip(right) {
            let right = right.inverted();
            let propagate = self.xor(left, right)?;
            result.push(self.xor(propagate, carry)?);
            let generate = self.and(left, right)?;
            let propagated = self.and(propagate, carry)?;
            carry = self.or(generate, propagated)?;
        }
        Ok((result, carry))
    }

    fn conditional_negate_bits(
        &mut self,
        value: &[Lit],
        negative: Lit,
    ) -> SymbolicResult<Vec<Lit>> {
        if value.is_empty() {
            return Err(
                crate::SynthError::invariant("cannot symbolically negate an empty value").into(),
            );
        }
        let mut carry = negative;
        let mut result = Vec::with_capacity(value.len());
        for &bit in value {
            let inverted = self.xor(bit, negative)?;
            result.push(self.xor(inverted, carry)?);
            carry = self.and(inverted, carry)?;
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
    ) -> SymbolicResult<Vec<Lit>> {
        let width = left_ty.width().max(right_ty.width());
        let signed = left_ty.is_signed() && right_ty.is_signed();
        let compare_ty = word::WordType::new(width, signed, result_ty.state())
            .map_err(crate::SynthError::from)?;
        let left = Self::resize(left, left_ty, compare_ty)?;
        let right = Self::resize(right, right_ty, compare_ty)?;
        let mut equal = Lit::TRUE;
        let mut less = Lit::FALSE;
        for (&left_bit, &right_bit) in left.iter().zip(&right).rev() {
            let different = self.xor(left_bit, right_bit)?;
            let less_here = self.and(left_bit.inverted(), right_bit)?;
            let first_difference = self.and(equal, less_here)?;
            less = self.or(less, first_difference)?;
            equal = self.and(equal, different.inverted())?;
        }
        if signed {
            let left_sign = *left.last().ok_or_else(|| {
                crate::SynthError::invariant("symbolic comparison has no left sign bit")
            })?;
            let right_sign = *right.last().ok_or_else(|| {
                crate::SynthError::invariant("symbolic comparison has no right sign bit")
            })?;
            let signs_differ = self.xor(left_sign, right_sign)?;
            less = self.select(signs_differ, left_sign, less)?;
        }
        let result = match op {
            word::BinaryOp::Lt => less,
            word::BinaryOp::Le => self.or(less, equal)?,
            word::BinaryOp::Gt => self.or(less, equal)?.inverted(),
            word::BinaryOp::Ge => less.inverted(),
            _ => {
                return Err(crate::SynthError::invariant(
                    "symbolic comparator received a non-comparison operation",
                )
                .into());
            }
        };
        Ok(vec![result])
    }

    pub(super) fn shift(
        &mut self,
        op: word::BinaryOp,
        value: &[Lit],
        value_ty: word::WordType,
        amount: &[Lit],
        result_ty: word::WordType,
    ) -> SymbolicResult<Vec<Lit>> {
        let mut current = Self::resize(value, value_ty, result_ty)?;
        let width = result_ty.width();
        let relevant_stages = if width <= 1 {
            0
        } else {
            u32::BITS - (width - 1).leading_zeros()
        };
        let stages = amount.len().min(relevant_stages as usize);
        for (stage, &control) in amount.iter().take(stages).enumerate() {
            let stage = u32::try_from(stage)
                .map_err(|_| crate::SynthError::capacity("symbolic shift stage count"))?;
            let distance = 1usize.checked_shl(stage).ok_or_else(|| {
                crate::SynthError::capacity("symbolic shift distance exceeds addressable capacity")
            })?;
            let right_fill = if op == word::BinaryOp::Ashr && result_ty.is_signed() {
                current[current.len() - 1]
            } else {
                Lit::FALSE
            };
            let mut next = Vec::with_capacity(current.len());
            for index in 0..current.len() {
                let shifted = match op {
                    word::BinaryOp::Shl if index >= distance => current[index - distance],
                    word::BinaryOp::Shr | word::BinaryOp::Ashr
                        if index + distance < current.len() =>
                    {
                        current[index + distance]
                    }
                    word::BinaryOp::Shl => Lit::FALSE,
                    word::BinaryOp::Shr | word::BinaryOp::Ashr => right_fill,
                    _ => {
                        return Err(crate::SynthError::invariant(
                            "symbolic shifter received a non-shift operation",
                        )
                        .into());
                    }
                };
                next.push(self.select(control, shifted, current[index])?);
            }
            current = next;
        }
        if amount.len() > stages {
            let overflow = self.reduce_or(&amount[stages..])?;
            let fill = if op == word::BinaryOp::Ashr && result_ty.is_signed() {
                current[current.len() - 1]
            } else {
                Lit::FALSE
            };
            for bit in &mut current {
                *bit = self.select(overflow, fill, *bit)?;
            }
        }
        Ok(current)
    }

    fn add_bits(
        &mut self,
        left: Vec<Lit>,
        right: Vec<Lit>,
        mut carry: Lit,
    ) -> SymbolicResult<Vec<Lit>> {
        if left.len() != right.len() || left.is_empty() {
            return Err(crate::SynthError::invariant(
                "symbolic adder inputs must have equal nonzero widths",
            )
            .into());
        }
        let width = left.len();
        let mut result = Vec::with_capacity(width);
        for (index, (left, right)) in left.into_iter().zip(right).enumerate() {
            let partial = self.xor(left, right)?;
            result.push(self.xor(partial, carry)?);
            if index + 1 != width {
                let generate = self.and(left, right)?;
                let propagated = self.and(partial, carry)?;
                carry = self.or(generate, propagated)?;
            }
        }
        Ok(result)
    }
}
