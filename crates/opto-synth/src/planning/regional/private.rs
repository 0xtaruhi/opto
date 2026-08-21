// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Private structural normalization before immutable work-graph construction.

use opto_ir::word;

pub(crate) fn optimize_private_structure(
    module: &mut word::WordModule,
    mapping: &crate::mapping::TargetMappingContext,
    clock_gating: Option<crate::ClockGatingStyle>,
    target_mapping: bool,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<(crate::SynthesisRegionGraph, crate::regional::WorkDesign), crate::SynthError> {
    crate::planning::dataflow::coalesce_static_wire_drivers(module)?;
    crate::planning::dataflow::resolve_static_connect_aliases(module)?;
    let mut private = module.clone();
    crate::planning::fsm::optimize_derived_fsms(&mut private, timing, port_bindings, runtime)?;
    let canonical = crate::planning::dataflow::optimize_combinational_dataflow(&mut private)?;
    crate::planning::dataflow::share_equivalent_sequential_values_by(
        &mut private,
        runtime,
        |value| canonical.representatives()[value.index()],
    )?;
    mapping.prepare_private_structure(&mut private, clock_gating, target_mapping)?;
    crate::planning::dataflow::optimize_combinational_dataflow(&mut private)?;
    private.compact_netlist().map_err(crate::SynthError::Word)?;
    private.validate().map_err(crate::SynthError::Word)?;
    *module = private;
    let regions = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let design = crate::regional::WorkDesign::seal(module, &regions)?;
    Ok((regions, design))
}
