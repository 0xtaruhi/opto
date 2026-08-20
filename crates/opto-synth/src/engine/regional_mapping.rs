// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Regional mapping epoch orchestration and timing-feedback convergence.

use crate::SynthesisProgress;
use crate::artifact::{MappedCellSource, provenance::ProvenanceBuilder};
use crate::mapping::library::CombinationalCellCatalog;
use crate::mapping::materialize::region_delta::{
    MappedRegionArtifact, MappedRegionFootprint, MappedValueSignal, WordMappedSignals,
};
use crate::mapping::{self, MappingConfig, RegionPlanBinding, cover, materialize};
use opto_ir::word;
use opto_runtime::ExecutionContext;

mod census;
mod epochs;
mod objective;

use objective::{BestMapping, MappedObjective};

pub(crate) struct RegionalMappingOutcome {
    pub(crate) plans: Box<[crate::RegionCoverPlan]>,
    pub(crate) plan_journal: Box<[(crate::RegionRowId, crate::RegionCoverPlan)]>,
    pub(crate) epochs: usize,
    pub(crate) mapped: mapping::MappedOutput,
    pub(crate) timing: Option<crate::closure::mmmc::MmmcTiming>,
}

type BoundaryValueObservation = ([u8; 32], Box<[word::ValueId]>);

pub(crate) struct RegionalMappingRequest<'a> {
    pub(crate) module: &'a word::WordModule,
    pub(crate) provenance: &'a mut ProvenanceBuilder,
    pub(crate) regions: &'a crate::SynthesisRegionGraph,
    pub(crate) region_ownership: &'a crate::boolean::bitblast::LoweredRegionOwnership,
    pub(crate) contracts: &'a crate::regional::RegionContractSet,
    pub(crate) regional_plans: &'a [RegionalPlanRow],
    pub(crate) sequential_operations: &'a [materialize::FrozenSequentialOperation],
    pub(crate) config: MappingConfig<'a>,
}

/// Read-only context shared by every regional mapping epoch.
struct RegionalMapper<'a> {
    regions: &'a crate::SynthesisRegionGraph,
    response_models: cover::CoverResponseModels<'a>,
    config: MappingConfig<'a>,
    runtime: &'a ExecutionContext,
    trace: crate::api::diagnostics::SynthTrace,
}

/// The single mutable epoch state governed by one regional mapper.
///
/// Frozen Word semantics and ownership define the generation in which the
/// rows are valid. The provenance ledger, contracts, rows, and exploration
/// journal advance together under the mapper's serialized publication rules.
struct RegionalMappingState<'a> {
    module: &'a word::WordModule,
    provenance: &'a mut ProvenanceBuilder,
    region_ownership: &'a crate::boolean::bitblast::LoweredRegionOwnership,
    sequential_operations: &'a [materialize::FrozenSequentialOperation],
    contracts: crate::regional::RegionContractSet,
    rows: Vec<RegionalPlanRow>,
    plan_journal:
        std::collections::BTreeMap<(usize, crate::RegionContextKey), crate::RegionCoverPlan>,
}

/// One compact plan and its generation-local source binding.
#[derive(Clone)]
pub(super) struct RegionalPlanRow {
    pub(super) plan: crate::RegionCoverPlan,
    pub(super) binding: RegionPlanBinding,
}

impl RegionalMappingState<'_> {
    fn journal_compacted_plan(
        &mut self,
        row: usize,
        plan: &crate::RegionCoverPlan,
    ) -> Result<(), crate::SynthError> {
        let key = (row, plan.context_key());
        match self.plan_journal.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(plan.clone());
            }
            std::collections::btree_map::Entry::Occupied(entry)
                if entry.get().checkpoint_record() != plan.checkpoint_record() =>
            {
                return Err(crate::SynthError::invariant(
                    "one regional decision context compacted to different portable plans",
                ));
            }
            std::collections::btree_map::Entry::Occupied(_) => {}
        }
        Ok(())
    }

    fn take_plan_journal(
        &mut self,
    ) -> Result<Box<[(crate::RegionRowId, crate::RegionCoverPlan)]>, crate::SynthError> {
        std::mem::take(&mut self.plan_journal)
            .into_iter()
            .map(|((row, _), plan)| Ok((crate::RegionRowId::from_index(row)?, plan)))
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
    connectivity: materialize::FrozenObservableConnectivity,
    cell_sources: Vec<Option<MappedCellSource>>,
    implementation_census: Option<ImplementationCensus>,
    signals: WordMappedSignals,
    boundary_nets: Box<[crate::closure::BoundaryNetObservation]>,
    footprints: Vec<Option<MappedRegionFootprint>>,
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
        regional_plans,
        sequential_operations,
        config,
    } = request;
    if regional_plans.len() != regions.regions().len()
        || regional_plans
            .iter()
            .zip(regions.regions())
            .any(|(mapping, region)| mapping.plan.region() != region.id())
    {
        return Err(crate::SynthError::invariant(
            "regional mappings do not align with the region graph",
        ));
    }
    let mut state = RegionalMappingState {
        module,
        provenance,
        region_ownership,
        sequential_operations,
        contracts: contracts.clone(),
        rows: regional_plans.to_vec(),
        plan_journal: std::collections::BTreeMap::new(),
    };
    let trace =
        crate::api::diagnostics::SynthTrace::timing(config.mapping_context.config.diagnostics);
    let mapper = RegionalMapper {
        regions,
        response_models: cover::CoverResponseModels::new(config.scenarios),
        config,
        runtime,
        trace,
    };
    mapper.run_epochs(&mut state, observer)
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
