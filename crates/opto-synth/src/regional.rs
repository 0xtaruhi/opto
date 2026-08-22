// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Region identities, contracts, portable plans, and deterministic epochs.

pub(crate) mod boundary;
mod contract;
mod epoch;
mod plan;
pub(crate) mod region_graph;
mod work;
pub use boundary::{
    BoundaryCheckKind, BoundaryContract, BoundaryContractError, BoundaryContractRow,
    BoundaryInputContract, BoundaryOutputContract, ContractGeneration, EarlyLate, FiniteValue,
    RegionContextKey, RiseFall, TimingCorners, TimingTag, TimingTagId, TimingTagInterner,
};
use boundary::{check_value_lane, input_transition_lane, path_timing_lane};
pub use region_graph::{
    BoundaryPortId, BoundaryValueRevision, OperationAnchorId, RegionAnchorId, RegionBoundaryPort,
    RegionBoundaryPortId, RegionPortDirection, RegionRevision, RegionRowId, SynthesisRegion,
    SynthesisRegionGraph, SynthesisRegionKind, SynthesisRegionRevision,
};

pub(crate) use contract::RegionContractSet;
pub(crate) use epoch::{EpochDecision, RegionalEpochCoordinator};
pub use plan::{BoundaryResponse, BoundaryResponseRow, RegionCoverPlan, RegionPlanCost};
pub(crate) use plan::{RegionCoverPlanRecord, RegionImplementationCell, RegionalSharedAllocations};
pub(crate) use work::{
    SynthesisExecutor, WorkContext, WorkDesign, WorkGraph, WorkProduct, logical_memory_cell_id,
    logical_operation_cell_id,
};
