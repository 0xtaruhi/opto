// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Owner-confined structural preparation followed by one final region freeze.

use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

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
    let mut owners = provisional.operation_owner_rows().to_vec();

    crate::planning::fsm::optimize_derived_fsms_in_regions(
        module,
        &owners,
        timing,
        port_bindings,
        runtime,
    )?;
    inherit_generated_owners(module, &mut owners, "FSM optimization")?;
    crate::planning::dataflow::rebalance_priority_muxes_in_regions(module, &mut owners)?;
    let canonical_values =
        crate::planning::dataflow::optimize_owned_combinational_dataflow(module, &owners)?;
    crate::planning::dataflow::share_equivalent_sequential_values_by(
        module,
        runtime,
        &owners,
        |value| {
            canonical_values
                .representatives()
                .get(value.index())
                .copied()
                .unwrap_or(value)
        },
    )?;
    mapping.publish_owned_preparation(module, clock_gating, target_mapping, &owners)?;
    inherit_generated_owners(module, &mut owners, "target preparation")?;

    crate::regional::region_graph::partition::build(
        module,
        crate::regional::region_graph::RegionPartitionPolicy::default(),
    )
}

/// Assign generated operations to the owner of their anchored source construct.
fn inherit_generated_owners(
    module: &word::WordModule,
    owners: &mut Vec<Option<crate::RegionRowId>>,
    stage: &'static str,
) -> Result<(), crate::SynthError> {
    if owners.len() > module.operations().len() {
        return Err(crate::SynthError::invariant(
            "regional structural preparation removed operation rows",
        ));
    }
    let drivers = crate::word::signal_driver::SignalDriverIndex::new(module)?;
    let mut source_owners = BTreeMap::<word::SourceIdentity, BTreeSet<crate::RegionRowId>>::new();
    for (operation, owner) in module.operations()[..owners.len()]
        .iter()
        .zip(owners.iter().copied())
    {
        let identity = operation.source.identity().ok_or_else(|| {
            crate::SynthError::invariant("owned structural operation has no stable source identity")
        })?;
        if let Some(owner) = owner {
            source_owners.entry(identity).or_default().insert(owner);
        }
    }
    while owners.len() < module.operations().len() {
        let operation_index = owners.len();
        let operation = &module.operations()[operation_index];
        let identity = operation.source.identity().ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "{stage} emitted an operation without a stable source identity"
            ))
        })?;
        let anchored_owners = source_owners.get(&identity).cloned().unwrap_or_default();
        let mut candidates = BTreeSet::new();
        for input in crate::word::operation_inputs(&operation.kind) {
            collect_value_owners(module, &drivers, owners, input, &mut candidates)?;
        }
        let owner = if anchored_owners.len() == 1 {
            anchored_owners.first().copied()
        } else if candidates.len() <= 1 {
            candidates.first().copied()
        } else {
            return Err(crate::SynthError::invariant(format!(
                "{stage} operation {operation_index} has ambiguous source owners {anchored_owners:?} and reads boundary inputs from owners {candidates:?}"
            )));
        };
        if let Some(owner) = owner {
            source_owners.entry(identity).or_default().insert(owner);
        }
        owners.push(owner);
    }
    Ok(())
}

fn collect_value_owners(
    module: &word::WordModule,
    drivers: &crate::word::signal_driver::SignalDriverIndex,
    owners: &[Option<crate::RegionRowId>],
    value: word::ValueId,
    candidates: &mut BTreeSet<crate::RegionRowId>,
) -> Result<(), crate::SynthError> {
    let value = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant("generated operation references an unknown value")
    })?;
    match value.kind {
        word::ValueKind::Operation(operation) => {
            if let Some(owner) = owners.get(operation.index()).copied().flatten() {
                candidates.insert(owner);
            }
        }
        word::ValueKind::Signal(reference) => {
            for driver in drivers.reference_drivers(reference).into_iter().flatten() {
                let Some(word::ValueKind::Operation(operation)) =
                    module.value(driver).map(|value| &value.kind)
                else {
                    continue;
                };
                if let Some(owner) = owners.get(operation.index()).copied().flatten() {
                    candidates.insert(owner);
                }
            }
        }
        word::ValueKind::Constant(_) => {}
    }
    Ok(())
}
