// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Target-aware synthesis from linked RTL to a mapped netlist.
//!
//! [`SynthesisEngine`] runs the production pipeline: process lowering,
//! word-level optimization, memory and operator planning, Boolean rewriting,
//! technology mapping, sequential mapping, and post-map optimization.
//! [`SynthesisRequest`] supplies sealed source, target-library, runtime, and
//! constraint views; [`SynthesisResult`] owns the published mapped artifact and
//! its provenance. Regional reuse is carried by an artifact-owned
//! [`IncrementalSnapshot`] explicitly borrowed by the next request; it is not
//! mutable process state in [`SynthesisEngine`].
//!
//! The production implementation freezes global Word semantics into a stable
//! [`SynthesisRegionGraph`], preserves region boundaries while building one
//! canonical Boolean subject, analyzes and maps region-owned cuts in parallel,
//! and commits compact [`RegionCoverPlan`]s in stable region order. Explicit
//! sparse timing scenarios drive immutable boundary contracts, bounded closure
//! epochs, and transactional max/min MMMC post-map repair.
//! [`ImplementationDb`] records which source operators each mapped cell
//! implements and which stable source synthesis regions own that cell, so
//! incremental compilation, repair, and reports do not reconstruct provenance
//! from names.

mod api;
mod artifact;
mod boolean;
mod closure;
mod engine;
mod frontend;
mod incremental;
mod mapping;
mod planning;
mod regional;
mod word;

#[cfg(test)]
pub(crate) use api::target_cell_reference_ports;
pub use api::{
    CheckDesignError, CombinationalCycle, CombinationalCycleNode, OptimizationPhase, ReferencePort,
    ReferencePortMap, StageId, SynthError, SynthesisConfig, SynthesisDiagnostics, SynthesisEffort,
    SynthesisMetrics, SynthesisOptions, SynthesisProgress, SynthesisProgressStatus,
    SynthesisReport, SynthesisResult, TimingSummary, check_design_with_references,
};
pub use artifact::{
    BoundaryEdgeId, ImplementationDb, ImplementationRegion, ImplementationRegionId,
    MappedCellOwnership,
};
pub use closure::{NoPowerEvaluation, SynthesisPowerEvaluator};
#[cfg(test)]
pub(crate) use engine::synthesize_rtl_module;
pub use engine::{SynthesisEngine, SynthesisRequest};
pub use incremental::{
    IncrementalSnapshot, InterfaceFingerprint, SourceChangeMetrics, SourceFingerprint,
    SourceSnapshot,
};
pub use mapping::ClockGatingStyle;
pub use mapping::library::target_cell_is_buffer_or_inverter;
#[cfg(test)]
pub(crate) use opto_library::{
    BooleanFunction, TargetCell, TargetPin, TargetSequential, TargetTimingArc, TargetTimingType,
};
use opto_library::{
    BooleanFunctionRef, TargetCellRef, TargetNextStateType, TargetPinDirection, TargetPinRef,
    TargetSequentialKind, TargetSequentialRef, TargetTimingArcRef,
};
pub use planning::{
    DurableOperatorArena, DynamicExtractShape, ImplementationCandidate, ImplementationCandidateId,
    OperatorId, OperatorKind, OperatorManifest, OperatorManifestInstance, OperatorShape,
    OperatorSignature, OperatorSignatureId, OperatorTermShape, PreservedOperatorInstance,
    SemanticOperator,
};
pub use regional::{
    BoundaryCheckKind, BoundaryContract, BoundaryContractError, BoundaryContractRow,
    BoundaryInputContract, BoundaryOutputContract, BoundaryPortId, BoundaryResponse,
    BoundaryResponseRow, BoundaryValueRevision, ContractGeneration, EarlyLate, FiniteValue,
    OperationAnchorId, RegionAnchorId, RegionBoundaryPort, RegionBoundaryPortId, RegionContextKey,
    RegionCoverPlan, RegionPlanCost, RegionPlanIdentity, RegionPlanSize, RegionPortDirection,
    RegionRevision, RegionRowId, RiseFall, SynthesisRegion, SynthesisRegionGraph,
    SynthesisRegionKind, SynthesisRegionRevision, TimingTag, TimingTagId, TimingTagInterner,
};

#[cfg(test)]
mod tests;

#[cfg(test)]
fn test_runtime() -> &'static opto_runtime::ExecutionContext {
    static RUNTIME: std::sync::OnceLock<opto_runtime::ExecutionContext> =
        std::sync::OnceLock::new();
    RUNTIME.get_or_init(opto_runtime::ExecutionContext::default)
}
