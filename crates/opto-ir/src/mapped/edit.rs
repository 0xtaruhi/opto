// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Optimistic, bounded-footprint edits for mapped-netlist regions.
//!
//! Read-only analysis captures object versions in [`RegionSnapshot`]. Workers
//! return a [`RegionDelta`] containing temporary IDs; the owning thread resolves
//! and validates it, then records only touched state in [`AppliedRegionDelta`]
//! so commit and rollback are deterministic and proportional to the region.

use super::{
    CellId, CellSlot, ConnectionSignal, MappedCell, MappedError, MappedGenerationId, MappedNetlist,
    NetId, NetPins, NetSlot, PinConnection, PinId, PinLinks,
};
use crate::{NameCheckpoint, NameId};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

mod helpers;
mod transaction;

use helpers::{
    link_pin, operation_names, save_cell, save_net, touch_signal_net, unlink_pin, validate_signal,
};
use thiserror::Error;

static NEXT_REGION_DELTA_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RegionDeltaId(NonZeroU64);

impl RegionDeltaId {
    fn fresh() -> Self {
        let raw = NEXT_REGION_DELTA_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("region delta ID space is exhausted");
        Self(NonZeroU64::new(raw).expect("region delta IDs start at one"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Delta-local net identity resolved only when a region edit is applied.
pub struct TempNetId {
    owner: RegionDeltaId,
    index: u32,
}

impl TempNetId {
    /// Returns the ordinal within the owning delta.
    ///
    /// Ordinals are meaningful only for semantic comparison; they do not make
    /// IDs from different deltas interchangeable.
    #[must_use]
    pub fn ordinal(self) -> u32 {
        self.index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Delta-local cell identity resolved only when a region edit is applied.
pub struct TempCellId {
    owner: RegionDeltaId,
    index: u32,
}

impl TempCellId {
    /// Returns the ordinal within the owning delta.
    ///
    /// Ordinals are meaningful only for semantic comparison; they do not make
    /// IDs from different deltas interchangeable.
    #[must_use]
    pub fn ordinal(self) -> u32 {
        self.index
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Existing, newly added, or constant signal referenced by a region delta.
pub enum ConnectionRef {
    /// Live net contained in the region snapshot.
    Net(NetId),
    /// Net added earlier in the same delta.
    NewNet(TempNetId),
    /// Constant logic value.
    Constant(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Complete specification of a target cell added by a region delta.
pub struct CellSpec {
    /// Requested unique instance name.
    pub name: String,
    /// Target-library cell type.
    pub cell_type: String,
    /// Optional dense target-cell catalog ID.
    pub library_cell: Option<u32>,
    /// Pin name, optional library pin ID, and signal for each binding.
    pub connections: Vec<(String, Option<u16>, ConnectionRef)>,
}

impl CellSpec {
    /// Creates a cell specification without pin connections.
    pub fn new(
        name: impl Into<String>,
        cell_type: impl Into<String>,
        library_cell: Option<u32>,
    ) -> Self {
        Self {
            name: name.into(),
            cell_type: cell_type.into(),
            library_cell,
            connections: Vec::new(),
        }
    }

    /// Appends one pin binding and returns the updated specification.
    #[must_use]
    pub fn connect(
        mut self,
        pin: impl Into<String>,
        library_pin: Option<u16>,
        signal: ConnectionRef,
    ) -> Self {
        self.connections.push((pin.into(), library_pin, signal));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Versioned read set captured for optimistic regional optimization.
///
/// Object membership is explicit. A delta may read and modify only the cells
/// and nets recorded here, preventing hidden whole-netlist dependencies.
pub struct RegionSnapshot {
    generation: MappedGenerationId,
    cells: BTreeMap<CellId, u64>,
    nets: BTreeMap<NetId, u64>,
}

impl RegionSnapshot {
    /// Returns the mapped-netlist owner from which this snapshot was captured.
    #[must_use]
    pub fn generation_id(&self) -> MappedGenerationId {
        self.generation
    }

    #[must_use]
    /// Returns whether `cell` belongs to the snapshot.
    pub fn contains_cell(&self, cell: CellId) -> bool {
        self.cells.contains_key(&cell)
    }

    #[must_use]
    /// Returns whether `net` belongs to the snapshot.
    pub fn contains_net(&self, net: NetId) -> bool {
        self.nets.contains_key(&net)
    }

    /// Iterates snapshotted cell IDs in deterministic order.
    pub fn cell_ids(&self) -> impl Iterator<Item = CellId> + '_ {
        self.cells.keys().copied()
    }

    /// Iterates snapshotted net IDs in deterministic order.
    pub fn net_ids(&self) -> impl Iterator<Item = NetId> + '_ {
        self.nets.keys().copied()
    }
}

#[derive(Debug, Clone)]
/// Ordered mutation plan produced from one [`RegionSnapshot`].
///
/// Construction performs footprint checks; application revalidates object
/// versions and complete connectivity before changing the owning netlist.
pub struct RegionDelta {
    snapshot: RegionSnapshot,
    identity: RegionDeltaId,
    operations: Vec<RegionOperation>,
    next_temp_net: u32,
    next_temp_cell: u32,
}

impl PartialEq for RegionDelta {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot == other.snapshot
            && self.next_temp_net == other.next_temp_net
            && self.next_temp_cell == other.next_temp_cell
            && self.operations.len() == other.operations.len()
            && self
                .operations
                .iter()
                .zip(&other.operations)
                .all(|(left, right)| left.semantically_eq(right))
    }
}

impl Eq for RegionDelta {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegionOperation {
    AddNet {
        id: TempNetId,
        name: Option<String>,
    },
    AddCell {
        id: TempCellId,
        spec: CellSpec,
    },
    RemoveCell(CellId),
    RemoveNet(NetId),
    ReconnectPin {
        pin: PinId,
        signal: ConnectionRef,
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

impl RegionOperation {
    fn semantically_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::AddNet {
                    id: left_id,
                    name: left_name,
                },
                Self::AddNet {
                    id: right_id,
                    name: right_name,
                },
            ) => left_id.ordinal() == right_id.ordinal() && left_name == right_name,
            (
                Self::AddCell {
                    id: left_id,
                    spec: left_spec,
                },
                Self::AddCell {
                    id: right_id,
                    spec: right_spec,
                },
            ) => {
                left_id.ordinal() == right_id.ordinal()
                    && cell_specs_semantically_equal(left_spec, right_spec)
            }
            (Self::RemoveCell(left), Self::RemoveCell(right)) => left == right,
            (Self::RemoveNet(left), Self::RemoveNet(right)) => left == right,
            (
                Self::ReconnectPin {
                    pin: left_pin,
                    signal: left_signal,
                },
                Self::ReconnectPin {
                    pin: right_pin,
                    signal: right_signal,
                },
            ) => left_pin == right_pin && signals_semantically_equal(*left_signal, *right_signal),
            (
                Self::ReplaceCell {
                    cell: left_cell,
                    cell_type: left_type,
                    library_cell: left_library_cell,
                },
                Self::ReplaceCell {
                    cell: right_cell,
                    cell_type: right_type,
                    library_cell: right_library_cell,
                },
            ) => {
                left_cell == right_cell
                    && left_type == right_type
                    && left_library_cell == right_library_cell
            }
            (
                Self::RenameCell {
                    cell: left_cell,
                    name: left_name,
                },
                Self::RenameCell {
                    cell: right_cell,
                    name: right_name,
                },
            ) => left_cell == right_cell && left_name == right_name,
            (
                Self::RenameNet {
                    net: left_net,
                    name: left_name,
                },
                Self::RenameNet {
                    net: right_net,
                    name: right_name,
                },
            ) => left_net == right_net && left_name == right_name,
            _ => false,
        }
    }
}

fn cell_specs_semantically_equal(left: &CellSpec, right: &CellSpec) -> bool {
    left.name == right.name
        && left.cell_type == right.cell_type
        && left.library_cell == right.library_cell
        && left.connections.len() == right.connections.len()
        && left
            .connections
            .iter()
            .zip(&right.connections)
            .all(|(left, right)| {
                left.0 == right.0
                    && left.1 == right.1
                    && signals_semantically_equal(left.2, right.2)
            })
}

fn signals_semantically_equal(left: ConnectionRef, right: ConnectionRef) -> bool {
    match (left, right) {
        (ConnectionRef::Net(left), ConnectionRef::Net(right)) => left == right,
        (ConnectionRef::NewNet(left), ConnectionRef::NewNet(right)) => {
            left.ordinal() == right.ordinal()
        }
        (ConnectionRef::Constant(left), ConnectionRef::Constant(right)) => left == right,
        _ => false,
    }
}

impl RegionDelta {
    #[must_use]
    /// Creates an empty mutation plan for `snapshot`.
    pub fn new(snapshot: RegionSnapshot) -> Self {
        Self {
            snapshot,
            identity: RegionDeltaId::fresh(),
            operations: Vec::new(),
            next_temp_net: 0,
            next_temp_cell: 0,
        }
    }

    #[must_use]
    /// Returns the immutable read set and versions this delta depends on.
    pub fn snapshot(&self) -> &RegionSnapshot {
        &self.snapshot
    }

    /// Plans a new net and returns its delta-local ID.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when temporary net IDs exceed 32-bit capacity.
    pub fn add_net(&mut self, name: Option<String>) -> Result<TempNetId, MappedError> {
        let id = TempNetId {
            owner: self.identity,
            index: self.next_temp_net,
        };
        self.next_temp_net = self
            .next_temp_net
            .checked_add(1)
            .ok_or_else(|| MappedError::capacity("temporary net ID"))?;
        self.operations.push(RegionOperation::AddNet { id, name });
        Ok(id)
    }

    /// Plans a new cell and returns its delta-local ID.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when temporary cell IDs exceed 32-bit capacity.
    pub fn add_cell(&mut self, spec: CellSpec) -> Result<TempCellId, MappedError> {
        let id = TempCellId {
            owner: self.identity,
            index: self.next_temp_cell,
        };
        self.next_temp_cell = self
            .next_temp_cell
            .checked_add(1)
            .ok_or_else(|| MappedError::capacity("temporary cell ID"))?;
        self.operations.push(RegionOperation::AddCell { id, spec });
        Ok(id)
    }

    /// Plans removal of a snapshotted cell.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when `cell` is outside the snapshot footprint.
    pub fn remove_cell(&mut self, cell: CellId) -> Result<(), MappedError> {
        self.require_cell(cell)?;
        self.operations.push(RegionOperation::RemoveCell(cell));
        Ok(())
    }

    /// Plans removal of a snapshotted net.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when `net` is outside the snapshot footprint.
    pub fn remove_net(&mut self, net: NetId) -> Result<(), MappedError> {
        self.require_net(net)?;
        self.operations.push(RegionOperation::RemoveNet(net));
        Ok(())
    }

    /// Plans reconnection of one pin.
    ///
    /// Existing-net references must belong to the snapshot. Pin ownership and
    /// temporary-net validity are checked during application.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when an existing net is outside the footprint.
    pub fn reconnect_pin(&mut self, pin: PinId, signal: ConnectionRef) -> Result<(), MappedError> {
        if let ConnectionRef::Net(net) = signal {
            self.require_net(net)?;
        }
        self.operations
            .push(RegionOperation::ReconnectPin { pin, signal });
        Ok(())
    }

    /// Plans replacement of a cell's target-library type.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when `cell` is outside the snapshot footprint.
    pub fn replace_cell(
        &mut self,
        cell: CellId,
        cell_type: impl Into<String>,
        library_cell: Option<u32>,
    ) -> Result<(), MappedError> {
        self.require_cell(cell)?;
        self.operations.push(RegionOperation::ReplaceCell {
            cell,
            cell_type: cell_type.into(),
            library_cell,
        });
        Ok(())
    }

    /// Plans a live cell rename.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when `cell` is outside the snapshot footprint.
    pub fn rename_cell(
        &mut self,
        cell: CellId,
        name: impl Into<String>,
    ) -> Result<(), MappedError> {
        self.require_cell(cell)?;
        self.operations.push(RegionOperation::RenameCell {
            cell,
            name: name.into(),
        });
        Ok(())
    }

    /// Plans a live net rename or removal of its optional name.
    ///
    /// # Errors
    ///
    /// Returns [`MappedError`] when `net` is outside the snapshot footprint.
    pub fn rename_net(&mut self, net: NetId, name: Option<String>) -> Result<(), MappedError> {
        self.require_net(net)?;
        self.operations
            .push(RegionOperation::RenameNet { net, name });
        Ok(())
    }

    fn require_cell(&self, cell: CellId) -> Result<(), MappedError> {
        self.snapshot
            .contains_cell(cell)
            .then_some(())
            .ok_or_else(|| {
                MappedError::invariant(format!(
                    "region delta writes cell {cell:?} outside its snapshot"
                ))
            })
    }

    fn require_net(&self, net: NetId) -> Result<(), MappedError> {
        self.snapshot
            .contains_net(net)
            .then_some(())
            .ok_or_else(|| {
                MappedError::invariant(format!(
                    "region delta writes net {net:?} outside its snapshot"
                ))
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Optimistic region edit rejected because its snapshot is stale or invalid.
pub enum RegionConflict {
    /// A snapshotted cell changed after analysis.
    #[error("mapped region conflicts on cell {0:?}")]
    StaleCell(CellId),
    /// A snapshotted net changed after analysis.
    #[error("mapped region conflicts on net {0:?}")]
    StaleNet(NetId),
    /// The delta violates a mapped-netlist invariant.
    #[error(transparent)]
    Invalid(#[from] MappedError),
}

impl RegionConflict {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(MappedError::invariant(message))
    }
}

#[derive(Debug)]
/// Bounded undo record for a successfully applied [`RegionDelta`].
///
/// The record owns only touched slots, connections, adjacency links, and the
/// name-table checkpoint. It must be either committed by dropping it or passed
/// to [`MappedNetlist::rollback_region_delta`].
pub struct AppliedRegionDelta {
    generation: MappedGenerationId,
    previous_revision: u64,
    committed_revision: u64,
    previous_net_count: usize,
    previous_cell_count: usize,
    old_net_len: usize,
    old_cell_len: usize,
    old_connection_len: usize,
    old_nets: BTreeMap<NetId, NetSlot>,
    old_cells: BTreeMap<CellId, CellSlot>,
    old_connections: BTreeMap<PinId, PinConnection>,
    old_net_pins: BTreeMap<NetId, NetPins>,
    old_pin_links: BTreeMap<PinId, PinLinks>,
    names: NameCheckpoint,
    added_nets: BTreeMap<TempNetId, NetId>,
    added_cells: BTreeMap<TempCellId, CellId>,
}

impl AppliedRegionDelta {
    /// Returns the mapped-netlist owner to which this undo record belongs.
    #[must_use]
    pub fn generation_id(&self) -> MappedGenerationId {
        self.generation
    }

    #[must_use]
    /// Resolves a delta-local net ID to its stable netlist ID.
    pub fn added_net(&self, id: TempNetId) -> Option<NetId> {
        self.added_nets.get(&id).copied()
    }

    #[must_use]
    /// Resolves a delta-local cell ID to its stable netlist ID.
    pub fn added_cell(&self, id: TempCellId) -> Option<CellId> {
        self.added_cells.get(&id).copied()
    }

    /// Iterates all added cell mappings in temporary-ID order.
    pub fn added_cells(&self) -> impl Iterator<Item = (TempCellId, CellId)> + '_ {
        self.added_cells
            .iter()
            .map(|(&temporary, &cell)| (temporary, cell))
    }

    /// Iterates live cell payloads as they existed before application.
    pub fn previous_live_cells(&self) -> impl Iterator<Item = (CellId, &MappedCell)> + '_ {
        self.old_cells
            .iter()
            .filter(|(_, slot)| slot.live)
            .map(|(&id, slot)| (id, &slot.cell))
    }

    /// Iterates cells whose identity or payload may have changed.
    pub fn affected_cells(&self) -> impl Iterator<Item = CellId> + '_ {
        self.old_cells
            .keys()
            .copied()
            .chain(self.added_cells.values().copied())
    }

    /// Iterates nets whose identity, name, or adjacency may have changed.
    pub fn affected_nets(&self) -> impl Iterator<Item = NetId> + '_ {
        self.old_nets
            .keys()
            .copied()
            .chain(self.added_nets.values().copied())
    }
}

#[cfg(test)]
mod tests;
