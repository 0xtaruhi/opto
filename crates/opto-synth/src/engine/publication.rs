// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Final mapped-owner publication and artifact assembly.

use super::{FinalizableState, report};
use crate::{IncrementalSnapshot, SynthesisMetrics, SynthesisResult};

pub(super) fn finalize(
    mut optimized: FinalizableState,
) -> Result<SynthesisResult, crate::SynthError> {
    #[cfg(test)]
    let synthesized = {
        let mut synthesized = optimized.ledger.synthesized.take().ok_or_else(|| {
            crate::SynthError::invariant("published artifact retained no word module")
        })?;
        synthesized
            .consolidate_names()
            .map_err(crate::SynthError::from)?;
        synthesized
    };
    // Region workers name their cells from the region identity because they
    // cannot allocate global names in parallel. Those names are long and move
    // whenever partitioning moves, so they are replaced with dense `U<n>`
    // names on the way out.
    optimized
        .mapped
        .assign_publication_names(crate::mapping::is_synthetic_region_cell_name, "U")
        .map_err(crate::SynthError::from)?;
    optimized
        .mapped
        .assign_publication_net_names(crate::mapping::is_synthetic_net_name, "n")
        .map_err(crate::SynthError::from)?;
    let (mapped, cell_remap) = optimized
        .mapped
        .finalize_for_publication()
        .map_err(crate::SynthError::from)?;
    optimized.mapped = mapped;
    optimized
        .implementations
        .remap_cells_for_publication(&cell_remap)?;
    optimized
        .implementations
        .validate_checkpoint(&optimized.mapped)?;
    optimized.operator_manifest.validate_checkpoint()?;
    let report = report::synthesis_report(&optimized.mapped, &optimized.options);
    let reuse = optimized.incremental_reuse;
    let synthesis_regions = reuse
        .regional_decision_hits
        .saturating_add(reuse.regional_decision_misses);
    let sizes = optimized.ledger.sizes;
    let operator_instances = optimized.operator_manifest.instances().len();
    let metrics = SynthesisMetrics {
        source_change: optimized.ledger.source_change,
        normalized_values: sizes.normalized_values,
        normalized_operations: sizes.normalized_operations,
        lowered_values: sizes.lowered_values,
        lowered_operations: sizes.lowered_operations,
        mapped_cells: optimized.mapped.cell_count() + optimized.mapped.design_instance_count(),
        mapped_nets: optimized.mapped.net_count(),
        operator_instances,
        operator_manifest_bytes: optimized.operator_manifest.serialized_size()?,
        boolean_recipe_hits: reuse.boolean_recipe_hits,
        boolean_recipe_misses: reuse.boolean_recipe_misses,
        regional_decision_hits: reuse.regional_decision_hits,
        regional_decision_misses: reuse.regional_decision_misses,
        synthesis_regions,
        regional_cover_plans: synthesis_regions,
        regional_epochs: optimized.ledger.regional_epochs,
        timing_resident_bytes: optimized.ledger.timing_memory.resident_bytes,
        timing_construction_scratch_high_water_bytes: optimized
            .ledger
            .timing_memory
            .construction_scratch_high_water_bytes,
        timing_construction_high_water_bytes: optimized
            .ledger
            .timing_memory
            .construction_high_water_bytes,
    };
    let mut result = SynthesisResult {
        #[cfg(test)]
        module: synthesized,
        mapped: optimized.mapped,
        report,
        implementation_db: optimized.implementations,
        operator_manifest: optimized.operator_manifest,
        timing: optimized.timing,
        metrics,
        incremental: IncrementalSnapshot::new(
            optimized.ledger.source_snapshot,
            optimized.ledger.regional_cache_records,
        ),
    };
    result.compact();
    Ok(result)
}
