// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_core::{Diagnostic, DiagnosticSource};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug)]
#[doc(hidden)]
pub struct ErrorSource {
    pub(crate) name: String,
    pub(crate) text: String,
    pub(crate) line: usize,
    pub(crate) column: Option<usize>,
    pub(crate) length: usize,
}

/// Product-shell failure with enough structure for source diagnostics.
#[derive(Debug, Error)]
pub enum ShellError {
    /// Command semantic error without attached source coordinates.
    #[error("{0}")]
    Command(String),
    /// Invalid command invocation with an actionable route to command help.
    #[error("{message}")]
    Usage {
        /// User-facing argument or option error.
        message: String,
        /// Concrete next step or spelling suggestion.
        help: String,
    },
    /// Command error annotated with source text and location.
    #[error("{message}")]
    Source {
        /// User-facing diagnostic message.
        message: String,
        /// Source buffer and coordinates used by the renderer.
        context: ErrorSource,
    },
    /// A typed subsystem error annotated with the Tcl invocation that triggered it.
    #[error("{source}")]
    Invocation {
        /// Subsystem failure raised while executing the invocation.
        #[source]
        source: Box<ShellError>,
        /// Tcl source buffer and coordinates of the invocation.
        context: ErrorSource,
    },
    /// Typed command-line or Tcl argument parsing failure.
    #[error("{0}")]
    Parse(String),
    /// File operation whose path is already the primary context.
    #[error("{path}: {source}")]
    Io {
        /// Path the shell attempted to access.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// File operation with an explicit action description.
    #[error("{operation} '{}': {source}", path.display())]
    FileIo {
        /// Human-readable operation being attempted.
        operation: &'static str,
        /// Path the operation attempted to access.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Terminal or standard-stream I/O failure.
    #[error("{action}: {source}")]
    Output {
        /// Output action being attempted.
        action: &'static str,
        /// Underlying stream failure.
        #[source]
        source: std::io::Error,
    },
    /// Current working directory could not be queried.
    #[error("failed to determine current directory: {0}")]
    CurrentDirectory(#[source] std::io::Error),
    /// Interactive line-editor failure.
    #[error("interactive line editor: {0}")]
    LineEditor(#[from] reedline::ReedlineError),
    /// Text passed to a C API contains an embedded NUL byte.
    #[error("{context}: text contains an embedded NUL byte")]
    Nul {
        /// C API input being constructed.
        context: &'static str,
        #[source]
        /// Position of the embedded NUL byte.
        source: std::ffi::NulError,
    },
    /// Tcl returned bytes that are not valid UTF-8.
    #[error("Tcl returned non-UTF-8 text")]
    Utf8(#[source] std::str::Utf8Error),
    /// Embedded Tcl runtime failure.
    #[error("{0}")]
    Tcl(#[from] opto_tcl_sys::TclError),
    /// Session operation failure outside a named Tcl command.
    #[error("{0}")]
    Session(#[from] opto_session::SessionError),
    /// Session failure annotated with the public Tcl command name.
    #[error("{command}: {source}")]
    SessionCommand {
        /// Public Tcl command reporting the failure.
        command: String,
        #[source]
        /// Underlying session failure.
        source: Box<opto_session::SessionError>,
    },
}

impl ShellError {
    pub(crate) fn command(message: impl Into<String>) -> Self {
        Self::Command(message.into())
    }

    pub(crate) fn usage(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Usage {
            message: message.into(),
            help: help.into(),
        }
    }

    pub(crate) fn parse(message: impl Into<String>) -> Self {
        Self::Parse(message.into())
    }

    pub(crate) fn with_source(
        self,
        name: impl Into<String>,
        text: impl Into<String>,
        line: usize,
    ) -> Self {
        let context = ErrorSource {
            name: name.into(),
            text: text.into(),
            line,
            column: None,
            length: 1,
        };
        match self {
            Self::Command(message) => Self::Source { message, context },
            Self::Source { .. } | Self::Invocation { .. } => self,
            source => Self::Invocation {
                source: Box::new(source),
                context,
            },
        }
    }

    pub(crate) fn source_context(&self) -> Option<&ErrorSource> {
        match self {
            Self::Source { context, .. } => Some(context),
            _ => None,
        }
    }

    pub(crate) fn invocation_context(&self) -> Option<&ErrorSource> {
        match self {
            Self::Invocation { context, .. } => Some(context),
            _ => None,
        }
    }
}

impl DiagnosticSource for ShellError {
    fn diagnostic(&self) -> Option<Diagnostic> {
        match self {
            Self::Usage { message, help } => {
                Some(Diagnostic::new("OPT-CLI-001", message).with_help(help))
            }
            Self::Invocation { source, .. } => source.diagnostic(),
            Self::Session(source) => source.diagnostic(),
            Self::SessionCommand { source, .. } => source.diagnostic(),
            _ => None,
        }
    }
}
