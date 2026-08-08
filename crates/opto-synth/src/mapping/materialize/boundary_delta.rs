// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Generation-local materialization of portable boundary-repair artifacts.

use super::MappedCellSource;
use crate::artifact::provenance::ProvenanceBuilder;
use crate::regional::{
    BoundaryRepairArtifactRecord, BoundaryRepairEndpoint, BoundaryRepairSchema,
    BoundaryRepairSignal,
};
use opto_ir::mapped::{
    AppliedRegionDelta, CellId, CellSpec, ConnectionRef, ConnectionSignal, MappedGenerationId,
    MappedNetlist, NetId, PinId, RegionDelta, TempCellId, TempNetId,
};
use opto_library::TargetCellSet;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MappedBoundaryRepairFootprint {
    mapped_generation: MappedGenerationId,
    semantic_identity: [u8; 32],
    artifact_generation: [u8; 32],
    driver: crate::RegionAnchorId,
    sink: crate::RegionAnchorId,
    cells: Box<[CellId]>,
    local_nets: Box<[NetId]>,
    sink_pins: Box<[PinId]>,
}

impl MappedBoundaryRepairFootprint {
    pub(crate) fn validate_generation(
        &self,
        mapped: &MappedNetlist,
    ) -> Result<(), crate::SynthError> {
        if self.mapped_generation != mapped.generation_id()
            || self.semantic_identity == [0; 32]
            || self.artifact_generation == [0; 32]
            || self.driver == self.sink
            || self.cells.is_empty()
            || self.sink_pins.is_empty()
            || self.cells.windows(2).any(|pair| pair[0] >= pair[1])
            || self.local_nets.windows(2).any(|pair| pair[0] >= pair[1])
            || self.sink_pins.windows(2).any(|pair| pair[0] >= pair[1])
            || self.cells.iter().any(|&cell| !mapped.is_live_cell(cell))
            || self.local_nets.iter().any(|&net| !mapped.is_live_net(net))
            || self
                .sink_pins
                .iter()
                .any(|&pin| mapped.connection(pin).is_none())
        {
            return Err(crate::SynthError::invariant(
                "boundary-repair footprint belongs to another mapped generation",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_sources(
        &self,
        sources: &[Option<MappedCellSource>],
    ) -> Result<(), crate::SynthError> {
        if self.cells.iter().any(|&cell| {
            !matches!(
                sources.get(cell.index()).and_then(Option::as_ref),
                Some(MappedCellSource::Boundary { driver, sink, .. })
                    if (*driver, *sink) == (self.driver, self.sink)
            )
        }) {
            return Err(crate::SynthError::invariant(
                "boundary-repair exact footprint disagrees with mapped provenance",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct PreparedCell {
    name: String,
    cell_type: String,
    library_cell: u32,
    origins: crate::artifact::implementation::OriginSetId,
    pins: Vec<(String, u16, PreparedSignal)>,
}

#[derive(Debug, Clone, Copy)]
enum PreparedSignal {
    Constant(bool),
    External(NetId),
    Local(usize),
}

#[derive(Debug)]
pub(crate) struct PreparedBoundaryRepair {
    semantic_identity: [u8; 32],
    artifact_generation: [u8; 32],
    driver: crate::RegionAnchorId,
    sink: crate::RegionAnchorId,
    local_net_names: Box<[Option<String>]>,
    cells: Box<[PreparedCell]>,
    sinks: Box<[(PinId, usize)]>,
    required_cells: Box<[CellId]>,
    required_nets: Box<[NetId]>,
}

#[derive(Debug)]
pub(crate) struct PendingBoundaryRepair {
    semantic_identity: [u8; 32],
    artifact_generation: [u8; 32],
    driver: crate::RegionAnchorId,
    sink: crate::RegionAnchorId,
    cells: Box<[(TempCellId, crate::artifact::implementation::OriginSetId)]>,
    local_nets: Box<[TempNetId]>,
    sink_pins: Box<[PinId]>,
}

pub(crate) struct BoundaryRepairPublication {
    pub(crate) footprint: MappedBoundaryRepairFootprint,
    pub(crate) sources: Box<[(CellId, MappedCellSource)]>,
}

impl PreparedBoundaryRepair {
    pub(crate) fn prepare_all(
        records: &[BoundaryRepairArtifactRecord],
        schema: &BoundaryRepairSchema,
        mapped: &MappedNetlist,
        cell_sources: &[Option<MappedCellSource>],
        provenance: &mut ProvenanceBuilder,
        library: &TargetCellSet,
    ) -> Result<Box<[Self]>, crate::SynthError> {
        let mut ordered = records
            .iter()
            .filter_map(|record| match schema.matches(record) {
                Ok(true) => Some(Ok(record)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        ordered.sort_unstable_by_key(|record| record.semantic_identity());
        if ordered.windows(2).any(|pair| {
            pair[0].semantic_identity() >= pair[1].semantic_identity()
                || (pair[0].driver(), pair[0].sink()) == (pair[1].driver(), pair[1].sink())
        }) {
            return Err(crate::SynthError::invariant(
                "boundary-repair restore contains duplicate edge artifacts",
            ));
        }
        ordered
            .into_iter()
            .map(|record| Self::prepare(record, mapped, cell_sources, provenance, library))
            .collect()
    }

    fn prepare(
        record: &BoundaryRepairArtifactRecord,
        mapped: &MappedNetlist,
        cell_sources: &[Option<MappedCellSource>],
        provenance: &mut ProvenanceBuilder,
        library: &TargetCellSet,
    ) -> Result<Self, crate::SynthError> {
        for cell in record.cells() {
            if mapped
                .cell_ids()
                .any(|candidate| mapped.cell_name(candidate) == Some(cell.name()))
            {
                return Err(crate::SynthError::invariant(
                    "cached boundary-repair cell collides with current mapped topology",
                ));
            }
            let target = library
                .get(cell.library_cell() as usize)
                .filter(|target| target.name() == cell.cell_type())
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "cached boundary-repair cell failed target-library reconstruction",
                    )
                })?;
            for pin in cell.pins() {
                if target
                    .pins()
                    .nth(pin.library_pin() as usize)
                    .is_none_or(|target_pin| target_pin.name() != pin.name())
                {
                    return Err(crate::SynthError::invariant(
                        "cached boundary-repair pin failed target-library reconstruction",
                    ));
                }
            }
        }

        let external_nets = record
            .external_nets()
            .iter()
            .map(|external| {
                let named = external
                    .name()
                    .map(|name| find_unique_net(mapped, name))
                    .transpose()?;
                let driven = external
                    .driver()
                    .map(|endpoint| {
                        let (cell, pin) = resolve_endpoint(mapped, endpoint)?;
                        if ownership_endpoint(cell_sources, cell)? != Some(record.driver()) {
                            return Err(crate::SynthError::invariant(
                                "cached boundary-repair driver anchor changed ownership",
                            ));
                        }
                        match mapped.connection(pin).map(|connection| connection.signal) {
                            Some(ConnectionSignal::Net(net)) => Ok(net),
                            _ => Err(crate::SynthError::invariant(
                                "cached boundary-repair driver anchor is no longer a net",
                            )),
                        }
                    })
                    .transpose()?;
                match (named, driven) {
                    (Some(named), Some(driven)) if named != driven => {
                        Err(crate::SynthError::invariant(
                            "cached boundary-repair external anchors resolve differently",
                        ))
                    }
                    (Some(net), _) | (_, Some(net)) => Ok(net),
                    (None, None) => Err(crate::SynthError::invariant(
                        "cached boundary-repair external net has no resolvable anchor",
                    )),
                }
            })
            .collect::<Result<Box<[_]>, crate::SynthError>>()?;

        let sinks = record
            .sinks()
            .iter()
            .map(|sink| {
                let (cell, pin) = resolve_endpoint(mapped, sink.endpoint())?;
                if ownership_endpoint(cell_sources, cell)? != Some(record.sink()) {
                    return Err(crate::SynthError::invariant(
                        "cached boundary-repair sink anchor changed ownership",
                    ));
                }
                Ok((pin, sink.local_net() as usize))
            })
            .collect::<Result<Box<[_]>, crate::SynthError>>()?;
        let required_cells = sinks
            .iter()
            .map(|&(pin, _)| {
                mapped.pin_owner(pin).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "cached boundary-repair sink pin has no live owner",
                    )
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut required_nets = external_nets.iter().copied().collect::<BTreeSet<_>>();
        for &(pin, _) in &sinks {
            if let Some(ConnectionSignal::Net(net)) =
                mapped.connection(pin).map(|connection| connection.signal)
            {
                required_nets.insert(net);
            }
        }

        let cells = record
            .cells()
            .iter()
            .map(|cell| {
                let pins = cell
                    .pins()
                    .iter()
                    .map(|pin| {
                        let signal = match pin.signal() {
                            BoundaryRepairSignal::Constant(value) => {
                                PreparedSignal::Constant(value)
                            }
                            BoundaryRepairSignal::External(index) => PreparedSignal::External(
                                *external_nets.get(index as usize).ok_or_else(|| {
                                    crate::SynthError::invariant(
                                        "cached boundary-repair external net is out of range",
                                    )
                                })?,
                            ),
                            BoundaryRepairSignal::Local(index) => {
                                PreparedSignal::Local(index as usize)
                            }
                        };
                        Ok((pin.name().to_string(), pin.library_pin(), signal))
                    })
                    .collect::<Result<_, crate::SynthError>>()?;
                Ok(PreparedCell {
                    name: cell.name().to_string(),
                    cell_type: cell.cell_type().to_string(),
                    library_cell: cell.library_cell(),
                    origins: provenance.intern_cached_operators(cell.operators())?,
                    pins,
                })
            })
            .collect::<Result<Box<[_]>, crate::SynthError>>()?;
        Ok(Self {
            semantic_identity: record.semantic_identity(),
            artifact_generation: record.generation(),
            driver: record.driver(),
            sink: record.sink(),
            local_net_names: record
                .local_nets()
                .iter()
                .map(|net| net.name().map(str::to_string))
                .collect(),
            cells,
            sinks,
            required_cells: required_cells.into_iter().collect(),
            required_nets: required_nets.into_iter().collect(),
        })
    }

    pub(crate) fn required_cells(&self) -> &[CellId] {
        &self.required_cells
    }

    pub(crate) fn required_nets(&self) -> &[NetId] {
        &self.required_nets
    }

    pub(crate) fn append_to_delta(
        &self,
        delta: &mut RegionDelta,
    ) -> Result<PendingBoundaryRepair, crate::SynthError> {
        if self
            .required_cells
            .iter()
            .any(|&cell| !delta.snapshot().contains_cell(cell))
            || self
                .required_nets
                .iter()
                .any(|&net| !delta.snapshot().contains_net(net))
        {
            return Err(crate::SynthError::invariant(
                "boundary-repair restore escaped its mapped snapshot",
            ));
        }
        let local_nets = self
            .local_net_names
            .iter()
            .map(|name| delta.add_net(name.clone()).map_err(crate::SynthError::from))
            .collect::<Result<Box<[_]>, _>>()?;
        let cells = self
            .cells
            .iter()
            .map(|cell| {
                let mut spec = CellSpec::new(&cell.name, &cell.cell_type, Some(cell.library_cell));
                for (pin, library_pin, signal) in &cell.pins {
                    let connection = match *signal {
                        PreparedSignal::Constant(value) => ConnectionRef::Constant(value),
                        PreparedSignal::External(net) => ConnectionRef::Net(net),
                        PreparedSignal::Local(index) => {
                            ConnectionRef::NewNet(*local_nets.get(index).ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "boundary-repair local net is out of range",
                                )
                            })?)
                        }
                    };
                    spec = spec.connect(pin, Some(*library_pin), connection);
                }
                delta
                    .add_cell(spec)
                    .map(|cell_id| (cell_id, cell.origins))
                    .map_err(crate::SynthError::from)
            })
            .collect::<Result<Box<[_]>, _>>()?;
        for &(pin, local) in &self.sinks {
            delta
                .reconnect_pin(
                    pin,
                    ConnectionRef::NewNet(*local_nets.get(local).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "boundary-repair sink local net is out of range",
                        )
                    })?),
                )
                .map_err(crate::SynthError::from)?;
        }
        let mut sink_pins = self.sinks.iter().map(|&(pin, _)| pin).collect::<Vec<_>>();
        sink_pins.sort_unstable();
        if sink_pins.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(crate::SynthError::invariant(
                "boundary-repair restore contains duplicate sink pins",
            ));
        }
        Ok(PendingBoundaryRepair {
            semantic_identity: self.semantic_identity,
            artifact_generation: self.artifact_generation,
            driver: self.driver,
            sink: self.sink,
            cells,
            local_nets,
            sink_pins: sink_pins.into_boxed_slice(),
        })
    }
}

impl PendingBoundaryRepair {
    pub(crate) fn resolve(
        self,
        applied: &AppliedRegionDelta,
    ) -> Result<BoundaryRepairPublication, crate::SynthError> {
        let mut sources = self
            .cells
            .iter()
            .map(|&(cell, origins)| {
                applied
                    .added_cell(cell)
                    .map(|cell| (cell, origins))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "applied boundary-repair delta lost an artifact cell",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        sources.sort_unstable_by_key(|&(cell, _)| cell);
        if sources.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(crate::SynthError::invariant(
                "applied boundary-repair delta reused an artifact cell",
            ));
        }
        let cells = sources.iter().map(|&(cell, _)| cell).collect::<Box<[_]>>();
        let mut local_nets = self
            .local_nets
            .iter()
            .map(|&net| {
                applied.added_net(net).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "applied boundary-repair delta lost an artifact net",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        local_nets.sort_unstable();
        if local_nets.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(crate::SynthError::invariant(
                "applied boundary-repair delta reused an artifact net",
            ));
        }
        let sources = cells
            .iter()
            .copied()
            .zip(sources.into_iter().map(|(_, origins)| origins))
            .map(|(cell, origins)| {
                (
                    cell,
                    MappedCellSource::Boundary {
                        origins,
                        driver: self.driver,
                        sink: self.sink,
                    },
                )
            })
            .collect();
        Ok(BoundaryRepairPublication {
            footprint: MappedBoundaryRepairFootprint {
                mapped_generation: applied.generation_id(),
                semantic_identity: self.semantic_identity,
                artifact_generation: self.artifact_generation,
                driver: self.driver,
                sink: self.sink,
                cells,
                local_nets: local_nets.into_boxed_slice(),
                sink_pins: self.sink_pins,
            },
            sources,
        })
    }
}

fn ownership_endpoint(
    sources: &[Option<MappedCellSource>],
    cell: CellId,
) -> Result<Option<crate::RegionAnchorId>, crate::SynthError> {
    match sources.get(cell.index()).and_then(Option::as_ref) {
        Some(MappedCellSource::Instance(_)) => Ok(None),
        Some(MappedCellSource::Value { owner, .. } | MappedCellSource::Region { owner, .. }) => {
            Ok(Some(*owner))
        }
        Some(MappedCellSource::Boundary { sink, .. }) => Ok(Some(*sink)),
        None => Err(crate::SynthError::invariant(
            "boundary-repair anchor has no mapped provenance owner",
        )),
    }
}

fn resolve_endpoint(
    mapped: &MappedNetlist,
    endpoint: &BoundaryRepairEndpoint,
) -> Result<(CellId, PinId), crate::SynthError> {
    let mut cells = mapped
        .cell_ids()
        .filter(|&cell| mapped.cell_name(cell) == Some(endpoint.cell()));
    let cell = cells.next().ok_or_else(|| {
        crate::SynthError::invariant("cached boundary-repair endpoint cell disappeared")
    })?;
    if cells.next().is_some() {
        return Err(crate::SynthError::invariant(
            "cached boundary-repair endpoint cell name is ambiguous",
        ));
    }
    let mut pins = mapped.pin_ids(cell).into_iter().flatten().filter(|&pin| {
        mapped.connection(pin).is_some_and(|connection| {
            connection.library_pin == Some(endpoint.library_pin())
                && mapped.pin_name(connection) == Some(endpoint.pin())
        })
    });
    let pin = pins.next().ok_or_else(|| {
        crate::SynthError::invariant("cached boundary-repair endpoint pin disappeared")
    })?;
    if pins.next().is_some() {
        return Err(crate::SynthError::invariant(
            "cached boundary-repair endpoint pin is ambiguous",
        ));
    }
    Ok((cell, pin))
}

fn find_unique_net(mapped: &MappedNetlist, name: &str) -> Result<NetId, crate::SynthError> {
    let mut nets = mapped
        .net_ids()
        .filter(|&net| mapped.net_name(net) == Some(name));
    let net = nets.next().ok_or_else(|| {
        crate::SynthError::invariant("cached boundary-repair named net disappeared")
    })?;
    if nets.next().is_some() {
        return Err(crate::SynthError::invariant(
            "cached boundary-repair named net is ambiguous",
        ));
    }
    Ok(net)
}
