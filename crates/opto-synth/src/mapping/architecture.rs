// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Publishes one regional construction for target-library mapping.

use super::roots::{MappingRoot, combinational_mapping_roots, mapping_roots, merge_by_value};
use crate::artifact::provenance::{PrivateArchitecturePublication, ProvenanceBuilder};
use crate::boolean::bitblast::{
    LocalRegionBooleanLowering, LocalRegionBooleanRequest, LoweredRegionBinding,
    implementation_providers, lower_local_region_boolean,
};
use crate::boolean::logic::{ChoiceDesign, ChoiceScopeId, ChoiceSubject, RegionLogicOptions};
use crate::mapping::{CandidateBindingDomain, RegionPlanBinding, TargetMappingContext};
use crate::planning::operator::ArchitectureDecisions;
use crate::planning::regional::{
    MemoryImplementationCandidate, RegionalMemoryLogicBinding, RegionalMemoryStateBinding,
    RegionalWordCone, RegionalWordConeRequest,
};
use crate::regional::RegionContractSet;
use crate::{
    DurableOperatorArena, RegionContextKey, RegionCoverPlan, RegionRowId, SynthError,
    SynthesisOptions, SynthesisRegion,
};
use opto_ir::word;
use opto_runtime::{ExecutionContext, Task, TaskKey};
use std::collections::BTreeMap;

const REGIONAL_MATERIALIZATION_TASK_DOMAIN: u32 = 0x4d41_544c;

struct RegionArchitectureMaterializer<'request, 'data> {
    request: &'request RegionalArchitectureRequest<'data>,
    semantics: &'request super::roots::FullDomainRootSemantics<'data>,
    roots: &'request [MappingRoot],
    source_cells: &'request BTreeMap<word::OpId, opto_ir::design::CellId>,
}

pub(crate) struct RegionalArchitectureRequest<'a> {
    pub(crate) source: &'a word::WordModule,
    pub(crate) operation_regions: &'a [Option<RegionRowId>],
    pub(crate) decisions: &'a [crate::incremental::RegionalCacheRecord],
    pub(crate) work: &'a crate::regional::WorkGraph,
    pub(crate) contracts: &'a RegionContractSet,
    pub(crate) options: &'a SynthesisOptions,
    pub(crate) clock_gating: Option<crate::ClockGatingStyle>,
    pub(crate) timing: &'a opto_timing::TimingContext,
    pub(crate) scenarios: &'a opto_timing::ScenarioSet,
    pub(crate) target_model: &'a crate::planning::regional::StructuralTargetModel,
    pub(crate) port_bindings: &'a opto_timing::PortBindings,
    pub(crate) mapping_context: &'a TargetMappingContext,
    pub(crate) rewrite_recipes: &'a crate::boolean::logic::RewriteRecipeCache,
    pub(crate) incremental_metrics: &'a crate::incremental::IncrementalRunMetrics,
}

pub(crate) struct RegionalArchitectureMapping {
    pub(crate) plan: RegionCoverPlan,
    pub(crate) binding: RegionPlanBinding,
    pub(crate) architecture: PrivateArchitecturePublication,
    pub(crate) operators: DurableOperatorArena,
    pub(crate) publication: Box<[crate::boolean::bitblast::RegionalPublicationBit]>,
    pub(crate) sequential: Box<[super::materialize::RegionalSequentialCellPlan]>,
    pub(crate) substrate: Box<[super::materialize::RegionalSubstrateCellPlan]>,
    pub(crate) proof: opto_ir::design::EquivalenceCertificate,
}

struct LoweredPrivateRegion {
    module: word::WordModule,
    values: PrivateValueBindings,
    root_bindings: Box<[(word::ValueId, word::SignalId)]>,
    architecture: PrivateArchitecturePublication,
    operators: DurableOperatorArena,
    lowered_binding: LoweredRegionBinding,
    state_operations: Box<[super::materialize::SequentialRegionBinding]>,
    mapping_roots: Box<[MappingRoot]>,
    sequential_timing: super::sequential::SequentialTimingProjection,
    substrate_instances: Box<[Box<str>]>,
}

struct PreparedRegionalChoice {
    private: LoweredPrivateRegion,
    cover: PreparedRegionCover,
}

struct LoweredRegionalChoice {
    private: LoweredPrivateRegion,
    root_pairs: Vec<(MappingRoot, word::ValueId)>,
    canonical: crate::boolean::logic::CanonicalRegionLogic,
}

struct RegionalMaterialization {
    restored_plan: Option<RegionCoverPlan>,
    region: SynthesisRegion,
    context: RegionContextKey,
    prepared: PreparedRegionalChoice,
    analysis: super::cover::RegionCoverAnalysis,
}

struct OptimizedPrivateRegion {
    module: word::WordModule,
    values: PrivateValueBindings,
    operation_sources: crate::planning::regional::LocalOperationSemantics,
    root_bindings: Box<[(word::ValueId, word::SignalId)]>,
    root_pairs: Vec<(MappingRoot, word::ValueId)>,
    state_relations: BTreeMap<word::OpId, [u8; 32]>,
    substrate_instances: Box<[Box<str>]>,
}

struct CharacterizedPrivateRegion {
    optimized: OptimizedPrivateRegion,
    decisions: ArchitectureDecisions,
}

struct RegionalCharacterization {
    memory_implementations: Box<[MemoryImplementationCandidate]>,
    restored_plan: Option<RegionCoverPlan>,
    context: RegionContextKey,
    private: CharacterizedPrivateRegion,
}

struct SelectedRegionalContext {
    memory_implementations: Box<[MemoryImplementationCandidate]>,
    restored_plan: Option<RegionCoverPlan>,
    context: RegionContextKey,
}

#[derive(Clone, Copy)]
struct PendingRegionalPublicationBit {
    target: word::ValueId,
    bit: u32,
    local: word::ValueId,
}

struct ExpandedMappingRoots {
    pairs: Vec<(MappingRoot, word::ValueId)>,
    publication: Box<[PendingRegionalPublicationBit]>,
}

struct PreparedRegionCover {
    slice: super::logic_partition::RegionLogicSlice,
    decision_key: [u8; 32],
    publication: Box<[crate::boolean::bitblast::RegionalPublicationBit]>,
}

struct PrivateValueBindings {
    source_to_local: BTreeMap<word::ValueId, word::ValueId>,
    boundary: Box<[(word::ValueId, word::ValueId)]>,
    memory_logic: Vec<RegionalMemoryLogicBinding>,
    memory_states: Vec<RegionalMemoryStateBinding>,
}

fn remap_private_values(
    changes: &crate::planning::dataflow::DataflowChanges,
    values: &mut PrivateValueBindings,
) {
    let representatives = changes.representatives();
    values
        .source_to_local
        .values_mut()
        .chain(values.boundary.iter_mut().map(|(_, local)| local))
        .for_each(|local| *local = representatives[local.index()]);
    for binding in &mut values.memory_logic {
        binding.local = representatives[binding.local.index()];
    }
    for binding in &mut values.memory_states {
        binding.local = representatives[binding.local.index()];
    }
}

fn commit_operation_rewrites(
    module: &mut word::WordModule,
    rewrites: &[crate::planning::operator::OperationRewrite],
    operation_sources: &mut crate::planning::regional::LocalOperationSemantics,
    values: &mut PrivateValueBindings,
) -> Result<(), SynthError> {
    if rewrites.is_empty() && module.validate().is_ok() {
        return Ok(());
    }
    let mut replacements = BTreeMap::new();
    for rewrite in rewrites {
        for &operation in &rewrite.replaced {
            let result = module
                .operation(operation)
                .ok_or_else(|| {
                    SynthError::invariant("SSA rewrite references an unknown operation")
                })?
                .result;
            replacements.insert(result, rewrite.replacement);
        }
    }
    let resolve = |mut value: word::ValueId| -> Result<word::ValueId, SynthError> {
        for _ in 0..=replacements.len() {
            let Some(&replacement) = replacements.get(&value) else {
                return Ok(value);
            };
            value = replacement;
        }
        Err(SynthError::invariant(
            "SSA rewrite side-database replacements contain a cycle",
        ))
    };
    for local in values
        .source_to_local
        .values_mut()
        .chain(values.boundary.iter_mut().map(|(_, local)| local))
    {
        *local = resolve(*local)?;
    }
    for binding in &mut values.memory_logic {
        binding.local = resolve(binding.local)?;
    }
    for binding in &mut values.memory_states {
        binding.local = resolve(binding.local)?;
    }
    let state_roots = module
        .operations()
        .iter()
        .filter_map(|operation| {
            matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            )
            .then_some(operation.result)
        })
        .collect::<Vec<_>>();
    compact_private_module(module, operation_sources, values, &state_roots)
}

fn compact_private_module(
    module: &mut word::WordModule,
    operation_sources: &mut crate::planning::regional::LocalOperationSemantics,
    values: &mut PrivateValueBindings,
    extra_roots: &[word::ValueId],
) -> Result<(), SynthError> {
    let roots = values
        .source_to_local
        .values()
        .copied()
        .chain(values.boundary.iter().map(|&(_, local)| local))
        .chain(values.memory_logic.iter().map(|binding| binding.local))
        .chain(values.memory_states.iter().map(|binding| binding.local))
        .chain(extra_roots.iter().copied())
        .collect::<Vec<_>>();
    let remap = module
        .compact_netlist_with_roots(&roots)
        .map_err(SynthError::from)?;
    let map = |value: &mut word::ValueId| -> Result<(), SynthError> {
        *value = remap.value(*value).ok_or_else(|| {
            SynthError::invariant("SSA transaction dropped a retained side-database value")
        })?;
        Ok(())
    };
    for local in values
        .source_to_local
        .values_mut()
        .chain(values.boundary.iter_mut().map(|(_, local)| local))
    {
        map(local)?;
    }
    for binding in &mut values.memory_logic {
        map(&mut binding.local)?;
    }
    for binding in &mut values.memory_states {
        map(&mut binding.local)?;
    }
    operation_sources.remap(&remap)?;
    module.validate().map_err(SynthError::from)
}

/// Builds every selected construction in a task-local Word module and publishes
/// only its portable plan and source binding.
#[expect(
    clippy::type_complexity,
    reason = "the two outputs are destructured once; a preparation carrier would add no owner or invariant"
)]
pub(crate) fn prepare_regional_architectures(
    request: &RegionalArchitectureRequest<'_>,
    runtime: &ExecutionContext,
) -> Result<
    (
        Box<[RegionalArchitectureMapping]>,
        Box<[Option<MemoryImplementationCandidate>]>,
    ),
    SynthError,
> {
    let regions = request.work.regions();
    if request.decisions.len() != regions.regions().len() {
        return Err(SynthError::invariant(
            "regional architecture plan does not align with the region graph",
        ));
    }
    let semantics = super::roots::FullDomainRootSemantics::new(request.source)?;
    let source_cells = request
        .work
        .regions()
        .regions()
        .iter()
        .flat_map(|&region| regions.operations(region).iter().copied())
        .map(|operation| {
            crate::regional::logical_operation_cell_id(regions, operation)
                .map(|cell| (operation, cell))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut roots = mapping_roots(request.source, request.timing, request.port_bindings, None)?;
    for root in &mut roots {
        root.requires_combinational_cover = semantics.requires_artifact(root.value)?;
    }
    let materializer = RegionArchitectureMaterializer {
        request,
        semantics: &semantics,
        roots: &roots,
        source_cells: &source_cells,
    };
    let work = request.work;
    let region_rows = regions
        .regions()
        .iter()
        .map(|region| (region.id(), region.row()))
        .collect::<BTreeMap<_, _>>();
    let profiling = request.mapping_context.config.diagnostics.timing;
    let results =
        crate::regional::SynthesisExecutor::execute(runtime, work, |item, regional_runtime| {
            let region_row = region_rows
                .get(&item.fixed_logic())
                .copied()
                .ok_or_else(|| {
                    SynthError::invariant("regional work item has no fixed-logic scope")
                })?;
            let region_index = region_row.index();
            let _region_profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("logic_lowering.region[{region_index}]")
            });
            let region = regions.regions()[region_index];
            let decision = &request.decisions[region_index];
            let memory_implementations = crate::planning::regional::decode_memory_implementations(
                decision.memory_implementations(),
            )?;
            let restored_plan =
                decision.restore_plan(region, request.contracts.contracts(region.row()))?;
            let private = materializer.characterize_private_region(
                &memory_implementations,
                region,
                regional_runtime,
            )?;
            Ok(crate::regional::WorkProduct::compiled_artifact(
                characterization_proof(region, &private.decisions),
                RegionalCharacterization {
                    memory_implementations,
                    restored_plan,
                    context: decision.context(),
                    private,
                },
            ))
        })?;
    let mut characterized = work
        .accept_results(results)?
        .into_vec()
        .into_iter()
        .map(|result| result.output)
        .collect::<Vec<_>>();
    let budgets = regions
        .regions()
        .iter()
        .map(|region| request.contracts.delay_budget(region.row()))
        .collect::<Vec<_>>();
    let mut decisions = characterized
        .iter_mut()
        .map(|candidate| &mut candidate.private.decisions)
        .collect::<Vec<_>>();
    ArchitectureDecisions::select_design_for_budgets(
        &mut decisions,
        request.target_model,
        &budgets,
        runtime,
    )?;
    let tasks = characterized
        .into_iter()
        .enumerate()
        .map(|(row, candidate)| {
            let region = regions.regions()[row];
            Task::new(
                TaskKey::new(REGIONAL_MATERIALIZATION_TASK_DOMAIN, row as u64),
                (region, candidate),
            )
            .with_estimated_work(region.estimated_work().max(1))
            .with_estimated_memory(region.estimated_work().max(1))
        })
        .collect();
    let prepared =
        runtime.map_ordered_composite(tasks, |(region, candidate), _regional_runtime| {
            let RegionalCharacterization {
                memory_implementations,
                restored_plan,
                context,
                private,
            } = candidate;
            let LoweredRegionalChoice {
                private,
                root_pairs,
                canonical,
            } = materializer.lower_characterized_region(private, region)?;
            let cover = materializer.prepare_region_cover(
                &private,
                &canonical.inputs,
                root_pairs,
                &memory_implementations,
                region,
            )?;
            let subject = ChoiceSubject {
                canonical,
                roots: cover.slice.roots().iter().map(|root| root.value).collect(),
                requirements: cover
                    .slice
                    .roots()
                    .iter()
                    .map(|root| root.required_time)
                    .collect(),
            };
            Ok::<_, SynthError>((
                SelectedRegionalContext {
                    memory_implementations,
                    restored_plan,
                    context,
                },
                PreparedRegionalChoice { private, cover },
                subject,
            ))
        })?;
    let (prepared, subjects): (Vec<_>, Vec<_>) = prepared
        .into_iter()
        .map(|(selection, prepared, subject)| ((selection, prepared), subject))
        .unzip();
    let choices = build_design_choices(subjects, request, runtime)?;
    let cover_scopes = prepared
        .iter()
        .enumerate()
        .map(|(row, (_, prepared))| {
            Ok(super::cover::DesignCoverScope {
                module: &prepared.private.module,
                roots: prepared.cover.slice.roots(),
                regional_slice: &prepared.cover.slice,
                scope: ChoiceScopeId::from_index(row)?,
            })
        })
        .collect::<Result<Vec<_>, SynthError>>()?;
    let cover_results = super::cover::analyze_design_cover(
        &choices,
        &cover_scopes,
        request.timing,
        &opto_timing::PortBindings::new([]),
        request.mapping_context,
        runtime,
    )?;
    let tasks = prepared
        .into_iter()
        .zip(cover_results)
        .enumerate()
        .map(|(row, (prepared, analysis))| {
            let region = regions.regions()[row];
            Task::new(
                TaskKey::new(REGIONAL_MATERIALIZATION_TASK_DOMAIN + 1, row as u64),
                (region, prepared, analysis),
            )
            .with_estimated_work(region.estimated_work().max(1))
            .with_estimated_memory(region.estimated_work().max(1))
        })
        .collect::<Vec<_>>();
    let mapped_regions = runtime.map_ordered_composite(tasks, |task, _regional_runtime| {
        let (region, (selection, prepared), cover_result) = task;
        let SelectedRegionalContext {
            memory_implementations,
            restored_plan,
            context,
        } = selection;
        let mapped = materializer.materialize(
            RegionalMaterialization {
                restored_plan,
                region,
                context,
                prepared,
                analysis: cover_result,
            },
        )?;
            crate::api::diagnostics::trace!(
                crate::api::diagnostics::SynthTrace::new(self::diagnostics_enabled(&materializer)),
                "regional.architecture",
                "row={} lowering_work={} nested_lanes={} area={:.4} cells={} violation={:.6} slack={:.4}",
                region.row().raw(),
                region.estimated_work().max(1),
                runtime.parallelism(),
                mapped.plan.cost().area.get(),
                mapped.plan.cost().cell_count,
                mapped.plan.cost().worst_normalized_violation.get(),
                mapped.plan.cost().minimum_slack.get(),
            );
        Ok::<_, SynthError>((memory_implementations, mapped))
    })?;
    let mut prepared_regions = Vec::with_capacity(mapped_regions.len());
    let mut selected_memories = vec![None; request.source.memories().len()];
    for (row, (memory_implementations, mapped)) in mapped_regions.into_iter().enumerate() {
        let region = regions.regions()[row];
        if mapped.plan.region() != region.id() {
            return Err(SynthError::invariant(
                "regional architecture result belongs to another region",
            ));
        }
        let memories = regions.memories(region);
        if memories.len() != memory_implementations.len() {
            return Err(SynthError::invariant(
                "selected memory implementations do not align with region ownership",
            ));
        }
        for (&memory, &implementation) in memories.iter().zip(&memory_implementations) {
            if selected_memories[memory.index()]
                .replace(implementation)
                .is_some()
            {
                return Err(SynthError::invariant(
                    "memory implementation is selected by more than one region",
                ));
            }
        }
        prepared_regions.push(mapped);
    }
    Ok((
        prepared_regions.into_boxed_slice(),
        selected_memories.into_boxed_slice(),
    ))
}

fn build_design_choices(
    subjects: Vec<ChoiceSubject>,
    request: &RegionalArchitectureRequest<'_>,
    runtime: &ExecutionContext,
) -> Result<ChoiceDesign, SynthError> {
    ChoiceDesign::from_subjects(
        subjects,
        RegionLogicOptions {
            optimize: request.mapping_context.combinational_catalog.can_invert(),
            config: request.mapping_context.config,
            runtime,
            incremental: Some(crate::boolean::logic::RewriteIncremental::new(
                request.rewrite_recipes,
                request.incremental_metrics,
            )),
        },
    )
}

fn characterization_proof(
    region: SynthesisRegion,
    decisions: &ArchitectureDecisions,
) -> opto_ir::design::EquivalenceCertificate {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/regional-characterization/v1\0");
    digest.update(&region.id().bytes());
    digest.update(&(decisions.operators().len() as u64).to_le_bytes());
    opto_ir::design::EquivalenceCertificate {
        regime: opto_ir::design::EquivalenceRegime::ByConstruction,
        digest: *digest.finalize().as_bytes(),
    }
}

fn diagnostics_enabled(materializer: &RegionArchitectureMaterializer<'_, '_>) -> bool {
    materializer
        .request
        .mapping_context
        .config
        .diagnostics
        .timing
}

impl RegionArchitectureMaterializer<'_, '_> {
    fn prepare_operators(
        &self,
        region: SynthesisRegion,
        module: &word::WordModule,
        decisions: &ArchitectureDecisions,
        operation_sources: &crate::planning::regional::LocalOperationSemantics,
    ) -> Result<(PrivateArchitecturePublication, DurableOperatorArena), SynthError> {
        let sources = crate::artifact::provenance::resolve_private_operator_sources(
            self.request.source,
            module,
            decisions,
            self.request.work.regions().operations(region),
            operation_sources,
        )?;
        let architecture = PrivateArchitecturePublication::capture_resolved(
            self.request.source,
            decisions,
            &sources,
        )?;
        let arena = DurableOperatorArena::capture(module, decisions, &sources, |operation| {
            self.request
                .work
                .regions()
                .operation_anchor(operation)
                .ok_or_else(|| {
                    SynthError::invariant("durable operator source has no stable occurrence anchor")
                })
        })?;
        Ok((architecture, arena))
    }

    fn endpoint_belongs_to_region(
        &self,
        value: word::ValueId,
        region: RegionRowId,
        memories: &[word::MemoryId],
    ) -> Result<bool, SynthError> {
        let stored = self.request.source.value(value).ok_or_else(|| {
            SynthError::invariant("regional root is absent from its source Word module")
        })?;
        match stored.kind {
            word::ValueKind::Signal(reference) => Ok(self
                .request
                .source
                .memory_read_ports()
                .iter()
                .any(|port| port.data == reference.signal && memories.contains(&port.memory))),
            word::ValueKind::Operation(operation) => self
                .request
                .operation_regions
                .get(operation.index())
                .copied()
                .flatten()
                .map_or(Ok(false), |owner| Ok(owner == region)),
            word::ValueKind::Constant(_) => Ok(false),
        }
    }

    fn materialize(
        &self,
        request: RegionalMaterialization,
    ) -> Result<RegionalArchitectureMapping, SynthError> {
        let RegionalMaterialization {
            restored_plan,
            region,
            context,
            prepared,
            analysis,
        } = request;
        let PreparedRegionalChoice { private, cover } = prepared;
        let PreparedRegionCover {
            slice,
            decision_key,
            publication,
        } = cover;
        let LoweredPrivateRegion {
            module,
            values,
            root_bindings,
            architecture,
            operators,
            lowered_binding,
            state_operations,
            mapping_roots: _,
            sequential_timing: _,
            substrate_instances,
        } = private;
        let response_models = super::cover::CoverResponseModels::new(self.request.scenarios);
        let domain = CandidateBindingDomain {
            source_module: self.request.source,
            local_module: &module,
            source_to_local: &values.source_to_local,
            boundary_bindings: &values.boundary,
            owned_memory_logic: &values.memory_logic,
            memory_states: &values.memory_states,
            sequential_operations: &state_operations,
            root_bindings: &root_bindings,
            region_binding: &lowered_binding,
            region: region.id(),
            target_cells: &self.request.options.target_cells,
            substrate_instances: &substrate_instances,
        };
        let (rematerialized, candidate) = match analysis {
            super::cover::RegionCoverAnalysis::NoCombinationalLogic => (
                empty_target_plan(region, context, self.request.contracts, decision_key)?,
                crate::mapping::build_candidate_binding(domain, &[], std::iter::empty())?,
            ),
            super::cover::RegionCoverAnalysis::Covered(mut analysis) => {
                let binding = analysis.candidate_binding(
                    domain,
                    &self.request.mapping_context.combinational_catalog,
                )?;
                let plan = analysis.compact_plan(
                    region,
                    context,
                    decision_key,
                    super::cover::CoverClosureDomain {
                        contracts: self.request.contracts.contracts(region.row()),
                        catalog: &self.request.mapping_context.combinational_catalog,
                        response_models: &response_models,
                        timing_tags: self.request.contracts.timing_tags(),
                        regional_slice: &slice,
                    },
                )?;
                (plan, binding)
            }
        };
        let plan = match restored_plan {
            Some(plan) if plan.matches_materialized_topology(&rematerialized) => plan,
            Some(_) => {
                return Err(SynthError::invariant(
                    "cached regional plan differs from the topology reconstructed by its context",
                ));
            }
            None => rematerialized,
        };
        let sequential = super::materialize::plan_regional_sequential_cells(
            &module,
            self.request.source,
            &state_operations,
            self.request.mapping_context,
            &candidate.endpoints,
        )?;
        let proof = regional_proof(&plan, &sequential);
        Ok(RegionalArchitectureMapping {
            plan,
            binding: candidate.binding,
            architecture,
            operators,
            publication,
            sequential,
            substrate: candidate.substrate,
            proof,
        })
    }

    fn lower_characterized_region(
        &self,
        private: CharacterizedPrivateRegion,
        region: SynthesisRegion,
    ) -> Result<LoweredRegionalChoice, SynthError> {
        let CharacterizedPrivateRegion {
            optimized,
            decisions: local_decisions,
        } = private;
        let OptimizedPrivateRegion {
            mut module,
            values,
            operation_sources,
            root_bindings,
            root_pairs,
            state_relations,
            substrate_instances,
        } = optimized;
        let boundary_inputs = frozen_boundary_inputs(&values.boundary);
        let mut provenance = ProvenanceBuilder::for_regional_candidate(&module);
        let local_root_values = root_pairs
            .iter()
            .map(|(_, local)| *local)
            .collect::<Vec<_>>();
        let state_values = module
            .operations()
            .iter()
            .filter_map(|operation| {
                matches!(
                    operation.kind,
                    word::OpKind::Register(_) | word::OpKind::Latch(_)
                )
                .then_some(operation.result)
            })
            .collect::<Vec<_>>();
        let mut tracked_values = boundary_inputs
            .iter()
            .chain(&local_root_values)
            .chain(values.boundary.iter().map(|(_, local)| local))
            .chain(values.memory_logic.iter().map(|binding| &binding.local))
            .chain(values.memory_states.iter().map(|binding| &binding.local))
            .copied()
            .collect::<Vec<_>>();
        tracked_values.sort_unstable();
        tracked_values.dedup();
        let profiling = self.request.mapping_context.config.diagnostics.timing;
        let row = region.row().raw();
        let (architecture, operators) = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("logic_lowering.region[{row}].operator_provenance")
            });
            self.prepare_operators(region, &module, &local_decisions, &operation_sources)?
        };
        let source_sequential = super::materialize::local_sequential_bindings(
            &module,
            self.request.source,
            region.id(),
            &operation_sources,
            self.source_cells,
            &state_relations,
        )?;
        let state_binding = crate::boolean::bitblast::lower_private_word_values(
            &mut module,
            &local_decisions,
            &mut provenance,
            region.row(),
            &state_values,
        )?;
        let lowered_sequential = super::materialize::lowered_sequential_operations(
            &module,
            &state_binding,
            &source_sequential,
        )?;
        for state in &lowered_sequential {
            let operation = module.operation(state.operation).ok_or_else(|| {
                SynthError::invariant("private scalar state disappeared before Boolean lowering")
            })?;
            tracked_values.push(operation.result);
            tracked_values.extend(crate::word::operation_inputs(&operation.kind));
            tracked_values.extend(
                state.lowering_sources.iter().filter_map(|&source| {
                    module.operation(source).map(|operation| operation.result)
                }),
            );
        }
        let sequential_timing = super::sequential::SequentialTimingProjection::build(
            &module,
            &self.request.mapping_context.sequential_catalog,
            &self.request.mapping_context.combinational_catalog,
        )?;
        let empty_port_bindings = opto_timing::PortBindings::new([]);
        let mut mapping_roots = combinational_mapping_roots(
            &module,
            self.request.timing,
            &empty_port_bindings,
            Some(&sequential_timing),
        )?;
        mapping_roots.extend(super::roots::state_mapping_roots(
            &module,
            lowered_sequential.iter().map(|state| state.operation),
            self.request.timing,
            &empty_port_bindings,
            Some(&sequential_timing),
        )?);
        let mapping_roots = merge_by_value(mapping_roots);
        tracked_values.extend(mapping_roots.iter().map(|root| root.value));
        tracked_values.sort_unstable();
        tracked_values.dedup();
        let lowering = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("logic_lowering.region[{row}].bitblast")
            });
            lower_local_region_boolean(
                &mut module,
                LocalRegionBooleanRequest {
                    plan: &local_decisions,
                    operators: &operators,
                    provenance: &mut provenance,
                    region: region.row(),
                    boundary_inputs: &boundary_inputs,
                    roots: &local_root_values,
                    tracked_values: &tracked_values,
                },
            )
        }?;
        let LocalRegionBooleanLowering {
            binding: lowered_binding,
            subject,
        } = lowering;
        Ok(LoweredRegionalChoice {
            private: LoweredPrivateRegion {
                module,
                values,
                root_bindings,
                architecture,
                operators,
                lowered_binding,
                state_operations: lowered_sequential,
                mapping_roots: mapping_roots.into_boxed_slice(),
                sequential_timing,
                substrate_instances,
            },
            root_pairs,
            canonical: subject,
        })
    }

    fn characterize_private_region(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
        runtime: &ExecutionContext,
    ) -> Result<CharacterizedPrivateRegion, SynthError> {
        let optimized = self.optimize_private_region(memory_implementations, region, runtime)?;
        let mut tracked_values = frozen_boundary_inputs(&optimized.values.boundary)
            .into_iter()
            .chain(optimized.root_pairs.iter().map(|(_, local)| *local))
            .chain(optimized.values.boundary.iter().map(|(_, local)| *local))
            .chain(
                optimized
                    .values
                    .memory_logic
                    .iter()
                    .map(|binding| binding.local),
            )
            .chain(
                optimized
                    .values
                    .memory_states
                    .iter()
                    .map(|binding| binding.local),
            )
            .collect::<Vec<_>>();
        tracked_values.sort_unstable();
        tracked_values.dedup();
        let row = region.row().raw();
        let _profile = crate::api::diagnostics::ProfileSpan::new(
            self.request.mapping_context.config.diagnostics.timing,
            || format!("logic_lowering.region[{row}].architecture_candidates"),
        );
        let decisions = ArchitectureDecisions::for_private_region(
            &optimized.module,
            &tracked_values,
            implementation_providers().into(),
        )?;
        Ok(CharacterizedPrivateRegion {
            optimized,
            decisions,
        })
    }

    fn optimize_private_region(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
        runtime: &ExecutionContext,
    ) -> Result<OptimizedPrivateRegion, SynthError> {
        let (
            RegionalWordCone {
                mut module,
                source_to_local,
                boundary_bindings,
                mut operation_sources,
                owned_memory_logic,
                memory_states,
                root_bindings,
            },
            mut root_pairs,
        ) = self.prepare_private_word(memory_implementations, region)?;
        let mut values = PrivateValueBindings {
            source_to_local,
            boundary: boundary_bindings,
            memory_logic: owned_memory_logic.into_vec(),
            memory_states: memory_states.into_vec(),
        };
        let fsm = crate::planning::fsm::optimize_derived_fsms(
            &mut module,
            self.request.timing,
            &opto_timing::PortBindings::new([]),
            runtime,
        )?;
        operation_sources.inherit_appended(&module)?;
        let mut state_relations = BTreeMap::new();
        for rewrite in fsm.rewrites() {
            let sources = operation_sources
                .sources(rewrite.replaced)
                .ok_or_else(|| SynthError::invariant("replaced FSM has no source relation"))?
                .to_vec();
            for &source in &sources {
                if state_relations
                    .insert(source, rewrite.state_relation)
                    .is_some_and(|proof| proof != rewrite.state_relation)
                {
                    return Err(SynthError::invariant(
                        "source state has conflicting FSM relations",
                    ));
                }
            }
            operation_sources.replace_from(rewrite.replacement, rewrite.replaced)?;
        }
        let canonical = crate::planning::dataflow::optimize_combinational_dataflow(&mut module)?;
        remap_private_values(&canonical, &mut values);
        operation_sources.inherit_appended(&module)?;
        let shareable =
            crate::planning::dataflow::shareable_sequential_operations(self.request.source)?
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
        let state_candidates = module
            .operations()
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| {
                matches!(
                    operation.kind,
                    word::OpKind::Register(_) | word::OpKind::Latch(_)
                )
                .then_some(index)
            })
            .map(|index| word::OpId::from_index(index).map_err(SynthError::from))
            .filter_map(|operation| match operation {
                Ok(operation)
                    if operation_sources.sources(operation).is_some_and(|sources| {
                        !sources.is_empty()
                            && sources.iter().all(|source| shareable.contains(source))
                    }) =>
                {
                    Some(Ok(operation))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let sharing = crate::planning::dataflow::share_equivalent_sequential_values_by(
            &mut module,
            &state_candidates,
            runtime,
            |value| canonical.representatives()[value.index()],
        )?;
        for &operation in &state_candidates {
            let result = module
                .operation(operation)
                .ok_or_else(|| SynthError::invariant("private state candidate disappeared"))?
                .result;
            let representative = sharing.representatives()[result.index()];
            if representative == result {
                continue;
            }
            let representative = module
                .value(representative)
                .and_then(|value| match value.kind {
                    word::ValueKind::Operation(operation) => Some(operation),
                    word::ValueKind::Constant(_) | word::ValueKind::Signal(_) => None,
                })
                .ok_or_else(|| SynthError::invariant("shared state has no representative"))?;
            operation_sources.merge_from(representative, operation)?;
        }
        remap_private_values(&sharing, &mut values);
        let observability = crate::word::uses::netlist_observability(&module)?;
        let state_roots = module
            .operations()
            .iter()
            .filter(|operation| {
                matches!(
                    operation.kind,
                    word::OpKind::Register(_) | word::OpKind::Latch(_)
                )
            })
            .filter_map(
                |operation| match observability.observes_value(operation.result) {
                    Ok(true) => Some(Ok(sharing.representatives()[operation.result.index()])),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                },
            )
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?
            .into_iter()
            .collect::<Vec<_>>();
        compact_private_module(
            &mut module,
            &mut operation_sources,
            &mut values,
            &state_roots,
        )?;
        for (root, local) in &mut root_pairs {
            *local = values
                .source_to_local
                .get(&root.value)
                .copied()
                .ok_or_else(|| {
                    SynthError::invariant("private root disappeared after local optimization")
                })?;
        }
        let state_feedback = private_state_feedback(&module, &operation_sources)?;
        let first_generated_instance = module.instances().len();
        self.request.mapping_context.prepare_private_structure(
            &mut module,
            &state_feedback,
            self.request.clock_gating,
            true,
        )?;
        let substrate_instances = module.instances()[first_generated_instance..]
            .iter()
            .map(|instance| module.name_str(instance.name).into())
            .collect();
        operation_sources.inherit_appended(&module)?;
        Ok(OptimizedPrivateRegion {
            module,
            values,
            operation_sources,
            root_bindings,
            root_pairs,
            state_relations,
            substrate_instances,
        })
    }

    fn prepare_region_cover(
        &self,
        private: &LoweredPrivateRegion,
        subject_inputs: &[word::ValueId],
        root_pairs: Vec<(MappingRoot, word::ValueId)>,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<PreparedRegionCover, SynthError> {
        let ExpandedMappingRoots {
            pairs: mut root_pairs,
            publication: pending_publication,
        } = expand_mapping_root_pairs(
            self.request.source,
            self.semantics,
            &private.lowered_binding,
            &self
                .request
                .work
                .regions()
                .bit_flows(region)
                .iter()
                .filter(|flow| {
                    self.request
                        .source
                        .value(flow.value())
                        .is_some_and(|stored| matches!(stored.kind, word::ValueKind::Operation(_)))
                })
                .map(|flow| (flow.value(), flow.bit()))
                .collect(),
            root_pairs,
        )?;
        let local_semantics = super::roots::FullDomainRootSemantics::new(&private.module)?;
        let substrate_outputs = target_output_artifact_keys(
            &private.module,
            &self.request.options.target_cells,
            &local_semantics,
        )?;
        suppress_substrate_roots(
            &private.module,
            &local_semantics,
            &private.lowered_binding,
            &substrate_outputs,
            &mut root_pairs,
        )?;
        append_local_mapping_roots(
            &private.module,
            &local_semantics,
            &private.lowered_binding,
            &substrate_outputs,
            private.mapping_roots.iter().copied(),
            &mut root_pairs,
        )?;
        let root_pairs = merge_mapping_root_pairs(&private.module, &local_semantics, root_pairs)?;
        let decision_key = crate::planning::regional::decision_key(memory_implementations);
        let mut slice = super::logic_partition::RegionLogicSlice::build_candidate(
            region.id(),
            decision_key,
            super::logic_partition::RegionLogicDomain {
                module: &private.module,
                subject_inputs,
                source_to_local: &private.values.source_to_local,
                region_binding: &private.lowered_binding,
                contracts: self.request.contracts.contracts(region.row()),
                roots: &root_pairs,
            },
        )?;
        let publication = finalize_regional_publication(
            &private.module,
            slice.roots(),
            &pending_publication,
            region.row(),
        )?;
        slice.project_sequential_timing(&private.sequential_timing);
        Ok(PreparedRegionCover {
            slice,
            decision_key,
            publication,
        })
    }

    fn prepare_private_word(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<(RegionalWordCone, Vec<(MappingRoot, word::ValueId)>), SynthError> {
        let memories = self.request.work.regions().memories(region);
        if memory_implementations.len() != memories.len() {
            return Err(SynthError::invariant(
                "regional memory decision does not match region ownership",
            ));
        }
        let mut regional_observations = Vec::new();
        let mut regional_roots = Vec::new();
        for &root in self.roots {
            let mut producers = std::collections::BTreeSet::new();
            for value in canonical_root_producers(self.request.source, self.semantics, root.value)?
            {
                if self.endpoint_belongs_to_region(value, region.row(), memories)? {
                    producers.insert(value);
                }
            }
            for value in producers {
                regional_observations.push(value);
                regional_roots.push(MappingRoot {
                    value,
                    requires_combinational_cover: self.semantics.requires_artifact(value)?,
                    ..root
                });
            }
        }
        // Packed boundary ports retain source-level identity for timing and
        // diagnostics, but they are not implementation dependencies. A
        // projection can span owners even though every one of its bits has an
        // unambiguous producer. Import only the frozen bit-flow endpoints so a
        // wrapper operation can never become a foreign regional input.
        for flow in self.request.work.regions().bit_flows(region) {
            if self
                .semantics
                .bit_requires_artifact(flow.value(), flow.bit())?
            {
                regional_observations.push(flow.value());
                regional_roots.push(MappingRoot {
                    value: flow.value(),
                    required_time: None,
                    output_load: None,
                    requires_combinational_cover: true,
                });
            }
        }
        let regional_roots = merge_by_value(regional_roots);
        let profiling = self.request.mapping_context.config.diagnostics.timing;
        let row = region.row().raw();
        let cone = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("regional_lowering.region[{row}].word_cone")
            });
            RegionalWordCone::build(RegionalWordConeRequest {
                source: self.request.source,
                operation_regions: self.request.operation_regions,
                region: region.row(),
                memories,
                memory_implementations,
                target_cells: &self.request.options.target_cells,
                observations: regional_observations,
                roots: regional_roots.iter().map(|root| root.value).collect(),
            })
        }?;
        let RegionalWordCone {
            mut module,
            source_to_local,
            boundary_bindings,
            operation_sources,
            owned_memory_logic,
            memory_states,
            root_bindings,
        } = cone;
        let mut operation_sources = operation_sources;
        let mut values = PrivateValueBindings {
            source_to_local,
            boundary: boundary_bindings,
            memory_logic: owned_memory_logic.into_vec(),
            memory_states: memory_states.into_vec(),
        };
        let local_changes = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("regional_optimization.region[{row}].dataflow")
            });
            crate::planning::dataflow::canonicalize_combinational_dataflow(&mut module)?
        };
        remap_private_values(&local_changes, &mut values);
        let rewrites = crate::planning::operator::share_muxed_arithmetic(&mut module)?;
        operation_sources.apply_rewrites(&module, &rewrites)?;
        commit_operation_rewrites(&mut module, &rewrites, &mut operation_sources, &mut values)?;
        if !rewrites.is_empty() {
            let local_changes =
                crate::planning::dataflow::canonicalize_combinational_dataflow(&mut module)?;
            remap_private_values(&local_changes, &mut values);
            commit_operation_rewrites(&mut module, &[], &mut operation_sources, &mut values)?;
        }
        operation_sources.inherit_appended(&module)?;
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::new(profiling),
            "regional.private_word",
            "row={row} operations={} roots={} constant_roots={}",
            module.operations().len(),
            regional_roots.len(),
            regional_roots
                .iter()
                .filter_map(|root| values.source_to_local.get(&root.value))
                .filter(|&&local| module
                    .value(local)
                    .is_some_and(|value| matches!(value.kind, word::ValueKind::Constant(_))))
                .count(),
        );
        let map_source = |value: &word::ValueId| {
            values.source_to_local.get(value).copied().ok_or_else(|| {
                SynthError::invariant(
                    "regional observable value is absent from its local Word cone",
                )
            })
        };
        let root_pairs = regional_roots
            .iter()
            .map(|root| map_source(&root.value).map(|local| (*root, local)))
            .collect::<Result<Vec<_>, _>>()?;
        let PrivateValueBindings {
            source_to_local,
            boundary: boundary_bindings,
            memory_logic,
            memory_states,
        } = values;
        Ok((
            RegionalWordCone {
                module,
                source_to_local,
                boundary_bindings,
                operation_sources,
                owned_memory_logic: memory_logic.into_boxed_slice(),
                memory_states: memory_states.into_boxed_slice(),
                root_bindings,
            },
            root_pairs,
        ))
    }
}

/// Resolves each private state operation to the structural read of the wire it
/// drives. This relation remains exact after FSM re-encoding and sequential
/// sharing, while source semantic rows may intentionally contain states with a
/// different representation width.
fn private_state_feedback(
    module: &word::WordModule,
    operation_sources: &crate::planning::regional::LocalOperationSemantics,
) -> Result<BTreeMap<word::OpId, word::ValueId>, SynthError> {
    let mut whole_generated_reads = BTreeMap::new();
    for (index, value) in module.values().iter().enumerate() {
        let word::ValueKind::Signal(reference) = value.kind else {
            continue;
        };
        let Some(signal) = module.signal(reference.signal) else {
            return Err(SynthError::invariant(
                "private state feedback read references an unknown signal",
            ));
        };
        if signal.name.is_none()
            && matches!(signal.kind, word::SignalKind::Wire)
            && reference.lsb == 0
            && reference.width() == signal.ty.width()
        {
            whole_generated_reads
                .entry(reference.signal)
                .or_insert(word::ValueId::from_index(index).map_err(SynthError::from)?);
        }
    }

    let mut feedback = BTreeMap::new();
    for (index, operation) in module.operations().iter().enumerate() {
        if !matches!(
            operation.kind,
            word::OpKind::Register(_) | word::OpKind::Latch(_)
        ) {
            continue;
        }
        let local = word::OpId::from_index(index).map_err(SynthError::from)?;
        let states = operation_sources
            .states(local)
            .ok_or_else(|| SynthError::invariant("private state has no semantic binding"))?;
        if states.is_empty() {
            return Err(SynthError::invariant(
                "private state has an empty semantic binding",
            ));
        }
        let memory_state = states.iter().any(|state| {
            matches!(
                state,
                crate::planning::regional::LocalStateSource::Memory { .. }
            )
        });
        if memory_state {
            if !states.iter().all(|state| {
                matches!(
                    state,
                    crate::planning::regional::LocalStateSource::Memory { .. }
                )
            }) {
                return Err(SynthError::invariant(
                    "private state mixes memory and operation semantics",
                ));
            }
            feedback.insert(local, operation.result);
            continue;
        }
        let held = module
            .connects()
            .iter()
            .filter(|connect| {
                connect.value == operation.result
                    && connect.target.range.is_none()
                    && connect.target.dynamic.is_none()
            })
            .find_map(|connect| whole_generated_reads.get(&connect.target.signal).copied())
            .ok_or_else(|| {
                SynthError::invariant(format!(
                    "private state {local:?} has no exact feedback boundary"
                ))
            })?;
        let held_ty = module
            .value(held)
            .ok_or_else(|| SynthError::invariant("private state feedback value is not live"))?
            .ty;
        let result_ty = module
            .value(operation.result)
            .ok_or_else(|| SynthError::invariant("private state result is not live"))?
            .ty;
        if held_ty != result_ty {
            return Err(SynthError::invariant(format!(
                "private state {local:?} feedback type {held_ty:?} does not match result type {result_ty:?}"
            )));
        }
        feedback.insert(local, held);
    }
    Ok(feedback)
}

pub(crate) fn regional_proof(
    plan: &RegionCoverPlan,
    sequential: &[super::materialize::RegionalSequentialCellPlan],
) -> opto_ir::design::EquivalenceCertificate {
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/regional-work-proof/v1\0");
    digest.update(&plan.region().bytes());
    digest.update(&plan.revision().bytes());
    digest.update(&plan.context_key().bytes());
    digest.update(&(plan.payload().len() as u64).to_le_bytes());
    digest.update(plan.payload());
    let mut relations = sequential
        .iter()
        .flat_map(|cell| cell.sources.iter())
        .filter_map(|source| source.state_relation)
        .collect::<Vec<_>>();
    relations.sort_unstable();
    relations.dedup();
    for relation in &relations {
        digest.update(relation);
    }
    opto_ir::design::EquivalenceCertificate {
        regime: if relations.is_empty() {
            opto_ir::design::EquivalenceRegime::ByConstruction
        } else {
            opto_ir::design::EquivalenceRegime::Sequential
        },
        digest: *digest.finalize().as_bytes(),
    }
}

fn frozen_boundary_inputs(bindings: &[(word::ValueId, word::ValueId)]) -> Vec<word::ValueId> {
    let mut inputs = bindings.iter().map(|&(_, local)| local).collect::<Vec<_>>();
    inputs.sort_unstable();
    inputs.dedup();
    inputs
}

fn canonical_root_producers(
    module: &word::WordModule,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
    root: word::ValueId,
) -> Result<std::collections::BTreeSet<word::ValueId>, SynthError> {
    let width = module
        .value(root)
        .ok_or_else(|| SynthError::invariant("mapping root is absent from its source Word module"))?
        .ty
        .width();
    (0..width).try_fold(std::collections::BTreeSet::new(), |mut producers, bit| {
        if let super::roots::CanonicalPublicationBit::Value { value, .. } =
            semantics.canonical_publication_bit(root, bit)?
        {
            producers.insert(value);
        }
        Ok(producers)
    })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MappingRootPairKey {
    Signal(word::SignalId, u32, u32),
    Value(word::ValueId),
}

fn mapping_root_pair_key(
    module: &word::WordModule,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
    value: word::ValueId,
) -> Result<MappingRootPairKey, SynthError> {
    let value = semantics.canonical_root(value)?;
    Ok(match module.value(value).map(|value| &value.kind) {
        Some(word::ValueKind::Signal(reference)) => {
            MappingRootPairKey::Signal(reference.signal, reference.lsb, reference.width())
        }
        Some(word::ValueKind::Operation(_) | word::ValueKind::Constant(_)) | None => {
            MappingRootPairKey::Value(value)
        }
    })
}

fn suppress_substrate_roots(
    module: &word::WordModule,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
    binding: &LoweredRegionBinding,
    substrate_outputs: &std::collections::BTreeSet<MappingRootPairKey>,
    roots: &mut [(MappingRoot, word::ValueId)],
) -> Result<(), SynthError> {
    for (root, local) in roots {
        let bits = binding
            .lowered_bits(*local)
            .map_or_else(|| vec![*local], <[word::ValueId]>::to_vec);
        let mut all_outputs_are_substrate = !bits.is_empty();
        for bit in bits {
            let key = mapping_root_pair_key(module, semantics, bit)?;
            all_outputs_are_substrate &= substrate_outputs.contains(&key);
        }
        if all_outputs_are_substrate {
            root.requires_combinational_cover = false;
        }
    }
    Ok(())
}

fn append_local_mapping_roots(
    module: &word::WordModule,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
    binding: &LoweredRegionBinding,
    substrate_outputs: &std::collections::BTreeSet<MappingRootPairKey>,
    roots: impl IntoIterator<Item = MappingRoot>,
    root_pairs: &mut Vec<(MappingRoot, word::ValueId)>,
) -> Result<(), SynthError> {
    for root in roots {
        let bits = binding
            .lowered_bits(root.value)
            .map_or_else(|| vec![root.value], <[word::ValueId]>::to_vec);
        for bit in bits {
            let requires_combinational_cover = semantics.requires_artifact(bit)?
                && !substrate_outputs.contains(&mapping_root_pair_key(module, semantics, bit)?);
            root_pairs.push((
                MappingRoot {
                    value: bit,
                    requires_combinational_cover,
                    ..root
                },
                bit,
            ));
        }
    }
    Ok(())
}

fn merge_mapping_root_pairs(
    module: &word::WordModule,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
    roots: Vec<(MappingRoot, word::ValueId)>,
) -> Result<Vec<(MappingRoot, word::ValueId)>, SynthError> {
    let mut roots = roots
        .into_iter()
        .map(|pair| Ok((mapping_root_pair_key(module, semantics, pair.1)?, pair)))
        .collect::<Result<Vec<_>, SynthError>>()?;
    roots.sort_by_key(|&(key, _)| key);
    let mut merged: Vec<(MappingRootPairKey, (MappingRoot, word::ValueId))> = Vec::new();
    for (key, next) in roots {
        if let Some((current_key, (current, _))) = merged.last_mut()
            && *current_key == key
        {
            current.required_time = match (current.required_time, next.0.required_time) {
                (Some(left), Some(right)) => Some(left.min(right)),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            };
            current.output_load = match (current.output_load, next.0.output_load) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (Some(value), None) | (None, Some(value)) => Some(value),
                (None, None) => None,
            };
            current.requires_combinational_cover |= next.0.requires_combinational_cover;
        } else {
            merged.push((key, next));
        }
    }
    Ok(merged.into_iter().map(|(_, pair)| pair).collect())
}

fn expand_mapping_root_pairs(
    source: &word::WordModule,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
    binding: &LoweredRegionBinding,
    publication_targets: &std::collections::BTreeSet<(word::ValueId, u32)>,
    roots: Vec<(MappingRoot, word::ValueId)>,
) -> Result<ExpandedMappingRoots, SynthError> {
    let mut expanded = Vec::new();
    let mut publication = Vec::new();
    let mut covered_publication_targets = std::collections::BTreeSet::new();
    let mut facts = word::KnownBitsAnalysis::new(source);
    for (root, local) in roots {
        let source_width = source
            .value(root.value)
            .ok_or_else(|| {
                SynthError::invariant("regional publication root is absent from its source module")
            })?
            .ty
            .width();
        let local_bits = binding
            .lowered_bits(local)
            .map_or_else(|| vec![local], <[word::ValueId]>::to_vec);
        if local_bits.len() != source_width as usize {
            return Err(SynthError::invariant(
                "regional publication root width differs after bit lowering",
            ));
        }
        for (bit, local) in local_bits.into_iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| SynthError::capacity("regional publication bit index"))?;
            let mut requires_artifact = semantics.bit_requires_artifact(root.value, bit)?;
            if let super::roots::CanonicalPublicationBit::Value {
                value: target,
                bit: target_bit,
            } = semantics.canonical_publication_bit(root.value, bit)?
                && source
                    .value(target)
                    .is_some_and(|stored| matches!(stored.kind, word::ValueKind::Operation(_)))
            {
                if publication_targets.contains(&(target, target_bit)) {
                    covered_publication_targets.insert((target, target_bit));
                }
                if facts.bit(source, target, target_bit) != word::KnownBit::Unknown {
                    requires_artifact = false;
                } else if requires_artifact && publication_targets.contains(&(target, target_bit)) {
                    publication.push(PendingRegionalPublicationBit {
                        target,
                        bit: target_bit,
                        local,
                    });
                }
            }
            expanded.push((
                MappingRoot {
                    requires_combinational_cover: requires_artifact,
                    ..root
                },
                local,
            ));
        }
    }
    for &(value, bit) in publication_targets {
        if semantics.bit_requires_artifact(value, bit)?
            && !covered_publication_targets.contains(&(value, bit))
        {
            return Err(SynthError::invariant(format!(
                "frozen regional bit flow {value:?}[{bit}] has no producer mapping root"
            )));
        }
    }
    Ok(ExpandedMappingRoots {
        pairs: expanded,
        publication: publication.into_boxed_slice(),
    })
}

fn finalize_regional_publication(
    local_module: &word::WordModule,
    roots: &[MappingRoot],
    pending: &[PendingRegionalPublicationBit],
    producer: RegionRowId,
) -> Result<Box<[crate::boolean::bitblast::RegionalPublicationBit]>, SynthError> {
    let semantics = super::roots::FullDomainRootSemantics::new(local_module)?;
    let mut cover_owners = BTreeMap::new();
    for root in roots {
        let local = semantics.canonical_root(root.value)?;
        cover_owners
            .entry(local)
            .and_modify(|required| *required |= root.requires_combinational_cover)
            .or_insert(root.requires_combinational_cover);
    }
    let mut publication = Vec::with_capacity(pending.len());
    for entry in pending {
        let local = semantics.canonical_root(entry.local)?;
        if !cover_owners.get(&local).copied().unwrap_or(false) {
            return Err(SynthError::invariant(format!(
                "full-domain regional publication {:?}[{}] lost its combinational cover root",
                entry.target, entry.bit,
            )));
        }
        publication.push(crate::boolean::bitblast::RegionalPublicationBit {
            target: entry.target,
            bit: entry.bit,
            producer,
        });
    }
    publication.sort_unstable_by_key(|entry| (entry.target, entry.bit));
    publication.dedup();
    Ok(publication.into_boxed_slice())
}

fn target_output_artifact_keys(
    module: &word::WordModule,
    target_cells: &opto_library::TargetCellSet,
    semantics: &super::roots::FullDomainRootSemantics<'_>,
) -> Result<std::collections::BTreeSet<MappingRootPairKey>, SynthError> {
    let mut outputs = std::collections::BTreeSet::new();
    for instance in module.instances() {
        let Some(cell) = target_cells
            .iter()
            .find(|cell| cell.name() == module.name_str(instance.module))
        else {
            continue;
        };
        for connection in &instance.connections {
            let pin = module.name_str(connection.port);
            let Some(pin) = cell.pins().find(|candidate| candidate.name() == pin) else {
                return Err(SynthError::invariant(format!(
                    "private target instance connects unknown pin '{pin}'"
                )));
            };
            if !matches!(
                pin.direction(),
                opto_library::TargetPinDirection::Output | opto_library::TargetPinDirection::Inout
            ) {
                continue;
            }
            for value in super::roots::scalar_value_parts(module, connection.value)? {
                outputs.insert(mapping_root_pair_key(module, semantics, value)?);
            }
        }
    }
    Ok(outputs)
}

fn empty_target_plan(
    region: SynthesisRegion,
    context: RegionContextKey,
    contracts: &RegionContractSet,
    decision_key: [u8; 32],
) -> Result<RegionCoverPlan, SynthError> {
    let zero =
        crate::FiniteValue::new(0.0).map_err(|error| SynthError::invariant(error.to_string()))?;
    Ok(RegionCoverPlan::new(
        region,
        context,
        crate::RegionPlanCost {
            legal: true,
            worst_normalized_violation: zero,
            minimum_slack: zero,
            total_negative_slack: zero,
            area: zero,
            leakage_power: None,
            dynamic_power: None,
            cell_count: 0,
            stable_plan_key: super::cover::empty_plan_key(region.id(), decision_key),
        },
        0,
        0,
        contracts.contracts(region.row()).to_vec(),
        Vec::new(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_state_feedback_uses_the_state_driven_generated_wire() {
        let mut module = word::WordModule::new("encoded_state_feedback");
        let span = word::SourceSpan::default();
        let bit = word::WordType::bits(1).unwrap();
        let original_ty = word::WordType::bits(2).unwrap();
        let original_data = module
            .constant(
                opto_ir::ConstBits::from_bin_str("00").unwrap(),
                original_ty,
                span.clone(),
            )
            .unwrap();
        let encoded_data = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                span.clone(),
            )
            .unwrap();
        let clock = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                span.clone(),
            )
            .unwrap();
        let original = module
            .register(
                word::RegisterOp {
                    name: None,
                    d: original_data,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                span.clone(),
            )
            .unwrap();
        let encoded = module
            .register(
                word::RegisterOp {
                    name: None,
                    d: encoded_data,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                span.clone(),
            )
            .unwrap();
        let original_wire = module
            .add_generated_wire(original_ty, span.clone())
            .unwrap();
        let original_feedback = module.read_signal(original_wire, span.clone()).unwrap();
        let encoded_wire = module.add_generated_wire(bit, span.clone()).unwrap();
        let encoded_feedback = module.read_signal(encoded_wire, span.clone()).unwrap();
        module
            .connect(word::LValue::signal(original_wire), original, span.clone())
            .unwrap();
        module
            .connect(word::LValue::signal(encoded_wire), encoded, span)
            .unwrap();

        let operation = |value| match module.value(value).unwrap().kind {
            word::ValueKind::Operation(operation) => operation,
            _ => unreachable!(),
        };
        let mut semantics = crate::planning::regional::LocalOperationSemantics::default();
        semantics
            .record_source(
                operation(original),
                word::OpId::from_index(4).unwrap(),
                true,
            )
            .unwrap();
        semantics
            .record_source(operation(encoded), word::OpId::from_index(5).unwrap(), true)
            .unwrap();
        let feedback = private_state_feedback(&module, &semantics).unwrap();
        assert_eq!(feedback[&operation(original)], original_feedback);
        assert_eq!(feedback[&operation(encoded)], encoded_feedback);
    }

    #[test]
    fn frozen_boundary_value_is_the_only_hard_input() {
        let mut module = word::WordModule::new("boundary_cast");
        let unsigned = word::WordType::bits(2).unwrap();
        let signed = word::WordType::new(2, true, word::LogicStateKind::FourState).unwrap();
        let port = module
            .add_port(
                "boundary",
                word::PortDirection::Input,
                unsigned,
                word::SourceSpan::default(),
            )
            .unwrap();
        let signal = module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let cast = module
            .cast(
                word::CastKind::SignExtend,
                signal,
                signed,
                word::SourceSpan::default(),
            )
            .unwrap();

        let source = word::ValueId::from_index(99).unwrap();
        assert_eq!(frozen_boundary_inputs(&[(source, cast)]), [cast]);
        assert!(!frozen_boundary_inputs(&[(source, cast)]).contains(&signal));
    }

    #[test]
    fn packed_root_publication_uses_bit_producers_not_the_wrapper_owner() {
        let mut module = word::WordModule::new("packed_root_producers");
        let ty = word::WordType::bits(1).unwrap();
        let span = word::SourceSpan::default();
        let port = module
            .add_port("a", word::PortDirection::Input, ty, span.clone())
            .unwrap();
        let input = module
            .read_signal(module.port(port).unwrap().signal, span.clone())
            .unwrap();
        let low = module
            .unary(word::UnaryOp::BitNot, input, span.clone())
            .unwrap();
        let high = module
            .unary(word::UnaryOp::LogicalNot, input, span.clone())
            .unwrap();
        let packed = module.concat(vec![high, low], span).unwrap();
        let semantics = super::super::roots::FullDomainRootSemantics::new(&module).unwrap();

        assert_eq!(
            canonical_root_producers(&module, &semantics, packed).unwrap(),
            [low, high].into_iter().collect()
        );
    }

    #[test]
    fn signal_roots_do_not_enter_the_operation_publication_contract() {
        let mut module = word::WordModule::new("signal_publication");
        let port = module
            .add_port(
                "a",
                word::PortDirection::Input,
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let value = module
            .read_signal(
                module.port(port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let semantics = super::super::roots::FullDomainRootSemantics::new(&module).unwrap();
        let binding = LoweredRegionBinding::new(module.values().len());

        let expanded = expand_mapping_root_pairs(
            &module,
            &semantics,
            &binding,
            &[(value, 0)].into_iter().collect(),
            vec![(
                MappingRoot {
                    value,
                    required_time: None,
                    output_load: None,
                    requires_combinational_cover: false,
                },
                value,
            )],
        )
        .unwrap();

        assert_eq!(expanded.pairs.len(), 1);
        assert!(expanded.publication.is_empty());
    }
}
