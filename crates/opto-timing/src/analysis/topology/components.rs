// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl TimingGraph {
    pub(crate) fn construction_scratch_high_water_bytes(&self) -> usize {
        let nets = self.net_count;
        let arcs = self.arcs.len();
        let topological_sort = opto_core::resident::slice_bytes::<usize>(nets.saturating_mul(3));
        let dependency_edges = arcs.saturating_mul(2);
        let dependency_plan =
            opto_core::resident::slice_bytes::<(usize, usize)>(dependency_edges.saturating_mul(2))
                .saturating_add(opto_core::resident::slice_bytes::<u32>(nets));
        topological_sort.max(dependency_plan)
    }

    pub(crate) fn shared_components(&self) -> Vec<crate::SharedTimingComponent> {
        use crate::{SharedTimingComponent, SharedTimingComponentKind as Kind};
        let mut components = Vec::with_capacity(17);
        let mut push = |kind, identity, bytes| {
            if bytes != 0 {
                components.push(SharedTimingComponent {
                    kind,
                    identity,
                    bytes,
                });
            }
        };
        push(
            Kind::GraphArcs,
            self.arcs.shared_identity(),
            self.arcs.shared_memory_bytes(),
        );
        push(
            Kind::NetNames,
            self.net_names.shared_identity(),
            self.net_names.shared_memory_bytes(),
        );
        let port_rows = self
            .port_nets
            .values()
            .map(|row| opto_core::resident::slice_bytes::<crate::TimingNetId>(row.len()))
            .sum::<usize>();
        push(
            Kind::PortNets,
            std::sync::Arc::as_ptr(&self.port_nets) as usize,
            btree_bytes::<PortId, Box<[crate::TimingNetId]>>(self.port_nets.len())
                .saturating_add(port_rows),
        );
        push(
            Kind::PortBindings,
            std::sync::Arc::as_ptr(&self.port_bindings).cast::<crate::TimingNetId>() as usize,
            opto_core::resident::slice_bytes::<crate::TimingNetId>(self.port_bindings.len()),
        );
        for (identity, bytes) in self.net_ports.shared_pages() {
            push(Kind::NetPorts, identity, bytes);
        }
        for (identity, bytes) in self.outgoing.shared_pages() {
            push(Kind::OutgoingArcs, identity, bytes);
        }
        for (identity, bytes) in self.incoming.shared_pages() {
            push(Kind::IncomingArcs, identity, bytes);
        }
        for (identity, bytes) in self.primary_inputs.shared_pages() {
            push(Kind::PrimaryInputs, identity, bytes);
        }
        for (identity, bytes) in self.sequential_outputs.shared_pages() {
            push(Kind::SequentialOutputs, identity, bytes);
        }
        for (identity, bytes) in self.instance_nets.shared_allocations() {
            push(Kind::InstanceNets, identity, bytes);
        }
        for (identity, bytes) in self.instance_cells.shared_pages() {
            push(Kind::InstanceCells, identity, bytes);
        }
        push(
            Kind::TopologicalOrder,
            self.topological_order.shared_identity(),
            self.topological_order.shared_memory_bytes(),
        );
        push(
            Kind::TopologicalPositions,
            self.topological_positions.shared_identity(),
            self.topological_positions.shared_memory_bytes(),
        );
        for (kind, (identity, bytes)) in [
            Kind::DependencyPredecessors,
            Kind::DependencySuccessors,
            Kind::DependencyPositions,
        ]
        .into_iter()
        .zip(self.propagation_plan.shared_components())
        {
            push(kind, identity, bytes);
        }
        components
    }
}
