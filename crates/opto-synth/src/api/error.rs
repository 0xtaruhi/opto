// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_core::{Diagnostic, DiagnosticLabel, DiagnosticLocation, DiagnosticSource};
use opto_ir::word;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use thiserror::Error;

const COMBINATIONAL_CYCLE_CODE: &str = "OPT-SYN-001";
const MAX_RENDERED_CYCLE_LOCATIONS: usize = 6;
const MAX_RENDERED_CYCLE_NODES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
/// One value on a combinational feedback path.
pub struct CombinationalCycleNode {
    description: String,
    source: word::SourceSpan,
}

impl CombinationalCycleNode {
    /// Creates one node on a reported feedback path.
    pub fn new(description: impl Into<String>, source: word::SourceSpan) -> Self {
        Self {
            description: description.into(),
            source,
        }
    }

    /// Returns the source-facing value or operation description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the HDL source span associated with this path node.
    #[must_use]
    pub fn source(&self) -> &word::SourceSpan {
        &self.source
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("combinational loop detected in module '{module}'")]
/// A pure combinational dependency path that returns to its starting value.
pub struct CombinationalCycle {
    module: String,
    region: Option<u32>,
    nodes: Box<[CombinationalCycleNode]>,
    debug_values: Box<[word::ValueId]>,
}

impl CombinationalCycle {
    /// Creates a structured combinational-cycle error.
    pub fn new(
        module: impl Into<String>,
        region: u32,
        nodes: Vec<CombinationalCycleNode>,
        debug_values: Vec<word::ValueId>,
    ) -> Self {
        Self {
            module: module.into(),
            region: Some(region),
            nodes: nodes.into_boxed_slice(),
            debug_values: debug_values.into_boxed_slice(),
        }
    }

    /// Creates a cycle detected at the normalized Word IR boundary, before
    /// synthesis regions exist.
    pub(crate) fn after_normalization(
        module: impl Into<String>,
        nodes: Vec<CombinationalCycleNode>,
        debug_values: Vec<word::ValueId>,
    ) -> Self {
        Self {
            module: module.into(),
            region: None,
            nodes: nodes.into_boxed_slice(),
            debug_values: debug_values.into_boxed_slice(),
        }
    }

    /// Returns the elaborated module containing the loop.
    #[must_use]
    pub fn module(&self) -> &str {
        &self.module
    }

    /// Returns the stable synthesis-region row involved in detection, or
    /// `None` when the cycle was found before region construction.
    #[must_use]
    pub fn region(&self) -> Option<u32> {
        self.region
    }

    /// Returns the path nodes in dependency order.
    #[must_use]
    pub fn nodes(&self) -> &[CombinationalCycleNode] {
        &self.nodes
    }

    /// Returns internal value identities for debug-only inspection.
    #[must_use]
    pub fn debug_values(&self) -> &[word::ValueId] {
        &self.debug_values
    }

    /// Formats the stable, source-facing dependency path without exposing
    /// arena-local value identities.
    #[must_use]
    pub fn path_description(&self) -> String {
        let descriptions = self
            .nodes
            .iter()
            .take(MAX_RENDERED_CYCLE_NODES)
            .map(|node| node.description.as_str())
            .collect::<Vec<_>>();
        let omitted = self.nodes.len().saturating_sub(descriptions.len());
        let first = descriptions.first().copied();
        let mut path = descriptions.join(" -> ");
        if omitted != 0 {
            write!(path, " -> <{omitted} nodes omitted>")
                .expect("writing to an owned String is infallible");
        }
        if let Some(first) = first {
            path.push_str(" -> ");
            path.push_str(first);
        }
        path
    }

    fn structured_diagnostic(&self) -> Diagnostic {
        let mut diagnostic = Diagnostic::new(
            COMBINATIONAL_CYCLE_CODE,
            format!("combinational loop detected in module '{}'", self.module),
        );
        diagnostic = match self.region {
            Some(region) => diagnostic.with_note(format!(
                "the loop was found while constructing synthesis region {region}"
            )),
            None => diagnostic.with_note(
                "the loop was found at the normalized Word IR boundary, before optimization or mapping",
            ),
        };
        diagnostic = diagnostic.with_note(format!(
            "ordered feedback path: {}",
            self.path_description()
        ));
        diagnostic = diagnostic.with_help(
            "break the feedback path with sequential state, or correct the unintended \
             combinational self-dependency",
        );
        let mut locations = BTreeSet::new();
        let mut rendered = 0usize;
        for node in &self.nodes {
            let Some(file) = node.source.file() else {
                continue;
            };
            let Some(line) = node.source.line() else {
                continue;
            };
            let key = (file.to_string(), line, node.source.column());
            if !locations.insert(key.clone()) {
                continue;
            }
            if rendered == MAX_RENDERED_CYCLE_LOCATIONS {
                continue;
            }
            let location = DiagnosticLocation::new(key.0, key.1, key.2);
            let label = DiagnosticLabel::new(
                location,
                if rendered == 0 {
                    format!("feedback path enters {}", node.description)
                } else {
                    format!("then passes through {}", node.description)
                },
            );
            diagnostic = if rendered == 0 {
                diagnostic.with_primary(label)
            } else {
                diagnostic.with_related(label)
            };
            rendered += 1;
        }
        let located = locations.len();
        if located > rendered {
            diagnostic = diagnostic.with_note(format!(
                "{} additional source locations are omitted from this report",
                located - rendered
            ));
        }
        diagnostic
    }
}

#[derive(Debug, Error)]
/// Failure returned while validating, lowering, mapping, or optimizing a design.
pub enum SynthError {
    /// A dependency walk found a pure combinational feedback loop.
    #[error(transparent)]
    CombinationalCycle(#[from] CombinationalCycle),
    /// Input IR or target data violates a required design contract.
    #[error("{0}")]
    InvalidDesign(String),
    /// The input requests a construct that synthesis does not implement.
    #[error("{0}")]
    Unsupported(String),
    /// A size calculation or typed-ID domain exceeds representable capacity.
    #[error("{0}")]
    Capacity(String),
    /// An internal or restored-artifact invariant is inconsistent.
    #[error("{0}")]
    Invariant(String),
    /// Technology mapping could not construct a legal implementation.
    #[error("{0}")]
    Mapping(String),
    /// Word IR construction or validation failed.
    #[error("{0}")]
    Word(#[from] opto_ir::word::WordError),
    /// Mapped-netlist construction or validation failed.
    #[error("{0}")]
    Mapped(#[from] opto_ir::mapped::MappedError),
    /// A typed IR value operation failed.
    #[error("{0}")]
    Value(#[from] opto_ir::ValueError),
    /// Boolean logic construction or validation failed.
    #[error("{0}")]
    Logic(#[from] opto_ir::logic::LogicError),
    /// Name interning or lookup failed.
    #[error("{0}")]
    Name(#[from] opto_ir::NameError),
    /// Linked-hierarchy elaboration failed in Word IR.
    #[error("hierarchy elaboration failed: {0}")]
    HierarchyIr(#[source] opto_ir::word::WordError),
    /// Runtime scheduling failed.
    #[error("{0}")]
    Runtime(#[from] opto_runtime::RuntimeError),
    /// Timing-model construction or analysis failed.
    #[error("{0}")]
    Timing(#[from] opto_timing::TimingError),
    /// Power-model construction or analysis failed in the injected owner.
    #[error("{0}")]
    Power(String),
    /// Target-library construction or validation failed.
    #[error("{0}")]
    Library(#[from] opto_library::LibraryError),
    /// A mapped-region edit conflicted with intervening mutations.
    #[error("{0}")]
    RegionConflict(#[from] opto_ir::mapped::RegionConflict),
    /// A committed mapped edit touched cells whose explicit owner is unknown.
    #[error("post-map edit touched mapped cells outside the ownership domain: {cells:?}")]
    UnknownMappedOwners {
        /// Mapped cell IDs outside the implementation ownership domain.
        cells: Box<[opto_ir::mapped::CellId]>,
    },
    /// A mutating operation failed and restoration of its prior state also
    /// failed.
    #[error("{operation} failed: {primary}; rollback also failed: {rollback}")]
    Rollback {
        /// Stable operation name used for diagnostics.
        operation: &'static str,
        /// Original failure that triggered rollback.
        #[source]
        primary: Box<SynthError>,
        /// Failure encountered while restoring the prior state.
        rollback: Box<SynthError>,
    },
}

impl DiagnosticSource for SynthError {
    #[allow(
        clippy::match_same_arms,
        reason = "the identical rendering arms bind different concrete error types"
    )]
    fn diagnostic(&self) -> Option<Diagnostic> {
        match self {
            Self::CombinationalCycle(cycle) => Some(cycle.structured_diagnostic()),
            Self::InvalidDesign(message) => Some(Diagnostic::new("OPT-SYN-002", message.clone())),
            Self::Unsupported(message) => Some(Diagnostic::new("OPT-SYN-003", message.clone())),
            Self::Capacity(message) => Some(Diagnostic::new("OPT-SYN-004", message.clone())),
            Self::Mapping(message) => Some(Diagnostic::new("OPT-SYN-100", message.clone())),
            Self::Timing(source) => Some(Diagnostic::new("OPT-SYN-200", source.to_string())),
            Self::Library(source) => Some(Diagnostic::new("OPT-SYN-300", source.to_string())),
            Self::Power(message) => Some(Diagnostic::new("OPT-SYN-400", message.clone())),
            Self::Runtime(source) => Some(Diagnostic::new("OPT-SYN-500", source.to_string())),
            Self::Invariant(message) => {
                Some(Diagnostic::new("OPT-SYN-900", message.clone()).with_help(
                    "this is an internal synthesis consistency failure; retain the input and \
                     diagnostic code when reporting it",
                ))
            }
            Self::Word(source) => Some(internal_diagnostic(source)),
            Self::Mapped(source) => Some(internal_diagnostic(source)),
            Self::Value(source) => Some(internal_diagnostic(source)),
            Self::Logic(source) => Some(internal_diagnostic(source)),
            Self::Name(source) => Some(internal_diagnostic(source)),
            Self::HierarchyIr(source) => Some(internal_diagnostic(source)),
            Self::RegionConflict(source) => Some(internal_diagnostic(source)),
            Self::UnknownMappedOwners { .. } => {
                Some(Diagnostic::new("OPT-SYN-902", self.to_string()).with_help(
                    "every post-map cell edit must carry explicit region or global ownership \
                     before regional plans can be invalidated safely",
                ))
            }
            Self::Rollback {
                operation,
                primary,
                rollback,
            } => Some(
                Diagnostic::new(
                    "OPT-SYN-901",
                    format!("{operation} failed: {primary}; rollback also failed: {rollback}"),
                )
                .with_help(
                    "the synthesis transaction could not restore its prior state; retain the \
                     input and diagnostic code when reporting it",
                ),
            ),
        }
    }
}

fn internal_diagnostic(source: &impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new("OPT-SYN-900", source.to_string()).with_help(
        "this is an internal synthesis consistency failure; retain the input and diagnostic code \
         when reporting it",
    )
}

impl SynthError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidDesign(message.into())
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub(crate) fn capacity(message: impl Into<String>) -> Self {
        Self::Capacity(message.into())
    }

    pub(crate) fn invariant(message: impl Into<String>) -> Self {
        Self::Invariant(message.into())
    }

    pub(crate) fn mapping(message: impl Into<String>) -> Self {
        Self::Mapping(message.into())
    }
}
