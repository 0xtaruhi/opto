// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Semantic operators recognized before structural lowering.
//!
//! Recognition records source-level intent separately from the concrete
//! implementation selected later. This lets reports and provenance refer to a
//! stable operator even when mapping replaces its cells.

use opto_ir::word;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Dense identifier for a recognized semantic operator.
pub struct OperatorId(u32);

impl OperatorId {
    /// Return the zero-based arena index.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }

    pub(crate) fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Source-level operation classes with selectable structural implementations.
pub enum OperatorKind {
    /// Binary addition.
    Add,
    /// Fused arithmetic region with additive and product terms.
    Sum,
    /// Binary subtraction.
    Subtract,
    /// Addition of one.
    Increment,
    /// Subtraction of one.
    Decrement,
    /// Binary multiplication.
    Multiply,
    /// Integer division with truncation toward zero.
    Divide,
    /// Integer remainder with the dividend's sign.
    Modulo,
    /// Variable-offset bit extraction.
    DynamicExtract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArithmeticTerm {
    Value {
        value: word::ValueId,
        ty: word::WordType,
        negative: bool,
    },
    Product {
        inputs: [word::ValueId; 2],
        input_types: [word::WordType; 2],
        ty: word::WordType,
        negative: bool,
        constant_input: Option<u8>,
    },
}

impl ArithmeticTerm {
    pub(crate) fn is_negative(self) -> bool {
        match self {
            Self::Value { negative, .. } | Self::Product { negative, .. } => negative,
        }
    }

    pub(crate) fn is_product(self) -> bool {
        matches!(self, Self::Product { .. })
    }

    pub(crate) fn has_variable_product(self) -> bool {
        matches!(
            self,
            Self::Product {
                constant_input: None,
                ..
            }
        )
    }

    pub(crate) fn inputs(self) -> impl Iterator<Item = word::ValueId> {
        let values = match self {
            Self::Value { value, .. } => [Some(value), None],
            Self::Product { inputs, .. } => inputs.map(Some),
        };
        values.into_iter().flatten()
    }
}

pub(crate) const ONE_HOT_EXTRACT_MAX_OFFSET: u128 = 4096;
const ONE_HOT_EXTRACT_MIN_TAPS: u32 = 4;
const ONE_HOT_EXTRACT_MAX_TAPS: u32 = 64;
const ONE_HOT_EXTRACT_MIN_WIDTH: u32 = 4;
const ONE_HOT_EXTRACT_SPARSITY: u128 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DynamicExtractOperator {
    maximum_offset: u128,
    selection_max: u128,
    alignment: u32,
    offset_width: u32,
    tap_count: u32,
}

impl DynamicExtractOperator {
    pub(crate) fn new(
        maximum_offset: u128,
        available_offsets: u32,
        alignment: u32,
        offset_width: u32,
    ) -> Self {
        let selection_max = maximum_offset.min(u128::from(available_offsets));
        // Alignment removes low offset bits. Derive the number of reachable
        // selection points without constructing their potentially huge range;
        // saturation deliberately makes oversized cases ineligible for one-hot
        // lowering below.
        let stride = 1u128.checked_shl(alignment.min(127)).unwrap_or(u128::MAX);
        let tap_count = selection_max
            .checked_div(stride)
            .and_then(|count| count.checked_add(1))
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(u32::MAX);
        Self {
            maximum_offset,
            selection_max,
            alignment,
            offset_width,
            tap_count,
        }
    }

    pub(crate) fn supports_one_hot(self, result_width: u32) -> bool {
        // One-hot muxing is useful only for a sparse, bounded selector. The
        // bounds prevent structural expansion from scaling with an unconstrained
        // offset while retaining the shapes where barrel stages are wasteful.
        result_width >= ONE_HOT_EXTRACT_MIN_WIDTH
            && self.selection_max < ONE_HOT_EXTRACT_MAX_OFFSET
            && (ONE_HOT_EXTRACT_MIN_TAPS..=ONE_HOT_EXTRACT_MAX_TAPS).contains(&self.tap_count)
            && u128::from(self.tap_count).saturating_mul(ONE_HOT_EXTRACT_SPARSITY)
                <= self.selection_max + 1
    }

    pub(crate) fn selection_max(self) -> u128 {
        self.selection_max
    }

    pub(crate) fn maximum_offset(self) -> u128 {
        self.maximum_offset
    }

    pub(crate) fn alignment(self) -> u32 {
        self.alignment
    }

    pub(crate) fn offset_width(self) -> u32 {
        self.offset_width
    }

    pub(crate) fn tap_count(self) -> u32 {
        self.tap_count
    }

    pub(crate) fn barrel_stages(self) -> u32 {
        if self.selection_max == 0 {
            return 0;
        }
        let selectable_bits = u128::BITS - self.selection_max.leading_zeros();
        selectable_bits
            .min(self.offset_width)
            .saturating_sub(self.alignment.min(self.offset_width))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Recognized operation together with its source identity and lowering shape.
///
/// `semantic_width` describes the language-level result. `width` may be wider
/// when the selected implementation needs guard or carry bits.
pub struct SemanticOperator {
    pub(crate) id: OperatorId,
    pub(crate) kind: OperatorKind,
    pub(crate) source_operation: word::OpId,
    pub(crate) inputs: [word::ValueId; 2],
    pub(crate) input_types: [word::WordType; 2],
    pub(crate) result: word::ValueId,
    pub(crate) constant_input: Option<u8>,
    pub(crate) term_count: u32,
    pub(crate) negative_term_count: u32,
    pub(crate) product_term_count: u32,
    pub(crate) variable_product_term_count: u32,
    pub(crate) semantic_width: u32,
    pub(crate) implementation_width: u32,
    pub(crate) signed: bool,
    pub(crate) dynamic_extract: Option<DynamicExtractOperator>,
}

impl SemanticOperator {
    /// Return the stable identifier assigned by the operator catalog.
    #[must_use]
    pub fn id(self) -> OperatorId {
        self.id
    }

    /// Return the recognized source-level operation class.
    #[must_use]
    pub fn kind(self) -> OperatorKind {
        self.kind
    }

    /// Return the primary Word IR operation represented by this operator.
    #[must_use]
    pub fn source_operation(self) -> word::OpId {
        self.source_operation
    }

    /// Return the two primary source operands.
    #[must_use]
    pub fn inputs(self) -> [word::ValueId; 2] {
        self.inputs
    }

    /// Return the types of the two primary source operands.
    #[must_use]
    pub fn input_types(self) -> [word::WordType; 2] {
        self.input_types
    }

    /// Return the source value produced by the operator.
    #[must_use]
    pub fn result(self) -> word::ValueId {
        self.result
    }

    /// Return the width required by structural implementation.
    #[must_use]
    pub fn width(self) -> u32 {
        self.implementation_width
    }

    /// Return the language-level result width.
    #[must_use]
    pub fn semantic_width(self) -> u32 {
        self.semantic_width
    }

    /// Return the number of terms in a fused arithmetic region.
    #[must_use]
    pub fn term_count(self) -> u32 {
        self.term_count
    }

    pub(crate) fn negative_term_count(self) -> u32 {
        self.negative_term_count
    }

    pub(crate) fn product_term_count(self) -> u32 {
        self.product_term_count
    }

    pub(crate) fn variable_product_term_count(self) -> u32 {
        self.variable_product_term_count
    }

    /// Report whether operand extension and comparison use signed semantics.
    #[must_use]
    pub fn is_signed(self) -> bool {
        self.signed
    }

    pub(crate) fn constant_input(self) -> Option<usize> {
        self.constant_input.map(usize::from)
    }

    pub(crate) fn dynamic_extract(self) -> Option<DynamicExtractOperator> {
        self.dynamic_extract
    }
}
