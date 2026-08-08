// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBlaster, word};

impl BitBlaster<'_> {
    pub(in crate::boolean::bitblast) fn shift_bits(
        &mut self,
        op: word::BinaryOp,
        value: word::ValueId,
        amount: word::ValueId,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let value_span = self.value(value)?;
        let value_ty = self.value_type(value)?;
        let amount_span = self.value(amount)?;
        let mut current = Vec::with_capacity(result_ty.width() as usize);
        for index in 0..result_ty.width() {
            current.push(self.resized_bit(
                value_span,
                value_ty,
                index,
                result_ty.is_signed(),
                source,
            )?);
        }
        let zero = self.zero_for_scalar(current[0], source)?;

        let relevant_stages = if result_ty.width() <= 1 {
            0
        } else {
            u32::BITS - (result_ty.width() - 1).leading_zeros()
        };
        let stages = amount_span.len().min(relevant_stages);
        for stage in 0..stages {
            let shift = 1u32 << stage;
            let control = self.bit(amount_span, stage);
            let right_fill = if op == word::BinaryOp::Ashr && result_ty.is_signed() {
                current[current.len() - 1]
            } else {
                zero
            };
            let mut next = Vec::with_capacity(current.len());
            for index in 0..result_ty.width() {
                let shifted = match op {
                    word::BinaryOp::Shl if index >= shift => current[(index - shift) as usize],
                    word::BinaryOp::Shr | word::BinaryOp::Ashr
                        if index + shift < result_ty.width() =>
                    {
                        current[(index + shift) as usize]
                    }
                    word::BinaryOp::Shl => zero,
                    word::BinaryOp::Shr | word::BinaryOp::Ashr => right_fill,
                    _ => {
                        return Err(crate::SynthError::invariant(format!(
                            "invalid shift op {op:?} during bitblast"
                        )));
                    }
                };
                next.push(self.emit_mux(control, shifted, current[index as usize], source)?);
            }
            current = next;
        }

        if amount_span.len() > relevant_stages {
            let high_bits = (relevant_stages..amount_span.len())
                .map(|index| self.bit(amount_span, index))
                .collect();
            let overflow = self.reduce_values(high_bits, word::BinaryOp::BitOr, source)?;
            let overflow_fill = if op == word::BinaryOp::Ashr && result_ty.is_signed() {
                current[current.len() - 1]
            } else {
                zero
            };
            for bit in &mut current {
                *bit = self.emit_mux(overflow, overflow_fill, *bit, source)?;
            }
        }
        Ok(current)
    }
}
