// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Regional mapping epoch orchestration and timing-feedback convergence.

use crate::SynthesisProgress;
use crate::artifact::{MappedCellSource, provenance::ProvenanceBuilder};
use crate::mapping::library::CombinationalCellCatalog;
use crate::mapping::materialize::region_delta::{
    MappedRegionArtifact, MappedRegionFootprint, MappedValueSignal, WordMappedSignals,
    regional_boundary_aliases,
};
use crate::mapping::{
    self, MappingConfig, RegionPlanBinding, RegionalMappingSeed, cover, logic_partition,
    mapping_roots, materialize,
};
use opto_ir::word;
use opto_runtime::{ExecutionContext, Task, TaskKey};

mod census;
mod epochs;
mod objective;
mod seed;

use objective::{BestMapping, MappedObjective};

pub(crate) struct RegionalMappingOutcome {
    pub(crate) plans: Box<[crate::RegionCoverPlan]>,
    pub(crate) plan_journal: Box<[RegionalPlanJournalRecord]>,
    pub(crate) epochs: usize,
    pub(crate) mapped: mapping::MappedOutput,
    pub(crate) timing: Option<crate::closure::mmmc::MmmcTiming>,
    pub(crate) boundary_repair_schema: crate::regional::BoundaryRepairSchema,
}

pub(crate) struct RegionalPlanJournalRecord {
    pub(crate) row: crate::RegionRowId,
    pub(crate) plan: crate::regional::RegionCoverPlanRecord,
}

type BoundaryValueObservation = ([u8; 32], Box<[word::ValueId]>);
const REGIONAL_COVER_TASK_DOMAIN: u32 = 0x5245_434f;
const REGIONAL_COMPACT_TASK_DOMAIN: u32 = 0x5243_4d50;

pub(crate) struct RegionalMappingRequest<'a> {
    pub(crate) module: &'a word::WordModule,
    pub(crate) provenance: &'a mut ProvenanceBuilder,
    pub(crate) regions: &'a crate::SynthesisRegionGraph,
    pub(crate) region_ownership: &'a crate::boolean::bitblast::LoweredRegionOwnership,
    pub(crate) contracts: &'a crate::regional::RegionContractSet,
    pub(crate) region_contexts: &'a [crate::RegionContextKey],
    pub(crate) region_decision_keys: &'a [[u8; 32]],
    pub(crate) seed: &'a RegionalMappingSeed,
    pub(crate) boundary_repairs: &'a [crate::regional::BoundaryRepairArtifactRecord],
    pub(crate) config: MappingConfig<'a>,
}

/// Read-only context shared by every regional mapping epoch.
struct RegionalMapper<'a> {
    regions: &'a crate::SynthesisRegionGraph,
    decision_keys: &'a [[u8; 32]],
    response_models: cover::CoverResponseModels<'a>,
    config: MappingConfig<'a>,
    runtime: &'a ExecutionContext,
    trace: crate::api::diagnostics::SynthTrace,
    boundary_repairs: &'a [crate::regional::BoundaryRepairArtifactRecord],
}

/// Frozen Word semantics and ownership plus the mutable provenance ledger.
struct RegionalIr<'a> {
    module: &'a word::WordModule,
    provenance: &'a mut ProvenanceBuilder,
    region_ownership: &'a crate::boolean::bitblast::LoweredRegionOwnership,
}

/// Regional state that one epoch may replace.
struct RegionalPlans {
    contracts: crate::regional::RegionContractSet,
    contexts: Vec<crate::RegionContextKey>,
    partition: logic_partition::RegionalLogicPartition,
    /// Retained cover analyses for contract-driven remapping.
    analyses: Option<Vec<cover::RegionCoverAnalysis>>,
    plans: Vec<crate::RegionCoverPlan>,
    bindings: Vec<RegionPlanBinding>,
    plan_journal: std::collections::BTreeMap<
        (usize, crate::RegionContextKey),
        crate::regional::RegionCoverPlanRecord,
    >,
}

impl RegionalPlans {
    fn journal_compacted_plan(
        &mut self,
        row: usize,
        plan: &crate::RegionCoverPlan,
    ) -> Result<(), crate::SynthError> {
        let record = plan.checkpoint_record();
        let key = (row, record.context_key());
        match self.plan_journal.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(record);
            }
            std::collections::btree_map::Entry::Occupied(entry) if entry.get() != &record => {
                return Err(crate::SynthError::invariant(
                    "one regional decision context compacted to different portable plans",
                ));
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
        Ok(())
    }

    fn take_plan_journal(&mut self) -> Result<Box<[RegionalPlanJournalRecord]>, crate::SynthError> {
        std::mem::take(&mut self.plan_journal)
            .into_iter()
            .map(|((row, _), plan)| {
                Ok(RegionalPlanJournalRecord {
                    row: crate::RegionRowId::from_index(row)?,
                    plan,
                })
            })
            .collect()
    }
}

/// The one mapped generation shared by every regional epoch.
///
/// Word IDs remain semantic bindings only. Mapped slot IDs are append-only;
/// replacing a region tombstones its previous footprint and installs its new
/// artifact without renumbering any surviving object.
struct RegionalMappedState {
    netlist: opto_ir::mapped::MappedNetlist,
    cell_sources: Vec<Option<MappedCellSource>>,
    implementation_census: Option<ImplementationCensus>,
    signals: WordMappedSignals,
    boundary_nets: Box<[crate::closure::BoundaryNetObservation]>,
    footprints: Vec<Option<MappedRegionFootprint>>,
    boundary_footprints: Vec<materialize::boundary_delta::MappedBoundaryRepairFootprint>,
    timing: Option<crate::closure::mmmc::MmmcTiming>,
}

#[derive(Clone)]
struct ImplementationCensus {
    library_area_all: f64,
    managed_cell_count: u64,
    leakage_by_scenario: Box<[ScenarioLeakageCensus]>,
    static_key: [u8; 32],
}

#[derive(Clone, Copy, Default)]
struct ScenarioLeakageCensus {
    known_total: f64,
    unknown_cells: u64,
}

/// What one epoch measured on the shared mapped generation.
struct MeasuredEpoch {
    plans: Vec<crate::RegionCoverPlan>,
    global_dynamic_power: Option<f64>,
}

fn observe_stage<T>(
    observer: &mut dyn FnMut(SynthesisProgress),
    stage: crate::StageId,
    operation: impl FnOnce() -> Result<T, crate::SynthError>,
) -> Result<T, crate::SynthError> {
    observer(SynthesisProgress::started(stage));
    match operation() {
        Ok(output) => {
            observer(SynthesisProgress::completed(stage));
            Ok(output)
        }
        Err(error) => {
            observer(SynthesisProgress::failed(stage));
            Err(error)
        }
    }
}

pub(crate) fn map_mapping_library_cells(
    request: RegionalMappingRequest<'_>,
    runtime: &ExecutionContext,
    observer: &mut dyn FnMut(SynthesisProgress),
) -> Result<RegionalMappingOutcome, crate::SynthError> {
    let RegionalMappingRequest {
        module,
        provenance,
        regions,
        region_ownership,
        contracts,
        region_contexts,
        region_decision_keys,
        seed,
        boundary_repairs,
        config,
    } = request;
    if region_contexts.len() != regions.regions().len()
        || region_decision_keys.len() != regions.regions().len()
    {
        return Err(crate::SynthError::invariant(
            "regional mapping identities do not align with the region graph",
        ));
    }
    let mut ir = RegionalIr {
        module,
        provenance,
        region_ownership,
    };
    let trace =
        crate::api::diagnostics::SynthTrace::timing(config.mapping_context.config.diagnostics);
    let contracts = contracts.clone();
    let partition = observe_stage(
        observer,
        crate::StageId::new("regional_mapping.partition"),
        || {
            let _profile = trace.span(|| "initial_mapping.regional_partition".to_string());
            let roots = mapping_roots(ir.module, config.timing, config.port_bindings)?;
            let partition = logic_partition::RegionalLogicPartition::build(
                ir.module,
                regions,
                ir.region_ownership,
                &contracts,
                &roots,
            )?;
            Ok(partition)
        },
    )?;
    let mapper = RegionalMapper {
        regions,
        decision_keys: region_decision_keys,
        response_models: cover::CoverResponseModels::new(config.scenarios),
        boundary_repairs,
        config,
        runtime,
        trace,
    };
    let mut state = RegionalPlans {
        contracts,
        contexts: region_contexts.to_vec(),
        partition,
        analyses: None,
        plans: Vec::new(),
        bindings: Vec::new(),
        plan_journal: std::collections::BTreeMap::new(),
    };
    mapper.seed_plans(&ir, &mut state, seed, observer)?;
    mapper.run_epochs(&mut ir, &mut state, observer)
}

/// Resolves frozen regional boundary values against the one mapped substrate.
fn resolve_boundary_nets(
    signals: &WordMappedSignals,
    boundary_values: &[BoundaryValueObservation],
) -> Result<Vec<crate::closure::BoundaryNetObservation>, crate::SynthError> {
    boundary_values
        .iter()
        .map(|&(semantic_key, ref bits)| {
            let nets = bits
                .iter()
                .copied()
                .map(|value| match signals.require(value)? {
                    MappedValueSignal::Net(net) => Ok(Some(net)),
                    MappedValueSignal::Constant(_) => Ok(None),
                })
                .collect::<Result<Box<[_]>, crate::SynthError>>()?;
            Ok(crate::closure::BoundaryNetObservation { semantic_key, nets })
        })
        .collect()
}

fn boundary_observation_values(
    regions: &crate::SynthesisRegionGraph,
    ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
) -> Result<Vec<BoundaryValueObservation>, crate::SynthError> {
    let mut values = std::collections::BTreeMap::new();
    for region in regions.regions() {
        for &port in regions
            .input_ports(*region)
            .iter()
            .chain(regions.output_ports(*region))
        {
            let port = regions.port(port).ok_or_else(|| {
                crate::SynthError::invariant("boundary observation references an unknown port")
            })?;
            let Some(lowered) = ownership.lowered_bits(port.value()) else {
                // Lowering can remove a source-level boundary entirely. It has
                // no mapped net to observe, so its retained local response
                // remains authoritative for this epoch.
                continue;
            };
            if lowered.len() != port.ty().width() as usize {
                return Err(crate::SynthError::invariant(format!(
                    "regional boundary {:?} lowered to {} bits instead of {}",
                    port.value(),
                    lowered.len(),
                    port.ty().width(),
                )));
            }
            match values.entry(port.semantic_key()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(lowered.to_vec().into_boxed_slice());
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if entry.get().as_ref() != lowered =>
                {
                    return Err(crate::SynthError::invariant(
                        "matching boundary semantic keys refer to different lowered bits",
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
    }
    Ok(values.into_iter().collect())
}

pub(super) fn empty_region_plan(
    region: crate::SynthesisRegion,
    context: crate::RegionContextKey,
    contracts: &crate::regional::RegionContractSet,
    decision_key: [u8; 32],
) -> Result<crate::RegionCoverPlan, crate::SynthError> {
    let zero = crate::FiniteValue::new(0.0)
        .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
    Ok(crate::RegionCoverPlan::new(
        crate::RegionPlanIdentity {
            region: region.id(),
            revision: region.revision(),
            context_key: context,
        },
        crate::RegionPlanCost {
            legal: true,
            worst_normalized_violation: zero,
            minimum_slack: zero,
            total_negative_slack: zero,
            area: zero,
            leakage_power: None,
            dynamic_power: None,
            cell_count: 0,
            stable_plan_key: cover::empty_plan_key(region.id(), decision_key),
        },
        crate::RegionPlanSize {
            local_net_count: 0,
            local_cell_count: 0,
            local_pin_count: 0,
        },
        contracts.contracts(region.row()).to_vec(),
        Vec::new(),
    ))
}
