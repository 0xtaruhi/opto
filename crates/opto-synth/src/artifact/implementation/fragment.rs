// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Immutable mapped-artifact containment.

use crate::RegionAnchorId;
use opto_ir::mapped::CellId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Dense identity of one mapped fragment footprint.
pub struct MappedFragmentId(u32);

impl MappedFragmentId {
    /// Returns the database-local fragment row.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    pub(super) fn from_index(index: usize) -> Result<Self, crate::SynthError> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| crate::SynthError::capacity("mapped fragment count"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Exact containment scope of one immutable mapped artifact.
pub enum FragmentFootprint {
    /// Static retained or linked implementation substrate.
    Global,
    /// Artifact contained by one semantic synthesis region.
    Region(RegionAnchorId),
    /// Artifact implementing one exact driver-to-sink boundary segment.
    Boundary {
        /// Region driving the segment.
        driver: RegionAnchorId,
        /// Region receiving the segment.
        sink: RegionAnchorId,
    },
}

impl FragmentFootprint {
    pub(super) const fn endpoint(self) -> Option<RegionAnchorId> {
        match self {
            Self::Global => None,
            Self::Region(region) => Some(region),
            Self::Boundary { sink, .. } => Some(sink),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct FragmentImpact {
    regions: BTreeSet<RegionAnchorId>,
    nonregional_changed: bool,
    unknown_cells: BTreeSet<CellId>,
}

impl FragmentImpact {
    pub(crate) fn regions(&self) -> &BTreeSet<RegionAnchorId> {
        &self.regions
    }

    pub(crate) fn unknown_cells(&self) -> &BTreeSet<CellId> {
        &self.unknown_cells
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.regions.is_empty() && !self.nonregional_changed && self.unknown_cells.is_empty()
    }

    pub(super) fn record(&mut self, cell: CellId, fragment: Option<FragmentFootprint>) {
        match fragment {
            Some(FragmentFootprint::Region(region)) => {
                self.regions.insert(region);
            }
            Some(FragmentFootprint::Global | FragmentFootprint::Boundary { .. }) => {
                self.nonregional_changed = true;
            }
            None => {
                self.unknown_cells.insert(cell);
            }
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.regions.extend(other.regions);
        self.nonregional_changed |= other.nonregional_changed;
        self.unknown_cells.extend(other.unknown_cells);
    }
}

pub(super) type SealedFragments = (
    Vec<Option<MappedFragmentId>>,
    Vec<FragmentFootprint>,
    BTreeMap<FragmentFootprint, MappedFragmentId>,
    Vec<Vec<CellId>>,
);

pub(super) fn seal_fragments(
    cells: Vec<Option<FragmentFootprint>>,
) -> Result<SealedFragments, crate::SynthError> {
    let mut unique = cells.iter().flatten().copied().collect::<BTreeSet<_>>();
    unique.insert(FragmentFootprint::Global);
    let fragments = unique.into_iter().collect::<Vec<_>>();
    let ids = fragments
        .iter()
        .copied()
        .enumerate()
        .map(|(index, fragment)| Ok((fragment, MappedFragmentId::from_index(index)?)))
        .collect::<Result<BTreeMap<_, _>, crate::SynthError>>()?;
    let cells = cells
        .into_iter()
        .map(|fragment| fragment.map(|fragment| ids[&fragment]))
        .collect::<Vec<_>>();
    let mut fragment_cells = vec![Vec::new(); fragments.len()];
    for (index, fragment) in cells.iter().copied().enumerate() {
        if let Some(fragment) = fragment {
            fragment_cells[fragment.raw() as usize]
                .push(CellId::from_index(index).map_err(crate::SynthError::Mapped)?);
        }
    }
    Ok((cells, fragments, ids, fragment_cells))
}
