// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{LoweredState, NormalizedState, PlannedState, SynthesisExecution};
use crate::artifact::provenance::ProvenanceBuilder;

pub(super) fn lower_logic(
    execution: &SynthesisExecution<'_>,
    planned: PlannedState,
) -> Result<LoweredState, crate::SynthError> {
    let PlannedState {
        normalized,
        mapping_context,
        target_model,
        regions,
        contracts,
    } = planned;
    let NormalizedState {
        environment,
        mut ledger,
        previous_regional_cache_records: _,
        source_instances,
        synthesized: mut source,
    } = normalized;
    let memory_regions = regions.memory_owner_rows();
    let operation_regions = regions.operation_owner_rows();
    let profiling = execution.engine.config.diagnostics.timing;
    let preparation = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "logic_lowering.architecture_preparation".to_string()
        });
        crate::mapping::prepare_regional_architectures(
            crate::mapping::RegionalArchitectureRequest {
                source: &source,
                operation_regions,
                decisions: &ledger.regional_cache_records,
                regions: &regions,
                contracts: &contracts,
                config: crate::mapping::ArchitectureMappingConfig {
                    options: &environment.options,
                    timing: environment.primary_scenario().constraints(),
                    scenarios: &environment.scenarios,
                    target_model: &target_model,
                    port_bindings: &environment.port_bindings,
                    mapping_context: &mapping_context,
                    rewrite_recipes: &execution.engine.rewrite_recipes,
                    incremental_metrics: &environment.incremental_metrics,
                },
            },
            execution.runtime,
        )
    }?;
    let (prepared_regions, memory_implementations) = preparation;
    let mut prepared_regions = prepared_regions.into_vec();
    for prepared in &mut prepared_regions {
        prepared.binding.resolve_sequential_sources(&source)?;
    }
    let memory_ownership = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "logic_lowering.selected_memory_lowering".to_string()
        });
        crate::planning::memory::lower_selected_memories(
            &mut source,
            &memory_implementations,
            &environment.options.target_cells,
        )
    }?;
    let operation_regions = crate::mapping::extend_operation_regions_for_memories(
        &source,
        operation_regions,
        memory_regions,
        &memory_ownership,
    )?;
    for prepared in &mut prepared_regions {
        prepared
            .binding
            .resolve_memory_sources(&source, &memory_ownership)?;
    }
    let mut regional_binding_values = prepared_regions
        .iter()
        .flat_map(|prepared| prepared.binding.source_values())
        .collect::<Vec<_>>();
    for region in regions.regions() {
        for &port in regions
            .input_ports(*region)
            .iter()
            .chain(regions.output_ports(*region))
        {
            regional_binding_values.push(
                regions
                    .port(port)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional shell references an unknown boundary port",
                        )
                    })?
                    .value(),
            );
        }
    }
    regional_binding_values.sort_unstable();
    regional_binding_values.dedup();
    let operator_manifest = crate::OperatorManifest::capture(
        prepared_regions.iter().map(|prepared| &prepared.operators),
    )?;
    let (provenance, mut region_ownership, mut regional_plans) = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "logic_lowering.global_bitblast".to_string()
        });
        let shell = crate::planning::operator::ArchitectureDecisions::for_regional_shell(&source);
        let mut provenance = ProvenanceBuilder::new(&source, &shell)?;
        let mut regional_plans = Vec::with_capacity(prepared_regions.len());
        for prepared in prepared_regions {
            let crate::mapping::RegionalArchitectureMapping {
                plan,
                binding,
                architecture,
                operators: _,
            } = prepared;
            provenance.import_private_architecture(architecture, &source)?;
            regional_plans.push(super::regional_mapping::RegionalPlanRow { plan, binding });
        }
        let ownership = crate::boolean::bitblast::bitblast_module_with_regions(
            &mut source,
            &shell,
            &mut provenance,
            &operation_regions,
            &regional_binding_values,
            crate::boolean::bitblast::GlobalBitblastScope::RegionalShell,
        )?;
        Ok::<_, crate::SynthError>((provenance, ownership, regional_plans))
    }?;
    {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "logic_lowering.binding_materialization".to_string()
        });
        for row in &mut regional_plans {
            row.binding
                .materialize_source_bits(&source, &region_ownership, &memory_ownership)?;
        }
    }
    ledger.lowered_values = source.values().len();
    ledger.lowered_operations = source.operations().len();
    region_ownership.infer_unowned(&source)?;
    Ok(LoweredState {
        environment,
        ledger,
        source_instances,
        mapping_context,
        regions,
        region_ownership,
        contracts,
        regional_plans: regional_plans.into_boxed_slice(),
        synthesized: source,
        provenance,
        operator_manifest,
    })
}
