// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Direct materialization of portable regional covers into mapped edits.
//!
//! This is deliberately a pure preparation layer. It never snapshots, applies,
//! commits, or rolls back a [`RegionDelta`]. The epoch coordinator can therefore
//! append every dirty region to one transaction, while workers independently
//! decode and bind immutable [`MappedRegionArtifact`] values.

use crate::boolean::logic::{LogicInputs, LogicSignature};
use crate::mapping::RegionPlanBinding;
use crate::mapping::cover::{LibraryCoverBinding, LibraryCoverSource};
use crate::mapping::library::CombinationalCellCatalog;
use crate::mapping::materialize::{
    ArtifactCell, ArtifactNetBinding, ArtifactNetTable, ArtifactSignal, target_pin_id,
    validate_artifact_nets,
};
use opto_ir::mapped::{
    AppliedRegionDelta, CellId, NetId, RegionDelta, RegionSnapshot, TempCellId, TempNetId,
};
use opto_ir::word;

mod aliases;

pub(crate) use aliases::{MappedValueSignal, WordMappedSignals, regional_binding_values};

/// Immutable mapped topology prepared from one portable library cover.
///
/// The artifact contains no revision-local cell IDs and does not own a mapped
/// transaction. Existing boundary nets are explicit; all other outputs use a
/// dense artifact-local net index resolved only when appended to a delta.
#[derive(Debug, Clone)]
pub(crate) struct MappedRegionArtifact {
    region: crate::RegionAnchorId,
    nets: ArtifactNetTable,
    cells: Box<[ArtifactCell<()>]>,
    roots: Box<[word::ValueId]>,
    leaves: Box<[word::ValueId]>,
}

/// Stable mapped footprint owned by one committed regional artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MappedRegionFootprint {
    region: crate::RegionAnchorId,
    cells: Box<[CellId]>,
    internal_nets: Box<[NetId]>,
    external_nets: Box<[NetId]>,
}

impl MappedRegionFootprint {
    pub(crate) fn cells(&self) -> &[CellId] {
        &self.cells
    }
}

/// Delta-local IDs retained until the mapped transaction succeeds.
#[derive(Debug)]
pub(crate) struct PendingMappedRegion {
    region: crate::RegionAnchorId,
    cells: Box<[TempCellId]>,
    internal_nets: Box<[TempNetId]>,
    external_nets: Box<[NetId]>,
}

impl PendingMappedRegion {
    /// Resolves temporary IDs after the caller's mapped/timing transaction has
    /// applied the delta. No state is changed here.
    pub(crate) fn resolve(
        self,
        applied: &AppliedRegionDelta,
    ) -> Result<MappedRegionFootprint, crate::SynthError> {
        let cells = self
            .cells
            .iter()
            .map(|&cell| {
                applied.added_cell(cell).ok_or_else(|| {
                    crate::SynthError::invariant("applied regional delta lost a materialized cell")
                })
            })
            .collect::<Result<Box<[_]>, _>>()?;
        let internal_nets = self
            .internal_nets
            .iter()
            .map(|&net| {
                applied.added_net(net).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "applied regional delta lost a materialized local net",
                    )
                })
            })
            .collect::<Result<Box<[_]>, _>>()?;
        Ok(MappedRegionFootprint {
            region: self.region,
            cells,
            internal_nets,
            external_nets: self.external_nets,
        })
    }
}

impl MappedRegionArtifact {
    pub(crate) fn from_library_plan(
        plan: &crate::RegionCoverPlan,
        plan_binding: &RegionPlanBinding,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
        mapped_values: &WordMappedSignals,
        regional_pins: &super::RegionalMappedPins,
        catalog: &CombinationalCellCatalog,
        target_cells: &opto_library::TargetCellSet,
    ) -> Result<Self, crate::SynthError> {
        if plan.payload().is_empty() {
            if plan.local_cell_count() != 0
                || plan.local_net_count() != 0
                || plan.local_pin_count() != 0
                || !plan_binding.is_empty()
            {
                return Err(crate::SynthError::invariant(
                    "non-empty regional plan has no portable topology",
                ));
            }
            return Ok(Self {
                region: plan.region(),
                nets: ArtifactNetTable::default(),
                cells: Box::new([]),
                roots: Box::new([]),
                leaves: Box::new([]),
            });
        }

        let cover = super::super::cover::decode_portable_cover(plan.payload())?;
        if cover.cells().len() != plan.local_cell_count() as usize
            || cover.outputs().len() != plan_binding.outputs.len()
        {
            return Err(crate::SynthError::invariant(
                "portable regional plan does not match its revision binding",
            ));
        }
        let expected_nets = cover.cells().iter().try_fold(0u32, |count, cell| {
            count
                .checked_add(if cell.second_truth().is_some() { 2 } else { 1 })
                .ok_or_else(|| crate::SynthError::capacity("portable regional plan nets"))
        })?;
        if expected_nets != plan.local_net_count() {
            return Err(crate::SynthError::invariant(
                "portable regional plan net count is inconsistent",
            ));
        }

        let mut nets = ArtifactNetTable::default();
        let mut input_values = plan_binding.resolve_inputs(region_binding)?.into_iter();
        let inputs = plan_binding
            .inputs
            .iter()
            .copied()
            .map(|binding| {
                let signal = match binding {
                    crate::mapping::RegionPlanValueBinding::ArtifactPinBit { pin, .. } => {
                        MappedValueSignal::Net(regional_pins.require(pin)?)
                    }
                    _ => mapped_values.require(input_values.next().ok_or_else(|| {
                        crate::SynthError::invariant("regional input binding has no lowered value")
                    })?)?,
                };
                Ok::<_, crate::SynthError>(nets.signal(signal))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut output_targets = vec![[None::<ArtifactSignal>; 2]; cover.cells().len()];
        for (&binding, source) in plan_binding.outputs.iter().zip(cover.outputs()) {
            let target = match binding {
                crate::mapping::RegionPlanValueBinding::Lowered(value) => {
                    nets.signal(mapped_values.require(value)?)
                }
                crate::mapping::RegionPlanValueBinding::ArtifactPinBit { pin, .. } => {
                    nets.signal(MappedValueSignal::Net(regional_pins.require(pin)?))
                }
                crate::mapping::RegionPlanValueBinding::SourceBit { .. }
                | crate::mapping::RegionPlanValueBinding::MemoryLogicBit { .. }
                | crate::mapping::RegionPlanValueBinding::MemoryStateBit { .. } => {
                    return Err(crate::SynthError::invariant(
                        "regional output binding was not materialized against global lowering",
                    ));
                }
            };
            match *source {
                LibraryCoverSource::Cell(index) => {
                    assign_output_target(&mut output_targets, index, 0, target)?;
                }
                LibraryCoverSource::CellSecond(index) => {
                    assign_output_target(&mut output_targets, index, 1, target)?;
                }
                LibraryCoverSource::Constant(value) => {
                    validate_frozen_output(plan.region(), target, ArtifactSignal::Constant(value))?;
                }
                LibraryCoverSource::Input(index) => {
                    let source = inputs.get(index).copied().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional cover output references an unknown input",
                        )
                    })?;
                    validate_frozen_output(plan.region(), target, source)?;
                }
            }
        }
        let cell_outputs = cover
            .cells()
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let primary = allocate_output(output_targets[index][0], &mut nets)?;
                let secondary = cell
                    .second_truth()
                    .map(|_| allocate_output(output_targets[index][1], &mut nets))
                    .transpose()?;
                Ok([Some(primary), secondary])
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let prefix = super::region_instance_prefix(plan.region());
        let mut cells = Vec::with_capacity(cover.cells().len());
        let mut pin_count = 0usize;
        for (index, cell) in cover.cells().iter().enumerate() {
            let sources = cell
                .sources()
                .iter()
                .copied()
                .map(|source| resolve_cover_source(source, &inputs, &cell_outputs, index))
                .collect::<Result<Vec<_>, _>>()?;
            let primary = cell_outputs[index][0].ok_or_else(|| {
                crate::SynthError::invariant("regional cell has no primary output")
            })?;
            let (mapped, library_cell) = match cell.binding(catalog)? {
                LibraryCoverBinding::Single(binding) => {
                    if cell_outputs[index][1].is_some() {
                        return Err(crate::SynthError::invariant(
                            "single-output regional cell has a secondary output",
                        ));
                    }
                    let (signature, synthetic) = synthetic_signature(&sources, cell.truth())?;
                    let output = synthetic_value(sources.len())?;
                    let mapped = catalog.cell_for_binding(binding, &signature, output);
                    let library_cell = catalog.binding_library_cell(binding)?;
                    (
                        bind_mapped_cell(
                            mapped,
                            library_cell,
                            &synthetic,
                            &[primary],
                            target_cells,
                        )?,
                        library_cell,
                    )
                }
                LibraryCoverBinding::Joint(binding) => {
                    let secondary = cell_outputs[index][1].ok_or_else(|| {
                        crate::SynthError::invariant("joint regional cell has no secondary output")
                    })?;
                    if sources.contains(&primary) || sources.contains(&secondary) {
                        return Err(crate::SynthError::invariant(format!(
                            "regional joint cover cell {index} in {:?} has a local output cycle",
                            plan.region()
                        )));
                    }
                    let (signature, synthetic) = synthetic_signature(&sources, cell.truth())?;
                    let outputs = [
                        synthetic_value(sources.len())?,
                        synthetic_value(sources.len() + 1)?,
                    ];
                    let mapped = catalog.joint_cell(binding, &signature, outputs);
                    let library_cell = catalog.joint_binding_library_cell(binding)?;
                    (
                        bind_mapped_cell(
                            mapped,
                            library_cell,
                            &synthetic,
                            &[primary, secondary],
                            target_cells,
                        )?,
                        library_cell,
                    )
                }
            };
            if sources.contains(&primary) {
                return Err(crate::SynthError::invariant(format!(
                    "regional cover cell {index} in {:?} has a local output cycle",
                    plan.region()
                )));
            }
            if mapped.library_cell != library_cell {
                return Err(crate::SynthError::invariant(
                    "regional cell binding resolved to inconsistent library IDs",
                ));
            }
            pin_count = pin_count
                .checked_add(mapped.connections.len())
                .ok_or_else(|| crate::SynthError::capacity("regional artifact pin count"))?;
            cells.push(ArtifactCell {
                name: format!("{prefix}{index}"),
                cell_type: mapped.cell_type,
                library_cell: Some(library_cell),
                connections: mapped.connections,
                metadata: (),
            });
        }
        if pin_count != plan.local_pin_count() as usize {
            return Err(crate::SynthError::invariant(
                "regional artifact pin count differs from its portable plan",
            ));
        }
        validate_artifact_nets(
            &format!("regional artifact {:?}", plan.region()),
            &nets,
            &cells,
            target_cells,
        )?;
        validate_implementation_cells(plan, &cells)?;

        finish_artifact(
            plan.region(),
            nets,
            cells.into_boxed_slice(),
            plan_binding,
            region_binding,
        )
    }

    pub(crate) const fn region(&self) -> crate::RegionAnchorId {
        self.region
    }

    pub(crate) fn roots(&self) -> &[word::ValueId] {
        &self.roots
    }

    pub(crate) fn leaves(&self) -> &[word::ValueId] {
        &self.leaves
    }

    pub(crate) fn validate_materialization(
        &self,
        footprint: &MappedRegionFootprint,
        mapped: &opto_ir::mapped::MappedNetlist,
    ) -> Result<(), crate::SynthError> {
        if self.cells.len() != footprint.cells.len()
            || self.nets.local_count() != footprint.internal_nets.len()
        {
            return Err(crate::SynthError::invariant(
                "materialized regional footprint has the wrong shape",
            ));
        }
        for (index, (expected, &cell)) in self.cells.iter().zip(&footprint.cells).enumerate() {
            let actual = mapped.connections(cell).ok_or_else(|| {
                crate::SynthError::invariant("materialized regional cell is not live")
            })?;
            if actual.len() != expected.connections.len() {
                return Err(crate::SynthError::invariant(format!(
                    "materialized regional cell {index} has {} pins instead of {}",
                    actual.len(),
                    expected.connections.len()
                )));
            }
            for ((pin, library_pin, signal), actual) in expected.connections.iter().zip(actual) {
                let expected_signal = match *signal {
                    ArtifactSignal::Constant(value) => {
                        opto_ir::mapped::ConnectionSignal::Constant(value)
                    }
                    ArtifactSignal::Net(id) => match self.nets.binding(id).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional artifact references an unknown sealed net",
                        )
                    })? {
                        ArtifactNetBinding::External { net, .. } => {
                            opto_ir::mapped::ConnectionSignal::Net(net)
                        }
                        ArtifactNetBinding::Local(net) => opto_ir::mapped::ConnectionSignal::Net(
                            *footprint.internal_nets.get(net).ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "regional connection references an unknown materialized net",
                                )
                            })?,
                        ),
                    },
                };
                if mapped.pin_name(actual) != Some(pin.as_str())
                    || actual.library_pin != *library_pin
                    || actual.signal != expected_signal
                {
                    return Err(crate::SynthError::invariant(format!(
                        "materialized regional cell {index} pin '{pin}' differs from its sealed artifact"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Returns every cell that the caller must include in the shared snapshot.
    pub(crate) fn required_cells(
        &self,
        previous: Option<&MappedRegionFootprint>,
    ) -> Result<Box<[CellId]>, crate::SynthError> {
        self.validate_previous(previous)?;
        Ok(previous.map_or_else(Box::<[CellId]>::default, |old| old.cells.clone()))
    }

    /// Returns every external/read or previous internal net that the caller
    /// must include in the shared snapshot.
    pub(crate) fn required_nets(
        &self,
        previous: Option<&MappedRegionFootprint>,
    ) -> Result<Box<[NetId]>, crate::SynthError> {
        self.validate_previous(previous)?;
        let mut nets = self.nets.external_nets().collect::<Vec<_>>();
        if let Some(previous) = previous {
            nets.extend_from_slice(&previous.internal_nets);
            nets.extend_from_slice(&previous.external_nets);
        }
        nets.sort_unstable();
        nets.dedup();
        Ok(nets.into_boxed_slice())
    }

    /// Appends this region to a caller-owned generation delta. The same method
    /// handles first installation and replacement of an earlier footprint.
    pub(crate) fn append_to_delta(
        &self,
        delta: &mut RegionDelta,
        previous: Option<&MappedRegionFootprint>,
    ) -> Result<PendingMappedRegion, crate::SynthError> {
        self.validate_snapshot(delta.snapshot(), previous)?;
        if let Some(previous) = previous {
            for &cell in &previous.cells {
                delta.remove_cell(cell).map_err(crate::SynthError::from)?;
            }
            for &net in &previous.internal_nets {
                delta.remove_net(net).map_err(crate::SynthError::from)?;
            }
        }
        let (internal_nets, cells) = super::append_artifact_cells(
            delta,
            &self.nets,
            &self.cells,
            "regional artifact references an unknown local net",
            |cell, &()| cell,
        )?;
        Ok(PendingMappedRegion {
            region: self.region,
            cells,
            internal_nets,
            external_nets: self.nets.external_nets().collect(),
        })
    }

    fn validate_previous(
        &self,
        previous: Option<&MappedRegionFootprint>,
    ) -> Result<(), crate::SynthError> {
        if previous.is_some_and(|previous| previous.region != self.region) {
            return Err(crate::SynthError::invariant(
                "regional replacement footprint belongs to another region",
            ));
        }
        Ok(())
    }

    fn validate_snapshot(
        &self,
        snapshot: &RegionSnapshot,
        previous: Option<&MappedRegionFootprint>,
    ) -> Result<(), crate::SynthError> {
        self.validate_previous(previous)?;
        if self
            .nets
            .external_nets()
            .any(|net| !snapshot.contains_net(net))
        {
            return Err(crate::SynthError::invariant(
                "regional artifact boundary net is absent from its transaction snapshot",
            ));
        }
        if let Some(previous) = previous
            && (previous
                .cells
                .iter()
                .any(|&cell| !snapshot.contains_cell(cell))
                || previous
                    .internal_nets
                    .iter()
                    .chain(previous.external_nets.iter())
                    .any(|&net| !snapshot.contains_net(net)))
        {
            return Err(crate::SynthError::invariant(
                "regional replacement footprint is absent from its transaction snapshot",
            ));
        }
        Ok(())
    }
}

struct BoundArtifactCell {
    cell_type: String,
    library_cell: u32,
    connections: Box<[(String, Option<u16>, ArtifactSignal)]>,
}

fn validate_implementation_cells(
    plan: &crate::RegionCoverPlan,
    cells: &[ArtifactCell<()>],
) -> Result<(), crate::SynthError> {
    let mut implementation = cells
        .iter()
        .map(|cell| {
            Ok(crate::regional::RegionImplementationCell {
                cell_name: cell.cell_type.clone().into_boxed_str(),
                pin_count: u32::try_from(cell.connections.len())
                    .map_err(|_| crate::SynthError::capacity("regional artifact cell pin count"))?,
            })
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    implementation.sort();
    if implementation.as_slice() != plan.implementation_cells() {
        return Err(crate::SynthError::invariant(
            "regional artifact cell census differs from its portable plan",
        ));
    }
    Ok(())
}

fn bind_mapped_cell(
    mapped: crate::mapping::MappedCell,
    library_cell: u32,
    synthetic_inputs: &[ArtifactSignal],
    outputs: &[ArtifactSignal],
    target_cells: &opto_library::TargetCellSet,
) -> Result<BoundArtifactCell, crate::SynthError> {
    let target = target_cells
        .get(library_cell as usize)
        .ok_or_else(|| crate::SynthError::invariant("regional target-cell index disappeared"))?;
    if target.name() != mapped.cell_name {
        return Err(crate::SynthError::invariant(
            "regional binding resolved to a mismatched target-cell index",
        ));
    }
    let mut connections = Vec::with_capacity(
        mapped
            .input_connections
            .len()
            .saturating_add(mapped.output_connections.len()),
    );
    for connection in mapped.input_connections {
        let signal = synthetic_inputs
            .get(connection.value.index())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant("regional binding produced an unknown synthetic input")
            })?;
        connections.push((
            connection.pin.clone(),
            Some(target_pin_id(target, &connection.pin)?),
            signal,
        ));
    }
    for connection in mapped.output_connections {
        let output = connection
            .value
            .index()
            .checked_sub(synthetic_inputs.len())
            .and_then(|index| outputs.get(index))
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional binding produced an unknown synthetic output",
                )
            })?;
        connections.push((
            connection.pin.clone(),
            Some(target_pin_id(target, &connection.pin)?),
            output,
        ));
    }
    Ok(BoundArtifactCell {
        cell_type: mapped.cell_name,
        library_cell,
        connections: connections.into_boxed_slice(),
    })
}

fn synthetic_signature(
    inputs: &[ArtifactSignal],
    truth: crate::boolean::logic::TruthTable,
) -> Result<(LogicSignature, Box<[ArtifactSignal]>), crate::SynthError> {
    let values = (0..inputs.len())
        .map(synthetic_value)
        .collect::<Result<Vec<_>, _>>()?;
    let inputs_for_signature = LogicInputs::from_slice(&values).ok_or_else(|| {
        crate::SynthError::invariant("portable regional cell exceeds signature capacity")
    })?;
    Ok((
        LogicSignature {
            inputs: inputs_for_signature,
            truth,
        },
        inputs.to_vec().into_boxed_slice(),
    ))
}

fn synthetic_value(index: usize) -> Result<word::ValueId, crate::SynthError> {
    word::ValueId::from_index(index).map_err(crate::SynthError::from)
}

fn validate_frozen_output(
    region: crate::RegionAnchorId,
    target: ArtifactSignal,
    source: ArtifactSignal,
) -> Result<(), crate::SynthError> {
    if target != source {
        return Err(crate::SynthError::invariant(format!(
            "regional cover for {region:?} simplified an output from frozen \
             substrate signal {target:?} to {source:?}"
        )));
    }
    Ok(())
}

fn assign_output_target(
    targets: &mut [[Option<ArtifactSignal>; 2]],
    cell: usize,
    output: usize,
    target: ArtifactSignal,
) -> Result<(), crate::SynthError> {
    let slot = targets
        .get_mut(cell)
        .and_then(|outputs| outputs.get_mut(output))
        .ok_or_else(|| crate::SynthError::invariant("regional output cell is unknown"))?;
    if slot.is_some_and(|current| current != target) {
        return Err(crate::SynthError::invariant(
            "one regional cell output maps to two substrate signals",
        ));
    }
    *slot = Some(target);
    Ok(())
}

fn allocate_output(
    target: Option<ArtifactSignal>,
    nets: &mut ArtifactNetTable,
) -> Result<ArtifactSignal, crate::SynthError> {
    // A constant or a correlated duplicate has no new boundary producer. Its
    // implementation output remains local for downstream cover consumers.
    nets.claim_output(target)
}

fn resolve_cover_source(
    source: LibraryCoverSource,
    inputs: &[ArtifactSignal],
    cells: &[[Option<ArtifactSignal>; 2]],
    available_cells: usize,
) -> Result<ArtifactSignal, crate::SynthError> {
    match source {
        LibraryCoverSource::Constant(value) => Ok(ArtifactSignal::Constant(value)),
        LibraryCoverSource::Input(index) => inputs.get(index).copied().ok_or_else(|| {
            crate::SynthError::invariant("regional cover input exceeds its revision binding")
        }),
        LibraryCoverSource::Cell(index) | LibraryCoverSource::CellSecond(index) => {
            if index >= available_cells {
                return Err(crate::SynthError::invariant(
                    "regional cover source is not topologically available",
                ));
            }
            let output = usize::from(matches!(source, LibraryCoverSource::CellSecond(_)));
            cells
                .get(index)
                .and_then(|outputs| outputs[output])
                .ok_or_else(|| crate::SynthError::invariant("regional cover output is absent"))
        }
    }
}

fn finish_artifact(
    region: crate::RegionAnchorId,
    nets: ArtifactNetTable,
    cells: Box<[ArtifactCell<()>]>,
    plan_binding: &RegionPlanBinding,
    region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
) -> Result<MappedRegionArtifact, crate::SynthError> {
    let mut roots = plan_binding.resolve_outputs(region_binding)?;
    roots.sort_unstable();
    roots.dedup();
    let mut leaves = plan_binding.resolve_inputs(region_binding)?;
    leaves.sort_unstable();
    leaves.dedup();
    Ok(MappedRegionArtifact {
        region,
        nets,
        cells,
        roots: roots.into_boxed_slice(),
        leaves: leaves.into_boxed_slice(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::materialize::ArtifactNetId;

    #[test]
    fn canonical_constant_outputs_keep_an_artifact_local_net() {
        let mut nets = ArtifactNetTable::default();

        let output = allocate_output(Some(ArtifactSignal::Constant(false)), &mut nets).unwrap();

        assert_eq!(output, ArtifactSignal::Net(ArtifactNetId(0)));
        assert_eq!(
            nets.binding(ArtifactNetId(0)),
            Some(ArtifactNetBinding::Local(0))
        );
        assert_eq!(nets.local_count(), 1);
    }
}
