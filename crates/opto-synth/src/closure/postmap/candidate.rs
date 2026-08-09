// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::artifact::implementation::ImplementationDelta;
use opto_ir::mapped::{CellId, RegionDelta, TempCellId};
use std::collections::HashSet;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct PostmapCandidate {
    pub(super) delta: RegionDelta,
    pub(super) implementation: ImplementationDelta,
    pub(super) guard: Vec<CellId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CandidateDisposition<T> {
    Accepted(T),
    Rejected,
    Stale,
}

pub(super) struct CandidateBatch {
    pub(super) selected: Vec<(CellId, PostmapCandidate)>,
    pub(super) deferred: Vec<CellId>,
}

pub(super) fn select_non_conflicting(
    candidates: impl IntoIterator<Item = (CellId, Option<PostmapCandidate>)>,
) -> CandidateBatch {
    let mut selected = Vec::new();
    let mut deferred = Vec::new();
    let mut reserved_cells = HashSet::new();
    let mut reserved_nets = HashSet::new();
    for (key, candidate) in candidates {
        let Some(candidate) = candidate else {
            continue;
        };
        let snapshot = candidate.delta.snapshot();
        let cell_conflict = candidate
            .guard
            .iter()
            .copied()
            .chain(snapshot.cell_ids())
            .any(|cell| reserved_cells.contains(&cell));
        let net_conflict = snapshot.net_ids().any(|net| reserved_nets.contains(&net));
        if cell_conflict || net_conflict {
            deferred.push(key);
            continue;
        }
        reserved_cells.extend(candidate.guard.iter().copied());
        reserved_cells.extend(snapshot.cell_ids());
        reserved_nets.extend(snapshot.net_ids());
        selected.push((key, candidate));
    }
    CandidateBatch { selected, deferred }
}

impl PostmapCandidate {
    pub(super) fn new(delta: RegionDelta) -> Self {
        Self {
            delta,
            implementation: ImplementationDelta::default(),
            guard: Vec::new(),
        }
    }

    pub(super) fn with_guard(mut self, guard: Vec<CellId>) -> Self {
        self.guard = guard;
        self
    }

    pub(super) fn record_added_cell(
        mut self,
        added: TempCellId,
        semantic_sources: impl IntoIterator<Item = CellId>,
        ownership_sources: impl IntoIterator<Item = CellId>,
    ) -> Result<Self, crate::SynthError> {
        self.implementation
            .record_added_cell(added, semantic_sources, ownership_sources)?;
        Ok(self)
    }

    pub(super) fn record_boundary_cell(
        mut self,
        added: TempCellId,
        semantic_sources: impl IntoIterator<Item = CellId>,
        driver_sources: impl IntoIterator<Item = CellId>,
        sink: CellId,
    ) -> Result<Self, crate::SynthError> {
        self.implementation
            .record_boundary_cell(added, semantic_sources, driver_sources, sink)?;
        Ok(self)
    }

    /// Records one fanout/cloning segment without allowing the boundary path
    /// to degrade into an implicit global or multi-owner bucket.
    pub(super) fn record_repair_segment(
        self,
        implementations: &crate::ImplementationDb,
        added: TempCellId,
        drivers: &[CellId],
        sink: CellId,
    ) -> Result<Self, crate::SynthError> {
        let mut driver_endpoint = None;
        for &driver in drivers {
            let endpoint = implementations.ownership_endpoint(driver)?;
            if driver_endpoint
                .replace(endpoint)
                .is_some_and(|previous| previous != endpoint)
            {
                return Err(crate::SynthError::invariant(
                    "repair segment has multiple driver ownership endpoints",
                ));
            }
        }
        let driver_endpoint = driver_endpoint.flatten();
        let sink_endpoint = implementations.ownership_endpoint(sink)?;
        if let (Some(driver), Some(sink_endpoint)) = (driver_endpoint, sink_endpoint)
            && driver != sink_endpoint
        {
            return self.record_boundary_cell(
                added,
                drivers.iter().copied(),
                drivers.iter().copied(),
                sink,
            );
        }
        let ownership = if driver_endpoint.is_some() && !drivers.is_empty() {
            drivers.to_vec()
        } else {
            vec![sink]
        };
        self.record_added_cell(added, drivers.iter().copied(), ownership)
    }
}
