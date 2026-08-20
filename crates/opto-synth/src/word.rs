// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Synthesis-specific analysis over canonical Word IR.
//!
//! This domain owns read-only graph facts shared by the frontend, planning,
//! Boolean lowering, and mapping. It does not mutate Word IR or choose an
//! implementation architecture.

pub(crate) mod bit_connectivity;
pub(crate) mod cycle;
pub(crate) mod instances;
pub(crate) mod signal_driver;
pub(crate) mod uses;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaledDynamicOffset {
    pub(crate) selector: opto_ir::word::ValueId,
    pub(crate) scale: u128,
    pub(crate) maximum_selector: u128,
}

pub(crate) fn known_u32(
    module: &opto_ir::word::WordModule,
    known_bits: &mut opto_ir::word::KnownBitsAnalysis,
    value: opto_ir::word::ValueId,
) -> Option<u32> {
    let width = module.value(value)?.ty.width();
    let mut result = 0u32;
    for index in 0..width {
        match known_bits.bit(module, value, index) {
            opto_ir::word::KnownBit::Zero => {}
            opto_ir::word::KnownBit::One if index < u32::BITS => result |= 1u32 << index,
            opto_ir::word::KnownBit::One | opto_ir::word::KnownBit::Unknown => return None,
        }
    }
    Some(result)
}

pub(crate) fn scaled_dynamic_offset(
    module: &opto_ir::word::WordModule,
    known_bits: &mut opto_ir::word::KnownBitsAnalysis,
    offset: opto_ir::word::ValueId,
) -> Option<ScaledDynamicOffset> {
    use opto_ir::word::{BinaryOp, OpKind, ValueKind};

    let stored = module.value(offset)?;
    let ValueKind::Operation(operation) = stored.kind else {
        return None;
    };
    let OpKind::Binary {
        op: BinaryOp::Mul,
        left,
        right,
    } = module.operation(operation)?.kind
    else {
        return None;
    };
    let (selector, scale) = match (
        known_u32(module, known_bits, left),
        known_u32(module, known_bits, right),
    ) {
        (Some(scale), None) if scale != 0 => (right, u128::from(scale)),
        (None, Some(scale)) if scale != 0 => (left, u128::from(scale)),
        _ => return None,
    };
    let selector_ty = module.value(selector)?.ty;
    if selector_ty.is_signed() || selector_ty.width() >= u128::BITS {
        return None;
    }
    let mut maximum_selector = 0u128;
    for index in 0..selector_ty.width() {
        if known_bits.bit(module, selector, index) != opto_ir::word::KnownBit::Zero {
            maximum_selector |= 1u128 << index;
        }
    }
    let maximum_product = maximum_selector.checked_mul(scale)?;
    if stored.ty.width() < u128::BITS && maximum_product >= (1u128 << stored.ty.width()) {
        return None;
    }
    Some(ScaledDynamicOffset {
        selector,
        scale,
        maximum_selector,
    })
}

pub(crate) type OperationInputs = smallvec::SmallVec<[opto_ir::word::ValueId; 4]>;

pub(crate) fn operation_inputs(kind: &opto_ir::word::OpKind) -> OperationInputs {
    let mut inputs = OperationInputs::new();
    kind.for_each_input(|input| inputs.push(input));
    inputs
}
