// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBackend, BitBlaster, BitVal, ScalarBit, word};

#[derive(Clone)]
struct PrefixNetwork {
    propagate: Vec<ScalarBit>,
    generate: Vec<ScalarBit>,
}

type PrefixInputs = (Vec<ScalarBit>, PrefixNetwork);

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(in crate::boolean::bitblast) fn constant_add_sub_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        subtract: bool,
        prefix: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let left_bits = self.resized_value_bits(left, result_ty, source)?;
        let right_bits = self.resized_value_bits(right, result_ty, source)?;
        let left_constant = self.bool_vector(&left_bits);
        let right_constant = self.bool_vector(&right_bits);
        let (mut variable, mut constant, invert_variable, invert_constant, carry) =
            match (left_constant, right_constant, subtract) {
                (_, Some(constant), false) => (left_bits, constant, false, false, false),
                (Some(constant), _, false) => (right_bits, constant, false, false, false),
                (_, Some(constant), true) => (left_bits, constant, false, true, true),
                (Some(constant), _, true) => (right_bits, constant, true, false, true),
                _ => {
                    return Err(crate::SynthError::invariant(
                        "constant adder recipe has no defined constant operand",
                    ));
                }
            };
        if invert_variable {
            for bit in &mut variable {
                *bit = self.emit_unary(word::UnaryOp::BitNot, *bit, source)?;
            }
        }
        if invert_constant {
            for bit in &mut constant {
                *bit = !*bit;
            }
        }
        if prefix {
            self.prefix_add_constant(&variable, &constant, carry, result_ty, source)
        } else {
            self.ripple_add_constant(&variable, &constant, carry, result_ty, source)
        }
    }

    fn resized_value_bits(
        &mut self,
        value: word::ValueId,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let span = self.value(value)?;
        let ty = self.value_type(value)?;
        (0..result_ty.width())
            .map(|index| self.resized_bit(span, ty, index, result_ty.is_signed(), source))
            .collect()
    }

    fn bool_vector(&self, bits: &[ScalarBit]) -> Option<Vec<bool>> {
        bits.iter().map(|&bit| self.scalar_constant(bit)).collect()
    }

    fn ripple_add_constant(
        &mut self,
        variable: &[ScalarBit],
        constant: &[bool],
        carry: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let mut carry = self.constant(
            if carry { BitVal::One } else { BitVal::Zero },
            result_ty.state(),
            source,
        )?;
        let mut result = Vec::with_capacity(variable.len());
        for (&bit, &constant) in variable.iter().zip(constant) {
            let xor = self.emit_binary(word::BinaryOp::BitXor, bit, carry, source)?;
            if constant {
                result.push(self.emit_unary(word::UnaryOp::BitNot, xor, source)?);
                carry = self.emit_binary(word::BinaryOp::BitOr, bit, carry, source)?;
            } else {
                result.push(xor);
                carry = self.emit_binary(word::BinaryOp::BitAnd, bit, carry, source)?;
            }
        }
        Ok(result)
    }

    fn prefix_add_constant(
        &mut self,
        variable: &[ScalarBit],
        constant: &[bool],
        carry_in: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;
        let one = self.constant(BitVal::One, result_ty.state(), source)?;
        let prefix_len = variable.len().saturating_sub(1);
        let mut prefix = PrefixNetwork {
            propagate: Vec::with_capacity(prefix_len),
            generate: Vec::with_capacity(prefix_len),
        };
        for (&bit, &constant) in variable.iter().zip(constant).take(prefix_len) {
            prefix.propagate.push(if constant { one } else { bit });
            prefix.generate.push(if constant { bit } else { zero });
        }
        if !prefix.generate.is_empty() {
            self.brent_kung_prefix(&mut prefix, source)?;
        }
        let mut result = Vec::with_capacity(variable.len());
        for (index, (&bit, &constant)) in variable.iter().zip(constant).enumerate() {
            let carry = if index == 0 {
                if carry_in { one } else { zero }
            } else if carry_in {
                self.emit_binary(
                    word::BinaryOp::BitOr,
                    prefix.generate[index - 1],
                    prefix.propagate[index - 1],
                    source,
                )?
            } else {
                prefix.generate[index - 1]
            };
            let sum = self.emit_binary(word::BinaryOp::BitXor, bit, carry, source)?;
            result.push(if constant {
                self.emit_unary(word::UnaryOp::BitNot, sum, source)?
            } else {
                sum
            });
        }
        Ok(result)
    }

    pub(super) fn ripple_add_sub_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        subtract: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let sign_extend = result_ty.is_signed();
        let mut left_bits = Vec::with_capacity(result_ty.width() as usize);
        let mut right_bits = Vec::with_capacity(result_ty.width() as usize);
        for index in 0..result_ty.width() {
            left_bits.push(self.resized_bit(left_span, left_ty, index, sign_extend, source)?);
            let mut bit = self.resized_bit(right_span, right_ty, index, sign_extend, source)?;
            if subtract {
                bit = self.emit_unary(word::UnaryOp::BitNot, bit, source)?;
            }
            right_bits.push(bit);
        }
        let carry = self.constant(
            if subtract { BitVal::One } else { BitVal::Zero },
            result_ty.state(),
            source,
        )?;
        self.add_vectors(&left_bits, &right_bits, carry, source)
    }

    pub(super) fn kogge_stone_add_sub_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        subtract: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let (propagate, mut prefix) =
            self.prefix_adder_inputs(left, right, subtract, result_ty, source)?;
        self.kogge_stone_prefix(&mut prefix, source)?;

        self.prefix_adder_sum(&propagate, &prefix, subtract, source)
    }

    fn kogge_stone_prefix(
        &mut self,
        prefix: &mut PrefixNetwork,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let mut next = prefix.clone();
        let mut distance = 1usize;
        while distance < prefix.generate.len() {
            next.clone_from(prefix);
            for index in distance..prefix.generate.len() {
                self.combine_prefix(index, index - distance, prefix, &mut next, source)?;
            }
            std::mem::swap(prefix, &mut next);
            distance = distance.checked_mul(2).ok_or_else(|| {
                crate::SynthError::invariant("prefix-adder stage distance overflow")
            })?;
        }
        Ok(())
    }

    pub(in crate::boolean::bitblast) fn kogge_stone_add_vectors(
        &mut self,
        left: &[ScalarBit],
        right: &[ScalarBit],
        carry: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        if left.len() < 2 {
            return self.add_vectors(left, right, carry, source);
        }
        let (propagate, mut prefix) = self.prefix_inputs_from_bits(left, right, source)?;
        self.seed_prefix_carry(&mut prefix, carry, source)?;
        self.kogge_stone_prefix(&mut prefix, source)?;
        self.seeded_prefix_sum(&propagate, &prefix, carry, source)
    }

    pub(super) fn brent_kung_add_sub_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        subtract: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let (propagate, mut prefix) =
            self.prefix_adder_inputs(left, right, subtract, result_ty, source)?;
        self.brent_kung_prefix(&mut prefix, source)?;
        self.prefix_adder_sum(&propagate, &prefix, subtract, source)
    }

    fn brent_kung_prefix(
        &mut self,
        prefix: &mut PrefixNetwork,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let count = prefix.generate.len();
        let mut current = prefix.clone();
        let mut next = current.clone();
        let mut stride = 1usize;
        while stride < count {
            next.clone_from(&current);
            let step = stride.checked_mul(2).ok_or_else(|| {
                crate::SynthError::invariant("prefix-adder stage stride overflow")
            })?;
            for index in (step - 1..count).step_by(step) {
                self.combine_prefix(index, index - stride, &current, &mut next, source)?;
            }
            std::mem::swap(&mut current, &mut next);
            stride = step;
        }
        let mut stride = count.next_power_of_two() / 4;
        while stride > 0 {
            next.clone_from(&current);
            let step = stride.checked_mul(2).ok_or_else(|| {
                crate::SynthError::invariant("prefix-adder stage stride overflow")
            })?;
            let start = stride
                .checked_mul(3)
                .and_then(|value| value.checked_sub(1))
                .ok_or_else(|| crate::SynthError::invariant("prefix-adder stage start overflow"))?;
            for index in (start..count).step_by(step) {
                self.combine_prefix(index, index - stride, &current, &mut next, source)?;
            }
            std::mem::swap(&mut current, &mut next);
            stride /= 2;
        }
        *prefix = current;
        Ok(())
    }

    pub(super) fn hybrid_brent_kung_add_sub_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        subtract: bool,
        result_ty: word::WordType,
        requested_ripple_width: u32,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let width = result_ty.width();
        let ripple_width = requested_ripple_width.max(2).min(width - 2);
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let sign_extend = result_ty.is_signed();
        let mut left_bits = Vec::with_capacity(width as usize);
        let mut right_bits = Vec::with_capacity(width as usize);
        for index in 0..width {
            left_bits.push(self.resized_bit(left_span, left_ty, index, sign_extend, source)?);
            let mut bit = self.resized_bit(right_span, right_ty, index, sign_extend, source)?;
            if subtract {
                bit = self.emit_unary(word::UnaryOp::BitNot, bit, source)?;
            }
            right_bits.push(bit);
        }
        let initial_carry = self.constant(
            if subtract { BitVal::One } else { BitVal::Zero },
            result_ty.state(),
            source,
        )?;
        let split = usize::try_from(ripple_width)
            .map_err(|_| crate::SynthError::capacity("hybrid adder split index overflow"))?;
        let (mut sum, carry) = self.add_vectors_with_carry(
            &left_bits[..split],
            &right_bits[..split],
            initial_carry,
            source,
        )?;
        let (propagate, mut prefix) =
            self.prefix_inputs_from_bits(&left_bits[split..], &right_bits[split..], source)?;
        self.brent_kung_prefix(&mut prefix, source)?;
        sum.extend(self.prefix_adder_sum_with_carry(&propagate, &prefix, carry, source)?);
        Ok(sum)
    }

    fn prefix_adder_inputs(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        subtract: bool,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<PrefixInputs, crate::SynthError> {
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let sign_extend = result_ty.is_signed();
        let width = result_ty.width();
        let mut propagate = Vec::with_capacity(width as usize);
        let mut generate = Vec::with_capacity((width - 1) as usize);
        for index in 0..width {
            let left_bit = self.resized_bit(left_span, left_ty, index, sign_extend, source)?;
            let mut right_bit =
                self.resized_bit(right_span, right_ty, index, sign_extend, source)?;
            if subtract {
                right_bit = self.emit_unary(word::UnaryOp::BitNot, right_bit, source)?;
            }
            propagate.push(self.emit_binary(
                word::BinaryOp::BitXor,
                left_bit,
                right_bit,
                source,
            )?);
            if index + 1 != width {
                generate.push(self.emit_binary(
                    word::BinaryOp::BitAnd,
                    left_bit,
                    right_bit,
                    source,
                )?);
            }
        }
        let prefix = PrefixNetwork {
            propagate: propagate[..generate.len()].to_vec(),
            generate,
        };
        Ok((propagate, prefix))
    }

    fn prefix_inputs_from_bits(
        &mut self,
        left: &[ScalarBit],
        right: &[ScalarBit],
        source: &word::SourceSpan,
    ) -> Result<PrefixInputs, crate::SynthError> {
        if left.len() != right.len() || left.len() < 2 {
            return Err(crate::SynthError::invariant(
                "hybrid prefix inputs must have equal widths of at least two bits",
            ));
        }
        let width = left.len();
        let mut propagate = Vec::with_capacity(width);
        let mut generate = Vec::with_capacity(width - 1);
        for (index, (&left, &right)) in left.iter().zip(right).enumerate() {
            propagate.push(self.emit_binary(word::BinaryOp::BitXor, left, right, source)?);
            if index + 1 != width {
                generate.push(self.emit_binary(word::BinaryOp::BitAnd, left, right, source)?);
            }
        }
        let prefix = PrefixNetwork {
            propagate: propagate[..generate.len()].to_vec(),
            generate,
        };
        Ok((propagate, prefix))
    }

    fn combine_prefix(
        &mut self,
        upper: usize,
        lower: usize,
        previous: &PrefixNetwork,
        prefix: &mut PrefixNetwork,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let propagated_generate = self.emit_binary(
            word::BinaryOp::BitAnd,
            previous.propagate[upper],
            previous.generate[lower],
            source,
        )?;
        prefix.generate[upper] = self.emit_binary(
            word::BinaryOp::BitOr,
            previous.generate[upper],
            propagated_generate,
            source,
        )?;
        prefix.propagate[upper] = self.emit_binary(
            word::BinaryOp::BitAnd,
            previous.propagate[upper],
            previous.propagate[lower],
            source,
        )?;
        Ok(())
    }

    fn prefix_adder_sum(
        &mut self,
        propagate: &[ScalarBit],
        prefix: &PrefixNetwork,
        subtract: bool,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let width = propagate.len();

        let mut sum = Vec::with_capacity(width);
        sum.push(if subtract {
            self.emit_unary(word::UnaryOp::BitNot, propagate[0], source)?
        } else {
            propagate[0]
        });
        for (index, &propagate_bit) in propagate.iter().enumerate().skip(1) {
            let carry = if subtract {
                self.emit_binary(
                    word::BinaryOp::BitOr,
                    prefix.generate[index - 1],
                    prefix.propagate[index - 1],
                    source,
                )?
            } else {
                prefix.generate[index - 1]
            };
            sum.push(self.emit_binary(word::BinaryOp::BitXor, propagate_bit, carry, source)?);
        }
        Ok(sum)
    }

    fn prefix_adder_sum_with_carry(
        &mut self,
        propagate: &[ScalarBit],
        prefix: &PrefixNetwork,
        carry_in: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let first = *propagate.first().ok_or_else(|| {
            crate::SynthError::invariant("hybrid prefix sum has no propagate bits")
        })?;
        let mut sum = Vec::with_capacity(propagate.len());
        sum.push(self.emit_binary(word::BinaryOp::BitXor, first, carry_in, source)?);
        for (index, &propagate_bit) in propagate.iter().enumerate().skip(1) {
            let propagated_carry = self.emit_binary(
                word::BinaryOp::BitAnd,
                prefix.propagate[index - 1],
                carry_in,
                source,
            )?;
            let carry = self.emit_binary(
                word::BinaryOp::BitOr,
                prefix.generate[index - 1],
                propagated_carry,
                source,
            )?;
            sum.push(self.emit_binary(word::BinaryOp::BitXor, propagate_bit, carry, source)?);
        }
        Ok(sum)
    }

    pub(in crate::boolean::bitblast) fn add_vectors(
        &mut self,
        left: &[ScalarBit],
        right: &[ScalarBit],
        mut carry: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        if left.len() != right.len() || left.is_empty() {
            return Err(crate::SynthError::invariant(
                "adder inputs must have equal non-zero widths",
            ));
        }
        let width = left.len();
        let mut sum = Vec::with_capacity(width);
        for (index, (&left_bit, &right_bit)) in left.iter().zip(right).enumerate() {
            let propagate =
                self.emit_binary(word::BinaryOp::BitXor, left_bit, right_bit, source)?;
            sum.push(self.emit_binary(word::BinaryOp::BitXor, propagate, carry, source)?);
            if index + 1 != width {
                let generate =
                    self.emit_binary(word::BinaryOp::BitAnd, left_bit, right_bit, source)?;
                let carry_propagate =
                    self.emit_binary(word::BinaryOp::BitAnd, propagate, carry, source)?;
                carry =
                    self.emit_binary(word::BinaryOp::BitOr, generate, carry_propagate, source)?;
            }
        }
        Ok(sum)
    }

    pub(in crate::boolean::bitblast) fn brent_kung_add_vectors(
        &mut self,
        left: &[ScalarBit],
        right: &[ScalarBit],
        carry: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        if left.len() < 2 {
            return self.add_vectors(left, right, carry, source);
        }
        let (propagate, mut prefix) = self.prefix_inputs_from_bits(left, right, source)?;
        self.seed_prefix_carry(&mut prefix, carry, source)?;
        self.brent_kung_prefix(&mut prefix, source)?;
        self.seeded_prefix_sum(&propagate, &prefix, carry, source)
    }

    /// Fold carry-in into bit zero before the prefix scan. This keeps the
    /// carry input from directly driving a separate gate on every result bit.
    /// Matrix carry extraction establishes that this input is structurally
    /// available before every remaining operand bit.
    fn seed_prefix_carry(
        &mut self,
        prefix: &mut PrefixNetwork,
        carry: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let propagated =
            self.emit_binary(word::BinaryOp::BitAnd, prefix.propagate[0], carry, source)?;
        prefix.generate[0] = self.emit_binary(
            word::BinaryOp::BitOr,
            prefix.generate[0],
            propagated,
            source,
        )?;
        Ok(())
    }

    /// The scanned generate terms already contain carry-in. Only the least
    /// significant sum bit still consumes the external carry directly.
    fn seeded_prefix_sum(
        &mut self,
        propagate: &[ScalarBit],
        prefix: &PrefixNetwork,
        carry: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        propagate
            .iter()
            .enumerate()
            .map(|(index, &bit)| {
                let carry = if index == 0 {
                    carry
                } else {
                    prefix.generate[index - 1]
                };
                self.emit_binary(word::BinaryOp::BitXor, bit, carry, source)
            })
            .collect()
    }

    fn add_vectors_with_carry(
        &mut self,
        left: &[ScalarBit],
        right: &[ScalarBit],
        mut carry: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(Vec<ScalarBit>, ScalarBit), crate::SynthError> {
        if left.len() != right.len() || left.is_empty() {
            return Err(crate::SynthError::invariant(
                "carry-producing adder inputs must have equal non-zero widths",
            ));
        }
        let mut sum = Vec::with_capacity(left.len());
        for (&left_bit, &right_bit) in left.iter().zip(right) {
            let propagate =
                self.emit_binary(word::BinaryOp::BitXor, left_bit, right_bit, source)?;
            sum.push(self.emit_binary(word::BinaryOp::BitXor, propagate, carry, source)?);
            let generate = self.emit_binary(word::BinaryOp::BitAnd, left_bit, right_bit, source)?;
            let carry_propagate =
                self.emit_binary(word::BinaryOp::BitAnd, propagate, carry, source)?;
            carry = self.emit_binary(word::BinaryOp::BitOr, generate, carry_propagate, source)?;
        }
        Ok((sum, carry))
    }
}
