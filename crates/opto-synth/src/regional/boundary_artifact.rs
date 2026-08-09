// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Portable post-map repairs owned by one exact synthesis-region edge.
//!
//! Runtime mapped IDs never enter this representation. A record binds stable
//! cell/pin/net names to a semantic graph edge, while materialization resolves
//! those names into the receiving mapped generation and publishes a separate
//! exact ID footprint.

use super::RegionContextKey;
use crate::{ImplementationDb, RegionAnchorId, RegionCoverPlan, SynthesisRegionGraph};
use opto_ir::mapped::{CellId, ConnectionSignal, MappedNetlist, NetId, PinId};
use opto_library::{TargetCellSet, TargetPinDirection};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const EDGE_ID_DOMAIN: &[u8] = b"opto/boundary-repair/edge/v1\0";
const GENERATION_DOMAIN: &[u8] = b"opto/boundary-repair/generation/v1\0";

#[derive(Debug, Clone)]
pub(crate) struct BoundaryRepairSchema {
    contexts: BTreeMap<RegionAnchorId, RegionContextKey>,
    edges: BTreeMap<(RegionAnchorId, RegionAnchorId), Box<[[u8; 32]]>>,
}

impl BoundaryRepairSchema {
    pub(crate) fn new(
        graph: &SynthesisRegionGraph,
        plans: &[RegionCoverPlan],
    ) -> Result<Self, crate::SynthError> {
        if plans.len() != graph.regions().len() {
            return Err(crate::SynthError::invariant(
                "boundary-repair schema does not align with regional plans",
            ));
        }
        let mut contexts = BTreeMap::new();
        let mut edges = BTreeMap::<_, Vec<_>>::new();
        for (region, plan) in graph.regions().iter().zip(plans) {
            if plan.region() != region.id()
                || contexts.insert(region.id(), plan.context_key()).is_some()
            {
                return Err(crate::SynthError::invariant(
                    "boundary-repair schema has inconsistent region identities",
                ));
            }
            for &port in graph.output_ports(*region) {
                let port = graph.port(port).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "boundary-repair schema references an unknown output port",
                    )
                })?;
                let Some(sink_row) = port.peer() else {
                    continue;
                };
                let sink = graph.region(sink_row).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "boundary-repair schema references an unknown sink region",
                    )
                })?;
                edges
                    .entry((region.id(), sink.id()))
                    .or_default()
                    .push(port.semantic_key());
            }
        }
        let edges = edges
            .into_iter()
            .map(|(edge, mut semantic_keys)| {
                semantic_keys.sort_unstable();
                if semantic_keys.windows(2).any(|pair| pair[0] >= pair[1]) {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair edge has duplicate semantic ports",
                    ));
                }
                Ok((edge, semantic_keys.into_boxed_slice()))
            })
            .collect::<Result<_, crate::SynthError>>()?;
        Ok(Self { contexts, edges })
    }

    fn edge(
        &self,
        driver: RegionAnchorId,
        sink: RegionAnchorId,
    ) -> Result<BoundaryRepairEdge, crate::SynthError> {
        let driver_context = self.contexts.get(&driver).copied().ok_or_else(|| {
            crate::SynthError::invariant("boundary repair has an unknown driver region")
        })?;
        let sink_context = self.contexts.get(&sink).copied().ok_or_else(|| {
            crate::SynthError::invariant("boundary repair has an unknown sink region")
        })?;
        let semantic_ports = self.edges.get(&(driver, sink)).cloned().ok_or_else(|| {
            crate::SynthError::invariant(
                "boundary repair does not correspond to a synthesis-region edge",
            )
        })?;
        let identity = edge_identity(driver, sink, driver_context, sink_context, &semantic_ports);
        Ok(BoundaryRepairEdge {
            driver,
            sink,
            driver_context,
            sink_context,
            semantic_ports,
            identity,
        })
    }

    pub(crate) fn matches(
        &self,
        repair: &BoundaryRepairArtifactRecord,
    ) -> Result<bool, crate::SynthError> {
        repair.validate()?;
        let Some(&driver_context) = self.contexts.get(&repair.driver()) else {
            return Ok(false);
        };
        let Some(&sink_context) = self.contexts.get(&repair.sink()) else {
            return Ok(false);
        };
        let Some(semantic_ports) = self.edges.get(&(repair.driver(), repair.sink())) else {
            return Ok(false);
        };
        Ok(driver_context == repair.driver_context()
            && sink_context == repair.sink_context()
            && semantic_ports.as_ref() == repair.edge.semantic_ports.as_ref()
            && edge_identity(
                repair.driver(),
                repair.sink(),
                driver_context,
                sink_context,
                semantic_ports,
            ) == repair.semantic_identity())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BoundaryRepairEdge {
    driver: RegionAnchorId,
    sink: RegionAnchorId,
    driver_context: RegionContextKey,
    sink_context: RegionContextKey,
    semantic_ports: Box<[[u8; 32]]>,
    identity: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairEndpoint {
    cell: Box<str>,
    pin: Box<str>,
    library_pin: u16,
}

impl BoundaryRepairEndpoint {
    pub(crate) fn cell(&self) -> &str {
        &self.cell
    }

    pub(crate) fn pin(&self) -> &str {
        &self.pin
    }

    pub(crate) const fn library_pin(&self) -> u16 {
        self.library_pin
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairExternalNet {
    name: Option<Box<str>>,
    driver: Option<BoundaryRepairEndpoint>,
}

impl BoundaryRepairExternalNet {
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) const fn driver(&self) -> Option<&BoundaryRepairEndpoint> {
        self.driver.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairLocalNet {
    name: Option<Box<str>>,
    driver: BoundaryRepairEndpoint,
}

impl BoundaryRepairLocalNet {
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum BoundaryRepairSignal {
    Constant(bool),
    External(u32),
    Local(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairPin {
    name: Box<str>,
    library_pin: u16,
    signal: BoundaryRepairSignal,
}

impl BoundaryRepairPin {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) const fn library_pin(&self) -> u16 {
        self.library_pin
    }

    pub(crate) const fn signal(&self) -> BoundaryRepairSignal {
        self.signal
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairCell {
    name: Box<str>,
    cell_type: Box<str>,
    library_cell: u32,
    operators: Box<[u32]>,
    pins: Box<[BoundaryRepairPin]>,
}

impl BoundaryRepairCell {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn cell_type(&self) -> &str {
        &self.cell_type
    }

    pub(crate) const fn library_cell(&self) -> u32 {
        self.library_cell
    }

    pub(crate) fn operators(&self) -> &[u32] {
        &self.operators
    }

    pub(crate) fn pins(&self) -> &[BoundaryRepairPin] {
        &self.pins
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairSink {
    endpoint: BoundaryRepairEndpoint,
    local_net: u32,
}

impl BoundaryRepairSink {
    pub(crate) const fn endpoint(&self) -> &BoundaryRepairEndpoint {
        &self.endpoint
    }

    pub(crate) const fn local_net(&self) -> u32 {
        self.local_net
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BoundaryRepairArtifactRecord {
    edge: BoundaryRepairEdge,
    generation: [u8; 32],
    external_nets: Box<[BoundaryRepairExternalNet]>,
    local_nets: Box<[BoundaryRepairLocalNet]>,
    cells: Box<[BoundaryRepairCell]>,
    sinks: Box<[BoundaryRepairSink]>,
}

impl BoundaryRepairArtifactRecord {
    pub(crate) const fn driver(&self) -> RegionAnchorId {
        self.edge.driver
    }

    pub(crate) const fn sink(&self) -> RegionAnchorId {
        self.edge.sink
    }

    pub(crate) const fn driver_context(&self) -> RegionContextKey {
        self.edge.driver_context
    }

    pub(crate) const fn sink_context(&self) -> RegionContextKey {
        self.edge.sink_context
    }

    pub(crate) const fn semantic_identity(&self) -> [u8; 32] {
        self.edge.identity
    }

    pub(crate) const fn generation(&self) -> [u8; 32] {
        self.generation
    }

    pub(crate) fn external_nets(&self) -> &[BoundaryRepairExternalNet] {
        &self.external_nets
    }

    pub(crate) fn local_nets(&self) -> &[BoundaryRepairLocalNet] {
        &self.local_nets
    }

    pub(crate) fn cells(&self) -> &[BoundaryRepairCell] {
        &self.cells
    }

    pub(crate) fn sinks(&self) -> &[BoundaryRepairSink] {
        &self.sinks
    }

    pub(crate) fn validate(&self) -> Result<(), crate::SynthError> {
        if self.edge.driver == self.edge.sink
            || self.edge.semantic_ports.is_empty()
            || self
                .edge
                .semantic_ports
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self.edge.identity
                != edge_identity(
                    self.edge.driver,
                    self.edge.sink,
                    self.edge.driver_context,
                    self.edge.sink_context,
                    &self.edge.semantic_ports,
                )
        {
            return Err(crate::SynthError::invariant(
                "boundary-repair semantic identity is invalid",
            ));
        }
        if self.cells.is_empty()
            || self
                .cells
                .windows(2)
                .any(|pair| pair[0].name >= pair[1].name)
            || self.external_nets.windows(2).any(|pair| pair[0] >= pair[1])
            || self.local_nets.windows(2).any(|pair| pair[0] >= pair[1])
            || self
                .sinks
                .windows(2)
                .any(|pair| pair[0].endpoint >= pair[1].endpoint)
        {
            return Err(crate::SynthError::invariant(
                "boundary-repair artifact is empty or not canonically ordered",
            ));
        }
        for external in &self.external_nets {
            if external.name.as_deref().is_none_or(str::is_empty) && external.driver.is_none() {
                return Err(crate::SynthError::invariant(
                    "boundary-repair external net has no stable anchor",
                ));
            }
        }
        for cell in &self.cells {
            if cell.name.is_empty()
                || cell.cell_type.is_empty()
                || cell.operators.windows(2).any(|pair| pair[0] >= pair[1])
                || cell.pins.is_empty()
                || cell
                    .pins
                    .windows(2)
                    .any(|pair| pair[0].name >= pair[1].name)
            {
                return Err(crate::SynthError::invariant(
                    "boundary-repair cell is not canonically encoded",
                ));
            }
            for pin in &cell.pins {
                let in_range = match pin.signal {
                    BoundaryRepairSignal::Constant(_) => true,
                    BoundaryRepairSignal::External(index) => {
                        (index as usize) < self.external_nets.len()
                    }
                    BoundaryRepairSignal::Local(index) => (index as usize) < self.local_nets.len(),
                };
                if pin.name.is_empty() || !in_range {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair pin has an invalid signal reference",
                    ));
                }
            }
        }
        for (index, local) in self.local_nets.iter().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| crate::SynthError::capacity("boundary-repair local nets"))?;
            let driver_matches = self
                .cells
                .binary_search_by(|cell| cell.name.cmp(&local.driver.cell))
                .ok()
                .and_then(|cell| self.cells.get(cell))
                .and_then(|cell| {
                    cell.pins
                        .binary_search_by(|pin| pin.name.cmp(&local.driver.pin))
                        .ok()
                        .and_then(|pin| cell.pins.get(pin))
                })
                .is_some_and(|pin| {
                    pin.library_pin == local.driver.library_pin
                        && pin.signal == BoundaryRepairSignal::Local(index)
                });
            if local.name.as_deref().is_some_and(str::is_empty) || !driver_matches {
                return Err(crate::SynthError::invariant(
                    "boundary-repair local net has no matching artifact driver",
                ));
            }
        }
        if self.sinks.iter().any(|sink| {
            sink.endpoint.cell.is_empty()
                || sink.endpoint.pin.is_empty()
                || sink.local_net as usize >= self.local_nets.len()
                || self
                    .cells
                    .binary_search_by(|cell| cell.name.cmp(&sink.endpoint.cell))
                    .is_ok()
        }) {
            return Err(crate::SynthError::invariant(
                "boundary-repair sink footprint is invalid",
            ));
        }
        if self.generation != artifact_generation(self)? {
            return Err(crate::SynthError::invariant(
                "boundary-repair content generation does not match its topology",
            ));
        }
        Ok(())
    }

    pub(crate) fn capture_all(
        schema: &BoundaryRepairSchema,
        mapped: &MappedNetlist,
        implementations: &ImplementationDb,
        library: &TargetCellSet,
        invalid_regions: &BTreeSet<RegionAnchorId>,
    ) -> Result<Box<[Self]>, crate::SynthError> {
        let mut records = Vec::new();
        for (driver, sink, cells) in implementations.boundary_edge_footprints() {
            if cells.is_empty()
                || invalid_regions.contains(&driver)
                || invalid_regions.contains(&sink)
            {
                continue;
            }
            records.push(Self::capture(
                schema,
                mapped,
                implementations,
                library,
                driver,
                sink,
                cells,
            )?);
        }
        records.sort_unstable_by_key(Self::semantic_identity);
        if records.windows(2).any(|pair| {
            pair[0].semantic_identity() >= pair[1].semantic_identity()
                || (pair[0].driver(), pair[0].sink()) == (pair[1].driver(), pair[1].sink())
        }) {
            return Err(crate::SynthError::invariant(
                "captured boundary repairs are not unique edge artifacts",
            ));
        }
        Ok(records.into_boxed_slice())
    }

    fn capture(
        schema: &BoundaryRepairSchema,
        mapped: &MappedNetlist,
        implementations: &ImplementationDb,
        library: &TargetCellSet,
        driver: RegionAnchorId,
        sink: RegionAnchorId,
        footprint: &[CellId],
    ) -> Result<Self, crate::SynthError> {
        let edge = schema.edge(driver, sink)?;
        let cells = footprint.iter().copied().collect::<BTreeSet<_>>();
        if cells.len() != footprint.len() || cells.iter().any(|&cell| !mapped.is_live_cell(cell)) {
            return Err(crate::SynthError::invariant(
                "boundary-repair footprint contains duplicate or removed cells",
            ));
        }

        let mut local_drivers = BTreeMap::<NetId, BoundaryRepairEndpoint>::new();
        for &cell in &cells {
            for pin in mapped.pin_ids(cell).into_iter().flatten() {
                if pin_direction(mapped, library, pin)? != TargetPinDirection::Output {
                    continue;
                }
                let Some(ConnectionSignal::Net(net)) =
                    mapped.connection(pin).map(|connection| connection.signal)
                else {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair output pin is not connected to a net",
                    ));
                };
                let endpoint = endpoint(mapped, pin)?;
                if local_drivers.insert(net, endpoint).is_some() {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair local net has multiple artifact drivers",
                    ));
                }
            }
        }
        if local_drivers.is_empty() {
            return Err(crate::SynthError::invariant(
                "boundary-repair artifact has no locally driven net",
            ));
        }
        let mut local_rows = local_drivers
            .iter()
            .map(|(&net, driver)| {
                (
                    net,
                    BoundaryRepairLocalNet {
                        name: mapped.net_name(net).map(Into::into),
                        driver: driver.clone(),
                    },
                )
            })
            .collect::<Vec<_>>();
        local_rows.sort_unstable_by(|left, right| left.1.cmp(&right.1));
        let local_ids = local_rows
            .iter()
            .enumerate()
            .map(|(index, (net, _))| {
                Ok((
                    *net,
                    u32::try_from(index)
                        .map_err(|_| crate::SynthError::capacity("local repair net count"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, crate::SynthError>>()?;
        let local_nets = local_rows
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Box<[_]>>();

        let mut external_ids = BTreeSet::new();
        for &cell in &cells {
            for connection in mapped.connections(cell).into_iter().flatten() {
                if let ConnectionSignal::Net(net) = connection.signal
                    && !local_ids.contains_key(&net)
                {
                    external_ids.insert(net);
                }
            }
        }
        let mut external_rows = external_ids
            .into_iter()
            .map(|net| external_net(mapped, library, net, &cells).map(|record| (net, record)))
            .collect::<Result<Vec<_>, _>>()?;
        external_rows.sort_unstable_by(|left, right| left.1.cmp(&right.1));
        if external_rows.windows(2).any(|pair| pair[0].1 == pair[1].1) {
            return Err(crate::SynthError::invariant(
                "boundary-repair external net anchors are ambiguous",
            ));
        }
        let external_ids = external_rows
            .iter()
            .enumerate()
            .map(|(index, (net, _))| {
                Ok((
                    *net,
                    u32::try_from(index)
                        .map_err(|_| crate::SynthError::capacity("external repair net count"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>, crate::SynthError>>()?;
        let external_nets = external_rows
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Box<[_]>>();

        let encoded_cells = encode_repair_cells(
            mapped,
            implementations,
            library,
            &cells,
            &local_ids,
            &external_ids,
        )?;

        let mut sinks = Vec::new();
        for (&net, &local_net) in &local_ids {
            for pin in mapped.pins_on_net(net).into_iter().flatten() {
                let owner = mapped.pin_owner(pin).ok_or_else(|| {
                    crate::SynthError::invariant("boundary-repair local net has an ownerless pin")
                })?;
                if cells.contains(&owner) {
                    continue;
                }
                if !matches!(
                    pin_direction(mapped, library, pin)?,
                    TargetPinDirection::Input | TargetPinDirection::Inout
                ) {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair local net escapes through a non-sink pin",
                    ));
                }
                if implementations.cell_ownership(owner)?
                    != crate::MappedCellOwnership::Region(sink)
                {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair local net does not terminate directly in its sink region",
                    ));
                }
                sinks.push(BoundaryRepairSink {
                    endpoint: endpoint(mapped, pin)?,
                    local_net,
                });
            }
        }
        sinks.sort_unstable();
        if sinks.is_empty()
            || sinks
                .windows(2)
                .any(|pair| pair[0].endpoint >= pair[1].endpoint)
        {
            return Err(crate::SynthError::invariant(
                "boundary-repair artifact has no unique sink-region footprint",
            ));
        }
        for external in &external_nets {
            if let Some(anchor) = &external.driver {
                let cell = find_cell(mapped, &anchor.cell)?;
                if implementations.cell_ownership(cell)?
                    != crate::MappedCellOwnership::Region(driver)
                {
                    return Err(crate::SynthError::invariant(
                        "boundary-repair external net is not driven directly by its driver region",
                    ));
                }
            }
        }

        let mut record = Self {
            edge,
            generation: [0; 32],
            external_nets,
            local_nets,
            cells: encoded_cells,
            sinks: sinks.into_boxed_slice(),
        };
        record.generation = artifact_generation(&record)?;
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let endpoint_bytes = |endpoint: &BoundaryRepairEndpoint| {
            endpoint.cell.len().saturating_add(endpoint.pin.len())
        };
        let strings = self
            .external_nets
            .iter()
            .map(|net| {
                net.name
                    .as_ref()
                    .map_or(0, |name| name.len())
                    .saturating_add(net.driver.as_ref().map_or(0, endpoint_bytes))
            })
            .chain(self.local_nets.iter().map(|net| {
                net.name
                    .as_ref()
                    .map_or(0, |name| name.len())
                    .saturating_add(endpoint_bytes(&net.driver))
            }))
            .chain(self.cells.iter().flat_map(|cell| {
                std::iter::once(cell.name.len())
                    .chain(std::iter::once(cell.cell_type.len()))
                    .chain(cell.pins.iter().map(|pin| pin.name.len()))
            }))
            .chain(
                self.sinks
                    .iter()
                    .flat_map(|sink| [sink.endpoint.cell.len(), sink.endpoint.pin.len()]),
            )
            .sum::<usize>();
        strings
            .saturating_add(
                opto_core::resident::slice_bytes::<BoundaryRepairExternalNet>(
                    self.external_nets.len(),
                ),
            )
            .saturating_add(opto_core::resident::slice_bytes::<BoundaryRepairLocalNet>(
                self.local_nets.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<BoundaryRepairCell>(
                self.cells.len(),
            ))
            .saturating_add(opto_core::resident::slice_bytes::<[u8; 32]>(
                self.edge.semantic_ports.len(),
            ))
            .saturating_add(
                self.cells
                    .iter()
                    .map(|cell| {
                        opto_core::resident::slice_bytes::<u32>(cell.operators.len())
                            .saturating_add(opto_core::resident::slice_bytes::<BoundaryRepairPin>(
                                cell.pins.len(),
                            ))
                    })
                    .fold(0usize, usize::saturating_add),
            )
            .saturating_add(opto_core::resident::slice_bytes::<BoundaryRepairSink>(
                self.sinks.len(),
            ))
    }
}

fn encode_repair_cells(
    mapped: &MappedNetlist,
    implementations: &ImplementationDb,
    library: &TargetCellSet,
    cells: &BTreeSet<CellId>,
    local_ids: &BTreeMap<NetId, u32>,
    external_ids: &BTreeMap<NetId, u32>,
) -> Result<Box<[BoundaryRepairCell]>, crate::SynthError> {
    let mut encoded = cells
        .iter()
        .copied()
        .map(|cell| {
            let mapped_cell = mapped.cell(cell).ok_or_else(|| {
                crate::SynthError::invariant("boundary-repair cell disappeared during capture")
            })?;
            let library_cell = mapped_cell.library_cell.ok_or_else(|| {
                crate::SynthError::invariant("boundary-repair cell has no target-library identity")
            })?;
            let target = library.get(library_cell as usize).ok_or_else(|| {
                crate::SynthError::invariant(
                    "boundary-repair cell references an unknown target-library cell",
                )
            })?;
            let cell_type = mapped.cell_type(cell).ok_or_else(|| {
                crate::SynthError::invariant("boundary-repair cell has no stable type")
            })?;
            if target.name() != cell_type {
                return Err(crate::SynthError::invariant(
                    "boundary-repair cell type disagrees with its library identity",
                ));
            }
            let mut pins = mapped
                .connections(cell)
                .ok_or_else(|| {
                    crate::SynthError::invariant("boundary-repair cell has no mapped connections")
                })?
                .iter()
                .map(|connection| {
                    let name = mapped.pin_name(connection).ok_or_else(|| {
                        crate::SynthError::invariant("boundary-repair cell pin has no stable name")
                    })?;
                    let library_pin = connection.library_pin.ok_or_else(|| {
                        crate::SynthError::invariant(
                            "boundary-repair cell pin has no library identity",
                        )
                    })?;
                    let signal = match connection.signal {
                        ConnectionSignal::Constant(value) => BoundaryRepairSignal::Constant(value),
                        ConnectionSignal::Net(net) => local_ids
                            .get(&net)
                            .copied()
                            .map(BoundaryRepairSignal::Local)
                            .or_else(|| {
                                external_ids
                                    .get(&net)
                                    .copied()
                                    .map(BoundaryRepairSignal::External)
                            })
                            .ok_or_else(|| {
                                crate::SynthError::invariant(
                                    "boundary-repair pin signal escaped its exact footprint",
                                )
                            })?,
                    };
                    Ok(BoundaryRepairPin {
                        name: name.into(),
                        library_pin,
                        signal,
                    })
                })
                .collect::<Result<Vec<_>, crate::SynthError>>()?;
            pins.sort_unstable();
            let mut operators = implementations
                .operators_for_cell(cell)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "boundary-repair cell has no implementation provenance",
                    )
                })?
                .iter()
                .map(|operator| operator.raw())
                .collect::<Vec<_>>();
            operators.sort_unstable();
            operators.dedup();
            Ok(BoundaryRepairCell {
                name: mapped
                    .cell_name(cell)
                    .ok_or_else(|| {
                        crate::SynthError::invariant("boundary-repair cell has no stable name")
                    })?
                    .into(),
                cell_type: cell_type.into(),
                library_cell,
                operators: operators.into_boxed_slice(),
                pins: pins.into_boxed_slice(),
            })
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    encoded.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(encoded.into_boxed_slice())
}

fn edge_identity(
    driver: RegionAnchorId,
    sink: RegionAnchorId,
    driver_context: RegionContextKey,
    sink_context: RegionContextKey,
    semantic_ports: &[[u8; 32]],
) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(EDGE_ID_DOMAIN);
    digest.update(&driver.bytes());
    digest.update(&sink.bytes());
    digest.update(&driver_context.bytes());
    digest.update(&sink_context.bytes());
    digest.update(&(semantic_ports.len() as u64).to_le_bytes());
    for key in semantic_ports {
        digest.update(key);
    }
    *digest.finalize().as_bytes()
}

fn artifact_generation(
    record: &BoundaryRepairArtifactRecord,
) -> Result<[u8; 32], crate::SynthError> {
    let payload = (
        &record.edge,
        &record.external_nets,
        &record.local_nets,
        &record.cells,
        &record.sinks,
    );
    let mut digest = blake3::Hasher::new();
    digest.update(GENERATION_DOMAIN);
    opto_archive::encode_into_std_write(&payload, &mut digest)
        .map_err(|_| crate::SynthError::invariant("boundary-repair generation encoding failed"))?;
    Ok(*digest.finalize().as_bytes())
}

fn endpoint(
    mapped: &MappedNetlist,
    pin: PinId,
) -> Result<BoundaryRepairEndpoint, crate::SynthError> {
    let cell = mapped
        .pin_owner(pin)
        .ok_or_else(|| crate::SynthError::invariant("boundary-repair endpoint has no live cell"))?;
    let connection = mapped
        .connection(pin)
        .ok_or_else(|| crate::SynthError::invariant("boundary-repair endpoint pin disappeared"))?;
    Ok(BoundaryRepairEndpoint {
        cell: mapped
            .cell_name(cell)
            .ok_or_else(|| {
                crate::SynthError::invariant("boundary-repair endpoint cell has no stable name")
            })?
            .into(),
        pin: mapped
            .pin_name(connection)
            .ok_or_else(|| {
                crate::SynthError::invariant("boundary-repair endpoint pin has no stable name")
            })?
            .into(),
        library_pin: connection.library_pin.ok_or_else(|| {
            crate::SynthError::invariant("boundary-repair endpoint has no library pin identity")
        })?,
    })
}

fn external_net(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    net: NetId,
    artifact_cells: &BTreeSet<CellId>,
) -> Result<BoundaryRepairExternalNet, crate::SynthError> {
    let mut drivers = Vec::new();
    for pin in mapped.pins_on_net(net).into_iter().flatten() {
        let owner = mapped.pin_owner(pin).ok_or_else(|| {
            crate::SynthError::invariant("boundary-repair external net has an ownerless pin")
        })?;
        if !artifact_cells.contains(&owner)
            && pin_direction(mapped, library, pin)? == TargetPinDirection::Output
        {
            drivers.push(endpoint(mapped, pin)?);
        }
    }
    drivers.sort_unstable();
    drivers.dedup();
    if drivers.len() > 1 {
        return Err(crate::SynthError::invariant(
            "boundary-repair external net has multiple stable drivers",
        ));
    }
    let record = BoundaryRepairExternalNet {
        name: mapped.net_name(net).map(Into::into),
        driver: drivers.pop(),
    };
    if record.name.is_none() && record.driver.is_none() {
        return Err(crate::SynthError::invariant(
            "boundary-repair external net has no portable anchor",
        ));
    }
    Ok(record)
}

fn pin_direction(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    pin: PinId,
) -> Result<TargetPinDirection, crate::SynthError> {
    let cell = mapped
        .pin_owner(pin)
        .ok_or_else(|| crate::SynthError::invariant("boundary-repair pin has no live owner"))?;
    let mapped_cell = mapped
        .cell(cell)
        .ok_or_else(|| crate::SynthError::invariant("boundary-repair pin owner disappeared"))?;
    let library_cell = mapped_cell.library_cell.ok_or_else(|| {
        crate::SynthError::invariant("boundary-repair endpoint is not a target-library cell")
    })?;
    let connection = mapped
        .connection(pin)
        .ok_or_else(|| crate::SynthError::invariant("boundary-repair pin disappeared"))?;
    let library_pin = connection.library_pin.ok_or_else(|| {
        crate::SynthError::invariant("boundary-repair endpoint has no library pin identity")
    })?;
    let target = library
        .get(library_cell as usize)
        .and_then(|cell| cell.pins().nth(library_pin as usize))
        .ok_or_else(|| {
            crate::SynthError::invariant("boundary-repair endpoint library identity is invalid")
        })?;
    if mapped.pin_name(connection) != Some(target.name()) {
        return Err(crate::SynthError::invariant(
            "boundary-repair endpoint pin name disagrees with its library identity",
        ));
    }
    Ok(target.direction())
}

fn find_cell(mapped: &MappedNetlist, name: &str) -> Result<CellId, crate::SynthError> {
    mapped
        .cell_ids()
        .find(|&cell| mapped.cell_name(cell) == Some(name))
        .ok_or_else(|| crate::SynthError::invariant("boundary-repair cell anchor disappeared"))
}
