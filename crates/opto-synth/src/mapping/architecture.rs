// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Publishes one regional construction for target-library mapping.

use super::roots::{MappingRoot, mapping_roots, merge_by_value};
use crate::artifact::provenance::ProvenanceBuilder;
use crate::boolean::logic::RegionLogicOptions;
use crate::planning::regional::{RegionalDecisionPlan, RegionalDecisionVector};
use crate::regional::RegionContractSet;
use opto_ir::word;
use opto_runtime::{ExecutionContext, Task, TaskKey};

const REGIONAL_ARCHITECTURE_TASK_DOMAIN: u32 = 0x5245_4741;

pub(crate) struct ArchitectureMappingConfig<'a> {
    pub(crate) options: &'a crate::SynthesisOptions,
    pub(crate) timing: &'a opto_timing::TimingContext,
    pub(crate) scenarios: &'a opto_timing::ScenarioSet,
    pub(crate) target_model: &'a crate::planning::regional::StructuralTargetModel,
    pub(crate) port_bindings: &'a opto_timing::PortBindings,
    pub(crate) mapping_context: &'a super::TargetMappingContext,
    pub(crate) rewrite_recipes: &'a crate::boolean::logic::RewriteRecipeCache,
    pub(crate) incremental_metrics: &'a crate::incremental::IncrementalRunMetrics,
}

struct RegionArchitectureMaterializer<'a> {
    source: &'a word::WordModule,
    signal_drivers: crate::word::signal_driver::SignalDriverIndex,
    operation_regions: &'a [Option<crate::RegionRowId>],
    regions: &'a crate::SynthesisRegionGraph,
    contracts: &'a RegionContractSet,
    roots: &'a [MappingRoot],
    config: &'a ArchitectureMappingConfig<'a>,
}

pub(crate) struct RegionalArchitectureRequest<'a> {
    pub(crate) source: &'a word::WordModule,
    pub(crate) operation_regions: &'a [Option<crate::RegionRowId>],
    pub(crate) plan: &'a RegionalDecisionPlan,
    pub(crate) regions: &'a crate::SynthesisRegionGraph,
    pub(crate) contracts: &'a RegionContractSet,
    pub(crate) contexts: &'a [crate::RegionContextKey],
    pub(crate) config: ArchitectureMappingConfig<'a>,
}

pub(crate) struct RegionalArchitecturePreparation {
    pub(crate) private_architectures:
        Box<[crate::artifact::provenance::PrivateArchitecturePublication]>,
    pub(crate) decision_keys: Box<[[u8; 32]]>,
    pub(crate) mapping_seed: super::RegionalMappingSeed,
    pub(crate) memory_implementations:
        Box<[crate::planning::regional::MemoryImplementationCandidate]>,
    pub(crate) operators: Box<[crate::DurableOperatorArena]>,
}

struct RegionArchitectureMapping {
    plan: crate::RegionCoverPlan,
    binding: crate::mapping::RegionPlanBinding,
    architecture: crate::artifact::provenance::PrivateArchitecturePublication,
    operators: crate::DurableOperatorArena,
}

struct PreparedPrivateWord {
    module: word::WordModule,
    source_to_local: std::collections::BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: Box<[(word::ValueId, word::ValueId)]>,
    operation_sources: Vec<Option<word::OpId>>,
    memory_values: Vec<crate::planning::regional::RegionalMemoryValueBinding>,
    root_bindings: Box<[(word::ValueId, word::SignalId)]>,
    local_boundary_inputs: Vec<word::ValueId>,
    root_pairs: Vec<(MappingRoot, word::ValueId)>,
}

struct PreparedOperators {
    architecture: crate::artifact::provenance::PrivateArchitecturePublication,
    arena: crate::DurableOperatorArena,
}

fn remap_private_values(
    changes: &crate::planning::dataflow::DataflowChanges,
    source_to_local: &mut std::collections::BTreeMap<word::ValueId, word::ValueId>,
    boundary_bindings: &mut [(word::ValueId, word::ValueId)],
    memory_values: &mut [crate::planning::regional::RegionalMemoryValueBinding],
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
) -> Result<RegionalArchitecturePreparation, crate::SynthError> {
    let RegionalArchitectureRequest {
        source,
        operation_regions,
        plan,
        regions,
        contracts,
        contexts,
        config,
    } = request;
    if plan.len() != regions.regions().len() || contexts.len() != regions.regions().len() {
        return Err(crate::SynthError::invariant(
            "regional architecture plan does not align with the region graph",
        ));
    }
    let roots = mapping_roots(source, config.timing, config.port_bindings)?;
    let materializer = RegionArchitectureMaterializer {
        source,
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
        let vector = plan.vector(region.row());
        let mapped = materializer.materialize(
            vector,
            region,
            contexts[region.row().index()],
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
        Ok::<_, crate::SynthError>((vector.clone(), mapped))
    })?;
    let mut selected_keys = Vec::with_capacity(mapped_regions.len());
    let mut selected_plans = Vec::with_capacity(mapped_regions.len());
    let mut selected_bindings = Vec::with_capacity(mapped_regions.len());
    let mut private_architectures = Vec::with_capacity(mapped_regions.len());
    let mut operators = Vec::with_capacity(mapped_regions.len());
    let mut selected_memories = vec![None; source.memories().len()];
    for (row, (vector, mapped)) in mapped_regions.into_iter().enumerate() {
        let region = regions.regions()[row];
        let memories = regions.memories(region);
        if memories.len() != vector.memory_implementations().len() {
            return Err(crate::SynthError::invariant(
                "selected memory implementations do not align with region ownership",
            ));
        }
        for (&memory, &implementation) in memories.iter().zip(vector.memory_implementations()) {
            if selected_memories[memory.index()]
                .replace(implementation)
                .is_some()
            {
                return Err(crate::SynthError::invariant(
                    "memory implementation is selected by more than one region",
                ));
            }
        }
        selected_keys.push(vector.stable_key());
        selected_plans.push(mapped.plan);
        selected_bindings.push(mapped.binding);
        private_architectures.push(mapped.architecture);
        operators.push(mapped.operators);
    }
    Ok(RegionalArchitecturePreparation {
        private_architectures: private_architectures.into_boxed_slice(),
        decision_keys: selected_keys.into_boxed_slice(),
        mapping_seed: super::RegionalMappingSeed::Private {
            plans: selected_plans.into_boxed_slice(),
            bindings: selected_bindings.into_boxed_slice(),
        },
        memory_implementations: selected_memories
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "first-class memory has no selected regional implementation",
                )
            })?
            .into_boxed_slice(),
        operators: operators.into_boxed_slice(),
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
        region: crate::SynthesisRegion,
        module: &word::WordModule,
        decisions: &crate::planning::operator::ArchitectureDecisions,
        operation_sources: &[Option<word::OpId>],
        source_to_local: &std::collections::BTreeMap<word::ValueId, word::ValueId>,
    ) -> Result<PreparedOperators, crate::SynthError> {
        let sources = crate::artifact::provenance::resolve_private_operator_sources(
            self.source,
            module,
            decisions,
            self.regions.operations(region),
            operation_sources,
        )?;
        let architecture =
            crate::artifact::provenance::PrivateArchitecturePublication::capture_resolved(
                self.source,
                decisions,
                region.id(),
                source_to_local,
                &sources,
            )?;
        let arena =
            crate::DurableOperatorArena::capture(module, decisions, &sources, |operation| {
                self.regions.operation_anchor(operation).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "durable operator source has no stable occurrence anchor",
                    )
                })
            })?;
        Ok(PreparedOperators {
            architecture,
            arena,
        })
    }

    fn value_belongs_to_region(
        &self,
        value: word::ValueId,
        region: crate::RegionRowId,
        memories: &[word::MemoryId],
    ) -> Result<bool, crate::SynthError> {
        let value = self.source.value(value).ok_or_else(|| {
            crate::SynthError::invariant("regional root is absent from its source Word module")
        })?;
        match value.kind {
            word::ValueKind::Operation(operation) => Ok(self
                .operation_regions
                .get(operation.index())
                .copied()
                .flatten()
                .ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "regional root operation {operation:?} is outside the ownership index"
                    ))
                })?
                == region),
            word::ValueKind::Signal(reference) => {
                if self
                    .source
                    .memory_read_ports()
                    .iter()
                    .any(|port| port.data == reference.signal && memories.contains(&port.memory))
                {
                    return Ok(true);
                }
                let Some(drivers) = self.signal_drivers.reference_drivers(reference) else {
                    return Ok(false);
                };
                if drivers.is_empty() {
                    return Ok(false);
                }
                for driver in drivers {
                    let driver = self.source.value(driver).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional signal driver is absent from its source Word module",
                        )
                    })?;
                    let word::ValueKind::Operation(operation) = driver.kind else {
                        return Ok(false);
                    };
                    let owner = self
                        .operation_regions
                        .get(operation.index())
                        .copied()
                        .flatten()
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "regional signal driver is outside the ownership index",
                            )
                        })?;
                    if owner != region {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            word::ValueKind::Constant(_) => Ok(false),
        }
    }

    fn value_is_region_sink(&self, value: word::ValueId, region: crate::SynthesisRegion) -> bool {
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
        vector: &RegionalDecisionVector,
        region: crate::SynthesisRegion,
        context: crate::RegionContextKey,
        runtime: &ExecutionContext,
    ) -> Result<RegionArchitectureMapping, crate::SynthError> {
        let restored_plan = vector
            .retained_plan()
            .map(|plan| plan.restore(region, context, self.contracts.contracts(region.row())))
            .transpose()?;
        let profiling = self.config.mapping_context.config.diagnostics.timing;
        let row = region.row().raw();
        let PreparedPrivateWord {
            mut module,
            source_to_local,
            boundary_bindings,
            operation_sources,
            memory_values,
            root_bindings,
            local_boundary_inputs,
            mut root_pairs,
        } = self.prepare_private_word(vector, region)?;
        let empty_port_bindings = opto_timing::PortBindings::new([]);
        let mut provenance = ProvenanceBuilder::for_regional_candidate(&module);
        let mut local_decisions =
            crate::planning::operator::ArchitectureDecisions::for_private_region(
                &module,
                crate::boolean::bitblast::implementation_providers().into(),
            )?;
        local_decisions.select_for_budget(
            self.config.target_model,
            self.contracts.delay_budget(region.row()),
        )?;
        let PreparedOperators {
            architecture,
            arena: operators,
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
        let ownership = {
            let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
                format!("logic_lowering.region[{row}].bitblast")
            });
            crate::boolean::bitblast::bitblast_local_region_values(
                &mut module,
                crate::boolean::bitblast::LocalRegionBitblastRequest {
                    plan: &local_decisions,
                    operators: &operators,
                    provenance: &mut provenance,
                    owner: region.row(),
                    boundary_inputs: &local_boundary_inputs,
                    roots: &local_root_values,
                    runtime,
                },
            )
        }?;
        root_pairs.extend(
            mapping_roots(&module, self.config.timing, &empty_port_bindings)?
                .into_iter()
                .map(|root| (root, root.value)),
        );
        let root_pairs = merge_mapping_root_pairs(root_pairs);
        let decision_key = vector.stable_key();
        let slice = super::logic_partition::RegionLogicSlice::build_candidate(
            &module,
            region.id(),
            decision_key,
            &source_to_local,
            &ownership,
            self.contracts.contracts(region.row()),
            &root_pairs,
        )?;
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
                    boundary_inputs: slice.inputs(),
                },
                regional_slice: &slice,
            },
        )?;
        let (rematerialized, binding) = match analysis {
            super::cover::RegionCoverAnalysis::Covered(analysis) => {
                let binding =
                    analysis.candidate_binding(crate::mapping::CandidateBindingInputs {
                        source_module: self.source,
                        local_module: &module,
                        source_to_local: &source_to_local,
                        boundary_bindings: &boundary_bindings,
                        memory_values: &memory_values,
                        operation_sources: &operation_sources,
                        root_bindings: &root_bindings,
                        ownership: &ownership,
                    })?;
                let response_models = super::cover::CoverResponseModels::new(self.config.scenarios);
                let plan = analysis.compact_plan(super::cover::CompactPlanInputs {
                    module: &module,
                    region,
                    context,
                    boundary_response: self.contracts.contracts(region.row()),
                    decision_key,
                    catalog: &self.config.mapping_context.combinational_catalog,
                    response_models: &response_models,
                    timing_tags: self.contracts.timing_tags(),
                    regional_slice: &slice,
                })?;
                (plan, binding)
            }
            super::cover::RegionCoverAnalysis::NoCombinationalLogic => (
                empty_target_plan(region, context, self.contracts, decision_key)?,
                crate::mapping::RegionPlanBinding::empty(),
            ),
        };
        let plan = match restored_plan {
            Some(plan) if plan.matches_materialized_topology(&rematerialized) => plan,
            Some(_) => {
                return Err(crate::SynthError::invariant(
                    "cached regional plan differs from the topology reconstructed by its context",
                ));
            }
            None => rematerialized,
        };
        Ok(RegionArchitectureMapping {
            plan,
            binding,
            architecture,
            operators,
        })
    }

    fn prepare_private_word(
        &self,
        vector: &RegionalDecisionVector,
        region: crate::SynthesisRegion,
    ) -> Result<PreparedPrivateWord, crate::SynthError> {
        let memories = self.regions.memories(region);
        if vector.memory_implementations().len() != memories.len() {
            return Err(crate::SynthError::invariant(
                "regional memory decision does not match region ownership",
            ));
        }
        let mut regional_roots = Vec::new();
        for &root in self.roots {
            if self.value_is_region_sink(root.value, region)
                && self.value_belongs_to_region(root.value, region.row(), memories)?
            {
                regional_roots.push(root);
            }
        }
        for &port in self.regions.output_ports(region) {
            let port = self.regions.port(port).ok_or_else(|| {
                crate::SynthError::invariant("regional output references an unknown port")
            })?;
            if self.contracts.dataflow(region.row(), port.id())?.live
                && !regional_roots.iter().any(|root| root.value == port.value())
            {
                regional_roots.push(MappingRoot {
                    value: port.value(),
                    required_time: None,
                    output_load: None,
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
            crate::planning::regional::RegionalWordCone::build(
                crate::planning::regional::RegionalWordConeRequest {
                    source: self.source,
                    operation_regions: self.operation_regions,
                    region: region.row(),
                    memories,
                    memory_implementations: vector.memory_implementations(),
                    target_cells: &self.config.options.target_cells,
                    boundary_inputs: &boundary_inputs,
                    roots: regional_roots.iter().map(|root| root.value).collect(),
                },
            )
        }?;
        let crate::planning::regional::RegionalWordConeParts {
            mut module,
            mut source_to_local,
            mut boundary_bindings,
            operation_sources,
            mut memory_values,
            root_bindings,
        } = cone.into_parts();
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
                crate::SynthError::invariant(
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
            module,
            source_to_local,
            boundary_bindings,
            operation_sources,
            memory_values: memory_values.into_vec(),
            root_bindings,
            local_boundary_inputs,
            root_pairs,
        })
    }

    fn checked_port_values(
        &self,
        ports: &[crate::RegionBoundaryPortId],
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        ports
            .iter()
            .map(|&port| {
                self.regions
                    .port(port)
                    .map(crate::RegionBoundaryPort::value)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "synthesis region references an unknown boundary port",
                        )
                    })
            })
            .collect()
    }
}

fn merge_mapping_root_pairs(
    mut roots: Vec<(MappingRoot, word::ValueId)>,
) -> Vec<(MappingRoot, word::ValueId)> {
    roots.sort_by_key(|(_, local)| *local);
    roots.into_iter().fold(Vec::new(), |mut merged, next| {
        if let Some((current, local)) = merged.last_mut()
            && *local == next.1
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
        } else {
            merged.push(next);
        }
        merged
    })
}

fn empty_target_plan(
    region: crate::SynthesisRegion,
    context: crate::RegionContextKey,
    contracts: &RegionContractSet,
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
    original: &[Option<crate::RegionRowId>],
    memory_regions: &[crate::RegionRowId],
    memory_ownership: &crate::planning::memory::MemoryLoweringOwnership,
) -> Result<Vec<Option<crate::RegionRowId>>, crate::SynthError> {
    if original.len() > module.operations().len() {
        return Err(crate::SynthError::invariant(
            "memory lowering removed source operations",
        ));
    }
    let mut owners = original.to_vec();
    owners.resize(module.operations().len(), None);
    for (operation, memory) in memory_ownership.operations() {
        let owner = memory_regions.get(memory.index()).copied().ok_or_else(|| {
            crate::SynthError::invariant("lowered memory has no synthesis-region owner")
        })?;
        let slot = owners.get_mut(operation.index()).ok_or_else(|| {
            crate::SynthError::invariant("lowered memory operation is outside the Word arena")
        })?;
        if slot.replace(owner).is_some() {
            return Err(crate::SynthError::invariant(
                "lowered memory operation already has a synthesis-region owner",
            ));
        }
    }
    Ok(owners)
}
