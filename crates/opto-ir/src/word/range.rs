// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Conservative unsigned bounds and alignment analysis for word-level values.
//!
//! The analysis deliberately returns unknown rather than assuming wraparound,
//! signed, cyclic, or wider-than-128-bit expressions are bounded. Dense memo
//! tables make repeated synthesis queries linear in the reachable value graph.

use super::{BinaryOp, CastKind, OpKind, UnaryOp, ValueId, ValueKind, WordModule, WordType};
use crate::BitVal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Inclusive unsigned bounds proven for a word-level value.
pub struct UnsignedValueRange {
    pub(crate) minimum: u128,
    pub(crate) maximum: u128,
}

#[derive(Debug, Clone, Copy, Default)]
enum RangeState {
    #[default]
    Uncomputed,
    Computing,
    Computed(Option<UnsignedValueRange>),
}

/// Reusable whole-module analysis for unsigned bounds and power-of-two
/// alignment. Synthesis queries many dynamic word operations in the same
/// module, so keeping one dense table avoids rebuilding an O(values) memo for
/// every query.
#[derive(Debug, Default)]
pub struct UnsignedValueAnalysis {
    ranges: Vec<RangeState>,
    alignments: Vec<Option<u32>>,
}

impl UnsignedValueAnalysis {
    #[must_use]
    /// Allocates dense memo tables sized for `module`.
    pub fn new(module: &WordModule) -> Self {
        Self {
            ranges: vec![RangeState::Uncomputed; module.values().len()],
            alignments: vec![None; module.values().len()],
        }
    }

    /// Returns conservative inclusive bounds for `value`.
    ///
    /// `None` means the value is signed, wider than 128 bits, cyclic, foreign
    /// to `module`, or otherwise cannot be bounded soundly.
    pub fn range(&mut self, module: &WordModule, value: ValueId) -> Option<UnsignedValueRange> {
        self.ensure_capacity(module);
        derive_range(module, value, &mut self.ranges)
    }

    /// Returns the largest proven power-of-two alignment exponent.
    ///
    /// A result of `n` means the value is always divisible by `2^n`.
    pub fn alignment(&mut self, module: &WordModule, value: ValueId) -> u32 {
        self.ensure_capacity(module);
        derive_alignment(module, value, &mut self.alignments)
    }

    fn ensure_capacity(&mut self, module: &WordModule) {
        let values = module.values().len();
        self.ranges.resize(values, RangeState::Uncomputed);
        self.alignments.resize(values, None);
    }
}

impl UnsignedValueRange {
    /// Returns the inclusive lower bound.
    #[must_use]
    pub fn minimum(self) -> u128 {
        self.minimum
    }

    /// Returns the inclusive upper bound.
    #[must_use]
    pub fn maximum(self) -> u128 {
        self.maximum
    }
}

/// Computes conservative unsigned bounds using a fresh analysis cache.
#[must_use]
pub fn unsigned_value_range(module: &WordModule, value: ValueId) -> Option<UnsignedValueRange> {
    UnsignedValueAnalysis::new(module).range(module, value)
}

fn derive_range(
    module: &WordModule,
    id: ValueId,
    ranges: &mut [RangeState],
) -> Option<UnsignedValueRange> {
    match ranges.get(id.index()).copied()? {
        RangeState::Computed(range) => return range,
        RangeState::Computing => return None,
        RangeState::Uncomputed => {}
    }
    ranges[id.index()] = RangeState::Computing;
    let value = module.value(id)?;
    let full = full_range(value.ty)?;
    let range = match &value.kind {
        ValueKind::Signal(_) => full,
        ValueKind::Constant(bits) => {
            let mut minimum = 0u128;
            let mut maximum = 0u128;
            for bit in 0..bits.width() {
                let weight = 1u128.checked_shl(bit)?;
                match bits.bit_lsb(bit)? {
                    BitVal::Zero => {}
                    BitVal::One => {
                        minimum |= weight;
                        maximum |= weight;
                    }
                    BitVal::X | BitVal::Z => maximum |= weight,
                }
            }
            UnsignedValueRange { minimum, maximum }
        }
        ValueKind::Operation(operation) => {
            let operation = module.operation(*operation)?;
            operation_range(module, &operation.kind, value.ty, ranges).unwrap_or(full)
        }
    };
    ranges[id.index()] = RangeState::Computed(Some(range));
    Some(range)
}

fn operation_range(
    module: &WordModule,
    operation: &OpKind,
    result_ty: WordType,
    ranges: &mut [RangeState],
) -> Option<UnsignedValueRange> {
    match operation {
        OpKind::Unary { op, .. } => match op {
            UnaryOp::LogicalNot
            | UnaryOp::ReductionAnd
            | UnaryOp::ReductionOr
            | UnaryOp::ReductionXor => Some(UnsignedValueRange {
                minimum: 0,
                maximum: 1,
            }),
            UnaryOp::BitNot => None,
        },
        OpKind::Binary { op, left, right } => {
            let left_ty = module.value(*left)?.ty;
            let right_ty = module.value(*right)?.ty;
            if left_ty.is_signed() || right_ty.is_signed() || result_ty.is_signed() {
                return None;
            }
            let left = derive_range(module, *left, ranges)?;
            let right = derive_range(module, *right, ranges)?;
            binary_range(*op, left, right, result_ty)
        }
        OpKind::Mux {
            then_value,
            else_value,
            ..
        } => {
            let then_range = derive_range(module, *then_value, ranges)?;
            let else_range = derive_range(module, *else_value, ranges)?;
            Some(UnsignedValueRange {
                minimum: then_range.minimum.min(else_range.minimum),
                maximum: then_range.maximum.max(else_range.maximum),
            })
        }
        OpKind::Cast {
            kind,
            value,
            target,
        } => {
            let source = derive_range(module, *value, ranges)?;
            let target_full = full_range(*target)?;
            match kind {
                CastKind::ZeroExtend => Some(source),
                CastKind::Truncate if source.maximum <= target_full.maximum => Some(source),
                CastKind::SignExtend => {
                    let source_ty = module.value(*value)?.ty;
                    let sign_threshold = 1u128.checked_shl(source_ty.width() - 1)?;
                    (source.maximum < sign_threshold).then_some(source)
                }
                CastKind::Truncate => None,
            }
        }
        OpKind::Concat { parts } => concat_range(module, parts, ranges),
        OpKind::Extract { .. }
        | OpKind::DynamicExtract { .. }
        | OpKind::DynamicInsert { .. }
        | OpKind::Register(_)
        | OpKind::Latch(_) => None,
    }
}

fn binary_range(
    op: BinaryOp,
    left: UnsignedValueRange,
    right: UnsignedValueRange,
    result_ty: WordType,
) -> Option<UnsignedValueRange> {
    let result_maximum = full_range(result_ty)?.maximum;
    let range = match op {
        BinaryOp::Add => UnsignedValueRange {
            minimum: left.minimum.checked_add(right.minimum)?,
            maximum: left.maximum.checked_add(right.maximum)?,
        },
        BinaryOp::Sub if left.minimum >= right.maximum => UnsignedValueRange {
            minimum: left.minimum - right.maximum,
            maximum: left.maximum - right.minimum,
        },
        BinaryOp::Mul => UnsignedValueRange {
            minimum: left.minimum.checked_mul(right.minimum)?,
            maximum: left.maximum.checked_mul(right.maximum)?,
        },
        BinaryOp::Div if right.minimum > 0 => UnsignedValueRange {
            minimum: left.minimum / right.maximum,
            maximum: left.maximum / right.minimum,
        },
        BinaryOp::Mod if right.minimum > 0 => UnsignedValueRange {
            minimum: 0,
            maximum: left.maximum.min(right.maximum.saturating_sub(1)),
        },
        BinaryOp::LogicalAnd
        | BinaryOp::LogicalOr
        | BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::Lt
        | BinaryOp::Le
        | BinaryOp::Gt
        | BinaryOp::Ge => UnsignedValueRange {
            minimum: 0,
            maximum: 1,
        },
        BinaryOp::BitAnd => UnsignedValueRange {
            minimum: 0,
            maximum: left.maximum.min(right.maximum),
        },
        BinaryOp::Sub
        | BinaryOp::Div
        | BinaryOp::Mod
        | BinaryOp::BitOr
        | BinaryOp::BitXor
        | BinaryOp::Shl
        | BinaryOp::Shr
        | BinaryOp::Ashr => return None,
    };
    (range.maximum <= result_maximum).then_some(range)
}

fn concat_range(
    module: &WordModule,
    parts: &[ValueId],
    ranges: &mut [RangeState],
) -> Option<UnsignedValueRange> {
    let mut result = UnsignedValueRange {
        minimum: 0,
        maximum: 0,
    };
    for part in parts {
        let width = module.value(*part)?.ty.width();
        let part = derive_range(module, *part, ranges)?;
        result.minimum = result
            .minimum
            .checked_shl(width)?
            .checked_add(part.minimum)?;
        result.maximum = result
            .maximum
            .checked_shl(width)?
            .checked_add(part.maximum)?;
    }
    Some(result)
}

/// Largest power-of-two alignment provable for `value`: the value is always
/// a multiple of `1 << alignment`. Conservatively 0 when nothing is known.
/// Strided selects (`index * element_width`) keep their alignment through
/// multiplies, shifts, and concatenations even before constant folding.
#[must_use]
pub fn unsigned_value_alignment(module: &WordModule, value: ValueId) -> u32 {
    UnsignedValueAnalysis::new(module).alignment(module, value)
}

fn derive_alignment(module: &WordModule, id: ValueId, alignments: &mut [Option<u32>]) -> u32 {
    if let Some(Some(alignment)) = alignments.get(id.index()).copied() {
        return alignment;
    }
    if let Some(slot) = alignments.get_mut(id.index()) {
        // Seed the memo so recursion over malformed graphs terminates.
        *slot = Some(0);
    }
    let Some(value) = module.value(id) else {
        return 0;
    };
    let width = value.ty.width();
    let alignment = match &value.kind {
        ValueKind::Signal(_) => 0,
        ValueKind::Constant(bits) => constant_alignment(bits),
        ValueKind::Operation(operation) => module.operation(*operation).map_or(0, |operation| {
            operation_alignment(module, &operation.kind, alignments)
        }),
    }
    .min(width);
    if let Some(slot) = alignments.get_mut(id.index()) {
        *slot = Some(alignment);
    }
    alignment
}

fn constant_alignment(bits: &crate::ConstBits) -> u32 {
    let mut alignment = 0;
    for bit in 0..bits.width() {
        match bits.bit_lsb(bit) {
            Some(BitVal::Zero) => alignment += 1,
            _ => break,
        }
    }
    alignment
}

fn operation_alignment(
    module: &WordModule,
    operation: &OpKind,
    alignments: &mut [Option<u32>],
) -> u32 {
    match operation {
        OpKind::Binary { op, left, right } => {
            let left_alignment = derive_alignment(module, *left, alignments);
            let right_alignment = derive_alignment(module, *right, alignments);
            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::BitOr | BinaryOp::BitXor => {
                    left_alignment.min(right_alignment)
                }
                BinaryOp::BitAnd => left_alignment.max(right_alignment),
                BinaryOp::Mul => left_alignment.saturating_add(right_alignment),
                BinaryOp::Shl => left_alignment,
                _ => 0,
            }
        }
        OpKind::Mux {
            then_value,
            else_value,
            ..
        } => derive_alignment(module, *then_value, alignments).min(derive_alignment(
            module,
            *else_value,
            alignments,
        )),
        OpKind::Cast { value, .. } | OpKind::Extract { value, lsb: 0, .. } => {
            derive_alignment(module, *value, alignments)
        }
        OpKind::Concat { parts } => {
            let mut alignment = 0u32;
            for &part in parts.iter().rev() {
                let part_width = module.value(part).map_or(0, |value| value.ty.width());
                let part_alignment = derive_alignment(module, part, alignments);
                if part_alignment >= part_width {
                    alignment += part_width;
                } else {
                    alignment += part_alignment;
                    break;
                }
            }
            alignment
        }
        _ => 0,
    }
}

fn full_range(ty: WordType) -> Option<UnsignedValueRange> {
    let maximum = match ty.width() {
        128 => u128::MAX,
        width if width < 128 => 1u128.checked_shl(width)?.checked_sub(1)?,
        _ => return None,
    };
    Some(UnsignedValueRange {
        minimum: 0,
        maximum,
    })
}
