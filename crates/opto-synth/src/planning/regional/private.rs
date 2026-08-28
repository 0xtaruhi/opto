// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Structural preparation from disposable canonical region snapshots.

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
    crate::planning::dataflow::resolve_static_connect_aliases(module)?;
    let partition = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    crate::planning::fsm::optimize_derived_fsms_in_regions(
        module,
        partition.operation_owner_rows(),
        timing,
        port_bindings,
        runtime,
    )?;
    let canonical_values =
        crate::planning::dataflow::optimize_priority_dataflow_in_regions(module)?;
    let partition = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    crate::planning::dataflow::share_equivalent_sequential_values_by(
        module,
        runtime,
        partition.operation_owner_rows(),
        |value| {
            canonical_values
                .representatives()
                .get(value.index())
                .copied()
                .unwrap_or(value)
        },
    )?;
    mapping.prepare_structure(module, clock_gating, target_mapping)?;
    crate::planning::dataflow::optimize_priority_dataflow_in_regions(module)?;

    crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )
}
