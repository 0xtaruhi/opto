// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Region-aware architecture selection and local Word construction.

mod decision;
mod envelope;
mod lowering;
mod private;
mod search;

pub(crate) use decision::{
    MemoryImplementationCandidate, RegionalDecisionPlan, RegionalDecisionVector,
};
pub(crate) use envelope::{RegionCostEnvelopeSet, StructuralTargetModel};
pub(crate) use lowering::{
    RegionalMemoryValueBinding, RegionalMemoryValueKind, RegionalWordCone, RegionalWordConeParts,
    RegionalWordConeRequest,
};
pub(crate) use private::optimize_structure as optimize_private_structure;
pub(crate) use search::{RegionalArchitectureSearch, RegionalSearchRequest};
