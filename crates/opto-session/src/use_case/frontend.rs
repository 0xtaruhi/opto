// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{DesignRecord, MappedObjectIndex, Session, build_object_index, transaction};
use opto_hdl::{DbUpdate, Frontend, FrontendOptions, VerilogSourceSet};
use opto_runtime::{Task, TaskKey};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
/// Process-local source units waiting for elaboration.
pub struct HdlCatalog {
    pub(crate) definitions: BTreeSet<String>,
    pub(crate) packages: BTreeSet<String>,
    pub(crate) verilog_units: Vec<VerilogSourceSet>,
}

impl HdlCatalog {
    pub(crate) fn from_designs(designs: &crate::DesignStore) -> Self {
        Self {
            definitions: designs.keys().cloned().collect(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CurrentDesignPolicy {
    ElaboratedTop,
    FirstImported,
}
impl Session {
    /// Parse `SystemVerilog` source batches without elaborating a top design.
    pub fn read_hdl(
        &mut self,
        files: &[PathBuf],
        frontend: &FrontendOptions,
    ) -> Result<String, crate::SessionError> {
        if files.is_empty() {
            return Err(crate::SessionError::state("read_hdl: no input files"));
        }
        let resolved = files
            .iter()
            .map(|file| {
                if file.is_absolute() || file.exists() {
                    return Ok(file.clone());
                }
                self.state
                    .settings
                    .hdl_search_path
                    .iter()
                    .map(|directory| directory.join(file))
                    .find(|candidate| candidate.exists())
                    .ok_or_else(|| {
                        crate::SessionError::state(format!(
                            "read_hdl: '{}' was not found in hdl_search_path ({})",
                            file.display(),
                            self.state
                                .settings
                                .hdl_search_path
                                .iter()
                                .map(|path| path.display().to_string())
                                .collect::<Vec<_>>()
                                .join(" ")
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.ingest_verilog(&resolved, frontend)
    }

    /// Parse Verilog source units without elaborating a top design.
    pub fn ingest_verilog(
        &mut self,
        files: &[PathBuf],
        frontend: &FrontendOptions,
    ) -> Result<String, crate::SessionError> {
        let source_set = Frontend::ingest_verilog(files, frontend, &self.process.runtime)?;
        let next_revision = self.next_revision()?;
        self.state
            .hdl_catalog
            .definitions
            .extend(source_set.definitions().iter().cloned());
        self.state
            .hdl_catalog
            .packages
            .extend(source_set.packages().iter().cloned());
        self.state.hdl_catalog.verilog_units.push(source_set);
        self.state.revision = next_revision;
        Ok("1".to_string())
    }

    /// Import fully lowered Verilog modules for tests and internal tooling.
    pub fn import_verilog(
        &mut self,
        files: &[PathBuf],
        frontend: &FrontendOptions,
    ) -> Result<String, crate::SessionError> {
        let update = Frontend::read_verilog(files, frontend, &self.process.runtime)?;
        self.apply_db_update(update, CurrentDesignPolicy::FirstImported)
    }

    pub(crate) fn apply_db_update(
        &mut self,
        update: DbUpdate,
        current_policy: CurrentDesignPolicy,
    ) -> Result<String, crate::SessionError> {
        let DbUpdate { modules, top } = update;
        if modules.is_empty() {
            return Err(crate::SessionError::state("no designs loaded"));
        }

        let mut unique_names = BTreeSet::new();
        for module in &modules {
            let name = module.word().name();
            if !unique_names.insert(name.to_string()) {
                return Err(crate::SessionError::state(format!(
                    "frontend returned duplicate design '{name}'",
                )));
            }
        }
        let tasks = modules
            .into_iter()
            .enumerate()
            .map(|(ordinal, module)| Task::new(TaskKey::new(0, ordinal as u64), module))
            .collect::<Vec<_>>();
        let prepared = self.process.runtime.map_ordered(tasks, |module| {
            let design = build_object_index(&module)?;
            Ok::<_, crate::SessionError>((module.word().name().to_string(), module, design))
        })?;
        let next_revision = self.next_revision()?;
        let names = prepared
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect::<Vec<_>>();

        let changed_designs = prepared
            .iter()
            .filter(|(name, module, _)| {
                !self
                    .state
                    .designs
                    .get(name)
                    .is_some_and(|record| record.source == *module)
            })
            .map(|(_, _, design)| design.clone())
            .collect::<Vec<_>>();
        let changed_names = changed_designs
            .iter()
            .map(|design| design.name.as_str())
            .collect::<BTreeSet<_>>();
        let mut synthesis_detachments = changed_names
            .iter()
            .filter_map(|&name| {
                self.state.designs.get(name).map(|record| {
                    record
                        .prepare_synthesis_detach()
                        .map(|prepared| (name, prepared))
                })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        transaction::reconcile_source_objects(self, &changed_designs)?;

        for (name, module, design) in prepared {
            if changed_names.contains(name.as_str()) {
                let previous_incremental =
                    if let Some(prepared) = synthesis_detachments.remove(name.as_str()) {
                        let record = self
                            .state
                            .designs
                            .get_mut(&name)
                            .expect("prepared changed design still exists during commit");
                        record.commit_synthesis_detach(prepared);
                        record.incremental_snapshot.take()
                    } else {
                        None
                    };
                let mut record = DesignRecord::new(module, next_revision, design);
                record.incremental_snapshot = previous_incremental;
                self.state.designs.insert(name.clone(), record);
            }
            self.state.hdl_catalog.definitions.insert(name);
        }
        debug_assert!(synthesis_detachments.is_empty());

        match current_policy {
            CurrentDesignPolicy::ElaboratedTop => {
                if let Some(top) = top {
                    self.state.current_design = Some(top);
                } else if self.state.current_design.is_none() && names.len() == 1 {
                    self.state.current_design = names.first().cloned();
                }
            }
            CurrentDesignPolicy::FirstImported => {
                self.state.current_design = names.first().cloned();
            }
        }

        if !changed_names.is_empty() {
            self.state.last_synthesis = None;
        }
        self.state.revision = next_revision;
        self.clear_stale_analysis_generation();
        Ok(names.join(" "))
    }

    /// Elaborate a named design and make it current.
    pub fn elaborate(&mut self, design_name: &str) -> Result<String, crate::SessionError> {
        let contains_template = self.state.hdl_catalog.definitions.contains(design_name);
        let source_sets = self.state.hdl_catalog.verilog_units.clone();
        if !contains_template {
            return Err(crate::SessionError::state(format!(
                "elaborate: design '{design_name}' was not read"
            )));
        }
        if source_sets.iter().any(|source_set| {
            source_set
                .definitions()
                .iter()
                .any(|name| name == design_name)
        }) {
            let update =
                Frontend::elaborate_verilog(&source_sets, design_name, &self.process.runtime)?;
            self.apply_db_update(update, CurrentDesignPolicy::ElaboratedTop)?;
            return Ok("1".to_string());
        }
        self.set_current_design(design_name)?;
        Ok("1".to_string())
    }

    /// Return the current design name, if one is selected.
    pub fn current_design(&self) -> Option<&str> {
        self.state.current_design.as_deref()
    }

    /// Select an existing session design as current.
    pub fn set_current_design(&mut self, name: &str) -> Result<String, crate::SessionError> {
        let mapped_index = {
            let record = self.state.designs.get(name).ok_or_else(|| {
                crate::SessionError::state(format!("set_db: design '{name}' not found"))
            })?;
            if record.mapped_object_index.is_some() {
                None
            } else {
                record
                    .synthesized
                    .as_ref()
                    .map(|synthesis| {
                        MappedObjectIndex::new(synthesis.mapped(), &self.process.runtime)
                    })
                    .transpose()?
            }
        };
        if let Some(index) = mapped_index {
            transaction::activate_stored_mapped_objects(self, name, index)?;
        }
        self.state.current_design = Some(name.to_string());
        self.clear_stale_analysis_generation();
        Ok(name.to_string())
    }
}
