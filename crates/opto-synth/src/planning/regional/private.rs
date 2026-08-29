// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Structural preparation from disposable canonical region snapshots.

use opto_ir::word;

/// Borrowed configuration for one canonical structural-preparation pass.
#[derive(Clone, Copy)]
pub(crate) struct StructureOptimizationRequest<'a> {
    pub(crate) mapping: &'a crate::mapping::TargetMappingContext,
    pub(crate) clock_gating: Option<crate::ClockGatingStyle>,
    pub(crate) target_mapping: bool,
    pub(crate) timing: &'a opto_timing::TimingContext,
    pub(crate) port_bindings: &'a opto_timing::PortBindings,
    pub(crate) runtime: &'a opto_runtime::ExecutionContext,
    pub(crate) partition_policy: crate::regional::region_graph::RegionPartitionPolicy,
}

pub(crate) fn optimize_structure(
    module: &mut word::WordModule,
    request: StructureOptimizationRequest<'_>,
) -> Result<crate::SynthesisRegionGraph, crate::SynthError> {
    let StructureOptimizationRequest {
        mapping,
        clock_gating,
        target_mapping,
        timing,
        port_bindings,
        runtime,
        partition_policy,
    } = request;
    crate::planning::dataflow::coalesce_static_wire_drivers(module)?;
    crate::planning::dataflow::resolve_static_connect_aliases(module)?;
    let partition = crate::regional::region_graph::partition::build(module, partition_policy)?;
    crate::planning::fsm::optimize_derived_fsms_in_regions(
        module,
        partition.operation_owner_rows(),
        timing,
        port_bindings,
        runtime,
    )?;
    let canonical_values =
        crate::planning::dataflow::optimize_priority_dataflow_in_regions(module)?;
    let partition = crate::regional::region_graph::partition::build(module, partition_policy)?;
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
    mapping.prepare_structure(module, clock_gating, target_mapping, partition_policy)?;
    crate::planning::dataflow::optimize_priority_dataflow_in_regions(module)?;

    crate::regional::region_graph::partition::build(module, partition_policy)
}
