// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! One-shot construction of an unpublished mapped netlist.
//!
//! The builder owns all name and connectivity arenas. Freezing transfers those
//! owners without cloning and validates names plus intrusive pin adjacency.

use super::{
    CellId, CellSlot, ConnectionSignal, DesignInstanceConnection, DesignInstanceId, MappedCell,
    MappedDesignInstance, MappedError, MappedGenerationId, MappedNetlist, MappedPort, NameId,
    NameTable, NetId, NetPins, NetSlot, PinConnection, PinId, PinLinks, PortDirection, PortId,
    RevisionId, external,
};
use std::collections::BTreeSet;

#[derive(Debug)]
/// One cell row prepared for deterministic packed mapped construction.
pub struct MappedCellSpec {
    /// Unique mapped instance name.
    pub name: String,
    /// Target-library cell type.
    pub cell_type: String,
    /// Optional dense target-library cell index.
    pub library_cell: Option<u32>,
    /// Pin name, optional target-library pin ID, and connected signal in pin order.
    pub connections: Vec<(String, Option<u16>, ConnectionSignal)>,
}

struct PreparedMappedCell {
    name: NameId,
    cell_type: NameId,
    library_cell: Option<u32>,
    connections: Vec<PinConnection>,
}

#[derive(Debug)]
/// Append-only owner used to construct one mapped netlist.
pub struct MappedBuilder {
    base_revision: RevisionId,
    name: NameId,
    names: NameTable,
    nets: Vec<NetSlot>,
    ports: Vec<MappedPort>,
    port_nets: Vec<NetId>,
    cells: Vec<CellSlot>,
    connections: Vec<PinConnection>,
    pin_owners: Vec<CellId>,
    pin_links: Vec<PinLinks>,
    net_pins: Vec<NetPins>,
    design_instances: Vec<MappedDesignInstance>,
    design_connections: Vec<DesignInstanceConnection>,
    design_connection_signals: Vec<ConnectionSignal>,
    constant_drivers: Vec<(NetId, bool)>,
}

impl MappedBuilder {
    /// Creates an empty builder for `name` synthesized from `base_revision`.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when the design name cannot be interned.
    pub fn new(name: &str, base_revision: RevisionId) -> Result<Self, MappedError> {
        if name.trim().is_empty() {
            return Err(MappedError::invariant("mapped design name cannot be empty"));
        }
        let mut names = NameTable::new();
        let name = names.intern(name)?;
        Ok(Self {
            base_revision,
            name,
            names,
            nets: Vec::new(),
            ports: Vec::new(),
            port_nets: Vec::new(),
            cells: Vec::new(),
            connections: Vec::new(),
            pin_owners: Vec::new(),
            pin_links: Vec::new(),
            net_pins: Vec::new(),
            design_instances: Vec::new(),
            design_connections: Vec::new(),
            design_connection_signals: Vec::new(),
            constant_drivers: Vec::new(),
        })
    }

    /// Adds a live canonical net with an optional name.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] on name-table or 32-bit arena capacity failure.
    pub fn add_net(&mut self, name: Option<&str>) -> Result<NetId, MappedError> {
        if name.is_some_and(|name| name.trim().is_empty()) {
            return Err(MappedError::invariant(
                "mapped net name cannot be empty; use an unnamed net instead",
            ));
        }
        let id = NetId::from_index(self.nets.len())?;
        let name = name.map(|name| self.names.intern(name)).transpose()?;
        self.nets.push(NetSlot {
            name,
            live: true,
            version: 0,
        });
        self.net_pins.push(NetPins::default());
        Ok(id)
    }

    /// Adds a vector port connected to existing scalar nets.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] for an empty or duplicate port name, an empty
    /// binding, an unknown net, or a capacity failure.
    pub fn add_port(
        &mut self,
        name: &str,
        direction: PortDirection,
        nets: &[NetId],
    ) -> Result<PortId, MappedError> {
        if name.trim().is_empty() {
            return Err(MappedError::invariant("mapped port name cannot be empty"));
        }
        if nets.is_empty() {
            return Err(MappedError::invariant(format!(
                "mapped port '{name}' has no connected bits"
            )));
        }
        if self.ports.iter().any(|port| {
            self.names
                .resolve(port.name)
                .is_some_and(|existing| existing == name)
        }) {
            return Err(MappedError::invariant(format!(
                "duplicate mapped port name '{name}'"
            )));
        }
        for net in nets {
            if !self.nets.get(net.index()).is_some_and(|slot| slot.live) {
                return Err(MappedError::invariant(format!(
                    "mapped port references unknown net {net:?}"
                )));
            }
        }
        let id = PortId::from_index(self.ports.len())?;
        let start = u32::try_from(self.port_nets.len())
            .map_err(|_| MappedError::capacity("port connection arena"))?;
        let final_net_count = self
            .port_nets
            .len()
            .checked_add(nets.len())
            .ok_or_else(|| MappedError::capacity("port connection arena"))?;
        let end = u32::try_from(final_net_count)
            .map_err(|_| MappedError::capacity("port connection arena"))?;
        let name = self.names.intern(name)?;
        self.port_nets.extend_from_slice(nets);
        self.ports.push(MappedPort {
            name,
            direction,
            net_start: start,
            net_end: end,
        });
        Ok(id)
    }

    /// Adds a target-library cell and all of its pin connections atomically.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when a connection names an unknown net, a name
    /// cannot be interned, or any compact arena exceeds capacity.
    pub fn add_cell(
        &mut self,
        name: &str,
        cell_type: &str,
        library_cell: Option<u32>,
        connections: &[(String, Option<u16>, ConnectionSignal)],
    ) -> Result<CellId, MappedError> {
        let ids = self.add_cells_packed(vec![MappedCellSpec {
            name: name.to_string(),
            cell_type: cell_type.to_string(),
            library_cell,
            connections: connections.to_vec(),
        }])?;
        Ok(ids[0])
    }

    /// Appends a complete ordered cell range after one capacity/connectivity
    /// preflight. Returned IDs are dense and align one-to-one with `cells`.
    ///
    /// This is the deterministic reduction boundary for region-local cell
    /// rows: workers prepare independent specs, while the coordinator assigns
    /// one exact global cell/pin prefix without repeated builder calls.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] before mutation for arena capacity overflow,
    /// unknown nets, empty/duplicate names, duplicate pins, or an unavailable
    /// name-table entry.
    pub fn add_cells_packed(
        &mut self,
        cells: Vec<MappedCellSpec>,
    ) -> Result<Box<[CellId]>, MappedError> {
        let first_cell = self.cells.len();
        let first_pin = self.connections.len();
        let final_cell_count = first_cell
            .checked_add(cells.len())
            .ok_or_else(|| MappedError::capacity("cell arena"))?;
        let mut added_names = BTreeSet::new();
        let added_pins = cells.iter().try_fold(0usize, |count, cell| {
            if cell.name.trim().is_empty() || cell.cell_type.trim().is_empty() {
                return Err(MappedError::invariant(
                    "mapped cells require non-empty instance and cell type names",
                ));
            }
            if self.instance_name_exists(&cell.name) || !added_names.insert(cell.name.as_str()) {
                return Err(MappedError::invariant(format!(
                    "duplicate mapped instance name '{}'",
                    cell.name
                )));
            }
            let mut pins = BTreeSet::new();
            for (pin, _, signal) in &cell.connections {
                if pin.trim().is_empty() || !pins.insert(pin.as_str()) {
                    return Err(MappedError::invariant(format!(
                        "mapped cell '{}' has an empty or duplicate pin name",
                        cell.name
                    )));
                }
                if let ConnectionSignal::Net(net) = signal
                    && !self.nets.get(net.index()).is_some_and(|slot| slot.live)
                {
                    return Err(MappedError::invariant(format!(
                        "mapped cell references unknown net {net:?}"
                    )));
                }
            }
            count
                .checked_add(cell.connections.len())
                .ok_or_else(|| MappedError::capacity("pin connection arena"))
        })?;
        let final_pin_count = first_pin
            .checked_add(added_pins)
            .ok_or_else(|| MappedError::capacity("pin connection arena"))?;
        if final_cell_count != 0 {
            let _ = CellId::from_index(final_cell_count - 1)?;
        }
        if final_pin_count != 0 {
            let _ = PinId::from_index(final_pin_count - 1)?;
        }

        let name_checkpoint = self.names.checkpoint();
        let prepared = cells
            .into_iter()
            .map(|cell| {
                let name = self.names.intern(&cell.name)?;
                let cell_type = self.names.intern(&cell.cell_type)?;
                let connections = cell
                    .connections
                    .into_iter()
                    .map(|(pin, library_pin, signal)| {
                        Ok(PinConnection {
                            pin: self.names.intern(&pin)?,
                            library_pin,
                            signal,
                        })
                    })
                    .collect::<Result<Vec<_>, MappedError>>()?;
                Ok(PreparedMappedCell {
                    name,
                    cell_type,
                    library_cell: cell.library_cell,
                    connections,
                })
            })
            .collect::<Result<Vec<_>, MappedError>>();
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.names.rollback(name_checkpoint)?;
                return Err(error);
            }
        };

        self.cells.reserve(prepared.len());
        self.connections.reserve(added_pins);
        self.pin_owners.reserve(added_pins);
        self.pin_links.reserve(added_pins);
        let mut ids = Vec::with_capacity(prepared.len());
        for (offset, cell) in prepared.into_iter().enumerate() {
            let id = CellId::from_index(first_cell + offset)?;
            let start = u32::try_from(self.connections.len())
                .map_err(|_| MappedError::capacity("pin connection arena"))?;
            for connection in cell.connections {
                let pin_id = PinId::from_index(self.connections.len())?;
                let signal = connection.signal;
                self.connections.push(connection);
                self.pin_owners.push(id);
                self.pin_links.push(PinLinks::default());
                if let ConnectionSignal::Net(net) = signal {
                    append_pin(&mut self.net_pins, &mut self.pin_links, net, pin_id);
                }
            }
            let end = u32::try_from(self.connections.len())
                .map_err(|_| MappedError::capacity("pin connection arena"))?;
            self.cells.push(CellSlot {
                cell: MappedCell {
                    name: cell.name,
                    cell_type: cell.cell_type,
                    library_cell: cell.library_cell,
                    connection_start: start,
                    connection_end: end,
                },
                live: true,
                version: 0,
            });
            ids.push(id);
        }
        debug_assert_eq!(self.cells.len(), final_cell_count);
        debug_assert_eq!(self.connections.len(), final_pin_count);
        Ok(ids.into_boxed_slice())
    }

    /// Adds a retained design occurrence and its vector port bindings.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] for an empty binding, an unknown net, a
    /// name-table failure, or compact arena capacity exhaustion.
    pub fn add_design_instance(
        &mut self,
        name: &str,
        module: &str,
        connections: &[(String, Vec<ConnectionSignal>)],
    ) -> Result<DesignInstanceId, MappedError> {
        if name.trim().is_empty() || module.trim().is_empty() {
            return Err(MappedError::invariant(
                "mapped design instances require non-empty instance and module names",
            ));
        }
        if self.instance_name_exists(name) {
            return Err(MappedError::invariant(format!(
                "duplicate mapped instance name '{name}'"
            )));
        }
        let id = DesignInstanceId::from_index(self.design_instances.len())?;
        let connection_start = u32::try_from(self.design_connections.len())
            .map_err(|_| MappedError::capacity("design instance connection arena"))?;
        let final_connection_count = self
            .design_connections
            .len()
            .checked_add(connections.len())
            .ok_or_else(|| MappedError::capacity("design instance connection arena"))?;
        let connection_end = u32::try_from(final_connection_count)
            .map_err(|_| MappedError::capacity("design instance connection arena"))?;
        let mut final_signal_count = self.design_connection_signals.len();
        let mut ports = BTreeSet::new();
        for (port, signals) in connections {
            if port.trim().is_empty() || !ports.insert(port.as_str()) {
                return Err(MappedError::invariant(format!(
                    "mapped design instance '{name}' has an empty or duplicate port binding"
                )));
            }
            if signals.is_empty() {
                return Err(MappedError::invariant(format!(
                    "mapped design instance '{name}' port '{port}' has no connected bits"
                )));
            }
            for signal in signals {
                if let ConnectionSignal::Net(net) = signal
                    && !self.nets.get(net.index()).is_some_and(|slot| slot.live)
                {
                    return Err(MappedError::invariant(format!(
                        "mapped design instance references unknown net {net:?}"
                    )));
                }
            }
            final_signal_count = final_signal_count
                .checked_add(signals.len())
                .ok_or_else(|| MappedError::capacity("design instance signal arena"))?;
        }
        let _ = u32::try_from(final_signal_count)
            .map_err(|_| MappedError::capacity("design instance signal arena"))?;

        let name_checkpoint = self.names.checkpoint();
        let prepared = (|| {
            let instance_name = self.names.intern(name)?;
            let module_name = self.names.intern(module)?;
            let bindings = connections
                .iter()
                .map(|(port, signals)| Ok((self.names.intern(port)?, signals.as_slice())))
                .collect::<Result<Vec<_>, MappedError>>()?;
            Ok((instance_name, module_name, bindings))
        })();
        let (instance_name, module_name, bindings) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.names.rollback(name_checkpoint)?;
                return Err(error);
            }
        };

        self.design_connections.reserve(connections.len());
        self.design_connection_signals
            .reserve(final_signal_count - self.design_connection_signals.len());
        for (port, signals) in bindings {
            let signal_start = u32::try_from(self.design_connection_signals.len())
                .map_err(|_| MappedError::capacity("design instance signal arena"))?;
            self.design_connection_signals.extend_from_slice(signals);
            let signal_end = u32::try_from(self.design_connection_signals.len())
                .map_err(|_| MappedError::capacity("design instance signal arena"))?;
            self.design_connections.push(DesignInstanceConnection {
                port,
                signal_start,
                signal_end,
            });
        }
        self.design_instances.push(MappedDesignInstance {
            name: instance_name,
            module: module_name,
            connection_start,
            connection_end,
        });
        Ok(id)
    }

    fn instance_name_exists(&self, name: &str) -> bool {
        self.cells.iter().any(|slot| {
            slot.live
                && self
                    .names
                    .resolve(slot.cell.name)
                    .is_some_and(|existing| existing == name)
        }) || self.design_instances.iter().any(|instance| {
            self.names
                .resolve(instance.name)
                .is_some_and(|existing| existing == name)
        })
    }

    /// Adds an explicit Boolean driver for `net`.
    ///
    /// Net existence is checked when the builder is frozen.
    pub fn drive_constant(&mut self, net: NetId, value: bool) {
        self.constant_drivers.push((net, value));
    }

    /// Transfers all arenas into an unpublished netlist and validates names and
    /// adjacency.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when pin adjacency or a referenced object is
    /// inconsistent.
    pub fn freeze(self) -> Result<MappedNetlist, MappedError> {
        let live_net_count = self.nets.len();
        let live_cell_count = self.cells.len();
        let external_nets = external::build_external_net_index(
            &self.port_nets,
            &self.constant_drivers,
            &self.design_connection_signals,
        );
        let netlist = MappedNetlist {
            generation: MappedGenerationId::fresh(),
            base_revision: self.base_revision,
            edit_revision: 0,
            published: false,
            name: self.name,
            names: self.names,
            nets: self.nets,
            live_net_count,
            ports: self.ports,
            port_nets: self.port_nets,
            cells: self.cells,
            live_cell_count,
            connections: self.connections,
            pin_owners: self.pin_owners,
            pin_links: self.pin_links,
            net_pins: self.net_pins,
            design_instances: self.design_instances,
            design_connections: self.design_connections,
            design_connection_signals: self.design_connection_signals,
            constant_drivers: self.constant_drivers,
            external_nets,
        };
        netlist.validate_live_counts()?;
        let mut scratch = netlist.validation_scratch();
        netlist.validate_external_net_index(&mut scratch[..netlist.nets.len()])?;
        scratch.fill(0);
        netlist.validate_connectivity(&mut scratch[..netlist.connections.len()])?;
        scratch.fill(0);
        netlist.validate_unique_names(&mut scratch[..netlist.names.entry_count()])?;
        Ok(netlist)
    }
}

pub(super) fn append_pin(
    net_pins: &mut [NetPins],
    pin_links: &mut [PinLinks],
    net: NetId,
    pin: PinId,
) {
    let tail = net_pins[net.index()].tail;
    pin_links[pin.index()] = PinLinks {
        previous: tail,
        next: None,
    };
    if let Some(tail) = tail {
        pin_links[tail.index()].next = Some(pin);
    } else {
        net_pins[net.index()].head = Some(pin);
    }
    net_pins[net.index()].tail = Some(pin);
}
