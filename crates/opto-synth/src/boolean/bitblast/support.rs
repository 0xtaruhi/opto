// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BitBackend, BitBlaster, BitSpan, BitVal, ConstBits, NonZeroU32, ScalarBit, constant_index, word,
};

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(super) fn is_native_scalar_operation(
        &self,
        kind: &word::OpKind,
        result_width: u32,
    ) -> Result<bool, crate::SynthError> {
        if !self.backend.preserves_native_word_operations() {
            return Ok(false);
        }
        if result_width != 1 {
            return Ok(false);
        }
        let scalar = |value: word::ValueId| -> Result<bool, crate::SynthError> {
            Ok(self.value_type(value)?.width() == 1)
        };
        Ok(match kind {
            word::OpKind::Unary { arg, .. } => scalar(*arg)?,
            word::OpKind::Binary { op, left, right } => {
                matches!(
                    op,
                    word::BinaryOp::BitAnd
                        | word::BinaryOp::BitOr
                        | word::BinaryOp::BitXor
                        | word::BinaryOp::LogicalAnd
                        | word::BinaryOp::LogicalOr
                        | word::BinaryOp::Eq
                        | word::BinaryOp::Ne
                ) && scalar(*left)?
                    && scalar(*right)?
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => scalar(*cond)? && scalar(*then_value)? && scalar(*else_value)?,
            word::OpKind::Register(register) => scalar(register.d)?,
            word::OpKind::Latch(latch) => scalar(latch.d)?,
            word::OpKind::TriState { .. }
            | word::OpKind::Concat { .. }
            | word::OpKind::Extract { .. }
            | word::OpKind::DynamicExtract { .. }
            | word::OpKind::DynamicInsert { .. }
            | word::OpKind::Cast { .. } => false,
        })
    }

    pub(super) fn value_type(
        &self,
        value: word::ValueId,
    ) -> Result<word::WordType, crate::SynthError> {
        self.module
            .value(value)
            .map(|value| value.ty)
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown RTL value {value:?}")))
    }

    pub(super) fn bit_type(&self, bit: ScalarBit) -> Result<word::WordType, crate::SynthError> {
        self.backend.bit_type(self.module, bit)
    }

    pub(super) fn scalar_constant(&self, value: ScalarBit) -> Option<bool> {
        self.backend.constant(self.module, value)
    }

    pub(super) fn resized_bit(
        &mut self,
        span: BitSpan,
        ty: word::WordType,
        index: u32,
        sign_extend: bool,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        if index < span.len() {
            Ok(self.bit(span, index))
        } else if sign_extend && ty.is_signed() {
            Ok(self.bit(span, span.len() - 1))
        } else {
            self.constant(BitVal::Zero, ty.state(), source)
        }
    }

    pub(super) fn scalar_value(
        &mut self,
        value: word::ValueId,
    ) -> Result<ScalarBit, crate::SynthError> {
        let span = self.value(value)?;
        if span.len() != 1 {
            return Err(crate::SynthError::invariant(format!(
                "expected scalar value during bitblast, got width {}",
                span.len()
            )));
        }
        Ok(self.bit(span, 0))
    }

    pub(super) fn unsigned_bit(
        &mut self,
        value: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let ty = self.bit_type(value)?;
        if !ty.is_signed() {
            return Ok(value);
        }
        let target = word::WordType::new(1, false, ty.state()).map_err(crate::SynthError::from)?;
        let word = self.backend.word_value(value).ok_or_else(|| {
            crate::SynthError::invariant("AXM scalar sign coercion is not a Word cast")
        })?;
        let value = self
            .module
            .cast(word::CastKind::ZeroExtend, word, target, source.clone())
            .map_err(crate::SynthError::from)?;
        self.record_generated_value(value)?;
        Ok(self.backend.import_word(self.module, value))
    }

    pub(super) fn scalar_with_type(
        &mut self,
        value: ScalarBit,
        target: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        if target.width() != 1 {
            return Err(crate::SynthError::invariant(
                "bitblast scalar coercion target is not one bit",
            ));
        }
        if self.bit_type(value)? == target {
            return Ok(value);
        }
        let Some(word) = self.backend.word_value(value) else {
            return Ok(value);
        };
        let value = self
            .module
            .cast(word::CastKind::ZeroExtend, word, target, source.clone())
            .map_err(crate::SynthError::from)?;
        self.record_generated_value(value)?;
        Ok(self.backend.import_word(self.module, value))
    }

    pub(super) fn logical_value(
        &mut self,
        value: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let span = self.value(value)?;
        self.reduce_span(span, word::BinaryOp::BitOr, source)
    }

    pub(super) fn reduce_span(
        &mut self,
        span: BitSpan,
        op: word::BinaryOp,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let values = (0..span.len()).map(|index| self.bit(span, index)).collect();
        self.reduce_values(values, op, source)
    }

    pub(super) fn reduce_values(
        &mut self,
        mut values: Vec<ScalarBit>,
        op: word::BinaryOp,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        if values.is_empty() {
            return Err(crate::SynthError::invariant(
                "cannot reduce an empty bit vector",
            ));
        }
        while values.len() > 1 {
            let mut next = Vec::with_capacity(values.len().div_ceil(2));
            for pair in values.chunks(2) {
                next.push(if let [left, right] = pair {
                    self.emit_binary(op, *left, *right, source)?
                } else {
                    pair[0]
                });
            }
            values = next;
        }
        Ok(values[0])
    }

    pub(super) fn emit_unary(
        &mut self,
        op: word::UnaryOp,
        arg: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let (bit, generated) = self.backend.emit_unary(self.module, op, arg, source)?;
        if let Some(value) = generated {
            self.record_generated_value(value)?;
        }
        Ok(bit)
    }

    pub(super) fn emit_binary(
        &mut self,
        op: word::BinaryOp,
        left: ScalarBit,
        right: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let (bit, generated) = self
            .backend
            .emit_binary(self.module, op, left, right, source)?;
        if let Some(value) = generated {
            self.record_generated_value(value)?;
        }
        Ok(bit)
    }

    pub(super) fn emit_mux(
        &mut self,
        cond: ScalarBit,
        mut then_value: ScalarBit,
        mut else_value: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let then_ty = self.bit_type(then_value)?;
        let else_ty = self.bit_type(else_value)?;
        if then_ty != else_ty {
            if then_ty.width() != 1 || else_ty.width() != 1 {
                return Err(crate::SynthError::invariant(
                    "bitblast mux received non-scalar branch values",
                ));
            }
            let state = if then_ty.state() == word::LogicStateKind::FourState
                || else_ty.state() == word::LogicStateKind::FourState
            {
                word::LogicStateKind::FourState
            } else {
                word::LogicStateKind::TwoState
            };
            let target = word::WordType::new(1, false, state).map_err(crate::SynthError::from)?;
            then_value = self.scalar_with_type(then_value, target, source)?;
            else_value = self.scalar_with_type(else_value, target, source)?;
        }
        let (bit, generated) =
            self.backend
                .emit_mux(self.module, cond, then_value, else_value, source)?;
        if let Some(value) = generated {
            self.record_generated_value(value)?;
        }
        Ok(bit)
    }

    pub(super) fn record_generated_value(
        &mut self,
        value: word::ValueId,
    ) -> Result<(), crate::SynthError> {
        if let Some(operator) = self.active_operator {
            self.provenance.set_value_operator(value, operator)?;
        }
        if let Some(region) = self.active_region {
            self.lowered_owners.set(value, region)?;
        }
        Ok(())
    }

    pub(super) fn constant(
        &mut self,
        bit: BitVal,
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let index = constant_index(bit, state);
        if let Some(value) = self.constants[index] {
            return Ok(value);
        }
        let ty = word::WordType::new(1, false, state).map_err(crate::SynthError::from)?;
        let value = self
            .module
            .constant(
                ConstBits::from_bits(vec![bit]).map_err(crate::SynthError::from)?,
                ty,
                source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let bit = self.backend.import_word(self.module, value);
        self.constants[index] = Some(bit);
        Ok(bit)
    }

    pub(super) fn zero_for_scalar(
        &mut self,
        value: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        let ty = self.bit_type(value)?;
        if ty.width() != 1 {
            return Err(crate::SynthError::invariant(format!(
                "zero fill expected a scalar value, got width {}",
                ty.width()
            )));
        }
        if !ty.is_signed() {
            return self.constant(BitVal::Zero, ty.state(), source);
        }
        let value = self
            .module
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero]).map_err(crate::SynthError::from)?,
                ty,
                source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        self.record_generated_value(value)?;
        Ok(self.backend.import_word(self.module, value))
    }

    pub(super) fn bit(&self, span: BitSpan, index: u32) -> ScalarBit {
        assert!(index < span.len());
        self.arena[(span.start + index) as usize]
    }

    pub(super) fn store(&mut self, bits: &[ScalarBit]) -> Result<BitSpan, crate::SynthError> {
        let len = NonZeroU32::new(bits.len().try_into().map_err(|_| {
            crate::SynthError::capacity("bit vector exceeds 32-bit width capacity")
        })?)
        .ok_or_else(|| crate::SynthError::invariant("bit vector cannot be empty"))?;
        let start =
            self.arena.len().try_into().map_err(|_| {
                crate::SynthError::capacity("bit arena exceeds 32-bit offset capacity")
            })?;
        self.arena.extend_from_slice(bits);
        Ok(BitSpan { start, len })
    }
}
