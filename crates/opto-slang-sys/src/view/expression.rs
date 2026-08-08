// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::convert::{
    has_flag, map_binary_op, map_cast_kind, map_unary_op, nonzero, optional_str, required_str,
    unknown_enum,
};
use crate::bridge::{pointer_element, read, read_invariant};
use crate::ffi;
use crate::{SlangBinaryOp, SlangBitRange, SlangCastKind, SlangError, SlangUnaryOp};
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::NonNull;

#[derive(Debug, Clone, Copy)]
/// Borrowed handle to one elaborated expression node.
pub struct SlangExpression<'a> {
    raw: NonNull<ffi::Expr>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangExpression<'a> {
    pub(super) fn from_raw(raw: *const ffi::Expr, context: &str) -> Result<Self, SlangError> {
        let raw = NonNull::new(raw.cast_mut()).ok_or_else(|| {
            SlangError::BridgeInvariant(format!("native slang bridge returned null {context}"))
        })?;
        Ok(Self {
            raw,
            _lifetime: PhantomData,
        })
    }

    fn view(self) -> Result<ffi::ExprView, SlangError> {
        // SAFETY: `raw` comes from the live snapshot and the bridge initializes the view on success.
        unsafe {
            read("expression", |view| {
                ffi::opto_slang_expr_view(self.raw.as_ptr(), view)
            })
        }
    }

    /// Returns the expression's source location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native expression or its
    /// source-file view is null or malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        let view = self.view()?;
        SlangSourceSpan::from_raw(view.source_file, view.source_line, view.source_column)
    }

    fn signal_ref_from_view(view: ffi::ExprView) -> Result<SlangSignalRef<'a>, SlangError> {
        if view.kind != ffi::EXPR_SIGNAL {
            return Err(SlangError::BridgeInvariant(
                "native slang bridge returned a non-signal where a signal was required".to_string(),
            ));
        }
        // SAFETY: a signal expression view owns a required snapshot-backed name string.
        let name = unsafe { required_str(view.signal_name, "signal name")? };
        let range = has_flag(view.signal_has_range).then_some(SlangBitRange {
            msb: view.signal_msb,
            lsb: view.signal_lsb,
        });
        Ok(SlangSignalRef { name, range })
    }

    /// Decodes the expression operator and its borrowed operands.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native operator,
    /// missing required operand/string, or malformed nested expression view.
    pub fn kind(self) -> Result<SlangExpressionKind<'a>, SlangError> {
        let view = self.view()?;
        match view.kind {
            ffi::EXPR_SIGNAL => Ok(SlangExpressionKind::Signal(Self::signal_ref_from_view(
                view,
            )?)),
            ffi::EXPR_CONSTANT => {
                // SAFETY: a constant expression view owns a required snapshot-backed bit string.
                let bits = unsafe { required_str(view.constant_bits, "constant bits")? };
                Ok(SlangExpressionKind::Constant(SlangLogicConstant {
                    width: has_flag(view.constant_has_width).then_some(view.constant_width),
                    bits,
                    signed: has_flag(view.constant_signed),
                }))
            }
            ffi::EXPR_UNARY => Ok(SlangExpressionKind::Unary {
                op: map_unary_op(view.unary_op)?,
                arg: SlangExpression::from_raw(view.unary_arg, "unary operand")?,
            }),
            ffi::EXPR_BINARY => Ok(SlangExpressionKind::Binary {
                op: map_binary_op(view.binary_op)?,
                left: SlangExpression::from_raw(view.binary_left, "binary left operand")?,
                right: SlangExpression::from_raw(view.binary_right, "binary right operand")?,
            }),
            ffi::EXPR_CONCAT => Ok(SlangExpressionKind::Concat(SlangConcat {
                expression: self,
            })),
            ffi::EXPR_MUX => Ok(SlangExpressionKind::Mux {
                condition: SlangExpression::from_raw(view.mux_condition, "mux condition")?,
                then_value: SlangExpression::from_raw(view.mux_then, "mux then operand")?,
                else_value: SlangExpression::from_raw(view.mux_else, "mux else operand")?,
            }),
            ffi::EXPR_CAST => Ok(SlangExpressionKind::Cast {
                kind: map_cast_kind(view.cast_kind)?,
                value: SlangExpression::from_raw(view.cast_value, "cast operand")?,
                width: view.cast_width,
                signed: has_flag(view.cast_signed),
            }),
            ffi::EXPR_EXTRACT => Ok(SlangExpressionKind::Extract {
                value: SlangExpression::from_raw(view.extract_value, "extract operand")?,
                lsb: view.extract_lsb,
                width: view.extract_width,
            }),
            ffi::EXPR_DYNAMIC_EXTRACT => Ok(SlangExpressionKind::DynamicExtract {
                value: SlangExpression::from_raw(
                    view.dynamic_extract_value,
                    "dynamic extract operand",
                )?,
                offset: SlangExpression::from_raw(
                    view.dynamic_extract_offset,
                    "dynamic extract offset",
                )?,
                width: view.dynamic_extract_width,
            }),
            raw => Err(unknown_enum("expression kind", raw)),
        }
    }

    pub(super) fn signal_ref(self) -> Result<SlangSignalRef<'a>, SlangError> {
        let view = self.view()?;
        Self::signal_ref_from_view(view)
    }
}

#[derive(Debug, Clone, Copy)]
/// Optional source location retained for an elaborated construct.
pub struct SlangSourceSpan<'a> {
    /// Source file, when known.
    pub file: Option<&'a Path>,
    /// One-based source line, when known.
    pub line: Option<u32>,
    /// One-based source column, when known.
    pub column: Option<u32>,
}

impl SlangSourceSpan<'_> {
    pub(super) fn from_view(view: ffi::SourceSpanView) -> Result<Self, SlangError> {
        Self::from_raw(view.file, view.line, view.column)
    }

    fn from_raw(file: *const std::ffi::c_char, line: u32, column: u32) -> Result<Self, SlangError> {
        // SAFETY: source paths are owned by the live materialized module.
        let file = unsafe { optional_str(file, "source file")? }.map(Path::new);
        Ok(Self {
            file,
            line: nonzero(line),
            column: nonzero(column),
        })
    }
}

#[derive(Debug, Clone, Copy)]
/// Named signal and optional selected bit range.
pub struct SlangSignalRef<'a> {
    /// Elaborated signal name.
    pub name: &'a str,
    /// Selected inclusive bit range, if the expression is a selection.
    pub range: Option<SlangBitRange>,
}

#[derive(Debug, Clone, Copy)]
/// Four-state logic constant as normalized bit text.
pub struct SlangLogicConstant<'a> {
    /// Explicit source width, or `None` for an unsized constant.
    pub width: Option<u32>,
    /// Most-significant-first four-state bit text.
    pub bits: &'a str,
    /// Whether the constant has signed interpretation.
    pub signed: bool,
}

#[derive(Debug, Clone, Copy)]
/// Decoded shape of an elaborated expression.
pub enum SlangExpressionKind<'a> {
    /// Signal reference, optionally selecting a constant bit range.
    Signal(SlangSignalRef<'a>),
    /// Four-state logic constant.
    Constant(SlangLogicConstant<'a>),
    /// Unary operator application.
    Unary {
        /// Unary operator.
        op: SlangUnaryOp,
        /// Operand expression.
        arg: SlangExpression<'a>,
    },
    /// Binary operator application.
    Binary {
        /// Binary operator.
        op: SlangBinaryOp,
        /// Left operand.
        left: SlangExpression<'a>,
        /// Right operand.
        right: SlangExpression<'a>,
    },
    /// Conditional selection expression.
    Mux {
        /// Boolean selection condition.
        condition: SlangExpression<'a>,
        /// Value selected when true.
        then_value: SlangExpression<'a>,
        /// Value selected when false.
        else_value: SlangExpression<'a>,
    },
    /// Concatenation of one or more operands.
    Concat(SlangConcat<'a>),
    /// Explicit or implicit cast.
    Cast {
        /// Cast semantics.
        kind: SlangCastKind,
        /// Value being converted.
        value: SlangExpression<'a>,
        /// Result bit width.
        width: u32,
        /// Whether the result uses signed interpretation.
        signed: bool,
    },
    /// Constant-offset bit extraction.
    Extract {
        /// Source value.
        value: SlangExpression<'a>,
        /// Least-significant source bit.
        lsb: u32,
        /// Number of extracted bits.
        width: u32,
    },
    /// Runtime-offset bit extraction.
    DynamicExtract {
        /// Source value.
        value: SlangExpression<'a>,
        /// Runtime least-significant-bit expression.
        offset: SlangExpression<'a>,
        /// Number of extracted bits.
        width: u32,
    },
}

#[derive(Debug, Clone, Copy)]
/// Borrowed iterable view of concatenation operands.
pub struct SlangConcat<'a> {
    expression: SlangExpression<'a>,
}

impl<'a> SlangConcat<'a> {
    /// Iterates over concatenation operands from left to right.
    #[must_use]
    pub fn parts(self) -> impl ExactSizeIterator<Item = Result<SlangExpression<'a>, SlangError>> {
        // SAFETY: the expression belongs to a live snapshot and is known to be a concatenation.
        let view = unsafe {
            read_invariant("concatenation", |view| {
                ffi::opto_slang_expr_view(self.expression.raw.as_ptr(), view)
            })
        };
        (0..view.concat_count).map(move |index| {
            // SAFETY: `index` is bounded by the concat count paired with this pointer array.
            let raw = unsafe { pointer_element(view.concat_parts, index, "concatenation operand") };
            SlangExpression::from_raw(raw, "concatenation operand")
        })
    }
}
