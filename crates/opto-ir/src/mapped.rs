// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Target-cell netlists and transactional region editing.
//!
//! [`MappedBuilder`] constructs ports, canonical nets, cells, and pin
//! connections before sealing a [`MappedNetlist`]. Cell and net IDs are stable
//! slot IDs: removing an object leaves a tombstone rather than renumbering
//! unrelated live objects.
//!
//! Post-map optimization operates through [`RegionSnapshot`] and
//! [`RegionDelta`]. Applying a delta validates its mapped owner, complete
//! cell/net footprint, producing an [`AppliedRegionDelta`] that can be committed
//! or rolled back without rebuilding the whole netlist. Intrusive adjacency is
//! owned by the netlist and must not be reconstructed from names.

use crate::{NameError, NameId, NameTable, RevisionId};
use opto_core::{DenseId, SlotId, resident};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

mod builder;
mod edit;
mod external;
mod publication;

pub use builder::{MappedBuilder, MappedCellSpec};

pub use edit::{
    AppliedRegionDelta, CellSpec, ConnectionRef, RegionConflict, RegionDelta, RegionSnapshot,
    TempCellId, TempNetId,
};
pub use publication::MappedCellRemap;

static NEXT_MAPPED_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Runtime identity of one mapped-netlist owner.
///
/// This identity is independent of [`MappedNetlist::edit_revision`]: revisions
/// order edits within one owner, while a generation ID prevents revision-local
/// IDs and derived artifacts from being used with another netlist. Generation
/// IDs are deliberately omitted from checkpoints and freshly allocated when a
/// mapped netlist is restored, so runtime identity cannot perturb deterministic
/// serialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct MappedGenerationId(NonZeroU64);

impl MappedGenerationId {
    fn fresh() -> Self {
        let raw = NEXT_MAPPED_GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("mapped generation ID space is exhausted");
        Self(NonZeroU64::new(raw).expect("mapped generation IDs start at one"))
    }

    /// Returns the nonzero runtime representation.
    #[must_use]
    pub fn get(self) -> NonZeroU64 {
        self.0
    }
}

macro_rules! compact_id {
    ($name:ident, $tag:ident, $storage:ident, $kind:literal) => {
        enum $tag {}

        #[doc = concat!("Compact ", $kind, " local to one [`MappedNetlist`].")]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[repr(transparent)]
        pub struct $name($storage<$tag>);

        impl $name {
            #[doc = concat!("Creates a ", $kind, " from its arena index.")]
            ///
            /// # Errors
            ///
            /// Returns [`MappedError`] when `index` exceeds 32-bit capacity.
            pub fn from_index(index: usize) -> Result<Self, MappedError> {
                $storage::from_index(index)
                    .map(Self)
                    .map_err(|_| MappedError::capacity($kind))
            }

            #[must_use]
            #[doc = concat!("Returns the arena index encoded by this ", $kind, ".")]
            pub fn index(self) -> usize {
                self.0.index()
            }
        }
    };
}

compact_id!(NetId, NetTag, SlotId, "net ID");
compact_id!(CellId, CellTag, SlotId, "cell ID");
compact_id!(PortId, PortTag, DenseId, "port ID");
compact_id!(PinId, PinTag, SlotId, "pin ID");
compact_id!(
    DesignInstanceId,
    DesignInstanceTag,
    DenseId,
    "design instance ID"
);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Construction, capacity, or invariant failure in mapped IR.
pub enum MappedError {
    /// A compact arena or ID exceeded 32-bit capacity.
    #[error("mapped netlist {0} exceeds 32-bit capacity")]
    Capacity(String),
    /// Connectivity, ownership, or publication state is invalid.
    #[error("{0}")]
    Invariant(String),
    /// A mapped name cannot be interned or resolved.
    #[error(transparent)]
    Name(#[from] NameError),
}

impl MappedError {
    pub(super) fn capacity(kind: &str) -> Self {
        Self::Capacity(kind.to_string())
    }

    pub(super) fn invariant(message: impl Into<String>) -> Self {
        Self::Invariant(message.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Direction of a mapped design port relative to the design.
pub enum PortDirection {
    /// Driven by the design environment.
    Input,
    /// Driven by mapped logic.
    Output,
    /// May be driven from either side.
    Inout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Mapped design port and its contiguous vector of scalar nets.
pub struct MappedPort {
    /// Interned port name.
    pub name: NameId,
    /// Direction relative to the design.
    pub direction: PortDirection,
    /// Inclusive start offset in the port-net arena.
    pub net_start: u32,
    /// Exclusive end offset in the port-net arena.
    pub net_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Live or tombstoned target-library cell payload.
pub struct MappedCell {
    /// Interned instance name.
    pub name: NameId,
    /// Interned target cell type.
    pub cell_type: NameId,
    /// Optional dense identifier in the selected target-cell catalog.
    pub library_cell: Option<u32>,
    /// Inclusive start offset in the pin-connection arena.
    pub connection_start: u32,
    /// Exclusive end offset in the pin-connection arena.
    pub connection_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// One named target-cell pin binding.
pub struct PinConnection {
    /// Interned pin name.
    pub pin: NameId,
    /// Optional dense identifier in the target cell's pin catalog.
    pub library_pin: Option<u16>,
    /// Connected net or Boolean constant.
    pub signal: ConnectionSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Unmapped design occurrence retained across an optimization boundary.
pub struct MappedDesignInstance {
    /// Interned occurrence name.
    pub name: NameId,
    /// Interned referenced definition name.
    pub module: NameId,
    /// Inclusive start offset in the design-connection arena.
    pub connection_start: u32,
    /// Exclusive end offset in the design-connection arena.
    pub connection_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// One vector port binding on a retained design occurrence.
pub struct DesignInstanceConnection {
    /// Interned referenced port name.
    pub port: NameId,
    /// Inclusive start offset in the design-signal arena.
    pub signal_start: u32,
    /// Exclusive end offset in the design-signal arena.
    pub signal_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Scalar signal used by mapped cells, ports, or retained design occurrences.
pub enum ConnectionSignal {
    /// Canonical net in the owning mapped netlist.
    Net(NetId),
    /// Boolean constant driver.
    Constant(bool),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) struct NetSlot {
    pub(super) name: Option<NameId>,
    pub(super) live: bool,
    pub(super) version: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) struct CellSlot {
    pub(super) cell: MappedCell,
    pub(super) live: bool,
    pub(super) version: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct NetPins {
    pub(super) head: Option<PinId>,
    pub(super) tail: Option<PinId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PinLinks {
    pub(super) previous: Option<PinId>,
    pub(super) next: Option<PinId>,
}

#[derive(Debug, Serialize, Deserialize)]
/// Canonical target-cell netlist with stable slot identities.
///
/// Net and cell deletion leaves tombstones, so surviving [`NetId`] and
/// [`CellId`] values never change. The mutable pre-publication form supports
/// transactional region edits; [`Self::finalize_for_publication`] densely
/// repacks and seals it exactly once.
pub struct MappedNetlist {
    #[serde(skip, default = "MappedGenerationId::fresh")]
    pub(super) generation: MappedGenerationId,
    pub(super) base_revision: RevisionId,
    pub(super) edit_revision: u64,
    pub(super) published: bool,
    pub(super) name: NameId,
    pub(super) names: NameTable,
    pub(super) nets: Vec<NetSlot>,
    pub(super) live_net_count: usize,
    pub(super) ports: Vec<MappedPort>,
    pub(super) port_nets: Vec<NetId>,
    pub(super) cells: Vec<CellSlot>,
    pub(super) live_cell_count: usize,
    pub(super) connections: Vec<PinConnection>,
    pub(super) pin_owners: Vec<CellId>,
    pub(super) pin_links: Vec<PinLinks>,
    pub(super) net_pins: Vec<NetPins>,
    pub(super) design_instances: Vec<MappedDesignInstance>,
    pub(super) design_connections: Vec<DesignInstanceConnection>,
    pub(super) design_connection_signals: Vec<ConnectionSignal>,
    pub(super) constant_drivers: Vec<(NetId, bool)>,
    pub(super) external_nets: Vec<NetId>,
}

impl MappedNetlist {
    /// Returns the runtime identity that owns every mapped ID in this netlist.
    #[must_use]
    pub fn generation_id(&self) -> MappedGenerationId {
        self.generation
    }

    #[must_use]
    /// Returns the session revision from which this netlist was synthesized.
    pub fn base_revision(&self) -> RevisionId {
        self.base_revision
    }

    #[must_use]
    /// Returns the monotonic mapped-generation revision.
    pub fn edit_revision(&self) -> u64 {
        self.edit_revision
    }

    #[must_use]
    /// Returns the mapped design name.
    pub fn name(&self) -> &str {
        self.names.resolve(self.name).unwrap_or("")
    }

    #[must_use]
    /// Returns the netlist's interned-name table.
    pub fn names(&self) -> &NameTable {
        &self.names
    }

    #[must_use]
    /// Returns the number of live nets, excluding tombstones.
    pub fn net_count(&self) -> usize {
        self.live_net_count
    }

    #[must_use]
    /// Returns allocated net slots, including tombstones.
    pub fn net_slot_count(&self) -> usize {
        self.nets.len()
    }

    /// Iterates live net IDs in slot order.
    ///
    /// # Panics
    ///
    /// Panics only if an allocated slot index exceeds the typed net-ID capacity;
    /// mapped builders reject that capacity overflow.
    pub fn net_ids(&self) -> impl Iterator<Item = NetId> + '_ {
        self.nets
            .iter()
            .enumerate()
            .filter(|(_, net)| net.live)
            .map(|(index, _)| NetId::from_index(index).expect("existing net index fits its ID"))
    }

    #[must_use]
    /// Returns mapped ports in stable insertion order.
    pub fn ports(&self) -> &[MappedPort] {
        &self.ports
    }

    #[must_use]
    /// Resolves a mapped port name.
    pub fn port_name(&self, port: PortId) -> Option<&str> {
        self.ports
            .get(port.index())
            .and_then(|port| self.names.resolve(port.name))
    }

    #[must_use]
    /// Returns scalar nets connected to `port` in vector order.
    pub fn port_nets(&self, port: PortId) -> Option<&[NetId]> {
        let port = self.ports.get(port.index())?;
        self.port_nets
            .get(port.net_start as usize..port.net_end as usize)
    }

    #[must_use]
    /// Returns the number of live target cells, excluding tombstones.
    pub fn cell_count(&self) -> usize {
        self.live_cell_count
    }

    #[must_use]
    /// Returns allocated cell slots, including tombstones.
    pub fn cell_slot_count(&self) -> usize {
        self.cells.len()
    }

    /// Iterates live cell IDs in slot order.
    ///
    /// # Panics
    ///
    /// Panics only if an allocated slot index exceeds the typed cell-ID
    /// capacity; mapped builders reject that capacity overflow.
    pub fn cell_ids(&self) -> impl Iterator<Item = CellId> + '_ {
        self.cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.live)
            .map(|(index, _)| CellId::from_index(index).expect("existing cell index fits its ID"))
    }

    #[must_use]
    /// Returns the number of retained design occurrences.
    pub fn design_instance_count(&self) -> usize {
        self.design_instances.len()
    }

    /// Iterates retained design-occurrence IDs in dense order.
    ///
    /// # Panics
    ///
    /// Panics only if the private occurrence arena exceeds its typed-ID
    /// capacity; construction checks the final range before committing it.
    pub fn design_instance_ids(&self) -> impl Iterator<Item = DesignInstanceId> + '_ {
        (0..self.design_instances.len()).map(|index| {
            DesignInstanceId::from_index(index).expect("existing design instance index fits its ID")
        })
    }

    #[must_use]
    /// Resolves a retained design-occurrence ID.
    pub fn design_instance(&self, instance: DesignInstanceId) -> Option<&MappedDesignInstance> {
        self.design_instances.get(instance.index())
    }

    #[must_use]
    /// Resolves a retained design occurrence's instance name.
    pub fn design_instance_name(&self, instance: DesignInstanceId) -> Option<&str> {
        self.design_instance(instance)
            .and_then(|instance| self.names.resolve(instance.name))
    }

    #[must_use]
    /// Resolves a retained design occurrence's referenced definition.
    pub fn design_instance_module(&self, instance: DesignInstanceId) -> Option<&str> {
        self.design_instance(instance)
            .and_then(|instance| self.names.resolve(instance.module))
    }

    #[must_use]
    /// Returns vector port bindings for a retained design occurrence.
    pub fn design_instance_connections(
        &self,
        instance: DesignInstanceId,
    ) -> Option<&[DesignInstanceConnection]> {
        let instance = self.design_instance(instance)?;
        self.design_connections
            .get(instance.connection_start as usize..instance.connection_end as usize)
    }

    #[must_use]
    /// Resolves the retained design port name for a connection.
    pub fn design_connection_port(&self, connection: &DesignInstanceConnection) -> Option<&str> {
        self.names.resolve(connection.port)
    }

    #[must_use]
    /// Returns scalar signals stored for a retained design port binding.
    pub fn design_connection_signals(
        &self,
        connection: &DesignInstanceConnection,
    ) -> Option<&[ConnectionSignal]> {
        self.design_connection_signals
            .get(connection.signal_start as usize..connection.signal_end as usize)
    }

    #[must_use]
    /// Resolves a live cell ID; tombstones return `None`.
    pub fn cell(&self, cell: CellId) -> Option<&MappedCell> {
        self.cells
            .get(cell.index())
            .filter(|slot| slot.live)
            .map(|slot| &slot.cell)
    }

    #[must_use]
    /// Resolves a live cell's instance name.
    pub fn cell_name(&self, cell: CellId) -> Option<&str> {
        self.cell(cell)
            .and_then(|cell| self.names.resolve(cell.name))
    }

    #[must_use]
    /// Resolves a live cell's target-library type name.
    pub fn cell_type(&self, cell: CellId) -> Option<&str> {
        self.cell(cell)
            .and_then(|cell| self.names.resolve(cell.cell_type))
    }

    #[must_use]
    /// Resolves a cell pin name.
    pub fn pin_name(&self, connection: &PinConnection) -> Option<&str> {
        self.names.resolve(connection.pin)
    }

    #[must_use]
    /// Returns all pin connections owned by a live cell.
    pub fn connections(&self, cell: CellId) -> Option<&[PinConnection]> {
        let cell = self.cell(cell)?;
        self.connections
            .get(cell.connection_start as usize..cell.connection_end as usize)
    }

    #[must_use]
    /// Iterates stable pin IDs owned by a live cell.
    ///
    /// # Panics
    ///
    /// Panics only if a validated cell range contains a pin index outside the
    /// typed pin-ID capacity.
    pub fn pin_ids(&self, cell: CellId) -> Option<impl Iterator<Item = PinId> + '_> {
        let cell = self.cell(cell)?;
        let start = cell.connection_start as usize;
        let end = cell.connection_end as usize;
        Some((start..end).map(|index| {
            PinId::from_index(index).expect("existing pin connection index fits its ID")
        }))
    }

    #[must_use]
    /// Resolves a pin ID whose owning cell remains live.
    pub fn connection(&self, pin: PinId) -> Option<&PinConnection> {
        self.pin_owner(pin)?;
        self.connections.get(pin.index())
    }

    #[must_use]
    /// Returns the live cell that owns `pin`.
    pub fn pin_owner(&self, pin: PinId) -> Option<CellId> {
        let owner = *self.pin_owners.get(pin.index())?;
        let cell = self.cell(owner)?;
        (cell.connection_start as usize..cell.connection_end as usize)
            .contains(&pin.index())
            .then_some(owner)
    }

    /// Iterates pins attached to a live net in intrusive-list order.
    #[must_use]
    pub fn pins_on_net(&self, net: NetId) -> Option<impl Iterator<Item = PinId> + '_> {
        let mut cursor = self
            .nets
            .get(net.index())
            .filter(|slot| slot.live)
            .and_then(|_| self.net_pins.get(net.index()))?
            .head;
        Some(std::iter::from_fn(move || {
            let pin = cursor?;
            cursor = self.pin_links[pin.index()].next;
            Some(pin)
        }))
    }

    #[must_use]
    /// Returns explicit constant drivers in insertion order.
    pub fn constant_drivers(&self) -> &[(NetId, bool)] {
        &self.constant_drivers
    }

    #[must_use]
    /// Resolves the optional name of a live net.
    pub fn net_name(&self, net: NetId) -> Option<&str> {
        self.nets
            .get(net.index())
            .filter(|slot| slot.live)
            .and_then(|slot| slot.name)
            .and_then(|name| self.names.resolve(name))
    }

    #[must_use]
    /// Returns whether `net` names a live slot.
    pub fn is_live_net(&self, net: NetId) -> bool {
        self.nets.get(net.index()).is_some_and(|slot| slot.live)
    }

    #[must_use]
    /// Returns whether `cell` names a live slot.
    pub fn is_live_cell(&self, cell: CellId) -> bool {
        self.cells.get(cell.index()).is_some_and(|slot| slot.live)
    }

    /// Releases construction slack after the netlist crosses its publication
    /// barrier. This does not renumber slot IDs or remove tombstones.
    pub fn compact(&mut self) {
        self.names.compact();
        self.nets.shrink_to_fit();
        self.ports.shrink_to_fit();
        self.port_nets.shrink_to_fit();
        self.cells.shrink_to_fit();
        self.connections.shrink_to_fit();
        self.pin_owners.shrink_to_fit();
        self.pin_links.shrink_to_fit();
        self.net_pins.shrink_to_fit();
        self.design_instances.shrink_to_fit();
        self.design_connections.shrink_to_fit();
        self.design_connection_signals.shrink_to_fit();
        self.constant_drivers.shrink_to_fit();
        self.external_nets.shrink_to_fit();
    }

    /// Deterministic byte model for heap storage owned by this published
    /// netlist. Live arena lengths, not allocator-dependent capacities, define
    /// the payload; each modeled allocation includes a 25% allocator margin
    /// and two words of metadata.
    #[must_use]
    pub fn owned_memory_bytes(&self) -> usize {
        self.names
            .owned_memory_bytes()
            .saturating_add(resident::slice_bytes::<NetSlot>(self.nets.len()))
            .saturating_add(resident::slice_bytes::<MappedPort>(self.ports.len()))
            .saturating_add(resident::slice_bytes::<NetId>(self.port_nets.len()))
            .saturating_add(resident::slice_bytes::<CellSlot>(self.cells.len()))
            .saturating_add(resident::slice_bytes::<PinConnection>(
                self.connections.len(),
            ))
            .saturating_add(resident::slice_bytes::<CellId>(self.pin_owners.len()))
            .saturating_add(resident::slice_bytes::<PinLinks>(self.pin_links.len()))
            .saturating_add(resident::slice_bytes::<NetPins>(self.net_pins.len()))
            .saturating_add(resident::slice_bytes::<MappedDesignInstance>(
                self.design_instances.len(),
            ))
            .saturating_add(resident::slice_bytes::<DesignInstanceConnection>(
                self.design_connections.len(),
            ))
            .saturating_add(resident::slice_bytes::<ConnectionSignal>(
                self.design_connection_signals.len(),
            ))
            .saturating_add(resident::slice_bytes::<(NetId, bool)>(
                self.constant_drivers.len(),
            ))
            .saturating_add(resident::slice_bytes::<NetId>(self.external_nets.len()))
    }

    fn validate_unique_names(&self, used: &mut [u8]) -> Result<(), MappedError> {
        let design_name = self.names.resolve(self.name).ok_or_else(|| {
            MappedError::invariant("mapped design has an invalid name identifier")
        })?;
        if design_name.trim().is_empty() {
            return Err(MappedError::invariant("mapped design name cannot be empty"));
        }
        for port in &self.ports {
            let resolved = self.names.resolve(port.name).ok_or_else(|| {
                MappedError::invariant("mapped port has an invalid name identifier")
            })?;
            if resolved.trim().is_empty() {
                return Err(MappedError::invariant("mapped port name cannot be empty"));
            }
            let mark = used.get_mut(port.name.raw() as usize).ok_or_else(|| {
                MappedError::invariant("mapped port name is outside the name arena")
            })?;
            if std::mem::replace(mark, 1) != 0 {
                return Err(MappedError::invariant(format!(
                    "mapped netlist contains duplicate port name '{resolved}'"
                )));
            }
        }
        used.fill(0);
        for cell in self.cell_ids() {
            let row = &self.cells[cell.index()].cell;
            let name = row.name;
            let resolved = self.names.resolve(name).ok_or_else(|| {
                MappedError::invariant("mapped cell has an invalid name identifier")
            })?;
            let cell_type = self.names.resolve(row.cell_type).ok_or_else(|| {
                MappedError::invariant("mapped cell has an invalid type identifier")
            })?;
            if resolved.trim().is_empty() || cell_type.trim().is_empty() {
                return Err(MappedError::invariant(
                    "mapped cells require non-empty instance and cell type names",
                ));
            }
            let mark = used.get_mut(name.raw() as usize).ok_or_else(|| {
                MappedError::invariant("mapped cell name is outside the name arena")
            })?;
            if std::mem::replace(mark, 1) != 0 {
                return Err(MappedError::invariant(format!(
                    "mapped netlist contains duplicate cell name '{resolved}'"
                )));
            }
            let mut pins = std::collections::BTreeSet::new();
            for connection in self
                .connections(cell)
                .ok_or_else(|| MappedError::invariant("mapped cell has an invalid pin range"))?
            {
                let pin = self.names.resolve(connection.pin).ok_or_else(|| {
                    MappedError::invariant("mapped cell pin has an invalid name identifier")
                })?;
                if pin.trim().is_empty() || !pins.insert(connection.pin) {
                    return Err(MappedError::invariant(format!(
                        "mapped cell '{resolved}' has an empty or duplicate pin name"
                    )));
                }
            }
        }
        for instance in self.design_instance_ids() {
            let row = self.design_instances[instance.index()];
            let name = row.name;
            let resolved = self.names.resolve(name).ok_or_else(|| {
                MappedError::invariant("mapped design instance has an invalid name identifier")
            })?;
            let module = self.names.resolve(row.module).ok_or_else(|| {
                MappedError::invariant("mapped design instance has an invalid module identifier")
            })?;
            if resolved.trim().is_empty() || module.trim().is_empty() {
                return Err(MappedError::invariant(
                    "mapped design instances require non-empty instance and module names",
                ));
            }
            let mark = used.get_mut(name.raw() as usize).ok_or_else(|| {
                MappedError::invariant("mapped design instance name is outside the name arena")
            })?;
            if std::mem::replace(mark, 1) != 0 {
                return Err(MappedError::invariant(format!(
                    "mapped netlist contains duplicate instance name '{resolved}'"
                )));
            }
            let mut ports = std::collections::BTreeSet::new();
            for connection in self.design_instance_connections(instance).ok_or_else(|| {
                MappedError::invariant("mapped design instance has an invalid connection range")
            })? {
                let port = self.names.resolve(connection.port).ok_or_else(|| {
                    MappedError::invariant(
                        "mapped design instance port has an invalid name identifier",
                    )
                })?;
                if port.trim().is_empty() || !ports.insert(connection.port) {
                    return Err(MappedError::invariant(format!(
                        "mapped design instance '{resolved}' has an empty or duplicate port binding"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Validates all invariants required of a restored published checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when the netlist is unpublished or contains an
    /// invalid name, range, reference, adjacency link, or duplicate instance.
    pub fn validate_checkpoint(&self) -> Result<(), MappedError> {
        if !self.published {
            return Err(MappedError::invariant(
                "checkpoint contains an unpublished mapped netlist",
            ));
        }
        if self.names.resolve(self.name).is_none() {
            return Err(MappedError::invariant(
                "checkpoint mapped netlist has an invalid design name",
            ));
        }
        self.validate_dense_publication_slots()?;
        let mut scratch = self.validation_scratch();
        self.validate_references()?;
        self.validate_external_net_index(&mut scratch[..self.nets.len()])?;
        scratch.fill(0);
        self.validate_connectivity(&mut scratch[..self.connections.len()])?;
        scratch.fill(0);
        self.validate_unique_names(&mut scratch[..self.names.entry_count()])
    }

    fn validate_dense_publication_slots(&self) -> Result<(), MappedError> {
        self.validate_live_counts()?;
        if self.net_count() != self.net_slot_count() || self.cell_count() != self.cell_slot_count()
        {
            return Err(MappedError::invariant(
                "mapped publication contains optimization-time tombstones; repack it first",
            ));
        }
        Ok(())
    }

    fn validate_live_counts(&self) -> Result<(), MappedError> {
        let nets = self.nets.iter().filter(|net| net.live).count();
        let cells = self.cells.iter().filter(|cell| cell.live).count();
        if nets != self.live_net_count || cells != self.live_cell_count {
            return Err(MappedError::invariant(
                "mapped live-count index disagrees with stable slots",
            ));
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "validation walks each mapped arena once while sharing range-contiguity state"
    )]
    fn validate_references(&self) -> Result<(), MappedError> {
        self.require_name(self.name, "design")?;
        validate_appended_ranges(
            self.ports.iter().map(|port| (port.net_start, port.net_end)),
            self.port_nets.len(),
            "port net",
            false,
        )?;
        validate_appended_ranges(
            self.cells
                .iter()
                .map(|slot| (slot.cell.connection_start, slot.cell.connection_end)),
            self.connections.len(),
            "cell pin",
            false,
        )?;
        validate_appended_ranges(
            self.design_instances
                .iter()
                .map(|instance| (instance.connection_start, instance.connection_end)),
            self.design_connections.len(),
            "design-instance connection",
            false,
        )?;
        validate_appended_ranges(
            self.design_connections
                .iter()
                .map(|connection| (connection.signal_start, connection.signal_end)),
            self.design_connection_signals.len(),
            "design-instance signal",
            true,
        )?;
        for slot in &self.nets {
            if let Some(name) = slot.name {
                self.require_name(name, "net")?;
                if self
                    .names
                    .resolve(name)
                    .is_some_and(|name| name.trim().is_empty())
                {
                    return Err(MappedError::invariant(
                        "mapped net name cannot be empty; use an unnamed net instead",
                    ));
                }
            }
        }
        for port in &self.ports {
            self.require_name(port.name, "port")?;
            let nets = self
                .port_nets
                .get(port.net_start as usize..port.net_end as usize)
                .ok_or_else(|| MappedError::invariant("mapped port has an invalid net range"))?;
            if nets.is_empty() {
                return Err(MappedError::invariant("mapped port has no connected bits"));
            }
            for &net in nets {
                if !self.is_live_net(net) {
                    return Err(MappedError::invariant(format!(
                        "mapped port references removed net {net:?} at publication"
                    )));
                }
            }
        }
        for &(net, _) in &self.constant_drivers {
            if !self.is_live_net(net) {
                return Err(MappedError::invariant(format!(
                    "mapped constant driver references removed net {net:?} at publication"
                )));
            }
        }
        for (cell_index, slot) in self.cells.iter().enumerate() {
            let cell = CellId::from_index(cell_index)?;
            self.require_name(slot.cell.name, "cell")?;
            self.require_name(slot.cell.cell_type, "cell type")?;
            let connections = self
                .connections
                .get(slot.cell.connection_start as usize..slot.cell.connection_end as usize)
                .ok_or_else(|| MappedError::invariant("mapped cell has an invalid pin range"))?;
            for connection in connections {
                self.require_name(connection.pin, "cell pin")?;
                if slot.live
                    && let ConnectionSignal::Net(net) = connection.signal
                    && !self.is_live_net(net)
                {
                    return Err(MappedError::invariant(format!(
                        "mapped cell {cell:?} references removed net {net:?} at publication"
                    )));
                }
            }
        }
        for instance in self.design_instance_ids() {
            let row = self.design_instances[instance.index()];
            self.require_name(row.name, "design instance")?;
            self.require_name(row.module, "design instance module")?;
            let connections = self
                .design_connections
                .get(row.connection_start as usize..row.connection_end as usize)
                .ok_or_else(|| {
                    MappedError::invariant("mapped design instance has an invalid connection range")
                })?;
            for connection in connections {
                self.require_name(connection.port, "design instance port")?;
                let signals = self
                    .design_connection_signals
                    .get(connection.signal_start as usize..connection.signal_end as usize)
                    .ok_or_else(|| {
                        MappedError::invariant(
                            "mapped design instance connection has an invalid signal range",
                        )
                    })?;
                for signal in signals {
                    if let ConnectionSignal::Net(net) = signal
                        && !self.is_live_net(*net)
                    {
                        return Err(MappedError::invariant(format!(
                            "mapped design instance {instance:?} references removed net {net:?} at publication"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn require_name(&self, name: NameId, kind: &str) -> Result<(), MappedError> {
        self.names.resolve(name).map(|_| ()).ok_or_else(|| {
            MappedError::invariant(format!("mapped {kind} has an invalid name identifier"))
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "bidirectional connectivity is verified as one symmetric invariant"
    )]
    fn validate_connectivity(&self, seen: &mut [u8]) -> Result<(), MappedError> {
        if self.connections.len() != self.pin_owners.len()
            || self.connections.len() != self.pin_links.len()
            || self.nets.len() != self.net_pins.len()
        {
            return Err(MappedError::invariant(
                "mapped connectivity arenas have inconsistent lengths",
            ));
        }
        for (net_index, slot) in self.nets.iter().enumerate() {
            let net = NetId::from_index(net_index)?;
            let adjacency = self.net_pins[net_index];
            if !slot.live {
                if adjacency != NetPins::default() {
                    return Err(MappedError::invariant(format!(
                        "removed net {net:?} retains pin adjacency"
                    )));
                }
                continue;
            }
            let mut previous = None;
            let mut current = adjacency.head;
            let mut count = 0usize;
            while let Some(pin) = current {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| MappedError::invariant("mapped pin adjacency overflow"))?;
                if count > self.connections.len() {
                    return Err(MappedError::invariant(format!(
                        "mapped net {net:?} pin adjacency contains a cycle"
                    )));
                }
                let connection = self.connections.get(pin.index()).ok_or_else(|| {
                    MappedError::invariant(format!(
                        "mapped net {net:?} references unknown pin {pin:?}"
                    ))
                })?;
                if connection.signal != ConnectionSignal::Net(net) {
                    return Err(MappedError::invariant(format!(
                        "mapped net {net:?} adjacency contains pin {pin:?} connected elsewhere"
                    )));
                }
                if self.pin_owner(pin).is_none() {
                    return Err(MappedError::invariant(format!(
                        "mapped net {net:?} adjacency contains ownerless pin {pin:?}"
                    )));
                }
                if std::mem::replace(&mut seen[pin.index()], 1) != 0 {
                    return Err(MappedError::invariant(format!(
                        "mapped pin {pin:?} occurs in multiple net adjacency lists"
                    )));
                }
                let links = self.pin_links[pin.index()];
                if links.previous != previous {
                    return Err(MappedError::invariant(format!(
                        "mapped pin {pin:?} has an inconsistent previous link"
                    )));
                }
                previous = Some(pin);
                current = links.next;
            }
            if previous != adjacency.tail {
                return Err(MappedError::invariant(format!(
                    "mapped net {net:?} has an inconsistent adjacency tail"
                )));
            }
        }
        for (cell_index, slot) in self.cells.iter().enumerate() {
            let cell = CellId::from_index(cell_index)?;
            let range = slot.cell.connection_start as usize..slot.cell.connection_end as usize;
            if range.end > self.connections.len() {
                return Err(MappedError::invariant(format!(
                    "mapped cell {cell:?} has an invalid pin range"
                )));
            }
            for pin_index in range {
                let pin = PinId::from_index(pin_index)?;
                if self.pin_owners[pin_index] != cell {
                    return Err(MappedError::invariant(format!(
                        "mapped pin {pin:?} has an inconsistent owner"
                    )));
                }
                let must_be_linked = slot.live
                    && matches!(self.connections[pin_index].signal, ConnectionSignal::Net(_));
                if (seen[pin_index] != 0) != must_be_linked {
                    return Err(MappedError::invariant(format!(
                        "mapped pin {pin:?} has inconsistent net adjacency membership"
                    )));
                }
                if !must_be_linked && self.pin_links[pin_index] != PinLinks::default() {
                    return Err(MappedError::invariant(format!(
                        "unlinked mapped pin {pin:?} retains adjacency links"
                    )));
                }
            }
        }
        for (pin_index, &owner) in self.pin_owners.iter().enumerate() {
            let cell = self.cells.get(owner.index()).ok_or_else(|| {
                MappedError::invariant(format!(
                    "mapped pin {pin_index} references unknown owner {owner:?}"
                ))
            })?;
            if !(cell.cell.connection_start as usize..cell.cell.connection_end as usize)
                .contains(&pin_index)
            {
                return Err(MappedError::invariant(format!(
                    "mapped pin {pin_index} lies outside owner {owner:?}'s pin range"
                )));
            }
        }
        Ok(())
    }
}

fn validate_appended_ranges(
    ranges: impl IntoIterator<Item = (u32, u32)>,
    arena_len: usize,
    kind: &str,
    require_nonempty: bool,
) -> Result<(), MappedError> {
    let mut expected_start = 0usize;
    for (index, (start, end)) in ranges.into_iter().enumerate() {
        let start = start as usize;
        let end = end as usize;
        if start != expected_start || end < start || (require_nonempty && end == start) {
            return Err(MappedError::invariant(format!(
                "mapped {kind} range {index} is not a valid appended arena row"
            )));
        }
        expected_start = end;
    }
    if expected_start != arena_len {
        return Err(MappedError::invariant(format!(
            "mapped {kind} ranges do not cover their arena"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapped_netlists_store_ports_cells_and_connections_in_ranges() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let a = builder.add_net(Some("a")).unwrap();
        let y = builder.add_net(Some("y")).unwrap();
        builder.add_port("a", PortDirection::Input, &[a]).unwrap();
        builder.add_port("y", PortDirection::Output, &[y]).unwrap();
        let cell = builder
            .add_cell(
                "U0",
                "INVX1",
                Some(7),
                &[
                    ("A".to_string(), Some(0), ConnectionSignal::Net(a)),
                    ("Y".to_string(), Some(1), ConnectionSignal::Net(y)),
                ],
            )
            .unwrap();
        let netlist = builder.freeze().unwrap();

        assert_eq!(netlist.name(), "top");
        assert_eq!(netlist.base_revision(), RevisionId::INITIAL);
        assert_eq!(netlist.net_count(), 2);
        assert_eq!(netlist.connections(cell).unwrap().len(), 2);
    }

    #[test]
    fn packed_cells_receive_exact_dense_cell_and_pin_ranges() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let a = builder.add_net(Some("a")).unwrap();
        let y = builder.add_net(Some("y")).unwrap();
        let z = builder.add_net(Some("z")).unwrap();
        let ids = builder
            .add_cells_packed(vec![
                MappedCellSpec {
                    name: "U0".to_string(),
                    cell_type: "INVX1".to_string(),
                    library_cell: Some(0),
                    connections: vec![
                        ("A".to_string(), Some(0), ConnectionSignal::Net(a)),
                        ("Y".to_string(), Some(1), ConnectionSignal::Net(y)),
                    ],
                },
                MappedCellSpec {
                    name: "U1".to_string(),
                    cell_type: "BUFX1".to_string(),
                    library_cell: Some(1),
                    connections: vec![
                        ("A".to_string(), Some(0), ConnectionSignal::Net(y)),
                        ("Y".to_string(), Some(1), ConnectionSignal::Net(z)),
                    ],
                },
            ])
            .unwrap();
        assert_eq!(ids[0].index(), 0);
        assert_eq!(ids[1].index(), 1);

        let netlist = builder.freeze().unwrap();
        assert_eq!(netlist.pin_ids(ids[0]).unwrap().count(), 2);
        assert_eq!(netlist.pin_ids(ids[1]).unwrap().count(), 2);
        assert_eq!(netlist.pins_on_net(y).unwrap().count(), 2);
    }

    #[test]
    fn mapped_netlists_keep_design_instances_separate_from_library_cells() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let a0 = builder.add_net(Some("a[0]")).unwrap();
        let a1 = builder.add_net(Some("a[1]")).unwrap();
        let instance = builder
            .add_design_instance(
                "u_child",
                "child",
                &[(
                    "a".to_string(),
                    vec![ConnectionSignal::Net(a0), ConnectionSignal::Net(a1)],
                )],
            )
            .unwrap();
        let netlist = builder.freeze().unwrap();
        let (netlist, _) = netlist.finalize_for_publication().unwrap();

        assert_eq!(netlist.cell_count(), 0);
        assert_eq!(netlist.design_instance_count(), 1);
        assert_eq!(netlist.design_instance_name(instance), Some("u_child"));
        assert_eq!(netlist.design_instance_module(instance), Some("child"));
        let connection = &netlist.design_instance_connections(instance).unwrap()[0];
        assert_eq!(netlist.design_connection_port(connection), Some("a"));
        assert_eq!(
            netlist.design_connection_signals(connection),
            Some([ConnectionSignal::Net(a0), ConnectionSignal::Net(a1)].as_slice())
        );
    }

    #[test]
    fn failed_design_instance_addition_does_not_pollute_connection_arenas() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let net = builder.add_net(Some("a")).unwrap();
        let error = builder
            .add_design_instance(
                "broken",
                "child",
                &[
                    ("a".to_string(), vec![ConnectionSignal::Net(net)]),
                    ("empty".to_string(), Vec::new()),
                ],
            )
            .unwrap_err();
        assert!(error.to_string().contains("no connected bits"));

        let instance = builder
            .add_design_instance(
                "valid",
                "child",
                &[("a".to_string(), vec![ConnectionSignal::Net(net)])],
            )
            .unwrap();
        let netlist = builder.freeze().unwrap();
        assert_eq!(netlist.design_instance_count(), 1);
        assert_eq!(
            netlist.design_instance_connections(instance).unwrap().len(),
            1
        );
    }

    #[test]
    fn mapped_builder_rejects_invalid_publication_names_before_mutation() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let net = builder.add_net(Some("a")).unwrap();
        builder.add_port("a", PortDirection::Input, &[net]).unwrap();
        assert!(
            builder
                .add_port("a", PortDirection::Output, &[net])
                .is_err()
        );
        assert!(
            builder
                .add_cell(
                    "bad",
                    "BUF",
                    None,
                    &[
                        ("A".to_string(), None, ConnectionSignal::Net(net)),
                        ("A".to_string(), None, ConnectionSignal::Net(net)),
                    ],
                )
                .is_err()
        );
        builder
            .add_cell(
                "good",
                "BUF",
                None,
                &[("A".to_string(), None, ConnectionSignal::Net(net))],
            )
            .unwrap();

        let netlist = builder.freeze().unwrap();
        assert_eq!(netlist.ports().len(), 1);
        assert_eq!(netlist.cell_count(), 1);
    }

    #[test]
    fn mapped_slot_ids_and_intrusive_links_have_compact_layouts() {
        assert_eq!(std::mem::size_of::<NetId>(), std::mem::size_of::<u32>());
        assert_eq!(
            std::mem::size_of::<Option<PinId>>(),
            std::mem::size_of::<u32>()
        );
        assert_eq!(
            std::mem::size_of::<PinLinks>(),
            2 * std::mem::size_of::<u32>()
        );
        assert_eq!(
            std::mem::size_of::<NetPins>(),
            2 * std::mem::size_of::<u32>()
        );
    }

    #[test]
    fn checkpoint_validation_covers_all_names_and_arena_ranges() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let a = builder.add_net(Some("a")).unwrap();
        builder.add_port("a", PortDirection::Input, &[a]).unwrap();
        builder
            .add_cell(
                "U0",
                "BUF",
                None,
                &[("A".to_string(), None, ConnectionSignal::Net(a))],
            )
            .unwrap();
        let netlist = builder.freeze().unwrap();
        let (mut netlist, _) = netlist.finalize_for_publication().unwrap();

        let cell_type = netlist.cells[0].cell.cell_type;
        netlist.cells[0].cell.cell_type = NameId::from_index(netlist.names.entry_count()).unwrap();
        assert!(netlist.validate_checkpoint().is_err());
        netlist.cells[0].cell.cell_type = cell_type;

        netlist.ports[0].net_end = u32::MAX;
        assert!(netlist.validate_checkpoint().is_err());
        netlist.ports[0].net_start = 1;
        netlist.ports[0].net_end = 1;
        assert!(netlist.validate_checkpoint().is_err());
    }
}
