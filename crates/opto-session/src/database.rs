// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Session, SessionError, SynthesisDirectiveKind};
use opto_synth::SynthesisEffort;
use std::path::PathBuf;

impl Session {
    /// Return the HDL source search path in deterministic lookup order.
    pub fn hdl_search_path(&self) -> &[PathBuf] {
        &self.state.settings.hdl_search_path
    }

    /// Atomically replace the HDL source search path.
    pub fn set_hdl_search_path(&mut self, paths: Vec<PathBuf>) -> usize {
        replace_value(&mut self.state.settings.hdl_search_path, paths)
    }

    /// Return the Liberty and parasitic search path in deterministic lookup order.
    pub fn lib_search_path(&self) -> &[PathBuf] {
        &self.state.settings.lib_search_path
    }

    /// Atomically replace the Liberty and parasitic search path.
    pub fn set_lib_search_path(&mut self, paths: Vec<PathBuf>) -> usize {
        replace_value(&mut self.state.settings.lib_search_path, paths)
    }

    /// Return the configured synthesis effort.
    pub fn synth_effort(&self) -> SynthesisEffort {
        self.state.settings.synth_effort
    }

    /// Atomically replace the synthesis effort.
    pub fn set_synth_effort(&mut self, effort: SynthesisEffort) -> usize {
        replace_value(&mut self.state.settings.synth_effort, effort)
    }

    /// Return whether clock-gating insertion is enabled for synthesis.
    pub fn clock_gating_enabled(&self) -> bool {
        self.state.settings.clock_gating
    }

    /// Atomically enable or disable clock-gating insertion.
    pub fn set_clock_gating_enabled(&mut self, enabled: bool) -> usize {
        replace_value(&mut self.state.settings.clock_gating, enabled)
    }

    /// Return the minimum register-bank width eligible for clock gating.
    pub fn clock_gating_minimum_bitwidth(&self) -> usize {
        self.state.settings.clock_gating_style.minimum_bitwidth
    }

    /// Set the minimum register-bank width eligible for clock gating.
    pub fn set_clock_gating_minimum_bitwidth(
        &mut self,
        minimum_bitwidth: usize,
    ) -> Result<usize, SessionError> {
        if minimum_bitwidth == 0 {
            return Err(SessionError::state(
                "set_db: clock_gating_minimum_bitwidth must be at least 1",
            ));
        }
        Ok(replace_value(
            &mut self.state.settings.clock_gating_style.minimum_bitwidth,
            minimum_bitwidth,
        ))
    }

    /// Return whether inserted clock gates use a latch-based enable.
    pub fn clock_gating_latch_based(&self) -> bool {
        self.state.settings.clock_gating_style.latch_based
    }

    /// Configure whether inserted clock gates use a latch-based enable.
    pub fn set_clock_gating_latch_based(&mut self, latch_based: bool) -> usize {
        replace_value(
            &mut self.state.settings.clock_gating_style.latch_based,
            latch_based,
        )
    }

    /// Apply a mutable schema-declared property to a typed object list.
    pub fn set_db_object_property(
        &mut self,
        objects: &str,
        property: &str,
        value: bool,
    ) -> Result<usize, SessionError> {
        let kind = match property {
            "dont_touch" => SynthesisDirectiveKind::DontTouch,
            "ungroup" => SynthesisDirectiveKind::Ungroup,
            other => {
                return Err(SessionError::state(format!(
                    "set_db: object property '.{other}' is unknown or read-only"
                )));
            }
        };
        self.set_synthesis_directive("set_db", objects, kind, value)
    }

    /// Return loaded Liberty names in publication order.
    pub fn library_names(&self) -> Vec<String> {
        self.process
            .libraries
            .current()
            .libraries()
            .into_iter()
            .map(|library| library.name)
            .collect()
    }

    /// Return loaded Liberty names matching a shell-style pattern.
    pub fn library_names_matching(&self, pattern: &str) -> Vec<String> {
        self.library_names()
            .into_iter()
            .filter(|name| opto_db::matches_pattern(name, pattern))
            .collect()
    }

    /// Return effective target-cell names matching a shell-style pattern.
    pub fn library_cell_names_matching(&self, pattern: &str) -> Result<Vec<String>, SessionError> {
        Ok(self
            .synthesis_options()?
            .target_cells
            .iter()
            .filter(|cell| opto_db::matches_pattern(cell.name(), pattern))
            .map(|cell| cell.name().to_string())
            .collect())
    }

    /// Mark named or patterned Liberty cells unavailable to synthesis.
    pub fn set_library_cells_dont_use(
        &mut self,
        patterns: &[String],
    ) -> Result<usize, SessionError> {
        if patterns.is_empty() {
            return Ok(0);
        }
        self.mark_library_cells_unavailable(patterns).map(|_| 1)
    }
}

fn replace_value<T: PartialEq>(slot: &mut T, value: T) -> usize {
    if *slot == value {
        0
    } else {
        *slot = value;
        1
    }
}
