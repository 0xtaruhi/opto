// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BTreeMap, BTreeSet, BitVal, ConstBits, FrameId, MaterializedPredicate, NonZeroU32,
    PendingWrite, Predicate, StateArena, TargetKey, derived_source, events, proc, word,
};

pub(super) fn block_effects(
    procedures: &proc::ProcModule,
    block: proc::BlockId,
) -> Result<impl Iterator<Item = (proc::EffectId, &proc::Effect)>, crate::SynthError> {
    procedures
        .block_effects(block)
        .ok_or_else(|| crate::SynthError::invariant("block is not in the procedure"))
}

fn value_type(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<word::WordType, crate::SynthError> {
    Ok(module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("value is not in the module arena"))?
        .ty)
}

fn signal_type(
    module: &word::WordModule,
    signal: word::SignalId,
) -> Result<word::WordType, crate::SynthError> {
    Ok(module
        .signal(signal)
        .ok_or_else(|| crate::SynthError::invariant("signal is not in the module arena"))?
        .ty)
}

fn constant_bits(bits: Vec<opto_ir::BitVal>) -> Result<ConstBits, crate::SynthError> {
    ConstBits::from_bits(bits).map_err(|error| crate::SynthError::capacity(error.to_string()))
}

pub(super) fn target_layout(
    module: &word::WordModule,
    procedures: &proc::ProcModule,
    blocks: &[proc::BlockId],
) -> Result<BTreeMap<word::SignalId, Vec<TargetKey>>, crate::SynthError> {
    let mut boundaries = BTreeMap::<word::SignalId, BTreeSet<u32>>::new();
    for &block in blocks {
        for (_, effect) in block_effects(procedures, block)? {
            let proc::ProcTarget::Signal { signal, select } = effect.target else {
                continue;
            };
            let width = module
                .signal(signal)
                .ok_or_else(|| crate::SynthError::invariant("unknown procedural target signal"))?
                .ty
                .width();
            let points = boundaries.entry(signal).or_default();
            points.extend([0, width]);
            if let proc::TargetSelect::Static(range) = select {
                let lsb = range.lsb.min(range.msb);
                points.extend([lsb, lsb + range.width()]);
            }
        }
    }
    boundaries
        .into_iter()
        .map(|(signal, points)| {
            let points = points.into_iter().collect::<Vec<_>>();
            let keys = points
                .windows(2)
                .map(|point| TargetKey {
                    signal,
                    lsb: point[0],
                    width: point[1] - point[0],
                })
                .collect();
            Ok((signal, keys))
        })
        .collect()
}

pub(super) struct SignalResolutionContext<'a> {
    pub(super) states: &'a StateArena,
    pub(super) layout: &'a BTreeMap<word::SignalId, Vec<TargetKey>>,
    pub(super) bases: &'a BTreeMap<TargetKey, word::ValueId>,
    pub(super) reads: &'a BTreeMap<word::SignalId, usize>,
    pub(super) writes: &'a [PendingWrite],
}

pub(super) fn resolve_signal(
    module: &mut word::WordModule,
    context: &SignalResolutionContext<'_>,
    frame: FrameId,
    original: word::ValueId,
    reference: word::SignalRef,
) -> Result<Option<word::ValueId>, crate::SynthError> {
    let original_source = module
        .value(original)
        .ok_or_else(|| crate::SynthError::invariant("procedural read value disappeared"))?
        .source
        .clone();
    let stateful = context.layout.contains_key(&reference.signal);
    let forwarded = context
        .reads
        .get(&reference.signal)
        .and_then(|&index| module.memory_read_ports().get(index))
        .is_some_and(|read| {
            matches!(read.timing, word::MemoryReadTiming::Asynchronous)
                && context
                    .writes
                    .iter()
                    .any(|write| write.blocking && write.memory == read.memory)
        });
    if !stateful && !forwarded {
        return Ok(None);
    }
    if stateful
        && module
            .signal(reference.signal)
            .is_some_and(|signal| signal.kind == word::SignalKind::ProcessLocal)
    {
        let keys = context.layout.get(&reference.signal).ok_or_else(|| {
            crate::SynthError::invariant("stateful process-local signal has no target layout")
        })?;
        if keys
            .iter()
            .filter(|key| {
                key.lsb < reference.lsb + reference.width() && key.lsb + key.width > reference.lsb
            })
            .any(|&key| {
                context
                    .states
                    .get(frame, key)
                    .is_none_or(|slot| slot.coverage != Predicate::Always)
            })
        {
            return Err(crate::SynthError::invalid(
                "process-local value is read before assignment on every incoming path",
            ));
        }
    }
    let mut value = if let Some(keys) = context.layout.get(&reference.signal) {
        let end = reference.lsb + reference.width();
        let mut parts = Vec::new();
        for &key in keys
            .iter()
            .rev()
            .filter(|key| key.lsb < end && key.lsb + key.width > reference.lsb)
        {
            let low = key.lsb.max(reference.lsb);
            let high = (key.lsb + key.width).min(end);
            let slot = match context.states.get(frame, key) {
                Some(slot) => slot.current,
                None => context.bases.get(&key).copied().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "procedural target has no entry-state value during signal resolution",
                    )
                })?,
            };
            let part = if low == key.lsb && high - low == key.width {
                slot
            } else {
                let mut role = [0u8; 8];
                role[..4].copy_from_slice(&(low - key.lsb).to_le_bytes());
                role[4..].copy_from_slice(&(high - low).to_le_bytes());
                module
                    .extract(
                        slot,
                        low - key.lsb,
                        high - low,
                        derived_source(&original_source, "procedural partial read", role)?,
                    )
                    .map_err(crate::SynthError::from)?
            };
            parts.push(part);
        }
        match parts.as_slice() {
            [] => original,
            [part] => *part,
            _ => module
                .concat(
                    parts,
                    derived_source(&original_source, "procedural partial read", b"concat")?,
                )
                .map_err(crate::SynthError::from)?,
        }
    } else {
        original
    };
    if forwarded {
        let (memory, address, source) = {
            let read = &module.memory_read_ports()[context.reads[&reference.signal]];
            (read.memory, read.address, read.source.clone())
        };
        let full = if reference.lsb == 0
            && reference.width() == signal_type(module, reference.signal)?.width()
        {
            value
        } else {
            module
                .read_signal(
                    reference.signal,
                    derived_source(&source, "forwarded memory read", b"full-signal")?,
                )
                .map_err(crate::SynthError::from)?
        };
        let forwarded = forward_memory_read(module, memory, address, full, context.writes)?;
        value = if reference.lsb == 0 && reference.width() == value_type(module, forwarded)?.width()
        {
            forwarded
        } else {
            module
                .extract(
                    forwarded,
                    reference.lsb,
                    reference.width(),
                    derived_source(&source, "forwarded memory read", b"slice")?,
                )
                .map_err(crate::SynthError::from)?
        };
    }
    let ty = value_type(module, original)?;
    if value_type(module, value)? == ty {
        Ok(Some(value))
    } else {
        module
            .cast(
                word::CastKind::ZeroExtend,
                value,
                ty,
                derived_source(&original_source, "procedural read", b"coerce")?,
            )
            .map(Some)
            .map_err(crate::SynthError::from)
    }
}

pub(super) fn forward_memory_read(
    module: &mut word::WordModule,
    memory: word::MemoryId,
    read_address: word::ValueId,
    mut value: word::ValueId,
    writes: &[PendingWrite],
) -> Result<word::ValueId, crate::SynthError> {
    for write in writes
        .iter()
        .filter(|write| write.blocking && write.memory == memory)
    {
        let address = module
            .binary(
                word::BinaryOp::Eq,
                read_address,
                write.address,
                write.source.clone(),
            )
            .map_err(crate::SynthError::from)?;
        let condition = match write.guard {
            MaterializedPredicate::Never => continue,
            MaterializedPredicate::Always => address,
            MaterializedPredicate::Value(guard) => module
                .binary(
                    word::BinaryOp::LogicalAnd,
                    guard,
                    address,
                    write.source.clone(),
                )
                .map_err(crate::SynthError::from)?,
        };
        let data = apply_mask(module, value, write.data, write.mask, &write.source)?;
        value = module
            .mux(condition, data, value, write.source.clone())
            .map_err(crate::SynthError::from)?;
    }
    Ok(value)
}

pub(super) fn apply_mask(
    module: &mut word::WordModule,
    old: word::ValueId,
    data: word::ValueId,
    mask: Option<word::MemoryWriteMask>,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let Some(mask) = mask else {
        return Ok(data);
    };
    let width = value_type(module, data)?.width();
    let mut bits = Vec::with_capacity(width as usize);
    for bit in (0..width).rev() {
        let mask_bit = bit / mask.granularity.get();
        let select = module
            .extract(mask.value, mask_bit, 1, source.clone())
            .map_err(crate::SynthError::from)?;
        let select = if mask.active_high {
            select
        } else {
            module
                .unary(word::UnaryOp::LogicalNot, select, source.clone())
                .map_err(crate::SynthError::from)?
        };
        let new_bit = module
            .extract(data, bit, 1, source.clone())
            .map_err(crate::SynthError::from)?;
        let old_bit = module
            .extract(old, bit, 1, source.clone())
            .map_err(crate::SynthError::from)?;
        bits.push(
            module
                .mux(select, new_bit, old_bit, source.clone())
                .map_err(crate::SynthError::from)?,
        );
    }
    let value = module
        .concat(bits, source.clone())
        .map_err(crate::SynthError::from)?;
    cast_like(module, value, data, source)
}

pub(super) fn memory_write_data(
    module: &mut word::WordModule,
    memory: word::MemoryId,
    select: proc::TargetSelect,
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<(word::ValueId, Option<word::MemoryWriteMask>), crate::SynthError> {
    let ty = module
        .memory(memory)
        .ok_or_else(|| crate::SynthError::invariant("unknown procedural memory"))?
        .element_type;
    if select == proc::TargetSelect::Whole {
        return Ok((value, None));
    }
    let zero = constant(module, ty, BitVal::Zero, source.clone())?;
    let mask_ty =
        word::WordType::new(ty.width(), false, ty.state()).map_err(crate::SynthError::from)?;
    let mask_zero = constant(module, mask_ty, BitVal::Zero, source.clone())?;
    let (data, mask) = match select {
        proc::TargetSelect::Whole => unreachable!(),
        proc::TargetSelect::Static(range) => {
            if range.msb < range.lsb {
                return Err(crate::SynthError::unsupported(
                    "ascending procedural memory part-select targets",
                ));
            }
            let data = static_insert(module, zero, range.lsb, value, source)?;
            let bits = (0..ty.width())
                .rev()
                .map(|bit| {
                    if (range.lsb..range.lsb + range.width()).contains(&bit) {
                        BitVal::One
                    } else {
                        BitVal::Zero
                    }
                })
                .collect();
            let mask = module
                .constant(constant_bits(bits)?, mask_ty, source.clone())
                .map_err(crate::SynthError::from)?;
            (data, mask)
        }
        proc::TargetSelect::Dynamic { offset, width } => {
            let data = module
                .dynamic_insert(zero, offset, value, source.clone())
                .map_err(crate::SynthError::from)?;
            let ones_ty = word::WordType::new(width.get(), false, ty.state())
                .map_err(crate::SynthError::from)?;
            let ones = constant(module, ones_ty, BitVal::One, source.clone())?;
            let mask = module
                .dynamic_insert(mask_zero, offset, ones, source.clone())
                .map_err(crate::SynthError::from)?;
            (data, mask)
        }
    };
    Ok((
        data,
        Some(word::MemoryWriteMask {
            value: mask,
            granularity: NonZeroU32::MIN,
            active_high: true,
        }),
    ))
}

pub(super) fn static_insert(
    module: &mut word::WordModule,
    base: word::ValueId,
    lsb: u32,
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let width = value_type(module, base)?.width();
    let value_width = value_type(module, value)?.width();
    let mut parts = Vec::with_capacity(3);
    if lsb + value_width < width {
        parts.push(
            module
                .extract(
                    base,
                    lsb + value_width,
                    width - lsb - value_width,
                    source.clone(),
                )
                .map_err(crate::SynthError::from)?,
        );
    }
    parts.push(value);
    if lsb > 0 {
        parts.push(
            module
                .extract(base, 0, lsb, source.clone())
                .map_err(crate::SynthError::from)?,
        );
    }
    let result = match parts.as_slice() {
        [value] => *value,
        _ => module
            .concat(parts, source.clone())
            .map_err(crate::SynthError::from)?,
    };
    let ty = value_type(module, base)?;
    if value_type(module, result)? == ty {
        Ok(result)
    } else {
        module
            .cast(word::CastKind::ZeroExtend, result, ty, source.clone())
            .map_err(crate::SynthError::from)
    }
}

pub(super) fn extract_assignment(
    module: &mut word::WordModule,
    value: word::ValueId,
    lsb: u32,
    width: u32,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let value_width = value_type(module, value)?.width();
    if lsb == 0 && width == value_width {
        Ok(value)
    } else {
        module
            .extract(value, lsb, width, source.clone())
            .map_err(crate::SynthError::from)
    }
}

pub(super) fn cast_like(
    module: &mut word::WordModule,
    value: word::ValueId,
    template: word::ValueId,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let target = value_type(module, template)?;
    if value_type(module, value)? == target {
        Ok(value)
    } else {
        module
            .cast(word::CastKind::ZeroExtend, value, target, source.clone())
            .map_err(crate::SynthError::from)
    }
}

pub(super) fn predicate_enable(predicate: MaterializedPredicate) -> Option<word::Enable> {
    match predicate {
        MaterializedPredicate::Never | MaterializedPredicate::Always => None,
        MaterializedPredicate::Value(value) => Some(word::Enable {
            value,
            active_high: true,
        }),
    }
}

pub(super) fn inferred_reset_kind(procedure: &proc::Procedure) -> Option<word::ResetKind> {
    match procedure.kind {
        proc::ProcedureKind::Latch => Some(word::ResetKind::Async),
        proc::ProcedureKind::FlipFlop => Some(match procedure.sensitivity {
            proc::Sensitivity::Edges(events) if events.len() > 1 => word::ResetKind::Async,
            _ => word::ResetKind::Sync,
        }),
        proc::ProcedureKind::Combinational | proc::ProcedureKind::CombinationalOrLatch => None,
    }
}

pub(super) fn constant_value(module: &word::WordModule, value: word::ValueId) -> bool {
    let Some(value) = module.value(value) else {
        return false;
    };
    match &value.kind {
        word::ValueKind::Constant(_) => true,
        word::ValueKind::Signal(_) => false,
        word::ValueKind::Operation(operation) => {
            module
                .operation(*operation)
                .is_some_and(|operation| match &operation.kind {
                    word::OpKind::Concat { parts } => {
                        parts.iter().all(|&part| constant_value(module, part))
                    }
                    word::OpKind::Cast { value, .. } | word::OpKind::Extract { value, .. } => {
                        constant_value(module, *value)
                    }
                    _ => false,
                })
        }
    }
}

pub(super) fn normalized_enable(
    module: &mut word::WordModule,
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<word::Enable, crate::SynthError> {
    if let Some((signal, active_high)) = events::normalize_boolean_value(module, value, true) {
        let value = module
            .read_signal(signal, source.clone())
            .map_err(crate::SynthError::from)?;
        Ok(word::Enable { value, active_high })
    } else {
        Ok(word::Enable {
            value,
            active_high: true,
        })
    }
}

pub(super) fn whole_target_name(
    module: &word::WordModule,
    target: &word::LValue,
) -> Option<opto_ir::NameId> {
    (target.range.is_none() && target.dynamic.is_none())
        .then(|| module.signal(target.signal).and_then(|signal| signal.name))
        .flatten()
}

pub(super) fn constant(
    module: &mut word::WordModule,
    ty: word::WordType,
    bit: BitVal,
    source: word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let len = usize::try_from(ty.width())
        .map_err(|_| crate::SynthError::capacity("constant width exceeds address capacity"))?;
    module
        .constant(constant_bits(vec![bit; len])?, ty, source)
        .map_err(crate::SynthError::from)
}
