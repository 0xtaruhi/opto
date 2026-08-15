// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional replacement of instance-owned timing graph topology.

use super::{GraphArcId, InstancePinRow, SequentialGraphArc, TimingGraph};
use crate::{LibraryCellId, TargetPinDirection, TimingEdge, TimingInstance, TimingLibrary};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
/// Complete rollback journal for one applied graph-region replacement.
///
/// Removed arcs remain live until commit, while newly allocated arcs and rows
/// remain reversible until rollback. The journal also records the topological
/// generation because another order rebuild may occur while the edit is live.
pub(crate) struct InstanceRegionGraphEdit {
    old_net_len: usize,
    old_nets: BTreeMap<usize, NetGraphState>,
    old_instance_cell_len: usize,
    old_instance_cells: Vec<(crate::TimingInstanceId, Option<LibraryCellId>)>,
    old_instance_net_len: usize,
    old_instance_nets: Vec<(crate::TimingInstanceId, Option<Box<[crate::TimingNetId]>>)>,
    structure_changed: bool,
    old_order_len: usize,
    was_stale: bool,
    old_generation: u64,
    old_arc_len: usize,
    allocated_arcs: Vec<GraphArcId>,
    removed_arcs: Vec<GraphArcId>,
}

#[derive(Debug, Clone)]
struct NetGraphState {
    outgoing: Vec<GraphArcId>,
    incoming: Vec<GraphArcId>,
    sequential_outputs: Vec<SequentialGraphArc>,
    capacitive_load: [f64; 2],
    fanout_load: f64,
    wire_fanout: f64,
    wire_capacitance: f64,
    wire_resistance: f64,
}

impl InstanceRegionGraphEdit {
    pub(crate) fn changes_structure(&self) -> bool {
        self.structure_changed
    }
}

impl TimingGraph {
    #[allow(
        clippy::too_many_lines,
        reason = "region replacement is a preflighted graph transaction whose journal must cover \
                  net growth, adjacency rows, arc arenas, and cycle validation together"
    )]
    /// Applies a region replacement after preflighting all graph capacities.
    ///
    /// The returned dirty nets seed incremental propagation. On error the graph
    /// is restored internally; on success the caller must eventually commit or
    /// roll back the returned journal.
    pub(crate) fn replace_instance_region(
        &mut self,
        library: &TimingLibrary,
        old_instances: &[TimingInstance],
        new_instances: &[TimingInstance],
    ) -> Result<(InstanceRegionGraphEdit, Vec<usize>), crate::TimingError> {
        let new_instance_cells = new_instances
            .iter()
            .map(|instance| self.library_cells.resolve(library, instance))
            .collect::<Result<Vec<_>, _>>()?;
        let missing_nets = new_instances
            .iter()
            .flat_map(|instance| &instance.connections)
            .map(|connection| connection.net.as_str())
            .filter(|name| self.net_id(name).is_none())
            .collect::<BTreeSet<_>>();
        let final_net_count = self
            .net_count
            .checked_add(missing_nets.len())
            .ok_or(crate::TimingModelError::Capacity { resource: "net ID" })?;
        if final_net_count != 0 {
            crate::TimingNetId::from_index(final_net_count - 1)?;
        }
        self.net_ports
            .try_reserve_rows(final_net_count)
            .map_err(super::packed_row_capacity)?;
        self.outgoing
            .try_reserve_rows(final_net_count)
            .map_err(super::packed_row_capacity)?;
        self.incoming
            .try_reserve_rows(final_net_count)
            .map_err(super::packed_row_capacity)?;
        self.primary_inputs
            .try_reserve_rows(final_net_count)
            .map_err(super::packed_row_capacity)?;
        self.sequential_outputs
            .try_reserve_rows(final_net_count)
            .map_err(super::packed_row_capacity)?;
        let old_net_len = self.net_count;
        for name in missing_nets {
            self.ensure_region_net(name)?;
        }
        let new_instance_nets = new_instances
            .iter()
            .map(|instance| {
                instance
                    .connections
                    .iter()
                    .map(|connection| {
                        let index = self
                            .net_id(&connection.net)
                            .expect("new region nets were inserted after capacity validation");
                        Ok(crate::TimingNetId::from_raw(u32::try_from(index).expect(
                            "region net capacity is validated before inserting missing nets",
                        )))
                    })
                    .collect::<Result<Vec<_>, crate::TimingError>>()
                    .map(Vec::into_boxed_slice)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let changed = old_instances
            .iter()
            .chain(new_instances)
            .map(|instance| instance.id.raw() as usize)
            .collect::<BTreeSet<_>>();
        let touched = old_instances
            .iter()
            .chain(new_instances)
            .flat_map(|instance| &instance.connections)
            .filter_map(|connection| self.net_id(&connection.net))
            .collect::<BTreeSet<_>>();
        let dirty = touched.clone();
        let old_nets = touched
            .iter()
            .filter(|&&net| net < old_net_len)
            .map(|&net| {
                (
                    net,
                    NetGraphState {
                        outgoing: self.outgoing[net].to_vec(),
                        incoming: self.incoming[net].to_vec(),
                        sequential_outputs: self.sequential_outputs[net].to_vec(),
                        capacitive_load: self.capacitive_loads[net],
                        fanout_load: self.fanout_loads[net],
                        wire_fanout: self.wire_fanouts[net],
                        wire_capacitance: self.wire_capacitances[net],
                        wire_resistance: self.wire_resistances[net],
                    },
                )
            })
            .collect();
        let old_instance_cells = changed
            .iter()
            .map(|&instance| {
                let instance = crate::TimingInstanceId::from_raw(
                    u32::try_from(instance)
                        .expect("timing instance identifiers originate from u32"),
                );
                (instance, self.instance_cells.get(instance))
            })
            .collect();
        let old_instance_nets = changed
            .iter()
            .map(|&instance| {
                let instance = crate::TimingInstanceId::from_raw(
                    u32::try_from(instance)
                        .expect("timing instance identifiers originate from u32"),
                );
                (
                    instance,
                    self.instance_nets
                        .get(instance)
                        .map(<[crate::TimingNetId]>::to_vec)
                        .map(Vec::into_boxed_slice),
                )
            })
            .collect();
        let removed_arcs = touched
            .iter()
            .flat_map(|&net| self.outgoing[net].iter().copied())
            .filter(|&id| changed.contains(&(self.arc(id).instance.raw() as usize)))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut edit = InstanceRegionGraphEdit {
            old_net_len,
            old_nets,
            old_instance_cell_len: self.instance_cells.len(),
            old_instance_cells,
            old_instance_net_len: self.instance_nets.len(),
            old_instance_nets,
            structure_changed: false,
            old_order_len: self.topological_order.len(),
            was_stale: self.topological_order_stale,
            old_generation: self.topological_generation,
            old_arc_len: self.arcs.len(),
            allocated_arcs: Vec::new(),
            removed_arcs,
        };

        let result = (|| {
            let arcs = &self.arcs;
            for &net in &touched {
                self.outgoing.retain(net, |arc| {
                    !changed.contains(
                        &(arcs.get(*arc).expect("live adjacency arc").instance.raw() as usize),
                    )
                });
                self.incoming.retain(net, |arc| {
                    !changed.contains(
                        &(arcs.get(*arc).expect("live adjacency arc").instance.raw() as usize),
                    )
                });
                self.sequential_outputs
                    .retain(net, |arc| !changed.contains(&(arc.instance.raw() as usize)));
            }
            for instance in old_instances {
                self.adjust_instance_loads(library, instance, -1.0)?;
            }
            for &instance in &changed {
                let id = crate::TimingInstanceId::from_raw(
                    u32::try_from(instance)
                        .expect("timing instance identifiers originate from u32"),
                );
                self.instance_cells.remove(id)?;
                self.instance_nets.remove(id)?;
            }
            for ((instance, &cell), nets) in new_instances
                .iter()
                .zip(&new_instance_cells)
                .zip(new_instance_nets)
            {
                self.instance_cells.insert(instance.id, cell)?;
                self.instance_nets.insert(instance.id, nets)?;
                self.adjust_instance_loads(library, instance, 1.0)?;
                let cell = library
                    .cells
                    .get(cell.index())
                    .expect("resolved timing library cell remains live");
                let pins = InstancePinRow::build(
                    cell,
                    instance
                        .connections
                        .iter()
                        .zip(
                            self.instance_nets
                                .get(instance.id)
                                .expect("new timing instance has typed net bindings"),
                        )
                        .map(|(connection, &net)| (connection.pin.as_str(), net)),
                )?;
                self.add_instance_arcs_view(
                    instance.id,
                    &instance.name,
                    cell,
                    &pins,
                    Some(&mut edit.allocated_arcs),
                )?;
            }
            self.instance_cells.trim();
            self.instance_nets.trim()?;
            for &net in &touched {
                self.refresh_wire_load(net);
            }
            Ok::<_, crate::TimingError>(())
        })();
        if let Err(error) = result {
            return match self.rollback_instance_region(edit) {
                Ok(_) => Err(error),
                Err(rollback) => Err(crate::TimingError::Rollback {
                    operation: "timing graph region update",
                    primary: Box::new(error),
                    rollback: Box::new(rollback),
                }),
            };
        }
        let RegionEdgeChanges {
            structure_changed,
            dependencies_changed,
            added: added_edges,
        } = self.region_edge_changes(&edit, &touched);
        if structure_changed {
            edit.structure_changed = true;
            for net in edit.old_net_len..self.net_count {
                self.topological_positions.push(
                    u32::try_from(self.topological_order.len()).expect(
                        "region preflight keeps the timing graph within compact-ID capacity",
                    ),
                );
                self.topological_order.push(net);
            }
            if self.added_edges_create_cycle(&added_edges) {
                return match self.rollback_instance_region(edit) {
                    Ok(_) => Err(crate::TimingAnalysisError::BufferInsertionLoop.into()),
                    Err(rollback) => Err(crate::TimingError::Rollback {
                        operation: "timing graph cycle validation",
                        primary: Box::new(crate::TimingAnalysisError::BufferInsertionLoop.into()),
                        rollback: Box::new(rollback),
                    }),
                };
            }
            // Removed edges leave the retained order conservative; new edges or
            // nets require a plan whose positions cover the changed graph.
            self.topological_order_stale |= dependencies_changed
                || !added_edges.is_empty()
                || self.net_count != edit.old_net_len;
        }
        Ok((edit, dirty.into_iter().collect()))
    }
}

/// What one region edit changed about the timing graph.
struct RegionEdgeChanges {
    /// Whether any arc set the edit touched differs from its snapshot.
    structure_changed: bool,
    /// Whether any touched net's propagation-plan dependencies differ. This is
    /// what decides plan reuse; adjacency alone does not see a latch enable.
    dependencies_changed: bool,
    /// Adjacency edges the edit introduced, for cycle validation.
    added: Vec<(usize, usize)>,
}

impl TimingGraph {
    fn region_edge_changes(
        &self,
        edit: &InstanceRegionGraphEdit,
        touched: &BTreeSet<usize>,
    ) -> RegionEdgeChanges {
        let mut changed = self.net_count != edit.old_net_len;
        let mut dependencies_changed = false;
        let mut added = Vec::new();
        let mut old_edges = Vec::new();
        let mut new_edges = Vec::new();
        for &net in touched {
            old_edges.clear();
            new_edges.clear();
            if let Some(old) = edit.old_nets.get(&net) {
                old_edges.extend(old.outgoing.iter().map(|&arc| self.arc(arc).to.index()));
            }
            new_edges.extend(
                self.outgoing[net]
                    .iter()
                    .map(|&arc| self.arc(arc).to.index()),
            );
            old_edges.sort_unstable();
            old_edges.dedup();
            new_edges.sort_unstable();
            new_edges.dedup();
            changed |= old_edges != new_edges;
            added.extend(
                new_edges
                    .iter()
                    .copied()
                    .filter(|to| old_edges.binary_search(to).is_err())
                    .map(|to| (net, to)),
            );
            if let Some(old) = edit.old_nets.get(&net) {
                // Compare the plan relation, including latch enables, not only
                // graph adjacency.
                old_edges.clear();
                new_edges.clear();
                old_edges.extend(self.plan_dependencies(&old.incoming));
                new_edges.extend(self.plan_dependencies(&self.incoming[net]));
                old_edges.sort_unstable();
                old_edges.dedup();
                new_edges.sort_unstable();
                new_edges.dedup();
                dependencies_changed |= old_edges != new_edges;
            }
        }
        RegionEdgeChanges {
            structure_changed: changed || dependencies_changed,
            dependencies_changed,
            added,
        }
    }

    fn added_edges_create_cycle(&mut self, added: &[(usize, usize)]) -> bool {
        added.iter().any(|&(from, to)| {
            from == to
                || (self.topological_order_stale
                    || self.topological_position(from) >= self.topological_position(to))
                    && self.net_reaches(to, from)
        })
    }

    fn net_reaches(&mut self, start: usize, target: usize) -> bool {
        self.cycle_epoch = self.cycle_epoch.wrapping_add(1);
        if self.cycle_epoch == 0 {
            self.cycle_visit_epochs.fill(0);
            self.cycle_epoch = 1;
        }
        let epoch = self.cycle_epoch;
        self.cycle_stack.clear();
        self.cycle_stack.push(start);
        self.cycle_visit_epochs[start] = epoch;
        while let Some(net) = self.cycle_stack.pop() {
            for &arc in &self.outgoing[net] {
                let to = self.arc(arc).to.index();
                if to == target {
                    return true;
                }
                if self.cycle_visit_epochs[to] != epoch {
                    self.cycle_visit_epochs[to] = epoch;
                    self.cycle_stack.push(to);
                }
            }
        }
        false
    }

    /// Restores adjacency, arc allocations, appended nets, and ordering state.
    pub(crate) fn rollback_instance_region(
        &mut self,
        edit: InstanceRegionGraphEdit,
    ) -> Result<Vec<usize>, crate::TimingError> {
        let seeds = edit.old_nets.keys().copied().collect::<BTreeSet<_>>();
        for (net, state) in edit.old_nets {
            self.outgoing.replace(net, state.outgoing);
            self.incoming.replace(net, state.incoming);
            self.sequential_outputs
                .replace(net, state.sequential_outputs);
            self.capacitive_loads[net] = state.capacitive_load;
            self.fanout_loads[net] = state.fanout_load;
            self.wire_fanouts[net] = state.wire_fanout;
            self.wire_capacitances[net] = state.wire_capacitance;
            self.wire_resistances[net] = state.wire_resistance;
        }
        self.arcs
            .rollback_allocations(edit.old_arc_len, &edit.allocated_arcs);
        while self.net_count > edit.old_net_len {
            let trailing = self.net_count - 1;
            self.net_names
                .pop()
                .expect("region rollback truncates an appended net");
            self.net_ports.replace(trailing, Vec::new());
            self.outgoing.replace(trailing, Vec::new());
            self.incoming.replace(trailing, Vec::new());
            self.primary_inputs.replace(trailing, Vec::new());
            self.sequential_outputs.replace(trailing, Vec::new());
            self.net_ports.pop_empty();
            self.outgoing.pop_empty();
            self.incoming.pop_empty();
            self.primary_inputs.pop_empty();
            self.sequential_outputs.pop_empty();
            self.capacitive_loads.pop();
            self.fanout_loads.pop();
            self.wire_fanouts.pop();
            self.wire_capacitances.pop();
            self.wire_resistances.pop();
            self.cycle_visit_epochs.pop();
            self.parasitic_nets.pop();
            self.constant_values.pop();
            self.net_count -= 1;
        }
        for (instance, cell) in edit.old_instance_cells {
            match cell {
                Some(cell) => {
                    self.instance_cells.insert(instance, cell)?;
                }
                None => {
                    self.instance_cells.remove(instance)?;
                }
            }
        }
        self.instance_cells.truncate(edit.old_instance_cell_len);
        for (instance, nets) in edit.old_instance_nets {
            match nets {
                Some(nets) => {
                    self.instance_nets.insert(instance, nets)?;
                }
                None => {
                    self.instance_nets.remove(instance)?;
                }
            }
        }
        self.instance_nets.truncate(edit.old_instance_net_len)?;
        if edit.structure_changed {
            self.topological_positions.truncate(edit.old_net_len);
            if self.topological_generation == edit.old_generation {
                self.topological_order.truncate(edit.old_order_len);
                self.topological_order_stale = edit.was_stale;
            } else {
                self.topological_order.retain(|&net| net < edit.old_net_len);
                self.topological_order_stale = true;
            }
        }
        Ok(seeds.into_iter().collect())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "commit consumes the rollback journal after releasing its deferred removals"
    )]
    /// Releases deferred old arcs after every fallible owner has prepared.
    pub(crate) fn commit_instance_region(&mut self, edit: InstanceRegionGraphEdit) {
        self.arcs.commit_removals(&edit.removed_arcs);
    }

    fn ensure_region_net(&mut self, name: &str) -> Result<usize, crate::TimingError> {
        if let Some(net) = self.net_id(name) {
            return Ok(net);
        }
        let net = self.net_count;
        self.net_names.push(name.to_string());
        self.net_count += 1;
        self.net_ports
            .push_empty()
            .map_err(super::packed_row_capacity)?;
        self.outgoing
            .push_empty()
            .map_err(super::packed_row_capacity)?;
        self.incoming
            .push_empty()
            .map_err(super::packed_row_capacity)?;
        self.primary_inputs
            .push_empty()
            .map_err(super::packed_row_capacity)?;
        self.sequential_outputs
            .push_empty()
            .map_err(super::packed_row_capacity)?;
        let parasitic = self.parasitics.net_id(name);
        let capacitance = parasitic
            .and_then(|id| self.parasitics.net_by_id(id))
            .and_then(crate::ParasiticNetRef::annotated_capacitance)
            .unwrap_or(0.0);
        self.parasitic_nets.push(parasitic);
        self.constant_values
            .push(crate::model::constant_net_value(name));
        self.capacitive_loads.push([capacitance; 2]);
        self.fanout_loads.push(0.0);
        self.wire_fanouts.push(0.0);
        self.wire_capacitances.push(0.0);
        self.wire_resistances.push(0.0);
        self.cycle_visit_epochs.push(0);
        Ok(net)
    }

    pub(super) fn adjust_instance_loads(
        &mut self,
        library: &TimingLibrary,
        instance: &TimingInstance,
        direction: f64,
    ) -> Result<(), crate::TimingError> {
        self.adjust_instance_loads_view(
            library,
            instance.id,
            &instance.name,
            &instance.cell,
            instance
                .connections
                .iter()
                .map(|connection| connection.pin.as_str()),
            direction,
        )
    }

    pub(super) fn adjust_instance_loads_view<'a>(
        &mut self,
        library: &TimingLibrary,
        instance_id: crate::TimingInstanceId,
        instance_name: &str,
        instance_cell: &str,
        pins: impl ExactSizeIterator<Item = &'a str>,
        direction: f64,
    ) -> Result<(), crate::TimingError> {
        let cell = self.cell(library, instance_id).ok_or_else(|| {
            crate::TimingModelError::UnknownCell {
                instance: instance_name.to_string(),
                cell: instance_cell.to_string(),
            }
        })?;
        let nets = self
            .instance_nets
            .get(instance_id)
            .expect("validated timing instances have typed net bindings");
        if pins.len() != nets.len() {
            return Err(crate::TimingAnalysisError::InconsistentTopology.into());
        }
        for (pin_name, &net) in pins.zip(nets) {
            let Some(pin) = cell.pins().find(|pin| {
                pin.name() == pin_name
                    && matches!(
                        pin.direction(),
                        TargetPinDirection::Input | TargetPinDirection::Inout
                    )
            }) else {
                continue;
            };
            if !self
                .parasitic_nets
                .get(net.index())
                .copied()
                .flatten()
                .and_then(|id| self.parasitics.net_by_id(id))
                .is_some_and(crate::ParasiticNetRef::pin_capacitance_included)
            {
                for edge in TimingEdge::ALL {
                    self.capacitive_loads[net.index()][edge.index()] +=
                        direction * pin.design_input_capacitance_at(edge);
                }
            }
            let fanout = pin.design_fanout_load();
            self.fanout_loads[net.index()] += direction * fanout;
            self.wire_fanouts[net.index()] += direction * fanout;
        }
        Ok(())
    }

    pub(super) fn refresh_wire_load(&mut self, net: usize) {
        for load in &mut self.capacitive_loads[net] {
            *load -= self.wire_capacitances[net];
        }
        let (capacitance, resistance) = self
            .wire_load_model
            .as_ref()
            .filter(|_| {
                self.parasitic_nets
                    .get(net)
                    .copied()
                    .flatten()
                    .and_then(|id| self.parasitics.net_by_id(id))
                    .is_none_or(|net| net.annotated_capacitance().is_none())
            })
            .map_or((0.0, 0.0), |model| {
                (
                    model.capacitance_at(self.wire_fanouts[net]),
                    model.resistance_at(self.wire_fanouts[net]),
                )
            });
        self.wire_capacitances[net] = capacitance;
        self.wire_resistances[net] = resistance;
        for load in &mut self.capacitive_loads[net] {
            *load += capacitance;
        }
    }
}
