// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::{mapped::MappedError, word::WordError};
use thiserror::Error;

#[derive(Debug, Error)]
/// Failure to parse an input format or serialize a design artifact.
pub enum FormatError {
    /// The design is structurally unsuitable for the requested output.
    #[error("{0}")]
    InvalidDesign(String),
    /// The format cannot represent a construct present in the design.
    #[error("{0}")]
    Unsupported(String),
    /// The design exceeds a representable index or size domain.
    #[error("{0}")]
    Capacity(String),
    /// A SPEF token or section is malformed.
    #[error("read_parasitics: SPEF line {line}: {detail}")]
    Spef {
        /// One-based input line number.
        line: usize,
        /// Description of the violated SPEF grammar or semantic rule.
        detail: String,
    },
    /// Word IR validation failed while writing structural Verilog.
    #[error("write_verilog: {0}")]
    Word(#[source] WordError),
    /// Mapped-netlist validation failed while writing structural Verilog.
    #[error("write_verilog: {0}")]
    Mapped(#[source] MappedError),
    /// The destination writer rejected output.
    #[error("write_verilog: failed to write output: {0}")]
    Io(#[from] std::io::Error),
}

impl FormatError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidDesign(message.into())
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub(crate) fn capacity(message: impl Into<String>) -> Self {
        Self::Capacity(message.into())
    }

    pub(crate) fn spef(line: usize, detail: impl Into<String>) -> Self {
        Self::Spef {
            line,
            detail: detail.into(),
        }
    }
}
