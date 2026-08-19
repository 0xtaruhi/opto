// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Publishes one regional construction for target-library mapping.

use super::roots::{MappingRoot, mapping_roots, merge_by_value};
use crate::artifact::provenance::{PrivateArchitecturePublication, ProvenanceBuilder};
use crate::boolean::bitblast::{
    LocalRegionBooleanLowering, LocalRegionBooleanRequest, LoweredRegionOwnership,
    implementation_providers, lower_local_region_boolean,
};
use crate::boolean::logic::RegionLogicOptions;
use crate::mapping::{CandidateBindingDomain, RegionPlanBinding, TargetMappingContext};
use crate::planning::operator::ArchitectureDecisions;
use crate::planning::regional::{
    MemoryImplementationCandidate, RegionalMemoryLogicBinding, RegionalMemoryStateBinding,
    RegionalWordCone, RegionalWordConeRequest,
};
use crate::regional::RegionContractSet;
use crate::{
    DurableOperatorArena, RegionContextKey, RegionCoverPlan, RegionRowId, SynthError,
    SynthesisOptions, SynthesisRegion, SynthesisRegionGraph,
};
use opto_ir::word;
use opto_runtime::{ExecutionContext, Task, TaskKey};
use std::collections::BTreeMap;

const REGIONAL_ARCHITECTURE_TASK_DOMAIN: u32 = 0x5245_4741;

struct RegionArchitectureMaterializer<'request, 'data> {
    request: &'request RegionalArchitectureRequest<'data>,
    semantics: &'request super::roots::FullDomainRootSemantics<'data>,
    roots: &'request [MappingRoot],
}

pub(crate) struct RegionalArchitectureRequest<'a> {
    pub(crate) source: &'a word::WordModule,
    pub(crate) operation_regions: &'a [Option<RegionRowId>],
    pub(crate) decisions: &'a [crate::incremental::RegionalCacheRecord],
    pub(crate) regions: &'a SynthesisRegionGraph,
    pub(crate) contracts: &'a RegionContractSet,
    pub(crate) options: &'a SynthesisOptions,
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
}

struct LoweredPrivateRegion {
    module: word::WordModule,
    source_to_local: BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: Box<[(word::ValueId, word::ValueId)]>,
    operation_sources: Vec<Option<word::OpId>>,
    owned_memory_logic: Vec<RegionalMemoryLogicBinding>,
    memory_states: Vec<RegionalMemoryStateBinding>,
    root_bindings: Box<[(word::ValueId, word::SignalId)]>,
    architecture: PrivateArchitecturePublication,
    operators: DurableOperatorArena,
    lowering: LocalRegionBooleanLowering,
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

fn remap_private_values(
    changes: &crate::planning::dataflow::DataflowChanges,
    source_to_local: &mut std::collections::BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: &mut [(word::ValueId, word::ValueId)],
    owned_memory_logic: &mut [RegionalMemoryLogicBinding],
    memory_states: &mut [RegionalMemoryStateBinding],
) {
    let representatives = changes.representatives();
    source_to_local
        .values_mut()
        .chain(boundary_bindings.iter_mut().map(|(_, local)| local))
        .for_each(|local| *local = representatives[local.index()]);
    for binding in owned_memory_logic {
        binding.local = representatives[binding.local.index()];
    }
    for binding in memory_states {
        binding.local = representatives[binding.local.index()];
    }
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
    if request.decisions.len() != request.regions.regions().len() {
        return Err(SynthError::invariant(
            "regional architecture plan does not align with the region graph",
        ));
    }
    let semantics = super::roots::FullDomainRootSemantics::new(request.source)?;
    let mut roots = mapping_roots(request.source, request.timing, request.port_bindings, None)?;
    for root in &mut roots {
        root.requires_combinational_cover = semantics.requires_artifact(root.value)?;
    }
    let materializer = RegionArchitectureMaterializer {
        request,
        semantics: &semantics,
        roots: &roots,
    };
    let lowering_work = request
        .regions
        .regions()
        .iter()
        .map(|&region| region.estimated_work().max(1))
        .collect::<Vec<_>>();
    let tasks: Vec<_> = (0..request.regions.regions().len())
        .map(|row| {
            Task::new(
                TaskKey::new(REGIONAL_ARCHITECTURE_TASK_DOMAIN, row as u64),
                row,
            )
            .with_estimated_work(lowering_work[row])
        })
        .collect();
    let profiling = request.mapping_context.config.diagnostics.timing;
    let mapped_regions = runtime.map_ordered_composite(tasks, |region_index, regional_runtime| {
        let _region_profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            format!("logic_lowering.region[{region_index}]")
        });
        let region = request.regions.regions()[region_index];
        let decision = &request.decisions[region_index];
        let memory_implementations = crate::planning::regional::decode_memory_implementations(
            decision.memory_implementations(),
        )?;
        let restored_plan = decision.restore_plan(
            region,
            request.contracts.contracts(region.row()),
        )?;
        let mapped = materializer.materialize(
            &memory_implementations,
            restored_plan,
            region,
            decision.context(),
            regional_runtime,
        )?;
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::new(self::diagnostics_enabled(&materializer)),
            "regional.architecture",
            "row={region_index} lowering_work={} nested_lanes={} area={:.4} cells={} violation={:.6} slack={:.4}",
            lowering_work[region_index],
            regional_runtime.parallelism(),
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
        let region = request.regions.regions()[row];
        if mapped.plan.region() != region.id() {
            return Err(SynthError::invariant(
                "regional architecture result belongs to another region",
            ));
        }
        let memories = request.regions.memories(region);
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
        operation_sources: &[Option<word::OpId>],
        source_to_local: &std::collections::BTreeMap<word::ValueId, word::ValueId>,
    ) -> Result<(PrivateArchitecturePublication, DurableOperatorArena), SynthError> {
        let sources = crate::artifact::provenance::resolve_private_operator_sources(
            self.request.source,
            module,
            decisions,
            self.request.regions.operations(region),
            operation_sources,
        )?;
        let architecture = PrivateArchitecturePublication::capture_resolved(
            self.request.source,
            decisions,
            region.id(),
            source_to_local,
            &sources,
        )?;
        let arena = DurableOperatorArena::capture(module, decisions, &sources, |operation| {
            self.request
                .regions
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
        memory_implementations: &[MemoryImplementationCandidate],
        restored_plan: Option<RegionCoverPlan>,
        region: SynthesisRegion,
        context: RegionContextKey,
        runtime: &ExecutionContext,
    ) -> Result<RegionalArchitectureMapping, SynthError> {
        let (private, root_pairs) = self.lower_private_region(memory_implementations, region)?;
        let PreparedRegionCover {
            slice,
            decision_key,
            publication,
        } = self.prepare_region_cover(&private, root_pairs, memory_implementations, region)?;
        let LoweredPrivateRegion {
            module,
            source_to_local,
            boundary_bindings,
            operation_sources,
            owned_memory_logic,
            memory_states,
            root_bindings,
            architecture,
            operators,
            lowering,
        } = private;
        let empty_port_bindings = opto_timing::PortBindings::new([]);
        let LocalRegionBooleanLowering { ownership, subject } = lowering;
        let analysis = super::cover::analyze_region_cover(
            &module,
            super::cover::RegionCoverRequest {
                roots: slice.roots(),
                timing: self.request.timing,
                port_bindings: &empty_port_bindings,
                catalog: &self.request.mapping_context.combinational_catalog,
                options: RegionLogicOptions {
                    optimize: self
                        .request
                        .mapping_context
                        .combinational_catalog
                        .can_invert(),
                    config: self.request.mapping_context.config,
                    runtime,
                    incremental: Some(crate::boolean::logic::RewriteIncremental::new(
                        self.request.rewrite_recipes,
                        self.request.incremental_metrics,
                    )),
                },
                regional_slice: &slice,
            },
            subject,
        )?;
        let response_models = super::cover::CoverResponseModels::new(self.request.scenarios);
        let (rematerialized, binding) = match analysis {
            super::cover::RegionCoverAnalysis::NoCombinationalLogic => (
                empty_target_plan(region, context, self.request.contracts, decision_key)?,
                RegionPlanBinding::empty(),
            ),
            super::cover::RegionCoverAnalysis::Covered(mut analysis) => {
                let binding = analysis.candidate_binding(
                    CandidateBindingDomain {
                        source_module: self.request.source,
                        local_module: &module,
                        source_to_local: &source_to_local,
                        boundary_bindings: &boundary_bindings,
                        owned_memory_logic: &owned_memory_logic,
                        memory_states: &memory_states,
                        operation_sources: &operation_sources,
                        root_bindings: &root_bindings,
                        ownership: &ownership,
                    },
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
        Ok(RegionalArchitectureMapping {
            plan,
            binding,
            architecture,
            operators,
            publication,
        })
    }

    fn lower_private_region(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<(LoweredPrivateRegion, Vec<(MappingRoot, word::ValueId)>), SynthError> {
        let (
            RegionalWordCone {
                mut module,
                source_to_local,
                boundary_bindings,
                operation_sources,
                owned_memory_logic,
                memory_states,
                root_bindings,
            },
            boundary_inputs,
            root_pairs,
        ) = self.prepare_private_word(memory_implementations, region)?;
        let operation_sources = operation_sources.into_vec();
        let owned_memory_logic = owned_memory_logic.into_vec();
        let memory_states = memory_states.into_vec();
        let mut provenance = ProvenanceBuilder::for_regional_candidate(&module);
        let local_root_values = root_pairs
            .iter()
            .map(|(_, local)| *local)
            .collect::<Vec<_>>();
        let mut tracked_values = boundary_inputs
            .iter()
            .chain(&local_root_values)
            .chain(boundary_bindings.iter().map(|(_, local)| local))
            .chain(owned_memory_logic.iter().map(|binding| &binding.local))
            .chain(memory_states.iter().map(|binding| &binding.local))
            .copied()
            .collect::<Vec<_>>();
        tracked_values.sort_unstable();
        tracked_values.dedup();
        let mut local_decisions = ArchitectureDecisions::for_private_region(
            &module,
            &tracked_values,
            implementation_providers().into(),
        )?;
        local_decisions.select_for_budget(
            self.request.target_model,
            self.request.contracts.delay_budget(region.row()),
        )?;
        let (architecture, operators) = self.prepare_operators(
            region,
            &module,
            &local_decisions,
            &operation_sources,
            &source_to_local,
        )?;
        let profiling = self.request.mapping_context.config.diagnostics.timing;
        let row = region.row().raw();
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
                    owner: region.row(),
                    boundary_inputs: &boundary_inputs,
                    roots: &local_root_values,
                    tracked_values: &tracked_values,
                },
            )
        }?;
        Ok((
            LoweredPrivateRegion {
                module,
                source_to_local,
                boundary_bindings,
                operation_sources,
                owned_memory_logic,
                memory_states,
                root_bindings,
                architecture,
                operators,
                lowering,
            },
            root_pairs,
        ))
    }

    fn prepare_region_cover(
        &self,
        private: &LoweredPrivateRegion,
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
            &private.lowering.ownership,
            &self
                .request
                .regions
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
            &private.lowering.ownership,
            &substrate_outputs,
            &mut root_pairs,
        )?;
        let sequential_timing = super::sequential::SequentialTimingProjection::build(
            &private.module,
            &self.request.mapping_context.sequential_catalog,
            &self.request.mapping_context.combinational_catalog,
        )?;
        let empty_port_bindings = opto_timing::PortBindings::new([]);
        let roots = mapping_roots(
            &private.module,
            self.request.timing,
            &empty_port_bindings,
            Some(&sequential_timing),
        )?;
        append_local_mapping_roots(
            &private.module,
            &local_semantics,
            &private.lowering.ownership,
            &substrate_outputs,
            roots,
            &mut root_pairs,
        )?;
        let root_pairs = merge_mapping_root_pairs(&private.module, &local_semantics, root_pairs)?;
        let decision_key = crate::planning::regional::decision_key(memory_implementations);
        let mut slice = super::logic_partition::RegionLogicSlice::build_candidate(
            region.id(),
            decision_key,
            super::logic_partition::RegionLogicDomain {
                module: &private.module,
                subject_inputs: &private.lowering.subject.inputs,
                source_to_local: &private.source_to_local,
                ownership: &private.lowering.ownership,
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
        slice.project_sequential_timing(&sequential_timing);
        Ok(PreparedRegionCover {
            slice,
            decision_key,
            publication,
        })
    }

    #[expect(
        clippy::type_complexity,
        reason = "the private tuple is consumed immediately; a stage carrier would add no owner or invariant"
    )]
    fn prepare_private_word(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<
        (
            RegionalWordCone,
            Vec<word::ValueId>,
            Vec<(MappingRoot, word::ValueId)>,
        ),
        SynthError,
    > {
        let memories = self.request.regions.memories(region);
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
        for flow in self.request.regions.bit_flows(region) {
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
            mut source_to_local,
            mut boundary_bindings,
            operation_sources,
            mut owned_memory_logic,
            mut memory_states,
            root_bindings,
        } = cone;
        let mut operation_sources = operation_sources.into_vec();
        let local_changes = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("regional_optimization.region[{row}].dataflow")
            });
            crate::planning::dataflow::canonicalize_combinational_dataflow(&mut module)?
        };
        remap_private_values(
            &local_changes,
            &mut source_to_local,
            &mut boundary_bindings,
            &mut owned_memory_logic,
            &mut memory_states,
        );
        if crate::planning::operator::share_muxed_arithmetic(&mut module)? != 0 {
            let local_changes =
                crate::planning::dataflow::canonicalize_combinational_dataflow(&mut module)?;
            remap_private_values(
                &local_changes,
                &mut source_to_local,
                &mut boundary_bindings,
                &mut owned_memory_logic,
                &mut memory_states,
            );
        }
        operation_sources.resize(module.operations().len(), None);
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::new(profiling),
            "regional.private_word",
            "row={row} operations={} roots={} constant_roots={}",
            module.operations().len(),
            regional_roots.len(),
            regional_roots
                .iter()
                .filter_map(|root| source_to_local.get(&root.value))
                .filter(|&&local| module
                    .value(local)
                    .is_some_and(|value| matches!(value.kind, word::ValueKind::Constant(_))))
                .count(),
        );
        let map_source = |value: &word::ValueId| {
            source_to_local.get(value).copied().ok_or_else(|| {
                SynthError::invariant(
                    "regional observable value is absent from its local Word cone",
                )
            })
        };
        let local_boundary_inputs = collect_local_boundary_inputs(&module)?;
        let root_pairs = regional_roots
            .iter()
            .map(|root| map_source(&root.value).map(|local| (*root, local)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            RegionalWordCone {
                module,
                source_to_local,
                boundary_bindings,
                operation_sources: operation_sources.into_boxed_slice(),
                owned_memory_logic,
                memory_states,
                root_bindings,
            },
            local_boundary_inputs,
            root_pairs,
        ))
    }
}

fn collect_local_boundary_inputs(
    module: &word::WordModule,
) -> Result<Vec<word::ValueId>, SynthError> {
    let mut inputs = Vec::new();
    for (index, stored) in module.values().iter().enumerate() {
        let word::ValueKind::Signal(reference) = stored.kind else {
            continue;
        };
        let signal = module.signal(reference.signal).ok_or_else(|| {
            SynthError::invariant("regional boundary value references an unknown signal")
        })?;
        let word::SignalKind::Port(port) = signal.kind else {
            continue;
        };
        let port = module.port(port).ok_or_else(|| {
            SynthError::invariant("regional boundary signal references an unknown port")
        })?;
        if matches!(
            port.direction,
            word::PortDirection::Input | word::PortDirection::Inout
        ) {
            inputs.push(word::ValueId::from_index(index).map_err(SynthError::from)?);
        }
    }
    Ok(inputs)
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
    ownership: &LoweredRegionOwnership,
    substrate_outputs: &std::collections::BTreeSet<MappingRootPairKey>,
    roots: &mut [(MappingRoot, word::ValueId)],
) -> Result<(), SynthError> {
    for (root, local) in roots {
        let bits = ownership
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
    ownership: &LoweredRegionOwnership,
    substrate_outputs: &std::collections::BTreeSet<MappingRootPairKey>,
    roots: Vec<MappingRoot>,
    root_pairs: &mut Vec<(MappingRoot, word::ValueId)>,
) -> Result<(), SynthError> {
    for mut root in roots {
        let bits = ownership
            .lowered_bits(root.value)
            .map_or_else(|| vec![root.value], <[word::ValueId]>::to_vec);
        let all_outputs_are_substrate = bits
            .into_iter()
            .map(|bit| mapping_root_pair_key(module, semantics, bit))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .all(|key| substrate_outputs.contains(&key));
        root.requires_combinational_cover =
            semantics.requires_artifact(root.value)? && !all_outputs_are_substrate;
        root_pairs.push((root, root.value));
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
    ownership: &LoweredRegionOwnership,
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
        let local_bits = ownership
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

pub(crate) fn extend_operation_regions_for_memories(
    module: &word::WordModule,
    original: &[Option<RegionRowId>],
    memory_regions: &[Option<RegionRowId>],
    memory_ownership: &crate::planning::memory::MemoryLoweringOwnership,
) -> Result<Vec<Option<RegionRowId>>, SynthError> {
    if original.len() > module.operations().len() {
        return Err(SynthError::invariant(
            "memory lowering removed source operations",
        ));
    }
    let mut owners = original.to_vec();
    owners.resize(module.operations().len(), None);
    for (operation, memory) in memory_ownership.operations() {
        let owner = memory_regions
            .get(memory.index())
            .copied()
            .flatten()
            .ok_or_else(|| SynthError::invariant("lowered memory has no synthesis-region owner"))?;
        let slot = owners.get_mut(operation.index()).ok_or_else(|| {
            SynthError::invariant("lowered memory operation is outside the Word arena")
        })?;
        if slot.replace(owner).is_some() {
            return Err(SynthError::invariant(
                "lowered memory operation already has a synthesis-region owner",
            ));
        }
    }
    for (value, memory) in memory_ownership.state_values() {
        let operation = match module.value(value).map(|stored| &stored.kind) {
            Some(word::ValueKind::Operation(operation)) => *operation,
            Some(word::ValueKind::Signal(_) | word::ValueKind::Constant(_)) | None => {
                return Err(SynthError::invariant(
                    "lowered memory state has no generating operation",
                ));
            }
        };
        let owner = memory_regions
            .get(memory.index())
            .copied()
            .flatten()
            .ok_or_else(|| SynthError::invariant("lowered memory has no synthesis-region owner"))?;
        let slot = owners.get_mut(operation.index()).ok_or_else(|| {
            SynthError::invariant("lowered memory state operation is outside the Word arena")
        })?;
        if slot.replace(owner).is_some() {
            return Err(SynthError::invariant(
                "lowered memory state already has a synthesis-region owner",
            ));
        }
    }
    Ok(owners)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_boundary_cast_is_not_a_hard_input() {
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

        assert_eq!(collect_local_boundary_inputs(&module).unwrap(), [signal]);
        assert!(
            !collect_local_boundary_inputs(&module)
                .unwrap()
                .contains(&cast)
        );
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
        let ownership = LoweredRegionOwnership::new(module.values().len());

        let expanded = expand_mapping_root_pairs(
            &module,
            &semantics,
            &ownership,
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
