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
        work,
        contracts,
    } = planned;
    let NormalizedState {
        environment,
        mut ledger,
        previous_regional_cache_records: _,
        source_instances,
        synthesized: mut source,
    } = normalized;
    let regions = work.regions();
    let memory_regions = regions.memory_region_rows();
    let operation_regions = regions.operation_region_rows();
    let profiling = execution.engine.config.diagnostics.timing;
    let preparation = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "logic_lowering.architecture_preparation".to_string()
        });
        crate::mapping::prepare_regional_architectures(
            &crate::mapping::RegionalArchitectureRequest {
                source: &source,
                operation_regions,
                decisions: &ledger.regional_cache_records,
                work: &work,
                contracts: &contracts,
                options: &environment.options,
                clock_gating: environment.clock_gating,
                timing: environment.primary_scenario().constraints(),
                scenarios: &environment.scenarios,
                target_model: &target_model,
                port_bindings: &environment.port_bindings,
                mapping_context: &mapping_context,
                rewrite_recipes: &execution.engine.rewrite_recipes,
                incremental_metrics: &environment.incremental_metrics,
            },
            execution.runtime,
        )
    }?;
    let (prepared_regions, memory_implementations) = preparation;
    let mut prepared_regions = prepared_regions.into_vec();
    let regional_publication = aggregate_regional_publication(
        &source,
        operation_regions,
        prepared_regions
            .iter()
            .flat_map(|prepared| prepared.publication.iter().copied()),
    )?;
    let memory_binding = {
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
        &memory_binding,
    )?;
    for prepared in &mut prepared_regions {
        prepared
            .binding
            .resolve_memory_sources(&source, &memory_binding)?;
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
    let source_sequential_operations =
        crate::mapping::materialize::sequential_region_bindings(&source, &regions)?;
    regional_binding_values.extend(crate::mapping::materialize::sequential_binding_values(
        &source,
        &source_sequential_operations,
    )?);
    regional_binding_values.sort_unstable();
    regional_binding_values.dedup();
    let operator_manifest = crate::OperatorManifest::capture(
        prepared_regions.iter().map(|prepared| &prepared.operators),
    )?;
    let (provenance, region_binding, mut regional_plans) = {
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
                publication: _,
                sequential,
            } = prepared;
            provenance.import_private_architecture(architecture, &source)?;
            regional_plans.push(super::regional_mapping::RegionalPlanRow {
                plan,
                binding,
                sequential,
            });
        }
        let binding = crate::boolean::bitblast::bitblast_module_with_regions(
            &mut source,
            &shell,
            &mut provenance,
            &operation_regions,
            &regional_binding_values,
            &regional_publication,
            crate::boolean::bitblast::GlobalBitblastScope::RegionalShell,
        )?;
        Ok::<_, crate::SynthError>((provenance, binding, regional_plans))
    }?;
    let sequential_operations = crate::mapping::materialize::lowered_sequential_operations(
        &source,
        &region_binding,
        &source_sequential_operations,
    )?;
    {
        let _profile = crate::api::diagnostics::ProfileSpan::new(profiling, || {
            "logic_lowering.binding_materialization".to_string()
        });
        for row in &mut regional_plans {
            row.binding
                .materialize_source_bits(&source, &region_binding, &memory_binding)?;
        }
    }
    ledger.lowered_values = source.values().len();
    ledger.lowered_operations = source.operations().len();
    Ok(LoweredState {
        environment,
        ledger,
        source_instances,
        mapping_context,
        work,
        region_binding,
        contracts,
        regional_plans: regional_plans.into_boxed_slice(),
        sequential_operations,
        synthesized: source,
        provenance,
        operator_manifest,
    })
}

fn aggregate_regional_publication(
    source: &opto_ir::word::WordModule,
    operation_regions: &[Option<crate::RegionRowId>],
    entries: impl IntoIterator<Item = crate::boolean::bitblast::RegionalPublicationBit>,
) -> Result<Vec<crate::boolean::bitblast::RegionalPublicationBit>, crate::SynthError> {
    let mut publication_by_bit =
        std::collections::BTreeMap::<(opto_ir::word::ValueId, u32), crate::RegionRowId>::new();
    let mut claimed_bits = std::collections::BTreeSet::new();
    for entry in entries {
        let key = (entry.target, entry.bit);
        claimed_bits.insert(key);
        let operation = source
            .value(entry.target)
            .and_then(|stored| match stored.kind {
                opto_ir::word::ValueKind::Operation(operation) => Some(operation),
                opto_ir::word::ValueKind::Signal(_) | opto_ir::word::ValueKind::Constant(_) => None,
            })
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "regional publication {:?}[{}] is not produced by an operation",
                    entry.target, entry.bit,
                ))
            })?;
        let authoritative = operation_regions
            .get(operation.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "regional publication {:?}[{}] has no source region",
                    entry.target, entry.bit,
                ))
            })?;
        if entry.producer == authoritative {
            publication_by_bit.insert(key, authoritative);
        }
    }
    if let Some((target, bit)) = claimed_bits
        .into_iter()
        .find(|key| !publication_by_bit.contains_key(key))
    {
        return Err(crate::SynthError::invariant(format!(
            "regional publication {target:?}[{bit}] was not emitted by its source region",
        )));
    }
    Ok(publication_by_bit
        .into_iter()
        .map(
            |((target, bit), producer)| crate::boolean::bitblast::RegionalPublicationBit {
                target,
                bit,
                producer,
            },
        )
        .collect())
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn publication_aggregation_selects_only_the_source_region() {
        let mut source = opto_ir::word::WordModule::new("publication_test");
        let input = source
            .add_port(
                "input",
                opto_ir::word::PortDirection::Input,
                opto_ir::word::WordType::bits(1).unwrap(),
                opto_ir::word::SourceSpan::default(),
            )
            .unwrap();
        let input = source
            .read_signal(
                source.port(input).unwrap().signal,
                opto_ir::word::SourceSpan::default(),
            )
            .unwrap();
        let target = source
            .unary(
                opto_ir::word::UnaryOp::BitNot,
                input,
                opto_ir::word::SourceSpan::default(),
            )
            .unwrap();
        let first = crate::RegionRowId::from_index(0).unwrap();
        let second = crate::RegionRowId::from_index(1).unwrap();
        let operation_regions = [Some(first)];
        let claim = |producer| crate::boolean::bitblast::RegionalPublicationBit {
            target,
            bit: 0,
            producer,
        };

        let duplicate = aggregate_regional_publication(
            &source,
            &operation_regions,
            [claim(second), claim(first), claim(second), claim(first)],
        )
        .unwrap();
        assert_eq!(duplicate, [claim(first)]);
        let error = aggregate_regional_publication(&source, &operation_regions, [claim(second)])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("was not emitted by its source region")
        );
    }
}
