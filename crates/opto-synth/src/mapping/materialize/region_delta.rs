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
use crate::mapping::materialize::target_pin_id;
use opto_ir::mapped::{
    AppliedRegionDelta, CellId, CellSpec, ConnectionRef, NetId, RegionDelta, RegionSnapshot,
    TempCellId, TempNetId,
};
use opto_ir::word;
use std::fmt::Write as _;

mod aliases;

pub(crate) use aliases::{MappedValueSignal, WordMappedSignals, regional_binding_values};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ArtifactSignal {
    Mapped(MappedValueSignal),
    LocalNet(usize),
}

impl ArtifactSignal {
    fn connection(self, local_nets: &[TempNetId]) -> Result<ConnectionRef, crate::SynthError> {
        match self {
            Self::Mapped(signal) => Ok(signal.connection()),
            Self::LocalNet(index) => local_nets
                .get(index)
                .copied()
                .map(ConnectionRef::NewNet)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "regional artifact references an unknown local net",
                    )
                }),
        }
    }
}

#[derive(Debug, Clone)]
struct ArtifactCell {
    name: String,
    cell_type: String,
    library_cell: Option<u32>,
    connections: Box<[(String, Option<u16>, ArtifactSignal)]>,
}

/// Immutable mapped topology prepared from one portable library cover.
///
/// The artifact contains no revision-local cell IDs and does not own a mapped
/// transaction. Existing boundary nets are explicit; all other outputs use a
/// dense artifact-local net index resolved only when appended to a delta.
#[derive(Debug, Clone)]
pub(crate) struct MappedRegionArtifact {
    region: crate::RegionAnchorId,
    cells: Box<[ArtifactCell]>,
    internal_net_count: usize,
    external_nets: Box<[NetId]>,
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
        binding: &RegionPlanBinding,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
        mapped_values: &WordMappedSignals,
        catalog: &CombinationalCellCatalog,
        target_cells: &opto_library::TargetCellSet,
    ) -> Result<Self, crate::SynthError> {
        if plan.payload().is_empty() {
            if plan.local_cell_count() != 0
                || plan.local_net_count() != 0
                || plan.local_pin_count() != 0
                || !binding.is_empty()
            {
                return Err(crate::SynthError::invariant(
                    "non-empty regional plan has no portable topology",
                ));
            }
            return Ok(Self {
                region: plan.region(),
                cells: Box::new([]),
                internal_net_count: 0,
                external_nets: Box::new([]),
                roots: Box::new([]),
                leaves: Box::new([]),
            });
        }

        let cover = super::super::cover::decode_portable_cover(plan.payload())?;
        if cover.cells().len() != plan.local_cell_count() as usize
            || cover.outputs().len() != binding.outputs.len()
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

        let input_values = binding.resolve_inputs(ownership)?;
        let inputs = input_values
            .iter()
            .copied()
            .map(|value| mapped_values.require(value).map(ArtifactSignal::Mapped))
            .collect::<Result<Vec<_>, _>>()?;
        let output_values = binding.resolve_outputs(ownership)?;
        let mut output_targets = vec![[None::<MappedValueSignal>; 2]; cover.cells().len()];
        for (&value, source) in output_values.iter().zip(cover.outputs()) {
            let target = mapped_values.require(value)?;
            match *source {
                LibraryCoverSource::Cell(index) => {
                    assign_output_target(&mut output_targets, index, 0, target)?;
                }
                LibraryCoverSource::CellSecond(index) => {
                    assign_output_target(&mut output_targets, index, 1, target)?;
                }
                LibraryCoverSource::Constant(value) => {
                    validate_frozen_output(
                        plan.region(),
                        target,
                        MappedValueSignal::Constant(value),
                    )?;
                }
                LibraryCoverSource::Input(index) => {
                    let source = inputs.get(index).copied().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional cover output references an unknown input",
                        )
                    })?;
                    let ArtifactSignal::Mapped(source) = source else {
                        return Err(crate::SynthError::invariant(
                            "regional cover input is not part of the frozen substrate",
                        ));
                    };
                    validate_frozen_output(plan.region(), target, source)?;
                }
            }
        }
        let mut output_owners = std::collections::BTreeSet::new();
        for outputs in &mut output_targets {
            for target in outputs {
                let Some(signal) = *target else { continue };
                if !output_owners.insert(signal) {
                    // Correlated region inputs can make distinct local cover
                    // nodes resolve to one global value. Keep one physical
                    // driver; later equivalents remain available on local
                    // nets for their own downstream cover consumers.
                    *target = None;
                }
            }
        }
        let mut internal_net_count = 0usize;
        let mut cell_outputs = cover
            .cells()
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                let primary = allocate_output(output_targets[index][0], &mut internal_net_count)?;
                let secondary = cell
                    .second_truth()
                    .map(|_| allocate_output(output_targets[index][1], &mut internal_net_count))
                    .transpose()?;
                Ok([Some(primary), secondary])
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;

        let prefix = region_instance_prefix(plan.region());
        let mut cells = Vec::with_capacity(cover.cells().len());
        let mut pin_count = 0usize;
        for (index, cell) in cover.cells().iter().enumerate() {
            let sources = cell
                .sources()
                .iter()
                .copied()
                .map(|source| resolve_cover_source(source, &inputs, &cell_outputs, index))
                .collect::<Result<Vec<_>, _>>()?;
            let mut primary = cell_outputs[index][0].ok_or_else(|| {
                crate::SynthError::invariant("regional cell has no primary output")
            })?;
            if sources.contains(&primary) && matches!(primary, ArtifactSignal::Mapped(_)) {
                // The frozen substrate already proves this publication target
                // equivalent to a cell input. A region may retain its
                // conservative implementation artifact, but it cannot drive
                // an equivalence class that it also imports.
                primary = allocate_output(None, &mut internal_net_count)?;
                cell_outputs[index][0] = Some(primary);
            }
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
                    let mut secondary = cell_outputs[index][1].ok_or_else(|| {
                        crate::SynthError::invariant("joint regional cell has no secondary output")
                    })?;
                    if sources.contains(&secondary)
                        && matches!(secondary, ArtifactSignal::Mapped(_))
                    {
                        secondary = allocate_output(None, &mut internal_net_count)?;
                        cell_outputs[index][1] = Some(secondary);
                    }
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
            });
        }
        if pin_count != plan.local_pin_count() as usize {
            return Err(crate::SynthError::invariant(
                "regional artifact pin count differs from its portable plan",
            ));
        }
        validate_implementation_cells(plan, &cells)?;

        finish_artifact(
            plan.region(),
            cells.into_boxed_slice(),
            internal_net_count,
            binding,
            ownership,
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
        let mut nets = self.external_nets.to_vec();
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
        let internal_nets = (0..self.internal_net_count)
            .map(|_| delta.add_net(None).map_err(crate::SynthError::from))
            .collect::<Result<Box<[_]>, _>>()?;
        let cells = self
            .cells
            .iter()
            .map(|cell| {
                let mut spec =
                    CellSpec::new(cell.name.clone(), cell.cell_type.clone(), cell.library_cell);
                for (pin, library_pin, signal) in &cell.connections {
                    spec = spec.connect(
                        pin.clone(),
                        *library_pin,
                        signal.connection(&internal_nets)?,
                    );
                }
                delta.add_cell(spec).map_err(crate::SynthError::from)
            })
            .collect::<Result<Box<[_]>, _>>()?;
        Ok(PendingMappedRegion {
            region: self.region,
            cells,
            internal_nets,
            external_nets: self.external_nets.clone(),
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
            .external_nets
            .iter()
            .any(|&net| !snapshot.contains_net(net))
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
    cells: &[ArtifactCell],
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
    target: MappedValueSignal,
    source: MappedValueSignal,
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
    targets: &mut [[Option<MappedValueSignal>; 2]],
    cell: usize,
    output: usize,
    target: MappedValueSignal,
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
    target: Option<MappedValueSignal>,
    local_net_count: &mut usize,
) -> Result<ArtifactSignal, crate::SynthError> {
    if let Some(target @ MappedValueSignal::Net(_)) = target {
        return Ok(ArtifactSignal::Mapped(target));
    }
    // A region output can become a canonical constant after the cover was
    // frozen. Keep the original cell output artifact-local for any internal
    // consumers while the substrate remains the sole constant driver.
    let index = *local_net_count;
    *local_net_count = local_net_count
        .checked_add(1)
        .ok_or_else(|| crate::SynthError::capacity("regional artifact local nets"))?;
    Ok(ArtifactSignal::LocalNet(index))
}

fn resolve_cover_source(
    source: LibraryCoverSource,
    inputs: &[ArtifactSignal],
    cells: &[[Option<ArtifactSignal>; 2]],
    available_cells: usize,
) -> Result<ArtifactSignal, crate::SynthError> {
    match source {
        LibraryCoverSource::Constant(value) => {
            Ok(ArtifactSignal::Mapped(MappedValueSignal::Constant(value)))
        }
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
    cells: Box<[ArtifactCell]>,
    internal_net_count: usize,
    binding: &RegionPlanBinding,
    ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
) -> Result<MappedRegionArtifact, crate::SynthError> {
    let mut external_nets = cells
        .iter()
        .flat_map(|cell| cell.connections.iter())
        .filter_map(|(_, _, signal)| match signal {
            ArtifactSignal::Mapped(MappedValueSignal::Net(net)) => Some(*net),
            ArtifactSignal::Mapped(MappedValueSignal::Constant(_))
            | ArtifactSignal::LocalNet(_) => None,
        })
        .collect::<Vec<_>>();
    external_nets.sort_unstable();
    external_nets.dedup();
    let mut roots = binding.resolve_outputs(ownership)?;
    roots.sort_unstable();
    roots.dedup();
    let mut leaves = binding.resolve_inputs(ownership)?;
    leaves.sort_unstable();
    leaves.dedup();
    Ok(MappedRegionArtifact {
        region,
        cells,
        internal_net_count,
        external_nets: external_nets.into_boxed_slice(),
        roots: roots.into_boxed_slice(),
        leaves: leaves.into_boxed_slice(),
    })
}

/// The prefix every region-scoped synthetic cell name carries.
pub(crate) const REGION_CELL_PREFIX: &str = "__opto_region_";

fn region_instance_prefix(region: crate::RegionAnchorId) -> String {
    let mut prefix = String::with_capacity(79);
    prefix.push_str(REGION_CELL_PREFIX);
    for byte in region.bytes() {
        write!(&mut prefix, "{byte:02x}").expect("writing to String cannot fail");
    }
    prefix.push_str("_cell_");
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_constant_outputs_keep_an_artifact_local_net() {
        let mut local_net_count = 0;

        let output = allocate_output(
            Some(MappedValueSignal::Constant(false)),
            &mut local_net_count,
        )
        .unwrap();

        assert_eq!(output, ArtifactSignal::LocalNet(0));
        assert_eq!(local_net_count, 1);
    }
}
