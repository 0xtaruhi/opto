// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBackend, BitBlaster, ScalarBit, word};

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(in crate::boolean::bitblast) fn compare_bits(
        &mut self,
        op: word::BinaryOp,
        left: word::ValueId,
        right: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let width = left_ty.width().max(right_ty.width());
        let sign_extend = left_ty.is_signed() && right_ty.is_signed();
        let mut left_bits = Vec::with_capacity(width as usize);
        let mut right_bits = Vec::with_capacity(width as usize);
        for index in 0..width {
            left_bits.push(self.resized_bit(left_span, left_ty, index, sign_extend, source)?);
            right_bits.push(self.resized_bit(right_span, right_ty, index, sign_extend, source)?);
        }
        // Every ordering relation can share one directional less-than network.
        let (ordered_left, ordered_right, invert_result) = match op {
            word::BinaryOp::Lt => (&left_bits, &right_bits, false),
            word::BinaryOp::Le => (&right_bits, &left_bits, true),
            word::BinaryOp::Gt => (&right_bits, &left_bits, false),
            word::BinaryOp::Ge => (&left_bits, &right_bits, true),
            _ => {
                return Err(crate::SynthError::invariant(format!(
                    "invalid comparison op {op:?} during bitblast"
                )));
            }
        };
        let mut less = self.unsigned_less(ordered_left, ordered_right, source)?;
        if left_ty.is_signed() && right_ty.is_signed() {
            let left_sign = ordered_left[(width - 1) as usize];
            let right_sign = ordered_right[(width - 1) as usize];
            let signs_differ =
                self.emit_binary(word::BinaryOp::BitXor, left_sign, right_sign, source)?;
            less = self.emit_mux(signs_differ, left_sign, less, source)?;
        }
        let result = if invert_result {
            self.emit_unary(word::UnaryOp::BitNot, less, source)?
        } else {
            less
        };
        Ok(vec![result])
    }

    pub(in crate::boolean::bitblast) fn unsigned_less(
        &mut self,
        left: &[ScalarBit],
        right: &[ScalarBit],
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let mut bits = left.iter().zip(right);
        let Some((&left, &right)) = bits.next() else {
            return Err(crate::SynthError::invariant(
                "comparison inputs must be nonempty",
            ));
        };
        let not_left = self.emit_unary(word::UnaryOp::BitNot, left, source)?;
        let mut less = self.emit_binary(word::BinaryOp::BitAnd, not_left, right, source)?;
        // Bits are LSB-first: each more-significant difference overrides the
        // ordering accumulated from the less-significant suffix.
        for (&left, &right) in bits {
            let differs = self.emit_binary(word::BinaryOp::BitXor, left, right, source)?;
            let not_left = self.emit_unary(word::UnaryOp::BitNot, left, source)?;
            less = self.emit_mux(differs, not_left, less, source)?;
        }
        Ok(less)
    }
}
