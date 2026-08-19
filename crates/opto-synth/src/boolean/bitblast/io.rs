// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBackend, BitBlaster, BitSpan, BitVal, ConstBits, ScalarBit, word};

pub(super) enum ConnectLowering {
    Boolean,
    PhysicalTriState(PhysicalTriStateConnect),
}

pub(super) struct PhysicalTriStateConnect {
    target: word::LValue,
    original: word::ValueId,
    data: word::ValueId,
    enable: word::Enable,
    source: word::SourceSpan,
}

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(super) fn binding_constant(
        &mut self,
        original: word::ValueId,
        bit: bool,
    ) -> Result<word::ValueId, crate::SynthError> {
        let stored = self
            .module
            .value(original)
            .ok_or_else(|| crate::SynthError::invariant("unknown AXM constant binding value"))?
            .clone();
        let ty =
            word::WordType::new(1, false, stored.ty.state()).map_err(crate::SynthError::from)?;
        let value = self
            .module
            .constant(
                ConstBits::from_bits(vec![if bit { BitVal::One } else { BitVal::Zero }])
                    .map_err(crate::SynthError::from)?,
                ty,
                stored.source,
            )
            .map_err(crate::SynthError::from)?;
        self.provenance.copy_value_origin(original, value)?;
        Ok(value)
    }

    pub(super) fn binding_projection(
        &mut self,
        original: word::ValueId,
        bit: u32,
    ) -> Result<word::ValueId, crate::SynthError> {
        let stored = self
            .module
            .value(original)
            .ok_or_else(|| crate::SynthError::invariant("unknown AXM binding value"))?
            .clone();
        if stored.ty.width() == 1 {
            return Ok(original);
        }
        let value = match stored.kind {
            word::ValueKind::Signal(reference) => {
                let lsb = reference
                    .lsb
                    .checked_add(bit)
                    .ok_or_else(|| crate::SynthError::capacity("AXM signal binding bit"))?;
                self.module
                    .read_signal_slice(reference.signal, lsb, 1, stored.source.clone())
                    .map_err(crate::SynthError::from)?
            }
            word::ValueKind::Constant(bits) => {
                let bit = bits.bit_lsb(bit).ok_or_else(|| {
                    crate::SynthError::invariant("AXM constant binding bit is absent")
                })?;
                if bit == BitVal::Z {
                    return Err(crate::SynthError::invalid(format!(
                        "tri-state constant in design '{}' at {:?} is not supported",
                        self.module.name(),
                        stored.source
                    )));
                }
                let ty = word::WordType::new(1, false, stored.ty.state())
                    .map_err(crate::SynthError::from)?;
                self.module
                    .constant(
                        ConstBits::from_bits(vec![bit]).map_err(crate::SynthError::from)?,
                        ty,
                        stored.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?
            }
            word::ValueKind::Operation(_) => self
                .module
                .extract(original, bit, 1, stored.source.clone())
                .map_err(crate::SynthError::from)?,
        };
        self.provenance.copy_value_origin(original, value)?;
        Ok(value)
    }

    pub(super) fn classify_connect(
        &self,
        connect: &word::Connect,
    ) -> Result<ConnectLowering, crate::SynthError> {
        if self.global_scope != super::GlobalBitblastScope::RegionalShell
            || self
                .module
                .signal(connect.target.signal)
                .is_none_or(|signal| signal.resolution != word::SignalResolution::TriState)
        {
            return Ok(ConnectLowering::Boolean);
        }

        let value = self.module.value(connect.value).ok_or_else(|| {
            crate::SynthError::invariant("physical tri-state connect value is unknown")
        })?;
        let word::ValueKind::Operation(operation) = value.kind else {
            return Err(crate::SynthError::invariant(
                "physical tri-state connect lost its explicit driver operation",
            ));
        };
        let operation = self.module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant("physical tri-state driver operation is unknown")
        })?;
        let word::OpKind::TriState { data, enable } = operation.kind else {
            return Err(crate::SynthError::invariant(
                "physical tri-state connect lost its data/enable contract",
            ));
        };
        Ok(ConnectLowering::PhysicalTriState(PhysicalTriStateConnect {
            target: connect.target.clone(),
            original: connect.value,
            data,
            enable,
            source: connect.source.clone(),
        }))
    }

    pub(super) fn lower_physical_tri_state_connect(
        &mut self,
        connect: PhysicalTriStateConnect,
    ) -> Result<(), crate::SynthError> {
        let value = self.module.value(connect.original).ok_or_else(|| {
            crate::SynthError::invariant("physical tri-state connect value is unknown")
        })?;
        if value.ty.width() != 1
            || self.lvalue_width(&connect.target)? != 1
            || self
                .module
                .value(connect.data)
                .is_none_or(|value| value.ty.width() != 1)
            || self
                .module
                .value(connect.enable.value)
                .is_none_or(|value| value.ty.width() != 1)
        {
            return Err(crate::SynthError::invariant(
                "non-scalar physical tri-state connect reached the regional shell",
            ));
        }
        let data = self.scalar_value(connect.data)?;
        let data = self.backend.word_value(data).ok_or_else(|| {
            crate::SynthError::invariant(
                "physical tri-state data cannot cross the regional Word shell",
            )
        })?;
        let enable_value = self.scalar_value(connect.enable.value)?;
        let enable_value = self.backend.word_value(enable_value).ok_or_else(|| {
            crate::SynthError::invariant(
                "physical tri-state enable cannot cross the regional Word shell",
            )
        })?;
        let lowered = self
            .module
            .tri_state(
                data,
                word::Enable {
                    value: enable_value,
                    active_high: connect.enable.active_high,
                },
                connect.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        self.provenance
            .copy_value_origin(connect.original, lowered)?;
        self.module
            .connect(connect.target, lowered, connect.source)
            .map_err(crate::SynthError::from)
    }

    pub(super) fn lower_boolean_connect(
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
            let value_ty = self.bit_type(value)?;
            if value_ty != target_ty {
                let word_value = self.backend.word_value(value).ok_or_else(|| {
                    crate::SynthError::invariant("AXM boundary cannot be written as a Word connect")
                })?;
                let cast = self
                    .module
                    .cast(
                        if value_ty.is_signed() {
                            word::CastKind::SignExtend
                        } else {
                            word::CastKind::ZeroExtend
                        },
                        word_value,
                        target_ty,
                        connect.source.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                self.record_generated_value(cast)?;
                value = self.backend.import_word(self.module, cast);
            }
            let lowered_ty = self.bit_type(value)?;
            if lowered_ty != target_ty {
                return Err(crate::SynthError::invariant(format!(
                    "bitblast failed to legalize scalar connect {:?}: target {target_ty:?}, value {lowered_ty:?}",
                    target.signal
                )));
            }
            let value = self.backend.word_value(value).ok_or_else(|| {
                crate::SynthError::invariant("AXM boundary cannot be written as a Word connect")
            })?;
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
            self.backend.word_value(bits[0]).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "instance {instance_index} port '{}' requires a Word value; an AXM literal cannot cross this boundary",
                    self.module.name_str(port)
                ))
            })?
        } else {
            bits.reverse();
            let bits = bits
                .into_iter()
                .map(|bit| {
                    self.backend.word_value(bit).ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "instance {instance_index} port '{}' requires Word values; AXM literals cannot cross this boundary",
                            self.module.name_str(port)
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
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
        if !self.active_values.insert(value_id) {
            return Err(crate::SynthError::invariant(
                "bit lowering encountered a static value cycle",
            ));
        }
        let result = self.value_uncached(value_id);
        self.active_values.remove(&value_id);
        result
    }

    fn value_uncached(&mut self, value_id: word::ValueId) -> Result<BitSpan, crate::SynthError> {
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
                    let value = self
                        .module
                        .read_signal_slice(reference.signal, bit, 1, value.source.clone())
                        .map_err(crate::SynthError::from)?;
                    Ok::<_, crate::SynthError>(self.backend.import_word(self.module, value))
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
        if let word::ValueKind::Operation(operation) = value.kind
            && self.global_scope == super::GlobalBitblastScope::RegionalShell
            && self
                .operation_regions
                .get(operation.index())
                .copied()
                .flatten()
                .is_none()
            && let Some(constant) = self.known_bits.constant(self.module, value_id)
        {
            let bits = self.constant_bits(value_id, &constant, value.ty, &value.source)?;
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
                if self.backend.treats_state_as_input()
                    && matches!(
                        operation.kind,
                        word::OpKind::Register(_) | word::OpKind::Latch(_)
                    )
                {
                    self.opaque_bits(value_id, value.ty.width(), &value.source)?
                } else if self.global_scope == super::GlobalBitblastScope::RegionalShell
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
                            if let Some(value) = self.backend.word_value(bit) {
                                self.lowered_owners.claim(value, owner);
                            }
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
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
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
        let publication = self
            .publication_contract
            .bits
            .get(&original)
            .cloned()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional shell value has no frozen publication contract",
                )
            })?;
        let endpoint_bits = self.signal_bits(endpoint, reference, source)?;
        if publication.len() != endpoint_bits.len() {
            return Err(crate::SynthError::invariant(
                "regional shell publication width changed after it was frozen",
            ));
        }
        let mut bits = Vec::with_capacity(endpoint_bits.len());
        for (index, (endpoint, owner)) in endpoint_bits
            .into_iter()
            .zip(publication.iter().copied())
            .enumerate()
        {
            let bit = match owner {
                super::FrozenPublicationBit::RegionArtifact => endpoint,
                super::FrozenPublicationBit::SubstrateConstant(value) => {
                    let bit = self.constant(value, ty.state(), source)?;
                    let value = self.backend.word_value(bit).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "substrate publication constant has no Word value",
                        )
                    })?;
                    let index = u32::try_from(index).map_err(|_| {
                        crate::SynthError::capacity("regional publication bit index")
                    })?;
                    self.module
                        .connect(
                            word::LValue::signal(signal).with_range(word::BitRange {
                                msb: index,
                                lsb: index,
                            }),
                            value,
                            source.clone(),
                        )
                        .map_err(crate::SynthError::from)?;
                    bit
                }
            };
            bits.push(bit);
        }
        for &bit in &bits {
            if let Some(value) = self.backend.word_value(bit) {
                self.provenance.copy_value_origin(original, value)?;
            }
        }
        Ok(bits)
    }

    pub(super) fn signal_bits(
        &mut self,
        original: word::ValueId,
        reference: word::SignalRef,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        if self.backend.follows_signal_drivers() {
            let mut bits = Vec::with_capacity(reference.width() as usize);
            for offset in 0..reference.width() {
                let lsb = reference
                    .lsb
                    .checked_add(offset)
                    .ok_or_else(|| crate::SynthError::invariant("signal bit index overflow"))?;
                if let Some((driver, bit)) = self.signal_drivers.resolve_bit(reference.signal, lsb)
                {
                    let span = self.value(driver)?;
                    if bit >= span.len() {
                        return Err(crate::SynthError::invariant(
                            "signal driver bit exceeds its lowered value",
                        ));
                    }
                    bits.push(self.bit(span, bit));
                } else {
                    let input = if reference.width() == 1 {
                        original
                    } else {
                        self.module
                            .read_signal_slice(reference.signal, lsb, 1, source.clone())
                            .map_err(crate::SynthError::from)?
                    };
                    bits.push(self.backend.import_word(self.module, input));
                }
            }
            return Ok(bits);
        }
        if reference.width() == 1 {
            return Ok(vec![self.backend.import_word(self.module, original)]);
        }
        (0..reference.width())
            .map(|offset| {
                let bit = reference
                    .lsb
                    .checked_add(offset)
                    .ok_or_else(|| crate::SynthError::invariant("signal bit index overflow"))?;
                let value = self
                    .module
                    .read_signal_slice(reference.signal, bit, 1, source.clone())
                    .map_err(crate::SynthError::from)?;
                Ok(self.backend.import_word(self.module, value))
            })
            .collect()
    }

    fn opaque_bits(
        &mut self,
        original: word::ValueId,
        width: u32,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        (0..width)
            .map(|bit| {
                let value = if width == 1 {
                    original
                } else {
                    self.module
                        .extract(original, bit, 1, source.clone())
                        .map_err(crate::SynthError::from)?
                };
                Ok(self.backend.import_word(self.module, value))
            })
            .collect()
    }

    pub(super) fn constant_bits(
        &mut self,
        original: word::ValueId,
        constant: &ConstBits,
        ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let original_is_constant = self
            .module
            .value(original)
            .is_some_and(|value| matches!(value.kind, word::ValueKind::Constant(_)));
        (0..ty.width())
            .map(|index| {
                let bit = constant.bit_lsb(index).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "constant has no bit {index} during bitblast"
                    ))
                })?;
                match bit {
                    BitVal::Zero | BitVal::One | BitVal::X
                        if ty.width() == 1 && original_is_constant =>
                    {
                        Ok(self.backend.import_word(self.module, original))
                    }
                    BitVal::Zero | BitVal::One | BitVal::X => {
                        self.constant(bit, ty.state(), source)
                    }
                    BitVal::Z => Err(crate::SynthError::invalid(format!(
                        "tri-state constant in design '{}' at {source:?} is not supported",
                        self.module.name()
                    ))),
                }
            })
            .collect()
    }
}
