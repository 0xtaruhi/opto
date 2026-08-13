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
            Self::Hdl(source) => Some(hdl_error_diagnostic(source)),
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
            Self::Timing(source) => source.diagnostic(),
            Self::Power(source) => source.diagnostic(),
            Self::Format(source) => Some(format_error_diagnostic(source)),
            Self::Library(source) => source.diagnostic(),
            Self::Command(_) => Some(Diagnostic::new("OPT-SES-001", self.to_string())),
            Self::State(_) => Some(Diagnostic::new("OPT-SES-002", self.to_string())),
            Self::Object(_) => Some(Diagnostic::new("OPT-SES-003", self.to_string())),
            Self::Collection(_) => Some(Diagnostic::new("OPT-SES-004", self.to_string())),
            Self::Capacity(_) => Some(Diagnostic::new("OPT-SES-005", self.to_string())),
            Self::Unsupported(_) => Some(
                Diagnostic::new("OPT-SES-006", self.to_string())
                    .with_help("use functionality documented as implemented by the Opto command catalog"),
            ),
            Self::Checkpoint(_) => Some(Diagnostic::new("OPT-SES-100", self.to_string())),
            Self::Io { .. } => Some(Diagnostic::new("OPT-SES-101", self.to_string())),
            Self::CheckDesign(_) => Some(Diagnostic::new("OPT-SES-200", self.to_string())),
            Self::Runtime(_) => Some(Diagnostic::new("OPT-SES-500", self.to_string())),
            Self::DefinitionGraph(_) => {
                Some(Diagnostic::new("OPT-SES-300", self.to_string()))
            }
            Self::DefinitionGraphContext { command, .. } => Some(
                Diagnostic::new("OPT-SES-300", self.to_string())
                    .with_note(format!("definition graph was requested by '{command}'")),
            ),
            Self::Word(_) | Self::Rtl(_) | Self::Mapped(_) => Some(
                Diagnostic::new("OPT-SES-900", self.to_string()).with_help(
                    "retain the design and diagnostic code when reporting this internal IR failure",
                ),
            ),
            Self::Registry(_)
            | Self::ObjectCapacity(_)
            | Self::Revision(_) => Some(
                Diagnostic::new("OPT-SES-901", self.to_string()).with_help(
                    "retain the session inputs and diagnostic code when reporting this internal capacity failure",
                ),
            ),
        }
    }
}

pub(crate) fn hdl_diagnostics(diagnostics: &[opto_hdl::SlangDiagnostic]) -> Vec<Diagnostic> {
    let mut converted = Vec::<Diagnostic>::new();
    for diagnostic in diagnostics {
        if diagnostic.severity == opto_hdl::SlangDiagnosticSeverity::Note {
            if let Some(previous) = converted.pop() {
                converted.push(previous.with_note(&diagnostic.message));
            }
            continue;
        }
        let mut converted_diagnostic = match diagnostic.severity {
            opto_hdl::SlangDiagnosticSeverity::Warning => {
                Diagnostic::warning(diagnostic.stable_code(), &diagnostic.message)
            }
            opto_hdl::SlangDiagnosticSeverity::Error => {
                Diagnostic::new(diagnostic.stable_code(), &diagnostic.message)
            }
            opto_hdl::SlangDiagnosticSeverity::Note => unreachable!(),
        };
        if let Some(location) = &diagnostic.location {
            converted_diagnostic =
                converted_diagnostic.with_primary(opto_core::DiagnosticLabel::new(
                    opto_core::DiagnosticLocation::new(
                        location.path.to_string_lossy(),
                        location.line,
                        Some(location.column),
                    )
                    .with_length(location.length),
                    "frontend diagnostic occurs here",
                ));
        }
        if let Some(option) = &diagnostic.option_name {
            converted_diagnostic =
                converted_diagnostic.with_note(format!("Slang diagnostic option: -W{option}"));
        }
        converted.push(converted_diagnostic);
    }
    converted
}

fn hdl_error_diagnostic(error: &opto_hdl::HdlError) -> Diagnostic {
    if let opto_hdl::HdlError::Slang(opto_slang_error) = error
        && let opto_hdl::SlangError::Diagnostics(diagnostics) = opto_slang_error
    {
        let mut converted = hdl_diagnostics(diagnostics);
        let primary = converted
            .iter()
            .position(|diagnostic| diagnostic.severity() == opto_core::DiagnosticSeverity::Error)
            .unwrap_or(0);
        let mut diagnostic = converted.swap_remove(primary);
        for additional in converted {
            if let Some(label) = additional.primary().cloned() {
                diagnostic = diagnostic.with_related(label);
            }
            diagnostic = diagnostic.with_note(format!(
                "additional {} [{}]: {}",
                match additional.severity() {
                    opto_core::DiagnosticSeverity::Warning => "warning",
                    opto_core::DiagnosticSeverity::Error => "error",
                },
                additional.code(),
                additional.title(),
            ));
        }
        return diagnostic;
    }
    match error {
        opto_hdl::HdlError::UnsupportedConstruct(message) => {
            Diagnostic::new("OPT-HDL-003", message).with_help(
                "rewrite the construct using the supported synthesizable SystemVerilog subset",
            )
        }
        opto_hdl::HdlError::InvalidModel(message) => Diagnostic::new("OPT-HDL-002", message),
        _ => Diagnostic::new("OPT-HDL-001", error.to_string()),
    }
}

fn format_error_diagnostic(error: &opto_formats::FormatError) -> Diagnostic {
    match error {
        opto_formats::FormatError::InvalidDesign(_) => {
            Diagnostic::new("OPT-FMT-001", error.to_string())
        }
        opto_formats::FormatError::Unsupported(_) => {
            Diagnostic::new("OPT-FMT-002", error.to_string()).with_help(
                "choose a supported output representation or simplify the unsupported construct",
            )
        }
        opto_formats::FormatError::Capacity(_) => {
            Diagnostic::new("OPT-FMT-003", error.to_string())
        }
        opto_formats::FormatError::Spef { line, .. } => {
            Diagnostic::new("OPT-FMT-100", error.to_string()).with_note(format!(
                "the invalid SPEF record is on one-based input line {line}"
            ))
        }
        opto_formats::FormatError::Io(_) => Diagnostic::new("OPT-FMT-004", error.to_string()),
        opto_formats::FormatError::Word(_) | opto_formats::FormatError::Mapped(_) => {
            Diagnostic::new("OPT-FMT-900", error.to_string()).with_help(
                "retain the input design and diagnostic code when reporting this internal serialization failure",
            )
        }
    }
}
