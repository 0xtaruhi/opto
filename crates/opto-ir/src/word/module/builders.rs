// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Value-producing operation builders on the Word IR module.

use super::{
    BinaryOp, BitVal, CastKind, ConstBits, LatchOp, LogicStateKind, NonZeroU32, OpKind, RegisterOp,
    ResetKind, SourceSpan, UnaryOp, ValueId, ValueKind, WordError, WordModule, WordType,
};

impl WordModule {
    /// Adds a typed four-state constant value.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when widths differ, a two-state type receives an
    /// unknown or high-impedance bit, or the value arena is at capacity.
    pub fn constant(
        &mut self,
        bits: ConstBits,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        if bits.width() != ty.width() {
            return Err(WordError::new(format!(
                "constant width {} does not match type width {}",
                bits.width(),
                ty.width()
            )));
        }
        if ty.state() == LogicStateKind::TwoState
            && bits
                .as_slice()
                .iter()
                .any(|bit| matches!(bit, BitVal::X | BitVal::Z))
        {
            return Err(WordError::new(
                "two-state constant cannot contain x or z bits",
            ));
        }
        self.push_value(ValueKind::Constant(bits), ty, source)
    }

    /// Adds a unary operation and returns its result value.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for a foreign operand, invalid result type, or
    /// operation/value arena capacity failure.
    pub fn unary(
        &mut self,
        op: UnaryOp,
        arg: ValueId,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let arg_ty = self.value_ty(arg)?;
        let result_ty = match op {
            UnaryOp::LogicalNot
            | UnaryOp::ReductionAnd
            | UnaryOp::ReductionOr
            | UnaryOp::ReductionXor => WordType::new(1, false, arg_ty.state())?,
            UnaryOp::BitNot => arg_ty,
        };
        let kind = OpKind::Unary { op, arg };
        self.push_operation(kind, result_ty, source)
    }

    /// Adds a binary operation using SystemVerilog-compatible result typing.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for a foreign operand, invalid result type, or
    /// operation/value arena capacity failure.
    pub fn binary(
        &mut self,
        op: BinaryOp,
        left: ValueId,
        right: ValueId,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let left_ty = self.value_ty(left)?;
        let right_ty = self.value_ty(right)?;
        let state = left_ty.merged_state(right_ty);
        let result_ty = match op {
            BinaryOp::LogicalAnd
            | BinaryOp::LogicalOr
            | BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => WordType::new(1, false, state)?,
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr => {
                WordType::new(left_ty.width(), left_ty.is_signed(), state)?
            }
            _ => WordType::new(
                left_ty.width().max(right_ty.width()),
                left_ty.is_signed() && right_ty.is_signed(),
                state,
            )?,
        };
        let kind = OpKind::Binary { op, left, right };
        self.push_operation(kind, result_ty, source)
    }

    /// Adds a typed two-way multiplexer.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] unless the condition is one bit and both arms have
    /// identical types, or when an ID or arena capacity is invalid.
    pub fn mux(
        &mut self,
        cond: ValueId,
        then_value: ValueId,
        else_value: ValueId,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        self.require_value_width(cond, 1, "mux condition")?;
        let then_ty = self.value_ty(then_value)?;
        let else_ty = self.value_ty(else_value)?;
        if then_ty != else_ty {
            return Err(WordError::new(format!(
                "mux branch types differ: {then_ty:?} vs {else_ty:?}"
            )));
        }
        let kind = OpKind::Mux {
            cond,
            then_value,
            else_value,
        };
        self.push_operation(kind, then_ty, source)
    }

    /// Concatenates one or more values, most-significant part first.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for an empty list, a foreign value, width overflow,
    /// or operation/value arena capacity failure.
    pub fn concat(
        &mut self,
        parts: Vec<ValueId>,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        if parts.is_empty() {
            return Err(WordError::new("concat requires at least one part"));
        }
        let mut width = 0u32;
        let mut state = LogicStateKind::TwoState;
        for part in &parts {
            let ty = self.value_ty(*part)?;
            width = width
                .checked_add(ty.width())
                .ok_or_else(|| WordError::new("concat width exceeds 32-bit capacity"))?;
            state = state.merge(ty.state());
        }
        let ty = WordType::new(width, false, state)?;
        let kind = OpKind::Concat { parts };
        self.push_operation(kind, ty, source)
    }

    /// Extracts a statically positioned contiguous bit range.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for a foreign value, zero width, an out-of-range
    /// selection, arithmetic overflow, or arena capacity failure.
    pub fn extract(
        &mut self,
        value: ValueId,
        lsb: u32,
        width: u32,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let width = NonZeroU32::new(width)
            .ok_or_else(|| WordError::new("extract width must be non-zero"))?;
        let value_ty = self.value_ty(value)?;
        let end = lsb
            .checked_add(width.get())
            .ok_or_else(|| WordError::new("extract range exceeds 32-bit capacity"))?;
        if end > value_ty.width() {
            return Err(WordError::new(format!(
                "extract [{} +: {}] exceeds value width {}",
                lsb,
                width.get(),
                value_ty.width()
            )));
        }
        let ty = value_ty.with_width(width.get())?;
        let kind = OpKind::Extract { value, lsb, width };
        self.push_operation(kind, ty, source)
    }

    /// Extracts a fixed-width range at an unsigned runtime offset.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for foreign values, zero or excessive width, a
    /// signed offset, or arena capacity failure.
    pub fn dynamic_extract(
        &mut self,
        value: ValueId,
        offset: ValueId,
        width: u32,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let width = NonZeroU32::new(width)
            .ok_or_else(|| WordError::new("dynamic extract width must be non-zero"))?;
        let value_ty = self.value_ty(value)?;
        let offset_ty = self.value_ty(offset)?;
        if width.get() > value_ty.width() {
            return Err(WordError::new(format!(
                "dynamic extract width {} exceeds value width {}",
                width.get(),
                value_ty.width()
            )));
        }
        if offset_ty.is_signed() {
            return Err(WordError::new("dynamic extract offset must be unsigned"));
        }
        let ty = value_ty.with_width(width.get())?;
        let kind = OpKind::DynamicExtract {
            value,
            offset,
            width,
        };
        self.push_operation(kind, ty, source)
    }

    /// Replaces a runtime-indexed range and returns the complete updated value.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] for foreign values, a signed offset, an excessive
    /// replacement width, mismatched logic-state domains, or capacity failure.
    pub fn dynamic_insert(
        &mut self,
        value: ValueId,
        offset: ValueId,
        replacement: ValueId,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let value_ty = self.value_ty(value)?;
        let offset_ty = self.value_ty(offset)?;
        let replacement_ty = self.value_ty(replacement)?;
        if offset_ty.is_signed() {
            return Err(WordError::new("dynamic insert offset must be unsigned"));
        }
        if replacement_ty.width() > value_ty.width() {
            return Err(WordError::new(format!(
                "dynamic insert replacement width {} exceeds value width {}",
                replacement_ty.width(),
                value_ty.width()
            )));
        }
        if replacement_ty.state() != value_ty.state() {
            return Err(WordError::new(
                "dynamic insert replacement logic state differs from its value",
            ));
        }
        self.push_operation(
            OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            },
            value_ty,
            source,
        )
    }

    /// Applies an explicit extension or truncation.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when an extension shrinks, a truncation widens,
    /// the source ID is foreign, or an arena is at capacity.
    pub fn cast(
        &mut self,
        kind: CastKind,
        value: ValueId,
        target: WordType,
        source: SourceSpan,
    ) -> Result<ValueId, WordError> {
        let value_ty = self.value_ty(value)?;
        match kind {
            CastKind::ZeroExtend | CastKind::SignExtend if target.width() < value_ty.width() => {
                return Err(WordError::new("extend cast cannot shrink a value"));
            }
            CastKind::Truncate if target.width() > value_ty.width() => {
                return Err(WordError::new("truncate cast cannot widen a value"));
            }
            _ => {}
        }
        self.push_operation(
            OpKind::Cast {
                kind,
                value,
                target,
            },
            target,
            source,
        )
    }

    /// Adds an edge-triggered register operation.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] unless clock, enable, and reset controls are one
    /// bit, reset values match the data type, all IDs are local, and capacity
    /// remains in the value and operation arenas.
    pub fn register(&mut self, op: RegisterOp, source: SourceSpan) -> Result<ValueId, WordError> {
        self.require_value_width(op.clock, 1, "register clock")?;
        if let Some(enable) = op.enable {
            self.require_value_width(enable.value, 1, "register enable")?;
        }
        for reset in &op.resets {
            self.require_value_width(reset.value, 1, "register reset")?;
            let d_ty = self.value_ty(op.d)?;
            let reset_ty = self.value_ty(reset.reset_value)?;
            if reset_ty != d_ty {
                return Err(WordError::new(format!(
                    "register reset value type {reset_ty:?} does not match data type {d_ty:?}"
                )));
            }
        }
        let ty = self.value_ty(op.d)?;
        self.push_operation(OpKind::Register(op), ty, source)
    }

    /// Adds a level-sensitive latch operation.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] unless the enable and reset controls are one bit,
    /// every reset is asynchronous, reset values match the data type, all IDs
    /// are local, and arena capacity remains.
    pub fn latch(&mut self, op: LatchOp, source: SourceSpan) -> Result<ValueId, WordError> {
        self.require_value_width(op.enable.value, 1, "latch enable")?;
        for reset in &op.resets {
            if reset.kind != ResetKind::Async {
                return Err(WordError::new("latch reset must be asynchronous"));
            }
            self.require_value_width(reset.value, 1, "latch reset")?;
            let d_ty = self.value_ty(op.d)?;
            let reset_ty = self.value_ty(reset.reset_value)?;
            if reset_ty != d_ty {
                return Err(WordError::new(format!(
                    "latch reset value type {reset_ty:?} does not match data type {d_ty:?}"
                )));
            }
        }
        let ty = self.value_ty(op.d)?;
        self.push_operation(OpKind::Latch(op), ty, source)
    }
}
