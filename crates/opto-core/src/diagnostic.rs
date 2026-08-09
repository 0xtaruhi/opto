// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Structured, presentation-independent diagnostics shared across Opto layers.

/// One source coordinate attached to a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticLocation {
    path: String,
    line: u32,
    column: Option<u32>,
    length: u32,
}

impl DiagnosticLocation {
    /// Creates a one-character source location.
    pub fn new(path: impl Into<String>, line: u32, column: Option<u32>) -> Self {
        Self {
            path: path.into(),
            line: line.max(1),
            column,
            length: 1,
        }
    }

    /// Returns the source path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the one-based source line.
    #[must_use]
    pub fn line(&self) -> u32 {
        self.line
    }

    /// Returns the optional one-based source column.
    #[must_use]
    pub fn column(&self) -> Option<u32> {
        self.column
    }

    /// Returns the highlighted character count.
    #[must_use]
    pub fn length(&self) -> u32 {
        self.length
    }
}

/// A source location and the reason it participates in a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticLabel {
    location: DiagnosticLocation,
    message: String,
}

impl DiagnosticLabel {
    /// Creates a labeled source location.
    pub fn new(location: DiagnosticLocation, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }

    /// Returns the source coordinate.
    #[must_use]
    pub fn location(&self) -> &DiagnosticLocation {
        &self.location
    }

    /// Returns the source-facing explanation.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A stable, structured error report independent of terminal or GUI rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    code: &'static str,
    title: String,
    primary: Option<DiagnosticLabel>,
    related: Vec<DiagnosticLabel>,
    notes: Vec<String>,
    help: Vec<String>,
}

impl Diagnostic {
    /// Creates a diagnostic with a stable searchable code and concise title.
    pub fn new(code: &'static str, title: impl Into<String>) -> Self {
        Self {
            code,
            title: title.into(),
            primary: None,
            related: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
        }
    }

    /// Sets the source location that should receive primary visual emphasis.
    #[must_use]
    pub fn with_primary(mut self, label: DiagnosticLabel) -> Self {
        self.primary = Some(label);
        self
    }

    /// Adds a source location that explains another part of the failure.
    #[must_use]
    pub fn with_related(mut self, label: DiagnosticLabel) -> Self {
        self.related.push(label);
        self
    }

    /// Adds explanatory context that is not tied to one source coordinate.
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Adds an actionable remediation suggestion.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Returns the concise primary message.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the primary source label, if the failing layer has source data.
    #[must_use]
    pub fn primary(&self) -> Option<&DiagnosticLabel> {
        self.primary.as_ref()
    }

    /// Returns related source labels in causal order.
    #[must_use]
    pub fn related(&self) -> &[DiagnosticLabel] {
        &self.related
    }

    /// Returns explanatory notes.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Returns remediation suggestions.
    #[must_use]
    pub fn help(&self) -> &[String] {
        &self.help
    }
}

/// Converts a typed subsystem error into a presentation-independent diagnostic.
pub trait DiagnosticSource {
    /// Returns a structured diagnostic when the error has user-facing context.
    fn diagnostic(&self) -> Option<Diagnostic>;
}
