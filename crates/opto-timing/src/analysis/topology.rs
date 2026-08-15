// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Sealed timing topology and generation-local electrical values.
//!
//! Stable timing-net and instance identities are resolved into compact graph
//! rows. Region edits journal adjacency, arc slots, mapped bindings, and
//! topological-order state together so a view can commit or roll back without
//! rebuilding unaffected topology.

use crate::{
    DesignRuleScope, LibraryCellId, PortId, TargetCellRef, TargetPinDirection, TargetPinRef,
    TargetTimingArcRef, TargetTimingType, TimingDesign, TimingEdge, TimingInstanceId,
    TimingInstanceRef, TimingLibrary, TimingPortDirection, model::SealedTopology,
};
use opto_core::RowArena;
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

mod arcs;
mod components;
mod fork;
mod helpers;
mod region;
mod storage;

pub(super) use arcs::sequential_element_for_control;
use arcs::{graph_arc_kind, sink_response};
pub(super) use helpers::connection_map_ref;
use helpers::*;
pub(crate) use region::InstanceRegionGraphEdit;
pub(crate) use storage::*;

#[derive(Debug)]
/// Compact timing graph owned by one analysis view.
///
/// Name, ordering, and selected topology allocations may be shared across
/// sibling views; electrical values and incremental edit state remain private.
pub(crate) struct TimingGraph {
    net_count: usize,
    port_nets: std::sync::Arc<BTreeMap<PortId, Box<[crate::TimingNetId]>>>,
    port_bindings: std::sync::Arc<[crate::TimingNetId]>,
    net_ports: RowArena<PortId>,
    pub(super) net_names: SharedNetNames,
    arcs: GraphArcArena,
    pub(super) outgoing: RowArena<GraphArcId>,
    pub(super) incoming: RowArena<GraphArcId>,
    pub(super) primary_inputs: RowArena<usize>,
    pub(super) sequential_outputs: RowArena<SequentialGraphArc>,
    pub(super) topological_order: SharedAppendVec<usize>,
    pub(super) topological_positions: SharedAppendVec<u32>,
    propagation_plan: opto_runtime::DependencyPlan,
    pub(super) topological_order_stale: bool,
    pub(super) topological_generation: u64,
    cycle_visit_epochs: Vec<u32>,
    cycle_epoch: u32,
    cycle_stack: Vec<usize>,
    pub(super) capacitive_loads: Vec<[f64; 2]>,
    pub(super) fanout_loads: Vec<f64>,
    wire_load_model: Option<crate::WireLoadModel>,
    wire_fanouts: Vec<f64>,
    wire_capacitances: Vec<f64>,
    wire_resistances: Vec<f64>,
    parasitics: crate::Parasitics,
    parasitic_nets: Vec<Option<crate::parasitics::ParasiticNetId>>,
    constant_values: Vec<Option<bool>>,
    library_cells: LibraryCellIndex,
    instance_cells: InstanceCellArena,
    instance_nets: InstanceNetArena,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GraphArcTopology {
    pub(super) from: crate::TimingNetId,
    pub(super) to: crate::TimingNetId,
    pub(super) instance: crate::TimingInstanceId,
    pub(super) pin: GraphPinId,
    pub(super) arc: GraphLibraryArcId,
    pub(super) kind: GraphArcKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct GraphArcValues {
    delay: [f64; 2],
    transition: [f64; 2],
    transition_valid: u8,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct GraphArcRef<'a> {
    topology: &'a GraphArcTopology,
    values: &'a GraphArcValues,
}

impl std::ops::Deref for GraphArcRef<'_> {
    type Target = GraphArcTopology;

    fn deref(&self) -> &Self::Target {
        self.topology
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GraphArcKind {
    Combinational,
    LatchData {
        enable_net: crate::TimingNetId,
        open_edge: TimingEdge,
        close_edge: TimingEdge,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SequentialGraphArc {
    pub(super) instance: crate::TimingInstanceId,
    pub(super) pin: GraphPinId,
    pub(super) arc: GraphLibraryArcId,
    pub(super) clock_net: crate::TimingNetId,
    pub(super) element: SequentialElement,
}

const INLINE_CELL_PINS: usize = 16;

/// One instance's net bindings aligned to its library-cell pin row.
///
/// Common standard cells stay entirely inline. The secondary row contains
/// only typed pin IDs sorted by borrowed library pin names, so timing arcs and
/// latch controls resolve pins by binary search without a per-instance map.
/// This is construction scratch, not resident graph storage; cells beyond the
/// inline limit spill only in proportion to that one cell's pin count.
struct InstancePinRow<'a> {
    pins: SmallVec<[TargetPinRef<'a>; INLINE_CELL_PINS]>,
    nets: SmallVec<[Option<crate::TimingNetId>; INLINE_CELL_PINS]>,
    by_name: SmallVec<[GraphPinId; INLINE_CELL_PINS]>,
}

impl<'a> InstancePinRow<'a> {
    fn build<'n>(
        cell: TargetCellRef<'a>,
        connections: impl IntoIterator<Item = (&'n str, crate::TimingNetId)>,
    ) -> Result<Self, crate::TimingError> {
        let pins = cell
            .pins()
            .collect::<SmallVec<[TargetPinRef<'a>; INLINE_CELL_PINS]>>();
        let mut by_name = SmallVec::<[GraphPinId; INLINE_CELL_PINS]>::with_capacity(pins.len());
        for index in 0..pins.len() {
            by_name.push(GraphPinId::from_index(index)?);
        }
        by_name.sort_unstable_by(|left, right| {
            pins[left.index()].name().cmp(pins[right.index()].name())
        });
        let mut row = Self {
            nets: smallvec::smallvec![None; pins.len()],
            pins,
            by_name,
        };
        for (pin, net) in connections {
            if let Some(id) = row.id_by_name(pin) {
                row.nets[id.index()] = Some(net);
            }
        }
        Ok(row)
    }

    fn id_by_name(&self, name: &str) -> Option<GraphPinId> {
        self.by_name
            .binary_search_by(|id| self.pins[id.index()].name().cmp(name))
            .ok()
            .map(|position| self.by_name[position])
    }

    fn net(&self, id: GraphPinId) -> Option<crate::TimingNetId> {
        self.nets[id.index()]
    }

    fn net_by_name(&self, name: &str) -> Option<crate::TimingNetId> {
        self.net(self.id_by_name(name)?)
    }
}

impl GraphArcRef<'_> {
    pub(super) fn interconnect_transition(&self, edge: TimingEdge) -> Option<f64> {
        let index = edge.index();
        (self.values.transition_valid & (1 << index) != 0).then_some(self.values.transition[index])
    }

    pub(super) fn interconnect_delay(&self, edge: TimingEdge) -> f64 {
        self.values.delay[edge.index()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SequentialElement {
    FlipFlop,
    Latch {
        open_edge: TimingEdge,
        close_edge: TimingEdge,
    },
}

impl TimingGraph {
    /// Compacts a quiescent graph and remaps every adjacency using moved arc IDs.
    ///
    /// Callers must not hold a live region edit because its journals use the
    /// pre-compaction arc and row identities.
    pub(crate) fn compact(&mut self) -> Result<(), crate::TimingError> {
        if let Some(remap) = self.arcs.compact()? {
            self.outgoing = remap_arc_rows(&self.outgoing, &remap)?;
            self.incoming = remap_arc_rows(&self.incoming, &remap)?;
        }
        self.compact_rows()?;
        self.net_names.compact()?;
        self.topological_order.compact();
        self.topological_positions.compact();
        self.cycle_visit_epochs.shrink_to_fit();
        self.cycle_stack.shrink_to_fit();
        self.capacitive_loads.shrink_to_fit();
        self.fanout_loads.shrink_to_fit();
        self.wire_fanouts.shrink_to_fit();
        self.wire_capacitances.shrink_to_fit();
        self.wire_resistances.shrink_to_fit();
        if let Some(model) = &mut self.wire_load_model {
            model.name.shrink_to_fit();
        }
        self.parasitic_nets.shrink_to_fit();
        self.constant_values.shrink_to_fit();
        self.instance_nets.compact().map_err(packed_row_capacity)?;
        Ok(())
    }

    fn compact_rows(&mut self) -> Result<(), crate::TimingError> {
        self.net_ports.compact().map_err(packed_row_capacity)?;
        self.outgoing.compact().map_err(packed_row_capacity)?;
        self.incoming.compact().map_err(packed_row_capacity)?;
        self.primary_inputs.compact().map_err(packed_row_capacity)?;
        self.sequential_outputs
            .compact()
            .map_err(packed_row_capacity)?;
        Ok(())
    }

    /// Seals fallible row overlays before an otherwise infallible edit commit.
    pub(crate) fn compact_incremental_rows(&mut self) -> Result<(), crate::TimingError> {
        self.compact_rows()?;
        self.instance_nets.compact().map_err(packed_row_capacity)
    }

    /// Accounts retained graph allocations. Parasitic validation indexes and
    /// `InstancePinRow` are bounded construction scratch and never enter this
    /// resident total.
    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let port_net_rows = self
            .port_nets
            .values()
            .map(|row| opto_core::resident::slice_bytes::<crate::TimingNetId>(row.len()))
            .sum::<usize>();
        self.net_names
            .owned_memory_bytes()
            .saturating_add(btree_bytes::<PortId, Box<[crate::TimingNetId]>>(
                self.port_nets.len(),
            ))
            .saturating_add(port_net_rows)
            .saturating_add(opto_core::resident::slice_bytes::<crate::TimingNetId>(
                self.port_bindings.len(),
            ))
            .saturating_add(self.net_ports.owned_memory_bytes())
            .saturating_add(self.arcs.owned_memory_bytes())
            .saturating_add(self.outgoing.owned_memory_bytes())
            .saturating_add(self.incoming.owned_memory_bytes())
            .saturating_add(self.primary_inputs.owned_memory_bytes())
            .saturating_add(self.sequential_outputs.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<usize>(
                self.topological_order.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<u32>(
                self.topological_positions.len(),
            ))
            .saturating_add(self.propagation_plan.owned_memory_bytes())
            .saturating_add(opto_core::resident::slice_bytes::<u32>(
                self.cycle_visit_epochs.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<usize>(
                self.cycle_stack.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<[f64; 2]>(
                self.capacitive_loads.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.fanout_loads.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.wire_fanouts.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.wire_capacitances.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<f64>(
                self.wire_resistances.len(),
            ))
            .saturating_add(self.wire_load_model.as_ref().map_or(0, |model| {
                opto_core::resident::allocation_bytes(model.name.len())
            }))
            .saturating_add(opto_core::resident::slice_bytes::<
                Option<crate::parasitics::ParasiticNetId>,
            >(self.parasitic_nets.len()))
            .saturating_add(opto_core::resident::slice_bytes::<Option<bool>>(
                self.constant_values.len(),
            ))
            .saturating_add(self.library_cells.owned_memory_bytes())
            .saturating_add(self.instance_cells.owned_memory_bytes())
            .saturating_add(self.instance_nets.owned_memory_bytes())
    }

    pub(crate) fn net_name(&self, net: usize) -> Option<&str> {
        self.net_names.get(net)
    }

    pub(crate) fn clock_scope_nets(
        &self,
        sources: &[PortId],
        scope: DesignRuleScope,
    ) -> BTreeSet<usize> {
        let clock_seeds = sources
            .iter()
            .flat_map(|source| self.port_nets.get(source).into_iter().flatten().copied())
            .map(crate::TimingNetId::index)
            .collect::<Vec<_>>();
        let clock_nets = self.reachable_nets(clock_seeds);
        if scope == DesignRuleScope::ClockPath {
            return clock_nets;
        }
        let data_seeds = self
            .sequential_outputs
            .iter()
            .enumerate()
            .filter(|(_, arcs)| {
                arcs.iter()
                    .any(|arc| clock_nets.contains(&arc.clock_net.index()))
            })
            .map(|(net, _)| net)
            .collect::<Vec<_>>();
        let data_nets = self.reachable_nets(data_seeds);
        if scope == DesignRuleScope::DataPath {
            return data_nets;
        }
        clock_nets.union(&data_nets).copied().collect()
    }

    fn reachable_nets(&self, seeds: Vec<usize>) -> BTreeSet<usize> {
        let mut reachable = BTreeSet::new();
        let mut worklist = VecDeque::from(seeds);
        while let Some(net) = worklist.pop_front() {
            if !reachable.insert(net) {
                continue;
            }
            for &arc in &self.outgoing[net] {
                worklist.push_back(self.arc(arc).to.index());
            }
        }
        reachable
    }

    #[allow(
        clippy::too_many_lines,
        reason = "graph construction performs one capacity-preflighted publication of topology, \
                  parasitic values, sequential metadata, and topological order"
    )]
    /// Builds view-specific graph state from one already sealed topology.
    ///
    /// All compact arenas and topological order are capacity-checked before the
    /// graph becomes visible to propagation.
    pub(crate) fn build_with_topology(
        design: &crate::model::SharedTimingDesign,
        library: &TimingLibrary,
        parasitics: crate::Parasitics,
        topology: SealedTopology,
    ) -> Result<Self, crate::TimingError> {
        let library_cells = LibraryCellIndex::build(library)?;
        let instance_cells = InstanceCellArena::try_from_entries(
            topology.instance_nets.len(),
            design.instances().map(|instance| {
                Ok((
                    instance.id,
                    library_cells.resolve_name(library, instance.name, instance.cell)?,
                ))
            }),
        )?;
        if library
            .cells
            .iter()
            .all(|cell| cell.pins().all(|pin| pin.timing_arcs().next().is_none()))
        {
            return Err(crate::TimingAnalysisError::NoLibertyTimingArcs.into());
        }

        if topology.port_nets.len() != design.ports().len() {
            return Err(crate::TimingAnalysisError::InconsistentTopology.into());
        }
        let net_names = topology.net_names;
        let instance_nets = topology.instance_nets;
        for instance in design.instances() {
            let nets = instance_nets
                .get(instance.id)
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?;
            if nets.len() != instance.connection_count()
                || nets.iter().any(|net| net.index() >= net_names.len())
            {
                return Err(crate::TimingAnalysisError::InconsistentTopology.into());
            }
        }
        validate_parasitics(&parasitics, &net_names, &instance_nets, design)?;
        let parasitic_nets = net_names
            .iter()
            .map(|name| parasitics.net_id(name))
            .collect::<Vec<_>>();
        let constant_values = net_names
            .iter()
            .map(crate::model::constant_net_value)
            .collect::<Vec<_>>();

        let mut port_nets = BTreeMap::<PortId, Vec<crate::TimingNetId>>::new();
        let mut net_port_entries = Vec::new();
        net_port_entries
            .try_reserve_exact(design.ports().len())
            .map_err(|_| packed_row_capacity(opto_core::PackedRowsError::Capacity))?;
        for (port, &net) in design.ports().iter().zip(topology.port_nets.iter()) {
            if net.index() >= net_names.len() {
                return Err(crate::TimingAnalysisError::InconsistentTopology.into());
            }
            let nets = port_nets.entry(port.id).or_default();
            if !nets.contains(&net) {
                nets.push(net);
                net_port_entries.push((net.index(), port.id));
            }
        }

        let net_count = net_names.len();
        let mut primary_input_entries = Vec::new();
        primary_input_entries
            .try_reserve_exact(design.ports().len())
            .map_err(|_| packed_row_capacity(opto_core::PackedRowsError::Capacity))?;
        for (port_index, (port, &net)) in design
            .ports()
            .iter()
            .zip(topology.port_nets.iter())
            .enumerate()
        {
            if matches!(
                port.direction,
                TimingPortDirection::Input | TimingPortDirection::Inout
            ) {
                primary_input_entries.push((net.index(), port_index));
            }
        }
        let capacitive_loads = parasitic_nets
            .iter()
            .map(|&parasitic| {
                let capacitance = parasitic
                    .and_then(|id| parasitics.net_by_id(id))
                    .and_then(crate::ParasiticNetRef::annotated_capacitance)
                    .unwrap_or(0.0);
                [capacitance; 2]
            })
            .collect::<Vec<_>>();
        let mut wire_fanouts = vec![0.0; net_count];
        for (port, &net) in design.ports().iter().zip(topology.port_nets.iter()) {
            if matches!(
                port.direction,
                TimingPortDirection::Output | TimingPortDirection::Inout
            ) {
                wire_fanouts[net.index()] += 1.0;
            }
        }

        let mut graph = Self {
            net_count,
            port_nets: std::sync::Arc::new(
                port_nets
                    .into_iter()
                    .map(|(port, nets)| (port, nets.into_boxed_slice()))
                    .collect(),
            ),
            port_bindings: topology.port_nets.into(),
            net_ports: row_arena_from_entries(net_count, net_port_entries)?,
            net_names,
            arcs: GraphArcArena::default(),
            outgoing: RowArena::try_empty(net_count).map_err(packed_row_capacity)?,
            incoming: RowArena::try_empty(net_count).map_err(packed_row_capacity)?,
            primary_inputs: row_arena_from_entries(net_count, primary_input_entries)?,
            sequential_outputs: RowArena::try_empty(net_count).map_err(packed_row_capacity)?,
            topological_order: SharedAppendVec::default(),
            topological_positions: SharedAppendVec::default(),
            propagation_plan: opto_runtime::DependencyPlan::from_topological_order(0, &[], |_| {
                std::iter::empty()
            })?,
            topological_order_stale: false,
            topological_generation: 0,
            cycle_visit_epochs: vec![0; net_count],
            cycle_epoch: 0,
            cycle_stack: Vec::new(),
            capacitive_loads,
            fanout_loads: vec![0.0; net_count],
            wire_load_model: library.wire_load_model.clone(),
            wire_fanouts,
            wire_capacitances: vec![0.0; net_count],
            wire_resistances: vec![0.0; net_count],
            parasitics,
            parasitic_nets,
            constant_values,
            library_cells,
            instance_cells,
            instance_nets,
        };
        for instance in design.instances() {
            graph.adjust_instance_loads_view(
                library,
                instance.id,
                instance.name,
                instance.cell,
                instance.connections().map(|connection| connection.pin),
                1.0,
            )?;
            graph.add_instance_arcs_compact(library, instance)?;
        }
        graph.arcs.seal_base();
        for net in 0..net_count {
            graph.refresh_wire_load(net);
        }
        let order = compute_design_topological_order(&graph)?;
        graph.assign_topological_order(order)?;
        graph.compact_rows()?;
        Ok(graph)
    }

    /// Appends incoming arc sources and latch enables used by the propagation plan.
    pub(super) fn plan_dependencies<'graph>(
        &'graph self,
        arcs: &'graph [GraphArcId],
    ) -> impl Iterator<Item = usize> + 'graph {
        arcs.iter().flat_map(move |&arc| {
            let arc = self.arc(arc);
            let enable = match arc.kind {
                GraphArcKind::Combinational => None,
                GraphArcKind::LatchData { enable_net, .. } => Some(enable_net.index()),
            };
            [Some(arc.from.index()), enable].into_iter().flatten()
        })
    }

    pub(super) fn assign_topological_order(
        &mut self,
        order: Vec<usize>,
    ) -> Result<(), crate::TimingError> {
        let propagation_plan =
            opto_runtime::DependencyPlan::from_topological_order(self.net_count, &order, |net| {
                self.plan_dependencies(&self.incoming[net])
            })?;
        let mut positions = vec![u32::MAX; self.net_count];
        for (position, &net) in order.iter().enumerate() {
            positions[net] = u32::try_from(position)
                .expect("timing graph construction limits topological positions to u32");
        }
        self.topological_positions.replace_base(positions);
        self.topological_order.replace_base(order);
        self.propagation_plan = propagation_plan;
        self.topological_order_stale = false;
        self.topological_generation += 1;
        Ok(())
    }

    /// Counts topological-order and dependency-plan rebuilds.
    #[cfg(test)]
    pub(crate) fn topological_generation(&self) -> u64 {
        self.topological_generation
    }

    pub(crate) fn topological_position(&self, net: usize) -> usize {
        self.topological_positions
            .get(net)
            .map_or(usize::MAX, |&position| position as usize)
    }

    pub(crate) fn ensure_topological_order(&mut self) -> Result<(), crate::TimingError> {
        if self.topological_order_stale {
            let order = compute_topological_order(self)?;
            self.assign_topological_order(order)?;
        }
        Ok(())
    }

    pub(super) fn propagation_worklist(
        &self,
        direction: opto_runtime::DependencyDirection,
        seeds: impl IntoIterator<Item = usize>,
    ) -> Result<opto_runtime::DependencyWorklist<'_>, crate::TimingError> {
        Ok(self.propagation_plan.worklist(direction, seeds)?)
    }

    pub(super) fn propagation_closure(
        &self,
        direction: opto_runtime::DependencyDirection,
        seeds: impl IntoIterator<Item = usize>,
    ) -> Result<Vec<usize>, crate::TimingError> {
        let mut included = vec![false; self.net_count];
        let mut pending = VecDeque::new();
        for seed in seeds {
            let slot = included
                .get_mut(seed)
                .ok_or(crate::TimingAnalysisError::DirtyNetOutOfRange { index: seed })?;
            if !*slot {
                *slot = true;
                pending.push_back(seed);
            }
        }
        while let Some(net) = pending.pop_front() {
            match direction {
                opto_runtime::DependencyDirection::Forward => {
                    for &arc in &self.outgoing[net] {
                        let to = self.arc(arc).to.index();
                        if !included[to] {
                            included[to] = true;
                            pending.push_back(to);
                        }
                    }
                }
                opto_runtime::DependencyDirection::Reverse => {
                    for &arc in &self.incoming[net] {
                        let from = self.arc(arc).from.index();
                        if !included[from] {
                            included[from] = true;
                            pending.push_back(from);
                        }
                    }
                }
            }
        }
        // Consumers require deterministic dependency order, not BFS discovery
        // order. Filter the sealed topological order after marking reachability.
        Ok(self
            .topological_order
            .iter()
            .copied()
            .filter(|&net| included[net])
            .collect())
    }

    pub(super) fn launch_nets(&self) -> impl Iterator<Item = usize> + '_ {
        self.topological_order.iter().copied().filter(|&net| {
            !self.primary_inputs[net].is_empty() || !self.sequential_outputs[net].is_empty()
        })
    }

    pub(crate) fn port_nets(&self, port: PortId) -> &[crate::TimingNetId] {
        self.port_nets.get(&port).map_or(&[], AsRef::as_ref)
    }

    pub(super) fn port_net(&self, port: usize) -> Option<crate::TimingNetId> {
        self.port_bindings.get(port).copied()
    }

    pub(crate) fn instance_nets(
        &self,
        instance: TimingInstanceId,
    ) -> Option<&[crate::TimingNetId]> {
        self.instance_nets.get(instance)
    }

    pub(crate) fn net_is_input_port(&self, net: crate::TimingNetId) -> bool {
        self.primary_inputs
            .get(net.index())
            .is_some_and(|ports| !ports.is_empty())
    }

    pub(crate) fn net_count(&self) -> usize {
        self.net_count
    }

    pub(crate) fn net_has_port(&self, net: usize, port: PortId) -> bool {
        self.net_ports
            .get(net)
            .is_some_and(|ports| ports.contains(&port))
    }

    pub(super) fn parasitic_sink_delay(&self, net: usize, object: &str, edge: TimingEdge) -> f64 {
        self.parasitic_nets
            .get(net)
            .copied()
            .flatten()
            .and_then(|id| self.parasitics.net_by_id(id))
            .and_then(|net| net.sink_delay(object, edge))
            .unwrap_or(0.0)
    }

    pub(super) fn parasitic_sink_delay_parts(
        &self,
        net: usize,
        instance: &str,
        pin: &str,
        edge: TimingEdge,
    ) -> f64 {
        self.parasitic_nets
            .get(net)
            .copied()
            .flatten()
            .and_then(|id| self.parasitics.net_by_id(id))
            .and_then(|net| net.sink_delay_parts(instance, pin, edge))
            .unwrap_or(0.0)
    }

    pub(super) fn wire_resistance(&self, net: usize) -> f64 {
        self.wire_resistances.get(net).copied().unwrap_or(0.0)
    }

    pub(crate) fn endpoint_for_net(&self, net: usize) -> Option<crate::TimingEndpoint> {
        self.net_ports
            .get(net)?
            .first()
            .copied()
            .map(crate::TimingEndpoint::Port)
    }

    pub(crate) fn net_id(&self, name: &str) -> Option<usize> {
        self.net_names.net_id(name)
    }

    pub(crate) fn cell<'a>(
        &self,
        library: &'a TimingLibrary,
        instance: TimingInstanceId,
    ) -> Option<TargetCellRef<'a>> {
        self.instance_cells
            .get(instance)
            .and_then(|id| library.cells.get(id.index()))
    }

    pub(crate) fn instance_cell_index(&self, instance: TimingInstanceId) -> Option<LibraryCellId> {
        self.instance_cells.get(instance)
    }

    pub(super) fn cell_pin_arc<'a>(
        &self,
        library: &'a TimingLibrary,
        instance: TimingInstanceId,
        pin: GraphPinId,
        arc: GraphLibraryArcId,
    ) -> Option<(&'a str, TargetTimingArcRef<'a>)> {
        let cell = self.instance_cell(library, instance)?;
        let pin = cell.pins().nth(pin.index())?;
        Some((pin.name(), pin.timing_arcs().nth(arc.index())?))
    }

    pub(super) fn instance_cell<'a>(
        &self,
        library: &'a TimingLibrary,
        instance: TimingInstanceId,
    ) -> Option<TargetCellRef<'a>> {
        self.instance_cells
            .get(instance)
            .and_then(|cell| library.cells.get(cell.index()))
    }

    pub(super) fn arc(&self, id: GraphArcId) -> GraphArcRef<'_> {
        self.arcs
            .get(id)
            .expect("graph adjacency references one live arc slot")
    }

    fn add_instance_arcs_compact(
        &mut self,
        library: &TimingLibrary,
        instance: crate::model::TimingInstanceView<'_>,
    ) -> Result<(), crate::TimingError> {
        let cell = self.cell(library, instance.id).ok_or_else(|| {
            crate::TimingModelError::UnknownCell {
                instance: instance.name.to_string(),
                cell: instance.cell.to_string(),
            }
        })?;
        let nets = self
            .instance_nets
            .get(instance.id)
            .expect("validated timing instances have typed net bindings");
        let pins = InstancePinRow::build(
            cell,
            instance
                .connections()
                .zip(nets)
                .map(|(connection, &net)| (connection.pin, net)),
        )?;
        self.add_instance_arcs_view(instance.id, instance.name, cell, &pins, None)
    }

    fn add_instance_arcs_view(
        &mut self,
        instance_id: TimingInstanceId,
        instance_name: &str,
        cell: TargetCellRef<'_>,
        pins: &InstancePinRow<'_>,
        mut allocated: Option<&mut Vec<GraphArcId>>,
    ) -> Result<(), crate::TimingError> {
        for (pin_index, &pin) in pins.pins.iter().enumerate() {
            let pin_id = GraphPinId::from_index(pin_index)?;
            for (arc_index, arc) in pin.timing_arcs().enumerate() {
                let arc_id = GraphLibraryArcId::from_index(arc_index)?;
                match arc.timing_type() {
                    timing_type
                        if is_propagation_timing_type(timing_type)
                            && matches!(
                                pin.direction(),
                                TargetPinDirection::Output | TargetPinDirection::Inout
                            ) =>
                    {
                        let Some(related_pin_id) = pins.id_by_name(arc.related_pin()) else {
                            continue;
                        };
                        let (Some(from), Some(to)) = (pins.net(related_pin_id), pins.net(pin_id))
                        else {
                            continue;
                        };
                        let (delay, transition) = sink_response(
                            self.parasitic_nets
                                .get(from.index())
                                .copied()
                                .flatten()
                                .and_then(|id| self.parasitics.net_by_id(id)),
                            instance_name,
                            arc.related_pin(),
                        );
                        let Some(kind) = graph_arc_kind(
                            cell,
                            pin,
                            arc,
                            pins,
                            &self.constant_values,
                            instance_name,
                        )?
                        else {
                            continue;
                        };
                        let (transition, transition_valid) = pack_optional_pair(transition);
                        let topology = GraphArcTopology {
                            from,
                            to,
                            instance: instance_id,
                            pin: pin_id,
                            arc: arc_id,
                            kind,
                        };
                        let values = GraphArcValues {
                            delay,
                            transition,
                            transition_valid,
                        };
                        let id = self.arcs.insert(topology, values)?;
                        self.outgoing.push(from.index(), id);
                        self.incoming.push(to.index(), id);
                        if let Some(allocated) = &mut allocated {
                            allocated.push(id);
                        }
                    }
                    TargetTimingType::ClockToQ(_)
                        if pin.direction() == TargetPinDirection::Output =>
                    {
                        let Some(clock_pin) = pins.id_by_name(arc.related_pin()) else {
                            continue;
                        };
                        let (Some(clock), Some(output)) = (pins.net(clock_pin), pins.net(pin_id))
                        else {
                            continue;
                        };
                        self.sequential_outputs.push(
                            output.index(),
                            SequentialGraphArc {
                                instance: instance_id,
                                pin: pin_id,
                                arc: arc_id,
                                clock_net: clock,
                                element: sequential_element_for_control(cell, arc.related_pin()),
                            },
                        );
                    }
                    _ => {
                        // These relations constrain asynchronous or
                        // non-register events; they do not contribute
                        // propagation edges to the setup timing graph.
                    }
                }
            }
        }
        Ok(())
    }
}

const fn is_propagation_timing_type(timing_type: TargetTimingType) -> bool {
    matches!(
        timing_type,
        TargetTimingType::Combinational
            | TargetTimingType::Clear
            | TargetTimingType::Preset
            | TargetTimingType::ThreeStateEnable
            | TargetTimingType::ThreeStateDisable
    )
}

fn pack_optional_pair(values: [Option<f64>; 2]) -> ([f64; 2], u8) {
    let mut packed = [0.0; 2];
    let mut valid = 0;
    for (index, value) in values.into_iter().enumerate() {
        if let Some(value) = value {
            packed[index] = value;
            valid |= 1 << index;
        }
    }
    (packed, valid)
}

fn packed_row_capacity(_: opto_core::PackedRowsError) -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "timing graph adjacency",
    }
    .into()
}

fn remap_arc_rows(
    rows: &RowArena<GraphArcId>,
    remap: &[Option<GraphArcId>],
) -> Result<RowArena<GraphArcId>, crate::TimingError> {
    let mut rebuilt =
        opto_core::RowArenaBuilder::try_with_capacity(rows.len()).map_err(packed_row_capacity)?;
    let mut scratch = Vec::new();
    for row in rows.iter() {
        scratch.clear();
        scratch
            .try_reserve(row.len())
            .map_err(|_| crate::TimingModelError::Capacity {
                resource: "live graph arc remap",
            })?;
        for id in row {
            scratch.push(remap.get(id.index()).copied().flatten().ok_or(
                crate::TimingModelError::Capacity {
                    resource: "live graph arc remap",
                },
            )?);
        }
        rebuilt
            .try_push_row(scratch.drain(..))
            .map_err(packed_row_capacity)?;
    }
    Ok(rebuilt.finish())
}

fn row_arena_from_entries<T: Copy + Ord>(
    row_count: usize,
    mut entries: Vec<(usize, T)>,
) -> Result<RowArena<T>, crate::TimingError> {
    entries.sort_unstable();
    entries.dedup();
    if entries.last().is_some_and(|(row, _)| *row >= row_count) {
        return Err(crate::TimingAnalysisError::InconsistentTopology.into());
    }
    let mut rows =
        opto_core::RowArenaBuilder::try_with_capacity(row_count).map_err(packed_row_capacity)?;
    let mut first = 0;
    for row in 0..row_count {
        let mut end = first;
        while entries
            .get(end)
            .is_some_and(|(candidate, _)| *candidate == row)
        {
            end += 1;
        }
        rows.try_push_row(entries[first..end].iter().map(|&(_, value)| value))
            .map_err(packed_row_capacity)?;
        first = end;
    }
    debug_assert_eq!(first, entries.len());
    Ok(rows.finish())
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn instance_cells_use_compact_direct_slots_and_trim_removed_tail() {
        let mut cells = InstanceCellArena::default();
        let instance = TimingInstanceId::from_raw(42);
        let cell = LibraryCellId::from_index(7).unwrap();

        assert_eq!(cells.insert(instance, cell).unwrap(), None);
        assert_eq!(cells.get(instance), Some(cell));
        assert_eq!(cells.len(), 43);
        assert!(
            cells.shared_pages().map(|(_, bytes)| bytes).sum::<usize>()
                >= 43 * std::mem::size_of::<u32>()
        );

        assert_eq!(cells.remove(instance).unwrap(), Some(cell));
        cells.trim();
        assert_eq!(cells.len(), 0);
    }
}
