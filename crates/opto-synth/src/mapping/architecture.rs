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
use crate::mapping::{CandidateBindingInputs, RegionPlanBinding, TargetMappingContext};
use crate::planning::operator::ArchitectureDecisions;
use crate::planning::regional::{
    MemoryImplementationCandidate, RegionalMemoryValueBinding, RegionalWordCone,
    RegionalWordConeRequest,
};
use crate::regional::{RegionContractSet, RegionCoverPlanRecord};
use crate::{
    DurableOperatorArena, RegionBoundaryPort, RegionBoundaryPortId, RegionContextKey,
    RegionCoverPlan, RegionRowId, SynthError, SynthesisOptions, SynthesisRegion,
    SynthesisRegionGraph,
};
use opto_ir::word;
use opto_runtime::{ExecutionContext, Task, TaskKey};
use std::collections::BTreeMap;

const REGIONAL_ARCHITECTURE_TASK_DOMAIN: u32 = 0x5245_4741;

pub(crate) struct ArchitectureMappingConfig<'a> {
    pub(crate) options: &'a SynthesisOptions,
    pub(crate) timing: &'a opto_timing::TimingContext,
    pub(crate) scenarios: &'a opto_timing::ScenarioSet,
    pub(crate) target_model: &'a crate::planning::regional::StructuralTargetModel,
    pub(crate) port_bindings: &'a opto_timing::PortBindings,
    pub(crate) mapping_context: &'a TargetMappingContext,
    pub(crate) rewrite_recipes: &'a crate::boolean::logic::RewriteRecipeCache,
    pub(crate) incremental_metrics: &'a crate::incremental::IncrementalRunMetrics,
}

struct RegionArchitectureMaterializer<'a> {
    source: &'a word::WordModule,
    semantics: &'a super::roots::FullDomainRootSemantics<'a>,
    signal_drivers: crate::word::signal_driver::SignalDriverIndex,
    operation_regions: &'a [Option<RegionRowId>],
    regions: &'a SynthesisRegionGraph,
    contracts: &'a RegionContractSet,
    roots: &'a [MappingRoot],
    config: &'a ArchitectureMappingConfig<'a>,
}

pub(crate) struct RegionalArchitectureRequest<'a> {
    pub(crate) source: &'a word::WordModule,
    pub(crate) operation_regions: &'a [Option<RegionRowId>],
    pub(crate) decisions: &'a [crate::incremental::RegionalCacheRecord],
    pub(crate) regions: &'a SynthesisRegionGraph,
    pub(crate) contracts: &'a RegionContractSet,
    pub(crate) config: ArchitectureMappingConfig<'a>,
}

pub(crate) struct RegionalArchitecturePreparation {
    pub(crate) regions: Box<[RegionalArchitectureMapping]>,
    pub(crate) memories: Box<[MemoryImplementationCandidate]>,
}

pub(crate) struct RegionalArchitectureMapping {
    pub(crate) plan: RegionCoverPlan,
    pub(crate) binding: RegionPlanBinding,
    pub(crate) architecture: PrivateArchitecturePublication,
    pub(crate) operators: DurableOperatorArena,
}

struct PreparedPrivateWord {
    cone: RegionalWordCone,
    boundary_inputs: Vec<word::ValueId>,
    root_pairs: Vec<(MappingRoot, word::ValueId)>,
}

struct PreparedOperators {
    architecture: PrivateArchitecturePublication,
    operators: DurableOperatorArena,
}

struct LoweredPrivateRegion {
    module: word::WordModule,
    source_to_local: BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: Box<[(word::ValueId, word::ValueId)]>,
    operation_sources: Vec<Option<word::OpId>>,
    memory_values: Vec<RegionalMemoryValueBinding>,
    root_bindings: Box<[(word::ValueId, word::SignalId)]>,
    root_pairs: Vec<(MappingRoot, word::ValueId)>,
    architecture: PrivateArchitecturePublication,
    operators: DurableOperatorArena,
    lowering: LocalRegionBooleanLowering,
}

struct PreparedRegionCover {
    private: LoweredPrivateRegion,
    slice: super::logic_partition::RegionLogicSlice,
    decision_key: [u8; 32],
}

#[derive(Clone, Copy)]
struct CompactRegionalCoverInputs<'a> {
    region: SynthesisRegion,
    context: RegionContextKey,
    decision_key: [u8; 32],
    module: &'a word::WordModule,
    source_to_local: &'a BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: &'a [(word::ValueId, word::ValueId)],
    memory_values: &'a [RegionalMemoryValueBinding],
    operation_sources: &'a [Option<word::OpId>],
    root_bindings: &'a [(word::ValueId, word::SignalId)],
    ownership: &'a LoweredRegionOwnership,
    slice: &'a super::logic_partition::RegionLogicSlice,
}

fn remap_private_values(
    changes: &crate::planning::dataflow::DataflowChanges,
    source_to_local: &mut std::collections::BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: &mut [(word::ValueId, word::ValueId)],
    memory_values: &mut [RegionalMemoryValueBinding],
) {
    let representatives = changes.representatives();
    source_to_local
        .values_mut()
        .chain(boundary_bindings.iter_mut().map(|(_, local)| local))
        .for_each(|local| *local = representatives[local.index()]);
    for binding in memory_values {
        binding.local = representatives[binding.local.index()];
    }
}

/// Builds every selected construction in a task-local Word module and publishes
/// only its portable plan and source binding.
pub(crate) fn prepare_regional_architectures(
    request: RegionalArchitectureRequest<'_>,
    runtime: &ExecutionContext,
) -> Result<RegionalArchitecturePreparation, SynthError> {
    let RegionalArchitectureRequest {
        source,
        operation_regions,
        decisions,
        regions,
        contracts,
        config,
    } = request;
    if decisions.len() != regions.regions().len() {
        return Err(SynthError::invariant(
            "regional architecture plan does not align with the region graph",
        ));
    }
    let semantics = super::roots::FullDomainRootSemantics::new(source)?;
    let mut roots = mapping_roots(source, config.timing, config.port_bindings, None)?;
    for root in &mut roots {
        root.requires_combinational_cover = semantics.requires_artifact(root.value)?;
    }
    let materializer = RegionArchitectureMaterializer {
        source,
        semantics: &semantics,
        signal_drivers: crate::word::signal_driver::SignalDriverIndex::new(source)?,
        operation_regions,
        regions,
        contracts,
        roots: &roots,
        config: &config,
    };
    let lowering_work = regions
        .regions()
        .iter()
        .map(|&region| region.estimated_work().max(1))
        .collect::<Vec<_>>();
    let tasks: Vec<_> = (0..regions.regions().len())
        .map(|row| {
            Task::new(
                TaskKey::new(REGIONAL_ARCHITECTURE_TASK_DOMAIN, row as u64),
                row,
            )
            .with_estimated_work(lowering_work[row])
        })
        .collect();
    let profiling = config.mapping_context.config.diagnostics.timing;
    let mapped_regions = runtime.map_ordered_composite(tasks, |region_index, regional_runtime| {
        let _region_profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            format!("logic_lowering.region[{region_index}]")
        });
        let region = regions.regions()[region_index];
        let decision = &decisions[region_index];
        let memory_implementations = crate::planning::regional::decode_memory_implementations(
            decision.memory_implementations(),
        )?;
        let mapped = materializer.materialize(
            &memory_implementations,
            decision.plan(),
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
    let mut selected_memories = vec![None; source.memories().len()];
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
    Ok(RegionalArchitecturePreparation {
        regions: prepared_regions.into_boxed_slice(),
        memories: selected_memories
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                SynthError::invariant("first-class memory has no selected regional implementation")
            })?
            .into_boxed_slice(),
    })
}

fn diagnostics_enabled(materializer: &RegionArchitectureMaterializer<'_>) -> bool {
    materializer
        .config
        .mapping_context
        .config
        .diagnostics
        .timing
}

impl RegionArchitectureMaterializer<'_> {
    fn prepare_operators(
        &self,
        region: SynthesisRegion,
        module: &word::WordModule,
        decisions: &ArchitectureDecisions,
        operation_sources: &[Option<word::OpId>],
        source_to_local: &std::collections::BTreeMap<word::ValueId, word::ValueId>,
    ) -> Result<PreparedOperators, SynthError> {
        let sources = crate::artifact::provenance::resolve_private_operator_sources(
            self.source,
            module,
            decisions,
            self.regions.operations(region),
            operation_sources,
        )?;
        let architecture = PrivateArchitecturePublication::capture_resolved(
            self.source,
            decisions,
            region.id(),
            source_to_local,
            &sources,
        )?;
        let arena = DurableOperatorArena::capture(module, decisions, &sources, |operation| {
            self.regions.operation_anchor(operation).ok_or_else(|| {
                SynthError::invariant("durable operator source has no stable occurrence anchor")
            })
        })?;
        Ok(PreparedOperators {
            architecture,
            operators: arena,
        })
    }

    fn value_belongs_to_region(
        &self,
        value: word::ValueId,
        region: RegionRowId,
        memories: &[word::MemoryId],
    ) -> Result<bool, SynthError> {
        let stored = self.source.value(value).ok_or_else(|| {
            SynthError::invariant("regional root is absent from its source Word module")
        })?;
        match stored.kind {
            word::ValueKind::Signal(reference) => {
                if self
                    .source
                    .memory_read_ports()
                    .iter()
                    .any(|port| port.data == reference.signal && memories.contains(&port.memory))
                {
                    return Ok(true);
                }
                Ok(
                    self.value_owner(value, &mut std::collections::BTreeSet::new())?
                        == Some(region),
                )
            }
            word::ValueKind::Operation(_) | word::ValueKind::Constant(_) => Ok(self
                .value_owner(value, &mut std::collections::BTreeSet::new())?
                == Some(region)),
        }
    }

    fn value_owner(
        &self,
        value: word::ValueId,
        active: &mut std::collections::BTreeSet<word::ValueId>,
    ) -> Result<Option<RegionRowId>, SynthError> {
        if !active.insert(value) {
            return Err(SynthError::invariant(
                "regional root ownership contains a static signal cycle",
            ));
        }
        let stored = self.source.value(value).ok_or_else(|| {
            SynthError::invariant("regional ownership references an unknown value")
        })?;
        let owner = match stored.kind {
            word::ValueKind::Operation(operation) => self
                .operation_regions
                .get(operation.index())
                .copied()
                .flatten(),
            word::ValueKind::Signal(reference) => {
                let Some(drivers) = self.signal_drivers.reference_drivers(reference) else {
                    active.remove(&value);
                    return Ok(None);
                };
                let mut owner = None;
                for driver in drivers {
                    let Some(candidate) = self.value_owner(driver, active)? else {
                        active.remove(&value);
                        return Ok(None);
                    };
                    if owner
                        .replace(candidate)
                        .is_some_and(|owner| owner != candidate)
                    {
                        active.remove(&value);
                        return Ok(None);
                    }
                }
                owner
            }
            word::ValueKind::Constant(_) => None,
        };
        active.remove(&value);
        Ok(owner)
    }

    fn value_is_region_sink(&self, value: word::ValueId, region: SynthesisRegion) -> bool {
        self.source.connects().iter().any(|connect| {
            connect.value == value
                && (self.source.signal_is_preserved(connect.target.signal)
                    || self
                        .source
                        .signal(connect.target.signal)
                        .is_some_and(|signal| {
                            let word::SignalKind::Port(port) = signal.kind else {
                                return false;
                            };
                            self.source.port(port).is_some_and(|port| {
                                matches!(
                                    port.direction,
                                    word::PortDirection::Output | word::PortDirection::Inout
                                )
                            })
                        }))
        }) || self
            .source
            .instances()
            .iter()
            .flat_map(|instance| &instance.connections)
            .any(|connection| connection.value == value)
            || self.regions.operations(region).iter().any(|&operation| {
                self.source.operation(operation).is_some_and(|operation| {
                    matches!(
                        operation.kind,
                        word::OpKind::Register(_) | word::OpKind::Latch(_)
                    ) && crate::word::operation_inputs(&operation.kind).contains(&value)
                })
            })
    }

    fn materialize(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        retained_plan: Option<&RegionCoverPlanRecord>,
        region: SynthesisRegion,
        context: RegionContextKey,
        runtime: &ExecutionContext,
    ) -> Result<RegionalArchitectureMapping, SynthError> {
        let restored_plan = retained_plan
            .map(|plan| plan.restore(region, context, self.contracts.contracts(region.row())))
            .transpose()?;
        let prepared = self.lower_private_region(memory_implementations, region)?;
        let PreparedRegionCover {
            private:
                LoweredPrivateRegion {
                    module,
                    source_to_local,
                    boundary_bindings,
                    operation_sources,
                    memory_values,
                    root_bindings,
                    architecture,
                    operators,
                    lowering,
                    root_pairs: _,
                },
            slice,
            decision_key,
        } = self.prepare_region_cover(prepared, memory_implementations, region)?;
        let empty_port_bindings = opto_timing::PortBindings::new([]);
        let LocalRegionBooleanLowering { ownership, subject } = lowering;
        let analysis = super::cover::analyze_region_cover(
            &module,
            super::cover::RegionCoverRequest {
                roots: slice.roots(),
                timing: self.config.timing,
                port_bindings: &empty_port_bindings,
                catalog: &self.config.mapping_context.combinational_catalog,
                options: RegionLogicOptions {
                    optimize: self
                        .config
                        .mapping_context
                        .combinational_catalog
                        .can_invert(),
                    config: self.config.mapping_context.config,
                    runtime,
                    incremental: Some(crate::boolean::logic::RewriteIncremental::new(
                        self.config.rewrite_recipes,
                        self.config.incremental_metrics,
                    )),
                },
                regional_slice: &slice,
            },
            subject,
        )?;
        let (rematerialized, binding) = self.compact_cover(
            analysis,
            &CompactRegionalCoverInputs {
                region,
                context,
                decision_key,
                module: &module,
                source_to_local: &source_to_local,
                boundary_bindings: &boundary_bindings,
                memory_values: &memory_values,
                operation_sources: &operation_sources,
                root_bindings: &root_bindings,
                ownership: &ownership,
                slice: &slice,
            },
        )?;
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
        })
    }

    fn lower_private_region(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<LoweredPrivateRegion, SynthError> {
        let PreparedPrivateWord {
            cone:
                RegionalWordCone {
                    mut module,
                    source_to_local,
                    boundary_bindings,
                    operation_sources,
                    memory_values,
                    root_bindings,
                },
            boundary_inputs,
            root_pairs,
        } = self.prepare_private_word(memory_implementations, region)?;
        let operation_sources = operation_sources.into_vec();
        let memory_values = memory_values.into_vec();
        let mut provenance = ProvenanceBuilder::for_regional_candidate(&module);
        let mut local_decisions =
            ArchitectureDecisions::for_private_region(&module, implementation_providers().into())?;
        local_decisions.select_for_budget(
            self.config.target_model,
            self.contracts.delay_budget(region.row()),
        )?;
        let PreparedOperators {
            architecture,
            operators,
        } = self.prepare_operators(
            region,
            &module,
            &local_decisions,
            &operation_sources,
            &source_to_local,
        )?;
        let local_root_values = root_pairs
            .iter()
            .map(|(_, local)| *local)
            .collect::<Vec<_>>();
        let mut binding_values = boundary_inputs
            .iter()
            .chain(&local_root_values)
            .chain(boundary_bindings.iter().map(|(_, local)| local))
            .chain(memory_values.iter().map(|binding| &binding.local))
            .copied()
            .collect::<Vec<_>>();
        binding_values.sort_unstable();
        binding_values.dedup();
        let profiling = self.config.mapping_context.config.diagnostics.timing;
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
                    binding_values: &binding_values,
                },
            )
        }?;
        Ok(LoweredPrivateRegion {
            module,
            source_to_local,
            boundary_bindings,
            operation_sources,
            memory_values,
            root_bindings,
            root_pairs,
            architecture,
            operators,
            lowering,
        })
    }

    fn prepare_region_cover(
        &self,
        mut private: LoweredPrivateRegion,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<PreparedRegionCover, SynthError> {
        private.root_pairs = expand_mapping_root_pairs(
            self.source,
            self.semantics,
            &private.lowering.ownership,
            private.root_pairs,
        )?;
        let local_semantics = super::roots::FullDomainRootSemantics::new(&private.module)?;
        let substrate_outputs = target_output_artifact_keys(
            &private.module,
            &self.config.options.target_cells,
            &local_semantics,
        )?;
        suppress_substrate_roots(
            &private.module,
            &local_semantics,
            &private.lowering.ownership,
            &substrate_outputs,
            &mut private.root_pairs,
        )?;
        let sequential_timing = super::sequential::SequentialTimingProjection::build(
            &private.module,
            &self.config.mapping_context.sequential_catalog,
            &self.config.mapping_context.combinational_catalog,
        )?;
        let empty_port_bindings = opto_timing::PortBindings::new([]);
        let roots = mapping_roots(
            &private.module,
            self.config.timing,
            &empty_port_bindings,
            Some(&sequential_timing),
        )?;
        append_local_mapping_roots(
            &private.module,
            &local_semantics,
            &private.lowering.ownership,
            &substrate_outputs,
            roots,
            &mut private.root_pairs,
        )?;
        private.root_pairs =
            merge_mapping_root_pairs(&private.module, &local_semantics, private.root_pairs)?;
        let decision_key = crate::planning::regional::decision_key(memory_implementations);
        let mut slice = super::logic_partition::RegionLogicSlice::build_candidate(
            region.id(),
            decision_key,
            super::logic_partition::RegionLogicCandidateInputs {
                module: &private.module,
                subject_inputs: &private.lowering.subject.inputs,
                source_to_local: &private.source_to_local,
                ownership: &private.lowering.ownership,
                contracts: self.contracts.contracts(region.row()),
                roots: &private.root_pairs,
            },
        )?;
        slice.project_sequential_timing(&sequential_timing);
        Ok(PreparedRegionCover {
            private,
            slice,
            decision_key,
        })
    }

    fn compact_cover(
        &self,
        analysis: super::cover::RegionCoverAnalysis,
        inputs: &CompactRegionalCoverInputs<'_>,
    ) -> Result<(RegionCoverPlan, RegionPlanBinding), SynthError> {
        let CompactRegionalCoverInputs {
            region,
            context,
            decision_key,
            module,
            source_to_local,
            boundary_bindings,
            memory_values,
            operation_sources,
            root_bindings,
            ownership,
            slice,
        } = *inputs;
        let super::cover::RegionCoverAnalysis::Covered(mut analysis) = analysis else {
            return Ok((
                empty_target_plan(region, context, self.contracts, decision_key)?,
                RegionPlanBinding::empty(),
            ));
        };
        let binding = analysis.candidate_binding(
            CandidateBindingInputs {
                source_module: self.source,
                local_module: module,
                source_to_local,
                boundary_bindings,
                memory_values,
                operation_sources,
                root_bindings,
                ownership,
            },
            &self.config.mapping_context.combinational_catalog,
        )?;
        let response_models = super::cover::CoverResponseModels::new(self.config.scenarios);
        let plan = analysis.compact_plan(super::cover::CompactPlanInputs {
            region,
            context,
            boundary_response: self.contracts.contracts(region.row()),
            decision_key,
            catalog: &self.config.mapping_context.combinational_catalog,
            response_models: &response_models,
            timing_tags: self.contracts.timing_tags(),
            regional_slice: slice,
        })?;
        Ok((plan, binding))
    }

    fn prepare_private_word(
        &self,
        memory_implementations: &[MemoryImplementationCandidate],
        region: SynthesisRegion,
    ) -> Result<PreparedPrivateWord, SynthError> {
        let memories = self.regions.memories(region);
        if memory_implementations.len() != memories.len() {
            return Err(SynthError::invariant(
                "regional memory decision does not match region ownership",
            ));
        }
        let mut regional_observations = Vec::new();
        let mut regional_roots = Vec::new();
        for &root in self.roots {
            if self.value_is_region_sink(root.value, region)
                && self.value_belongs_to_region(root.value, region.row(), memories)?
            {
                regional_observations.push(root.value);
                let value = self.semantics.canonical_root(root.value)?;
                if self.value_belongs_to_region(value, region.row(), memories)? {
                    regional_roots.push(MappingRoot {
                        value,
                        requires_combinational_cover: self.semantics.requires_artifact(value)?,
                        ..root
                    });
                }
            }
        }
        for &port in self.regions.output_ports(region) {
            let port = self.regions.port(port).ok_or_else(|| {
                SynthError::invariant("regional output references an unknown port")
            })?;
            regional_observations.push(port.value());
            let value = self.semantics.canonical_root(port.value())?;
            if self.value_belongs_to_region(value, region.row(), memories)? {
                regional_roots.push(MappingRoot {
                    value,
                    required_time: None,
                    output_load: None,
                    requires_combinational_cover: self.semantics.requires_artifact(value)?,
                });
            }
        }
        let regional_roots = merge_by_value(regional_roots);
        let boundary_inputs = self.checked_port_values(self.regions.input_ports(region))?;
        let profiling = self.config.mapping_context.config.diagnostics.timing;
        let row = region.row().raw();
        let cone = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("regional_lowering.region[{row}].word_cone")
            });
            RegionalWordCone::build(RegionalWordConeRequest {
                source: self.source,
                operation_regions: self.operation_regions,
                region: region.row(),
                memories,
                memory_implementations,
                target_cells: &self.config.options.target_cells,
                boundary_inputs: &boundary_inputs,
                observations: regional_observations,
                roots: regional_roots.iter().map(|root| root.value).collect(),
            })
        }?;
        let RegionalWordCone {
            mut module,
            mut source_to_local,
            mut boundary_bindings,
            operation_sources,
            mut memory_values,
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
            &mut memory_values,
        );
        if crate::planning::operator::share_muxed_arithmetic(&mut module)? != 0 {
            let local_changes =
                crate::planning::dataflow::canonicalize_combinational_dataflow(&mut module)?;
            remap_private_values(
                &local_changes,
                &mut source_to_local,
                &mut boundary_bindings,
                &mut memory_values,
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
        let local_boundary_inputs = boundary_inputs
            .iter()
            .map(map_source)
            .collect::<Result<Vec<_>, _>>()?;
        let root_pairs = regional_roots
            .iter()
            .map(|root| map_source(&root.value).map(|local| (*root, local)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PreparedPrivateWord {
            cone: RegionalWordCone {
                module,
                source_to_local,
                boundary_bindings,
                operation_sources: operation_sources.into_boxed_slice(),
                memory_values,
                root_bindings,
            },
            boundary_inputs: local_boundary_inputs,
            root_pairs,
        })
    }

    fn checked_port_values(
        &self,
        ports: &[RegionBoundaryPortId],
    ) -> Result<Vec<word::ValueId>, SynthError> {
        ports
            .iter()
            .map(|&port| {
                self.regions
                    .port(port)
                    .map(RegionBoundaryPort::value)
                    .ok_or_else(|| {
                        SynthError::invariant(
                            "synthesis region references an unknown boundary port",
                        )
                    })
            })
            .collect()
    }
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
    roots: Vec<(MappingRoot, word::ValueId)>,
) -> Result<Vec<(MappingRoot, word::ValueId)>, SynthError> {
    let mut expanded = Vec::new();
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
            expanded.push((
                MappingRoot {
                    requires_combinational_cover: semantics
                        .bit_requires_artifact(root.value, bit)?,
                    ..root
                },
                local,
            ));
        }
    }
    Ok(expanded)
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
            stable_plan_key: super::cover::empty_plan_key(region.id(), decision_key),
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

pub(crate) fn extend_operation_regions_for_memories(
    module: &word::WordModule,
    original: &[Option<RegionRowId>],
    memory_regions: &[RegionRowId],
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
    Ok(owners)
}
