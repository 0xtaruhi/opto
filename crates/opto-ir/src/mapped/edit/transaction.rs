// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    AppliedRegionDelta, BTreeMap, BTreeSet, CellId, CellSlot, CellSpec, ConnectionRef,
    ConnectionSignal, MappedCell, MappedError, MappedNetlist, NameId, NetId, NetPins, NetSlot,
    PinConnection, PinId, PinLinks, RegionConflict, RegionDelta, RegionOperation, RegionSnapshot,
    TempCellId, TempNetId, link_pin, operation_names, save_cell, save_net, touch_signal_net,
    unlink_pin, validate_signal,
};

#[derive(Debug)]
pub(super) enum ResolvedOperation {
    AddNet {
        id: NetId,
        name: Option<String>,
    },
    AddCell {
        id: CellId,
        spec: CellSpec,
        connections: Vec<(String, Option<u16>, ConnectionSignal)>,
    },
    RemoveCell(CellId),
    RemoveNet(NetId),
    ReconnectPin {
        pin: PinId,
        signal: ConnectionSignal,
    },
    ReplaceCell {
        cell: CellId,
        cell_type: String,
        library_cell: Option<u32>,
    },
    RenameCell {
        cell: CellId,
        name: String,
    },
    RenameNet {
        net: NetId,
        name: Option<String>,
    },
}

struct ResolvedDelta {
    operations: Vec<ResolvedOperation>,
    added_nets: BTreeMap<TempNetId, NetId>,
    added_cells: BTreeMap<TempCellId, CellId>,
}

#[derive(Default)]
struct OperationValidation {
    removed_cells: BTreeSet<CellId>,
    removed_nets: BTreeSet<NetId>,
    reconnected: BTreeMap<PinId, ConnectionSignal>,
    written_cells: BTreeSet<CellId>,
    written_nets: BTreeSet<NetId>,
}

impl MappedNetlist {
    /// Captures versions for an explicit regional cell and net read set.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when any supplied ID is foreign or tombstoned.
    pub fn snapshot_region(
        &self,
        cells: impl IntoIterator<Item = CellId>,
        nets: impl IntoIterator<Item = NetId>,
    ) -> Result<RegionSnapshot, MappedError> {
        let cells = cells
            .into_iter()
            .map(|id| {
                let slot = self
                    .cells
                    .get(id.index())
                    .filter(|slot| slot.live)
                    .ok_or_else(|| {
                        MappedError::invariant(format!("cannot snapshot removed cell {id:?}"))
                    })?;
                Ok::<_, MappedError>((id, slot.version))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let nets = nets
            .into_iter()
            .map(|id| {
                let slot = self
                    .nets
                    .get(id.index())
                    .filter(|slot| slot.live)
                    .ok_or_else(|| {
                        MappedError::invariant(format!("cannot snapshot removed net {id:?}"))
                    })?;
                Ok::<_, MappedError>((id, slot.version))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(RegionSnapshot {
            generation: self.generation,
            cells,
            nets,
        })
    }

    /// Validates and atomically applies a regional mutation plan.
    ///
    /// No netlist state changes unless snapshot, footprint, capacity,
    /// connectivity, and naming validation all succeed.
    ///
    /// # Errors
    ///
    /// Returns [`RegionConflict`] when a snapshotted object is stale, the
    /// netlist is published, or the delta violates a mapped invariant.
    ///
    /// # Panics
    ///
    /// Panics only if the capacity preflight succeeds but the same unchanged
    /// arena length later fails its compact-ID conversion.
    #[allow(
        clippy::too_many_lines,
        reason = "the transaction keeps validation and its single atomic commit boundary together"
    )]
    pub fn apply_region_delta(
        &mut self,
        delta: RegionDelta,
    ) -> Result<AppliedRegionDelta, RegionConflict> {
        let RegionDelta {
            snapshot,
            operations: requested_operations,
            ..
        } = delta;
        if self.published {
            return Err(RegionConflict::invalid(
                "cannot edit a published mapped netlist".to_string(),
            ));
        }
        self.check_snapshot(&snapshot)?;
        let ResolvedDelta {
            operations,
            added_nets,
            added_cells,
        } = self.resolve_operations(&requested_operations)?;
        self.validate_operations(&snapshot, &operations)?;
        let removed_net_count = operations
            .iter()
            .filter(|operation| matches!(operation, ResolvedOperation::RemoveNet(_)))
            .count();
        let removed_cell_count = operations
            .iter()
            .filter(|operation| matches!(operation, ResolvedOperation::RemoveCell(_)))
            .count();
        let next_net_count = updated_live_count(
            self.live_net_count,
            removed_net_count,
            added_nets.len(),
            "net",
        )?;
        let next_cell_count = updated_live_count(
            self.live_cell_count,
            removed_cell_count,
            added_cells.len(),
            "cell",
        )?;

        let next_revision = self.edit_revision.checked_add(1).ok_or_else(|| {
            RegionConflict::invalid("mapped edit revision space is exhausted".to_string())
        })?;
        let names = self.names.checkpoint();
        let mut interned = BTreeMap::<String, NameId>::new();
        for name in operation_names(&operations) {
            if interned.contains_key(name) {
                continue;
            }
            match self.names.intern(name) {
                Ok(id) => {
                    interned.insert(name.to_string(), id);
                }
                Err(error) => {
                    self.names
                        .rollback(names)
                        .expect("name rollback immediately follows failed interning");
                    return Err(RegionConflict::from(MappedError::from(error)));
                }
            }
        }

        let mut applied = AppliedRegionDelta {
            generation: self.generation,
            previous_revision: self.edit_revision,
            committed_revision: next_revision,
            previous_net_count: self.live_net_count,
            previous_cell_count: self.live_cell_count,
            old_net_len: self.nets.len(),
            old_cell_len: self.cells.len(),
            old_connection_len: self.connections.len(),
            old_nets: BTreeMap::new(),
            old_cells: BTreeMap::new(),
            old_connections: BTreeMap::new(),
            old_net_pins: BTreeMap::new(),
            old_pin_links: BTreeMap::new(),
            names,
            added_nets,
            added_cells,
            renamed_nets: BTreeSet::new(),
        };

        for operation in operations {
            match operation {
                ResolvedOperation::AddNet { id, name } => {
                    debug_assert_eq!(id.index(), self.nets.len());
                    self.nets.push(NetSlot {
                        name: name.map(|name| interned[&name]),
                        live: true,
                        version: next_revision,
                    });
                    self.net_pins.push(NetPins::default());
                }
                ResolvedOperation::AddCell {
                    id,
                    spec,
                    connections,
                } => {
                    debug_assert_eq!(id.index(), self.cells.len());
                    let start = u32::try_from(self.connections.len())
                        .expect("validated mapped pin capacity fits u32");
                    for (pin, library_pin, signal) in connections {
                        let pin_id = PinId::from_index(self.connections.len())
                            .expect("resolved mapped pin ID fits its arena");
                        self.connections.push(PinConnection {
                            pin: interned[&pin],
                            library_pin,
                            signal,
                        });
                        self.pin_owners.push(id);
                        self.pin_links.push(PinLinks::default());
                        if let ConnectionSignal::Net(net) = signal {
                            link_pin(self, &mut applied, net, pin_id);
                            touch_signal_net(
                                self,
                                &mut applied,
                                ConnectionSignal::Net(net),
                                next_revision,
                            );
                        }
                    }
                    let end = u32::try_from(self.connections.len())
                        .expect("validated mapped pin capacity fits u32");
                    self.cells.push(CellSlot {
                        cell: MappedCell {
                            name: interned[&spec.name],
                            cell_type: interned[&spec.cell_type],
                            library_cell: spec.library_cell,
                            connection_start: start,
                            connection_end: end,
                        },
                        live: true,
                        version: next_revision,
                    });
                }
                ResolvedOperation::RemoveCell(cell) => {
                    save_cell(self, &mut applied, cell);
                    let pin_range = {
                        let record = self.cells[cell.index()].cell;
                        record.connection_start as usize..record.connection_end as usize
                    };
                    for pin_index in pin_range {
                        let pin = PinId::from_index(pin_index)
                            .expect("existing mapped pin index fits its ID");
                        let signal = self.connections[pin_index].signal;
                        if matches!(signal, ConnectionSignal::Net(_)) {
                            unlink_pin(self, &mut applied, pin);
                            touch_signal_net(self, &mut applied, signal, next_revision);
                        }
                    }
                    let slot = &mut self.cells[cell.index()];
                    slot.live = false;
                    slot.version = next_revision;
                }
                ResolvedOperation::RemoveNet(net) => {
                    save_net(self, &mut applied, net);
                    let slot = &mut self.nets[net.index()];
                    slot.live = false;
                    slot.version = next_revision;
                }
                ResolvedOperation::ReconnectPin { pin, signal } => {
                    applied
                        .old_connections
                        .entry(pin)
                        .or_insert(self.connections[pin.index()]);
                    let old_signal = self.connections[pin.index()].signal;
                    if matches!(old_signal, ConnectionSignal::Net(_)) {
                        unlink_pin(self, &mut applied, pin);
                    }
                    self.connections[pin.index()].signal = signal;
                    if let ConnectionSignal::Net(net) = signal {
                        link_pin(self, &mut applied, net, pin);
                    }
                    let owner = self.pin_owner(pin).expect("validated pin has a live owner");
                    save_cell(self, &mut applied, owner);
                    self.cells[owner.index()].version = next_revision;
                    touch_signal_net(self, &mut applied, old_signal, next_revision);
                    touch_signal_net(self, &mut applied, signal, next_revision);
                }
                ResolvedOperation::ReplaceCell {
                    cell,
                    cell_type,
                    library_cell,
                } => {
                    save_cell(self, &mut applied, cell);
                    let slot = &mut self.cells[cell.index()];
                    slot.cell.cell_type = interned[&cell_type];
                    slot.cell.library_cell = library_cell;
                    slot.version = next_revision;
                }
                ResolvedOperation::RenameCell { cell, name } => {
                    save_cell(self, &mut applied, cell);
                    let slot = &mut self.cells[cell.index()];
                    slot.cell.name = interned[&name];
                    slot.version = next_revision;
                }
                ResolvedOperation::RenameNet { net, name } => {
                    save_net(self, &mut applied, net);
                    applied.renamed_nets.insert(net);
                    let slot = &mut self.nets[net.index()];
                    slot.name = name.map(|name| interned[&name]);
                    slot.version = next_revision;
                }
            }
        }
        applied.renamed_nets.retain(|net| {
            let old = applied
                .old_nets
                .get(net)
                .expect("renamed nets are saved before mutation");
            let current = &self.nets[net.index()];
            current.live && old.name != current.name
        });
        self.live_net_count = next_net_count;
        self.live_cell_count = next_cell_count;
        self.edit_revision = next_revision;
        Ok(applied)
    }

    /// Restores the exact state that preceded an applied region delta.
    ///
    /// Rollback succeeds only while no later edit has advanced the netlist
    /// revision. Added slots are truncated and all touched records, adjacency
    /// links, names, and versions are restored from the bounded undo record.
    ///
    /// # Errors
    ///
    /// Returns [`RegionConflict`] when a later edit makes the undo record stale
    /// or the name table cannot return to its checkpoint.
    pub fn rollback_region_delta(
        &mut self,
        applied: AppliedRegionDelta,
    ) -> Result<(), RegionConflict> {
        if self.generation != applied.generation {
            return Err(RegionConflict::invalid(format!(
                "cannot rollback mapped generation {:?} into foreign generation {:?}",
                applied.generation, self.generation
            )));
        }
        if self.edit_revision != applied.committed_revision {
            return Err(RegionConflict::invalid(format!(
                "cannot rollback mapped revision {} after revision {} was published",
                applied.committed_revision, self.edit_revision
            )));
        }
        self.connections.truncate(applied.old_connection_len);
        self.pin_owners.truncate(applied.old_connection_len);
        self.pin_links.truncate(applied.old_connection_len);
        self.cells.truncate(applied.old_cell_len);
        self.nets.truncate(applied.old_net_len);
        self.net_pins.truncate(applied.old_net_len);
        for (pin, connection) in applied.old_connections {
            self.connections[pin.index()] = connection;
        }
        for (cell, slot) in applied.old_cells {
            self.cells[cell.index()] = slot;
        }
        for (net, slot) in applied.old_nets {
            self.nets[net.index()] = slot;
        }
        for (net, pins) in applied.old_net_pins {
            self.net_pins[net.index()] = pins;
        }
        for (pin, links) in applied.old_pin_links {
            self.pin_links[pin.index()] = links;
        }
        self.live_net_count = applied.previous_net_count;
        self.live_cell_count = applied.previous_cell_count;
        self.names
            .rollback(applied.names)
            .map_err(MappedError::from)
            .map_err(RegionConflict::from)?;
        self.edit_revision = applied.previous_revision;
        Ok(())
    }

    fn check_snapshot(&self, snapshot: &RegionSnapshot) -> Result<(), RegionConflict> {
        if self.generation != snapshot.generation {
            return Err(RegionConflict::invalid(format!(
                "mapped region snapshot belongs to generation {:?}, not {:?}",
                snapshot.generation, self.generation
            )));
        }
        for (&cell, &version) in &snapshot.cells {
            let current = self.cells.get(cell.index()).filter(|slot| slot.live);
            if current.is_none_or(|slot| slot.version != version) {
                return Err(RegionConflict::StaleCell(cell));
            }
        }
        for (&net, &version) in &snapshot.nets {
            let current = self.nets.get(net.index()).filter(|slot| slot.live);
            if current.is_none_or(|slot| slot.version != version) {
                return Err(RegionConflict::StaleNet(net));
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "every region operation is resolved in one exhaustive, side-effect-free pass"
    )]
    fn resolve_operations(
        &self,
        requested_operations: &[RegionOperation],
    ) -> Result<ResolvedDelta, RegionConflict> {
        let mut added_nets = BTreeMap::new();
        let mut added_cells = BTreeMap::new();
        for operation in requested_operations {
            match operation {
                RegionOperation::AddNet { id, .. } => {
                    let final_id = NetId::from_index(self.nets.len() + added_nets.len())
                        .map_err(RegionConflict::from)?;
                    added_nets.insert(*id, final_id);
                }
                RegionOperation::AddCell { id, .. } => {
                    let final_id = CellId::from_index(self.cells.len() + added_cells.len())
                        .map_err(RegionConflict::from)?;
                    added_cells.insert(*id, final_id);
                }
                _ => {}
            }
        }
        let added_pin_count =
            requested_operations
                .iter()
                .try_fold(0usize, |count, operation| {
                    let added = match operation {
                        RegionOperation::AddCell { spec, .. } => spec.connections.len(),
                        _ => 0,
                    };
                    count.checked_add(added).ok_or_else(|| {
                        RegionConflict::invalid("mapped pin connection count overflow".to_string())
                    })
                })?;
        let final_pin_count = self
            .connections
            .len()
            .checked_add(added_pin_count)
            .ok_or_else(|| {
                RegionConflict::invalid("mapped pin connection count overflow".to_string())
            })?;
        if final_pin_count > u32::MAX as usize {
            return Err(RegionConflict::invalid(
                "mapped pin connection arena exceeds 32-bit capacity".to_string(),
            ));
        }

        let resolve_signal = |signal: ConnectionRef| -> Result<ConnectionSignal, RegionConflict> {
            match signal {
                ConnectionRef::Net(net) => Ok(ConnectionSignal::Net(net)),
                ConnectionRef::NewNet(net) => added_nets
                    .get(&net)
                    .copied()
                    .map(ConnectionSignal::Net)
                    .ok_or_else(|| {
                        RegionConflict::invalid(format!(
                            "region delta references unknown temporary net {net:?}"
                        ))
                    }),
                ConnectionRef::Constant(value) => Ok(ConnectionSignal::Constant(value)),
            }
        };

        let operations = requested_operations
            .iter()
            .map(|operation| match operation {
                RegionOperation::AddNet { id, name } => Ok(ResolvedOperation::AddNet {
                    id: added_nets[id],
                    name: name.clone(),
                }),
                RegionOperation::AddCell { id, spec } => Ok(ResolvedOperation::AddCell {
                    id: added_cells[id],
                    spec: spec.clone(),
                    connections: spec
                        .connections
                        .iter()
                        .map(|(pin, library_pin, signal)| {
                            Ok((pin.clone(), *library_pin, resolve_signal(*signal)?))
                        })
                        .collect::<Result<_, RegionConflict>>()?,
                }),
                RegionOperation::RemoveCell(cell) => Ok(ResolvedOperation::RemoveCell(*cell)),
                RegionOperation::RemoveNet(net) => Ok(ResolvedOperation::RemoveNet(*net)),
                RegionOperation::ReconnectPin { pin, signal } => {
                    Ok(ResolvedOperation::ReconnectPin {
                        pin: *pin,
                        signal: resolve_signal(*signal)?,
                    })
                }
                RegionOperation::ReplaceCell {
                    cell,
                    cell_type,
                    library_cell,
                } => Ok(ResolvedOperation::ReplaceCell {
                    cell: *cell,
                    cell_type: cell_type.clone(),
                    library_cell: *library_cell,
                }),
                RegionOperation::RenameCell { cell, name } => Ok(ResolvedOperation::RenameCell {
                    cell: *cell,
                    name: name.clone(),
                }),
                RegionOperation::RenameNet { net, name } => Ok(ResolvedOperation::RenameNet {
                    net: *net,
                    name: name.clone(),
                }),
            })
            .collect::<Result<Vec<_>, RegionConflict>>()?;
        Ok(ResolvedDelta {
            operations,
            added_nets,
            added_cells,
        })
    }

    fn validate_operations(
        &self,
        snapshot: &RegionSnapshot,
        operations: &[ResolvedOperation],
    ) -> Result<(), RegionConflict> {
        let new_nets = operations
            .iter()
            .filter_map(|operation| match operation {
                ResolvedOperation::AddNet { id, .. } => Some(*id),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut validation = OperationValidation::default();
        for operation in operations {
            validation.validate_operation(self, snapshot, operation, &new_nets)?;
        }
        validation.validate_cell_names(self, operations)?;
        validation.validate_removed_nets(self, operations)
    }
}

impl OperationValidation {
    fn validate_operation(
        &mut self,
        netlist: &MappedNetlist,
        snapshot: &RegionSnapshot,
        operation: &ResolvedOperation,
        new_nets: &BTreeSet<NetId>,
    ) -> Result<(), RegionConflict> {
        match operation {
            ResolvedOperation::AddCell {
                spec, connections, ..
            } => Self::validate_added_cell(netlist, snapshot, spec, connections, new_nets),
            ResolvedOperation::RemoveCell(cell) => {
                self.validate_removed_cell(netlist, snapshot, *cell, new_nets)
            }
            ResolvedOperation::RemoveNet(net) => {
                Self::require_snapshot_net(snapshot, *net, "removes")?;
                Self::record_write(&mut self.written_nets, *net, "net")?;
                self.removed_nets.insert(*net);
                Ok(())
            }
            ResolvedOperation::ReconnectPin { pin, signal } => {
                self.validate_reconnection(netlist, snapshot, *pin, *signal, new_nets)
            }
            ResolvedOperation::ReplaceCell { cell, .. }
            | ResolvedOperation::RenameCell { cell, .. } => {
                Self::require_snapshot_cell(snapshot, *cell, "writes")?;
                Self::record_write(&mut self.written_cells, *cell, "cell")
            }
            ResolvedOperation::RenameNet { net, .. } => {
                Self::require_snapshot_net(snapshot, *net, "writes")?;
                Self::record_write(&mut self.written_nets, *net, "net")
            }
            ResolvedOperation::AddNet { .. } => Ok(()),
        }
    }

    fn validate_added_cell(
        netlist: &MappedNetlist,
        snapshot: &RegionSnapshot,
        spec: &CellSpec,
        connections: &[(String, Option<u16>, ConnectionSignal)],
        new_nets: &BTreeSet<NetId>,
    ) -> Result<(), RegionConflict> {
        if spec.name.is_empty() || spec.cell_type.is_empty() {
            return Err(RegionConflict::invalid(
                "mapped cells require non-empty instance and cell type names".to_string(),
            ));
        }
        let mut pins = BTreeSet::new();
        for (pin, _, signal) in connections {
            if pin.is_empty() || !pins.insert(pin) {
                return Err(RegionConflict::invalid(format!(
                    "mapped cell '{}' has an empty or duplicate pin name",
                    spec.name
                )));
            }
            validate_signal(netlist, snapshot, *signal, new_nets)?;
        }
        Ok(())
    }

    fn validate_removed_cell(
        &mut self,
        netlist: &MappedNetlist,
        snapshot: &RegionSnapshot,
        cell: CellId,
        new_nets: &BTreeSet<NetId>,
    ) -> Result<(), RegionConflict> {
        Self::require_snapshot_cell(snapshot, cell, "removes")?;
        for connection in netlist.connections(cell).ok_or_else(|| {
            RegionConflict::invalid(format!("region delta references removed cell {cell:?}"))
        })? {
            validate_signal(netlist, snapshot, connection.signal, new_nets)?;
        }
        Self::record_write(&mut self.written_cells, cell, "cell")?;
        self.removed_cells.insert(cell);
        Ok(())
    }

    fn validate_reconnection(
        &mut self,
        netlist: &MappedNetlist,
        snapshot: &RegionSnapshot,
        pin: PinId,
        signal: ConnectionSignal,
        new_nets: &BTreeSet<NetId>,
    ) -> Result<(), RegionConflict> {
        let owner = netlist.pin_owner(pin).ok_or_else(|| {
            RegionConflict::invalid(format!("region delta references unknown pin {pin:?}"))
        })?;
        if !snapshot.contains_cell(owner) {
            return Err(RegionConflict::invalid(format!(
                "region delta reconnects pin {pin:?} outside its cell snapshot"
            )));
        }
        if self.reconnected.insert(pin, signal).is_some() {
            return Err(RegionConflict::invalid(format!(
                "region delta reconnects pin {pin:?} more than once"
            )));
        }
        let old_signal = netlist
            .connection(pin)
            .expect("validated pin has a live connection")
            .signal;
        validate_signal(netlist, snapshot, old_signal, new_nets)?;
        validate_signal(netlist, snapshot, signal, new_nets)
    }

    fn require_snapshot_cell(
        snapshot: &RegionSnapshot,
        cell: CellId,
        action: &str,
    ) -> Result<(), RegionConflict> {
        if snapshot.contains_cell(cell) {
            return Ok(());
        }
        Err(RegionConflict::invalid(format!(
            "region delta {action} cell {cell:?} outside its snapshot"
        )))
    }

    fn require_snapshot_net(
        snapshot: &RegionSnapshot,
        net: NetId,
        action: &str,
    ) -> Result<(), RegionConflict> {
        if snapshot.contains_net(net) {
            return Ok(());
        }
        Err(RegionConflict::invalid(format!(
            "region delta {action} net {net:?} outside its snapshot"
        )))
    }

    fn record_write<T: Copy + Ord + std::fmt::Debug>(
        written: &mut BTreeSet<T>,
        object: T,
        kind: &str,
    ) -> Result<(), RegionConflict> {
        if written.insert(object) {
            return Ok(());
        }
        Err(RegionConflict::invalid(format!(
            "region delta writes {kind} {object:?} more than once"
        )))
    }

    fn validate_cell_names(
        &self,
        netlist: &MappedNetlist,
        operations: &[ResolvedOperation],
    ) -> Result<(), RegionConflict> {
        let mut future_names = BTreeSet::new();
        let mut released_names = self
            .removed_cells
            .iter()
            .map(|cell| netlist.cells[cell.index()].cell.name)
            .collect::<BTreeSet<_>>();
        let mut renamed_cells = BTreeSet::new();
        for operation in operations {
            let name = match operation {
                ResolvedOperation::AddCell { spec, .. } => spec.name.as_str(),
                ResolvedOperation::RenameCell { cell, name } => {
                    let old = netlist.cells[cell.index()].cell.name;
                    let old_name = netlist.names.resolve(old).ok_or_else(|| {
                        RegionConflict::invalid("mapped cell has an invalid name identifier")
                    })?;
                    if old_name == name.as_str() {
                        continue;
                    }
                    released_names.insert(old);
                    renamed_cells.insert(*cell);
                    name.as_str()
                }
                _ => continue,
            };
            if name.is_empty() {
                return Err(RegionConflict::invalid(
                    "mapped cells require non-empty instance names",
                ));
            }
            if !future_names.insert(name) {
                return Err(RegionConflict::invalid(format!(
                    "mapped region assigns duplicate cell name '{name}'"
                )));
            }
        }
        let interned_future = future_names
            .iter()
            .filter_map(|&name| netlist.names.get(name).map(|id| (name, id)))
            .collect::<Vec<_>>();
        if interned_future
            .iter()
            .any(|(_, id)| !released_names.contains(id))
        {
            let mut used = BTreeSet::new();
            for cell in netlist
                .cell_ids()
                .filter(|cell| !self.removed_cells.contains(cell) && !renamed_cells.contains(cell))
            {
                used.insert(netlist.cells[cell.index()].cell.name);
            }
            for instance in netlist.design_instance_ids() {
                used.insert(netlist.design_instances[instance.index()].name);
            }
            for (name, id) in interned_future {
                if used.contains(&id) {
                    return Err(RegionConflict::invalid(format!(
                        "mapped region assigns duplicate cell name '{name}'"
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_removed_nets(
        &self,
        netlist: &MappedNetlist,
        operations: &[ResolvedOperation],
    ) -> Result<(), RegionConflict> {
        for net in &self.removed_nets {
            if netlist.is_external_net(*net) {
                return Err(RegionConflict::invalid(format!(
                    "region delta removes externally referenced net {net:?}"
                )));
            }
            for pin in netlist
                .pins_on_net(*net)
                .expect("removed candidate net is live before transaction")
            {
                let owner = netlist
                    .pin_owner(pin)
                    .expect("net adjacency contains only live cell pins");
                if self.removed_cells.contains(&owner) {
                    continue;
                }
                let signal = self
                    .reconnected
                    .get(&pin)
                    .copied()
                    .unwrap_or(netlist.connections[pin.index()].signal);
                if signal == ConnectionSignal::Net(*net) {
                    return Err(RegionConflict::invalid(format!(
                        "region delta removes net {net:?} while pin {pin:?} still references it"
                    )));
                }
            }
            for operation in operations {
                if let ResolvedOperation::AddCell { connections, .. } = operation
                    && connections
                        .iter()
                        .any(|(_, _, signal)| *signal == ConnectionSignal::Net(*net))
                {
                    return Err(RegionConflict::invalid(format!(
                        "region delta removes net {net:?} while a new cell references it"
                    )));
                }
            }
        }
        Ok(())
    }
}

fn updated_live_count(
    current: usize,
    removed: usize,
    added: usize,
    resource: &'static str,
) -> Result<usize, RegionConflict> {
    current
        .checked_sub(removed)
        .and_then(|count| count.checked_add(added))
        .ok_or_else(|| {
            RegionConflict::invalid(format!("mapped live {resource} count is inconsistent"))
        })
}
