// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_core::{Diagnostic, DiagnosticSource};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
/// Failure while resolving objects, mutating session state, or running a use
/// case.
pub enum SessionError {
    /// Invalid command arguments or command-level semantics.
    #[error("{0}")]
    Command(String),
    /// Session lifecycle or current-design state is invalid.
    #[error("{0}")]
    State(String),
    /// An object name, type, or identity could not be resolved.
    #[error("{0}")]
    Object(String),
    /// A collection handle or operation is invalid.
    #[error("{0}")]
    Collection(String),
    /// A count, identifier, or byte calculation exceeded capacity.
    #[error("{0}")]
    Capacity(String),
    /// The requested compatibility behavior is not implemented.
    #[error("{0}")]
    Unsupported(String),
    /// Checkpoint encoding, decoding, or validation failed.
    #[error("checkpoint: {0}")]
    Checkpoint(String),
    /// A named file operation failed.
    #[error("{operation}: failed to access '{}': {source}", .path.display())]
    Io {
        /// Stable operation name.
        operation: &'static str,
        /// Path the operation attempted to access.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// HDL parsing, elaboration, or materialization failed.
    #[error("{0}")]
    Hdl(#[from] opto_hdl::HdlError),
    /// Synthesis failed outside a design-specific synthesis command.
    #[error("{0}")]
    Synthesis(#[from] opto_synth::SynthError),
    /// Synthesis failed for a named design and command.
    #[error("{command}: design '{design}': {source}")]
    Synth {
        /// Public synthesis command name.
        command: &'static str,
        /// Design being synthesized.
        design: String,
        /// Underlying synthesis failure.
        #[source]
        source: opto_synth::SynthError,
    },
    /// Pre-synthesis structural validation failed.
    #[error("check_design: {0}")]
    CheckDesign(#[from] opto_synth::CheckDesignError),
    /// Timing constraint, model, or analysis failed.
    #[error("{0}")]
    Timing(#[from] opto_timing::TimingError),
    /// Power annotation or analysis failed.
    #[error("{0}")]
    Power(#[from] opto_power::PowerError),
    /// Report parsing or rendering failed.
    #[error("{0}")]
    Format(#[from] opto_formats::FormatError),
    /// Liberty library parsing or validation failed.
    #[error("{0}")]
    Library(#[from] opto_library::LibraryError),
    /// Runtime scheduling failed.
    #[error("{0}")]
    Runtime(#[from] opto_runtime::RuntimeError),
    /// Linked-definition graph construction failed.
    #[error("{0}")]
    DefinitionGraph(#[from] opto_db::DefinitionGraphError),
    /// Definition graph construction failed for a command.
    #[error("{command}: {source}")]
    DefinitionGraphContext {
        /// Command requesting the graph.
        command: String,
        /// Underlying definition-graph failure.
        #[source]
        source: opto_db::DefinitionGraphError,
    },
    /// Word IR construction or validation failed.
    #[error("{0}")]
    Word(#[from] opto_ir::word::WordError),
    /// RTL construction or validation failed.
    #[error("{0}")]
    Rtl(#[from] opto_ir::rtl::RtlError),
    /// Mapped-netlist construction or validation failed.
    #[error("{0}")]
    Mapped(#[from] opto_ir::mapped::MappedError),
    /// Permanent object registration failed.
    #[error("{0}")]
    Registry(#[from] opto_db::RegistryError),
    /// A permanent object ID exceeded its representable domain.
    #[error("{0}")]
    ObjectCapacity(#[from] opto_core::CapacityError),
    /// The monotonic session revision counter was exhausted.
    #[error("session revision space exhausted")]
    Revision(#[from] opto_core::RevisionExhausted),
}

impl SessionError {
    pub(crate) fn state(message: impl Into<String>) -> Self {
        Self::State(message.into())
    }

    pub(crate) fn object(message: impl Into<String>) -> Self {
        Self::Object(message.into())
    }

    pub(crate) fn capacity(message: impl Into<String>) -> Self {
        Self::Capacity(message.into())
    }

    pub(crate) fn checkpoint(message: impl Into<String>) -> Self {
        Self::Checkpoint(message.into())
    }

    pub(crate) fn synthesis(
        command: &'static str,
        design: impl Into<String>,
        source: opto_synth::SynthError,
    ) -> Self {
        Self::Synth {
            command,
            design: design.into(),
            source,
        }
    }
}

impl DiagnosticSource for SessionError {
    fn diagnostic(&self) -> Option<Diagnostic> {
        match self {
            Self::Synthesis(source) => source.diagnostic(),
            Self::Synth {
                command,
                design,
                source,
            } => source.diagnostic().map(|diagnostic| {
                diagnostic.with_note(format!(
                    "synthesis command '{command}' was running for design '{design}'"
                ))
            }),
            _ => None,
        }
    }
}
