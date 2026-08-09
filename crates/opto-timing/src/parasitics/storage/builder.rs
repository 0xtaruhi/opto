// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::parasitics::network::ComputedNet;

impl ParasiticStoreBuilder {
    pub(super) fn push_computed(
        &mut self,
        computed: ComputedNet,
    ) -> Result<(), crate::TimingError> {
        self.check_net_capacity()?;
        let ComputedNet {
            name,
            total_capacitance,
            load_annotated,
            delay_model,
            pin_capacitance_included,
            nodes,
            resistors,
            connections,
        } = computed;
        let node_start = checked_start(self.nodes.len(), "parasitic node arena")?;
        let resistor_start = checked_start(self.resistors.len(), "parasitic resistor arena")?;
        let connection_start = checked_start(self.connections.len(), "parasitic connection arena")?;
        let node_count = checked_count(nodes.len(), "parasitic nodes per net")?;
        let resistor_count = checked_count(resistors.len(), "parasitic resistors per net")?;
        let connection_count = checked_count(connections.len(), "parasitic connections per net")?;
        let name_id = self.intern_name(&name)?;
        for node in nodes {
            let name = self.intern_name(&node.name)?;
            self.nodes.push(ParasiticNode {
                name,
                ground_capacitance_farads: node.ground_capacitance_farads,
            });
        }
        for resistor in resistors {
            self.resistors.push(ParasiticResistor {
                first: node_start
                    .checked_add(resistor.first)
                    .ok_or_else(|| capacity("parasitic node arena"))?,
                second: node_start
                    .checked_add(resistor.second)
                    .ok_or_else(|| capacity("parasitic node arena"))?,
                resistance_ohms: resistor.resistance_ohms,
            });
        }
        for connection in connections {
            let object = self.intern_name(&connection.object)?;
            self.connections.push(ParasiticConnection {
                object,
                node: node_start
                    .checked_add(connection.node)
                    .ok_or_else(|| capacity("parasitic node arena"))?,
                role: connection.role,
                pin_capacitance_farads: connection.pin_capacitance_farads,
                delay: connection.delay,
                transition: connection.transition,
            });
        }
        self.nets.push(ParasiticNet {
            name: name_id,
            total_capacitance,
            load_annotated,
            delay_model,
            pin_capacitance_included,
            node_start,
            node_count,
            resistor_start,
            resistor_count,
            connection_start,
            connection_count,
        });
        Ok(())
    }

    pub(super) fn push_ref(
        &mut self,
        source: ParasiticNetRef<'_>,
        retained: Option<ParasiticNetRef<'_>>,
    ) -> Result<(), crate::TimingError> {
        self.check_net_capacity()?;
        let node_start = checked_start(self.nodes.len(), "parasitic node arena")?;
        let resistor_start = checked_start(self.resistors.len(), "parasitic resistor arena")?;
        let connection_start = checked_start(self.connections.len(), "parasitic connection arena")?;
        let source_nodes = source.nodes()?;
        let source_resistors = source.resistors()?;
        let source_connections = source.connections()?;
        let node_count = checked_count(source_nodes.len(), "parasitic nodes per net")?;
        let resistor_count = checked_count(source_resistors.len(), "parasitic resistors per net")?;
        let connection_count =
            checked_count(source_connections.len(), "parasitic connections per net")?;
        let name = self.intern_name(source.required_name(source.net.name)?)?;
        for node in source_nodes {
            let name = self.intern_name(source.required_name(node.name)?)?;
            self.nodes.push(ParasiticNode {
                name,
                ground_capacitance_farads: node.ground_capacitance_farads,
            });
        }
        for resistor in source_resistors {
            self.resistors.push(ParasiticResistor {
                first: rebase_node(resistor.first, source.net.node_start, node_start)?,
                second: rebase_node(resistor.second, source.net.node_start, node_start)?,
                resistance_ohms: resistor.resistance_ohms,
            });
        }
        for connection in source_connections {
            let object_name = source.required_name(connection.object)?;
            let object = self.intern_name(object_name)?;
            let retained_response = retained
                .and_then(|net| net.connection_with_role(connection.role, object_name))
                .filter(|old| old.delay.is_some() || old.transition.is_some());
            self.connections.push(ParasiticConnection {
                object,
                node: rebase_node(connection.node, source.net.node_start, node_start)?,
                role: connection.role,
                pin_capacitance_farads: connection.pin_capacitance_farads,
                delay: retained_response.map_or(connection.delay, |old| old.delay),
                transition: retained_response.map_or(connection.transition, |old| old.transition),
            });
        }
        self.nets.push(ParasiticNet {
            name,
            total_capacitance: source.net.total_capacitance,
            load_annotated: source.net.load_annotated,
            delay_model: source.net.delay_model,
            pin_capacitance_included: source.net.pin_capacitance_included,
            node_start,
            node_count,
            resistor_start,
            resistor_count,
            connection_start,
            connection_count,
        });
        Ok(())
    }

    fn intern_name(&mut self, name: &str) -> Result<NameId, crate::TimingError> {
        if name.is_empty() {
            return Err(invalid_net(
                "<database>",
                "parasitic names must not be empty",
            ));
        }
        self.names
            .intern(name)
            .map_err(|_| capacity("parasitic name arena"))
    }

    fn check_net_capacity(&self) -> Result<(), crate::TimingError> {
        let count = self
            .nets
            .len()
            .checked_add(1)
            .ok_or_else(|| capacity("parasitic net arena"))?;
        checked_count(count, "parasitic net arena").map(|_| ())
    }

    pub(super) fn finish(mut self) -> ParasiticStore {
        self.names.compact();
        ParasiticStore {
            names: self.names,
            nets: self.nets.into_boxed_slice(),
            nodes: self.nodes.into_boxed_slice(),
            resistors: self.resistors.into_boxed_slice(),
            connections: self.connections.into_boxed_slice(),
        }
    }
}
