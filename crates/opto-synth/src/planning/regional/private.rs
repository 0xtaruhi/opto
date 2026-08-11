// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Owner-confined structural preparation followed by one final region freeze.

use opto_ir::word;

pub(crate) fn optimize_structure(
    module: &mut word::WordModule,
    mapping: &crate::mapping::TargetMappingContext,
    clock_gating: Option<crate::ClockGatingStyle>,
    target_mapping: bool,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<crate::SynthesisRegionGraph, crate::SynthError> {
    crate::planning::dataflow::coalesce_static_wire_drivers(module)?;
    let provisional = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let mut ownership = crate::regional::StructuralOwnershipProvenance::new(module, &provisional)?;

    crate::planning::fsm::optimize_derived_fsms_in_regions(
        module,
        &mut ownership,
        timing,
        port_bindings,
        runtime,
    )?;
    let canonical_values =
        crate::planning::dataflow::optimize_owned_priority_dataflow(module, &mut ownership)?;
    crate::planning::dataflow::share_equivalent_sequential_values_by(
        module,
        runtime,
        ownership.owners(),
        |value| {
            canonical_values
                .representatives()
                .get(value.index())
                .copied()
                .unwrap_or(value)
        },
    )?;
    mapping.publish_owned_preparation(module, clock_gating, target_mapping, &mut ownership)?;
    crate::planning::dataflow::optimize_owned_priority_dataflow(module, &mut ownership)?;

    let final_partition = crate::regional::region_graph::partition::build_with_ownership(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
        &ownership,
    )?;
    ownership.verify_frozen(module, &final_partition)?;
    Ok(final_partition)
}
