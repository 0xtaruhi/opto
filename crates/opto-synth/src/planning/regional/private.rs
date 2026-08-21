// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical sealing before immutable work-graph construction.

use opto_ir::word;

pub(crate) fn seal_work_design(
    module: &mut word::WordModule,
) -> Result<(crate::SynthesisRegionGraph, crate::regional::WorkDesign), crate::SynthError> {
    crate::planning::dataflow::coalesce_static_wire_drivers(module)?;
    crate::planning::dataflow::resolve_static_connect_aliases(module)?;
    module.validate().map_err(crate::SynthError::Word)?;
    let regions = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let design = crate::regional::WorkDesign::seal(module, &regions)?;
    Ok((regions, design))
}
