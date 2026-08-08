// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable Word-level synthesis regions and their explicit typed boundaries.

mod graph;
pub(crate) mod partition;

pub use graph::{
    BoundaryPortId, BoundaryValueRevision, OperationAnchorId, RegionAnchorId, RegionBoundaryPort,
    RegionBoundaryPortId, RegionPortDirection, RegionRevision, RegionRowId, SynthesisRegion,
    SynthesisRegionGraph, SynthesisRegionKind, SynthesisRegionRevision,
};
pub(crate) use partition::RegionPartitionPolicy;

#[cfg(test)]
mod tests;
