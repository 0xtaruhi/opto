// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Synthesis stage ordering and publication.
//!
//! Each stage produces a complete state consumed by the next; only finalization
//! seals a [`SynthesisResult`]. Progress observers see start/terminal pairs even
//! when a stage fails.

use crate::artifact::provenance::{ProvenanceBuilder, SourceInstanceProvenance};
use crate::mapping::{MappingConfig, TargetMappingContext, TargetMappingContextKey};
use crate::{
    ImplementationDb, IncrementalSnapshot, SourceChangeMetrics, SourceSnapshot, StageId,
    SynthesisEffort, SynthesisOptions, SynthesisProgress, SynthesisRegionGraph, SynthesisResult,
};
use opto_ir::{rtl::RtlModule, word};
use opto_runtime::ExecutionContext;
use opto_timing::{Scenario, ScenarioSet};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};

mod lowering;
mod publication;
mod regional_cache;
mod regional_mapping;
mod report;

const TARGET_MAPPING_CONTEXT_CAPACITY: usize = 4;
type TargetMappingContextCache = SmallVec<
    [(TargetMappingContextKey, Arc<TargetMappingContext>); TARGET_MAPPING_CONTEXT_CAPACITY],
>;

/// All immutable inputs and session identities required for one synthesis.
///
/// The request borrows or owns a sealed RTL module. Constraint and target views
/// are shared snapshots, so analysis workers cannot observe session mutation
/// during synthesis.
pub struct SynthesisRequest<'a> {
    /// Session revision from which the source view was derived.
    pub base_revision: opto_ir::RevisionId,
    /// Stable timing identity of the elaborated top-level design.
    pub design_id: opto_timing::DesignId,
    /// Stable timing identities parallel to top-level source ports.
    pub port_bindings: opto_timing::PortBindings,
    /// Persistent cell, pin, and net identities bound to mapped object names.
    pub object_bindings: Arc<opto_timing::TimingObjectBindings>,
    /// Canonical linked RTL consumed by normalization.
    pub source: Cow<'a, RtlModule>,
    /// Design-unit names resolved while linking the source.
    pub design_references: Arc<BTreeSet<String>>,
    /// Known reference port contracts used by structural validation.
    pub reference_ports: Arc<crate::ReferencePortMap>,
    /// Target-library mapping inputs.
    pub options: SynthesisOptions,
    /// Optimization search intensity.
    pub effort: SynthesisEffort,
    /// Clock-gating style, when `synthesis -gate_clock` requested gating.
    pub clock_gating: Option<crate::ClockGatingStyle>,
    /// Explicit sparse multi-mode, multi-corner analysis bindings.
    pub scenarios: ScenarioSet,
    /// Session-owned power service used by regional and post-map objectives.
    pub power_evaluator: Arc<dyn crate::SynthesisPowerEvaluator>,
    /// Compatible prior artifact state for incremental metrics and regional reuse.
    pub previous_incremental: Option<&'a IncrementalSnapshot>,
}

impl fmt::Debug for SynthesisRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SynthesisRequest")
            .field("base_revision", &self.base_revision)
            .field("design_id", &self.design_id)
            .field("port_bindings", &self.port_bindings)
            .field("object_bindings", &self.object_bindings)
            .field("source", &self.source)
            .field("design_references", &self.design_references)
            .field("reference_ports", &self.reference_ports)
            .field("options", &self.options)
            .field("effort", &self.effort)
            .field("clock_gating", &self.clock_gating)
            .field("scenarios", &self.scenarios)
            .field("power_evaluator", &"<dyn SynthesisPowerEvaluator>")
            .field("previous_incremental", &self.previous_incremental)
            .finish()
    }
}

#[cfg(test)]
impl SynthesisRequest<'static> {
    #[must_use]
    pub(crate) fn unconstrained(source: RtlModule, options: SynthesisOptions) -> Self {
        Self::unconstrained_source(Cow::Owned(source), options)
    }
}

impl<'a> SynthesisRequest<'a> {
    #[cfg(test)]
    fn unconstrained_source(source: Cow<'a, RtlModule>, options: SynthesisOptions) -> Self {
        let design_id = opto_timing::DesignId::from_uid(
            opto_core::ObjectUid::from_raw(1).expect("standalone design identity is nonzero"),
        );
        let port_bindings = opto_timing::PortBindings::new(
            source.word().ports().iter().enumerate().map(|(index, _)| {
                let uid = u64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(2))
                    .and_then(opto_core::ObjectUid::from_raw)
                    .expect("standalone port identity exceeds permanent UID capacity");
                opto_timing::PortId::from_uid(uid)
            }),
        );
        let scenario_library = opto_timing::TimingLibrary {
            cells: options.target_cells.clone(),
            ..opto_timing::TimingLibrary::default()
        };
        Self {
            base_revision: opto_ir::RevisionId::INITIAL,
            design_id,
            port_bindings,
            object_bindings: Arc::new(opto_timing::TimingObjectBindings::new()),
            source,
            design_references: Arc::new(BTreeSet::new()),
            reference_ports: Arc::new(crate::target_cell_reference_ports(&options.target_cells)),
            options,
            effort: SynthesisEffort::Medium,
            clock_gating: None,
            scenarios: ScenarioSet::single(
                Arc::new(opto_timing::TimingContext::default()),
                Arc::new(scenario_library),
                opto_timing::Parasitics::default(),
            ),
            power_evaluator: Arc::new(crate::NoPowerEvaluation),
            previous_incremental: None,
        }
    }

    /// Binds the validated synthesis environment to its canonical linked source.
    #[must_use]
    pub fn with_linked_source<'b>(self, source: &'b RtlModule) -> SynthesisRequest<'b>
    where
        'a: 'b,
    {
        SynthesisRequest {
            base_revision: self.base_revision,
            design_id: self.design_id,
            port_bindings: self.port_bindings,
            object_bindings: self.object_bindings,
            source: Cow::Borrowed(source),
            design_references: self.design_references,
            reference_ports: self.reference_ports,
            options: self.options,
            effort: self.effort,
            clock_gating: self.clock_gating,
            scenarios: self.scenarios,
            power_evaluator: self.power_evaluator,
            previous_incremental: self.previous_incremental,
        }
    }
}

fn validate_mapping_library(request: &SynthesisRequest<'_>) -> Result<(), crate::SynthError> {
    if request.options.target_cells.is_empty() {
        return Err(crate::SynthError::invalid(
            "synthesis requires a non-empty target library",
        ));
    }
    request
        .options
        .target_cells
        .validate_for_synthesis()
        .map_err(|error| crate::SynthError::invalid(error.to_string()))?;
    for scenario in request.scenarios.scenarios() {
        for (kind, library) in [
            ("early", scenario.early_library()),
            ("late", scenario.late_library()),
        ] {
            library
                .cells
                .validate_for_synthesis()
                .map_err(|error| crate::SynthError::invalid(error.to_string()))?;
            validate_scenario_target_cells(request, scenario, kind, library)?;
        }
        if scenario.constraints().has_optimization_constraints()
            && (scenario.early_library().cells.is_empty()
                || scenario.late_library().cells.is_empty())
        {
            return Err(crate::SynthError::invalid(format!(
                "scenario '{}' requires explicit early and late timing libraries",
                scenario.name()
            )));
        }
    }
    Ok(())
}

fn validate_scenario_target_cells(
    request: &SynthesisRequest<'_>,
    scenario: &Scenario,
    kind: &str,
    library: &opto_timing::TimingLibrary,
) -> Result<(), crate::SynthError> {
    if library.cells.is_empty() {
        return Ok(());
    }
    let timing_cells = library
        .cells
        .iter()
        .map(|cell| (cell.name(), cell))
        .collect::<BTreeMap<_, _>>();
    for (_, target) in request.options.target_cells.synthesis_cells() {
        let Some(timing) = timing_cells.get(target.name()) else {
            return Err(crate::SynthError::invalid(format!(
                "target cell '{}' is absent from scenario '{}' {kind} library",
                target.name(),
                scenario.name()
            )));
        };
        if !timing.mapping_eq(target) {
            return Err(crate::SynthError::invalid(format!(
                "target cell '{}' has incompatible mapping semantics in scenario '{}' {kind} library",
                target.name(),
                scenario.name()
            )));
        }
    }
    Ok(())
}

#[derive(Debug)]
/// Reusable synthesis service with immutable content-addressed recipe caches.
///
/// The engine is safe to reuse across designs. Cached mapping contexts contain
/// only immutable target-derived data and are keyed by the complete mapping
/// configuration. Regional reuse belongs to the prior artifact supplied in
/// [`SynthesisRequest`], never to this process-scoped service.
pub struct SynthesisEngine {
    config: crate::SynthesisConfig,
    mapping_contexts: RwLock<TargetMappingContextCache>,
    rewrite_recipes: crate::boolean::logic::RewriteRecipeCache,
}

impl Default for SynthesisEngine {
    fn default() -> Self {
        Self::with_config(crate::SynthesisConfig::default())
    }
}

impl SynthesisEngine {
    /// Construct an engine with default synthesis configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct an engine with explicit diagnostic and optimization controls.
    #[must_use]
    pub fn with_config(config: crate::SynthesisConfig) -> Self {
        Self {
            config,
            mapping_contexts: RwLock::default(),
            rewrite_recipes: crate::boolean::logic::RewriteRecipeCache::default(),
        }
    }

    /// Returns the mapping context for `options`, reusing the cached one when
    /// the target library is unchanged.
    ///
    /// Construction happens outside the lock: building the four target catalogs
    /// walks the whole library, and holding the engine-wide lock across it would
    /// serialize every concurrent synthesis behind one library load. A losing
    /// racer discards its own build and adopts the published entry, so the
    /// returned context is still shared.
    ///
    /// A poisoned lock only means an earlier synthesis panicked while touching the
    /// LRU. The cache holds nothing but immutable target-derived data, so
    /// rebuilding is always correct and never a synthesis failure.
    fn mapping_context(&self, options: &SynthesisOptions) -> Arc<TargetMappingContext> {
        let target = TargetMappingContextKey::from_options(options);
        if let Ok(mut cached) = self.mapping_contexts.write()
            && let Some(index) = cached.iter().position(|(key, _)| *key == target)
        {
            let entry = cached.remove(index);
            let context = Arc::clone(&entry.1);
            cached.push(entry);
            return context;
        }
        let context = Arc::new(TargetMappingContext::new(options, self.config));
        let Ok(mut cached) = self.mapping_contexts.write() else {
            return context;
        };
        if let Some(index) = cached.iter().position(|(key, _)| *key == target) {
            return Arc::clone(&cached[index].1);
        }
        if cached.len() == TARGET_MAPPING_CONTEXT_CAPACITY {
            cached.remove(0);
        }
        cached.push((target, Arc::clone(&context)));
        context
    }

    /// Run the complete synthesis pipeline and publish progress to `observer`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::SynthError`] when validation, lowering, optimization,
    /// mapping, analysis, or artifact publication fails.
    pub fn synthesize(
        &self,
        request: SynthesisRequest<'_>,
        runtime: &ExecutionContext,
        observer: &mut dyn FnMut(SynthesisProgress),
    ) -> Result<SynthesisResult, crate::SynthError> {
        self.synthesize_with_runtime(request, runtime, observer)
    }

    fn synthesize_with_runtime(
        &self,
        request: SynthesisRequest<'_>,
        runtime: &ExecutionContext,
        observer: &mut dyn FnMut(SynthesisProgress),
    ) -> Result<SynthesisResult, crate::SynthError> {
        let input = SynthesisInput::new(request)?;
        let design_id = input.environment.design_id;
        let mut execution = SynthesisExecution {
            engine: self,
            runtime,
            observer,
            design_id,
        };
        let normalized = execution.run_stage(StageId::NORMALIZATION, |execution| {
            normalize(execution, input)
        })?;
        let planned = execution.run_stage(StageId::REGIONAL_PLANNING, |execution| {
            plan_regions(execution, normalized)
        })?;
        let lowered = execution.run_stage(StageId::LOGIC_LOWERING, |execution| {
            lowering::lower_logic(execution, planned)
        })?;
        let initially_mapped = execution.run_stage(StageId::INITIAL_MAPPING, |execution| {
            map_initial_logic(execution, lowered)
        })?;
        let mapped = execution.run_stage(StageId::MAPPED_NETLIST, |_| {
            build_mapped_artifact(initially_mapped)
        })?;
        let optimized = execution.run_stage(StageId::POSTMAP_OPTIMIZATION, |execution| {
            optimize_postmap(execution, mapped)
        })?;
        execution.run_stage(StageId::FINALIZATION, |_| publication::finalize(optimized))
    }
}

struct SynthesisExecution<'a> {
    engine: &'a SynthesisEngine,
    runtime: &'a ExecutionContext,
    observer: &'a mut dyn FnMut(SynthesisProgress),
    design_id: opto_timing::DesignId,
}

impl SynthesisExecution<'_> {
    fn run_stage<T>(
        &mut self,
        stage: StageId,
        operation: impl FnOnce(&mut Self) -> Result<T, crate::SynthError>,
    ) -> Result<T, crate::SynthError> {
        (self.observer)(SynthesisProgress::started(stage));
        let _profile = crate::api::diagnostics::ProfileSpan::new(
            self.engine.config.diagnostics.timing,
            || format!("stage.{}[{:?}]", stage.as_str(), self.design_id.uid()),
        );
        let output = match operation(self) {
            Ok(output) => output,
            Err(error) => {
                (self.observer)(SynthesisProgress::failed(stage));
                return Err(error);
            }
        };
        (self.observer)(SynthesisProgress::completed(stage));
        Ok(output)
    }
}

struct SynthesisEnvironment {
    base_revision: opto_ir::RevisionId,
    design_id: opto_timing::DesignId,
    port_bindings: opto_timing::PortBindings,
    object_bindings: Arc<opto_timing::TimingObjectBindings>,
    design_references: Arc<BTreeSet<String>>,
    reference_ports: Arc<crate::ReferencePortMap>,
    options: SynthesisOptions,
    effort: SynthesisEffort,
    clock_gating: Option<crate::ClockGatingStyle>,
    scenarios: ScenarioSet,
    power_evaluator: Arc<dyn crate::SynthesisPowerEvaluator>,
    incremental_metrics: Arc<crate::incremental::IncrementalRunMetrics>,
}

impl SynthesisEnvironment {
    fn primary_scenario(&self) -> &Scenario {
        self.scenarios
            .scenarios()
            .first()
            .expect("ScenarioSet construction rejects empty sets")
    }
}

/// Identities and measurements every stage carries but no stage interprets.
///
/// Stages move the ledger along unchanged except for the few fields they own,
/// so adding a carried measurement costs one field instead of one field per
/// stage state.
struct SynthesisLedger {
    source_snapshot: SourceSnapshot,
    source_change: SourceChangeMetrics,
    normalized_values: usize,
    normalized_operations: usize,
    lowered_values: usize,
    lowered_operations: usize,
    regional_cache_records: Box<[crate::incremental::RegionalCacheRecord]>,
    regional_epochs: usize,
    timing_memory: crate::closure::mmmc::MmmcTimingMemory,
    /// Word module retained for synthesis tests, captured at publication.
    #[cfg(test)]
    synthesized: Option<word::WordModule>,
}

impl SynthesisLedger {
    fn record_timing_memory(
        &mut self,
        timing: Option<&crate::closure::mmmc::MmmcTiming>,
    ) -> Result<(), crate::SynthError> {
        let Some(memory) = timing.map(crate::closure::mmmc::MmmcTiming::memory_usage) else {
            return Ok(());
        };
        memory
            .resident_bytes
            .checked_add(memory.construction_scratch_high_water_bytes)
            .ok_or_else(|| crate::SynthError::capacity("MMMC timing memory accounting"))?;
        if memory.construction_high_water_bytes < memory.resident_bytes
            || memory.construction_high_water_bytes < memory.construction_scratch_high_water_bytes
        {
            return Err(crate::SynthError::invariant(
                "MMMC timing construction high-water is inconsistent",
            ));
        }
        self.timing_memory = memory;
        Ok(())
    }
}

struct SynthesisInput {
    environment: SynthesisEnvironment,
    source: RtlModule,
    ledger: SynthesisLedger,
    previous_regional_cache_records: Arc<[crate::incremental::RegionalCacheRecord]>,
    source_instances: SourceInstanceProvenance,
}

impl SynthesisInput {
    fn new(request: SynthesisRequest<'_>) -> Result<Self, crate::SynthError> {
        if request.source.word().name().is_empty() {
            return Err(crate::SynthError::invariant(
                "current RTL module has no name",
            ));
        }
        validate_mapping_library(&request)?;
        if let Some(previous) = request.previous_incremental {
            previous.validate_checkpoint()?;
        }
        let source_snapshot = SourceSnapshot::capture(&request.source, request.effort);
        let source_instances = SourceInstanceProvenance::capture(request.source.word());
        let source_change = source_snapshot.changes_from(
            request
                .previous_incremental
                .map(IncrementalSnapshot::source),
        );
        let previous_regional_cache_records = request.previous_incremental.map_or_else(
            || Arc::from([]),
            IncrementalSnapshot::regional_cache_records,
        );
        let SynthesisRequest {
            base_revision,
            design_id,
            port_bindings,
            object_bindings,
            source,
            design_references,
            reference_ports,
            options,
            effort,
            clock_gating,
            scenarios,
            power_evaluator,
            previous_incremental: _,
        } = request;
        let source = source.into_owned();
        Ok(Self {
            environment: SynthesisEnvironment {
                base_revision,
                design_id,
                port_bindings,
                object_bindings,
                design_references,
                reference_ports,
                options,
                effort,
                clock_gating,
                scenarios,
                power_evaluator,
                incremental_metrics: Arc::new(crate::incremental::IncrementalRunMetrics::default()),
            },
            source,
            previous_regional_cache_records,
            ledger: SynthesisLedger {
                source_snapshot,
                source_change,
                normalized_values: 0,
                normalized_operations: 0,
                lowered_values: 0,
                lowered_operations: 0,
                regional_cache_records: Box::new([]),
                regional_epochs: 0,
                timing_memory: crate::closure::mmmc::MmmcTimingMemory::default(),
                #[cfg(test)]
                synthesized: None,
            },
            source_instances,
        })
    }
}

struct NormalizedState {
    environment: SynthesisEnvironment,
    ledger: SynthesisLedger,
    previous_regional_cache_records: Arc<[crate::incremental::RegionalCacheRecord]>,
    source_instances: SourceInstanceProvenance,
    synthesized: word::WordModule,
}

struct PlannedState {
    normalized: NormalizedState,
    mapping_context: Arc<TargetMappingContext>,
    target_model: crate::planning::regional::StructuralTargetModel,
    regions: SynthesisRegionGraph,
    design: crate::regional::WorkDesign,
    contracts: crate::regional::RegionContractSet,
}

struct LoweredState {
    environment: SynthesisEnvironment,
    ledger: SynthesisLedger,
    source_instances: SourceInstanceProvenance,
    mapping_context: Arc<TargetMappingContext>,
    regions: SynthesisRegionGraph,
    region_binding: crate::boolean::bitblast::LoweredRegionBinding,
    contracts: crate::regional::RegionContractSet,
    regional_plans: Box<[regional_mapping::RegionalPlanRow]>,
    sequential_operations: Box<[crate::mapping::materialize::SequentialRegionBinding]>,
    synthesized: word::WordModule,
    provenance: ProvenanceBuilder,
    operator_manifest: crate::OperatorManifest,
}

struct InitiallyMappedState {
    lowered: LoweredState,
    mapped: crate::mapping::MappedOutput,
    timing: Option<crate::closure::mmmc::MmmcTiming>,
}

struct MappedState {
    environment: SynthesisEnvironment,
    ledger: SynthesisLedger,
    mapped: opto_ir::mapped::MappedNetlist,
    connectivity: crate::mapping::materialize::FrozenObservableConnectivity,
    fanout_load_profile: crate::closure::postmap::MappedFanoutLoadProfile,
    implementations: ImplementationDb,
    timing: Option<crate::closure::mmmc::MmmcTiming>,
    operator_manifest: crate::OperatorManifest,
}

struct FinalizableState {
    options: SynthesisOptions,
    ledger: SynthesisLedger,
    mapped: opto_ir::mapped::MappedNetlist,
    connectivity: crate::mapping::materialize::FrozenObservableConnectivity,
    implementations: ImplementationDb,
    timing: Option<crate::TimingSummary>,
    incremental_reuse: crate::incremental::IncrementalReuseMetrics,
    operator_manifest: crate::OperatorManifest,
}

fn normalize(
    execution: &mut SynthesisExecution<'_>,
    input: SynthesisInput,
) -> Result<NormalizedState, crate::SynthError> {
    let SynthesisInput {
        environment,
        source,
        mut ledger,
        previous_regional_cache_records,
        source_instances,
    } = input;
    let synthesized = crate::frontend::lower_to_validated_word(
        source,
        &environment.reference_ports,
        execution.runtime,
        execution.observer,
    )?;
    ledger.normalized_values = synthesized.values().len();
    ledger.normalized_operations = synthesized.operations().len();
    Ok(NormalizedState {
        environment,
        ledger,
        previous_regional_cache_records,
        source_instances,
        synthesized,
    })
}

fn plan_regions(
    execution: &SynthesisExecution<'_>,
    mut normalized: NormalizedState,
) -> Result<PlannedState, crate::SynthError> {
    crate::boolean::bitblast::validate_synthesizable_constants(&normalized.synthesized)?;
    let mapping_context = execution
        .engine
        .mapping_context(&normalized.environment.options);
    let (regions, design) =
        crate::planning::regional::seal_work_design(&mut normalized.synthesized)?;
    normalized.ledger.normalized_values = normalized.synthesized.values().len();
    normalized.ledger.normalized_operations = normalized.synthesized.operations().len();
    let trace = crate::api::diagnostics::SynthTrace::timing(execution.engine.config.diagnostics);
    for region in regions.regions() {
        crate::api::diagnostics::trace!(
            trace,
            "regional_planning.region",
            "row={} kind={:?} operations={} inputs={} outputs={} work={}",
            region.row().raw(),
            region.kind(),
            regions.operations(*region).len(),
            regions.input_ports(*region).len(),
            regions.output_ports(*region).len(),
            region.estimated_work(),
        );
    }
    let target_model = crate::planning::regional::StructuralTargetModel::build(
        &normalized.environment.scenarios,
        |cells| {
            crate::mapping::library::CombinationalCellCatalog::from_cells(
                cells,
                crate::SynthesisDiagnostics::default(),
            )
            .representative_cost()
        },
    );
    let cost_envelopes =
        crate::planning::regional::RegionCostEnvelopeSet::build(&regions, &target_model);
    let budget_weights = cost_envelopes.budget_weights();
    let contracts = crate::regional::RegionContractSet::allocate(
        &normalized.synthesized,
        &regions,
        &budget_weights,
        &normalized.environment.scenarios,
        &normalized.environment.port_bindings,
        &normalized.environment.object_bindings,
        0,
    )?;
    normalized.ledger.regional_cache_records = crate::planning::regional::select_architectures(
        crate::planning::regional::RegionalSearchRequest {
            module: &normalized.synthesized,
            regions: &regions,
            scenarios: &normalized.environment.scenarios,
            target_cells: &normalized.environment.options.target_cells,
            target_model: &target_model,
            contracts: &contracts,
            effort: normalized.environment.effort,
            target_fingerprint: normalized
                .environment
                .options
                .target_cells
                .content_fingerprint()
                .bytes(),
            previous: &normalized.previous_regional_cache_records,
            metrics: &normalized.environment.incremental_metrics,
        },
        execution.runtime,
    )?;
    normalized.previous_regional_cache_records = Arc::from([]);
    Ok(PlannedState {
        normalized,
        mapping_context,
        target_model,
        regions,
        design,
        contracts,
    })
}

fn map_initial_logic(
    execution: &mut SynthesisExecution<'_>,
    mut lowered: LoweredState,
) -> Result<InitiallyMappedState, crate::SynthError> {
    let regional_mapping::RegionalMappingOutcome {
        plans: selected_plans,
        plan_journal,
        epochs,
        mapped,
        timing,
    } = regional_mapping::map_mapping_library_cells(
        regional_mapping::RegionalMappingRequest {
            module: &lowered.synthesized,
            provenance: &mut lowered.provenance,
            regions: &lowered.regions,
            region_binding: &lowered.region_binding,
            contracts: &lowered.contracts,
            regional_plans: &lowered.regional_plans,
            sequential_operations: &lowered.sequential_operations,
            config: MappingConfig {
                options: &lowered.environment.options,
                port_bindings: &lowered.environment.port_bindings,
                mapping_context: &lowered.mapping_context,
                scenarios: &lowered.environment.scenarios,
                object_bindings: Arc::clone(&lowered.environment.object_bindings),
                effort: lowered.environment.effort,
                design_id: lowered.environment.design_id,
                design_references: &lowered.environment.design_references,
                reference_ports: &lowered.environment.reference_ports,
                source_instances: &lowered.source_instances,
                base_revision: lowered.environment.base_revision,
                power_evaluator: lowered.environment.power_evaluator.as_ref(),
            },
        },
        execution.runtime,
        &mut *execution.observer,
    )?;
    lowered.ledger.regional_epochs = epochs;
    regional_cache::publish(
        &mut lowered.ledger.regional_cache_records,
        &selected_plans,
        plan_journal,
    )?;
    Ok(InitiallyMappedState {
        lowered,
        mapped,
        timing,
    })
}

fn build_mapped_artifact(
    initially_mapped: InitiallyMappedState,
) -> Result<MappedState, crate::SynthError> {
    let InitiallyMappedState {
        lowered,
        mapped,
        timing,
    } = initially_mapped;
    let crate::mapping::MappedOutput {
        netlist,
        cell_sources,
    } = mapped;
    let connectivity = crate::mapping::materialize::FrozenObservableConnectivity::capture(
        &netlist,
        &lowered.environment.options.target_cells,
        &lowered.environment.reference_ports,
    )?;
    let implementations = lowered.provenance.finish(
        &lowered.regions,
        &lowered.synthesized,
        &netlist,
        &cell_sources,
    )?;
    let fanout_load_profile = crate::closure::postmap::MappedFanoutLoadProfile::build(
        &netlist,
        &lowered.environment.options.target_cells,
    )?;
    #[cfg_attr(
        not(test),
        expect(unused_mut, reason = "the word module is retained only by tests")
    )]
    let mut ledger = lowered.ledger;
    #[cfg(test)]
    {
        ledger.synthesized = Some(lowered.synthesized);
    }
    Ok(MappedState {
        environment: lowered.environment,
        ledger,
        mapped: netlist,
        connectivity,
        fanout_load_profile,
        implementations,
        timing,
        operator_manifest: lowered.operator_manifest,
    })
}

impl MappedState {
    fn into_finalizable(mut self) -> Result<FinalizableState, crate::SynthError> {
        self.ledger.record_timing_memory(self.timing.as_ref())?;
        let timing = self
            .timing
            .as_mut()
            .map(crate::closure::mmmc::MmmcTiming::summary)
            .transpose()?;
        Ok(FinalizableState {
            incremental_reuse: self.environment.incremental_metrics.snapshot(),
            options: self.environment.options,
            ledger: self.ledger,
            mapped: self.mapped,
            connectivity: self.connectivity,
            implementations: self.implementations,
            timing,
            operator_manifest: self.operator_manifest,
        })
    }
}

fn optimize_postmap(
    execution: &mut SynthesisExecution<'_>,
    mut mapped: MappedState,
) -> Result<FinalizableState, crate::SynthError> {
    let postmap_catalog =
        crate::closure::postmap::PostmapCellCatalog::new(&mapped.environment.options);
    let outcome = crate::closure::postmap::optimize_mapped_netlist(
        crate::closure::postmap::PostmapRequest {
            mapped: &mut mapped.mapped,
            implementations: &mut mapped.implementations,
            timing: mapped.timing.take(),
            options: &mapped.environment.options,
            catalog: &postmap_catalog,
            scenarios: &mapped.environment.scenarios,
            fanout_load_profile: &mapped.fanout_load_profile,
            policy: mapped.environment.effort.policy(),
            runtime: execution.runtime,
            power_evaluator: Arc::clone(&mapped.environment.power_evaluator),
            connectivity: &mapped.connectivity,
        },
        execution.engine.config,
        &mut *execution.observer,
    )?;
    let fragment_impact = mapped.implementations.take_committed_fragment_impact();
    if !outcome.changed && !fragment_impact.is_empty() {
        return Err(crate::SynthError::invariant(
            "post-map provenance recorded fragment changes without a committed replacement",
        ));
    }
    if !fragment_impact.unknown_cells().is_empty() {
        return Err(crate::SynthError::UnknownMappedFragments {
            cells: fragment_impact.unknown_cells().iter().copied().collect(),
        });
    }
    let touched_regions = fragment_impact.regions();
    for record in &mut mapped.ledger.regional_cache_records {
        if record
            .plan_region()
            .is_some_and(|region| touched_regions.contains(&region))
        {
            record.clear_plan();
        }
    }
    mapped.timing = outcome.timing;
    mapped.into_finalizable()
}

#[cfg(test)]
pub(crate) fn synthesize_rtl_module(
    source: RtlModule,
    options: SynthesisOptions,
    runtime: &ExecutionContext,
) -> Result<SynthesisResult, crate::SynthError> {
    SynthesisEngine::new().synthesize(
        SynthesisRequest::unconstrained(source, options),
        runtime,
        &mut |_| {},
    )
}

#[cfg(test)]
mod tests;
