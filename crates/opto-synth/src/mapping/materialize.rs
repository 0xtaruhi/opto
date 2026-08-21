// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Construction of the immutable mapped substrate and direct mapped artifacts.

use crate::SynthesisOptions;
use crate::artifact::provenance::SourceInstanceProvenance;
use opto_ir::BitVal;
use opto_ir::mapped::{
    CellId, CellSpec, ConnectionRef, ConnectionSignal, MappedBuilder, MappedCellSpec,
    MappedNetlist, NetId, PinId, PortDirection, PortId, RegionDelta, TempCellId, TempNetId,
};
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

pub(crate) mod region_delta;
mod sequential_delta;

/// The prefix every synthetic internal net name carries.
pub(crate) const MAPPED_NET_PREFIX: &str = "_mapped_net_";

/// The prefix every region-scoped synthetic cell name carries.
pub(crate) const REGION_CELL_PREFIX: &str = "__opto_region_";

/// Prefix for physical tri-state drivers introduced at the global boundary.
const TRI_STATE_CELL_PREFIX: &str = "_tri_state_";

use crate::artifact::MappedCellSource;
pub(crate) use sequential_delta::{
    MappedFixedSubstrateArtifact, RegionalSequentialCellPlan, RegionalSubstrateCellPlan,
    RegionalSubstrateConnection, SequentialRegionBinding, local_sequential_bindings,
    lowered_sequential_operations, plan_regional_sequential_cells,
    reconcile_sequential_publication, sequential_binding_values, sequential_plan_values,
    sequential_region_bindings,
};

fn region_instance_prefix(region: crate::RegionAnchorId) -> String {
    let mut prefix = String::with_capacity(79);
    prefix.push_str(REGION_CELL_PREFIX);
    for byte in region.bytes() {
        write!(&mut prefix, "{byte:02x}").expect("writing to String cannot fail");
    }
    prefix.push_str("_cell_");
    prefix
}

fn regional_substrate_instance_name(region: crate::RegionAnchorId, local: &str) -> String {
    let mut name = region_instance_prefix(region);
    name.push_str("substrate_");
    name.push_str(local);
    name
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ArtifactNetId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactNetBinding {
    External { net: NetId, producer: bool },
    Local(usize),
}

/// The authoritative net namespace of one sealed mapped artifact.
///
/// Cells refer only to compact artifact IDs. Whether an ID binds an existing
/// mapped net or a transaction-local net is recorded exactly once here, so net
/// identity cannot silently acquire or lose producer ownership while a region
/// is materialized.
#[derive(Debug, Clone, Default)]
struct ArtifactNetTable {
    bindings: Vec<ArtifactNetBinding>,
    external: BTreeMap<NetId, ArtifactNetId>,
    local_count: usize,
}

impl ArtifactNetTable {
    fn signal(&mut self, signal: region_delta::MappedValueSignal) -> ArtifactSignal {
        match signal {
            region_delta::MappedValueSignal::Constant(value) => ArtifactSignal::Constant(value),
            region_delta::MappedValueSignal::Net(net) => {
                let id = if let Some(&id) = self.external.get(&net) {
                    id
                } else {
                    let id = ArtifactNetId(self.bindings.len());
                    self.bindings.push(ArtifactNetBinding::External {
                        net,
                        producer: false,
                    });
                    self.external.insert(net, id);
                    id
                };
                ArtifactSignal::Net(id)
            }
        }
    }

    fn allocate_local(&mut self) -> Result<ArtifactSignal, crate::SynthError> {
        let local = self.local_count;
        self.local_count = self
            .local_count
            .checked_add(1)
            .ok_or_else(|| crate::SynthError::capacity("artifact local nets"))?;
        let id = ArtifactNetId(self.bindings.len());
        self.bindings.push(ArtifactNetBinding::Local(local));
        Ok(ArtifactSignal::Net(id))
    }

    /// Claims the sole artifact-local producer for an external boundary net.
    ///
    /// Correlated outputs can name the same frozen mapped bit. The first cover
    /// output in stable cell order owns publication; later implementations stay
    /// local. The claim lives beside net identity and is checked against the
    /// sealed output pins before publication.
    fn claim_output(
        &mut self,
        target: Option<ArtifactSignal>,
    ) -> Result<ArtifactSignal, crate::SynthError> {
        if let Some(target @ ArtifactSignal::Net(id)) = target
            && let Some(ArtifactNetBinding::External { producer, .. }) = self.bindings.get_mut(id.0)
            && !*producer
        {
            *producer = true;
            return Ok(target);
        }
        self.allocate_local()
    }

    fn local_count(&self) -> usize {
        self.local_count
    }

    fn external_nets(&self) -> impl Iterator<Item = NetId> + '_ {
        self.external.keys().copied()
    }

    fn binding(&self, id: ArtifactNetId) -> Option<ArtifactNetBinding> {
        self.bindings.get(id.0).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ArtifactSignal {
    Constant(bool),
    Net(ArtifactNetId),
}

impl ArtifactSignal {
    fn connection(
        self,
        nets: &ArtifactNetTable,
        local_nets: &[TempNetId],
        missing_local_net: &'static str,
    ) -> Result<ConnectionRef, crate::SynthError> {
        match self {
            Self::Constant(value) => Ok(ConnectionRef::Constant(value)),
            Self::Net(id) => match nets.binding(id).ok_or_else(|| {
                crate::SynthError::invariant("artifact connection references an unknown net")
            })? {
                ArtifactNetBinding::External { net, .. } => Ok(ConnectionRef::Net(net)),
                ArtifactNetBinding::Local(index) => local_nets
                    .get(index)
                    .copied()
                    .map(ConnectionRef::NewNet)
                    .ok_or_else(|| crate::SynthError::invariant(missing_local_net)),
            },
        }
    }
}

#[derive(Debug, Clone)]
struct ArtifactCell<M> {
    name: String,
    cell_type: String,
    library_cell: Option<u32>,
    connections: Box<[(String, Option<u16>, ArtifactSignal)]>,
    metadata: M,
}

type AppendedArtifactCells<O> = (Box<[TempNetId]>, Box<[O]>);

fn append_artifact_cells<M, O>(
    delta: &mut RegionDelta,
    nets: &ArtifactNetTable,
    cells: &[ArtifactCell<M>],
    missing_local_net: &'static str,
    output: impl Fn(TempCellId, &M) -> O,
) -> Result<AppendedArtifactCells<O>, crate::SynthError> {
    let local_nets = (0..nets.local_count())
        .map(|_| delta.add_net(None).map_err(crate::SynthError::from))
        .collect::<Result<Box<[_]>, _>>()?;
    let cells = cells
        .iter()
        .map(|cell| {
            let mut spec = CellSpec::new(&cell.name, &cell.cell_type, cell.library_cell);
            for (pin, library_pin, signal) in &cell.connections {
                spec = spec.connect(
                    pin,
                    *library_pin,
                    signal.connection(nets, &local_nets, missing_local_net)?,
                );
            }
            delta
                .add_cell(spec)
                .map(|id| output(id, &cell.metadata))
                .map_err(crate::SynthError::from)
        })
        .collect::<Result<Box<[_]>, _>>()?;
    Ok((local_nets, cells))
}

/// Seals one artifact's bit-level connectivity and producer claims.
///
/// Every target-library connection is scalar, so artifact nets are physical
/// bits. Validation therefore follows the exact pins referenced by each
/// combinational output function instead of treating an aggregate signal or a
/// whole cell as the cycle-detection unit.
fn validate_artifact_nets<M>(
    label: &str,
    nets: &ArtifactNetTable,
    cells: &[ArtifactCell<M>],
    target_cells: &opto_library::TargetCellSet,
) -> Result<(), crate::SynthError> {
    let mut drivers = vec![0u32; nets.bindings.len()];
    let mut edges = vec![Vec::<ArtifactNetId>::new(); nets.bindings.len()];

    for (cell_index, cell) in cells.iter().enumerate() {
        let library = cell
            .library_cell
            .and_then(|index| target_cells.get(index as usize))
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "{label} cell {cell_index} is absent from the target library"
                ))
            })?;
        let pin_signal = cell
            .connections
            .iter()
            .map(|(name, _, signal)| (name.as_str(), *signal))
            .collect::<BTreeMap<_, _>>();
        for (name, library_pin, signal) in &cell.connections {
            let pin = artifact_pin(label, cell_index, name, *library_pin, library)?;
            let ArtifactSignal::Net(id) = *signal else {
                continue;
            };
            if matches!(
                pin.direction(),
                opto_library::TargetPinDirection::Output | opto_library::TargetPinDirection::Inout
            ) {
                drivers[id.0] = drivers[id.0].saturating_add(1);
            }
        }
        if library.sequential().next().is_some() {
            continue;
        }
        for (name, library_pin, output) in &cell.connections {
            let pin = artifact_pin(label, cell_index, name, *library_pin, library)?;
            let ArtifactSignal::Net(output) = *output else {
                continue;
            };
            if !matches!(
                pin.direction(),
                opto_library::TargetPinDirection::Output | opto_library::TargetPinDirection::Inout
            ) {
                continue;
            }
            for function in [pin.function(), pin.three_state()].into_iter().flatten() {
                function.for_each_pin(&mut |source_name| {
                    if let Some(ArtifactSignal::Net(source)) = pin_signal.get(source_name).copied()
                    {
                        edges[source.0].push(output);
                    }
                });
            }
        }
    }

    for (index, binding) in nets.bindings.iter().copied().enumerate() {
        match binding {
            ArtifactNetBinding::External { net, producer } => {
                if drivers[index] > 1 {
                    return Err(crate::SynthError::invariant(format!(
                        "{label} external bit {net:?} has {} artifact producers",
                        drivers[index]
                    )));
                }
                if producer != (drivers[index] == 1) {
                    return Err(crate::SynthError::invariant(format!(
                        "{label} external bit {net:?} producer claim does not match its output pin"
                    )));
                }
            }
            ArtifactNetBinding::Local(local) => {
                if drivers[index] != 1 {
                    return Err(crate::SynthError::invariant(format!(
                        "{label} local bit {local} has {} producers instead of one",
                        drivers[index]
                    )));
                }
            }
        }
    }

    for targets in &mut edges {
        targets.sort_unstable();
        targets.dedup();
    }
    let mut indegree = vec![0usize; edges.len()];
    for targets in &edges {
        for target in targets {
            indegree[target.0] = indegree[target.0].saturating_add(1);
        }
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, &degree)| (degree == 0).then_some(ArtifactNetId(index)))
        .collect::<Vec<_>>();
    let mut visited = 0usize;
    while let Some(source) = ready.pop() {
        visited += 1;
        for target in &edges[source.0] {
            indegree[target.0] -= 1;
            if indegree[target.0] == 0 {
                ready.push(*target);
            }
        }
    }
    if visited != edges.len() {
        let cyclic = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, &degree)| (degree != 0).then_some(nets.bindings[index]))
            .collect::<Vec<_>>();
        return Err(crate::SynthError::invariant(format!(
            "{label} contains a physical bit-level combinational cycle through {cyclic:?}"
        )));
    }
    Ok(())
}

fn artifact_pin<'a>(
    label: &str,
    cell: usize,
    name: &str,
    index: Option<u16>,
    library: opto_library::TargetCellRef<'a>,
) -> Result<opto_library::TargetPinRef<'a>, crate::SynthError> {
    index
        .and_then(|index| library.pins().nth(index as usize))
        .or_else(|| library.pins().find(|pin| pin.name() == name))
        .ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "{label} cell {cell} pin '{name}' is absent from its target cell"
            ))
        })
}

#[derive(Debug)]
pub(crate) struct MappedOutput {
    pub(crate) netlist: MappedNetlist,
    pub(crate) cell_sources: Box<[(CellId, MappedCellSource)]>,
}

pub(crate) type MappedSubstrate = (MappedOutput, Box<[Option<NetId>]>, RegionalMappedPins);

#[derive(Debug)]
pub(crate) struct RegionalMappedPins(Box<[(crate::mapping::RegionalPinKey, NetId)]>);

impl RegionalMappedPins {
    pub(crate) fn get(&self, pin: crate::mapping::RegionalPinKey) -> Option<NetId> {
        self.0
            .binary_search_by_key(&pin, |&(candidate, _)| candidate)
            .ok()
            .map(|index| self.0[index].1)
    }

    pub(crate) fn require(
        &self,
        pin: crate::mapping::RegionalPinKey,
    ) -> Result<NetId, crate::SynthError> {
        self.get(pin).ok_or_else(|| {
            crate::SynthError::invariant("regional artifact pin has no stable mapped substrate net")
        })
    }
}

#[derive(Debug)]
struct ObservableOutput {
    name: String,
    bit: usize,
    net: NetId,
}

/// Global mapped connectivity frozen before post-map transactions begin.
///
/// Regional and post-map optimizers may replace the implementation behind a
/// boundary net, but they cannot delete that net or change its unique physical
/// driver. Source-instance output identities are resolved once here instead of
/// being rediscovered by every speculative edit.
#[derive(Debug)]
pub(crate) struct FrozenObservableConnectivity {
    boundary_nets: BTreeSet<NetId>,
    resolved_nets: BTreeSet<NetId>,
    outputs: Box<[ObservableOutput]>,
    output_nets: BTreeSet<NetId>,
    static_driver_counts: Box<[u8]>,
    source_driver_pins: BTreeSet<PinId>,
    source_sink_pins: BTreeSet<PinId>,
}

fn mapped_target_pin<'a>(
    mapped: &MappedNetlist,
    target_cells: &'a opto_library::TargetCellSet,
    pin: PinId,
) -> Result<Option<opto_library::TargetPinRef<'a>>, crate::SynthError> {
    let cell = mapped
        .pin_owner(pin)
        .ok_or_else(|| crate::SynthError::invariant("mapped pin has no live owner"))?;
    let stored = mapped
        .cell(cell)
        .ok_or_else(|| crate::SynthError::invariant("mapped pin owner is unknown"))?;
    let Some(library_index) = stored.library_cell else {
        return Ok(None);
    };
    let connection = mapped
        .connection(pin)
        .ok_or_else(|| crate::SynthError::invariant("mapped pin has no binding"))?;
    let library = target_cells.get(library_index as usize).ok_or_else(|| {
        crate::SynthError::invariant("mapped cell is absent from the target library")
    })?;
    connection
        .library_pin
        .and_then(|index| library.pins().nth(index as usize))
        .or_else(|| {
            mapped
                .pin_name(connection)
                .and_then(|name| library.pins().find(|pin| pin.name() == name))
        })
        .map(Some)
        .ok_or_else(|| {
            crate::SynthError::invariant("mapped pin is absent from its target-library cell")
        })
}

impl FrozenObservableConnectivity {
    pub(crate) fn capture(
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        reference_ports: &crate::ReferencePortMap,
    ) -> Result<Self, crate::SynthError> {
        Self::capture_model(mapped, target_cells, reference_ports, true)
    }

    /// Captures immutable port/source direction data before regional producers
    /// are installed. The caller must validate the affected bits as artifacts
    /// are committed.
    pub(crate) fn capture_substrate(
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        reference_ports: &crate::ReferencePortMap,
    ) -> Result<Self, crate::SynthError> {
        Self::capture_model(mapped, target_cells, reference_ports, false)
    }

    fn capture_model(
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        reference_ports: &crate::ReferencePortMap,
        validate: bool,
    ) -> Result<Self, crate::SynthError> {
        let mut boundary_nets = BTreeSet::new();
        let mut resolved_nets = BTreeSet::new();
        let mut outputs = Vec::new();
        let mut static_driver_counts = vec![0u8; mapped.net_slot_count()];
        for (index, port) in mapped.ports().iter().enumerate() {
            let id = PortId::from_index(index).map_err(crate::SynthError::from)?;
            let nets = mapped.port_nets(id).ok_or_else(|| {
                crate::SynthError::invariant("mapped port has no frozen net binding")
            })?;
            boundary_nets.extend(nets.iter().copied());
            if matches!(port.direction, PortDirection::Input | PortDirection::Inout) {
                for &net in nets {
                    static_driver_counts[net.index()] =
                        static_driver_counts[net.index()].saturating_add(1);
                }
            }
            if port.direction == PortDirection::Inout {
                resolved_nets.extend(nets.iter().copied());
            }
            if port.direction == PortDirection::Output {
                let name = mapped.port_name(id).unwrap_or("<unnamed>");
                outputs.extend(nets.iter().enumerate().map(|(bit, &net)| ObservableOutput {
                    name: name.to_string(),
                    bit,
                    net,
                }));
            }
        }
        let mut source_driver_pins = BTreeSet::new();
        let mut source_sink_pins = BTreeSet::new();
        for cell in mapped.cell_ids() {
            let stored = mapped
                .cell(cell)
                .ok_or_else(|| crate::SynthError::invariant("mapped source cell is unknown"))?;
            if stored.library_cell.is_some() {
                continue;
            }
            let cell_type = mapped.cell_type(cell).ok_or_else(|| {
                crate::SynthError::invariant("mapped source cell has no type name")
            })?;
            for pin in mapped.pin_ids(cell).into_iter().flatten() {
                let connection = mapped.connection(pin).ok_or_else(|| {
                    crate::SynthError::invariant("mapped source cell has no pin binding")
                })?;
                let pin_name = mapped.pin_name(connection).ok_or_else(|| {
                    crate::SynthError::invariant("mapped source cell has no pin name")
                })?;
                let direction = reference_ports
                    .get(cell_type)
                    .and_then(|ports| ports.get(pin_name))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "mapped source pin '{cell_type}.{pin_name}' has no direction contract"
                        ))
                    })?
                    .direction;
                if matches!(
                    direction,
                    word::PortDirection::Output | word::PortDirection::Inout
                ) {
                    source_driver_pins.insert(pin);
                }
                if matches!(
                    direction,
                    word::PortDirection::Input | word::PortDirection::Inout
                ) {
                    source_sink_pins.insert(pin);
                }
            }
        }
        for &(net, _) in mapped.constant_drivers() {
            static_driver_counts[net.index()] = static_driver_counts[net.index()].saturating_add(1);
        }
        for cell in mapped.cell_ids() {
            for pin in mapped.pin_ids(cell).into_iter().flatten() {
                let connection = mapped.connection(pin).ok_or_else(|| {
                    crate::SynthError::invariant("mapped cell has no pin binding")
                })?;
                if mapped_target_pin(mapped, target_cells, pin)?
                    .is_some_and(|pin| pin.three_state().is_some())
                    && let ConnectionSignal::Net(net) = connection.signal
                {
                    resolved_nets.insert(net);
                }
            }
        }
        let output_nets = outputs.iter().map(|output| output.net).collect();
        let frozen = Self {
            boundary_nets,
            resolved_nets,
            outputs: outputs.into_boxed_slice(),
            output_nets,
            static_driver_counts: static_driver_counts.into_boxed_slice(),
            source_driver_pins,
            source_sink_pins,
        };
        if validate {
            frozen.validate(mapped, target_cells)?;
        }
        Ok(frozen)
    }

    pub(crate) fn validate_affected(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        affected_nets: impl IntoIterator<Item = NetId>,
    ) -> Result<(), crate::SynthError> {
        for net in affected_nets.into_iter().collect::<BTreeSet<_>>() {
            if self.boundary_nets.contains(&net) && !mapped.is_live_net(net) {
                return Err(crate::SynthError::invariant(format!(
                    "mapped publication boundary bit {net:?} was removed"
                )));
            }
            if !mapped.is_live_net(net) || !self.net_is_required(mapped, target_cells, net)? {
                continue;
            }
            let drivers = self.driver_count(mapped, target_cells, net)?;
            if drivers == 1 || (drivers > 0 && self.resolved_nets.contains(&net)) {
                continue;
            }
            let name = mapped.net_name(net).unwrap_or("<unnamed>");
            if drivers == 0 {
                return Err(crate::SynthError::invariant(format!(
                    "mapped bit '{name}' ({net:?}) is consumed but has no physical producer"
                )));
            }
            return Err(crate::SynthError::invariant(format!(
                "mapped bit '{name}' ({net:?}) has {drivers} physical producers ({})",
                self.driver_sources(mapped, target_cells, net)?.join(", ")
            )));
        }
        Ok(())
    }

    pub(crate) fn preserves_affected(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        affected_nets: impl IntoIterator<Item = NetId>,
    ) -> Result<bool, crate::SynthError> {
        let affected = affected_nets.into_iter().collect::<BTreeSet<_>>();
        if affected
            .intersection(&self.boundary_nets)
            .any(|&net| !mapped.is_live_net(net))
        {
            return Ok(false);
        }
        for net in affected {
            if !self.net_is_required(mapped, target_cells, net)? {
                continue;
            }
            let drivers = self.driver_count(mapped, target_cells, net)?;
            if drivers == 0 || (!self.resolved_nets.contains(&net) && drivers != 1) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn validate(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
    ) -> Result<(), crate::SynthError> {
        if let Some(&net) = self
            .boundary_nets
            .iter()
            .find(|&&net| !mapped.is_live_net(net))
        {
            return Err(crate::SynthError::invariant(format!(
                "mapped publication boundary net {net:?} was removed after connectivity freeze"
            )));
        }
        for output in &self.outputs {
            let drivers = self.driver_count(mapped, target_cells, output.net)?;
            match drivers {
                1 => {}
                0 => {
                    return Err(crate::SynthError::invariant(format!(
                        "mapped output '{}[{}]' has no physical driver",
                        output.name, output.bit
                    )));
                }
                _ if self.resolved_nets.contains(&output.net) => {}
                _ => {
                    let sources = self.driver_sources(mapped, target_cells, output.net)?;
                    return Err(crate::SynthError::invariant(format!(
                        "mapped output '{}[{}]' has {drivers} physical drivers ({})",
                        output.name,
                        output.bit,
                        sources.join(", "),
                    )));
                }
            }
        }
        for &net in self
            .required_nets(mapped, target_cells)?
            .difference(&self.output_nets)
        {
            let drivers = self.driver_count(mapped, target_cells, net)?;
            match drivers {
                1 => {}
                0 => {
                    return Err(crate::SynthError::invariant(format!(
                        "mapped net {net:?} is consumed but has no physical driver"
                    )));
                }
                _ if self.resolved_nets.contains(&net) => {}
                _ => {
                    let sources = self.driver_sources(mapped, target_cells, net)?;
                    return Err(crate::SynthError::invariant(format!(
                        "mapped net {net:?} is consumed but has {drivers} physical drivers ({})",
                        sources.join(", "),
                    )));
                }
            }
        }
        Ok(())
    }

    fn required_nets(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
    ) -> Result<BTreeSet<NetId>, crate::SynthError> {
        let mut required = BTreeSet::new();
        for net in mapped.net_ids() {
            if self.net_is_required(mapped, target_cells, net)? {
                required.insert(net);
            }
        }
        Ok(required)
    }

    fn net_is_required(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        net: NetId,
    ) -> Result<bool, crate::SynthError> {
        if self.output_nets.contains(&net) {
            return Ok(true);
        }
        for pin in mapped.pins_on_net(net).into_iter().flatten() {
            if self.source_sink_pins.contains(&pin) {
                return Ok(true);
            }
            if mapped_target_pin(mapped, target_cells, pin)?.is_some_and(|pin| {
                matches!(
                    pin.direction(),
                    opto_library::TargetPinDirection::Input
                        | opto_library::TargetPinDirection::Inout
                )
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn driver_count(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        net: NetId,
    ) -> Result<u8, crate::SynthError> {
        let mut count = self
            .static_driver_counts
            .get(net.index())
            .copied()
            // Post-map transactions may add net slots after the frozen
            // substrate. Treat those as having no static port/constant driver;
            // dynamic cell pins below still provide their current drivers.
            .unwrap_or(0);
        for pin in mapped.pins_on_net(net).into_iter().flatten() {
            if self.source_driver_pins.contains(&pin) {
                count = count.saturating_add(1);
                continue;
            }
            if mapped_target_pin(mapped, target_cells, pin)?.is_some_and(|pin| {
                matches!(
                    pin.direction(),
                    opto_library::TargetPinDirection::Output
                        | opto_library::TargetPinDirection::Inout
                )
            }) {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    fn driver_sources(
        &self,
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        net: NetId,
    ) -> Result<Vec<String>, crate::SynthError> {
        let mut sources = mapped
            .constant_drivers()
            .iter()
            .filter(|&&(candidate, _)| candidate == net)
            .map(|&(_, value)| format!("constant {}", u8::from(value)))
            .collect::<Vec<_>>();
        for pin in mapped.pins_on_net(net).into_iter().flatten() {
            let cell = mapped.pin_owner(pin).ok_or_else(|| {
                crate::SynthError::invariant("mapped boundary pin has no live owner")
            })?;
            let connection = mapped.connection(pin).ok_or_else(|| {
                crate::SynthError::invariant("mapped driver cell has no pin binding")
            })?;
            let pin_name = mapped.pin_name(connection).unwrap_or("<unnamed>");
            if self.source_driver_pins.contains(&pin) {
                sources.push(format!(
                    "{}.{}",
                    mapped.cell_type(cell).unwrap_or("<source-cell>"),
                    pin_name
                ));
                continue;
            }
            if mapped_target_pin(mapped, target_cells, pin)?.is_some_and(|pin| {
                matches!(
                    pin.direction(),
                    opto_library::TargetPinDirection::Output
                        | opto_library::TargetPinDirection::Inout
                )
            }) {
                sources.push(format!(
                    "{}.{}",
                    mapped.cell_type(cell).unwrap_or("<target-cell>"),
                    pin_name
                ));
            }
        }
        Ok(sources)
    }
}

/// Rejects a published interface whose output bits have no unique physical driver.
#[cfg(test)]
pub(crate) fn validate_observable_drivers(
    mapped: &MappedNetlist,
    target_cells: &opto_library::TargetCellSet,
    reference_ports: &crate::ReferencePortMap,
) -> Result<(), crate::SynthError> {
    FrozenObservableConnectivity::capture(mapped, target_cells, reference_ports).map(drop)
}

#[derive(Clone, Copy)]
/// Borrowed inputs needed to freeze the global mapped substrate.
///
/// `observed_values` add publication roots but do not transfer their internal
/// Word topology into the substrate; regional artifacts remain the sole owner
/// of that implementation.
pub(crate) struct MappedSubstrateRequest<'a> {
    pub(crate) module: &'a word::WordModule,
    pub(crate) options: &'a SynthesisOptions,
    pub(crate) design_references: &'a BTreeSet<String>,
    pub(crate) reference_ports: &'a crate::ReferencePortMap,
    pub(crate) source_instances: &'a SourceInstanceProvenance,
    pub(crate) base_revision: opto_ir::RevisionId,
    pub(crate) observed_values: &'a [word::ValueId],
    pub(crate) value_aliases: &'a [(word::ValueId, word::ValueId)],
    pub(crate) regional_pins: &'a [crate::mapping::RegionalPinKey],
}

fn target_pin_id(
    cell: opto_library::TargetCellRef<'_>,
    pin: &str,
) -> Result<u16, crate::SynthError> {
    let index = cell
        .pins()
        .position(|candidate| candidate.name() == pin)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "target cell '{}' has no pin '{pin}'",
                cell.name()
            ))
        })?;
    u16::try_from(index)
        .map_err(|_| crate::SynthError::capacity("target pin index exceeds 16-bit capacity"))
}

#[cfg(test)]
pub(crate) fn build_test_substrate(
    module: &word::WordModule,
    options: &SynthesisOptions,
    design_references: &BTreeSet<String>,
    reference_ports: &crate::ReferencePortMap,
    source_instances: &SourceInstanceProvenance,
    base_revision: opto_ir::RevisionId,
) -> Result<MappedOutput, crate::SynthError> {
    let (output, _, _) = build_mapped_substrate(MappedSubstrateRequest {
        module,
        options,
        design_references,
        reference_ports,
        source_instances,
        base_revision,
        observed_values: &[],
        value_aliases: &[],
        regional_pins: &[],
    })?;
    Ok(output)
}

/// Builds the immutable global substrate and Word-value net bindings.
///
/// Only full-domain aliases, constants, ports, retained instances, state, and
/// explicitly observed roots enter this substrate. Region-local care reductions
/// cannot create substrate equivalence or erase publication obligations.
pub(crate) fn build_mapped_substrate(
    request: MappedSubstrateRequest<'_>,
) -> Result<MappedSubstrate, crate::SynthError> {
    let MappedSubstrateRequest {
        module,
        options,
        design_references,
        reference_ports,
        source_instances,
        base_revision,
        observed_values,
        value_aliases,
        regional_pins,
    } = request;
    let offsets = signal_offsets(module)?;
    let signal_bit_count = module.signals().iter().try_fold(0usize, |count, signal| {
        count
            .checked_add(signal.ty.width() as usize)
            .ok_or_else(|| crate::SynthError::invariant("mapped signal bit count overflow"))
    })?;
    let bit_count = signal_bit_count
        .checked_add(module.operations().len())
        .ok_or_else(|| crate::SynthError::invariant("mapped operation count overflow"))?;
    let (mut aliases, constants) =
        build_alias_classes(module, &offsets, signal_bit_count, bit_count)?;
    for &(value, representative) in value_aliases {
        let (ScalarSignal::Bit(value), ScalarSignal::Bit(representative)) = (
            scalar_signal(module, &offsets, signal_bit_count, value)?,
            scalar_signal(module, &offsets, signal_bit_count, representative)?,
        ) else {
            return Err(crate::SynthError::invariant(
                "mapped value alias does not identify two scalar net bits",
            ));
        };
        aliases.union(value, representative);
    }

    // The mapped substrate represents only externally observed equivalence
    // classes; regional artifacts remain the owner of internal Word topology.
    let required_roots = required_substrate_roots(
        module,
        &offsets,
        signal_bit_count,
        observed_values,
        &mut aliases,
    )?;

    let mut builder =
        MappedBuilder::new(module.name(), base_revision).map_err(crate::SynthError::from)?;
    let mut root_nets = vec![None; bit_count];
    for (signal_index, signal) in module.signals().iter().enumerate() {
        let base = offsets[signal_index];
        for bit in 0..signal.ty.width() as usize {
            let root = aliases.find(base + bit);
            if !required_roots.contains(&root) || root_nets[root].is_some() {
                continue;
            }
            let name = signal.name.map(|name| module.name_str(name)).map(|name| {
                if signal.ty.width() == 1 {
                    name.to_string()
                } else {
                    format!("{name}[{bit}]")
                }
            });
            root_nets[root] = Some(
                builder
                    .add_net(name.as_deref())
                    .map_err(crate::SynthError::from)?,
            );
        }
    }
    for operation_index in 0..module.operations().len() {
        let bit = signal_bit_count + operation_index;
        let root = aliases.find(bit);
        if required_roots.contains(&root) && root_nets[root].is_none() {
            root_nets[root] = Some(
                builder
                    .add_net(Some(&format!("{MAPPED_NET_PREFIX}{operation_index}")))
                    .map_err(crate::SynthError::from)?,
            );
        }
    }
    let mut regional_pins = regional_pins.to_vec();
    regional_pins.sort_unstable();
    regional_pins.dedup();
    let regional_pins = regional_pins
        .into_iter()
        .map(|pin| {
            builder
                .add_net(None)
                .map(|net| (pin, net))
                .map_err(crate::SynthError::from)
        })
        .collect::<Result<Box<[_]>, _>>()?;
    if required_roots
        .iter()
        .any(|&root| root_nets.get(root).is_none_or(Option::is_none))
    {
        return Err(crate::SynthError::invariant(
            "required mapped substrate root has no canonical net",
        ));
    }

    for port in module.ports() {
        let signal = module.signal(port.signal).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped port references unknown signal {:?}",
                port.signal
            ))
        })?;
        let base = offsets[port.signal.index()];
        let nets = (0..signal.ty.width() as usize)
            .map(|bit| net_for_bit(&mut aliases, &root_nets, base + bit))
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let direction = match port.direction {
            word::PortDirection::Input => PortDirection::Input,
            word::PortDirection::Output => PortDirection::Output,
            word::PortDirection::Inout => PortDirection::Inout,
            word::PortDirection::Ref => {
                return Err(crate::SynthError::invariant(format!(
                    "reference port '{}' survived linked elaboration",
                    module.name_str(port.name)
                )));
            }
        };
        builder
            .add_port(module.name_str(port.name), direction, &nets)
            .map_err(crate::SynthError::from)?;
    }

    let mut cell_sources = Vec::new();
    let mut prepared_cells = prepare_substrate_instances(
        SubstrateInstanceDomain {
            module,
            options,
            design_references,
            reference_ports,
            source_instances,
            offsets: &offsets,
            signal_bit_count,
            root_nets: &root_nets,
        },
        &mut aliases,
        &mut builder,
    )?;
    append_tri_state_cells(
        module,
        options,
        &offsets,
        signal_bit_count,
        &mut aliases,
        &root_nets,
        &mut prepared_cells,
    )?;
    append_packed_cells(&mut builder, &mut cell_sources, prepared_cells)?;

    let observed_nets = observed_values
        .iter()
        .copied()
        .map(
            |value| match scalar_signal(module, &offsets, signal_bit_count, value)? {
                ScalarSignal::Bit(bit) => net_for_bit(&mut aliases, &root_nets, bit).map(Some),
                ScalarSignal::Constant(_) => Ok(None),
            },
        )
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    for (root, value) in constants {
        if let Some(net) = root_nets[root] {
            builder.drive_constant(net, value);
        }
    }
    let netlist = builder.freeze().map_err(crate::SynthError::from)?;
    if cell_sources.len() != netlist.cell_count() {
        return Err(crate::SynthError::invariant(format!(
            "{} live mapped cells have {} provenance sources",
            netlist.cell_count(),
            cell_sources.len()
        )));
    }
    Ok((
        MappedOutput {
            netlist,
            cell_sources: cell_sources.into_boxed_slice(),
        },
        observed_nets.into_boxed_slice(),
        RegionalMappedPins(regional_pins),
    ))
}

#[derive(Clone, Copy)]
struct SubstrateInstanceDomain<'a> {
    module: &'a word::WordModule,
    options: &'a SynthesisOptions,
    design_references: &'a BTreeSet<String>,
    reference_ports: &'a crate::ReferencePortMap,
    source_instances: &'a SourceInstanceProvenance,
    offsets: &'a [usize],
    signal_bit_count: usize,
    root_nets: &'a [Option<NetId>],
}

fn prepare_substrate_instances(
    domain: SubstrateInstanceDomain<'_>,
    aliases: &mut DisjointSets,
    builder: &mut MappedBuilder,
) -> Result<Vec<(MappedCellSpec, MappedCellSource)>, crate::SynthError> {
    let SubstrateInstanceDomain {
        module,
        options,
        design_references,
        reference_ports,
        source_instances,
        offsets,
        signal_bit_count,
        root_nets,
    } = domain;
    let instance_is_source = (0..module.instances().len())
        .map(|index| {
            let instance = word::InstId::from_index(index).map_err(crate::SynthError::Word)?;
            source_instances.is_source_instance(module, instance)
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    let mut prepared = Vec::new();
    for (instance_index, instance) in module.instances().iter().enumerate() {
        let cell_type = module.name_str(instance.module);
        let is_source_instance = instance_is_source[instance_index];
        if design_references.contains(cell_type) {
            if !is_source_instance {
                return Err(crate::SynthError::mapping(format!(
                    "synthesis introduced instance '{}' of design '{cell_type}'",
                    module.name_str(instance.name)
                )));
            }
            let mut connections = Vec::with_capacity(instance.connections.len());
            for connection in &instance.connections {
                let signals = scalar_signals(module, offsets, signal_bit_count, connection.value)?
                    .into_iter()
                    .map(|signal| match signal {
                        ScalarSignal::Bit(bit) => {
                            net_for_bit(aliases, root_nets, bit).map(ConnectionSignal::Net)
                        }
                        ScalarSignal::Constant(value) => Ok(ConnectionSignal::Constant(value)),
                    })
                    .collect::<Result<Vec<_>, crate::SynthError>>()?;
                connections.push((module.name_str(connection.port).to_string(), signals));
            }
            builder
                .add_design_instance(module.name_str(instance.name), cell_type, &connections)
                .map_err(crate::SynthError::from)?;
            continue;
        }
        let library_cell = options
            .target_cells
            .iter()
            .position(|cell| cell.name() == cell_type)
            .map(|index| {
                u32::try_from(index).map_err(|_| {
                    crate::SynthError::capacity("target cell index exceeds 32-bit capacity")
                })
            })
            .transpose()?;
        let target_cell = library_cell.and_then(|index| options.target_cells.get(index as usize));
        let introducible = target_cell.is_some_and(|cell| {
            cell.is_synthesis_eligible() || (!cell.dont_use() && cell.clock_gate().is_some())
        });
        if !is_source_instance && !introducible {
            return Err(crate::SynthError::mapping(format!(
                "synthesis introduced instance '{}' of cell '{cell_type}' outside the eligible target-cell set",
                module.name_str(instance.name)
            )));
        }
        let link_ports = reference_ports.get(cell_type);
        if target_cell.is_none() && link_ports.is_none() {
            return Err(crate::SynthError::mapping(format!(
                "source instance '{}' references unknown resolution-library cell '{cell_type}'",
                module.name_str(instance.name)
            )));
        }
        let mut connections = Vec::with_capacity(instance.connections.len());
        for connection in &instance.connections {
            let pin = module.name_str(connection.port);
            let library_pin = target_cell
                .and_then(|cell| cell.pins().position(|candidate| candidate.name() == pin))
                .map(|index| {
                    u16::try_from(index).map_err(|_| {
                        crate::SynthError::capacity("target pin index exceeds 16-bit capacity")
                    })
                })
                .transpose()?;
            if target_cell.is_some() && library_pin.is_none() {
                return Err(crate::SynthError::mapping(format!(
                    "instance '{}' connects unknown pin '{pin}' of target cell '{cell_type}'",
                    module.name_str(instance.name)
                )));
            }
            if target_cell.is_none() && link_ports.is_some_and(|ports| !ports.contains_key(pin)) {
                return Err(crate::SynthError::mapping(format!(
                    "instance '{}' connects unknown pin '{pin}' of resolution-library cell '{cell_type}'",
                    module.name_str(instance.name)
                )));
            }
            let signal = match scalar_signal(module, offsets, signal_bit_count, connection.value)? {
                ScalarSignal::Bit(bit) => {
                    ConnectionSignal::Net(net_for_bit(aliases, root_nets, bit)?)
                }
                ScalarSignal::Constant(value) => ConnectionSignal::Constant(value),
            };
            connections.push((pin.to_string(), library_pin, signal));
        }
        let instance_id =
            word::InstId::from_index(instance_index).map_err(crate::SynthError::Word)?;
        prepared.push((
            MappedCellSpec {
                name: module.name_str(instance.name).to_string(),
                cell_type: cell_type.to_string(),
                library_cell,
                connections,
            },
            MappedCellSource::Instance(instance_id),
        ));
    }
    Ok(prepared)
}

fn required_substrate_roots(
    module: &word::WordModule,
    offsets: &[usize],
    signal_bit_count: usize,
    observed_values: &[word::ValueId],
    aliases: &mut DisjointSets,
) -> Result<BTreeSet<usize>, crate::SynthError> {
    let mut roots = BTreeSet::new();
    for port in module.ports() {
        let signal = module.signal(port.signal).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped port references unknown signal {:?}",
                port.signal
            ))
        })?;
        let base = offsets[port.signal.index()];
        for bit in 0..signal.ty.width() as usize {
            roots.insert(aliases.find(base + bit));
        }
    }
    for instance in module.instances() {
        for connection in &instance.connections {
            for signal in scalar_signals(module, offsets, signal_bit_count, connection.value)? {
                if let ScalarSignal::Bit(bit) = signal {
                    roots.insert(aliases.find(bit));
                }
            }
        }
    }
    for connect in module.connects() {
        let Some(driver) = tri_state_connect_driver(module, connect)? else {
            continue;
        };
        roots.insert(aliases.find(scalar_target_bit(module, offsets, &connect.target)?));
        for value in [driver.data, driver.enable.value] {
            if let ScalarSignal::Bit(bit) = scalar_signal(module, offsets, signal_bit_count, value)?
            {
                roots.insert(aliases.find(bit));
            }
        }
    }
    for &value in observed_values {
        if let ScalarSignal::Bit(bit) = scalar_signal(module, offsets, signal_bit_count, value)? {
            roots.insert(aliases.find(bit));
        }
    }
    Ok(roots)
}

fn build_alias_classes(
    module: &word::WordModule,
    offsets: &[usize],
    signal_bit_count: usize,
    bit_count: usize,
) -> Result<(DisjointSets, BTreeMap<usize, bool>), crate::SynthError> {
    let mut aliases = DisjointSets::new(bit_count);
    let mut constants = BTreeMap::<usize, bool>::new();
    let semantics = super::roots::FullDomainRootSemantics::new(module)?;
    for (index, operation) in module.operations().iter().enumerate() {
        if module
            .value(operation.result)
            .is_none_or(|value| value.ty.width() != 1)
        {
            continue;
        }
        let input = semantics.canonical_root(operation.result)?;
        if input == operation.result {
            continue;
        }
        let output = signal_bit_count + index;
        match scalar_signal(module, offsets, signal_bit_count, input)? {
            ScalarSignal::Bit(input) => aliases.union(output, input),
            ScalarSignal::Constant(value) => record_constant(&mut constants, output, value)?,
        }
    }
    for connect in module.connects() {
        let target = scalar_target_bit(module, offsets, &connect.target)?;
        if tri_state_connect_driver(module, connect)?.is_some() {
            continue;
        }
        match scalar_signal(module, offsets, signal_bit_count, connect.value)? {
            ScalarSignal::Bit(source) => aliases.union(target, source),
            ScalarSignal::Constant(value) => record_constant(&mut constants, target, value)?,
        }
    }
    let mut canonical_constants = BTreeMap::<usize, bool>::new();
    for (bit, value) in constants {
        let root = aliases.find(bit);
        if canonical_constants
            .insert(root, value)
            .is_some_and(|previous| previous != value)
        {
            return Err(crate::SynthError::invariant(
                "one mapped net has conflicting constant drivers",
            ));
        }
    }
    Ok((aliases, canonical_constants))
}

#[derive(Clone, Copy)]
struct TriStateDriver {
    data: word::ValueId,
    enable: word::Enable,
}

fn tri_state_connect_driver(
    module: &word::WordModule,
    connect: &word::Connect,
) -> Result<Option<TriStateDriver>, crate::SynthError> {
    let signal = module.signal(connect.target.signal).ok_or_else(|| {
        crate::SynthError::invariant("tri-state connect target signal is unknown")
    })?;
    if signal.resolution != word::SignalResolution::TriState {
        return Ok(None);
    }
    let value = module
        .value(connect.value)
        .ok_or_else(|| crate::SynthError::invariant("tri-state connect value is unknown"))?;
    if value.ty.width() != 1 {
        return Err(crate::SynthError::invariant(
            "vector tri-state driver reached mapped netlist conversion",
        ));
    }
    let word::ValueKind::Operation(operation) = value.kind else {
        return Err(crate::SynthError::invariant(
            "physical tri-state contribution is not an explicit driver operation",
        ));
    };
    let operation = module
        .operation(operation)
        .ok_or_else(|| crate::SynthError::invariant("tri-state driver operation is unknown"))?;
    let word::OpKind::TriState { data, enable } = operation.kind else {
        return Err(crate::SynthError::invariant(
            "physical tri-state contribution has no data/enable contract",
        ));
    };
    if module.value(data).is_none_or(|value| value.ty.width() != 1)
        || module
            .value(enable.value)
            .is_none_or(|value| value.ty.width() != 1)
    {
        return Err(crate::SynthError::invariant(
            "scalar tri-state driver has a non-scalar data or enable input",
        ));
    }
    Ok(Some(TriStateDriver { data, enable }))
}

#[derive(Clone, Copy)]
struct TriStateTarget<'a> {
    library_cell: u32,
    cell: opto_library::TargetCellRef<'a>,
    data_pin: &'a str,
    enable_pin: &'a str,
    output_pin: &'a str,
}

fn select_tri_state_target(
    cells: &opto_library::TargetCellSet,
    active_high: bool,
) -> Result<TriStateTarget<'_>, crate::SynthError> {
    let mut compatible = cells
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            if cell.dont_use()
                || !cell.usage().is_general_purpose()
                || cell.clock_gate().is_some()
                || cell.memory().is_some()
                || cell.sequential().next().is_some()
                || cell.pins().len() != 3
            {
                return None;
            }
            let inputs = cell
                .pins()
                .filter(|pin| pin.direction() == opto_library::TargetPinDirection::Input)
                .collect::<Vec<_>>();
            let outputs = cell
                .pins()
                .filter(|pin| {
                    matches!(
                        pin.direction(),
                        opto_library::TargetPinDirection::Output
                            | opto_library::TargetPinDirection::Inout
                    )
                })
                .collect::<Vec<_>>();
            if inputs.len() != 2 || outputs.len() != 1 {
                return None;
            }
            let output = outputs[0];
            let (data_pin, data_polarity) = output.function()?.as_literal()?;
            let (enable_pin, disabled_polarity) = output.three_state()?.as_literal()?;
            if !data_polarity
                || data_pin == enable_pin
                || disabled_polarity == active_high
                || !inputs.iter().any(|pin| pin.name() == data_pin)
                || !inputs.iter().any(|pin| pin.name() == enable_pin)
            {
                return None;
            }
            let library_cell = u32::try_from(index).ok()?;
            Some(TriStateTarget {
                library_cell,
                cell,
                data_pin,
                enable_pin,
                output_pin: output.name(),
            })
        })
        .collect::<Vec<_>>();
    compatible.sort_by(|left, right| {
        opto_library::normalized_cell_area(left.cell.area())
            .total_cmp(&opto_library::normalized_cell_area(right.cell.area()))
            .then_with(|| left.cell.name().cmp(right.cell.name()))
            .then_with(|| left.library_cell.cmp(&right.library_cell))
    });
    compatible.into_iter().next().ok_or_else(|| {
        crate::SynthError::mapping(format!(
            "target library has no compatible active-{} tri-state buffer",
            if active_high { "high" } else { "low" }
        ))
    })
}

fn append_tri_state_cells(
    module: &word::WordModule,
    options: &SynthesisOptions,
    offsets: &[usize],
    signal_bit_count: usize,
    aliases: &mut DisjointSets,
    root_nets: &[Option<NetId>],
    prepared: &mut Vec<(MappedCellSpec, MappedCellSource)>,
) -> Result<(), crate::SynthError> {
    let mut used_names = module
        .instances()
        .iter()
        .map(|instance| module.name_str(instance.name).to_string())
        .collect::<BTreeSet<_>>();
    for (index, connect) in module.connects().iter().enumerate() {
        let Some(driver) = tri_state_connect_driver(module, connect)? else {
            continue;
        };
        let target = select_tri_state_target(&options.target_cells, driver.enable.active_high)?;
        let data = scalar_signal(module, offsets, signal_bit_count, driver.data)?;
        let enable = scalar_signal(module, offsets, signal_bit_count, driver.enable.value)?;
        let output = ConnectionSignal::Net(net_for_bit(
            aliases,
            root_nets,
            scalar_target_bit(module, offsets, &connect.target)?,
        )?);
        let mut connection = |signal| match signal {
            ScalarSignal::Bit(bit) => {
                net_for_bit(aliases, root_nets, bit).map(ConnectionSignal::Net)
            }
            ScalarSignal::Constant(value) => Ok(ConnectionSignal::Constant(value)),
        };
        let data = connection(data)?;
        let enable = connection(enable)?;
        let mut suffix = index;
        let name = loop {
            let candidate = format!("{TRI_STATE_CELL_PREFIX}{suffix}");
            if used_names.insert(candidate.clone()) {
                break candidate;
            }
            suffix = suffix.checked_add(1).ok_or_else(|| {
                crate::SynthError::capacity("tri-state cell name suffix overflow")
            })?;
        };
        prepared.push((
            MappedCellSpec {
                name,
                cell_type: target.cell.name().to_string(),
                library_cell: Some(target.library_cell),
                connections: vec![
                    (
                        target.data_pin.to_string(),
                        Some(target_pin_id(target.cell, target.data_pin)?),
                        data,
                    ),
                    (
                        target.enable_pin.to_string(),
                        Some(target_pin_id(target.cell, target.enable_pin)?),
                        enable,
                    ),
                    (
                        target.output_pin.to_string(),
                        Some(target_pin_id(target.cell, target.output_pin)?),
                        output,
                    ),
                ],
            },
            MappedCellSource::StructuralValue(connect.value),
        ));
    }
    Ok(())
}

fn record_constant(
    constants: &mut BTreeMap<usize, bool>,
    bit: usize,
    value: bool,
) -> Result<(), crate::SynthError> {
    if constants
        .insert(bit, value)
        .is_some_and(|previous| previous != value)
    {
        return Err(crate::SynthError::invariant(
            "one mapped value has conflicting constant drivers",
        ));
    }
    Ok(())
}

fn append_packed_cells(
    builder: &mut MappedBuilder,
    cell_sources: &mut Vec<(CellId, MappedCellSource)>,
    prepared: Vec<(MappedCellSpec, MappedCellSource)>,
) -> Result<(), crate::SynthError> {
    let (cells, sources): (Vec<_>, Vec<_>) = prepared.into_iter().unzip();
    let ids = builder
        .add_cells_packed(cells)
        .map_err(crate::SynthError::from)?;
    if ids.len() != sources.len() {
        return Err(crate::SynthError::invariant(
            "packed mapped cells lost their provenance source rows",
        ));
    }
    cell_sources.extend(ids.iter().copied().zip(sources));
    Ok(())
}

fn signal_offsets(module: &word::WordModule) -> Result<Vec<usize>, crate::SynthError> {
    let mut offsets = Vec::with_capacity(module.signals().len());
    let mut next = 0usize;
    for signal in module.signals() {
        offsets.push(next);
        next = next
            .checked_add(signal.ty.width() as usize)
            .ok_or_else(|| crate::SynthError::invariant("mapped signal offset overflow"))?;
    }
    Ok(offsets)
}

fn scalar_target_bit(
    module: &word::WordModule,
    offsets: &[usize],
    target: &word::LValue,
) -> Result<usize, crate::SynthError> {
    if target.dynamic.is_some() {
        return Err(crate::SynthError::invariant(
            "dynamic target reached mapped netlist conversion",
        ));
    }
    let signal = module.signal(target.signal).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "mapped connect references unknown signal {:?}",
            target.signal
        ))
    })?;
    let bit = match target.range {
        Some(range) if range.width() == 1 => range.lsb,
        None if signal.ty.width() == 1 => 0,
        _ => {
            return Err(crate::SynthError::invariant(
                "vector connect reached mapped netlist conversion",
            ));
        }
    };
    offsets[target.signal.index()]
        .checked_add(bit as usize)
        .ok_or_else(|| crate::SynthError::invariant("mapped target bit offset overflow"))
}

#[derive(Clone, Copy)]
enum ScalarSignal {
    Bit(usize),
    Constant(bool),
}

fn scalar_signal(
    module: &word::WordModule,
    offsets: &[usize],
    operation_base: usize,
    value_id: word::ValueId,
) -> Result<ScalarSignal, crate::SynthError> {
    let value = module.value(value_id).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "mapped netlist references unknown value {value_id:?}"
        ))
    })?;
    if value.ty.width() != 1 {
        return Err(crate::SynthError::invariant(
            "vector value reached mapped netlist conversion",
        ));
    }
    match &value.kind {
        word::ValueKind::Signal(reference) => offsets[reference.signal.index()]
            .checked_add(reference.lsb as usize)
            .map(ScalarSignal::Bit)
            .ok_or_else(|| crate::SynthError::invariant("mapped value bit offset overflow")),
        word::ValueKind::Constant(constant) => match constant.bit_lsb(0) {
            Some(bit) => crate::boolean::resolve_publication_bit(bit, module.name(), &value.source)
                .map(|resolved| ScalarSignal::Constant(resolved == BitVal::One)),
            None => Err(crate::SynthError::invariant(format!(
                "mapped netlist for '{}' cannot read constant value {value_id:?} at {:?}",
                module.name(),
                value.source
            ))),
        },
        word::ValueKind::Operation(operation) => operation_base
            .checked_add(operation.index())
            .map(ScalarSignal::Bit)
            .ok_or_else(|| crate::SynthError::invariant("mapped operation bit offset overflow")),
    }
}

fn scalar_signals(
    module: &word::WordModule,
    offsets: &[usize],
    operation_base: usize,
    value: word::ValueId,
) -> Result<Vec<ScalarSignal>, crate::SynthError> {
    let mut signals = Vec::new();
    for_each_scalar_signal(module, offsets, operation_base, value, &mut |signal| {
        signals.push(signal);
        Ok(())
    })?;
    Ok(signals)
}

fn for_each_scalar_signal(
    module: &word::WordModule,
    offsets: &[usize],
    operation_base: usize,
    value: word::ValueId,
    visit: &mut impl FnMut(ScalarSignal) -> Result<(), crate::SynthError>,
) -> Result<(), crate::SynthError> {
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant(format!("mapped netlist references unknown value {value:?}"))
    })?;
    if stored.ty.width() == 1 {
        return visit(scalar_signal(module, offsets, operation_base, value)?);
    }
    let word::ValueKind::Operation(operation) = stored.kind else {
        return Err(crate::SynthError::invariant(
            "vector design-instance connection was not bitblasted",
        ));
    };
    let operation = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown concatenation operation {operation:?}"))
    })?;
    let word::OpKind::Concat { parts } = &operation.kind else {
        return Err(crate::SynthError::invariant(
            "vector design-instance connection is not a concatenation of scalar bits",
        ));
    };
    for &part in parts.iter().rev() {
        for_each_scalar_signal(module, offsets, operation_base, part, visit)?;
    }
    Ok(())
}

fn net_for_bit(
    aliases: &mut DisjointSets,
    root_nets: &[Option<NetId>],
    bit: usize,
) -> Result<NetId, crate::SynthError> {
    let root = aliases.find(bit);
    root_nets.get(root).copied().flatten().ok_or_else(|| {
        crate::SynthError::invariant(format!("mapped bit {bit} has no canonical net"))
    })
}

struct DisjointSets {
    parents: Vec<usize>,
}

impl DisjointSets {
    fn new(len: usize) -> Self {
        Self {
            parents: (0..len).collect(),
        }
    }

    fn find(&mut self, mut value: usize) -> usize {
        let mut root = value;
        while self.parents[root] != root {
            root = self.parents[root];
        }
        while self.parents[value] != value {
            let parent = self.parents[value];
            self.parents[value] = root;
            value = parent;
        }
        root
    }

    fn union(&mut self, left: usize, right: usize) {
        let left = self.find(left);
        let right = self.find(right);
        if left != right {
            let (representative, replaced) = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            self.parents[replaced] = representative;
        }
    }
}

#[cfg(test)]
mod tests;
