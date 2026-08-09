// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{MemoryImplementationCandidate, RegionalDecisionPlan, RegionalDecisionVector};
use crate::incremental::RegionalCacheRecord;
use crate::{RegionContextKey, SynthesisEffort, SynthesisRegionGraph};
use opto_ir::word;
use opto_runtime::ExecutionContext;
use opto_timing::ScenarioSet;
use std::collections::BTreeSet;

pub(crate) struct RegionalArchitectureSearch;

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

pub(crate) struct RegionalSearchOutcome {
    pub(crate) cache_records: Box<[RegionalCacheRecord]>,
    pub(crate) contexts: Box<[RegionContextKey]>,
    pub(crate) decision_plan: RegionalDecisionPlan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CandidateScore {
    primary: u64,
    secondary: u64,
    depth: u64,
    candidate: u32,
}

#[derive(Debug, Clone)]
struct RankedMemoryCandidate {
    candidate: MemoryImplementationCandidate,
    score: CandidateScore,
}

#[derive(Clone, Copy)]
struct RegionRowSearchRequest<'a> {
    module: &'a word::WordModule,
    memories: &'a [word::MemoryId],
    target_cells: &'a opto_library::TargetCellSet,
    target_model: &'a crate::planning::regional::StructuralTargetModel,
}

impl RegionalArchitectureSearch {
    pub(crate) fn select(
        request: RegionalSearchRequest<'_>,
        runtime: &ExecutionContext,
    ) -> Result<RegionalSearchOutcome, crate::SynthError> {
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
                                crate::SynthError::invariant(
                                    "regional predecessor row is out of range",
                                )
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
            .copied()
            .map(|context| {
                previous
                    .binary_search_by_key(&context, RegionalCacheRecord::context)
                    .ok()
                    .map(|index| &previous[index])
            })
            .collect::<Vec<_>>();
        let results =
            runtime.analyze_indexed(regions.regions().len(), |row| match &cached[row] {
                Some(cached) => restore_cached_region(
                    module,
                    regions.memories(regions.regions()[row]),
                    target_cells,
                    cached,
                ),
                None => search_region(RegionRowSearchRequest {
                    module,
                    memories: regions.memories(regions.regions()[row]),
                    target_cells,
                    target_model,
                }),
            })?;
        let mut decision_rows = Vec::with_capacity(results.len());
        let mut checkpoint_records = Vec::with_capacity(results.len());
        for (row, vector) in results.into_iter().enumerate() {
            let memory_implementations = vector.portable_memory_implementations();
            if cached[row].is_some() {
                metrics.regional_decision_hit();
            } else {
                metrics.regional_decision_miss();
            }
            let checkpoint = cached[row].cloned().unwrap_or_else(|| {
                RegionalCacheRecord::new(contexts[row], &memory_implementations)
            });
            checkpoint_records.push(checkpoint);
            decision_rows.push(vector);
        }
        Ok(RegionalSearchOutcome {
            cache_records: checkpoint_records.into_boxed_slice(),
            contexts: contexts.into_boxed_slice(),
            decision_plan: RegionalDecisionPlan::new(decision_rows),
        })
    }
}

fn search_region(
    request: RegionRowSearchRequest<'_>,
) -> Result<RegionalDecisionVector, crate::SynthError> {
    let RegionRowSearchRequest {
        module,
        memories,
        target_cells,
        target_model,
    } = request;
    let memory_implementations = memories
        .iter()
        .copied()
        .map(|memory| {
            rank_memory(module, memory, target_cells, target_model)?
                .first()
                .map(|candidate| candidate.candidate)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "ranked regional memory has no construction candidate",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RegionalDecisionVector::new(memory_implementations))
}

fn restore_cached_region(
    module: &word::WordModule,
    memories: &[word::MemoryId],
    target_cells: &opto_library::TargetCellSet,
    cached: &RegionalCacheRecord,
) -> Result<RegionalDecisionVector, crate::SynthError> {
    if cached.memory_implementations().len() != memories.len().saturating_mul(4) {
        return Err(crate::SynthError::invariant(
            "regional cache memory shape does not match reconstructed identity",
        ));
    }
    let (encoded_memories, remainder) = cached.memory_implementations().as_chunks::<4>();
    debug_assert!(remainder.is_empty(), "validated four-byte memory records");
    let memory_implementations = encoded_memories
        .iter()
        .map(|bytes| MemoryImplementationCandidate::from_raw(u32::from_le_bytes(*bytes)))
        .collect::<Vec<_>>();
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
    Ok(RegionalDecisionVector::new(memory_implementations)
        .with_retained_plan(cached.plan().cloned()))
}

fn rank_memory(
    module: &word::WordModule,
    memory: word::MemoryId,
    target_cells: &opto_library::TargetCellSet,
    target_model: &crate::planning::regional::StructuralTargetModel,
) -> Result<Vec<RankedMemoryCandidate>, crate::SynthError> {
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
        candidates.push(RankedMemoryCandidate {
            candidate: MemoryImplementationCandidate::RegisterBank,
            score: CandidateScore {
                primary,
                secondary,
                depth: depth_score,
                candidate: MemoryImplementationCandidate::RegisterBank.raw(),
            },
        });
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
        candidates.push(RankedMemoryCandidate {
            candidate,
            score: CandidateScore {
                primary,
                secondary,
                depth: delay_score,
                candidate: candidate.raw(),
            },
        });
    }
    candidates.sort_by_key(|candidate| candidate.score);
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
