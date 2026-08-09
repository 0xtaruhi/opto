// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBlaster, BitSpan, BitVal, ConstBits, word};

impl BitBlaster<'_> {
    pub(super) fn lower_connect(
        &mut self,
        connect: &word::Connect,
    ) -> Result<(), crate::SynthError> {
        let span = self.value(connect.value)?;
        let target_width = self.lvalue_width(&connect.target)?;
        if span.len() != target_width {
            return Err(crate::SynthError::invariant(format!(
                "bitblast width mismatch: target={}, value={}",
                target_width,
                span.len()
            )));
        }

        for offset in 0..target_width {
            let target = self.scalar_lvalue(&connect.target, offset)?;
            let mut value = self.bit(span, offset);
            let target_ty = self.scalar_lvalue_type(&target)?;
            let value_ty = self.value_type(value)?;
            if value_ty != target_ty {
                value = self
                    .module
                    .cast(
                        if value_ty.is_signed() {
                            word::CastKind::SignExtend
                        } else {
                            word::CastKind::ZeroExtend
                        },
                        value,
                        target_ty,
                        connect.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                self.record_generated_value(value)?;
            }
            let lowered_ty = self.value_type(value)?;
            if lowered_ty != target_ty {
                return Err(crate::SynthError::invariant(format!(
                    "bitblast failed to legalize scalar connect {:?}: target {target_ty:?}, value {lowered_ty:?}",
                    target.signal
                )));
            }
            self.module
                .connect(target, value, connect.source.clone())
                .map_err(|error| {
                    crate::SynthError::invariant(format!(
                        "bitblast scalar connect {:?} rejected after type legalization: {error}",
                        connect.target.signal
                    ))
                })?;
        }
        Ok(())
    }

    pub(super) fn lower_instance_connection(
        &mut self,
        instance_index: usize,
        port: opto_ir::NameId,
        value: word::ValueId,
        source: word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let span = self.value(value)?;
        let mut bits = (0..span.len())
            .map(|offset| self.bit(span, offset))
            .collect::<Vec<_>>();
        let lowered = if bits.len() == 1 {
            bits[0]
        } else {
            bits.reverse();
            self.module
                .concat(bits, source)
                .map_err(crate::SynthError::from)?
        };
        let instance = word::InstId::from_index(instance_index).map_err(crate::SynthError::Word)?;
        let port = self.module.name_str(port).to_string();
        self.module
            .set_instance_connection_value(instance, &port, lowered)
            .map_err(crate::SynthError::from)?;
        Ok(())
    }

    pub(super) fn lvalue_width(&self, target: &word::LValue) -> Result<u32, crate::SynthError> {
        if target.dynamic.is_some() {
            return Err(crate::SynthError::invariant(
                "dynamic connect target reached bitblast",
            ));
        }
        let signal = self.module.signal(target.signal).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown RTL signal {:?}", target.signal))
        })?;
        Ok(target
            .range
            .map_or(signal.ty.width(), word::BitRange::width))
    }

    pub(super) fn scalar_lvalue(
        &self,
        target: &word::LValue,
        offset: u32,
    ) -> Result<word::LValue, crate::SynthError> {
        let width = self.lvalue_width(target)?;
        if offset >= width {
            return Err(crate::SynthError::invariant(format!(
                "bitblast target offset {offset} exceeds width {width}"
            )));
        }
        if width == 1 && target.range.is_none() {
            return Ok(target.clone());
        }
        let bit = match target.range {
            Some(range) if range.msb >= range.lsb => range
                .lsb
                .checked_add(offset)
                .ok_or_else(|| crate::SynthError::invariant("bitblast lvalue index overflow"))?,
            Some(range) => range
                .lsb
                .checked_sub(offset)
                .ok_or_else(|| crate::SynthError::invariant("bitblast lvalue index underflow"))?,
            None => offset,
        };
        Ok(word::LValue::signal(target.signal).with_range(word::BitRange { msb: bit, lsb: bit }))
    }

    fn scalar_lvalue_type(
        &self,
        target: &word::LValue,
    ) -> Result<word::WordType, crate::SynthError> {
        let ty = self
            .module
            .signal(target.signal)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "unknown bitblast target signal {:?}",
                    target.signal
                ))
            })?
            .ty;
        word::WordType::new(1, target.range.is_none() && ty.is_signed(), ty.state())
            .map_err(crate::SynthError::from)
    }

    pub(super) fn value(&mut self, value_id: word::ValueId) -> Result<BitSpan, crate::SynthError> {
        if let Some(span) = self.cache.get(value_id.index()).and_then(|entry| *entry) {
            return Ok(span);
        }
        let value = self
            .module
            .value(value_id)
            .ok_or_else(|| crate::SynthError::invariant(format!("unknown RTL value {value_id:?}")))?
            .clone();
        if self.boundary_inputs.contains(&value_id) {
            let reference = match &value.kind {
                word::ValueKind::Signal(reference) => *reference,
                kind => {
                    return Err(crate::SynthError::invariant(format!(
                        "region-local hard boundary {value_id:?} reached bit lowering as {kind:?}; Word operations must be cut by the cone importer",
                    )));
                }
            };
            let bits = (0..value.ty.width())
                .map(|offset| {
                    let bit = reference.lsb.checked_add(offset).ok_or_else(|| {
                        crate::SynthError::capacity("regional boundary bit index")
                    })?;
                    self.module
                        .read_signal_slice(reference.signal, bit, 1, value.source.clone())
                        .map_err(crate::SynthError::from)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let span = self.store(&bits)?;
            let index = value_id.index();
            if self.cache.len() <= index {
                self.cache.resize(index + 1, None);
            }
            self.cache[index] = Some(span);
            return Ok(span);
        }
        let bits = match value.kind {
            word::ValueKind::Signal(reference) => {
                self.signal_bits(value_id, reference, &value.source)?
            }
            word::ValueKind::Constant(constant) => {
                self.constant_bits(value_id, &constant, value.ty, &value.source)?
            }
            word::ValueKind::Operation(operation_id) => {
                let operation = self
                    .module
                    .operation(operation_id)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "unknown RTL operation {operation_id:?}"
                        ))
                    })?
                    .clone();
                if self.global_scope == super::GlobalBitblastScope::RegionalShell
                    && self
                        .operation_regions
                        .get(operation_id.index())
                        .copied()
                        .flatten()
                        .is_some()
                    && !matches!(
                        operation.kind,
                        word::OpKind::Concat { .. }
                            | word::OpKind::Extract { .. }
                            | word::OpKind::Cast { .. }
                            | word::OpKind::Register(_)
                            | word::OpKind::Latch(_)
                    )
                {
                    self.regional_shell_bits(value_id, value.ty, &value.source)?
                } else {
                    let previous_region = self.active_region;
                    self.active_region = self
                        .operation_regions
                        .get(operation_id.index())
                        .copied()
                        .flatten();
                    let bits =
                        if self.is_native_scalar_operation(&operation.kind, value.ty.width())? {
                            vec![self.legalize_native_scalar_operation(
                                value_id,
                                operation.kind,
                                &operation.source,
                            )?]
                        } else {
                            self.operation_bits(
                                operation_id,
                                operation.kind,
                                value.ty,
                                &operation.source,
                            )?
                        };
                    if let Some(owner) = self.active_region {
                        for &bit in &bits {
                            // Width-only operations may forward an already-owned
                            // producer bit across a hard region boundary. The
                            // producer remains its unique owner; freshly emitted
                            // values were assigned strictly when constructed.
                            self.lowered_owners.claim(bit, owner);
                        }
                    }
                    self.active_region = previous_region;
                    bits
                }
            }
        };
        let span = self.store(&bits)?;
        let index = value_id.index();
        if self.cache.len() <= index {
            self.cache.resize(index + 1, None);
        }
        self.cache[index] = Some(span);
        Ok(span)
    }

    fn regional_shell_bits(
        &mut self,
        original: word::ValueId,
        ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let signal = self
            .module
            .add_generated_wire(ty, source.clone())
            .map_err(crate::SynthError::from)?;
        let endpoint = self
            .module
            .read_signal(signal, source.clone())
            .map_err(crate::SynthError::from)?;
        self.provenance.copy_value_origin(original, endpoint)?;
        let reference = match self.module.value(endpoint).map(|value| &value.kind) {
            Some(word::ValueKind::Signal(reference)) => *reference,
            _ => {
                return Err(crate::SynthError::invariant(
                    "generated regional shell endpoint is not a signal value",
                ));
            }
        };
        let bits = self.signal_bits(endpoint, reference, source)?;
        for &bit in &bits {
            self.provenance.copy_value_origin(original, bit)?;
        }
        Ok(bits)
    }

    pub(super) fn signal_bits(
        &mut self,
        original: word::ValueId,
        reference: word::SignalRef,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        if reference.width() == 1 {
            return Ok(vec![original]);
        }
        (0..reference.width())
            .map(|offset| {
                let bit = reference
                    .lsb
                    .checked_add(offset)
                    .ok_or_else(|| crate::SynthError::invariant("signal bit index overflow"))?;
                self.module
                    .read_signal_slice(reference.signal, bit, 1, source.clone())
                    .map_err(crate::SynthError::from)
            })
            .collect()
    }

    pub(super) fn constant_bits(
        &mut self,
        original: word::ValueId,
        constant: &ConstBits,
        ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        (0..ty.width())
            .map(|index| {
                let bit = constant.bit_lsb(index).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "constant has no bit {index} during bitblast"
                    ))
                })?;
                let resolved =
                    crate::boolean::resolve_synthesis_bit(bit, self.module.name(), source)?;
                match (bit, resolved) {
                    (BitVal::Zero | BitVal::One, _) if ty.width() == 1 => Ok(original),
                    (BitVal::X, BitVal::Zero) if ty.width() == 1 => {
                        self.zero_for_scalar(original, source)
                    }
                    (_, BitVal::Zero | BitVal::One) => self.constant(resolved, ty.state(), source),
                    (_, BitVal::X | BitVal::Z) => {
                        unreachable!("the synthesis constant policy returns only two-state values")
                    }
                }
            })
            .collect()
    }
}
