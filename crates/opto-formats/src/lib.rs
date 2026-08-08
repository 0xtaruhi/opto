// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic readers, writers, and reports for synthesis artifacts.
//!
//! Writers emit structural Verilog, parse SPEF parasitics, and build canonical
//! reports from sealed Opto data. Formatters do not mutate session state or run
//! synthesis or timing analysis implicitly; callers supply the analyzed model
//! and report context.
//!
//! Output order is stable across runs. Names, hierarchy rows, and aggregate
//! categories are emitted from typed IDs or ordered maps rather than hash-table
//! iteration, which keeps reports suitable for regression baselines.

use std::collections::BTreeMap;

mod document;
mod error;
mod power;
mod reports;
mod resources;
mod spef;
mod timing;
mod verilog;

pub use document::{MessageKind, ReportBlock, ReportDocument, ReportField, ReportTable};
pub use power::{PowerReportKind, ReportPowerOptions, report_power};
pub use reports::{
    report_area, report_hierarchical_mapped_area, report_hierarchical_mapped_qor,
    report_mapped_area, report_mapped_qor, report_qor, report_timestamp,
};
pub use resources::{ResourceImplementationEntry, ResourceReportEntry, report_resources};
pub use spef::{
    Spef, SpefCapacitor, SpefConnection, SpefConnectionKind, SpefDirection, SpefNet, SpefResistor,
    parse_spef,
};
pub use timing::{report_clock, report_parasitic_annotations, report_timing, report_timing_checks};
pub use verilog::{write_mapped_verilog, write_verilog};

/// Library metadata required to classify and total mapped-cell area.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AreaReportContext {
    /// Area in library units, indexed by exact target-cell name.
    pub library_cell_area: BTreeMap<String, f64>,
    /// Reporting category indexed by exact target-cell name.
    pub library_cell_kind: BTreeMap<String, AreaCellKind>,
    /// Ordered source libraries represented in the report header.
    pub libraries: Vec<AreaLibrary>,
}

/// Name and source path of one library contributing to an area report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaLibrary {
    /// Liberty `library(...)` name.
    pub name: String,
    /// User-visible source path.
    pub source: String,
}

/// Mutually exclusive area-report classification for a target cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaCellKind {
    /// Combinational logic other than a pure buffer or inverter.
    Combinational,
    /// Buffer or inverter cell.
    BufferInverter,
    /// Flip-flop, latch, or other state-holding cell.
    Sequential,
    /// Hard macro or cell outside the logic categories above.
    Macro,
}
pub use error::FormatError;

#[cfg(test)]
mod snapshot_tests;
