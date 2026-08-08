// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl ParasiticStore {
    #[allow(
        clippy::too_many_lines,
        reason = "checkpoint validation proves cross-arena ownership, ordering, ranges, names, \
                  electrical values, and driver presence in one invariant pass"
    )]
    pub(super) fn validate_checkpoint(&self) -> Result<(), crate::TimingError> {
        checked_count(self.nets.len(), "parasitic net arena")?;
        if self.names.resolve(NameId::default()) != Some("") {
            return Err(invalid_net(
                "<database>",
                "parasitic name table is missing its reserved empty name",
            ));
        }
        if self.nets.is_empty() {
            if self.names.entry_count() != 1
                || !self.nodes.is_empty()
                || !self.resistors.is_empty()
                || !self.connections.is_empty()
            {
                return Err(invalid_net(
                    "<database>",
                    "empty parasitics retain unowned arena records",
                ));
            }
            return Ok(());
        }

        let mut next_name_id = 1u32;
        let mut node_end = 0usize;
        let mut resistor_end = 0usize;
        let mut connection_end = 0usize;
        let mut previous_net = None;
        for net in &self.nets {
            let net_name = self.validate_name(net.name, &mut next_name_id)?;
            if previous_net.is_some_and(|previous: &str| previous >= net_name) {
                return Err(invalid_net(
                    "<database>",
                    "parasitic nets are not strictly name-sorted",
                ));
            }
            previous_net = Some(net_name);
            if !net.total_capacitance.is_finite() || net.total_capacitance < 0.0 {
                return Err(invalid_net(
                    "<database>",
                    "parasitic total capacitance is invalid",
                ));
            }

            let nodes = canonical_range(
                net.node_start,
                net.node_count,
                node_end,
                self.nodes.len(),
                "node",
            )?;
            let resistors = canonical_range(
                net.resistor_start,
                net.resistor_count,
                resistor_end,
                self.resistors.len(),
                "resistor",
            )?;
            let connections = canonical_range(
                net.connection_start,
                net.connection_count,
                connection_end,
                self.connections.len(),
                "connection",
            )?;
            if nodes.is_empty() {
                return Err(invalid_net("<database>", "parasitic net has no nodes"));
            }
            node_end = nodes.end;
            resistor_end = resistors.end;
            connection_end = connections.end;

            for node in &self.nodes[nodes.clone()] {
                self.validate_name(node.name, &mut next_name_id)?;
                if !node.ground_capacitance_farads.is_finite()
                    || node.ground_capacitance_farads < 0.0
                {
                    return Err(invalid_net(
                        "<database>",
                        "parasitic node capacitance is invalid",
                    ));
                }
            }
            for resistor in &self.resistors[resistors] {
                if !nodes.contains(&(resistor.first as usize))
                    || !nodes.contains(&(resistor.second as usize))
                    || resistor.first == resistor.second
                    || !resistor.resistance_ohms.is_finite()
                    || resistor.resistance_ohms <= 0.0
                {
                    return Err(invalid_net("<database>", "parasitic resistor is invalid"));
                }
            }
            let mut previous_connection = None;
            let mut has_driver = false;
            for connection in &self.connections[connections] {
                let object = self.validate_name(connection.object, &mut next_name_id)?;
                let key = (connection.role, object);
                if previous_connection.is_some_and(|previous| previous >= key)
                    || !nodes.contains(&(connection.node as usize))
                    || connection
                        .pin_capacitance_farads
                        .iter()
                        .any(|value| !value.is_finite() || *value < 0.0)
                    || !valid_response(connection.delay)
                    || !valid_response(connection.transition)
                {
                    return Err(invalid_net("<database>", "parasitic connection is invalid"));
                }
                previous_connection = Some(key);
                has_driver |= connection.role == RcConnectionRole::Driver;
            }
            if !has_driver {
                return Err(invalid_net("<database>", "parasitic net has no driver"));
            }
        }
        if node_end != self.nodes.len()
            || resistor_end != self.resistors.len()
            || connection_end != self.connections.len()
            || next_name_id as usize != self.names.entry_count()
        {
            return Err(invalid_net(
                "<database>",
                "parasitic arenas contain unowned or noncanonical records",
            ));
        }
        Ok(())
    }

    fn validate_name<'a>(
        &'a self,
        id: NameId,
        next_name: &mut u32,
    ) -> Result<&'a str, crate::TimingError> {
        let name = required_store_name(&self.names, id)?;
        if id.raw() > *next_name {
            return Err(invalid_net(
                "<database>",
                "parasitic names are not interned in canonical first-use order",
            ));
        }
        if id.raw() == *next_name {
            *next_name = next_name
                .checked_add(1)
                .ok_or_else(|| capacity("parasitic name arena"))?;
        }
        Ok(name)
    }
}
