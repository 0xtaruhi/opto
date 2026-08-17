// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    unsafe_code,
    reason = "this crate is the audited Rust boundary for the native slang bridge"
)]

//! Safe Rust boundary for Opto's slang bridge.
//!
//! The C++ bridge is the sole owner of the lowered slang result. Rust exposes
//! immutable module inventory through [`SlangCompilation`] and lowered views
//! whose lifetimes are tied to [`SlangMaterializedModule`]. Dropping the last
//! materialized-module guard releases that module's native lowering arenas; no
//! slang AST pointer or duplicate owning syntax tree crosses this boundary.
//!
//! The bridge has two phases. [`analyze`] returns source-level inventory and
//! dependencies, while [`compile_units_lazy`] creates a compilation whose
//! modules are materialized on demand. Views borrow their materialized-module
//! guard and cannot outlive native storage. All public errors copy diagnostic
//! text before the C++ owner is released.

use std::fmt;
use std::path::PathBuf;

mod bridge;
mod compiler;
mod ffi;
mod view;

pub use view::{
    SlangArrayKind, SlangAttribute, SlangAttributeValue, SlangBlock, SlangBlockId,
    SlangCompilation, SlangConcat, SlangContinuousAssign, SlangEdgeTarget, SlangEffect,
    SlangExpression, SlangExpressionKind, SlangIndexRange, SlangInstance, SlangInstanceConnection,
    SlangLogicConstant, SlangLoopRegion, SlangLoopRegionId, SlangMaterializedModule, SlangModule,
    SlangNet, SlangPort, SlangProcedure, SlangSensitivityEvent, SlangSignalRef, SlangSourceSpan,
    SlangSwitchArm, SlangSwitchArms, SlangTerminator, SlangTerminatorKind, SlangTypeField,
    SlangTypeLayout, SlangTypeLayoutKind,
};

/// Stable digest of the pinned Slang sources and Opto's native bridge.
pub const NATIVE_FRONTEND_FINGERPRINT: &str = env!("OPTO_SLANG_NATIVE_FINGERPRINT");

/// Options for compiling files directly through the slang bridge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlangCompileOptions {
    /// Optional top definition; absent means slang must infer it.
    pub top: Option<String>,
    /// Include search paths in lookup order.
    pub include_paths: Vec<PathBuf>,
    /// Preprocessor definitions in command-line order.
    pub defines: Vec<SlangDefine>,
    /// Verilog-family source revision.
    pub language: SlangLanguage,
    /// Maximum native worker count, or `None` for the runtime default.
    pub max_threads: Option<usize>,
}

/// Owned source unit used by separate analysis and elaboration flows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangSourceUnit {
    /// Primary source files in compilation order.
    pub files: Vec<SlangSourceFile>,
    /// Include dependencies captured during an earlier analysis.
    pub dependencies: Vec<SlangSourceFile>,
    /// Include search paths in lookup order.
    pub include_paths: Vec<PathBuf>,
    /// Preprocessor definitions in command-line order.
    pub defines: Vec<SlangDefine>,
    /// Verilog-family source revision.
    pub language: SlangLanguage,
}

/// Source-language revision selected for slang.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SlangLanguage {
    /// IEEE 1364-2005 Verilog.
    Verilog2005,
    /// IEEE 1800-2017 `SystemVerilog`.
    #[default]
    SystemVerilog2017,
}

/// Path and immutable text of one source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangSourceFile {
    /// Diagnostic path associated with the text.
    pub path: PathBuf,
    /// Complete UTF-8 source contents.
    pub text: String,
}

/// Source inventory returned by syntax analysis without elaboration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangAnalysis {
    /// Definition names in deterministic discovery order.
    pub definitions: Vec<String>,
    /// Package names in deterministic discovery order.
    pub packages: Vec<String>,
    /// Transitive include files required by the analyzed units.
    pub dependencies: Vec<SlangSourceFile>,
    /// Structured diagnostics emitted while analyzing the source units.
    pub diagnostics: Vec<SlangDiagnostic>,
}

/// Severity of one structured native frontend diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangDiagnosticSeverity {
    /// Secondary explanatory information associated with another diagnostic.
    Note,
    /// A recoverable source condition that may be surprising.
    Warning,
    /// A source condition that prevents analysis or elaboration.
    Error,
}

/// Source coordinate copied out of the native frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangDiagnosticLocation {
    /// Diagnostic source path.
    pub path: PathBuf,
    /// One-based source line.
    pub line: u32,
    /// One-based source column.
    pub column: u32,
    /// Highlighted byte length, clamped to at least one.
    pub length: u32,
}

/// Structured diagnostic copied out of Slang without parsing rendered text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangDiagnostic {
    /// Effective severity after Slang's diagnostic mapping.
    pub severity: SlangDiagnosticSeverity,
    /// Stable Slang diagnostic subsystem number.
    pub subsystem: u16,
    /// Stable diagnostic number within the subsystem.
    pub code: u16,
    /// Formatted diagnostic message without a source prefix.
    pub message: String,
    /// Optional Slang warning-control name.
    pub option_name: Option<String>,
    /// Optional source location for location-free diagnostics.
    pub location: Option<SlangDiagnosticLocation>,
}

/// Earliest authoritative stage classification for a native lowering failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangLoweringFailureCategory {
    /// The source construct is intentionally outside the synthesis profile.
    UnsupportedProfile,
    /// Slang semantics could not be represented by the frontend projection.
    InvalidProjection,
    /// Deterministic structural capacity was exceeded.
    Capacity,
    /// Native lowering violated an internal contract.
    Invariant,
    /// A non-standard native failure escaped lowering.
    Native,
}

/// Structured failure produced while materializing one lowered module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangLoweringFailure {
    /// Stable failure category owned by the lowering boundary.
    pub category: SlangLoweringFailureCategory,
    /// Stable code within `category`.
    pub code: u16,
    /// Human-readable detail copied out of native storage.
    pub message: String,
    /// Source coordinate when the rejecting construct supplied one.
    pub location: Option<SlangDiagnosticLocation>,
}

impl SlangLoweringFailure {
    /// Returns a stable, searchable product-facing code.
    #[must_use]
    pub fn stable_code(&self) -> String {
        let category = match self.category {
            SlangLoweringFailureCategory::UnsupportedProfile => "P",
            SlangLoweringFailureCategory::InvalidProjection => "R",
            SlangLoweringFailureCategory::Capacity => "C",
            SlangLoweringFailureCategory::Invariant => "I",
            SlangLoweringFailureCategory::Native => "N",
        };
        format!("OPT-HDL-L{category}-{:04}", self.code)
    }
}

impl SlangDiagnostic {
    /// Returns a stable, searchable product-facing code.
    #[must_use]
    pub fn stable_code(&self) -> String {
        format!("OPT-HDL-S{:02}-{:04}", self.subsystem, self.code)
    }
}

/// One preprocessor definition passed to slang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangDefine {
    /// Macro name without the leading `` `define`` token.
    pub name: String,
    /// Optional replacement text; `None` defines an empty macro.
    pub value: Option<String>,
}

/// Direction of a lowered module port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangPortDirection {
    /// Driven by the containing environment.
    Input,
    /// Driven by the module.
    Output,
    /// Bidirectional resolved port.
    Inout,
    /// Exact variable alias shared with the containing instance.
    Ref,
}

/// Inclusive source indices of a packed bit range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlangBitRange {
    /// Source index written on the left of the range.
    pub msb: u32,
    /// Source index written on the right of the range.
    pub lsb: u32,
}

/// Unary operation preserved by the native lowered view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangUnaryOp {
    /// Logical negation (`!`).
    LogicalNot,
    /// Bitwise complement (`~`).
    BitNot,
    /// Reduction AND (`&`).
    ReductionAnd,
    /// Reduction OR (`|`).
    ReductionOr,
    /// Reduction XOR (`^`).
    ReductionXor,
}

/// Binary operation preserved by the native lowered view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangBinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Bitwise AND.
    BitAnd,
    /// Bitwise OR.
    BitOr,
    /// Bitwise XOR.
    BitXor,
    /// Logical AND.
    LogicalAnd,
    /// Logical OR.
    LogicalOr,
    /// Equality comparison.
    Eq,
    /// Inequality comparison.
    Ne,
    /// Less-than comparison.
    Lt,
    /// Less-than-or-equal comparison.
    Le,
    /// Greater-than comparison.
    Gt,
    /// Greater-than-or-equal comparison.
    Ge,
    /// Logical left shift.
    Shl,
    /// Logical right shift.
    Shr,
    /// Arithmetic right shift.
    Ashr,
    /// Division.
    Div,
    /// Remainder.
    Mod,
}

/// Width-changing cast represented explicitly during lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangCastKind {
    /// Extend with zero bits.
    ZeroExtend,
    /// Extend by replicating the sign bit.
    SignExtend,
    /// Keep the low-order target-width bits.
    Truncate,
}

/// Active edge of an event control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangEdge {
    /// Rising edge.
    Pos,
    /// Falling edge.
    Neg,
}

/// Semantic category of a lowered procedural block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangProcedureKind {
    /// Proven combinational procedure.
    Comb,
    /// Level-sensitive storage procedure.
    Latch,
    /// Edge-triggered storage procedure.
    Flop,
    /// Classic `always` block whose combinational/latch status is inferred later.
    CombOrLatch,
}

/// Placement of a source loop's continuation condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangLoopForm {
    /// Condition is tested before the body.
    PreTest,
    /// Condition is tested after the body.
    PostTest,
    /// The source loop has no condition.
    Unconditional,
}

/// Scheduling semantics of a procedural assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangAssignmentMode {
    /// Value is visible immediately to later statements in the procedure.
    Blocking,
    /// Value is scheduled for the end of the current time step.
    Nonblocking,
}

/// Resolution function attached to a lowered net.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlangNetResolution {
    /// Multiple drivers are illegal.
    SingleDriver,
    /// Drivers resolve through wired AND.
    WiredAnd,
    /// Drivers resolve through wired OR.
    WiredOr,
}

/// Failure reported by the safe slang bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlangError {
    /// Caller input is structurally invalid before native compilation.
    InvalidInput(String),
    /// Slang rejected the source; the string contains copied diagnostics.
    CompileFailed(String),
    /// Slang rejected the source and returned structured diagnostics.
    Diagnostics(Vec<SlangDiagnostic>),
    /// Native module lowering rejected the source with structured context.
    LoweringFailed(SlangLoweringFailure),
    /// Native data violated an invariant required by the Rust view.
    BridgeInvariant(String),
}

impl fmt::Display for SlangError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::BridgeInvariant(message) => f.write_str(message),
            Self::CompileFailed(message) if message.is_empty() => {
                f.write_str("slang compilation failed")
            }
            Self::CompileFailed(message) => write!(f, "slang compilation failed: {message}"),
            Self::Diagnostics(diagnostics) => {
                f.write_str("slang compilation failed")?;
                if let Some(primary) = diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.severity == SlangDiagnosticSeverity::Error)
                {
                    write!(f, ": {}", primary.message)?;
                }
                Ok(())
            }
            Self::LoweringFailed(failure) => {
                write!(
                    f,
                    "slang module lowering failed [{}]: {}",
                    failure.stable_code(),
                    failure.message
                )
            }
        }
    }
}

impl std::error::Error for SlangError {}

/// Compiles files eagerly and materializes every lowered module.
///
/// # Errors
///
/// Returns [`SlangError::InvalidInput`] for an empty file list and propagates
/// compiler diagnostics or native bridge invariant failures.
pub fn compile(
    files: &[PathBuf],
    options: &SlangCompileOptions,
) -> Result<SlangCompilation, SlangError> {
    if files.is_empty() {
        return Err(SlangError::InvalidInput(
            "slang compile requires at least one input file".to_string(),
        ));
    }
    compiler::compile(files, options)
}

/// Compiles files while deferring module lowering until requested.
///
/// # Errors
///
/// Returns [`SlangError::InvalidInput`] for an empty file list and propagates
/// compiler diagnostics or native bridge invariant failures.
pub fn compile_lazy(
    files: &[PathBuf],
    options: &SlangCompileOptions,
) -> Result<SlangCompilation, SlangError> {
    if files.is_empty() {
        return Err(SlangError::InvalidInput(
            "slang compile requires at least one input file".to_string(),
        ));
    }
    compiler::compile_lazy(files, options)
}

/// Analyzes owned source units without selecting or elaborating a top.
///
/// # Errors
///
/// Returns [`SlangError`] for invalid source options, parse/semantic diagnostics,
/// or a malformed native analysis view.
pub fn analyze(
    units: &[SlangSourceUnit],
    max_threads: Option<usize>,
) -> Result<SlangAnalysis, SlangError> {
    compiler::analyze(units, max_threads)
}

/// Elaborates owned source units and eagerly materializes every module.
///
/// # Errors
///
/// Returns [`SlangError`] for invalid inputs, compile/elaboration diagnostics,
/// a missing top, or a malformed native snapshot.
pub fn compile_units(
    units: &[SlangSourceUnit],
    top: &str,
    max_threads: Option<usize>,
) -> Result<SlangCompilation, SlangError> {
    compiler::compile_units(units, top, max_threads)
}

/// Elaborates owned source units with on-demand module materialization.
///
/// # Errors
///
/// Returns [`SlangError`] for invalid inputs, compile/elaboration diagnostics,
/// a missing top, or a malformed native snapshot.
pub fn compile_units_lazy(
    units: &[SlangSourceUnit],
    top: &str,
    max_threads: Option<usize>,
) -> Result<SlangCompilation, SlangError> {
    compiler::compile_units_lazy(units, top, max_threads)
}

#[cfg(test)]
mod tests;
