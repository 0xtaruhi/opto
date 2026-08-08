// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    LibraryError, PowerCell, PowerLibraryUnits, TargetCellSet, TimingLibraryUnits,
    TimingModelCounts, WireLoadModel, liberty,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug)]
/// Canonical artifacts produced by parsing one Liberty library.
///
/// Most fields are crate-private so selection policy remains separate from
/// syntax parsing. [`Self::target_cells`] exposes the immutable cell view needed
/// by import consumers.
pub struct LibraryImport {
    pub(crate) name: String,
    pub(crate) source: String,
    pub(crate) default_operating_conditions: Option<String>,
    pub(crate) default_wire_load: Option<String>,
    pub(crate) default_wire_load_mode: Option<String>,
    pub(crate) wire_loads: BTreeMap<String, WireLoadModel>,
    pub(crate) units: TimingLibraryUnits,
    pub(crate) power_units: PowerLibraryUnits,
    pub(crate) target_cells: TargetCellSet,
    pub(crate) power_cells: Arc<[PowerCell]>,
    pub(crate) timing_models: TimingModelCounts,
    pub(crate) cell_count: usize,
    pub(crate) pin_count: usize,
}

impl LibraryImport {
    /// Borrow the parsed target cells in Liberty declaration order.
    #[must_use]
    pub fn target_cells(&self) -> &TargetCellSet {
        &self.target_cells
    }
}

/// Reads and parses one `.lib` file.
///
/// # Errors
///
/// Returns [`LibraryError::UnsupportedInput`] for non-`.lib` paths, or a
/// read/parse/validation error for invalid input.
pub fn read_lib_input(path: &Path) -> Result<LibraryImport, LibraryError> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("lib") {
        return Err(LibraryError::UnsupportedInput {
            path: path.to_path_buf(),
        });
    }
    let text = fs::read_to_string(path).map_err(|source| LibraryError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    liberty::parse_liberty(&text, &path.display().to_string())
}

/// Parse an in-memory Liberty source.
///
/// `source` is used in diagnostics and should identify the caller's input.
///
/// # Errors
///
/// Returns [`LibraryError`] for invalid Liberty syntax, unsupported constructs,
/// inconsistent units/models, or target-cell arena capacity and semantic
/// validation failures.
pub fn parse_liberty(text: &str, source: &str) -> Result<LibraryImport, LibraryError> {
    liberty::parse_liberty(text, source)
}

#[cfg(test)]
mod tests;
