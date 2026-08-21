// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal sealing followed by one private structural transaction.

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
    let initial_regions = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let initial_design = crate::regional::WorkDesign::seal(&initial_regions)?;
    let work = crate::regional::WorkGraph::build_structural(&initial_regions, &initial_design)?;
    if work.tasks().is_empty() {
        return Ok((initial_regions, initial_design));
    }

    let [(optimized, proof)] = runtime
        .map_ordered_composite(work.tasks(), |_, task_runtime| {
            let mut private = module.clone();
            let proof = crate::planning::fsm::optimize_derived_fsms(
                &mut private,
                timing,
                port_bindings,
                task_runtime,
            )?;
            let canonical =
                crate::planning::dataflow::optimize_combinational_dataflow(&mut private)?;
            crate::planning::dataflow::share_equivalent_sequential_values_by(
                &mut private,
                task_runtime,
                |value| canonical.representatives()[value.index()],
            )?;
            mapping.prepare_private_structure(&mut private, clock_gating, target_mapping)?;
            crate::planning::dataflow::optimize_combinational_dataflow(&mut private)?;
            private.compact_netlist().map_err(crate::SynthError::Word)?;
            private.validate().map_err(crate::SynthError::Word)?;
            Ok::<_, crate::SynthError>((private, proof))
        })?
        .try_into()
        .map_err(|_| crate::SynthError::invariant("structural reduce task produced no result"))?;
    *module = optimized;

    let regions = crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )?;
    let design = initial_design.rewrite_all(&regions, proof)?;
    Ok((regions, design))
}
