// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Construction of the immutable mapped substrate and direct mapped artifacts.

use crate::SynthesisOptions;
use crate::artifact::provenance::SourceInstanceProvenance;
use opto_ir::BitVal;
use opto_ir::mapped::{
    CellId, ConnectionSignal, MappedBuilder, MappedCellSpec, MappedNetlist, NetId, PortDirection,
};
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod boundary_delta;
pub(crate) mod region_delta;
mod sequential_delta;

/// The prefix every synthetic internal net name carries.
pub(crate) const MAPPED_NET_PREFIX: &str = "_mapped_net_";

use crate::artifact::MappedCellSource;
pub(crate) use region_delta::REGION_CELL_PREFIX;
use region_delta::{BoundaryAlias, BoundaryAliasSource};
pub(crate) use sequential_delta::{MappedSequentialArtifact, sequential_binding_values};

#[derive(Debug)]
pub(crate) struct MappedOutput {
    pub(crate) netlist: MappedNetlist,
    pub(crate) cell_sources: Box<[(CellId, MappedCellSource)]>,
}

pub(crate) struct MappedSubstrate {
    pub(crate) netlist: MappedNetlist,
    pub(crate) cell_sources: Box<[(CellId, MappedCellSource)]>,
    pub(crate) observed_nets: Box<[Option<NetId>]>,
}

#[derive(Clone, Copy)]
pub(crate) struct MappedSubstrateRequest<'a> {
    pub(crate) module: &'a word::WordModule,
    pub(crate) options: &'a SynthesisOptions,
    pub(crate) design_references: &'a BTreeSet<String>,
    pub(crate) reference_ports: &'a crate::ReferencePortMap,
    pub(crate) source_instances: &'a SourceInstanceProvenance,
    pub(crate) base_revision: opto_ir::RevisionId,
    pub(crate) observed_values: &'a [word::ValueId],
    pub(crate) boundary_aliases: &'a [BoundaryAlias],
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
    let MappedSubstrate {
        netlist,
        cell_sources,
        observed_nets: _,
    } = build_mapped_substrate(MappedSubstrateRequest {
        module,
        options,
        design_references,
        reference_ports,
        source_instances,
        base_revision,
        observed_values: &[],
        boundary_aliases: &[],
    })?;
    Ok(MappedOutput {
        netlist,
        cell_sources,
    })
}

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
        boundary_aliases,
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
    let (mut aliases, constants) = build_alias_classes(
        module,
        &offsets,
        signal_bit_count,
        bit_count,
        boundary_aliases,
    )?;

    // The mapped substrate represents only externally observed equivalence
    // classes; regional artifacts remain the owner of internal Word topology.
    let required_roots = required_substrate_roots(
        module,
        &offsets,
        signal_bit_count,
        observed_values,
        boundary_aliases,
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
    Ok(MappedSubstrate {
        netlist,
        cell_sources: cell_sources.into_boxed_slice(),
        observed_nets: observed_nets.into_boxed_slice(),
    })
}

fn required_substrate_roots(
    module: &word::WordModule,
    offsets: &[usize],
    signal_bit_count: usize,
    observed_values: &[word::ValueId],
    boundary_aliases: &[BoundaryAlias],
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
    for alias in boundary_aliases {
        if let ScalarSignal::Bit(bit) =
            scalar_signal(module, offsets, signal_bit_count, alias.target)?
        {
            roots.insert(aliases.find(bit));
        }
        if let BoundaryAliasSource::Value(value) = alias.source
            && let ScalarSignal::Bit(bit) = scalar_signal(module, offsets, signal_bit_count, value)?
        {
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
    boundary_aliases: &[BoundaryAlias],
) -> Result<(DisjointSets, BTreeMap<usize, bool>), crate::SynthError> {
    let mut aliases = DisjointSets::new(bit_count);
    let mut constants = BTreeMap::<usize, bool>::new();
    for (index, operation) in module.operations().iter().enumerate() {
        let Some(input) = scalar_cast_input(module, operation) else {
            continue;
        };
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
    let mut constant_aliases = BTreeMap::<word::ValueId, usize>::new();
    for alias in boundary_aliases {
        let target = match scalar_signal(module, offsets, signal_bit_count, alias.target)? {
            ScalarSignal::Bit(bit) => bit,
            ScalarSignal::Constant(target) => {
                let source = match alias.source {
                    BoundaryAliasSource::Value(value) => {
                        scalar_signal(module, offsets, signal_bit_count, value)?
                    }
                    BoundaryAliasSource::Constant(value) => ScalarSignal::Constant(value),
                };
                match source {
                    ScalarSignal::Constant(source) if source == target => continue,
                    ScalarSignal::Constant(source) => {
                        return Err(crate::SynthError::invariant(format!(
                            "regional boundary alias binds constant {source} to constant {target}"
                        )));
                    }
                    ScalarSignal::Bit(source) => {
                        record_constant(&mut constants, source, target)?;
                        continue;
                    }
                }
            }
        };
        match alias.source {
            BoundaryAliasSource::Value(value) => {
                match scalar_signal(module, offsets, signal_bit_count, value)? {
                    ScalarSignal::Bit(source) => aliases.union(target, source),
                    ScalarSignal::Constant(constant) => {
                        record_constant(&mut constants, target, constant)?;
                        // Constant sources have no bit to union. Their targets
                        // must still share one class so a multi-use boundary
                        // value materializes as one canonical mapped net.
                        match constant_aliases.entry(value) {
                            std::collections::btree_map::Entry::Vacant(slot) => {
                                slot.insert(target);
                            }
                            std::collections::btree_map::Entry::Occupied(slot) => {
                                aliases.union(target, *slot.get());
                            }
                        }
                    }
                }
            }
            BoundaryAliasSource::Constant(value) => {
                record_constant(&mut constants, target, value)?;
            }
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

pub(crate) fn scalar_cast_input(
    module: &word::WordModule,
    operation: &word::Operation,
) -> Option<word::ValueId> {
    let word::OpKind::Cast { value, .. } = operation.kind else {
        return None;
    };
    (module.value(operation.result)?.ty.width() == 1 && module.value(value)?.ty.width() == 1)
        .then_some(value)
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
            Some(BitVal::Zero) => Ok(ScalarSignal::Constant(false)),
            Some(BitVal::One) => Ok(ScalarSignal::Constant(true)),
            Some(bit @ (BitVal::X | BitVal::Z)) => Err(crate::SynthError::invariant(format!(
                "mapped netlist for '{}' cannot contain unresolved {bit:?} in value {value_id:?} at {:?}",
                module.name(),
                value.source
            ))),
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
