// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::MemoryImplementationCandidate;
use crate::incremental::RegionalCacheRecord;
use crate::{RegionContextKey, SynthesisEffort, SynthesisRegionGraph};
use opto_ir::word;
use opto_runtime::ExecutionContext;
use opto_timing::ScenarioSet;
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(crate) struct RegionalSearchRequest<'a> {
    pub(crate) module: &'a word::WordModule,
    pub(crate) regions: &'a SynthesisRegionGraph,
    pub(crate) scenarios: &'a ScenarioSet,
    pub(crate) target_cells: &'a opto_library::TargetCellSet,
    pub(crate) target_model: &'a crate::planning::regional::StructuralTargetModel,
    pub(crate) contracts: &'a crate::regional::RegionContractSet,
    pub(crate) effort: SynthesisEffort,
    pub(crate) target_fingerprint: [u8; 32],
    pub(crate) previous: &'a [RegionalCacheRecord],
    pub(crate) metrics: &'a crate::incremental::IncrementalRunMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateScore {
    primary: u64,
    secondary: u64,
    depth: u64,
    candidate: u32,
}

pub(crate) fn select_architectures(
    request: RegionalSearchRequest<'_>,
    runtime: &ExecutionContext,
) -> Result<Box<[RegionalCacheRecord]>, crate::SynthError> {
    let RegionalSearchRequest {
        module,
        regions,
        scenarios,
        target_cells,
        target_model,
        contracts,
        effort,
        target_fingerprint,
        previous,
        metrics,
    } = request;
    let contexts = regions
        .regions()
        .iter()
        .map(|region| -> Result<RegionContextKey, crate::SynthError> {
            let predecessor_summaries = regions
                .predecessors(*region)
                .iter()
                .map(|&predecessor| {
                    regions
                        .region(predecessor)
                        .ok_or_else(|| {
                            crate::SynthError::invariant("regional predecessor row is out of range")
                        })
                        .map(|region| region.revision().bytes())
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(RegionContextKey::seal(
                region.revision(),
                contracts.contracts(region.row()),
                scenarios.generation(),
                target_fingerprint,
                effort,
                &predecessor_summaries,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cached = contexts
        .iter()
        .map(|context| {
            previous
                .binary_search_by_key(context, RegionalCacheRecord::context)
                .ok()
                .map(|index| &previous[index])
        })
        .collect::<Vec<_>>();
    let records = runtime.analyze_indexed(regions.regions().len(), |row| {
        let region = regions.regions()[row];
        if let Some(cached) = cached[row] {
            validate_cached_region(module, regions.memories(region), target_cells, cached)?;
            return Ok::<_, crate::SynthError>(cached.clone());
        }
        let implementations =
            search_region(module, regions.memories(region), target_cells, target_model)?;
        let encoded = implementations
            .iter()
            .flat_map(|implementation| implementation.raw().to_le_bytes())
            .collect::<Vec<_>>();
        Ok::<_, crate::SynthError>(RegionalCacheRecord::new(contexts[row], &encoded))
    })?;
    for cached in cached {
        if cached.is_some() {
            metrics.regional_decision_hit();
        } else {
            metrics.regional_decision_miss();
        }
    }
    Ok(records.into_boxed_slice())
}

fn search_region(
    module: &word::WordModule,
    memories: &[word::MemoryId],
    target_cells: &opto_library::TargetCellSet,
    target_model: &crate::planning::regional::StructuralTargetModel,
) -> Result<Box<[MemoryImplementationCandidate]>, crate::SynthError> {
    memories
        .iter()
        .copied()
        .map(|memory| {
            rank_memory(module, memory, target_cells, target_model)?
                .first()
                .map(|candidate| candidate.1)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "ranked regional memory has no construction candidate",
                    )
                })
        })
        .collect()
}

fn validate_cached_region(
    module: &word::WordModule,
    memories: &[word::MemoryId],
    target_cells: &opto_library::TargetCellSet,
    cached: &RegionalCacheRecord,
) -> Result<(), crate::SynthError> {
    if cached.memory_implementations().len() != memories.len().saturating_mul(4) {
        return Err(crate::SynthError::invariant(
            "regional cache memory shape does not match reconstructed identity",
        ));
    }
    let memory_implementations =
        super::decode_memory_implementations(cached.memory_implementations())?;
    for (&memory, &implementation) in memories.iter().zip(&memory_implementations) {
        let valid = match implementation {
            MemoryImplementationCandidate::RegisterBank => {
                crate::planning::memory::register_bank_is_supported(module, memory)
            }
            MemoryImplementationCandidate::Macro(cell) => {
                crate::planning::memory::compatible_memory_macros(module, memory, target_cells)?
                    .contains(&cell)
            }
        };
        if !valid {
            return Err(crate::SynthError::invariant(
                "regional cache memory candidate failed target reconstruction",
            ));
        }
    }
    Ok(())
}

fn rank_memory(
    module: &word::WordModule,
    memory: word::MemoryId,
    target_cells: &opto_library::TargetCellSet,
    target_model: &crate::planning::regional::StructuralTargetModel,
) -> Result<Vec<(CandidateScore, MemoryImplementationCandidate)>, crate::SynthError> {
    let resource = module.memory(memory).ok_or_else(|| {
        crate::SynthError::invariant("regional search references an unknown memory")
    })?;
    let mut candidates = Vec::new();
    let register_bank_supported =
        crate::planning::memory::register_bank_is_supported(module, memory);
    let register_bank_characterized = target_model.has_characterized_logic_costs();
    if register_bank_supported && register_bank_characterized {
        let reads = module
            .memory_read_ports()
            .iter()
            .filter(|port| port.memory == memory)
            .count() as u64;
        let writes = module
            .memory_write_ports()
            .iter()
            .filter(|port| port.memory == memory)
            .count() as u64;
        let depth = u64::from(resource.depth.get());
        let width = u64::from(resource.element_type.width());
        let logic_units = depth
            .saturating_mul(width)
            .saturating_mul(1u64.saturating_add(reads))
            .saturating_add(depth.saturating_mul(writes));
        let logic_depth =
            (u32::BITS - (resource.depth.get() - 1).leading_zeros()).saturating_add(1);
        let (primary, secondary, depth_score) =
            target_model.score(crate::planning::provider::StructuralEstimate {
                logic_depth,
                logic_units,
                wiring_units: logic_units.saturating_add(width.saturating_mul(reads + writes)),
            })?;
        candidates.push((
            CandidateScore {
                primary,
                secondary,
                depth: depth_score,
                candidate: MemoryImplementationCandidate::RegisterBank.raw(),
            },
            MemoryImplementationCandidate::RegisterBank,
        ));
    }
    let mut uncharacterized_macros = BTreeSet::new();
    for cell_index in
        crate::planning::memory::compatible_memory_macros(module, memory, target_cells)?
    {
        let cell = target_cells.get(cell_index as usize).ok_or_else(|| {
            crate::SynthError::invariant("compatible memory macro disappeared from target cells")
        })?;
        let area = opto_library::normalized_cell_area(cell.area());
        let Some(delay) = target_model.characterized_macro_delay(cell.name()) else {
            uncharacterized_macros.insert(cell.name().to_string());
            continue;
        };
        let (primary, secondary, delay_score) = target_model.score_macro(area, delay);
        let candidate = MemoryImplementationCandidate::Macro(cell_index);
        candidates.push((
            CandidateScore {
                primary,
                secondary,
                depth: delay_score,
                candidate: candidate.raw(),
            },
            candidate,
        ));
    }
    candidates.sort_by_key(|candidate| candidate.0);
    if candidates.is_empty() {
        let mut reasons = Vec::new();
        if register_bank_supported && !register_bank_characterized {
            reasons.push(
                "the register-bank implementation has no characterized combinational target basis"
                    .to_string(),
            );
        }
        if !uncharacterized_macros.is_empty() {
            reasons.push(format!(
                "compatible macros [{}] lack complete early/late output timing characterization",
                uncharacterized_macros
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if reasons.is_empty() {
            reasons.push("no semantically compatible implementation exists".to_string());
        }
        return Err(crate::SynthError::mapping(format!(
            "memory '{}' cannot be implemented: {}",
            module.name_str(resource.name),
            reasons.join("; ")
        )));
    }
    Ok(candidates)
}
