// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl TimingGraph {
    #[allow(
        clippy::too_many_lines,
        reason = "view forking shares immutable topology while rebuilding every value column and \
                  memory account under the follower library and parasitics"
    )]
    pub(crate) fn fork_view(
        &self,
        design: &crate::model::SharedTimingDesign,
        positions: &crate::model::InstancePositions,
        library: &TimingLibrary,
        parasitics: crate::Parasitics,
    ) -> Result<Self, crate::TimingError> {
        validate_parasitics(&parasitics, &self.net_names, &self.instance_nets, design)?;
        let library_cells = LibraryCellIndex::build(library)?;
        let instance_cells = self.instance_cells.fork_shared();
        let parasitic_nets = self
            .net_names
            .iter()
            .map(|name| parasitics.net_id(name))
            .collect::<Vec<_>>();
        let capacitive_loads = parasitic_nets
            .iter()
            .map(|&id| {
                let capacitance = id
                    .and_then(|id| parasitics.net_by_id(id))
                    .and_then(crate::ParasiticNetRef::annotated_capacitance)
                    .unwrap_or(0.0);
                [capacitance; 2]
            })
            .collect();
        let arcs = self
            .arcs
            .fork_base_with(|arc| {
                let instance = design
                    .instance(
                        positions
                            .get(arc.instance)
                            .expect("prepared topology instance remains live"),
                    )
                    .expect("prepared topology instance remains live");
                let cell = instance_cells
                    .get(arc.instance)
                    .and_then(|cell| library.cells.get(cell.index()))
                    .expect("matching topology schema preserves every cell");
                let related_pin = cell
                    .pins()
                    .nth(arc.pin.index())
                    .and_then(|pin| pin.timing_arcs().nth(arc.arc.index()))
                    .expect("matching topology schema preserves pin order");
                let (delay, transition) = sink_response(
                    parasitic_nets[arc.from.index()].and_then(|id| parasitics.net_by_id(id)),
                    instance.name,
                    related_pin.related_pin(),
                );
                let (transition, transition_valid) = pack_optional_pair(transition);
                GraphArcValues {
                    delay,
                    transition,
                    transition_valid,
                }
            })
            .ok_or(crate::TimingAnalysisError::InconsistentTopology)?;
        let mut graph = Self {
            net_count: self.net_count,
            port_nets: self.port_nets.clone(),
            port_bindings: self.port_bindings.clone(),
            net_ports: self
                .net_ports
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            net_names: self
                .net_names
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            arcs,
            outgoing: self
                .outgoing
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            incoming: self
                .incoming
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            primary_inputs: self
                .primary_inputs
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            sequential_outputs: self
                .sequential_outputs
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            topological_order: self
                .topological_order
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            topological_positions: self
                .topological_positions
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
            propagation_plan: self.propagation_plan.clone(),
            topological_order_stale: false,
            topological_generation: self.topological_generation,
            cycle_visit_epochs: vec![0; self.net_count],
            cycle_epoch: 0,
            cycle_stack: Vec::new(),
            capacitive_loads,
            fanout_loads: vec![0.0; self.net_count],
            wire_load_model: library.wire_load_model.clone(),
            wire_fanouts: vec![0.0; self.net_count],
            wire_capacitances: vec![0.0; self.net_count],
            wire_resistances: vec![0.0; self.net_count],
            parasitics,
            parasitic_nets,
            constant_values: self.constant_values.clone(),
            library_cells,
            instance_cells,
            instance_nets: self
                .instance_nets
                .fork_shared()
                .ok_or(crate::TimingAnalysisError::InconsistentTopology)?,
        };
        for (port, &net) in design.ports().iter().zip(graph.port_bindings.iter()) {
            if matches!(
                port.direction,
                TimingPortDirection::Output | TimingPortDirection::Inout
            ) {
                graph.wire_fanouts[net.index()] += 1.0;
            }
        }
        for instance in design.instances() {
            graph.adjust_instance_loads_view(
                library,
                instance.id,
                instance.name,
                instance.cell,
                instance.connections().map(|connection| connection.pin),
                1.0,
            )?;
        }
        for net in 0..graph.net_count {
            graph.refresh_wire_load(net);
        }
        Ok(graph)
    }
}
