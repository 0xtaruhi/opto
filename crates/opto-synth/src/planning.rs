// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Word-level canonicalization and architecture planning.
//!
//! These passes preserve source-level intent while selecting implementation
//! recipes for memories, arithmetic, control, and sequential structures. They
//! produce decisions consumed by Boolean lowering and mapping.

pub(crate) mod architecture;
pub(crate) mod dataflow;
pub(crate) mod fsm;
pub(crate) mod mapping_policy;
pub(crate) mod memory;
pub(crate) mod operator;
pub(crate) mod provider;
pub(crate) mod regional;

pub use architecture::{OperatorId, OperatorKind, SemanticOperator};
pub use operator::{
    DurableOperatorArena, DynamicExtractShape, OperatorManifest, OperatorManifestInstance,
    OperatorShape, OperatorSignature, OperatorSignatureId, OperatorTermShape,
    PreservedOperatorInstance,
};
pub use operator::{ImplementationCandidate, ImplementationCandidateId};
