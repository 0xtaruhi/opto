// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
/// Failure to ingest, elaborate, or lower an HDL design.
pub enum HdlError {
    /// No source files were supplied.
    #[error("verilog frontend: no input files")]
    NoInputFiles,
    /// Elaboration was requested without ingested source units.
    #[error("verilog frontend: no ingested source units")]
    NoSourceUnits,
    /// A source file could not be read.
    #[error("verilog frontend: failed to read '{}': {source}", .path.display())]
    ReadSource {
        /// Source path that could not be read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Native Slang analysis or elaboration failed.
    #[error("verilog frontend: {0}")]
    Slang(#[source] opto_slang_sys::SlangError),
    /// Parallel lowering failed.
    #[error("verilog frontend: {0}")]
    Runtime(#[from] opto_runtime::RuntimeError),
    /// Word-IR construction or validation failed.
    #[error("verilog frontend: {0}")]
    Ir(#[source] opto_ir::word::WordError),
    /// Procedural-IR construction failed.
    #[error("verilog frontend: {0}")]
    Proc(#[from] opto_ir::proc::ProcError),
    /// RTL-IR construction failed.
    #[error("verilog frontend: {0}")]
    Rtl(#[from] opto_ir::rtl::RtlError),
    /// A four-state constant could not be represented.
    #[error("verilog frontend: {0}")]
    Constant(#[source] opto_ir::ValueError),
    /// IR failure associated with a copied source location.
    #[error("verilog frontend: {source} at {location}")]
    IrAt {
        /// User-facing source location.
        location: String,
        /// Word-IR error at that location.
        #[source]
        source: opto_ir::word::WordError,
    },
    /// Elaborated HDL violates a frontend model invariant.
    #[error("{0}")]
    InvalidModel(String),
    /// The frontend recognized but cannot lower a construct.
    #[error("{0}")]
    UnsupportedConstruct(String),
}

impl HdlError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidModel(message.into())
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::UnsupportedConstruct(message.into())
    }
}
