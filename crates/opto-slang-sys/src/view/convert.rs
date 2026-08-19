// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Total conversion of native tags, flags, and borrowed strings.
//!
//! Unknown enum values are bridge invariant failures rather than defaults; a
//! frontend upgrade therefore cannot silently acquire different HDL semantics.

use crate::ffi;
use crate::{
    SlangBinaryOp, SlangCastKind, SlangEdge, SlangError, SlangLoopForm, SlangNetResolution,
    SlangPortDirection, SlangProcedureKind, SlangUnaryOp,
};
use std::ffi::{CStr, c_char, c_int};

pub(super) fn map_port_direction(raw: c_int) -> Result<SlangPortDirection, SlangError> {
    match raw {
        ffi::PORT_INPUT => Ok(SlangPortDirection::Input),
        ffi::PORT_OUTPUT => Ok(SlangPortDirection::Output),
        ffi::PORT_INOUT => Ok(SlangPortDirection::Inout),
        ffi::PORT_REF => Ok(SlangPortDirection::Ref),
        _ => Err(unknown_enum("port direction", raw)),
    }
}

pub(super) fn map_net_resolution(raw: c_int) -> Result<SlangNetResolution, SlangError> {
    match raw {
        0 => Ok(SlangNetResolution::SingleDriver),
        1 => Ok(SlangNetResolution::WiredAnd),
        2 => Ok(SlangNetResolution::WiredOr),
        3 => Ok(SlangNetResolution::PullZero),
        4 => Ok(SlangNetResolution::PullOne),
        5 => Ok(SlangNetResolution::SupplyZero),
        6 => Ok(SlangNetResolution::SupplyOne),
        _ => Err(unknown_enum("net resolution", raw)),
    }
}

pub(super) fn map_unary_op(raw: c_int) -> Result<SlangUnaryOp, SlangError> {
    match raw {
        ffi::UNARY_LOGICAL_NOT => Ok(SlangUnaryOp::LogicalNot),
        ffi::UNARY_BIT_NOT => Ok(SlangUnaryOp::BitNot),
        ffi::UNARY_REDUCTION_AND => Ok(SlangUnaryOp::ReductionAnd),
        ffi::UNARY_REDUCTION_OR => Ok(SlangUnaryOp::ReductionOr),
        ffi::UNARY_REDUCTION_XOR => Ok(SlangUnaryOp::ReductionXor),
        _ => Err(unknown_enum("unary operator", raw)),
    }
}

pub(super) fn map_binary_op(raw: c_int) -> Result<SlangBinaryOp, SlangError> {
    match raw {
        ffi::BINARY_ADD => Ok(SlangBinaryOp::Add),
        ffi::BINARY_SUB => Ok(SlangBinaryOp::Sub),
        ffi::BINARY_MUL => Ok(SlangBinaryOp::Mul),
        ffi::BINARY_BIT_AND => Ok(SlangBinaryOp::BitAnd),
        ffi::BINARY_BIT_OR => Ok(SlangBinaryOp::BitOr),
        ffi::BINARY_BIT_XOR => Ok(SlangBinaryOp::BitXor),
        ffi::BINARY_LOGICAL_AND => Ok(SlangBinaryOp::LogicalAnd),
        ffi::BINARY_LOGICAL_OR => Ok(SlangBinaryOp::LogicalOr),
        ffi::BINARY_EQ => Ok(SlangBinaryOp::Eq),
        ffi::BINARY_NE => Ok(SlangBinaryOp::Ne),
        ffi::BINARY_LT => Ok(SlangBinaryOp::Lt),
        ffi::BINARY_LE => Ok(SlangBinaryOp::Le),
        ffi::BINARY_GT => Ok(SlangBinaryOp::Gt),
        ffi::BINARY_GE => Ok(SlangBinaryOp::Ge),
        ffi::BINARY_SHL => Ok(SlangBinaryOp::Shl),
        ffi::BINARY_SHR => Ok(SlangBinaryOp::Shr),
        ffi::BINARY_ASHR => Ok(SlangBinaryOp::Ashr),
        ffi::BINARY_DIV => Ok(SlangBinaryOp::Div),
        ffi::BINARY_MOD => Ok(SlangBinaryOp::Mod),
        _ => Err(unknown_enum("binary operator", raw)),
    }
}

pub(super) fn map_cast_kind(raw: c_int) -> Result<SlangCastKind, SlangError> {
    match raw {
        ffi::CAST_ZERO_EXTEND => Ok(SlangCastKind::ZeroExtend),
        ffi::CAST_SIGN_EXTEND => Ok(SlangCastKind::SignExtend),
        ffi::CAST_TRUNCATE => Ok(SlangCastKind::Truncate),
        _ => Err(unknown_enum("cast kind", raw)),
    }
}

pub(super) fn map_procedure_kind(raw: c_int) -> Result<SlangProcedureKind, SlangError> {
    match raw {
        ffi::PROCEDURE_COMB => Ok(SlangProcedureKind::Comb),
        ffi::PROCEDURE_LATCH => Ok(SlangProcedureKind::Latch),
        ffi::PROCEDURE_FLOP => Ok(SlangProcedureKind::Flop),
        ffi::PROCEDURE_COMB_OR_LATCH => Ok(SlangProcedureKind::CombOrLatch),
        _ => Err(unknown_enum("procedure kind", raw)),
    }
}

pub(super) fn map_loop_form(raw: c_int) -> Result<SlangLoopForm, SlangError> {
    match raw {
        ffi::LOOP_PRE_TEST => Ok(SlangLoopForm::PreTest),
        ffi::LOOP_POST_TEST => Ok(SlangLoopForm::PostTest),
        ffi::LOOP_UNCONDITIONAL => Ok(SlangLoopForm::Unconditional),
        _ => Err(unknown_enum("loop form", raw)),
    }
}

pub(super) fn map_edge(raw: c_int) -> Result<SlangEdge, SlangError> {
    match raw {
        ffi::EDGE_POS => Ok(SlangEdge::Pos),
        ffi::EDGE_NEG => Ok(SlangEdge::Neg),
        _ => Err(unknown_enum("edge", raw)),
    }
}

pub(super) fn unknown_enum(kind: &str, raw: c_int) -> SlangError {
    SlangError::BridgeInvariant(format!("native slang bridge returned unknown {kind} {raw}"))
}

pub(super) fn has_flag(raw: c_int) -> bool {
    raw != 0
}

pub(super) fn nonzero(value: u32) -> Option<u32> {
    (value != 0).then_some(value)
}

/// Borrows a required native UTF-8 string.
///
/// # Safety
///
/// A non-null `ptr` must reference a live NUL-terminated byte sequence for the
/// returned lifetime. Null is reported as a bridge invariant failure.
pub(super) unsafe fn required_str<'a>(
    ptr: *const c_char,
    context: &str,
) -> Result<&'a str, SlangError> {
    // SAFETY: the caller guarantees any non-null pointer references a live bridge string.
    unsafe { optional_str(ptr, context) }?.ok_or_else(|| {
        SlangError::BridgeInvariant(format!("native slang bridge returned null {context}"))
    })
}

/// Borrows an optional native UTF-8 string.
///
/// # Safety
///
/// A non-null `ptr` must reference a live NUL-terminated byte sequence for the
/// returned lifetime.
pub(super) unsafe fn optional_str<'a>(
    ptr: *const c_char,
    context: &str,
) -> Result<Option<&'a str>, SlangError> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: the caller guarantees the checked non-null pointer is NUL-terminated and live.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(Some)
        .map_err(|_| {
            SlangError::BridgeInvariant(format!("native slang bridge returned non-UTF-8 {context}"))
        })
}
