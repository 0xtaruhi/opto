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

pub(crate) mod region_delta;
mod sequential_delta;

/// The prefix every synthetic internal net name carries.
pub(crate) const MAPPED_NET_PREFIX: &str = "_mapped_net_";

use crate::artifact::MappedCellSource;
pub(crate) use region_delta::REGION_CELL_PREFIX;
pub(crate) use sequential_delta::{MappedSequentialArtifact, sequential_binding_values};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ArtifactSignal {
    Mapped(region_delta::MappedValueSignal),
    LocalNet(usize),
}

impl ArtifactSignal {
    fn connection(
        self,
        local_nets: &[TempNetId],
        missing_local_net: &'static str,
    ) -> Result<ConnectionRef, crate::SynthError> {
        match self {
            Self::Mapped(signal) => Ok(signal.connection()),
            Self::LocalNet(index) => local_nets
                .get(index)
                .copied()
                .map(ConnectionRef::NewNet)
                .ok_or_else(|| crate::SynthError::invariant(missing_local_net)),
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
    local_net_count: usize,
    cells: &[ArtifactCell<M>],
    missing_local_net: &'static str,
    output: impl Fn(TempCellId, &M) -> O,
) -> Result<AppendedArtifactCells<O>, crate::SynthError> {
    let local_nets = (0..local_net_count)
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
                    signal.connection(&local_nets, missing_local_net)?,
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

#[derive(Debug)]
pub(crate) struct MappedOutput {
    pub(crate) netlist: MappedNetlist,
    pub(crate) cell_sources: Box<[(CellId, MappedCellSource)]>,
}

pub(crate) type MappedSubstrate = (MappedOutput, Box<[Option<NetId>]>);

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
    outputs: Box<[ObservableOutput]>,
    static_driver_counts: Box<[u8]>,
    source_driver_pins: BTreeSet<PinId>,
}

impl FrozenObservableConnectivity {
    pub(crate) fn capture(
        mapped: &MappedNetlist,
        target_cells: &opto_library::TargetCellSet,
        reference_ports: &crate::ReferencePortMap,
    ) -> Result<Self, crate::SynthError> {
        let mut boundary_nets = BTreeSet::new();
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
            }
        }
        for &(net, _) in mapped.constant_drivers() {
            static_driver_counts[net.index()] = static_driver_counts[net.index()].saturating_add(1);
        }
        let frozen = Self {
            boundary_nets,
            outputs: outputs.into_boxed_slice(),
            static_driver_counts: static_driver_counts.into_boxed_slice(),
            source_driver_pins,
        };
        frozen.validate(mapped, target_cells)?;
        Ok(frozen)
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
        for output in self
            .outputs
            .iter()
            .filter(|output| affected.contains(&output.net))
        {
            if self.driver_count(mapped, target_cells, output.net)? != 1 {
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
            match self.driver_count(mapped, target_cells, output.net)? {
                1 => {}
                0 => {
                    return Err(crate::SynthError::invariant(format!(
                        "mapped output '{}[{}]' has no physical driver",
                        output.name, output.bit
                    )));
                }
                _ => {
                    return Err(crate::SynthError::invariant(format!(
                        "mapped output '{}[{}]' has multiple physical drivers",
                        output.name, output.bit
                    )));
                }
            }
        }
        Ok(())
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
            .ok_or_else(|| {
                crate::SynthError::invariant("mapped boundary net exceeds connectivity freeze")
            })?;
        for pin in mapped.pins_on_net(net).into_iter().flatten() {
            if self.source_driver_pins.contains(&pin) {
                count = count.saturating_add(1);
                continue;
            }
            let cell = mapped.pin_owner(pin).ok_or_else(|| {
                crate::SynthError::invariant("mapped boundary pin has no live owner")
            })?;
            let stored = mapped
                .cell(cell)
                .ok_or_else(|| crate::SynthError::invariant("mapped boundary cell is unknown"))?;
            let Some(library_index) = stored.library_cell else {
                continue;
            };
            let connection = mapped.connection(pin).ok_or_else(|| {
                crate::SynthError::invariant("mapped driver cell has no pin binding")
            })?;
            let library_cell = target_cells.get(library_index as usize).ok_or_else(|| {
                crate::SynthError::invariant("mapped driver cell is absent from the target library")
            })?;
            let library_pin = match connection.library_pin {
                Some(pin) => library_cell.pins().nth(pin as usize),
                None => mapped
                    .pin_name(connection)
                    .and_then(|name| library_cell.pins().find(|pin| pin.name() == name)),
            }
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "mapped target-cell pin is absent from its target-library cell",
                )
            })?;
            if matches!(
                library_pin.direction(),
                opto_library::TargetPinDirection::Output | opto_library::TargetPinDirection::Inout
            ) {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
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
    let (output, _) = build_mapped_substrate(MappedSubstrateRequest {
        module,
        options,
        design_references,
        reference_ports,
        source_instances,
        base_revision,
        observed_values: &[],
    })?;
    Ok(output)
}

/// Builds the immutable global substrate and Word-value net bindings.
///
/// Only full-domain aliases, constants, ports, retained instances, state, and
/// explicitly observed roots enter this owner. Region-local care reductions
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
        builder
            .add_port(
                module.name_str(port.name),
                match port.direction {
                    word::PortDirection::Input => PortDirection::Input,
                    word::PortDirection::Output => PortDirection::Output,
                    word::PortDirection::Inout => PortDirection::Inout,
                },
                &nets,
            )
            .map_err(crate::SynthError::from)?;
    }

    let instance_is_source = (0..module.instances().len())
        .map(|index| {
            let instance = word::InstId::from_index(index).map_err(crate::SynthError::Word)?;
            source_instances.is_source_instance(module, instance)
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    let mut cell_sources = Vec::new();
    let mut prepared_cells = Vec::new();
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
                let signals = scalar_signals(module, &offsets, signal_bit_count, connection.value)?
                    .into_iter()
                    .map(|signal| match signal {
                        ScalarSignal::Bit(bit) => {
                            net_for_bit(&mut aliases, &root_nets, bit).map(ConnectionSignal::Net)
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
            let signal = match scalar_signal(module, &offsets, signal_bit_count, connection.value)?
            {
                ScalarSignal::Bit(bit) => {
                    ConnectionSignal::Net(net_for_bit(&mut aliases, &root_nets, bit)?)
                }
                ScalarSignal::Constant(value) => ConnectionSignal::Constant(value),
            };
            connections.push((pin.to_string(), library_pin, signal));
        }
        let instance_name = module.name_str(instance.name).to_string();
        let instance_id =
            word::InstId::from_index(instance_index).map_err(crate::SynthError::Word)?;
        prepared_cells.push((
            MappedCellSpec {
                name: instance_name,
                cell_type: cell_type.to_string(),
                library_cell,
                connections,
            },
            MappedCellSource::Instance(instance_id),
        ));
    }
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
    ))
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
