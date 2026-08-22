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
    let regions = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let design = crate::regional::WorkDesign::seal(module, &regions)?;
    if coalescing.is_empty() {
        return Ok((regions, design));
    }

    // RFC 0013 Amendment 1 publication: deterministic slot assignment and
    // fragment splice into the semantic Word module, followed by an
    // incremental revision update through the changed cone only. The base
    // partition is discarded; rebuilding it over the spliced module keeps one
    // region graph that names every published entity (accepted cutover cost).
    let (wave, signals) = coalescing.into_parts();
    let published = module
        .publish_fragments(wave)
        .map_err(crate::SynthError::Word)?;
    let regions = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let deltas = crate::regional::coalesce_revision_deltas(
        module,
        &regions,
        design.design(),
        &published,
        &signals,
    )?;
    let committed = design
        .design()
        .commit(deltas, crate::regional::validate_coalesce_proof)
        .map_err(|error| {
            crate::SynthError::invariant(format!("static-wire revision commit failed: {error}"))
        })?;
    Ok((
        regions,
        crate::regional::WorkDesign::from_revision(committed),
    ))
}
