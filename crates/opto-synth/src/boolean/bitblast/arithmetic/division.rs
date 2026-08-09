// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBlaster, BitVal, word};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnsignedMagic {
    multiplier: u64,
    shift: u32,
    add: bool,
}

impl BitBlaster<'_> {
    pub(in crate::boolean::bitblast) fn divide_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        remainder: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let width = result_ty.width() as usize;
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let signed = result_ty.is_signed();
        let mut dividend = Vec::with_capacity(width);
        let mut divisor = Vec::with_capacity(width);
        for index in 0..result_ty.width() {
            dividend.push(self.resized_bit(left_span, left_ty, index, signed, source)?);
            divisor.push(self.resized_bit(right_span, right_ty, index, signed, source)?);
        }

        let dividend_constant = self.constant_vector(&dividend);
        let divisor_constant = self.constant_vector(&divisor);
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;
        let (dividend, divisor, result_negative) = if signed {
            let dividend_sign = dividend[width - 1];
            let divisor_sign = divisor[width - 1];
            let dividend = self.conditional_negate_vector(&dividend, dividend_sign, source)?;
            let divisor = self.conditional_negate_vector(&divisor, divisor_sign, source)?;
            let negative = if remainder {
                dividend_sign
            } else {
                self.emit_binary(word::BinaryOp::BitXor, dividend_sign, divisor_sign, source)?
            };
            (dividend, divisor, Some(negative))
        } else {
            (dividend, divisor, None)
        };

        let dividend_constant =
            dividend_constant.map(|bits| twos_complement_magnitude(bits, signed));
        let divisor_constant = divisor_constant.map(|bits| twos_complement_magnitude(bits, signed));
        let (quotient, modulus) = if let Some(constant) = divisor_constant.as_deref() {
            self.divide_by_constant(&dividend, constant, remainder, result_ty.state(), source)?
        } else if let Some(constant) = dividend_constant.as_deref() {
            self.divide_constant_numerator(constant, &divisor, result_ty.state(), source)?
        } else {
            self.restoring_divide(&dividend, &divisor, result_ty.state(), source)?
        };
        let mut result = if remainder { modulus } else { quotient };
        if let Some(negative) = result_negative {
            result = self.conditional_negate_vector(&result, negative, source)?;
        }

        // Division by zero is undefined in the RTL language. The synthesis
        // boundary resolves undefined bits to zero consistently with X
        // constants elsewhere in the bitblaster.
        if divisor_constant.is_none() {
            let nonzero = self.reduce_values(divisor, word::BinaryOp::BitOr, source)?;
            for bit in &mut result {
                *bit = self.emit_mux(nonzero, *bit, zero, source)?;
            }
        }
        Ok(result)
    }

    fn divide_by_constant(
        &mut self,
        dividend: &[word::ValueId],
        divisor: &[bool],
        need_remainder: bool,
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<(Vec<word::ValueId>, Vec<word::ValueId>), crate::SynthError> {
        let width = dividend.len();
        let zero = self.constant(BitVal::Zero, state, source)?;
        let Some(divisor_value) = bits_to_u64(divisor) else {
            let significant = significant_bits(divisor);
            if significant == 0 {
                return Ok((vec![zero; width], vec![zero; width]));
            }
            return self.restoring_divide_constant(
                dividend,
                &divisor[..significant],
                state,
                source,
            );
        };
        if divisor_value == 0 {
            return Ok((vec![zero; width], vec![zero; width]));
        }
        if divisor_value.is_power_of_two() {
            let shift = divisor_value.trailing_zeros() as usize;
            let quotient = (0..width)
                .map(|index| dividend.get(index + shift).copied().unwrap_or(zero))
                .collect();
            let modulus = (0..width)
                .map(|index| if index < shift { dividend[index] } else { zero })
                .collect();
            return Ok((quotient, modulus));
        }
        if width <= u64::BITS as usize {
            let magic = unsigned_magic(
                u32::try_from(width).expect("invariant division width is at most 64"),
                divisor_value,
            )
            .ok_or_else(|| {
                crate::SynthError::invariant("failed to derive invariant-division magic")
            })?;
            let multiplier = lsb_bits(magic.multiplier, width);
            let magic_work = nonadjacent_digit_count(&multiplier)
                .saturating_mul(width.saturating_mul(2))
                .saturating_add(usize::from(magic.add).saturating_mul(width.saturating_mul(2)))
                .saturating_add(if need_remainder {
                    nonadjacent_digit_count(&lsb_bits(divisor_value, width)).saturating_mul(width)
                } else {
                    0
                });
            let restoring_work = width.saturating_mul(significant_bits(divisor).saturating_add(1));
            if magic_work >= restoring_work {
                let significant = significant_bits(divisor);
                return self.restoring_divide_constant(
                    dividend,
                    &divisor[..significant],
                    state,
                    source,
                );
            }
            let product = self.constant_multiply_vector(
                dividend,
                &multiplier,
                width * 2,
                None,
                state,
                source,
            )?;
            let mut quotient = product[width..].to_vec();
            if magic.add {
                let (difference, _) =
                    self.add_sub_vectors(dividend, &quotient, true, state, source)?;
                let mut half = Vec::with_capacity(width);
                half.extend_from_slice(&difference[1..]);
                half.push(zero);
                quotient = self
                    .add_sub_vectors(&half, &quotient, false, state, source)?
                    .0;
            }
            quotient = logical_right_shift(&quotient, magic.shift as usize, zero);
            let modulus = if need_remainder {
                let divisor_bits = lsb_bits(divisor_value, width);
                let product = self.constant_multiply_vector(
                    &quotient,
                    &divisor_bits,
                    width,
                    None,
                    state,
                    source,
                )?;
                self.add_sub_vectors(dividend, &product, true, state, source)?
                    .0
            } else {
                vec![zero; width]
            };
            return Ok((quotient, modulus));
        }
        let significant = significant_bits(divisor);
        self.restoring_divide_constant(dividend, &divisor[..significant], state, source)
    }

    fn divide_constant_numerator(
        &mut self,
        dividend: &[bool],
        divisor: &[word::ValueId],
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<(Vec<word::ValueId>, Vec<word::ValueId>), crate::SynthError> {
        let width = divisor.len();
        let zero = self.constant(BitVal::Zero, state, source)?;
        let significant = significant_bits(dividend).max(1);
        let constant_bits = dividend[..significant]
            .iter()
            .map(|&bit| if bit { BitVal::One } else { BitVal::Zero })
            .map(|bit| self.constant(bit, state, source))
            .collect::<Result<Vec<_>, _>>()?;
        let (small_quotient, small_remainder) =
            self.restoring_divide(&constant_bits, &divisor[..significant], state, source)?;
        let high_nonzero = if significant < width {
            Some(self.reduce_values(
                divisor[significant..].to_vec(),
                word::BinaryOp::BitOr,
                source,
            )?)
        } else {
            None
        };
        let mut quotient = vec![zero; width];
        let mut modulus = vec![zero; width];
        for index in 0..significant {
            quotient[index] = if let Some(high) = high_nonzero {
                self.emit_mux(high, zero, small_quotient[index], source)?
            } else {
                small_quotient[index]
            };
            let original = constant_bits[index];
            modulus[index] = if let Some(high) = high_nonzero {
                self.emit_mux(high, original, small_remainder[index], source)?
            } else {
                small_remainder[index]
            };
        }
        Ok((quotient, modulus))
    }

    fn restoring_divide_constant(
        &mut self,
        dividend: &[word::ValueId],
        divisor: &[bool],
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<(Vec<word::ValueId>, Vec<word::ValueId>), crate::SynthError> {
        let zero = self.constant(BitVal::Zero, state, source)?;
        let divisor = divisor
            .iter()
            .map(|&bit| if bit { BitVal::One } else { BitVal::Zero })
            .map(|bit| self.constant(bit, state, source))
            .collect::<Result<Vec<_>, _>>()?;
        let mut partial = vec![zero; divisor.len() + 1];
        let mut quotient = vec![zero; dividend.len()];
        let mut divisor_extended = divisor;
        divisor_extended.push(zero);
        for index in (0..dividend.len()).rev() {
            partial.rotate_right(1);
            partial[0] = dividend[index];
            let (difference, no_borrow) =
                self.add_sub_vectors(&partial, &divisor_extended, true, state, source)?;
            for (partial, difference) in partial.iter_mut().zip(difference) {
                *partial = self.emit_mux(no_borrow, difference, *partial, source)?;
            }
            quotient[index] = no_borrow;
        }
        let mut modulus = partial;
        modulus.resize(dividend.len(), zero);
        modulus.truncate(dividend.len());
        Ok((quotient, modulus))
    }

    fn restoring_divide(
        &mut self,
        dividend: &[word::ValueId],
        divisor: &[word::ValueId],
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<(Vec<word::ValueId>, Vec<word::ValueId>), crate::SynthError> {
        if dividend.len() != divisor.len() || dividend.is_empty() {
            return Err(crate::SynthError::invariant(
                "divider operands must have equal nonzero widths",
            ));
        }
        let zero = self.constant(BitVal::Zero, state, source)?;
        let mut partial = vec![zero; dividend.len() + 1];
        let mut divisor_extended = divisor.to_vec();
        divisor_extended.push(zero);
        let mut quotient = vec![zero; dividend.len()];
        for index in (0..dividend.len()).rev() {
            partial.rotate_right(1);
            partial[0] = dividend[index];
            let (difference, no_borrow) =
                self.add_sub_vectors(&partial, &divisor_extended, true, state, source)?;
            for (partial, difference) in partial.iter_mut().zip(difference) {
                *partial = self.emit_mux(no_borrow, difference, *partial, source)?;
            }
            quotient[index] = no_borrow;
        }
        partial.pop();
        Ok((quotient, partial))
    }

    fn add_sub_vectors(
        &mut self,
        left: &[word::ValueId],
        right: &[word::ValueId],
        subtract: bool,
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<(Vec<word::ValueId>, word::ValueId), crate::SynthError> {
        if left.len() != right.len() || left.is_empty() {
            return Err(crate::SynthError::invariant(
                "vector adder operands must have equal nonzero widths",
            ));
        }
        let mut carry = self.constant(
            if subtract { BitVal::One } else { BitVal::Zero },
            state,
            source,
        )?;
        let mut result = Vec::with_capacity(left.len());
        for (&left, &right) in left.iter().zip(right) {
            let right = if subtract {
                self.emit_unary(word::UnaryOp::BitNot, right, source)?
            } else {
                right
            };
            let propagate = self.emit_binary(word::BinaryOp::BitXor, left, right, source)?;
            result.push(self.emit_binary(word::BinaryOp::BitXor, propagate, carry, source)?);
            let generate = self.emit_binary(word::BinaryOp::BitAnd, left, right, source)?;
            let propagated = self.emit_binary(word::BinaryOp::BitAnd, propagate, carry, source)?;
            carry = self.emit_binary(word::BinaryOp::BitOr, generate, propagated, source)?;
        }
        Ok((result, carry))
    }

    fn conditional_negate_vector(
        &mut self,
        value: &[word::ValueId],
        negative: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let mut carry = negative;
        let mut result = Vec::with_capacity(value.len());
        for &bit in value {
            let inverted = self.emit_binary(word::BinaryOp::BitXor, bit, negative, source)?;
            result.push(self.emit_binary(word::BinaryOp::BitXor, inverted, carry, source)?);
            carry = self.emit_binary(word::BinaryOp::BitAnd, inverted, carry, source)?;
        }
        Ok(result)
    }

    fn constant_vector(&self, bits: &[word::ValueId]) -> Option<Vec<bool>> {
        bits.iter().map(|&bit| self.scalar_constant(bit)).collect()
    }
}

fn twos_complement_magnitude(mut bits: Vec<bool>, signed: bool) -> Vec<bool> {
    if !signed || !bits.last().copied().unwrap_or(false) {
        return bits;
    }
    let mut carry = true;
    for bit in &mut bits {
        *bit = !*bit;
        let sum = *bit ^ carry;
        carry &= *bit;
        *bit = sum;
    }
    bits
}

fn significant_bits(bits: &[bool]) -> usize {
    bits.iter()
        .rposition(|&bit| bit)
        .map_or(0, |index| index + 1)
}

fn bits_to_u64(bits: &[bool]) -> Option<u64> {
    if bits.len() > u64::BITS as usize && bits[u64::BITS as usize..].iter().any(|&bit| bit) {
        return None;
    }
    Some(
        bits.iter()
            .take(u64::BITS as usize)
            .enumerate()
            .fold(0u64, |value, (index, &bit)| {
                value | (u64::from(bit) << index)
            }),
    )
}

fn lsb_bits(value: u64, width: usize) -> Vec<bool> {
    (0..width)
        .map(|index| index < u64::BITS as usize && value & (1u64 << index) != 0)
        .collect()
}

fn nonadjacent_digit_count(bits: &[bool]) -> usize {
    let mut count = 0usize;
    let mut carry = false;
    for index in 0..=bits.len() {
        let bit = bits.get(index).copied().unwrap_or(false);
        match (bit, carry) {
            (false, false) | (true, true) => {}
            (false, true) => {
                count += 1;
                carry = false;
            }
            (true, false) => {
                count += 1;
                carry = bits.get(index + 1).copied().unwrap_or(false);
            }
        }
    }
    count
}

fn logical_right_shift<T: Copy>(bits: &[T], shift: usize, zero: T) -> Vec<T> {
    (0..bits.len())
        .map(|index| bits.get(index + shift).copied().unwrap_or(zero))
        .collect()
}

fn unsigned_magic(width: u32, divisor: u64) -> Option<UnsignedMagic> {
    if width == 0 || width > u64::BITS || divisor <= 1 || divisor.is_power_of_two() {
        return None;
    }
    let floor_log2 = u64::BITS - 1 - divisor.leading_zeros();
    if floor_log2 >= width {
        return None;
    }
    let exponent = width.checked_add(floor_log2)?;
    let numerator = 1u128.checked_shl(exponent)?;
    let divisor_wide = u128::from(divisor);
    let mut proposed = numerator / divisor_wide;
    let remainder = numerator % divisor_wide;
    let threshold = 1u128 << floor_log2;
    let add = divisor_wide - remainder >= threshold;
    if add {
        proposed *= 2;
        if remainder * 2 >= divisor_wide {
            proposed += 1;
        }
    }
    let mask = if width == u64::BITS {
        u128::from(u64::MAX)
    } else {
        (1u128 << width) - 1
    };
    Some(UnsignedMagic {
        multiplier: u64::try_from((proposed + 1) & mask)
            .expect("invariant-division multiplier is masked to the requested width"),
        shift: floor_log2,
        add,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_division_is_exhaustive_through_twelve_bits() {
        for width in 2..=12 {
            let limit = 1u64 << width;
            for divisor in 3..limit {
                if divisor.is_power_of_two() {
                    continue;
                }
                let magic = unsigned_magic(width, divisor).unwrap();
                for dividend in 0..limit {
                    let product = u128::from(dividend) * u128::from(magic.multiplier);
                    let mut quotient = u64::try_from(product >> width)
                        .expect("twelve-bit exhaustive products fit in u64");
                    if magic.add {
                        quotient = ((dividend - quotient) >> 1) + quotient;
                    }
                    quotient >>= magic.shift;
                    assert_eq!(
                        quotient,
                        dividend / divisor,
                        "{width}-bit {dividend}/{divisor}"
                    );
                }
            }
        }
    }
}
