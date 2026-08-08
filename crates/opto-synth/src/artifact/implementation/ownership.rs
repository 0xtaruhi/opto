// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::RegionAnchorId;
use opto_ir::mapped::CellId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub(super) struct RegionOwnerId(pub(super) u32);

impl RegionOwnerId {
    pub(super) const GLOBAL: Self = Self(0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Dense identity of one driver-to-sink synthesis-region boundary.
pub struct BoundaryEdgeId(pub(super) u32);

impl BoundaryEdgeId {
    /// Return the database-local edge row.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(super) struct BoundaryEdge {
    pub(super) driver: RegionAnchorId,
    pub(super) sink: RegionAnchorId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub(super) struct MappedOwnerId(pub(super) u32);

impl MappedOwnerId {
    pub(super) const BOUNDARY_TAG: u32 = 1 << 31;

    pub(super) fn region(id: RegionOwnerId) -> Result<Self, crate::SynthError> {
        if id.0 >= Self::BOUNDARY_TAG {
            return Err(crate::SynthError::capacity("mapped region-owner count"));
        }
        Ok(Self(id.0))
    }

    pub(super) fn boundary(id: BoundaryEdgeId) -> Result<Self, crate::SynthError> {
        if id.0 >= Self::BOUNDARY_TAG {
            return Err(crate::SynthError::capacity("mapped boundary-edge count"));
        }
        Ok(Self(Self::BOUNDARY_TAG | id.0))
    }

    pub(super) const fn region_id(self) -> Option<RegionOwnerId> {
        if self.0 & Self::BOUNDARY_TAG == 0 {
            Some(RegionOwnerId(self.0))
        } else {
            None
        }
    }

    pub(super) const fn boundary_id(self) -> Option<BoundaryEdgeId> {
        if self.0 & Self::BOUNDARY_TAG != 0 {
            Some(BoundaryEdgeId(self.0 & !Self::BOUNDARY_TAG))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Explicit synthesis-region ownership of one mapped cell.
///
/// Every live mapped cell has exactly one owner atom: one synthesis region,
/// one driver-to-sink boundary edge, or the static global substrate.
pub enum MappedCellOwnership {
    /// One semantic synthesis region owns the cell.
    Region(RegionAnchorId),
    /// A post-map artifact owned by one exact driver-to-sink region edge.
    Boundary {
        /// Dense identity of the interned edge.
        edge: BoundaryEdgeId,
        /// Region driving the boundary segment.
        driver: RegionAnchorId,
        /// Single region receiving the boundary segment.
        sink: RegionAnchorId,
    },
    /// A retained/link instance or other static non-regional mapped object.
    Global,
    /// A stable mapped slot whose cell has been removed.
    Removed,
    /// The cell is outside the implementation ownership slot domain.
    Unknown,
}

#[derive(Debug, Default)]
pub(crate) struct MappedOwnerImpact {
    regions: BTreeSet<RegionAnchorId>,
    boundary_edges: BTreeSet<BoundaryEdge>,
    global_changed: bool,
    unknown_cells: BTreeSet<CellId>,
}

impl MappedOwnerImpact {
    pub(crate) fn regions(&self) -> &BTreeSet<RegionAnchorId> {
        &self.regions
    }

    pub(crate) fn unknown_cells(&self) -> &BTreeSet<CellId> {
        &self.unknown_cells
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.regions.is_empty()
            && self.boundary_edges.is_empty()
            && !self.global_changed
            && self.unknown_cells.is_empty()
    }

    pub(super) fn record(&mut self, cell: CellId, ownership: MappedCellOwnership) {
        match ownership {
            MappedCellOwnership::Region(region) => {
                self.regions.insert(region);
            }
            MappedCellOwnership::Boundary {
                edge: _,
                driver,
                sink,
            } => {
                self.boundary_edges.insert(BoundaryEdge { driver, sink });
            }
            MappedCellOwnership::Global => {
                self.global_changed = true;
            }
            MappedCellOwnership::Removed | MappedCellOwnership::Unknown => {
                self.unknown_cells.insert(cell);
            }
        }
    }

    pub(super) fn record_boundary(&mut self, driver: RegionAnchorId, sink: RegionAnchorId) {
        self.boundary_edges.insert(BoundaryEdge { driver, sink });
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.regions.extend(other.regions);
        self.boundary_edges.extend(other.boundary_edges);
        self.global_changed |= other.global_changed;
        self.unknown_cells.extend(other.unknown_cells);
    }
}

type SealedOwners = (
    Vec<Option<MappedOwnerId>>,
    Vec<RegionAnchorId>,
    BTreeMap<RegionAnchorId, RegionOwnerId>,
    Vec<BoundaryEdge>,
    Vec<Vec<CellId>>,
    BTreeMap<BoundaryEdge, BoundaryEdgeId>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitialCellOwner {
    Global,
    Region(RegionAnchorId),
    Boundary {
        driver: RegionAnchorId,
        sink: RegionAnchorId,
    },
}

pub(super) fn seal_owners(
    owners: Vec<Option<InitialCellOwner>>,
) -> Result<SealedOwners, crate::SynthError> {
    let mut regions = Vec::new();
    let mut ids = BTreeMap::new();
    let mut boundary_edges = Vec::new();
    let mut boundary_edge_cells = Vec::<Vec<CellId>>::new();
    let mut boundary_edge_ids = BTreeMap::new();
    let cells = owners
        .into_iter()
        .enumerate()
        .map(|(index, owner)| {
            owner
                .map(|owner| {
                    let region = match owner {
                        InitialCellOwner::Global => {
                            return MappedOwnerId::region(RegionOwnerId::GLOBAL);
                        }
                        InitialCellOwner::Region(region) => region,
                        InitialCellOwner::Boundary { driver, sink } => {
                            if driver == sink {
                                return Err(crate::SynthError::invariant(
                                    "initial boundary owner has identical endpoints",
                                ));
                            }
                            let edge = BoundaryEdge { driver, sink };
                            let id = if let Some(&id) = boundary_edge_ids.get(&edge) {
                                id
                            } else {
                                let id = BoundaryEdgeId(
                                    u32::try_from(boundary_edges.len()).map_err(|_| {
                                        crate::SynthError::capacity("mapped boundary-edge count")
                                    })?,
                                );
                                boundary_edges.push(edge);
                                boundary_edge_cells.push(Vec::new());
                                boundary_edge_ids.insert(edge, id);
                                id
                            };
                            boundary_edge_cells[id.0 as usize].push(
                                CellId::from_index(index).map_err(crate::SynthError::Mapped)?,
                            );
                            return MappedOwnerId::boundary(id);
                        }
                    };
                    let id = if let Some(&id) = ids.get(&region) {
                        id
                    } else {
                        let id = RegionOwnerId(u32::try_from(regions.len() + 1).map_err(|_| {
                            crate::SynthError::capacity("mapped region-owner count")
                        })?);
                        regions.push(region);
                        ids.insert(region, id);
                        id
                    };
                    MappedOwnerId::region(id)
                })
                .transpose()
        })
        .collect::<Result<_, crate::SynthError>>()?;
    Ok((
        cells,
        regions,
        ids,
        boundary_edges,
        boundary_edge_cells,
        boundary_edge_ids,
    ))
}
