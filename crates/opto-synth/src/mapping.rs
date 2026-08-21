// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Technology-mapping mechanics and deterministic artifact materialization.
//!
//! This domain owns target catalogs, cover selection, sequential mapping, and
//! regional commit. Epoch scheduling and timing feedback belong to the engine.

use crate::SynthesisOptions;
use opto_ir::word;

mod architecture;
mod cell;
pub(crate) mod clock_gating;
pub(crate) mod cover;
pub(crate) mod library;
pub(crate) mod logic_partition;
pub(crate) mod materialize;
mod region_binding;
mod roots;
mod sequential;
pub(crate) mod word_util;

#[cfg(test)]
mod sequential_integration_tests;

pub use clock_gating::ClockGatingStyle;

pub(crate) use architecture::{
    RegionalArchitectureMapping, RegionalArchitectureRequest,
    extend_operation_regions_for_memories, prepare_regional_architectures, regional_proof,
};
pub(crate) use cell::{MappedCell, MappedInputConnection, MappedOutputConnection};
use library::CombinationalCellCatalog;
pub(crate) use materialize::MappedOutput;
#[cfg(test)]
pub(crate) use materialize::build_test_substrate;
pub(crate) use region_binding::{
    CandidateBinding, CandidateBindingDomain, RegionPlanBinding, RegionPlanValueBinding,
    SequentialEndpoint, SequentialPinKey, SequentialPinRole, build_candidate_binding,
};
use sequential::SequentialCellCatalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TargetMappingContextKey([u8; 32]);

impl TargetMappingContextKey {
    pub(crate) fn from_options(options: &SynthesisOptions) -> Self {
        Self(options.target_cells.content_fingerprint().bytes())
    }

    #[cfg(test)]
    pub(crate) const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug)]
pub(crate) struct TargetMappingContext {
    pub(crate) combinational_catalog: CombinationalCellCatalog,
    sequential_catalog: SequentialCellCatalog,
    clock_gating_catalog: clock_gating::ClockGatingCatalog,
    pub(crate) config: crate::SynthesisConfig,
}

impl TargetMappingContext {
    pub(crate) fn new(options: &SynthesisOptions, config: crate::SynthesisConfig) -> Self {
        Self {
            combinational_catalog: CombinationalCellCatalog::new(options, config.diagnostics),
            sequential_catalog: SequentialCellCatalog::new(options),
            clock_gating_catalog: clock_gating::ClockGatingCatalog::new(options),
            config,
        }
    }

    pub(crate) fn prepare_private_structure(
        &self,
        module: &mut word::WordModule,
        state_feedback: &std::collections::BTreeMap<word::OpId, word::ValueId>,
        clock_gating: Option<ClockGatingStyle>,
        target_mapping: bool,
    ) -> Result<(), crate::SynthError> {
        let trace = crate::api::diagnostics::SynthTrace::new(self.config.diagnostics.timing);
        let mut stage_started = std::time::Instant::now();
        let mut finish_stage = |stage: &str| {
            crate::api::diagnostics::trace!(
                trace,
                "mapping.prepare",
                "stage={stage} wall={:?}",
                stage_started.elapsed()
            );
            stage_started = std::time::Instant::now();
        };
        sequential::normalize_sequential_controls(module)?;
        finish_stage("normalize controls");
        if target_mapping {
            sequential::lower_controls(module)?;
            finish_stage("lower controls");
        }
        if target_mapping {
            if let Some(style) = clock_gating {
                clock_gating::gate_register_clocks(module, &self.clock_gating_catalog, style)?;
                finish_stage("gate clocks");
            }
            // The single expansion site, and the last pass that may consume an
            // enable. Everything before it either keeps the enable exact or
            // turns it into a gated clock; whatever the target cannot realize as
            // an enabled cell becomes a next-state mux here.
            sequential::expand_unsupported_enables(
                module,
                &self.sequential_catalog,
                state_feedback,
            )?;
            finish_stage("expand enables");
            sequential::normalize_enable_polarities(
                module,
                &self.sequential_catalog,
                &self.combinational_catalog,
            )?;
            finish_stage("normalize enable polarities");
        }
        Ok(())
    }
}

pub(crate) struct MappingConfig<'a> {
    pub(crate) options: &'a SynthesisOptions,
    pub(crate) port_bindings: &'a opto_timing::PortBindings,
    pub(crate) mapping_context: &'a TargetMappingContext,
    pub(crate) scenarios: &'a opto_timing::ScenarioSet,
    pub(crate) object_bindings: std::sync::Arc<opto_timing::TimingObjectBindings>,
    pub(crate) effort: crate::SynthesisEffort,
    pub(crate) design_id: opto_timing::DesignId,
    pub(crate) design_references: &'a std::collections::BTreeSet<String>,
    pub(crate) reference_ports: &'a crate::ReferencePortMap,
    pub(crate) source_instances: &'a crate::artifact::provenance::SourceInstanceProvenance,
    pub(crate) base_revision: opto_ir::RevisionId,
    pub(crate) power_evaluator: &'a dyn crate::SynthesisPowerEvaluator,
}

/// Whether a mapped cell name was generated by region materialization.
pub(crate) fn is_synthetic_region_cell_name(name: &str) -> bool {
    name.starts_with(materialize::REGION_CELL_PREFIX)
}

/// Whether a mapped net name was generated by materialization.
pub(crate) fn is_synthetic_net_name(name: &str) -> bool {
    name.starts_with(materialize::MAPPED_NET_PREFIX)
}
