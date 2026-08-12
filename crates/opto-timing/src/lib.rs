// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Constraint-aware static timing analysis and incremental timing updates.
//!
//! A [`TimingEngine`] binds a mapped netlist to a typed timing-library model,
//! propagates arrival and required times, and returns immutable
//! [`TimingAnalysis`] or compact [`TimingQuality`] summaries. The constraint
//! context models clocks, I/O delays, path exceptions, design rules, and
//! operating conditions with permanent database identities rather than names.
//!
//! [`IncrementalTiming`] applies a prepared [`RegionEdit`] to the affected
//! timing cone and can roll it back during speculative optimization. Publication
//! validates model generations, endpoint shape, and mapped identities so an
//! analysis result cannot be reused after an incompatible structural edit.

#![allow(
    clippy::wildcard_imports,
    reason = "private timing submodules deliberately import their parent's internal prelude; \
              spelling out those implementation-only imports duplicates the module boundary and \
              makes coordinated propagation changes harder to review"
)]
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        clippy::too_many_lines,
        reason = "timing tests intentionally assert bit-stable deterministic values, and each long \
                  integration test keeps one complete constraint or incremental transaction visible"
    )
)]

use opto_core::RevisionId;
pub use opto_db::{CellId, ClockId, DesignId, NetId, PinId, PortId};
use opto_ir::mapped::NetId as MappedNetId;
pub use opto_library::{
    ArcDelayModel, CcsTimingModel, EcsmPinReceiverCapacitanceModel, EcsmTimingModel, LookupTable,
    NldmTimingModel, PinReceiverCapacitanceModel, ReceiverCapacitanceModel, SampledWaveform,
    SampledWaveformGrid, TargetPinDirection, TimingCheckKind, TimingEdge, TimingLibrary,
    TimingLibraryUnits, TimingModelKind, TimingSense, TimingThresholds, TimingTopologySchema,
    WireLoadModel,
};
#[cfg(test)]
use opto_library::{TargetCell, TargetCellSet, TargetPin, TargetSequential, TargetTimingArc};
use opto_library::{
    TargetCellRef, TargetPinRef, TargetSequentialKind, TargetTimingArcRef, TargetTimingType,
};
#[cfg(test)]
use std::collections::BTreeMap;
mod analysis;
mod bindings;
mod constraints;
mod engine;
mod error;
mod model;
mod parasitics;
mod result;
mod scenario;

pub use bindings::PortBindings;
pub use constraints::*;
pub use engine::{
    IncrementalTiming, IncrementalTimingMemory, RegionEdit, TimingEngine, TimingEngineMetrics,
};
pub use error::{
    ConstraintError, TimingAnalysisError, TimingEngineError, TimingError, TimingModelError,
};
pub(crate) use model::InstanceRegionModelEdit;
pub use model::*;
pub use parasitics::*;
pub(crate) use result::{Arrival, LaunchClock};
pub use result::{
    CellTimingEstimate, CheckTimingAnalysis, ClockReportRow, DelayType,
    InterconnectPathContribution, NetTimingState, PathStep, PathStepKind, PinTimingState,
    ReportTimingOptions, TimingAnalysis, TimingElectricalSnapshot, TimingElectricalState,
    TimingLibraryMetadata, TimingNetStates, TimingPathException, TimingQuality,
    TimingQualitySummary, TimingRequirement,
};
pub use scenario::{
    AnalysisViewId, Scenario, ScenarioActivityTarget, ScenarioCheckSet, ScenarioGeneration,
    ScenarioId, ScenarioPowerView, ScenarioSet, ScenarioSetError, ScenarioSwitchingActivity,
    ScenarioTimingViews,
};

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::{
    assert_path_summary, test_analyze_timing, test_clock_id, test_design_id, test_library,
    test_library_units, test_object_uid, test_port, test_port_id, test_timing_model,
};
#[cfg(test)]
mod tests;
