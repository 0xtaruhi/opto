// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Kind of syntax failure in a Liberty source.
pub enum LibrarySyntaxErrorKind {
    /// The lexer cannot recognize the current input.
    InvalidToken,
    /// Input ended before the expected construct.
    UnexpectedEnd {
        /// Human-readable description of the expected construct.
        expected: &'static str,
    },
    /// A different token appeared where a construct was expected.
    UnexpectedToken {
        /// Human-readable description of the expected construct.
        expected: &'static str,
        /// Human-readable description of the actual token.
        found: &'static str,
    },
}

impl std::fmt::Display for LibrarySyntaxErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidToken => formatter.write_str("invalid token"),
            Self::UnexpectedEnd { expected } => {
                write!(formatter, "expected {expected}, found end of file")
            }
            Self::UnexpectedToken { expected, found } => {
                write!(formatter, "expected {expected}, found {found}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Kind of syntax failure in a Liberty Boolean expression.
pub enum BooleanFunctionErrorKind {
    /// The expression ended before an operand was complete.
    UnexpectedEnd,
    /// A token is not valid at its position.
    UnexpectedToken,
    /// Valid expression text is followed by unused input.
    TrailingInput,
    /// The expression exceeds the supported nesting or node budget.
    ComplexityLimit,
}

impl std::fmt::Display for BooleanFunctionErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEnd => formatter.write_str("unexpected end of expression"),
            Self::UnexpectedToken => formatter.write_str("unexpected token"),
            Self::TrailingInput => formatter.write_str("trailing input"),
            Self::ComplexityLimit => formatter.write_str("expression complexity limit exceeded"),
        }
    }
}

#[derive(Debug, Error)]
/// Failure to read, parse, validate, or select Liberty library data.
pub enum LibraryError {
    /// Input is not a supported Liberty `.lib` file.
    #[error("read_lib: unsupported input '{}'; expected a Liberty .lib file", .path.display())]
    UnsupportedInput {
        /// Rejected input path.
        path: PathBuf,
    },
    /// The input file could not be read.
    #[error("{}: {source}", .path.display())]
    Read {
        /// Input path that could not be read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// The Liberty source is syntactically invalid.
    #[error("read_lib: {source_name}:{line}:{column}: {kind}")]
    Syntax {
        /// Diagnostic source name.
        source_name: String,
        /// One-based source line.
        line: usize,
        /// One-based source column.
        column: usize,
        /// Syntax failure kind.
        kind: LibrarySyntaxErrorKind,
    },
    /// A numeric attribute contains invalid text.
    #[error("read_lib: invalid number '{value}' for '{attribute}'")]
    InvalidNumber {
        /// Liberty attribute name.
        attribute: &'static str,
        /// Rejected numeric text.
        value: String,
    },
    /// A required attribute value is absent.
    #[error("read_lib: '{attribute}' requires a value")]
    MissingValue {
        /// Liberty attribute name.
        attribute: &'static str,
    },
    /// The parser encountered a recognized but unsupported construct.
    #[error("read_lib: unsupported Liberty construct '{construct}'")]
    UnsupportedConstruct {
        /// Unsupported construct description.
        construct: String,
    },
    /// A timing model violates its shape or numeric invariants.
    #[error("read_lib: invalid {model} timing model: {detail}")]
    InvalidTimingModel {
        /// Timing-model family.
        model: &'static str,
        /// Description of the violated invariant.
        detail: String,
    },
    /// A wire-load model is invalid.
    #[error("read_lib: invalid wire_load '{name}': {detail}")]
    InvalidWireLoad {
        /// Wire-load model name.
        name: String,
        /// Description of the violated invariant.
        detail: String,
    },
    /// Parsed cells cannot form a valid synthesis library.
    #[error("invalid synthesis library: {detail}")]
    InvalidSynthesisLibrary {
        /// Description of the violated synthesis invariant.
        detail: String,
    },
    /// A compact target-library arena exceeded its ID capacity.
    #[error("target-library {arena} arena exceeds the 32-bit ID capacity")]
    ArenaCapacity {
        /// Compact arena that exceeded its capacity.
        arena: &'static str,
    },
    /// One selector ambiguously names multiple loaded libraries.
    #[error("library selector '{selector}' matches multiple loaded libraries: {libraries}")]
    AmbiguousLibrarySelector {
        /// Selector that matched more than one library.
        selector: String,
        /// Diagnostic list of matches.
        libraries: String,
    },
    /// The host cannot represent the source's total pin count.
    #[error("read_lib: '{source_name}' exceeds the host pin-count capacity")]
    PinCountCapacity {
        /// Liberty source whose pin count is too large.
        source_name: String,
    },
    /// A parsed library has no declared library name.
    #[error("read_lib: '{source_name}' has no library name")]
    MissingLibraryName {
        /// Liberty source missing the required name.
        source_name: String,
    },
    /// A Liberty Boolean expression is invalid.
    #[error("invalid Liberty Boolean function '{expression}' at byte {offset}: {kind}")]
    BooleanFunction {
        /// Complete rejected expression.
        expression: String,
        /// Zero-based failing byte offset.
        offset: usize,
        /// Expression syntax failure kind.
        kind: BooleanFunctionErrorKind,
    },
    /// A library-store revision counter is exhausted.
    #[error(transparent)]
    Revision(#[from] opto_core::RevisionExhausted),
}
