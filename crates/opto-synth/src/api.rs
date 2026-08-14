// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable library-facing requests, results, diagnostics, and reports.
//!
//! This domain owns the library-facing contract types assembled by the crate
//! root. Core synthesis domains may return these shared error and result types,
//! but they must not reach into orchestration in [`crate::engine`].

pub(crate) mod check;
pub(crate) mod diagnostics;
pub(crate) mod error;
pub(crate) mod types;

#[cfg(test)]
pub(crate) use check::target_cell_reference_ports;
pub use check::{CheckDesignError, ReferencePort, ReferencePortMap, check_design_with_references};
pub use error::{CombinationalCycle, CombinationalCycleNode, SynthError};
pub use types::{
    OptimizationPhase, StageId, SynthesisConfig, SynthesisDiagnostics, SynthesisEffort,
    SynthesisMetrics, SynthesisOptions, SynthesisProgress, SynthesisProgressStatus,
    SynthesisReport, SynthesisResult, SynthesisTimingProgress, TimingSummary,
};
