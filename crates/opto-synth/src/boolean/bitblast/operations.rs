// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BitBackend, BitBlaster, BitVal, ImplementationRequest, ScalarBit, lower_implementation, word,
};

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(super) fn operation_bits(
        &mut self,
        operation: word::OpId,
        kind: word::OpKind,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let result = match kind {
            word::OpKind::Unary { op, arg } => self.unary_bits(op, arg, source),
            word::OpKind::Binary { op, left, right } => {
                self.binary_bits(operation, op, left, right, result_ty, source)
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => self.mux_bits(cond, then_value, else_value, result_ty, source),
            word::OpKind::Concat { parts } => self.concat_bits(&parts),
            word::OpKind::Extract { value, lsb, width } => {
                self.extract_bits(value, lsb, width.get())
            }
            word::OpKind::DynamicExtract {
                value: _,
                offset: _,
                width: _,
            } => self.dynamic_extract_bits(operation, result_ty, source),
            word::OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => self.dynamic_insert_bits(value, offset, replacement, source),
            word::OpKind::Cast {
                kind,
                value,
                target,
            } => self.cast_bits(kind, value, target, source),
            word::OpKind::Register(register) => self.register_bits(&register, source),
            word::OpKind::Latch(latch) => self.latch_bits(&latch, source),
        }?;
        Ok(result)
    }

    pub(super) fn legalize_native_scalar_operation(
        &mut self,
        original: word::ValueId,
        kind: word::OpKind,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let original_bit = self.backend.import_word(self.module, original);
        match kind {
            word::OpKind::Unary { op, arg } => {
                let rewritten = self.scalar_value(arg)?;
                if rewritten == self.backend.import_word(self.module, arg) {
                    Ok(original_bit)
                } else {
                    self.emit_unary(op, rewritten, source)
                }
            }
            word::OpKind::Binary { op, left, right } => {
                let rewritten_left = self.scalar_value(left)?;
                let rewritten_right = self.scalar_value(right)?;
                if rewritten_left == self.backend.import_word(self.module, left)
                    && rewritten_right == self.backend.import_word(self.module, right)
                {
                    Ok(original_bit)
                } else {
                    self.emit_binary(op, rewritten_left, rewritten_right, source)
                }
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                let rewritten_cond = self.scalar_value(cond)?;
                let result_ty = self.value_type(original)?;
                let rewritten_then = self.scalar_value(then_value)?;
                let rewritten_then = self.scalar_with_type(rewritten_then, result_ty, source)?;
                let rewritten_else = self.scalar_value(else_value)?;
                let rewritten_else = self.scalar_with_type(rewritten_else, result_ty, source)?;
                if rewritten_cond == self.backend.import_word(self.module, cond)
                    && rewritten_then == self.backend.import_word(self.module, then_value)
                    && rewritten_else == self.backend.import_word(self.module, else_value)
                {
                    Ok(original_bit)
                } else {
                    self.emit_mux(rewritten_cond, rewritten_then, rewritten_else, source)
                }
            }
            word::OpKind::Register(register) => {
                let bits = self.register_bits(&register, source)?;
                let [value]: [ScalarBit; 1] = bits.try_into().map_err(|bits: Vec<_>| {
                    crate::SynthError::invariant(format!(
                        "native scalar register legalized to {} bits",
                        bits.len()
                    ))
                })?;
                Ok(value)
            }
            word::OpKind::Latch(latch) => {
                let bits = self.latch_bits(&latch, source)?;
                let [value]: [ScalarBit; 1] = bits.try_into().map_err(|bits: Vec<_>| {
                    crate::SynthError::invariant(format!(
                        "native scalar latch legalized to {} bits",
                        bits.len()
                    ))
                })?;
                Ok(value)
            }
            word::OpKind::Concat { .. }
            | word::OpKind::Extract { .. }
            | word::OpKind::DynamicExtract { .. }
            | word::OpKind::DynamicInsert { .. }
            | word::OpKind::Cast { .. } => Err(crate::SynthError::invariant(
                "non-native operation reached scalar legalization",
            )),
        }
    }

    pub(super) fn unary_bits(
        &mut self,
        op: word::UnaryOp,
        arg: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let span = self.value(arg)?;
        match op {
            word::UnaryOp::BitNot => (0..span.len())
                .map(|index| self.emit_unary(word::UnaryOp::BitNot, self.bit(span, index), source))
                .collect(),
            word::UnaryOp::LogicalNot => {
                let value = self.reduce_span(span, word::BinaryOp::BitOr, source)?;
                Ok(vec![self.emit_unary(
                    word::UnaryOp::LogicalNot,
                    value,
                    source,
                )?])
            }
            word::UnaryOp::ReductionAnd => Ok(vec![self.reduce_span(
                span,
                word::BinaryOp::BitAnd,
                source,
            )?]),
            word::UnaryOp::ReductionOr => Ok(vec![self.reduce_span(
                span,
                word::BinaryOp::BitOr,
                source,
            )?]),
            word::UnaryOp::ReductionXor => Ok(vec![self.reduce_span(
                span,
                word::BinaryOp::BitXor,
                source,
            )?]),
        }
    }

    pub(super) fn binary_bits(
        &mut self,
        operation: word::OpId,
        op: word::BinaryOp,
        left: word::ValueId,
        right: word::ValueId,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        match op {
            word::BinaryOp::Add
            | word::BinaryOp::Sub
            | word::BinaryOp::Mul
            | word::BinaryOp::Div
            | word::BinaryOp::Mod => {
                let source_operation = self.source_operation(operation)?.ok_or_else(|| {
                    crate::SynthError::invariant(
                        "region-local generated arithmetic has no architecture decision",
                    )
                })?;
                if self
                    .operator_for_source_operation(source_operation)
                    .is_none()
                {
                    if self.plan.is_operation_elided(source_operation) {
                        let placeholder = self.constant(BitVal::Zero, result_ty.state(), source)?;
                        return Ok(vec![placeholder; result_ty.width() as usize]);
                    }
                    return Err(crate::SynthError::invariant(format!(
                        "live arithmetic operation {source_operation:?} has no implementation decision"
                    )));
                }
                let operator = self
                    .operator_for_source_operation(source_operation)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "arithmetic operation {source_operation:?} has no semantic operator"
                        ))
                    })?;
                let source_semantic = self.plan.operator(operator).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "arithmetic operation {source_operation:?} references an unknown semantic operator"
                    ))
                })?;
                if source_semantic.source_operation() != source_operation {
                    return Err(crate::SynthError::invariant(format!(
                        "arithmetic operation {source_operation:?} resolved to a different semantic source"
                    )));
                }
                let semantic = self.local_semantic_operator(source_semantic, operation)?;
                let implementation_ty =
                    word::WordType::new(semantic.width(), result_ty.is_signed(), result_ty.state())
                        .map_err(crate::SynthError::from)?;
                let selected = self
                    .plan
                    .selected_candidate(operator)
                    .ok_or_else(|| crate::SynthError::invariant("operator has no candidate"))?;
                let recipe_name =
                    self.plan
                        .candidate_recipe_name(selected.id())
                        .ok_or_else(|| {
                            crate::SynthError::invariant(format!(
                                "implementation candidate {} has no registered recipe",
                                selected.id().raw()
                            ))
                        })?;
                let previous = self.active_operator.replace(operator);
                let result = lower_implementation(
                    selected.provider(),
                    selected.recipe(),
                    self,
                    ImplementationRequest {
                        operator: semantic,
                        result_type: implementation_ty,
                        source,
                    },
                );
                self.active_operator = previous;
                let mut result = result?;
                if result.len() != semantic.width() as usize {
                    return Err(crate::SynthError::invariant(format!(
                        "implementation '{}' produced {} bits for operator width {}",
                        recipe_name,
                        result.len(),
                        semantic.width()
                    )));
                }
                let placeholder = self.constant(BitVal::Zero, result_ty.state(), source)?;
                result.resize(semantic.semantic_width() as usize, placeholder);
                Ok(result)
            }
            word::BinaryOp::BitAnd | word::BinaryOp::BitOr | word::BinaryOp::BitXor => {
                self.bitwise_binary_bits(op, left, right, result_ty, source)
            }
            word::BinaryOp::LogicalAnd | word::BinaryOp::LogicalOr => {
                let left = self.logical_value(left, source)?;
                let right = self.logical_value(right, source)?;
                Ok(vec![self.emit_binary(op, left, right, source)?])
            }
            word::BinaryOp::Eq | word::BinaryOp::Ne => self.equality_bits(op, left, right, source),
            word::BinaryOp::Lt | word::BinaryOp::Le | word::BinaryOp::Gt | word::BinaryOp::Ge => {
                self.compare_bits(op, left, right, source)
            }
            word::BinaryOp::Shl | word::BinaryOp::Shr | word::BinaryOp::Ashr => {
                self.shift_bits(op, left, right, result_ty, source)
            }
        }
    }

    pub(super) fn bitwise_binary_bits(
        &mut self,
        op: word::BinaryOp,
        left: word::ValueId,
        right: word::ValueId,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let sign_extend = result_ty.is_signed();
        (0..result_ty.width())
            .map(|index| {
                let left = self.resized_bit(left_span, left_ty, index, sign_extend, source)?;
                let right = self.resized_bit(right_span, right_ty, index, sign_extend, source)?;
                self.emit_binary(op, left, right, source)
            })
            .collect()
    }

    pub(super) fn equality_bits(
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
        let mut differences = Vec::with_capacity(width as usize);
        for index in 0..width {
            let left = self.resized_bit(left_span, left_ty, index, sign_extend, source)?;
            let right = self.resized_bit(right_span, right_ty, index, sign_extend, source)?;
            differences.push(self.emit_binary(word::BinaryOp::BitXor, left, right, source)?);
        }
        let differs = self.reduce_values(differences, word::BinaryOp::BitOr, source)?;
        if op == word::BinaryOp::Ne {
            Ok(vec![differs])
        } else {
            Ok(vec![self.emit_unary(
                word::UnaryOp::BitNot,
                differs,
                source,
            )?])
        }
    }

    pub(super) fn mux_bits(
        &mut self,
        cond: word::ValueId,
        then_value: word::ValueId,
        else_value: word::ValueId,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let cond = self.scalar_value(cond)?;
        let then_span = self.value(then_value)?;
        let else_span = self.value(else_value)?;
        if then_span.len() != else_span.len() {
            return Err(crate::SynthError::invariant(
                "bitblast mux branches have different widths",
            ));
        }
        (0..then_span.len())
            .map(|index| {
                let bit_ty = word::WordType::new(
                    1,
                    result_ty.width() == 1 && result_ty.is_signed(),
                    result_ty.state(),
                )
                .map_err(crate::SynthError::from)?;
                let then_bit = self.scalar_with_type(self.bit(then_span, index), bit_ty, source)?;
                let else_bit = self.scalar_with_type(self.bit(else_span, index), bit_ty, source)?;
                self.emit_mux(cond, then_bit, else_bit, source)
            })
            .collect()
    }

    pub(super) fn concat_bits(
        &mut self,
        parts: &[word::ValueId],
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let mut bits = Vec::new();
        for part in parts.iter().rev() {
            let span = self.value(*part)?;
            bits.extend((0..span.len()).map(|index| self.bit(span, index)));
        }
        Ok(bits)
    }

    pub(super) fn extract_bits(
        &mut self,
        value: word::ValueId,
        lsb: u32,
        width: u32,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let span = self.value(value)?;
        let end = lsb
            .checked_add(width)
            .ok_or_else(|| crate::SynthError::invariant("bitblast extract range overflow"))?;
        if end > span.len() {
            return Err(crate::SynthError::invariant(
                "bitblast extract exceeds source width",
            ));
        }
        Ok((lsb..end).map(|index| self.bit(span, index)).collect())
    }

    pub(super) fn cast_bits(
        &mut self,
        kind: word::CastKind,
        value: word::ValueId,
        target: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let span = self.value(value)?;
        let source_ty = self.value_type(value)?;
        let mut bits = (0..target.width())
            .map(|index| {
                if index < span.len() {
                    Ok(self.bit(span, index))
                } else {
                    match kind {
                        word::CastKind::SignExtend => Ok(self.bit(span, span.len() - 1)),
                        word::CastKind::ZeroExtend => {
                            self.constant(BitVal::Zero, source_ty.state(), source)
                        }
                        word::CastKind::Truncate => Err(crate::SynthError::invariant(
                            "truncate cast cannot require extension during bitblast",
                        )),
                    }
                }
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        if target.width() == 1 {
            if self.bit_type(bits[0])? != target
                && let Some(word) = self.backend.word_value(bits[0])
            {
                let cast = self
                    .module
                    .cast(kind, word, target, source.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_value(cast)?;
                bits[0] = self.backend.import_word(self.module, cast);
            }
        } else {
            for bit in &mut bits {
                *bit = self.unsigned_bit(*bit, source)?;
            }
        }
        Ok(bits)
    }
}
