// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Configuration, progress events, and the sealed output of synthesis.
//!
//! Progress events describe completed observations. [`SynthesisResult`] owns
//! the mapped netlist, implementation provenance, and incremental snapshot.

use crate::{ImplementationDb, IncrementalSnapshot, SourceChangeMetrics, SourceSnapshot};
use opto_core::resident;
use opto_ir::mapped::{MappedNetlist, PortId};
#[cfg(test)]
use opto_ir::word;
use opto_library::TargetCellSet;
use serde::{Deserialize, Serialize};
use std::mem::size_of;

#[derive(Debug, Clone)]
/// Technology-specific inputs required to synthesis a design.
pub struct SynthesisOptions {
    /// Cells that mapping and post-map optimization may instantiate.
    pub target_cells: TargetCellSet,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Diagnostic controls for one synthesis invocation.
///
/// Synthesis has no user-selectable transform, architecture, or search-budget
/// controls. Every design is synthesized by the one documented flow, so a result
/// is reproducible from its inputs alone.
pub struct SynthesisConfig {
    /// Optional diagnostic output and consistency checks.
    pub diagnostics: SynthesisDiagnostics,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Expensive diagnostics intended for profiling and implementation validation.
#[allow(
    clippy::struct_excessive_bools,
    reason = "these are independent opt-in diagnostic channels, not a state machine"
)]
pub struct SynthesisDiagnostics {
    /// Emit timing-optimization progress diagnostics.
    pub timing: bool,
    /// Emit diagnostics while constructing joint-cell implementations.
    pub joint_cells: bool,
    /// Emit diagnostics for multi-function resynthesis.
    pub mfs: bool,
    /// Recompute incremental analyses and compare them with full results.
    pub check_incremental: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Search intensity used by the synthesis commands.
///
/// Effort changes which optimization passes and repetitions are enabled. It
/// does not flatten design-unit ownership or select a different database
/// representation.
pub enum SynthesisEffort {
    /// Minimize repeated search while retaining the complete synthesis pipeline.
    Low,
    /// Run the bounded baseline mapping and post-map flow.
    #[default]
    Medium,
    /// Add critical-fanout repair and repeated timing passes.
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the policy is a derived set of independent pass gates for one effort level"
)]
pub(crate) struct SynthesisPolicy {
    pub(crate) resynthesis: bool,
    pub(crate) critical_fanout_cloning: bool,
    pub(crate) repeated_timing_passes: bool,
}

impl SynthesisEffort {
    pub(crate) const fn policy(self) -> SynthesisPolicy {
        match self {
            Self::Low => SynthesisPolicy {
                resynthesis: false,
                critical_fanout_cloning: false,
                repeated_timing_passes: false,
            },
            Self::Medium => SynthesisPolicy {
                resynthesis: true,
                critical_fanout_cloning: false,
                repeated_timing_passes: false,
            },
            Self::High => SynthesisPolicy {
                resynthesis: true,
                critical_fanout_cloning: true,
                repeated_timing_passes: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Stable identifier for one synthesis pipeline stage.
pub struct StageId(&'static str);

impl StageId {
    /// Flatten the linked design hierarchy into the canonical root module.
    pub const LINKED_ELABORATION: Self = Self::new("linked_elaboration");
    /// Normalize procedural CFGs and resolved nets into structural Word IR.
    pub const NORMALIZATION: Self = Self::new("normalization");
    /// Analyze independent procedural CFGs on deterministic workers.
    pub const NORMALIZATION_CFG_ANALYSIS: Self = Self::new("normalization.cfg_analysis");
    /// Commit normalized procedures into the shared Word IR in stable order.
    pub const NORMALIZATION_PROCEDURE_COMMIT: Self = Self::new("normalization.procedure_commit");
    /// Freeze Word semantics, partition stable regions, and build contracts.
    pub const REGIONAL_PLANNING: Self = Self::new("regional_planning");
    /// Lower word-level operations into Boolean logic.
    pub const LOGIC_LOWERING: Self = Self::new("logic_lowering");
    /// Optimize and cover the initial logic network.
    pub const INITIAL_MAPPING: Self = Self::new("initial_mapping");
    /// Materialize the mapped design database.
    pub const MAPPED_NETLIST: Self = Self::new("mapped_netlist");
    /// Optimize mapped cells for area, timing, and design rules.
    pub const POSTMAP_OPTIMIZATION: Self = Self::new("postmap_optimization");
    /// Build reports, metrics, provenance, and the synthesis result.
    pub const FINALIZATION: Self = Self::new("finalization");

    /// Construct a custom stable stage identifier.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// Borrow the identifier as its diagnostic name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// One lifecycle or candidate-observation event from synthesis.
///
/// The variants make the two event shapes explicit: stage lifecycle events
/// cannot accidentally carry candidate metrics, and candidate observations
/// always carry their phase, area, and cell count together.
pub enum SynthesisProgress {
    /// A stage entered or left one lifecycle state.
    Stage {
        /// Stable stage identifier suitable for logs and machine consumers.
        stage: StageId,
        /// Lifecycle state represented by this event.
        status: SynthesisProgressStatus,
    },
    /// An optimization phase committed a candidate artifact.
    Candidate {
        /// Optimization phase responsible for the candidate.
        phase: OptimizationPhase,
        /// Total mapped cell area in target-library area units.
        area: f64,
        /// Number of mapped cells after the commit.
        cells: usize,
        /// Timing measurements, when timing was evaluated for this candidate.
        timing: Option<SynthesisTimingProgress>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
/// Timing measurements attached to a committed synthesis candidate.
pub struct SynthesisTimingProgress {
    /// Worst slack in the timing library's time unit, if an endpoint exists.
    pub worst_slack: Option<f64>,
    /// Sum of negative endpoint slack in the timing library's time unit.
    pub total_negative_slack: f64,
    /// Number of endpoints with negative slack.
    pub violations: usize,
    /// Number of candidates evaluated by the phase so far.
    pub evaluations: usize,
}

/// The terminal state of one synthesis-stage progress event.
///
/// Every started stage emits exactly one completed or failed event. Candidate
/// updates are completed observations rather than stage-lifecycle events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynthesisProgressStatus {
    /// The stage has begun and has not produced an artifact yet.
    Started,
    /// The stage or candidate commit completed successfully.
    Completed,
    /// The stage terminated without publishing its output.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Optimization phase responsible for a committed candidate observation.
pub enum OptimizationPhase {
    /// Initial covering of Boolean logic with target-library cells.
    TechnologyMapping,
    /// Local multi-function Boolean resynthesis after mapping.
    BooleanResynthesis,
    /// Sequential-cell implementation selection.
    RegisterOptimization,
    /// One-directional cell sizing that preserves earlier improvements.
    MonotonicSizing,
    /// Cell sizing that trades area against timing and design-rule quality.
    TradeoffSizing,
    /// Whole-net buffer-tree synthesis for electrically expensive fanout.
    FanoutTreeSynthesis,
    /// Driver cloning for residual timing-critical branches.
    CriticalFanoutCloning,
    /// Repair of explicit transition, capacitance, or fanout violations.
    DesignRuleRepair,
    /// Commutative input permutation to improve pin-dependent timing.
    PinSwap,
}

impl OptimizationPhase {
    /// Return the stable stage identifier used for progress reporting.
    #[must_use]
    pub const fn stage(self) -> StageId {
        match self {
            Self::TechnologyMapping => StageId::new("technology_mapping"),
            Self::BooleanResynthesis => StageId::new("postmap_boolean_resynthesis"),
            Self::RegisterOptimization => StageId::new("postmap_registers"),
            Self::MonotonicSizing => StageId::new("postmap_monotonic_sizing"),
            Self::TradeoffSizing => StageId::new("postmap_tradeoff_sizing"),
            Self::FanoutTreeSynthesis => StageId::new("postmap_fanout_tree_synthesis"),
            Self::CriticalFanoutCloning => StageId::new("postmap_residual_fanout_cloning"),
            Self::DesignRuleRepair => StageId::new("postmap_design_rule_repair"),
            Self::PinSwap => StageId::new("postmap_pin_swap"),
        }
    }
}

impl SynthesisProgress {
    fn lifecycle(stage: StageId, status: SynthesisProgressStatus) -> Self {
        Self::Stage { stage, status }
    }

    /// Construct a stage-start lifecycle observation.
    #[must_use]
    pub fn started(stage: StageId) -> Self {
        Self::lifecycle(stage, SynthesisProgressStatus::Started)
    }

    /// Construct a successful stage-completion lifecycle observation.
    #[must_use]
    pub fn completed(stage: StageId) -> Self {
        Self::lifecycle(stage, SynthesisProgressStatus::Completed)
    }

    /// Construct a failed stage-completion lifecycle observation.
    #[must_use]
    pub fn failed(stage: StageId) -> Self {
        Self::lifecycle(stage, SynthesisProgressStatus::Failed)
    }

    pub(crate) fn candidate(phase: OptimizationPhase, area: f64, cells: usize) -> Self {
        Self::Candidate {
            phase,
            area,
            cells,
            timing: None,
        }
    }

    pub(crate) fn timing_candidate(
        phase: OptimizationPhase,
        area: f64,
        cells: usize,
        analysis: &opto_timing::TimingQualitySummary,
        evaluations: usize,
    ) -> Self {
        Self::Candidate {
            phase,
            area,
            cells,
            timing: Some(SynthesisTimingProgress {
                worst_slack: analysis.wns(),
                total_negative_slack: analysis.tns(),
                violations: analysis.violating_paths(),
                evaluations,
            }),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// Structural and area summary of a mapped synthesis artifact.
pub struct SynthesisReport {
    /// Elaborated top-level design name.
    pub design: String,
    /// Number of top-level ports in the mapped design.
    pub ports: usize,
    /// Number of instantiated mapped cells.
    pub cells: usize,
    /// Number of nets in the mapped design.
    pub nets: usize,
    /// Sum of mapped-instance area in target-library area units.
    pub total_cell_area: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Pipeline-size and incremental-reuse measurements recorded at finalization.
pub struct SynthesisMetrics {
    /// Scheduler work, utilization-time, ready-depth, and memory-admission counters.
    pub execution: opto_runtime::ExecutionMetrics,
    /// Change classification against the previous compatible source snapshot.
    pub source_change: SourceChangeMetrics,
    /// Number of values after RTL normalization.
    pub normalized_values: usize,
    /// Number of operations after RTL normalization.
    pub normalized_operations: usize,
    /// Number of values after Boolean lowering.
    pub lowered_values: usize,
    /// Number of operations after Boolean lowering.
    pub lowered_operations: usize,
    /// Number of cells in the published mapped netlist.
    pub mapped_cells: usize,
    /// Number of nets in the published mapped netlist.
    pub mapped_nets: usize,
    /// Semantic operator occurrences recognized in region-private modules.
    pub operator_instances: usize,
    /// Serialized bytes occupied by the durable operator manifest.
    pub operator_manifest_bytes: usize,
    /// Boolean truth-window recipes reused from prior revisions.
    pub boolean_recipe_hits: usize,
    /// Boolean truth-window recipes synthesized for this synthesis.
    pub boolean_recipe_misses: usize,
    /// Region decision vectors reused from the prior artifact snapshot.
    pub regional_decision_hits: usize,
    /// Region decision vectors rebuilt for this synthesis.
    pub regional_decision_misses: usize,
    /// Stable synthesis regions in the frozen Word revision.
    pub synthesis_regions: usize,
    /// Compact selected region plans committed as mapped artifacts.
    pub regional_cover_plans: usize,
    /// Deterministic contract epochs executed by initial mapping.
    pub regional_epochs: usize,
    /// Largest resident byte footprint of the transient MMMC timing service.
    ///
    /// The service is released before publication, so these bytes are not part
    /// of [`SynthesisResult::resident_memory_bytes`].
    pub timing_resident_bytes: usize,
    /// Maximum concurrent construction scratch used by MMMC timing views.
    pub timing_construction_scratch_high_water_bytes: usize,
    /// Maximum resident-plus-transient byte footprint observed while building
    /// and retaining the MMMC timing service.
    pub timing_construction_high_water_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
/// Sealed, checkpointable output of a successful synthesis invocation.
///
/// The result keeps the mapped connectivity and [`ImplementationDb`] together:
/// consumers must not treat either as meaningful without the other.
pub struct SynthesisResult {
    #[cfg(test)]
    #[serde(skip, default = "checkpoint_test_module")]
    pub(crate) module: word::WordModule,
    pub(crate) mapped: MappedNetlist,
    pub(crate) report: SynthesisReport,
    pub(crate) implementation_db: ImplementationDb,
    pub(crate) operator_manifest: crate::OperatorManifest,
    pub(crate) timing: Option<TimingSummary>,
    pub(crate) metrics: SynthesisMetrics,
    pub(crate) incremental: IncrementalSnapshot,
}

impl SynthesisResult {
    #[cfg(test)]
    /// Return the retained word-level module used by synthesis tests.
    pub fn module(&self) -> &word::WordModule {
        &self.module
    }

    /// Borrow the published mapped netlist.
    pub fn mapped(&self) -> &MappedNetlist {
        &self.mapped
    }

    /// Borrow the structural and area summary computed from `mapped`.
    pub fn report(&self) -> &SynthesisReport {
        &self.report
    }

    /// Borrow implementation provenance indexed by mapped object identifiers.
    pub fn implementation_db(&self) -> &ImplementationDb {
        &self.implementation_db
    }

    /// Borrow durable operator semantics and source provenance.
    pub fn operator_manifest(&self) -> &crate::OperatorManifest {
        &self.operator_manifest
    }

    /// Return the timing summary when timing constraints were available.
    pub fn timing(&self) -> Option<TimingSummary> {
        self.timing
    }

    /// Return pipeline-size and incremental-reuse measurements.
    pub fn metrics(&self) -> SynthesisMetrics {
        self.metrics
    }

    /// Borrow the source identity retained for the next incremental synthesis.
    pub fn source_snapshot(&self) -> &SourceSnapshot {
        self.incremental.source()
    }

    /// Borrow all state required by a compatible incremental synthesis.
    pub const fn incremental_snapshot(&self) -> &IncrementalSnapshot {
        &self.incremental
    }

    /// Consume the artifact while retaining its incremental state.
    pub fn into_incremental_snapshot(self) -> IncrementalSnapshot {
        self.incremental
    }

    /// Releases construction slack in every sealed artifact arena. Logical IDs
    /// and checkpoint content remain unchanged.
    pub fn compact(&mut self) {
        self.mapped.compact();
        self.report.design.shrink_to_fit();
        self.implementation_db.compact();
    }

    /// Deterministic byte model for this sealed artifact. It is based on live
    /// owned payloads after compaction, never `Vec::capacity`, and each modeled
    /// allocation includes a 25% allocator margin plus two metadata words.
    #[must_use]
    pub fn resident_memory_bytes(&self) -> usize {
        size_of::<Self>()
            .saturating_add(self.mapped.owned_memory_bytes())
            .saturating_add(resident::allocation_bytes(self.report.design.len()))
            .saturating_add(self.implementation_db.owned_memory_bytes())
            .saturating_add(self.operator_manifest.owned_memory_bytes())
            .saturating_add(self.incremental.owned_memory_bytes())
    }

    /// Validate all cross-object identifiers and checkpoint invariants.
    ///
    /// # Errors
    ///
    /// Returns the first invalid source snapshot, mapped-netlist reference, or
    /// implementation-provenance relationship.
    pub fn validate_checkpoint(&self) -> Result<(), crate::SynthError> {
        self.incremental.validate_checkpoint()?;
        self.mapped
            .validate_checkpoint()
            .map_err(crate::SynthError::from)?;
        let mapped_ports =
            self.mapped
                .ports()
                .iter()
                .enumerate()
                .try_fold(0usize, |total, (index, _)| {
                    let port = PortId::from_index(index).map_err(crate::SynthError::Mapped)?;
                    let width = self.mapped.port_nets(port).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "checkpoint mapped port has no valid net range",
                        )
                    })?;
                    total.checked_add(width.len()).ok_or_else(|| {
                        crate::SynthError::capacity("checkpoint mapped port-bit count")
                    })
                })?;
        let mapped_cells = self
            .mapped
            .cell_count()
            .checked_add(self.mapped.design_instance_count())
            .ok_or_else(|| crate::SynthError::capacity("checkpoint mapped instance count"))?;
        self.report
            .validate_checkpoint(&self.mapped, mapped_ports, mapped_cells)?;
        self.metrics.validate_checkpoint(
            &self.mapped,
            &self.operator_manifest,
            mapped_cells,
            self.timing.is_some(),
        )?;
        if let Some(timing) = self.timing {
            timing.validate_checkpoint()?;
        }
        self.implementation_db.validate_checkpoint(&self.mapped)?;
        self.operator_manifest.validate_checkpoint()
    }

    /// Deterministic upper bound for temporary memory used by
    /// [`Self::validate_checkpoint`].
    #[must_use]
    pub fn checkpoint_validation_memory_bytes(&self) -> usize {
        self.mapped.checkpoint_validation_memory_bytes()
    }

    #[cfg(test)]
    pub(crate) fn into_module_and_report(
        self,
    ) -> (word::WordModule, SynthesisReport, MappedNetlist) {
        (self.module, self.report, self.mapped)
    }
}

impl SynthesisReport {
    fn validate_checkpoint(
        &self,
        mapped: &MappedNetlist,
        mapped_ports: usize,
        mapped_cells: usize,
    ) -> Result<(), crate::SynthError> {
        if self.design.is_empty()
            || self.design != mapped.name()
            || self.ports != mapped_ports
            || self.cells != mapped_cells
            || self.nets != mapped.net_count()
        {
            return Err(crate::SynthError::invariant(
                "checkpoint synthesis report disagrees with its mapped netlist",
            ));
        }
        if !self.total_cell_area.is_finite() || self.total_cell_area < 0.0 {
            return Err(crate::SynthError::invariant(
                "checkpoint synthesis report has invalid total cell area",
            ));
        }
        Ok(())
    }
}

impl SynthesisMetrics {
    fn validate_checkpoint(
        &self,
        mapped: &MappedNetlist,
        operators: &crate::OperatorManifest,
        mapped_cells: usize,
        has_timing: bool,
    ) -> Result<(), crate::SynthError> {
        let execution = self.execution;
        let composite = [
            execution.composite_active_nanoseconds,
            execution.composite_wall_nanoseconds,
            execution.composite_estimated_work,
            execution.composite_peak_ready_tasks,
            execution.composite_peak_admitted_memory,
        ];
        if (execution.composite_batches == 0 && composite != [0; 5])
            || (execution.composite_batches != 0
                && (execution.composite_peak_ready_tasks == 0
                    || execution.composite_estimated_work == 0))
        {
            return Err(crate::SynthError::invariant(
                "checkpoint execution metrics are inconsistent",
            ));
        }
        let changes = self.source_change;
        if changes.changed_values > changes.values
            || changes.changed_operations > changes.operations
            || changes.changed_boundaries > changes.boundaries
            || changes.rebuilt_regions.checked_add(changes.reused_regions) != Some(changes.regions)
        {
            return Err(crate::SynthError::invariant(
                "checkpoint source-change metrics are inconsistent",
            ));
        }
        if self.mapped_cells != mapped_cells || self.mapped_nets != mapped.net_count() {
            return Err(crate::SynthError::invariant(
                "checkpoint synthesis metrics disagree with their mapped netlist",
            ));
        }
        if self.operator_instances != operators.instances().len()
            || self.operator_manifest_bytes != operators.serialized_size()?
        {
            return Err(crate::SynthError::invariant(
                "checkpoint operator metrics disagree with their manifest",
            ));
        }
        if self
            .regional_decision_hits
            .checked_add(self.regional_decision_misses)
            != Some(self.synthesis_regions)
            || self.regional_cover_plans != self.synthesis_regions
        {
            return Err(crate::SynthError::invariant(
                "checkpoint regional synthesis metrics are inconsistent",
            ));
        }
        let timing_memory = [
            self.timing_resident_bytes,
            self.timing_construction_scratch_high_water_bytes,
            self.timing_construction_high_water_bytes,
        ];
        if !has_timing && timing_memory != [0; 3] {
            return Err(crate::SynthError::invariant(
                "checkpoint without timing retains MMMC memory metrics",
            ));
        }
        if has_timing {
            self.timing_resident_bytes
                .checked_add(self.timing_construction_scratch_high_water_bytes)
                .ok_or_else(|| {
                    crate::SynthError::invariant("checkpoint MMMC timing memory metrics overflow")
                })?;
            if self.timing_construction_high_water_bytes < self.timing_resident_bytes
                || self.timing_construction_high_water_bytes
                    < self.timing_construction_scratch_high_water_bytes
            {
                return Err(crate::SynthError::invariant(
                    "checkpoint MMMC timing high-water metrics are inconsistent",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
/// Endpoint timing quality recorded for a completed mapped design.
pub struct TimingSummary {
    /// Latest endpoint arrival in the timing library's time unit.
    pub arrival: f64,
    /// Worst endpoint slack in the timing library's time unit.
    ///
    /// `None` means no endpoint has a required-time constraint.
    pub slack: Option<f64>,
    /// Sum of negative endpoint slack in the timing library's time unit.
    pub tns: f64,
    /// Number of endpoints with negative slack.
    pub violating_paths: usize,
    /// Worst `actual / limit` ratio among enabled electrical checks.
    pub worst_design_rule_ratio: f64,
    /// Number of enabled maximum transition, capacitance, or fanout failures.
    pub design_rule_violations: usize,
}

impl TimingSummary {
    fn validate_checkpoint(self) -> Result<(), crate::SynthError> {
        if !self.arrival.is_finite()
            || self.slack.is_some_and(|slack| !slack.is_finite())
            || !self.tns.is_finite()
            || self.tns > 0.0
        {
            return Err(crate::SynthError::invariant(
                "checkpoint timing summary has invalid path quality",
            ));
        }
        // A zero-valued maximum design-rule limit is legal and produces an
        // infinite ratio for any positive violation. Infinity is therefore a
        // meaningful nonnegative magnitude here; NaN and negative ratios are
        // never valid.
        if self.worst_design_rule_ratio.is_nan()
            || self.worst_design_rule_ratio < 0.0
            || self.design_rule_violations == 0 && self.worst_design_rule_ratio != 0.0
        {
            return Err(crate::SynthError::invariant(
                "checkpoint timing summary has invalid design-rule quality",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
fn checkpoint_test_module() -> word::WordModule {
    word::WordModule::new("<restored-checkpoint>")
}
