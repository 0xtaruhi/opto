// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl IncrementalTiming {
    /// Restores all timing layers captured by `edit`.
    ///
    /// # Errors
    ///
    /// Returns an error if topology restoration or page compaction fails.
    pub fn rollback(&mut self, edit: RegionEdit) -> Result<(), crate::TimingError> {
        // Propagation journals refer to the edited topology's appended slots,
        // so restore their values before truncating those slots.
        let previous_net_count = propagation_net_count(&self.model);
        let electrical_snapshot = edit.electrical_snapshot.clone();
        let changed_structure = edit.edit.changes_structure();
        self.model.rollback_instance_region(edit.edit)?;
        let restored_net_count = propagation_net_count(&self.model);
        restore_propagation(&mut self.propagation, edit.propagation);
        for _ in restored_net_count..previous_net_count {
            remove_last_propagation_net(&mut self.propagation);
        }
        self.closure.rollback(edit.closure);
        self.design_rules.rollback(edit.design_rules);
        for net in edit.required_dirty {
            if let Some(dirty) = self.required_dirty.get_mut(net) {
                *dirty = false;
            }
        }
        self.required_dirty.truncate(restored_net_count);
        self.region_edit_active = false;
        self.region_commit_prepared = false;
        if changed_structure {
            self.model.graph.ensure_topological_order()?;
            self.refresh_constraint_index();
        }
        self.model.prepare_instance_region_commit()?;
        self.closure.compact_rows()?;
        *self
            .electrical_snapshot
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = electrical_snapshot;
        Ok(())
    }

    /// Repack every dirty row page before an external publication commits.
    ///
    /// # Errors
    ///
    /// Returns [`crate::TimingEngineError::NoActiveRegionEdit`] without a live edit,
    /// or a model/closure capacity error if compaction cannot complete.
    pub fn prepare_commit(&mut self, _edit: &RegionEdit) -> Result<(), crate::TimingError> {
        if !self.region_edit_active {
            return Err(crate::TimingEngineError::NoActiveRegionEdit {
                operation: "prepare a region commit",
            }
            .into());
        }
        if self.region_commit_prepared {
            return Ok(());
        }
        self.model.prepare_instance_region_commit()?;
        self.closure.compact_rows()?;
        self.region_commit_prepared = true;
        Ok(())
    }

    /// Accepts an edit after [`Self::prepare_commit`] succeeds.
    ///
    /// # Panics
    ///
    /// Panics unless a region edit is active and its commit preparation has
    /// completed successfully.
    pub fn commit_prepared(&mut self, edit: RegionEdit) {
        assert!(
            self.region_edit_active && self.region_commit_prepared,
            "region commit must be active and successfully prepared"
        );
        self.model.commit_instance_region(edit.edit);
        self.closure.commit(edit.closure);
        self.region_edit_active = false;
        self.region_commit_prepared = false;
    }

    /// Prepares and accepts an edit without an external publication step.
    ///
    /// # Errors
    ///
    /// Returns a preparation failure after rolling the edit back. If both
    /// preparation and rollback fail, returns [`crate::TimingError::Rollback`] with
    /// both causes.
    pub fn commit(&mut self, edit: RegionEdit) -> Result<(), crate::TimingError> {
        if let Err(primary) = self.prepare_commit(&edit) {
            return match self.rollback(edit) {
                Ok(()) => Err(primary),
                Err(rollback) => Err(crate::TimingError::Rollback {
                    operation: "incremental timing commit",
                    primary: Box::new(primary),
                    rollback: Box::new(rollback),
                }),
            };
        }
        self.commit_prepared(edit);
        Ok(())
    }
}
