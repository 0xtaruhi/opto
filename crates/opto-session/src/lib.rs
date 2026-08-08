// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    clippy::missing_errors_doc,
    reason = "public session operations share the documented SessionError model and transactional failure semantics"
)]
#![allow(
    clippy::fn_params_excessive_bools,
    clippy::struct_excessive_bools,
    reason = "typed use-case options preserve independent DC command switches without lossy flag packing"
)]

//! Stateful synthesis and design-analysis session.
//!
//! [`Session`] is the product-level design database. It owns analyzed HDL,
//! linked definitions, constraints, selected libraries, parasitics, synthesized
//! artifacts, and analysis-engine caches. Public use cases implement the
//! semantic operations behind Tcl commands; the shell itself remains a thin
//! adapter.
//!
//! Changes are revisioned and transactional. A failed command cannot publish a
//! partially updated object registry, collection, timing context, or synthesis
//! result. Cache keys include the exact design, library, constraint, and
//! parasitic generations they depend on, so stale analysis is never rebound by
//! name after an edit.

pub use opto_db::{AnyObjectId, ObjectClass};
pub use opto_db::{ClockId, NetId, PortId};
pub use opto_formats::{PowerReportKind, ReportPowerOptions};
pub use opto_hdl::{FrontendOptions, VerilogLanguage};
pub use opto_ir::word::SynthesisDirectiveKind;
pub use opto_power::{PowerEngineMetrics, SwitchingActivity};
pub use opto_synth::{
    OptimizationPhase, SourceChangeMetrics, StageId, SynthesisEffort, SynthesisMetrics,
    SynthesisProgressStatus,
};
pub use opto_timing::{
    CaseAnalysisValue, ClockGroupKind, ClockSpec, ConstraintChange, CornerSelection, DelayType,
    DesignRuleScope, DisabledTiming, EdgeQualifier, EdgeSelection, ExceptionCorner,
    ExceptionFilter, GeneratedClock, IoDelayKind, IoDelaySpec, LatencySide, ParasiticDelayModel,
    PathException, PathExceptionKind, ReportTimingOptions, TimingDerateKind, TimingEdge,
    TimingEndpoint, TimingObject,
};

#[cfg(test)]
use opto_db::{ObjectLocator, RevisionId};
#[cfg(test)]
use opto_hdl::DbUpdate;
#[cfg(test)]
use opto_runtime::ExecutionContext;
#[cfg(test)]
use std::path::PathBuf;

mod database;
mod design_graph;
mod design_view;
mod error;
mod handles;
mod libraries;
mod object_index;
mod objects;
mod parasitics_state;
mod power;
mod state;
mod synthesis;
mod timing;
mod transaction;
mod use_case;

use design_view::{DesignView, MappedObjectIndex};
pub use error::SessionError;
use handles::ObjectHandleCodec;
pub use handles::{CollectionFilter, FilterOperator};
use object_index::build_object_index;
pub use opto_synth::{SynthesisConfig, SynthesisDiagnostics};
use parasitics_state::ParasiticsState;
pub use power::SwitchingActivityUpdate;
use state::{
    ArtifactBinding, DefinitionGraphCache, DefinitionGraphCacheKey, DesignRecord, DesignStore,
    SynthesisKey, TimingDesignGeneration, TimingModelCache, TimingModelKey,
};
pub use state::{ConstraintCheckpoint, Session, SessionConfig};
#[cfg(test)]
use use_case::CurrentDesignPolicy;
pub use use_case::{
    HdlCatalog, ReadParasiticsCompletion, ReadParasiticsOptions, SynthesisEvent, SynthesisTrace,
    SynthesisTraceSink,
};

#[cfg(test)]
mod tests;
