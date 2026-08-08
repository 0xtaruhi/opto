// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::Session;
use opto_library::{LibraryLinkPlan, LibrarySelection, read_lib_input};
use opto_runtime::{Task, TaskKey};
use opto_synth::SynthesisOptions;
use opto_timing::TimingLibrary;
use std::path::{Path, PathBuf};

impl Session {
    /// Parse and atomically publish Liberty libraries.
    ///
    /// Inputs are resolved through the session search path and parsed in
    /// deterministic task order. Existing libraries remain active if any
    /// parse or publication step fails.
    pub fn read_libs(&mut self, files: &[PathBuf]) -> Result<String, crate::SessionError> {
        let resolved = self.resolve_read_lib_inputs(files)?;
        if resolved.is_empty() {
            return Err(crate::SessionError::state("read_libs: no input files"));
        }
        let tasks = resolved
            .into_iter()
            .enumerate()
            .map(|(ordinal, path)| -> Result<_, crate::SessionError> {
                let ordinal = u64::try_from(ordinal).map_err(|_| {
                    crate::SessionError::capacity("read_libs: input task key overflow")
                })?;
                Ok(Task::new(TaskKey::new(4, ordinal), path))
            })
            .collect::<Result<Vec<_>, crate::SessionError>>()?;
        let outputs = self.process.runtime.map_ordered(tasks, |path| {
            read_lib_input(&path).map_err(crate::SessionError::Library)
        })?;
        let libraries = outputs;
        let mut next_libraries = self.process.libraries.clone();
        let report = next_libraries.append(libraries)?;
        self.publish_libraries(next_libraries);
        self.clear_stale_analysis_generation();

        let advanced =
            (report.timing_models.ccs != 0 || report.timing_models.ecsm != 0).then(|| {
                format!(
                    "; timing models: {} NLDM, {} CCS, {} ECSM",
                    report.timing_models.nldm, report.timing_models.ccs, report.timing_models.ecsm
                )
            });
        Ok(format!(
            "Loaded {} Liberty libraries ({} cells, {} pins){}",
            report.libraries,
            report.cells,
            report.pins,
            advanced.as_deref().unwrap_or("")
        ))
    }

    /// Return the number of active Liberty library blocks.
    pub fn liberty_library_count(&self) -> usize {
        self.process.libraries.current().library_count()
    }

    /// Mark target cells matching any pattern unavailable to synthesis.
    pub fn mark_library_cells_unavailable(
        &mut self,
        patterns: &[String],
    ) -> Result<String, crate::SessionError> {
        if patterns.is_empty() {
            return Err(crate::SessionError::state(
                "set_db: at least one library cell pattern is required",
            ));
        }
        let mut next_libraries = self.process.libraries.clone();
        let (matched, changed) = next_libraries
            .set_dont_use(patterns)
            .map_err(crate::SessionError::Library)?;
        if matched == 0 {
            return Err(crate::SessionError::state(format!(
                "set_db: no usable library cells match '{}'",
                patterns.join(" ")
            )));
        }
        if changed != 0 {
            self.publish_libraries(next_libraries);
            self.clear_stale_analysis_generation();
        }
        Ok("1".to_string())
    }

    fn publish_libraries(&mut self, libraries: opto_library::LibraryStore) {
        self.process.libraries = libraries;
    }

    fn resolve_read_lib_inputs(
        &self,
        files: &[PathBuf],
    ) -> Result<Vec<PathBuf>, crate::SessionError> {
        if files.is_empty() {
            return Ok(Vec::new());
        }

        files
            .iter()
            .map(|file| self.resolve_lib_search_path_file("read_libs", file))
            .collect()
    }

    pub(super) fn resolve_lib_search_path_file(
        &self,
        command: &str,
        file: &Path,
    ) -> Result<PathBuf, crate::SessionError> {
        if file.is_absolute() || file.exists() {
            return Ok(file.to_path_buf());
        }

        for dir in &self.state.settings.lib_search_path {
            let candidate = dir.join(file);
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        Err(crate::SessionError::state(format!(
            "{command}: '{}' was not found in lib_search_path ({})",
            file.display(),
            self.state
                .settings
                .lib_search_path
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(" ")
        )))
    }

    pub(crate) fn synthesis_options(&self) -> Result<SynthesisOptions, crate::SessionError> {
        Ok(SynthesisOptions {
            target_cells: self.active_mapping_library_cells()?,
        })
    }

    fn active_mapping_library_cells(
        &self,
    ) -> Result<opto_library::TargetCellSet, crate::SessionError> {
        let selection = self.mapping_library_selection();
        if selection.is_empty() {
            return Ok(opto_library::TargetCellSet::default());
        }

        self.process
            .libraries
            .current()
            .target_cells(&selection)
            .map_err(Into::into)
    }

    pub(crate) fn active_link_plan(&self) -> Result<LibraryLinkPlan, crate::SessionError> {
        let selection = self.resolution_library_selection();
        Ok(self.process.libraries.current().link_plan(&selection)?)
    }

    pub(crate) fn timing_library(&self) -> Result<TimingLibrary, crate::SessionError> {
        let selection = self.resolution_library_selection();
        if selection.is_empty() {
            return Ok(TimingLibrary::default());
        }
        self.process
            .libraries
            .current()
            .timing_library(&selection)
            .map_err(Into::into)
    }

    pub(crate) fn resolution_library_selection(&self) -> LibrarySelection {
        self.process.libraries.current().all_libraries(true)
    }

    pub(crate) fn mapping_library_selection(&self) -> LibrarySelection {
        self.process.libraries.current().all_libraries(false)
    }
}
