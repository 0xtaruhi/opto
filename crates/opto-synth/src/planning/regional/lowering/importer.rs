// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Imports one region's Word dependency cone into a task-local module.

use super::{BTreeMap, RegionalWordImporter, word};

#[derive(Debug, Clone, Copy)]
enum OperandImport {
    Whole,
    Bits,
}

#[derive(Debug, Clone, Copy)]
struct DynamicBarrel {
    source: word::OpId,
    value: word::ValueId,
    offset: word::ValueId,
    input_width: u32,
    maximum_offset: u32,
}

impl RegionalWordImporter<'_> {
    pub(super) fn import_memories(
        &mut self,
        memories: &[word::MemoryId],
    ) -> Result<(), crate::SynthError> {
        let mut local_memories = BTreeMap::new();
        for &memory in memories {
            let source = self.source.memory(memory).ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local memory import references an unknown memory",
                )
            })?;
            let local = self
                .module
                .add_memory(
                    self.source.name_str(source.name),
                    source.element_type,
                    source.depth,
                    source.source.clone(),
                )
                .map_err(crate::SynthError::from)?;
            local_memories.insert(memory, local);
        }
        for read in self
            .source
            .memory_read_ports()
            .iter()
            .filter(|read| local_memories.contains_key(&read.memory))
        {
            let signal = self.source.signal(read.data).ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local memory read references an unknown data signal",
                )
            })?;
            let local = self
                .module
                .add_generated_wire(signal.ty, signal.source.clone())
                .map_err(crate::SynthError::from)?;
            self.memory_signals.insert(read.data, local);
        }
        for read in self
            .source
            .memory_read_ports()
            .iter()
            .filter(|read| local_memories.contains_key(&read.memory))
        {
            let timing = match read.timing {
                word::MemoryReadTiming::Asynchronous => word::MemoryReadTiming::Asynchronous,
                word::MemoryReadTiming::Synchronous {
                    clock,
                    enable,
                    disabled,
                } => word::MemoryReadTiming::Synchronous {
                    clock: word::MemoryClock {
                        value: self.import(clock.value)?,
                        edge: clock.edge,
                    },
                    enable: enable
                        .map(|enable| self.import_enable(enable))
                        .transpose()?,
                    disabled,
                },
            };
            let address = self.import(read.address)?;
            let memory = local_memories.get(&read.memory).copied().ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local memory read lost its imported memory owner",
                )
            })?;
            let data = self
                .memory_signals
                .get(&read.data)
                .copied()
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "region-local memory read lost its imported data signal",
                    )
                })?;
            self.module
                .add_memory_read_port(word::MemoryReadPort {
                    memory,
                    address,
                    data,
                    timing,
                    read_during_write: read.read_during_write,
                    source: read.source.clone(),
                })
                .map_err(crate::SynthError::from)?;
        }
        for write in self
            .source
            .memory_write_ports()
            .iter()
            .filter(|write| local_memories.contains_key(&write.memory))
        {
            let address = self.import(write.address)?;
            let data = self.import(write.data)?;
            let clock = word::MemoryClock {
                value: self.import(write.clock.value)?,
                edge: write.clock.edge,
            };
            let enable = write
                .enable
                .map(|enable| self.import_enable(enable))
                .transpose()?;
            let mask = write
                .mask
                .map(|mask| {
                    Ok::<_, crate::SynthError>(word::MemoryWriteMask {
                        value: self.import(mask.value)?,
                        granularity: mask.granularity,
                        active_high: mask.active_high,
                    })
                })
                .transpose()?;
            let memory = local_memories.get(&write.memory).copied().ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local memory write lost its imported memory owner",
                )
            })?;
            self.module
                .add_memory_write_port(word::MemoryWritePort {
                    memory,
                    address,
                    data,
                    clock,
                    enable,
                    mask,
                    priority: write.priority,
                    source: write.source.clone(),
                })
                .map_err(crate::SynthError::from)?;
        }
        Ok(())
    }

    pub(super) fn import(
        &mut self,
        source: word::ValueId,
    ) -> Result<word::ValueId, crate::SynthError> {
        if let Some(local) = self.source_to_local.get(&source).copied() {
            return Ok(local);
        }
        let value = self.source.value(source).ok_or_else(|| {
            crate::SynthError::invariant("region-local Word import reached an unknown value")
        })?;
        if self.visiting.contains(&source) {
            if !self.source_acyclic {
                // Importing a packed value is deliberately coarser than the
                // bit-level dependency graph. Prove the source graph sound
                // before cutting a false whole-value recursion at a local
                // boundary; a genuine feedback loop retains its precise HDL
                // diagnostic from the shared validator.
                crate::word::cycle::validate_combinational_acyclic(self.source)?;
                self.source_acyclic = true;
            }
            return self.import_active_value(source, value.ty, &value.source);
        }
        self.visiting.insert(source);
        self.import_path.push(source);
        let local = match value.kind {
            word::ValueKind::Constant(ref bits) => self
                .module
                .constant(bits.clone(), value.ty, value.source.clone())
                .map_err(crate::SynthError::from)?,
            word::ValueKind::Signal(reference) => {
                self.import_signal(source, reference, value.ty, &value.source)?
            }
            word::ValueKind::Operation(operation)
                if self
                    .operation_regions
                    .get(operation.index())
                    .copied()
                    .flatten()
                    == Some(self.region) =>
            {
                if self.operation_is_state(operation) {
                    self.import_state_feedback(source, value.ty, &value.source)?
                } else {
                    self.import_operation(operation)?
                }
            }
            word::ValueKind::Operation(_) => {
                self.import_boundary(source, value.ty, &value.source)?
            }
        };
        if self.import_path.pop() != Some(source) {
            return Err(crate::SynthError::invariant(
                "region-local Word import path lost stack order",
            ));
        }
        self.visiting.remove(&source);
        self.source_to_local.insert(source, local);
        Ok(local)
    }

    fn import_active_value(
        &mut self,
        source: word::ValueId,
        ty: word::WordType,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let mut parts = (0..ty.width())
            .map(|bit| self.import_value_bit(source, bit, span))
            .collect::<Result<Vec<_>, _>>()?;
        let mut local = if let [part] = parts.as_slice() {
            *part
        } else {
            parts.reverse();
            let local = self
                .module
                .concat(parts, span.clone())
                .map_err(crate::SynthError::from)?;
            self.record_generated_operation(local)?;
            local
        };
        let local_ty = self
            .module
            .value(local)
            .map(|stored| stored.ty)
            .ok_or_else(|| crate::SynthError::invariant("active imported value disappeared"))?;
        if local_ty != ty {
            local = self
                .module
                .cast(
                    if ty.is_signed() {
                        word::CastKind::SignExtend
                    } else {
                        word::CastKind::ZeroExtend
                    },
                    local,
                    ty,
                    span.clone(),
                )
                .map_err(crate::SynthError::from)?;
            self.record_generated_operation(local)?;
        }
        Ok(local)
    }

    fn import_signal(
        &mut self,
        source: word::ValueId,
        reference: word::SignalRef,
        ty: word::WordType,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        if let Some(&signal) = self.memory_signals.get(&reference.signal) {
            return self
                .module
                .read_signal_slice(signal, reference.lsb, reference.width(), span.clone())
                .map_err(crate::SynthError::from);
        }
        let Some(bits) = self.signal_drivers.resolve_reference(reference) else {
            return self.import_boundary(source, ty, span);
        };
        if let Some(driver) = self
            .signal_drivers
            .exact_reference_driver(self.source, reference, ty)
        {
            self.import(driver)
        } else {
            let mut parts = Vec::with_capacity(bits.len());
            for (driver, bit) in bits {
                if self.visiting.contains(&driver) {
                    parts.push(self.import_value_bit(driver, bit, span)?);
                } else {
                    let local = self.import(driver)?;
                    parts.push(self.extract_local_bit(local, bit, span)?);
                }
            }
            let mut local = if let [part] = parts.as_slice() {
                *part
            } else {
                if parts.is_empty() {
                    return Err(crate::SynthError::invariant(
                        "region-local signal reconstruction has no driver bits",
                    ));
                }
                parts.reverse();
                let local = self
                    .module
                    .concat(parts, span.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
                local
            };
            let local_ty = self
                .module
                .value(local)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "region-local reconstructed signal disappeared during import",
                    )
                })?
                .ty;
            if local_ty != ty {
                local = self
                    .module
                    .cast(
                        if ty.is_signed() {
                            word::CastKind::SignExtend
                        } else {
                            word::CastKind::ZeroExtend
                        },
                        local,
                        ty,
                        span.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
            }
            Ok(local)
        }
    }

    fn import_value_bit(
        &mut self,
        source: word::ValueId,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        if let Some(&local) = self.imported_bits.get(&(source, bit)) {
            self.extend_generated_operation_sources(local)?;
            return Ok(local);
        }
        let source_ty = self.source_value_type(source)?;
        if bit >= source_ty.width() {
            return Err(crate::SynthError::invariant(
                "region-local bit import exceeds its source value",
            ));
        }
        let local = if self.visiting.contains(&source) {
            self.import_active_value_bit(source, bit, span)?
        } else {
            let local = self.import(source)?;
            self.extract_local_bit(local, bit, span)?
        };
        self.imported_bits.insert((source, bit), local);
        Ok(local)
    }

    fn import_active_value_bit(
        &mut self,
        source: word::ValueId,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let kind = self
            .source
            .value(source)
            .map(|stored| stored.kind.clone())
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "active region-local bit import references an unknown value",
                )
            })?;
        match kind {
            word::ValueKind::Signal(reference) => {
                let bits = self
                    .signal_drivers
                    .resolve_reference(reference)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "active region-local signal bit has no exact driver",
                        )
                    })?;
                let (driver, driver_bit) = bits.get(bit as usize).copied().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "active region-local signal bit exceeds its driver map",
                    )
                })?;
                self.import_value_bit(driver, driver_bit, span)
            }
            word::ValueKind::Operation(operation)
                if self
                    .operation_regions
                    .get(operation.index())
                    .copied()
                    .flatten()
                    == Some(self.region) =>
            {
                if self.operation_is_state(operation) {
                    let local = self.import_recursive_boundary(
                        source,
                        self.source_value_type(source)?,
                        span,
                    )?;
                    return self.extract_local_bit(local, bit, span);
                }
                let operation_kind = self
                    .source
                    .operation(operation)
                    .map(|operation| operation.kind.clone())
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "active region-local bit references an unknown operation",
                        )
                    })?;
                self.import_active_operation_bit(operation, operation_kind, bit, span)
            }
            word::ValueKind::Constant(_) | word::ValueKind::Operation(_) => {
                let local =
                    self.import_recursive_boundary(source, self.source_value_type(source)?, span)?;
                self.extract_local_bit(local, bit, span)
            }
        }
    }

    fn import_active_operation_bit(
        &mut self,
        source: word::OpId,
        operation: word::OpKind,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        match operation {
            word::OpKind::Unary {
                op: word::UnaryOp::BitNot,
                arg,
            } => {
                let arg = self.import_value_bit(arg, bit, span)?;
                let local = self
                    .module
                    .unary(word::UnaryOp::BitNot, arg, span.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
                Ok(local)
            }
            word::OpKind::Unary {
                op: word::UnaryOp::LogicalNot,
                arg,
            } if bit == 0 && self.source_value_type(arg)?.width() == 1 => {
                let arg = self.import_value_bit(arg, 0, span)?;
                let local = self
                    .module
                    .unary(word::UnaryOp::LogicalNot, arg, span.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
                Ok(local)
            }
            word::OpKind::Binary {
                op: op @ (word::BinaryOp::BitAnd | word::BinaryOp::BitOr | word::BinaryOp::BitXor),
                left,
                right,
            } => {
                let left = self.import_extended_value_bit(left, bit, span)?;
                let right = self.import_extended_value_bit(right, bit, span)?;
                let local = self
                    .module
                    .binary(op, left, right, span.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
                Ok(local)
            }
            word::OpKind::Binary {
                op: op @ (word::BinaryOp::LogicalAnd | word::BinaryOp::LogicalOr),
                left,
                right,
            } if bit == 0
                && self.source_value_type(left)?.width() == 1
                && self.source_value_type(right)?.width() == 1 =>
            {
                let left = self.import_value_bit(left, 0, span)?;
                let right = self.import_value_bit(right, 0, span)?;
                let local = self
                    .module
                    .binary(op, left, right, span.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
                Ok(local)
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                let cond = self.import(cond)?;
                let then_value = self.import_value_bit(then_value, bit, span)?;
                let else_value = self.import_value_bit(else_value, bit, span)?;
                let local = self
                    .module
                    .mux(cond, then_value, else_value, span.clone())
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
                Ok(local)
            }
            word::OpKind::Concat { parts } => {
                let mut remaining = bit;
                for part in parts.into_iter().rev() {
                    let width = self.source_value_type(part)?.width();
                    if remaining < width {
                        return self.import_value_bit(part, remaining, span);
                    }
                    remaining -= width;
                }
                Err(crate::SynthError::invariant(
                    "active concatenation bit exceeds its parts",
                ))
            }
            word::OpKind::Extract { value, lsb, .. } => self.import_value_bit(
                value,
                lsb.checked_add(bit).ok_or_else(|| {
                    crate::SynthError::invariant("active extract bit offset overflow")
                })?,
                span,
            ),
            word::OpKind::Cast { kind, value, .. } => {
                let source_ty = self.source_value_type(value)?;
                if bit < source_ty.width() {
                    self.import_value_bit(value, bit, span)
                } else if kind == word::CastKind::SignExtend {
                    self.import_value_bit(value, source_ty.width() - 1, span)
                } else {
                    self.constant_bit(false, span)
                }
            }
            word::OpKind::DynamicExtract {
                value,
                offset,
                width,
            } => {
                if let Some(offset) =
                    crate::word::known_u32(self.source, &mut self.known_bits, offset)
                {
                    let selected = offset.checked_add(bit).ok_or_else(|| {
                        crate::SynthError::invariant("active dynamic extract bit offset overflow")
                    })?;
                    let input_width = self.source_value_type(value)?.width();
                    if offset
                        .checked_add(width.get())
                        .is_some_and(|end| end <= input_width)
                    {
                        self.import_value_bit(value, selected, span)
                    } else {
                        self.constant_bit(false, span)
                    }
                } else {
                    self.import_dynamic_extract_bit(source, value, offset, width.get(), bit, span)
                }
            }
            operation => self.import_shared_active_operation_bit(source, &operation, bit, span),
        }
    }

    fn import_shared_active_operation_bit(
        &mut self,
        source: word::OpId,
        operation: &word::OpKind,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        if let Some(&local) = self.active_operations.get(&source) {
            self.extend_generated_operation_sources(local)?;
            return self.extract_local_bit(local, bit, span);
        }
        let local = self.import_operation_kind(operation, span, OperandImport::Bits)?;
        self.active_operations.insert(source, local);
        self.extract_local_bit(local, bit, span)
    }

    fn import_dynamic_extract_bit(
        &mut self,
        source: word::OpId,
        value: word::ValueId,
        offset: word::ValueId,
        width: u32,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let input_width = self.source_value_type(value)?.width();
        let offset_width = self.source_value_type(offset)?.width();
        let stages = offset_width.min(u32::BITS - input_width.leading_zeros());
        let maximum_offset = input_width.checked_sub(width).ok_or_else(|| {
            crate::SynthError::invariant("dynamic extract width exceeds its input")
        })?;
        let barrel = DynamicBarrel {
            source,
            value,
            offset,
            input_width,
            maximum_offset,
        };
        let selected = self.import_dynamic_barrel_bit(barrel, stages, bit, 0, span)?;
        if stages == offset_width {
            return Ok(selected);
        }
        let high = if let Some(&high) = self.dynamic_high_bits.get(&source) {
            self.extend_generated_operation_sources(high)?;
            high
        } else {
            let mut high = None;
            for selector_bit in stages..offset_width {
                let bit = match self.known_bits.bit(self.source, offset, selector_bit) {
                    word::KnownBit::Zero => continue,
                    word::KnownBit::One => {
                        high = Some(self.constant_bit(true, span)?);
                        break;
                    }
                    word::KnownBit::Unknown => self.import_value_bit(offset, selector_bit, span)?,
                };
                high = Some(if let Some(previous) = high {
                    let combined = self
                        .module
                        .binary(word::BinaryOp::BitOr, previous, bit, span.clone())
                        .map_err(crate::SynthError::from)?;
                    self.record_generated_operation(combined)?;
                    combined
                } else {
                    bit
                });
            }
            let high = match high {
                Some(high) => high,
                None => self.constant_bit(false, span)?,
            };
            self.dynamic_high_bits.insert(source, high);
            high
        };
        let zero = self.constant_bit(false, span)?;
        let selected = self
            .module
            .mux(high, zero, selected, span.clone())
            .map_err(crate::SynthError::from)?;
        self.record_generated_operation(selected)?;
        Ok(selected)
    }

    fn import_dynamic_barrel_bit(
        &mut self,
        barrel: DynamicBarrel,
        stage: u32,
        bit: u32,
        base_offset: u64,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        if base_offset > u64::from(barrel.maximum_offset) {
            return self.constant_bit(false, span);
        }
        let maximum_remaining = (1u64 << stage) - 1;
        let fully_valid = base_offset
            .checked_add(maximum_remaining)
            .is_some_and(|offset| offset <= u64::from(barrel.maximum_offset));
        if fully_valid
            && let Some(&local) = self.dynamic_barrel_bits.get(&(barrel.source, stage, bit))
        {
            self.extend_generated_operation_sources(local)?;
            return Ok(local);
        }
        let local = if stage == 0 {
            self.import_value_bit(barrel.value, bit, span)?
        } else {
            let selector_bit = stage - 1;
            let shift = 1u32 << selector_bit;
            let known = self
                .known_bits
                .bit(self.source, barrel.offset, selector_bit);
            let mut branch = |upper| {
                let bit = if upper {
                    bit.checked_add(shift)
                        .filter(|&bit| bit < barrel.input_width)
                } else {
                    Some(bit)
                };
                let base_offset = base_offset + u64::from(upper) * u64::from(shift);
                bit.map(|bit| {
                    self.import_dynamic_barrel_bit(barrel, selector_bit, bit, base_offset, span)
                })
                .transpose()?
                .map_or_else(|| self.constant_bit(false, span), Ok)
            };
            match known {
                word::KnownBit::Zero => branch(false)?,
                word::KnownBit::One => branch(true)?,
                word::KnownBit::Unknown => {
                    let lower = branch(false)?;
                    let upper = branch(true)?;
                    let select = self.import_value_bit(barrel.offset, selector_bit, span)?;
                    let local = self
                        .module
                        .mux(select, upper, lower, span.clone())
                        .map_err(crate::SynthError::from)?;
                    self.record_generated_operation(local)?;
                    local
                }
            }
        };
        if fully_valid {
            self.dynamic_barrel_bits
                .insert((barrel.source, stage, bit), local);
        }
        Ok(local)
    }

    fn import_extended_value_bit(
        &mut self,
        source: word::ValueId,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let ty = self.source_value_type(source)?;
        if bit < ty.width() {
            self.import_value_bit(source, bit, span)
        } else if ty.is_signed() {
            self.import_value_bit(source, ty.width() - 1, span)
        } else {
            self.constant_bit(false, span)
        }
    }

    fn extract_local_bit(
        &mut self,
        local: word::ValueId,
        bit: u32,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let ty = self
            .module
            .value(local)
            .map(|stored| stored.ty)
            .ok_or_else(|| {
                crate::SynthError::invariant("region-local bit source disappeared during import")
            })?;
        if bit >= ty.width() {
            return Err(crate::SynthError::invariant(
                "region-local bit exceeds its imported value",
            ));
        }
        if ty.width() == 1 {
            return Ok(local);
        }
        let part = self
            .module
            .extract(local, bit, 1, span.clone())
            .map_err(crate::SynthError::from)?;
        self.record_generated_operation(part)?;
        Ok(part)
    }

    fn constant_bit(
        &mut self,
        value: bool,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        self.module
            .constant(
                opto_ir::ConstBits::from_bin_str(if value { "1" } else { "0" })
                    .map_err(crate::SynthError::from)?,
                word::WordType::bits(1).map_err(crate::SynthError::from)?,
                span.clone(),
            )
            .map_err(crate::SynthError::from)
    }

    fn source_value_type(&self, value: word::ValueId) -> Result<word::WordType, crate::SynthError> {
        self.source
            .value(value)
            .map(|stored| stored.ty)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local bit dependency references an unknown source value",
                )
            })
    }

    fn record_generated_operation(
        &mut self,
        value: word::ValueId,
    ) -> Result<(), crate::SynthError> {
        let operation = match self
            .module
            .value(value)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local generated value disappeared during import",
                )
            })?
            .kind
        {
            word::ValueKind::Operation(operation) => operation,
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => {
                return Err(crate::SynthError::invariant(
                    "region-local generated operation returned a non-operation value",
                ));
            }
        };
        let sources = self.current_operation_sources();
        self.operation_sources.set(operation, sources)
    }

    fn current_operation_sources(&self) -> Vec<word::OpId> {
        let mut sources = self
            .import_path
            .iter()
            .filter_map(|&value| {
                let word::ValueKind::Operation(operation) = self.source.value(value)?.kind else {
                    return None;
                };
                (self
                    .operation_regions
                    .get(operation.index())
                    .copied()
                    .flatten()
                    == Some(self.region))
                .then_some(operation)
            })
            .collect::<Vec<_>>();
        sources.sort_unstable();
        sources.dedup();
        sources
    }

    fn extend_generated_operation_sources(
        &mut self,
        value: word::ValueId,
    ) -> Result<(), crate::SynthError> {
        let stored = self.module.value(value).ok_or_else(|| {
            crate::SynthError::invariant(
                "cached region-local generated value disappeared during import",
            )
        })?;
        let word::ValueKind::Operation(operation) = stored.kind else {
            return Ok(());
        };
        let additional = self.current_operation_sources();
        self.operation_sources.merge(operation, additional)
    }

    fn operation_is_state(&self, operation: word::OpId) -> bool {
        self.source.operation(operation).is_some_and(|operation| {
            matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            )
        })
    }

    fn import_boundary(
        &mut self,
        source: word::ValueId,
        ty: word::WordType,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        if let Some(word::ValueKind::Signal(reference)) =
            self.source.value(source).map(|value| &value.kind)
        {
            if let Some(&(stored_ty, local)) = self.boundary_signals.get(reference)
                && stored_ty == ty
            {
                self.boundary_bindings.push((source, local));
                return Ok(local);
            }
            let local_signal =
                if let Some(&signal) = self.boundary_port_signals.get(&reference.signal) {
                    signal
                } else {
                    let source_signal = self.source.signal(reference.signal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "region-local boundary references an unknown source signal",
                        )
                    })?;
                    let port = self
                        .module
                        .add_port(
                            format!("boundary$signal{}", reference.signal.index()),
                            word::PortDirection::Input,
                            source_signal.ty,
                            span.clone(),
                        )
                        .map_err(crate::SynthError::from)?;
                    let signal = self
                        .module
                        .port(port)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "new region-local signal boundary is absent from the port arena",
                            )
                        })?
                        .signal;
                    self.boundary_port_signals.insert(reference.signal, signal);
                    signal
                };
            let mut local = self
                .module
                .read_signal_slice(local_signal, reference.lsb, reference.width(), span.clone())
                .map_err(crate::SynthError::from)?;
            let local_ty = self
                .module
                .value(local)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "region-local signal boundary value is absent from the arena",
                    )
                })?
                .ty;
            if local_ty != ty {
                local = self
                    .module
                    .cast(
                        if ty.is_signed() {
                            word::CastKind::SignExtend
                        } else {
                            word::CastKind::ZeroExtend
                        },
                        local,
                        ty,
                        span.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                self.record_generated_operation(local)?;
            }
            self.boundary_signals.insert(*reference, (ty, local));
            self.boundary_bindings.push((source, local));
            return Ok(local);
        }
        let port = self
            .module
            .add_port(
                format!("boundary${}", source.index()),
                word::PortDirection::Input,
                ty,
                span.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let signal = self
            .module
            .port(port)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "new region-local boundary port is absent from the port arena",
                )
            })?
            .signal;
        let local = self
            .module
            .read_signal(signal, span.clone())
            .map_err(crate::SynthError::from)?;
        self.boundary_bindings.push((source, local));
        Ok(local)
    }

    fn import_state_feedback(
        &mut self,
        source: word::ValueId,
        ty: word::WordType,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        let signal = self
            .module
            .add_generated_wire(ty, span.clone())
            .map_err(crate::SynthError::from)?;
        let local = self
            .module
            .read_signal(signal, span.clone())
            .map_err(crate::SynthError::from)?;
        self.state_feedback_signals.insert(source, signal);
        Ok(local)
    }

    fn import_recursive_boundary(
        &mut self,
        source: word::ValueId,
        ty: word::WordType,
        span: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        if let Some(&local) = self.recursive_boundaries.get(&source) {
            return Ok(local);
        }
        let local = self.import_boundary(source, ty, span)?;
        self.recursive_boundaries.insert(source, local);
        Ok(local)
    }

    pub(super) fn import_operation(
        &mut self,
        source: word::OpId,
    ) -> Result<word::ValueId, crate::SynthError> {
        let operation = self.source.operation(source).cloned().ok_or_else(|| {
            crate::SynthError::invariant("region-local Word import reached an unknown operation")
        })?;
        let span = operation.source.clone();
        let local = self.import_operation_kind(&operation.kind, &span, OperandImport::Whole)?;
        let word::ValueKind::Operation(local_operation) = self
            .module
            .value(local)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local operation builder returned an unknown value",
                )
            })?
            .kind
        else {
            return Err(crate::SynthError::invariant(
                "region-local operation builder returned a non-operation value",
            ));
        };
        self.operation_sources.set(local_operation, [source])?;
        Ok(local)
    }

    fn import_operation_kind(
        &mut self,
        operation: &word::OpKind,
        span: &word::SourceSpan,
        operands: OperandImport,
    ) -> Result<word::ValueId, crate::SynthError> {
        let local = match operation {
            word::OpKind::Unary { op, arg } => {
                let arg = self.import_operand(*arg, span, operands)?;
                self.module.unary(*op, arg, span.clone())
            }
            word::OpKind::Binary { op, left, right } => {
                let left = self.import_operand(*left, span, operands)?;
                let right = self.import_operand(*right, span, operands)?;
                self.module.binary(*op, left, right, span.clone())
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                let cond = self.import_operand(*cond, span, operands)?;
                let then_value = self.import_operand(*then_value, span, operands)?;
                let else_value = self.import_operand(*else_value, span, operands)?;
                self.module.mux(cond, then_value, else_value, span.clone())
            }
            word::OpKind::TriState { data, enable } => {
                let data = self.import_operand(*data, span, operands)?;
                let enable = self.import_enable_with(*enable, span, operands)?;
                self.module.tri_state(data, enable, span.clone())
            }
            word::OpKind::Concat { parts } => {
                let parts = parts
                    .iter()
                    .map(|&part| self.import_operand(part, span, operands))
                    .collect::<Result<Vec<_>, _>>()?;
                self.module.concat(parts, span.clone())
            }
            word::OpKind::Extract { value, lsb, width } => {
                let value = self.import_operand(*value, span, operands)?;
                self.module.extract(value, *lsb, width.get(), span.clone())
            }
            word::OpKind::DynamicExtract {
                value,
                offset,
                width,
            } => {
                let value = self.import_operand(*value, span, operands)?;
                let offset = self.import_operand(*offset, span, operands)?;
                self.module
                    .dynamic_extract(value, offset, width.get(), span.clone())
            }
            word::OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                let value = self.import_operand(*value, span, operands)?;
                let offset = self.import_operand(*offset, span, operands)?;
                let replacement = self.import_operand(*replacement, span, operands)?;
                self.module
                    .dynamic_insert(value, offset, replacement, span.clone())
            }
            word::OpKind::Cast {
                kind,
                value,
                target,
            } => {
                let value = self.import_operand(*value, span, operands)?;
                self.module.cast(*kind, value, *target, span.clone())
            }
            word::OpKind::Register(register) => {
                let register = word::RegisterOp {
                    name: None,
                    d: self.import_operand(register.d, span, operands)?,
                    clock: self.import_operand(register.clock, span, operands)?,
                    edge: register.edge,
                    enable: register
                        .enable
                        .map(|enable| self.import_enable_with(enable, span, operands))
                        .transpose()?,
                    resets: register
                        .resets
                        .iter()
                        .map(|&reset| self.import_reset_with(reset, span, operands))
                        .collect::<Result<Vec<_>, _>>()?,
                };
                self.module.register(register, span.clone())
            }
            word::OpKind::Latch(latch) => {
                let latch = word::LatchOp {
                    name: None,
                    d: self.import_operand(latch.d, span, operands)?,
                    enable: self.import_enable_with(latch.enable, span, operands)?,
                    resets: latch
                        .resets
                        .iter()
                        .map(|&reset| self.import_reset_with(reset, span, operands))
                        .collect::<Result<Vec<_>, _>>()?,
                };
                self.module.latch(latch, span.clone())
            }
        }
        .map_err(crate::SynthError::from)?;
        self.record_generated_operation(local)?;
        Ok(local)
    }

    fn import_operand(
        &mut self,
        source: word::ValueId,
        span: &word::SourceSpan,
        operands: OperandImport,
    ) -> Result<word::ValueId, crate::SynthError> {
        match operands {
            OperandImport::Whole => self.import(source),
            OperandImport::Bits => {
                self.import_active_value(source, self.source_value_type(source)?, span)
            }
        }
    }

    fn import_enable(&mut self, enable: word::Enable) -> Result<word::Enable, crate::SynthError> {
        self.import_enable_with(enable, &word::SourceSpan::default(), OperandImport::Whole)
    }

    fn import_enable_with(
        &mut self,
        enable: word::Enable,
        span: &word::SourceSpan,
        operands: OperandImport,
    ) -> Result<word::Enable, crate::SynthError> {
        Ok(word::Enable {
            value: self.import_operand(enable.value, span, operands)?,
            active_high: enable.active_high,
        })
    }

    fn import_reset_with(
        &mut self,
        reset: word::Reset,
        span: &word::SourceSpan,
        operands: OperandImport,
    ) -> Result<word::Reset, crate::SynthError> {
        Ok(word::Reset {
            kind: reset.kind,
            value: self.import_operand(reset.value, span, operands)?,
            active_high: reset.active_high,
            reset_value: self.import_operand(reset.reset_value, span, operands)?,
        })
    }
}
