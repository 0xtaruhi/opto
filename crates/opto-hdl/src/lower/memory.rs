// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    clippy::wildcard_imports,
    reason = "this private memory lowering stage consumes the parent's typed lowering prelude and \
              does not define a reusable public module boundary"
)]

//! Lowering of flattened source memory selections into typed Word memory ports.
//!
//! Source offsets are measured in flattened bits, while Word memory addresses
//! select elements. Static and dynamic spans must therefore prove whole-element
//! alignment before division; partial selections remain within one element.
//! Concatenation iterates high addresses first so the lowest selected element
//! remains in the least-significant result bits.

use super::*;

/// Converts a constant flattened bit range into an element address and select.
pub(super) fn static_memory_select(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    range: SlangBitRange,
    source: SourceSpan,
) -> Result<MemorySelection, HdlError> {
    let memory = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
    let element_width = memory.element_type.width();
    let depth = memory.depth;
    let lsb = range.lsb.min(range.msb);
    let width = range.msb.abs_diff(range.lsb) + 1;
    let address = lsb / element_width;
    let element_lsb = lsb % element_width;
    let end = lsb
        .checked_add(width)
        .ok_or_else(|| HdlError::invalid("verilog frontend: memory selection range overflows"))?;
    if address >= depth.get()
        || end
            > element_width.checked_mul(depth.get()).ok_or_else(|| {
                HdlError::invalid(
                    "verilog frontend: flattened memory width exceeds 32-bit capacity",
                )
            })?
    {
        return Err(HdlError::unsupported(
            "verilog frontend: memory selection is outside storage bounds",
        ));
    }
    let address = memory_address_constant(module, address, depth, source)?;
    if width > element_width {
        if element_lsb != 0 || !width.is_multiple_of(element_width) {
            return Err(HdlError::unsupported(
                "verilog frontend: a static memory span must contain whole elements",
            ));
        }
        return Ok(MemorySelection::Span {
            address,
            elements: NonZeroU32::new(width / element_width)
                .expect("nonzero width divided by smaller width is nonzero"),
        });
    }
    if element_lsb
        .checked_add(width)
        .is_none_or(|selection_end| selection_end > element_width)
    {
        return Err(HdlError::unsupported(
            "verilog frontend: a static memory selection cannot cross an element boundary",
        ));
    }
    let select = if element_lsb == 0 && width == element_width {
        TargetSelect::Whole
    } else {
        TargetSelect::Static(BitRange {
            msb: element_lsb + width - 1,
            lsb: element_lsb,
        })
    };
    Ok(MemorySelection::Element { address, select })
}

pub(super) enum MemorySelection {
    Element {
        address: ValueId,
        select: TargetSelect,
    },
    Span {
        address: ValueId,
        elements: NonZeroU32,
    },
}

/// Splits an unsigned flattened offset into element address and intra-element offset.
pub(super) fn dynamic_memory_select(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    offset: ValueId,
    width: u32,
    source: SourceSpan,
) -> Result<MemorySelection, HdlError> {
    let element_width = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
        .element_type
        .width();
    let width = NonZeroU32::new(width).ok_or_else(|| {
        HdlError::invalid("verilog frontend: dynamic memory selection width must be non-zero")
    })?;
    let offset_type = module
        .value(offset)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory offset"))?
        .ty;
    if offset_type.is_signed() {
        return Err(HdlError::invalid(
            "verilog frontend: memory offsets must be unsigned",
        ));
    }
    if element_width == 1 {
        return Ok(if width.get() == 1 {
            MemorySelection::Element {
                address: offset,
                select: TargetSelect::Whole,
            }
        } else {
            MemorySelection::Span {
                address: offset,
                elements: width,
            }
        });
    }
    let scale = unsigned_constant(module, element_width, offset_type, source.clone())?;
    let address = if let Some(address) = scaled_memory_address(module, offset, element_width) {
        address
    } else {
        module
            .binary(BinaryOp::Div, offset, scale, source.clone())
            .map_err(HdlError::Ir)?
    };
    if width.get() > element_width {
        let elements = NonZeroU32::new(width.get() / element_width).ok_or_else(|| {
            HdlError::invalid("verilog frontend: dynamic memory span must be non-zero")
        })?;
        if !width.get().is_multiple_of(element_width) {
            return Err(HdlError::unsupported(
                "verilog frontend: a dynamic memory span must contain whole elements",
            ));
        }
        return Ok(MemorySelection::Span { address, elements });
    }
    let select = if width.get() == element_width {
        TargetSelect::Whole
    } else {
        TargetSelect::Dynamic {
            offset: module
                .binary(BinaryOp::Mod, offset, scale, source)
                .map_err(HdlError::Ir)?,
            width,
        }
    };
    Ok(MemorySelection::Element { address, select })
}

fn scaled_memory_address(
    module: &ModuleLowerer,
    offset: ValueId,
    element_width: u32,
) -> Option<ValueId> {
    let operation = match module.value(offset)?.kind {
        opto_ir::word::ValueKind::Operation(operation) => module.operation(operation)?,
        opto_ir::word::ValueKind::Signal(_) | opto_ir::word::ValueKind::Constant(_) => return None,
    };
    let opto_ir::word::OpKind::Binary {
        op: BinaryOp::Mul,
        left,
        right,
    } = &operation.kind
    else {
        return None;
    };
    if unsigned_constant_value(module, *right) == Some(element_width) {
        Some(*left)
    } else if unsigned_constant_value(module, *left) == Some(element_width) {
        Some(*right)
    } else {
        None
    }
}

fn unsigned_constant_value(module: &ModuleLowerer, value: ValueId) -> Option<u32> {
    let opto_ir::word::ValueKind::Constant(bits) = &module.value(value)?.kind else {
        return None;
    };
    bits.as_slice().iter().try_fold(0u32, |value, bit| {
        let bit = match bit {
            BitVal::Zero => 0,
            BitVal::One => 1,
            BitVal::X | BitVal::Z => return None,
        };
        value.checked_mul(2)?.checked_add(bit)
    })
}

pub(super) fn memory_address_offset(
    module: &mut ModuleLowerer,
    address: ValueId,
    offset: u32,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    if offset == 0 {
        return Ok(address);
    }
    let ty = module
        .value(address)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory address"))?
        .ty;
    let offset = unsigned_constant(module, offset, ty, source.clone())?;
    module
        .binary(BinaryOp::Add, address, offset, source)
        .map_err(HdlError::Ir)
}

pub(super) fn memory_address_constant(
    module: &mut ModuleLowerer,
    address: u32,
    depth: NonZeroU32,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    let width = (u32::BITS - (depth.get() - 1).leading_zeros()).max(1);
    let ty = WordType::new(width, false, LogicStateKind::FourState).map_err(HdlError::Ir)?;
    unsigned_constant(module, address, ty, source)
}

fn unsigned_constant(
    module: &mut ModuleLowerer,
    value: u32,
    ty: WordType,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    if ty.is_signed() || value.checked_shr(ty.width()).unwrap_or(0) != 0 {
        return Err(HdlError::invalid(
            "verilog frontend: memory offset constant exceeds its unsigned type",
        ));
    }
    let bits = (0..ty.width())
        .rev()
        .map(|bit| {
            if value.checked_shr(bit).unwrap_or(0) & 1 == 0 {
                BitVal::Zero
            } else {
                BitVal::One
            }
        })
        .collect();
    module
        .constant(
            ConstBits::from_bits(bits).map_err(HdlError::Constant)?,
            ty,
            source,
        )
        .map_err(HdlError::Ir)
}

/// Reads and concatenates every element in source flattening order.
pub(super) fn read_whole_memory(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    let depth = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?
        .depth;
    let mut elements = Vec::with_capacity(depth.get() as usize);
    for address in (0..depth.get()).rev() {
        let address = memory_address_constant(module, address, depth, source.clone())?;
        elements.push(read_memory(
            module,
            memory,
            address,
            TargetSelect::Whole,
            source.clone(),
        )?);
    }
    module.concat(elements, source).map_err(HdlError::Ir)
}

/// Reads a consecutive whole-element span from a dynamic base address.
pub(super) fn read_memory_span(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    address: ValueId,
    elements: NonZeroU32,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    let mut values = Vec::with_capacity(elements.get() as usize);
    for offset in (0..elements.get()).rev() {
        let address = memory_address_offset(module, address, offset, source.clone())?;
        values.push(read_memory(
            module,
            memory,
            address,
            TargetSelect::Whole,
            source.clone(),
        )?);
    }
    module.concat(values, source).map_err(HdlError::Ir)
}

pub(super) fn read_memory(
    module: &mut ModuleLowerer,
    memory: MemoryId,
    address: ValueId,
    select: TargetSelect,
    source: SourceSpan,
) -> Result<ValueId, HdlError> {
    let memory_definition = module
        .memory(memory)
        .ok_or_else(|| HdlError::invalid("verilog frontend: unknown memory"))?;
    let element_type = memory_definition.element_type;
    let base = module.name_str(memory_definition.name).to_string();
    let mut ordinal = module.memory_read_ports().len();
    let name = loop {
        let candidate = format!("{base}$read${ordinal}");
        if module.signal_id(&candidate).is_none() && module.memory_id(&candidate).is_none() {
            break candidate;
        }
        ordinal = ordinal.checked_add(1).ok_or_else(|| {
            HdlError::invalid("verilog frontend: memory read port name space is exhausted")
        })?;
    };
    let data = module
        .add_wire(name, element_type, source.clone())
        .map_err(HdlError::Ir)?;
    module
        .add_memory_read_port(MemoryReadPort {
            memory,
            address,
            data,
            timing: MemoryReadTiming::Asynchronous,
            read_during_write: ReadDuringWrite::OldData,
            source: source.clone(),
        })
        .map_err(HdlError::Ir)?;
    let value = module
        .read_signal(data, source.clone())
        .map_err(HdlError::Ir)?;
    match select {
        TargetSelect::Whole => Ok(value),
        TargetSelect::Static(range) => module
            .extract(value, range.lsb.min(range.msb), range.width(), source)
            .map_err(HdlError::Ir),
        TargetSelect::Dynamic { offset, width } => module
            .dynamic_extract(value, offset, width.get(), source)
            .map_err(HdlError::Ir),
    }
}
