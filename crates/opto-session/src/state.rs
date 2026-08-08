// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{DesignView, HdlCatalog, MappedObjectIndex, ObjectHandleCodec, SessionError};
use opto_db::{
    AnyObjectId, DesignId, DesignIndex, ObjectRegistry, ObjectRegistryCheckpoint, ResolvedObject,
    RevisionId,
};
use opto_ir::rtl::RtlModule;
use opto_library::{LibraryFingerprint, LibraryStore};
use opto_runtime::ExecutionContext;
use opto_synth::{
    IncrementalSnapshot, SourceFingerprint, SynthesisConfig, SynthesisEffort, SynthesisReport,
    SynthesisResult,
};
use opto_timing::{
    ParasiticsFingerprint, TimingCheckpoint, TimingContext, TimingEngine, TimingFingerprint,
    TimingModel,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct DesignRecord {
    pub(crate) source: RtlModule,
    pub(crate) source_revision: RevisionId,
    pub(crate) synthesized: Option<SynthesisResult>,
    pub(crate) synthesis_binding: Option<ArtifactBinding>,
    pub(crate) incremental_snapshot: Option<IncrementalSnapshot>,
    /// Canonical source object inventory rebuilt from `source`.
    pub(crate) object_index: DesignIndex,
    /// Compact lookup/order sidecar selecting `synthesized.mapped()` as the
    /// active object backend. `None` selects the source index above.
    pub(crate) mapped_object_index: Option<MappedObjectIndex>,
}

#[derive(Debug, Clone, Copy)]
#[must_use]
pub(crate) struct PreparedSynthesisDetach(SynthesisDetachKind);

#[derive(Debug, Clone, Copy)]
enum SynthesisDetachKind {
    PreserveSnapshot,
    DetachArtifact,
}

impl DesignRecord {
    pub(crate) fn new(
        source: RtlModule,
        source_revision: RevisionId,
        object_index: DesignIndex,
    ) -> Self {
        Self {
            source,
            source_revision,
            synthesized: None,
            synthesis_binding: None,
            incremental_snapshot: None,
            object_index,
            mapped_object_index: None,
        }
    }

    pub(crate) fn incremental_snapshot(&self) -> Option<&IncrementalSnapshot> {
        self.synthesized
            .as_ref()
            .map(SynthesisResult::incremental_snapshot)
            .or(self.incremental_snapshot.as_ref())
    }

    pub(crate) fn prepare_synthesis_detach(&self) -> Result<PreparedSynthesisDetach, SessionError> {
        match (
            self.synthesized.as_ref(),
            self.synthesis_binding.as_ref(),
            self.incremental_snapshot.as_ref(),
        ) {
            (Some(_), Some(_), None) => {
                Ok(PreparedSynthesisDetach(SynthesisDetachKind::DetachArtifact))
            }
            (None, None, _) => Ok(PreparedSynthesisDetach(
                SynthesisDetachKind::PreserveSnapshot,
            )),
            _ => Err(SessionError::state(
                "cannot invalidate a partial synthesized design state",
            )),
        }
    }

    pub(crate) fn commit_synthesis_detach(&mut self, prepared: PreparedSynthesisDetach) {
        match prepared.0 {
            SynthesisDetachKind::PreserveSnapshot => {
                assert!(
                    self.synthesized.is_none() && self.synthesis_binding.is_none(),
                    "prepared empty synthesis state changed before commit"
                );
            }
            SynthesisDetachKind::DetachArtifact => {
                assert!(
                    self.synthesis_binding.is_some() && self.incremental_snapshot.is_none(),
                    "prepared synthesized synthesis state changed before commit"
                );
                let synthesis = self
                    .synthesized
                    .take()
                    .expect("prepared synthesized design owns its synthesis artifact");
                self.incremental_snapshot = Some(synthesis.into_incremental_snapshot());
                self.synthesis_binding = None;
                self.mapped_object_index = None;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SynthesisKey {
    pub(crate) source: SourceFingerprint,
    pub(crate) timing: TimingFingerprint,
    pub(crate) parasitics: ParasiticsFingerprint,
    pub(crate) resolution_providers: opto_library::LibraryFingerprint,
    pub(crate) mapping_library: opto_library::LibraryFingerprint,
    pub(crate) timing_library: opto_library::LibraryFingerprint,
    pub(crate) activity: Option<[u8; 32]>,
    pub(crate) synthesis_config: SynthesisConfig,
    pub(crate) effort: SynthesisEffort,
    pub(crate) clock_gating: Option<opto_synth::ClockGatingStyle>,
}

/// Durable binding between semantic artifact identity and the session
/// generation that published it. The publication revision is never part of
/// the content key and therefore cannot invalidate a semantic cache hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArtifactBinding {
    pub(crate) content_key: SynthesisKey,
    pub(crate) published_revision: RevisionId,
}

#[derive(Debug, Default)]
pub(crate) struct DesignStore {
    pub(crate) records: BTreeMap<String, DesignRecord>,
}

impl DesignStore {
    pub(crate) fn get(&self, name: &str) -> Option<&DesignRecord> {
        self.records.get(name)
    }

    pub(crate) fn get_mut(&mut self, name: &str) -> Option<&mut DesignRecord> {
        self.records.get_mut(name)
    }
    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.records.contains_key(name)
    }

    pub(crate) fn insert(&mut self, name: String, record: DesignRecord) -> Option<DesignRecord> {
        self.records.insert(name, record)
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.records.keys()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &DesignRecord)> {
        self.records.iter()
    }
}

/// State that defines the durable identity and behavior of a design session.
///
/// Checkpoints serialize this owner explicitly. Process-local handles, loaded
/// Liberty data, schedulers, and analysis caches deliberately live elsewhere.
#[derive(Debug)]
pub(crate) struct PersistentState {
    pub(crate) revision: RevisionId,
    pub(crate) designs: DesignStore,
    pub(crate) current_design: Option<String>,
    pub(crate) settings: DatabaseSettings,
    pub(crate) timing: TimingContext,
    pub(crate) last_synthesis: Option<SynthesisReport>,
    pub(crate) objects: ObjectRegistry,
    pub(crate) parasitics: crate::ParasiticsState,
    pub(crate) power: crate::power::PowerContext,
    pub(crate) hdl_catalog: HdlCatalog,
}

impl Default for PersistentState {
    fn default() -> Self {
        Self {
            revision: RevisionId::INITIAL,
            designs: DesignStore::default(),
            current_design: None,
            settings: DatabaseSettings::default(),
            timing: TimingContext::default(),
            last_synthesis: None,
            objects: ObjectRegistry::default(),
            parasitics: crate::ParasiticsState::default(),
            power: crate::power::PowerContext::default(),
            hdl_catalog: HdlCatalog::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DatabaseSettings {
    pub(crate) hdl_search_path: Vec<PathBuf>,
    pub(crate) lib_search_path: Vec<PathBuf>,
    pub(crate) synth_effort: SynthesisEffort,
    pub(crate) clock_gating: bool,
    pub(crate) clock_gating_style: opto_synth::ClockGatingStyle,
}

impl Default for DatabaseSettings {
    fn default() -> Self {
        Self {
            hdl_search_path: vec![PathBuf::from(".")],
            lib_search_path: vec![PathBuf::from(".")],
            synth_effort: SynthesisEffort::Medium,
            clock_gating: false,
            clock_gating_style: opto_synth::ClockGatingStyle::default(),
        }
    }
}

/// State whose lifetime is the current process rather than the checkpoint.
#[derive(Debug)]
pub(crate) struct ProcessState {
    pub(crate) handles: ObjectHandleCodec,
    pub(crate) libraries: LibraryStore,
    pub(crate) runtime: ExecutionContext,
    pub(crate) timing_model_cache: RefCell<Option<TimingModelCache>>,
    pub(crate) definition_graph_cache: RefCell<Option<DefinitionGraphCache>>,
    pub(crate) timing_engine: TimingEngine,
    pub(crate) power_engine: opto_power::PowerEngine,
    pub(crate) synthesis_config: opto_synth::SynthesisConfig,
    pub(crate) synthesis_engine: opto_synth::SynthesisEngine,
}

impl Default for ProcessState {
    fn default() -> Self {
        Self::with_runtime(
            ExecutionContext::default(),
            opto_synth::SynthesisConfig::default(),
        )
    }
}

impl ProcessState {
    fn with_runtime(
        runtime: ExecutionContext,
        synthesis_config: opto_synth::SynthesisConfig,
    ) -> Self {
        let timing_engine = TimingEngine::new(runtime.clone());
        let synthesis_engine = opto_synth::SynthesisEngine::with_config(synthesis_config);
        Self {
            handles: ObjectHandleCodec::default(),
            libraries: LibraryStore::default(),
            runtime,
            timing_model_cache: RefCell::default(),
            definition_graph_cache: RefCell::default(),
            timing_engine,
            power_engine: opto_power::PowerEngine::new(),
            synthesis_config,
            synthesis_engine,
        }
    }

    /// Drops every derived analysis generation in dependency order.
    ///
    /// Power owns an `Arc` to its timing model, so it must be cleared before
    /// the timing caches if a stale generation is to release all shared state.
    pub(crate) fn clear_analysis_caches(&self) {
        self.power_engine.clear();
        self.timing_engine.clear();
        drop(self.timing_model_cache.borrow_mut().take());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DefinitionGraphCacheKey {
    pub(crate) root: String,
    pub(crate) providers: LibraryFingerprint,
    pub(crate) designs: Vec<(String, RevisionId)>,
}

#[derive(Clone)]
pub(crate) struct DefinitionGraphCache {
    pub(crate) key: DefinitionGraphCacheKey,
    pub(crate) graph: Arc<crate::design_graph::LinkedHierarchy>,
}

impl std::fmt::Debug for DefinitionGraphCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefinitionGraphCache")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default)]
/// Construction controls for a new synthesis session.
pub struct SessionConfig {
    /// Maximum runtime workers, or `None` for the runtime default.
    pub max_threads: Option<usize>,
    /// Synthesis diagnostics and optimization controls.
    pub synthesis: opto_synth::SynthesisConfig,
}

#[derive(Debug)]
/// Stateful design, constraint, analysis, and synthesis session.
///
/// Permanent design objects live in monotonic registries. Object handles and
/// analysis models are derived process state and may be invalidated when the
/// session revision or active libraries change.
pub struct Session {
    checkpoint_owner: Arc<()>,
    constraint_transactions: ConstraintTransactions,
    pub(crate) state: PersistentState,
    pub(crate) process: ProcessState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimingModelKey {
    pub(crate) design_generation: TimingDesignGeneration,
    pub(crate) library: LibraryFingerprint,
    pub(crate) parasitics_revision: RevisionId,
    pub(crate) design: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimingDesignGeneration {
    Source {
        source_revision: RevisionId,
    },
    Artifact {
        source_revision: RevisionId,
        published_revision: RevisionId,
        effort: SynthesisEffort,
    },
}

#[derive(Debug)]
pub(crate) struct TimingModelCache {
    pub(crate) key: TimingModelKey,
    pub(crate) model: Arc<TimingModel>,
}

#[derive(Debug)]
#[must_use = "a constraint checkpoint must be committed or restored"]
/// Opaque nested transaction checkpoint for constraint-owned session state.
///
/// A checkpoint belongs to exactly one session. Committing keeps semantic
/// changes; restoring rolls back constraints, object registration, and caches.
pub struct ConstraintCheckpoint {
    owner: Arc<()>,
    transaction: u64,
    revision: RevisionId,
    current_design: Option<String>,
    objects: ObjectRegistryCheckpoint,
    timing: TimingCheckpoint,
}

#[derive(Debug, Default)]
struct ConstraintTransactions {
    next: u64,
    active: Vec<u64>,
}

impl ConstraintTransactions {
    fn begin(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.active.push(id);
        id
    }

    fn commit(&mut self, id: u64) {
        let position = self.position(id);
        self.active.truncate(position);
    }

    fn restore(&mut self, id: u64) {
        let position = self.position(id);
        self.active.truncate(position);
    }

    fn position(&self, id: u64) -> usize {
        self.active
            .iter()
            .position(|&active| active == id)
            .expect("validated domain checkpoint must have an active session transaction")
    }
}

impl Default for Session {
    fn default() -> Self {
        Self {
            checkpoint_owner: Arc::new(()),
            constraint_transactions: ConstraintTransactions::default(),
            state: PersistentState::default(),
            process: ProcessState::default(),
        }
    }
}

impl Session {
    /// Construct a session with default runtime and synthesis configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a session with an explicit maximum worker count.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime configuration cannot be constructed.
    pub fn with_parallelism(max_threads: usize) -> Result<Self, SessionError> {
        Self::with_config(SessionConfig {
            max_threads: Some(max_threads),
            ..SessionConfig::default()
        })
    }

    /// Construct a session from runtime, synthesis, and initial path controls.
    ///
    /// # Errors
    ///
    /// Returns an error when runtime initialization fails.
    pub fn with_config(config: SessionConfig) -> Result<Self, SessionError> {
        let mut execution = opto_runtime::ExecutionConfig::default();
        if let Some(max_threads) = config.max_threads {
            execution.max_threads = max_threads;
        }
        let state = PersistentState::default();
        Ok(Self {
            checkpoint_owner: Arc::new(()),
            constraint_transactions: ConstraintTransactions::default(),
            state,
            process: ProcessState::with_runtime(
                ExecutionContext::new(&execution)?,
                config.synthesis,
            ),
        })
    }

    /// Return the current monotonic semantic revision.
    pub fn revision(&self) -> RevisionId {
        self.state.revision
    }

    /// Starts a nested transaction for constraint-owned session state.
    pub fn constraint_checkpoint(&mut self) -> ConstraintCheckpoint {
        ConstraintCheckpoint {
            owner: Arc::clone(&self.checkpoint_owner),
            transaction: self.constraint_transactions.begin(),
            revision: self.state.revision,
            current_design: self.state.current_design.clone(),
            objects: self.state.objects.checkpoint(),
            timing: self.state.timing.checkpoint(),
        }
    }

    /// Keeps semantic changes.
    ///
    /// # Errors
    ///
    /// Returns an error if the checkpoint belongs to another session, is not
    /// active, or its timing domain is inconsistent.
    pub fn commit_constraint_checkpoint(
        &mut self,
        checkpoint: ConstraintCheckpoint,
    ) -> Result<(), SessionError> {
        self.require_checkpoint_owner(&checkpoint)?;
        self.state.timing.validate_checkpoint(&checkpoint.timing)?;
        self.state.timing.commit_checkpoint(checkpoint.timing)?;
        self.constraint_transactions.commit(checkpoint.transaction);
        Ok(())
    }

    /// Restores semantic state and invalidates the derived timing-model cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the checkpoint is foreign, stale, or inconsistent
    /// with any checkpointed state domain.
    pub fn restore_constraint_checkpoint(
        &mut self,
        checkpoint: ConstraintCheckpoint,
    ) -> Result<(), SessionError> {
        self.require_checkpoint_owner(&checkpoint)?;
        self.state.timing.validate_checkpoint(&checkpoint.timing)?;
        self.state.objects.validate_checkpoint(checkpoint.objects)?;
        self.state.objects.rollback(checkpoint.objects)?;
        self.state.revision = checkpoint.revision;
        self.state.current_design = checkpoint.current_design;
        self.constraint_transactions.restore(checkpoint.transaction);
        self.state.timing.rollback_checkpoint(checkpoint.timing)?;
        self.process.clear_analysis_caches();
        Ok(())
    }

    fn require_checkpoint_owner(
        &self,
        checkpoint: &ConstraintCheckpoint,
    ) -> Result<(), SessionError> {
        if Arc::ptr_eq(&self.checkpoint_owner, &checkpoint.owner) {
            Ok(())
        } else {
            Err(SessionError::state(
                "constraint checkpoint belongs to another session",
            ))
        }
    }

    pub(crate) fn current(&self) -> Result<DesignView<'_>, SessionError> {
        let name = self.state.current_design.as_deref().ok_or_else(|| {
            SessionError::state("no current design; use elaborate or set_db current_design")
        })?;
        self.state
            .designs
            .get(name)
            .map(DesignView::from_record)
            .ok_or_else(|| {
                SessionError::state(format!(
                    "current design '{name}' is missing from design store"
                ))
            })
    }

    pub(crate) fn current_record(&self) -> Result<&DesignRecord, SessionError> {
        let name = self.current_design_name()?;
        self.state.designs.get(name).ok_or_else(|| {
            SessionError::state(format!(
                "current design '{name}' is missing from design store"
            ))
        })
    }

    pub(crate) fn current_design_name(&self) -> Result<&str, SessionError> {
        self.state.current_design.as_deref().ok_or_else(|| {
            SessionError::state("no current design; use elaborate or set_db current_design")
        })
    }

    pub(crate) fn design_by_name(&self, name: &str) -> Result<DesignView<'_>, SessionError> {
        self.state
            .designs
            .get(name)
            .map(DesignView::from_record)
            .ok_or_else(|| {
                SessionError::state(format!("design '{name}' is missing from design store"))
            })
    }

    pub(crate) fn design_uid(&self, name: &str) -> Result<DesignId, SessionError> {
        self.state
            .objects
            .get_resolved(ResolvedObject::Design { name })
            .and_then(AnyObjectId::downcast)
            .ok_or_else(|| SessionError::state(format!("design '{name}' has no object identity")))
    }

    pub(crate) fn port_bindings(
        &self,
        design: DesignView<'_>,
    ) -> Result<opto_timing::PortBindings, SessionError> {
        let ids = design
            .ports()
            .map(|port| {
                let id = self
                    .state
                    .objects
                    .get_resolved(ResolvedObject::Port {
                        design: design.name(),
                        name: port.name,
                    })
                    .and_then(AnyObjectId::downcast)
                    .ok_or_else(|| {
                        SessionError::state(format!(
                            "port '{}' in design '{}' has no typed object identity",
                            port.name,
                            design.name()
                        ))
                    })?;
                Ok::<_, SessionError>(id)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(opto_timing::PortBindings::new(ids))
    }

    pub(crate) fn next_revision(&self) -> Result<RevisionId, SessionError> {
        self.state.revision.next().map_err(SessionError::Revision)
    }
}
