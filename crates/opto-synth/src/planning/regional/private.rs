// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical sealing before immutable work-graph construction.

use opto_ir::word;

pub(crate) fn seal_work_design(
    module: &mut word::WordModule,
) -> Result<(crate::SynthesisRegionGraph, crate::regional::WorkDesign), crate::SynthError> {
    // Alias canonicalization runs ahead of structural rewriting so the sealed
    // generation observes the final connect topology.
    crate::planning::dataflow::resolve_static_connect_aliases(module)?;
    let coalescing = crate::planning::dataflow::static_wire_driver_fragments(module)?;
    module.validate().map_err(crate::SynthError::Word)?;
    if coalescing.is_empty() {
        let regions = crate::regional::region_graph::partition::build(
            module,
            crate::regional::region_graph::RegionPartitionPolicy::default(),
        )?;
        let design = crate::regional::WorkDesign::seal(module, &regions)?;
        return Ok((regions, design));
    }

    // RFC 0013 Amendment 1 publication: deterministic slot assignment and
    // fragment splice into the semantic Word module, followed by an
    // incremental revision update through the changed cone only. Partitioning
    // happens once, after the canonical Word splice, so no provisional region
    // geometry can leak into stable revision identity.
    let base = crate::regional::WorkDesign::revision_of(module)?;
    let (wave, signals) = coalescing.into_parts();
    let (_, (regions, committed)) = module.publish_fragments_checked(
        wave,
        |published_module, published| -> Result<_, crate::SynthError> {
            let regions = crate::regional::region_graph::partition::build(
                published_module,
                crate::regional::region_graph::RegionPartitionPolicy::default(),
            )?;
            let deltas = crate::regional::coalesce_revision_deltas(
                published_module,
                &regions,
                base,
                published,
                &signals,
            )?;
            let next = crate::regional::WorkDesign::seal(published_module, &regions)?;
            let committed = crate::regional::commit_coalescing_revision(base, next, &deltas)?;
            Ok((regions, committed))
        },
    )?;
    Ok((regions, committed))
}
