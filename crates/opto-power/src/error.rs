// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_core::{Diagnostic, DiagnosticSource};
use thiserror::Error;

#[derive(Debug, Error)]
/// Failure to construct or update a power analysis.
pub enum PowerError {
    /// Parallel runtime failed.
    #[error("report_power: runtime error: {0}")]
    Runtime(#[from] opto_runtime::RuntimeError),
    /// Required Liberty unit metadata is absent.
    #[error("report_power: Liberty is missing '{attribute}'")]
    MissingLibraryUnit {
        /// Missing Liberty attribute.
        attribute: &'static str,
    },
    /// A timing instance has no matching synthesis target cell.
    #[error("report_power: Liberty cell '{cell}' has no target-cell definition")]
    MissingTargetCell {
        /// Missing cell name.
        cell: String,
    },
    /// A characterized pin is not connected in the instance.
    #[error("report_power: cell '{cell}' pin '{pin}' is not connected")]
    MissingPinConnection {
        /// Instance or cell name.
        cell: String,
        /// Missing pin name.
        pin: String,
    },
    /// Exact Boolean evaluation would exceed its bounded input count.
    #[error("report_power: Boolean function has {inputs} inputs; the exact limit is {limit}")]
    FunctionInputLimit {
        /// Actual input count.
        inputs: usize,
        /// Supported exact-evaluation limit.
        limit: usize,
    },
    /// A Liberty Boolean function references no known pin.
    #[error("report_power: Boolean function references unknown pin '{pin}'")]
    UnknownFunctionPin {
        /// Unknown pin name.
        pin: String,
    },
    /// Switching activity violates probability or rate invariants.
    #[error("report_power: invalid switching activity: {detail}")]
    InvalidActivity {
        /// Violated probability, ratio, finiteness, or rate condition.
        detail: String,
    },
    /// The combinational activity graph contains a cycle.
    #[error("report_power: combinational activity graph contains a cycle")]
    ActivityPropagationCycle,
    /// Model, timing state, and annotations have different generations.
    #[error("report_power: timing, activity, and power inputs belong to different generations")]
    GenerationMismatch,
    /// Timing-net rows are missing, duplicated, or out of dense order.
    #[error("report_power: timing net state is incomplete or out of order at net {net}")]
    InvalidTimingNetState {
        /// First invalid net ID.
        net: u32,
    },
    /// An instance's typed pin/net bindings are inconsistent.
    #[error("report_power: timing instance '{instance}' has invalid typed net bindings")]
    InvalidInstanceBindings {
        /// Invalid instance name.
        instance: String,
    },
    /// A combinational net has multiple activity drivers.
    #[error("report_power: net {net} has multiple combinational drivers")]
    MultipleNetDrivers {
        /// Multiply driven timing-net ID.
        net: u32,
    },
    /// Duplicate explicit annotations disagree.
    #[error("report_power: net {net} has conflicting switching-activity annotations")]
    ConflictingActivityAnnotation {
        /// Conflicting timing-net ID.
        net: u32,
    },
    /// A compact arena exceeded its addressable capacity.
    #[error("report_power: {resource} exceeds 32-bit capacity")]
    Capacity {
        /// Compact allocation or index whose checked limit was exceeded.
        resource: &'static str,
    },
    /// Shared cache state was poisoned by a panic.
    #[error("report_power: engine state is poisoned")]
    EnginePoisoned,
    /// A monotonic cache metric overflowed.
    #[error("report_power: metric '{metric}' overflowed")]
    MetricOverflow {
        /// Monotonic counter that could not represent the next update.
        metric: &'static str,
    },
}

impl DiagnosticSource for PowerError {
    fn diagnostic(&self) -> Option<Diagnostic> {
        let internal = matches!(
            self,
            Self::GenerationMismatch
                | Self::InvalidTimingNetState { .. }
                | Self::InvalidInstanceBindings { .. }
                | Self::EnginePoisoned
                | Self::MetricOverflow { .. }
        );
        let mut diagnostic = Diagnostic::new(
            if internal {
                "OPT-PWR-900"
            } else {
                "OPT-PWR-001"
            },
            self.to_string(),
        );
        if internal {
            diagnostic = diagnostic.with_help(
                "retain the design and diagnostic code when reporting this internal power-analysis failure",
            );
        }
        Some(diagnostic)
    }
}
