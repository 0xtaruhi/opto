// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::artifact::implementation::ImplementationDelta;
use opto_ir::mapped::{CellId, RegionDelta, TempCellId};

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
        fragment: crate::FragmentFootprint,
    ) -> Result<Self, crate::SynthError> {
        self.implementation
            .record_added_cell(added, semantic_sources, fragment)?;
        Ok(self)
    }

    /// Records one fanout/cloning segment in its exact immutable fragment.
    pub(super) fn record_repair_segment(
        self,
        implementations: &crate::ImplementationDb,
        added: TempCellId,
        drivers: &[CellId],
        sink: CellId,
    ) -> Result<Self, crate::SynthError> {
        let fragment = implementations.repair_fragment(drivers, sink)?;
        self.record_added_cell(added, drivers.iter().copied(), fragment)
    }
}
