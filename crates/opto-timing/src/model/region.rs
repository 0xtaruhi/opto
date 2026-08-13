// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! One transaction spanning timing design rows, graph topology, and mapped-net bindings.

use super::{
    MappedNetId, OwnedTimingInstance, TimingGeneration, TimingInstanceId, TimingModel, TimingNetId,
    TimingRegionDelta, TimingTopologyState, analysis, mapped_binding_capacity,
};
use std::collections::BTreeMap;

#[derive(Debug)]
/// Rollback journal for an applied instance-region model update.
///
/// Dense design rows may move through swap removal, so the journal records
/// positions as well as stable instance IDs and restores the bidirectional
/// position index in reverse operation order.
pub(crate) struct InstanceRegionModelEdit {
    journal: Vec<RegionInstanceOp>,
    mapped_nets: MappedNetModelEdit,
    graph: analysis::InstanceRegionGraphEdit,
    old_topology: TimingTopologyState,
}

#[derive(Debug)]
enum RegionInstanceOp {
    Replaced {
        id: TimingInstanceId,
        old: OwnedTimingInstance,
    },
    Removed {
        position: usize,
        old: OwnedTimingInstance,
    },
    Added {
        id: TimingInstanceId,
        position: usize,
    },
}

impl RegionInstanceOp {
    fn instance_id(&self) -> TimingInstanceId {
        match self {
            Self::Replaced { id, .. } | Self::Added { id, .. } => *id,
            Self::Removed { old, .. } => old.id,
        }
    }
}

impl InstanceRegionModelEdit {
    pub(crate) fn changes_structure(&self) -> bool {
        self.graph.changes_structure()
    }

    pub(crate) fn affected_instances(&self) -> impl Iterator<Item = TimingInstanceId> + '_ {
        self.journal.iter().map(RegionInstanceOp::instance_id)
    }
}

#[derive(Debug)]
struct MappedNetModelEdit {
    original_timing_len: usize,
    mapped_len_at_start: usize,
    timing_undo: BTreeMap<TimingNetId, Option<MappedNetId>>,
    mapped_undo: BTreeMap<MappedNetId, Option<TimingNetId>>,
}

impl TimingModel {
    pub(super) fn install_mapped_net_bindings(
        &mut self,
        bindings: BTreeMap<MappedNetId, Option<String>>,
    ) -> Result<(), crate::TimingError> {
        let edit = self.apply_mapped_net_bindings(bindings)?;
        self.update_mapped_binding_topology(&edit);
        self.reseal_generation();
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "model-region publication preflights compact capacities and journals graph, design, \
                  object-binding, and mapped-net changes as one rollback unit"
    )]
    /// Applies one mapped-generation region delta across all model owners.
    ///
    /// Generation identity and final capacities are checked first. Graph,
    /// design, stable-position, and mapped-net changes then form one rollback
    /// unit; any intermediate failure is recovered before returning.
    pub(crate) fn apply_instance_region(
        &mut self,
        delta: TimingRegionDelta,
    ) -> Result<(InstanceRegionModelEdit, Vec<usize>), crate::TimingError> {
        if self.mapped_generation != delta.mapped_generation {
            return Err(crate::TimingModelError::ForeignMappedRegionEdit.into());
        }
        if delta.is_empty() {
            return Err(crate::TimingModelError::EmptyRegionDelta.into());
        }
        let TimingRegionDelta {
            mapped_generation: _,
            updates,
            mapped_net_bindings,
        } = delta;
        let old_topology = self.topology;
        let mut old_graph_instances = Vec::new();
        let mut new_graph_instances = Vec::new();
        for (&id, replacement) in &updates {
            let position = self.instance_positions.get(id);
            if position.is_none() && replacement.is_none() {
                return Err(
                    crate::TimingModelError::UnknownRemovedInstance { id: id.raw() }.into(),
                );
            }
            if let Some(replacement) = replacement
                && replacement.id != id
            {
                return Err(crate::TimingModelError::ReplacementIdMismatch {
                    expected: id.raw(),
                    actual: replacement.id.raw(),
                }
                .into());
            }
            if let Some(position) = position {
                old_graph_instances.push(
                    self.owned_instance_at(position)
                        .expect("instance position references a live design row"),
                );
            }
            if let Some(replacement) = replacement {
                new_graph_instances.push(replacement.clone());
            }
        }
        let mut replacements = Vec::new();
        let mut removals = Vec::new();
        let mut additions = Vec::new();
        for (id, replacement) in updates {
            match (self.instance_positions.get(id), replacement) {
                (Some(_), Some(replacement)) => replacements.push((id, replacement)),
                (Some(_), None) => removals.push(id),
                (None, Some(replacement)) => additions.push((id, replacement)),
                (None, None) => unreachable!("unknown removals were validated before graph edit"),
            }
        }
        let final_instance_count = self
            .design
            .instance_count()
            .checked_sub(removals.len())
            .and_then(|count| count.checked_add(additions.len()))
            .ok_or_else(design_row_capacity)?;
        if final_instance_count > u32::MAX as usize {
            return Err(design_row_capacity());
        }
        let (graph, dirty) = self.graph.replace_instance_region(
            &self.library,
            &old_graph_instances,
            &new_graph_instances,
        )?;
        let mut journal = Vec::with_capacity(replacements.len() + removals.len() + additions.len());
        let mutation = (|| {
            for (id, replacement) in replacements {
                let position = self
                    .instance_positions
                    .get(id)
                    .expect("replacement instance was validated before graph edit");
                let replacement = OwnedTimingInstance::from_timing(replacement);
                let old = self
                    .design
                    .replace(position, replacement)
                    .expect("instance position references a live design row");
                journal.push(RegionInstanceOp::Replaced { id, old });
            }
            for id in removals {
                let position = self
                    .instance_positions
                    .get(id)
                    .expect("removed instance was validated before graph edit");
                let old = self
                    .design
                    .swap_remove(position)
                    .expect("instance position references a live design row");
                self.instance_positions.remove(id)?;
                journal.push(RegionInstanceOp::Removed { position, old });
                if let Some(moved) = self.design.instance(position) {
                    self.instance_positions.insert(moved.id, position)?;
                }
            }
            for (id, replacement) in additions {
                let position = self.design.instance_count();
                self.design
                    .push(OwnedTimingInstance::from_timing(replacement))?;
                journal.push(RegionInstanceOp::Added { id, position });
                self.instance_positions.insert(id, position)?;
            }
            Ok::<(), crate::TimingError>(())
        })();
        if let Err(error) = mutation {
            return Err(self.rollback_failed_instance_region(error, journal, graph, old_topology));
        }
        let mapped_nets = match self.apply_mapped_net_bindings(mapped_net_bindings) {
            Ok(edit) => edit,
            Err(error) => {
                return Err(self.rollback_failed_instance_region(
                    error,
                    journal,
                    graph,
                    old_topology,
                ));
            }
        };
        let edit = InstanceRegionModelEdit {
            journal,
            mapped_nets,
            graph,
            old_topology,
        };
        for instance in &old_graph_instances {
            self.topology.remove_instance(instance);
        }
        for instance in &new_graph_instances {
            self.topology.insert_instance(instance);
        }
        self.update_mapped_binding_topology(&edit.mapped_nets);
        self.reseal_generation();
        Ok((edit, dirty))
    }

    /// Restores mapped bindings, graph state, dense rows, and topology identity.
    pub(crate) fn rollback_instance_region(
        &mut self,
        edit: InstanceRegionModelEdit,
    ) -> Result<Vec<usize>, crate::TimingError> {
        self.rollback_mapped_net_bindings(edit.mapped_nets)?;
        let dirty = self.graph.rollback_instance_region(edit.graph)?;
        for op in edit.journal.into_iter().rev() {
            match op {
                RegionInstanceOp::Replaced { id, old } => {
                    let position = self
                        .instance_positions
                        .get(id)
                        .ok_or(crate::TimingModelError::UnknownRemovedInstance { id: id.raw() })?;
                    self.design
                        .replace(position, old)
                        .expect("rollback instance position references a live row");
                }
                RegionInstanceOp::Removed { position, old } => {
                    if position > self.design.instance_count() {
                        return Err(crate::TimingModelError::RollbackPositionOutOfBounds {
                            position,
                            design_len: self.design.instance_count(),
                        }
                        .into());
                    }
                    let old_id = old.id;
                    if position == self.design.instance_count() {
                        self.design.push(old)?;
                        self.instance_positions.insert(old_id, position)?;
                    } else {
                        let moved = self
                            .design
                            .replace(position, old)
                            .expect("rollback row remains live");
                        let moved_id = moved.id;
                        self.design.push(moved)?;
                        self.instance_positions.insert(old_id, position)?;
                        self.instance_positions
                            .insert(moved_id, self.design.instance_count() - 1)?;
                    }
                }
                RegionInstanceOp::Added { id, position } => {
                    if position.checked_add(1) != Some(self.design.instance_count()) {
                        return Err(crate::TimingModelError::RollbackPositionOutOfBounds {
                            position,
                            design_len: self.design.instance_count(),
                        }
                        .into());
                    }
                    self.design.pop();
                    self.instance_positions.remove(id)?;
                }
            }
        }
        self.topology = edit.old_topology;
        self.reseal_generation();
        Ok(dirty)
    }

    /// Consumes a prepared journal and releases the graph's deferred removals.
    pub(crate) fn commit_instance_region(&mut self, edit: InstanceRegionModelEdit) {
        self.graph.commit_instance_region(edit.graph);
    }

    /// Completes fallible row compaction before the infallible commit step.
    pub(crate) fn prepare_instance_region_commit(&mut self) -> Result<(), crate::TimingError> {
        self.graph.compact_incremental_rows()
    }

    fn rollback_failed_instance_region(
        &mut self,
        primary: crate::TimingError,
        journal: Vec<RegionInstanceOp>,
        graph: analysis::InstanceRegionGraphEdit,
        old_topology: TimingTopologyState,
    ) -> crate::TimingError {
        let edit = InstanceRegionModelEdit {
            journal,
            mapped_nets: MappedNetModelEdit {
                original_timing_len: self.timing_to_mapped_net.len(),
                mapped_len_at_start: self.mapped_to_timing_net.len(),
                timing_undo: BTreeMap::new(),
                mapped_undo: BTreeMap::new(),
            },
            graph,
            old_topology,
        };
        match self.rollback_instance_region(edit) {
            Ok(_) => primary,
            Err(rollback) => crate::TimingError::Rollback {
                operation: "timing model region update",
                primary: Box::new(primary),
                rollback: Box::new(rollback),
            },
        }
    }

    fn update_mapped_binding_topology(&mut self, edit: &MappedNetModelEdit) {
        let topology = &mut self.topology;
        let graph = &self.graph;
        let bindings = &self.mapped_to_timing_net;
        for (&mapped, &old) in &edit.mapped_undo {
            let new = bindings.get(mapped.index()).copied().flatten();
            if old == new {
                continue;
            }
            if let Some(old) = old {
                let name = graph
                    .net_name(old.index())
                    .expect("mapped bindings reference live timing graph nets");
                topology.remove_mapped_binding(mapped, name);
            }
            if let Some(new) = new {
                let name = graph
                    .net_name(new.index())
                    .expect("mapped bindings reference live timing graph nets");
                topology.insert_mapped_binding(mapped, name);
            }
        }
    }

    fn reseal_generation(&mut self) {
        self.generation = TimingGeneration::seal(self.topology.fingerprint(), self.analysis_inputs);
    }

    fn apply_mapped_net_bindings(
        &mut self,
        bindings: BTreeMap<MappedNetId, Option<String>>,
    ) -> Result<MappedNetModelEdit, crate::TimingError> {
        let mut edit = MappedNetModelEdit {
            original_timing_len: self.timing_to_mapped_net.len(),
            mapped_len_at_start: self.mapped_to_timing_net.len(),
            timing_undo: BTreeMap::new(),
            mapped_undo: BTreeMap::new(),
        };
        self.timing_to_mapped_net
            .try_resize(self.graph.net_count())
            .map_err(|_| mapped_binding_capacity())?;
        let result = (|| {
            for (mapped, name) in bindings {
                if mapped.index() >= self.mapped_to_timing_net.len() {
                    self.mapped_to_timing_net
                        .try_resize(mapped.index() + 1)
                        .map_err(|_| mapped_binding_capacity())?;
                }
                edit.mapped_undo.entry(mapped).or_insert(
                    self.mapped_to_timing_net
                        .get(mapped.index())
                        .copied()
                        .flatten(),
                );
                if let Some(old_timing) = self
                    .mapped_to_timing_net
                    .get(mapped.index())
                    .copied()
                    .flatten()
                {
                    edit.timing_undo.entry(old_timing).or_insert(
                        self.timing_to_mapped_net
                            .get(old_timing.index())
                            .copied()
                            .flatten(),
                    );
                    self.timing_to_mapped_net
                        .try_set(old_timing.index(), None)
                        .map_err(|_| mapped_binding_capacity())?;
                }
                self.mapped_to_timing_net
                    .try_set(mapped.index(), None)
                    .map_err(|_| mapped_binding_capacity())?;
                let Some(name) = name else {
                    continue;
                };
                let timing_index = self.graph.net_id(&name).ok_or_else(|| {
                    crate::TimingModelError::MappedNetMissingGraphNet {
                        mapped,
                        name: name.clone(),
                    }
                })?;
                let timing = TimingNetId::from_index(timing_index)?;
                if let Some(other) = self
                    .timing_to_mapped_net
                    .get(timing_index)
                    .copied()
                    .flatten()
                    && other != mapped
                {
                    return Err(crate::TimingModelError::MappedNetAlias {
                        name,
                        first: other,
                        second: mapped,
                    }
                    .into());
                }
                edit.timing_undo.entry(timing).or_insert(
                    self.timing_to_mapped_net
                        .get(timing_index)
                        .copied()
                        .flatten(),
                );
                self.timing_to_mapped_net
                    .try_set(timing_index, Some(mapped))
                    .map_err(|_| mapped_binding_capacity())?;
                self.mapped_to_timing_net
                    .try_set(mapped.index(), Some(timing))
                    .map_err(|_| mapped_binding_capacity())?;
            }
            Ok::<(), crate::TimingError>(())
        })();
        if let Err(error) = result {
            return match self.rollback_mapped_net_bindings(edit) {
                Ok(()) => Err(error),
                Err(rollback) => Err(crate::TimingError::Rollback {
                    operation: "timing mapped-net update",
                    primary: Box::new(error),
                    rollback: Box::new(rollback),
                }),
            };
        }
        Ok(edit)
    }

    fn rollback_mapped_net_bindings(
        &mut self,
        edit: MappedNetModelEdit,
    ) -> Result<(), crate::TimingError> {
        for (mapped, old) in edit.mapped_undo {
            if mapped.index() >= self.mapped_to_timing_net.len() {
                return Err(crate::TimingModelError::RollbackMissingNet {
                    id: u32::try_from(mapped.index())
                        .expect("mapped net IDs are represented by compact u32 indices"),
                }
                .into());
            }
            self.mapped_to_timing_net
                .try_set(mapped.index(), old)
                .map_err(|_| mapped_binding_capacity())?;
        }
        for (timing, old) in edit.timing_undo {
            if timing.index() >= self.timing_to_mapped_net.len() {
                return Err(
                    crate::TimingModelError::RollbackMissingNet { id: timing.raw() }.into(),
                );
            }
            self.timing_to_mapped_net
                .try_set(timing.index(), old)
                .map_err(|_| mapped_binding_capacity())?;
        }
        self.timing_to_mapped_net.truncate(edit.original_timing_len);
        self.mapped_to_timing_net.truncate(edit.mapped_len_at_start);
        Ok(())
    }
}

fn design_row_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "timing design row arena",
    }
    .into()
}
