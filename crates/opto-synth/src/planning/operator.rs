// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod catalog;
mod decisions;
mod demand;
mod durable;
mod sharing;

use crate::{OperatorId, OperatorKind, SemanticOperator};

pub use catalog::{ImplementationCandidate, ImplementationCandidateId};
pub(crate) use decisions::ArchitectureDecisions;
pub use durable::{
    DurableOperatorArena, DynamicExtractShape, OperatorManifest, OperatorManifestInstance,
    OperatorShape, OperatorSignature, OperatorSignatureId, OperatorTermShape,
    PreservedOperatorInstance,
};
pub(crate) use sharing::share_muxed_arithmetic;
