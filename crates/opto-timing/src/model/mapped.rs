// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::design::CompactTimingDesignBuilder;
use super::{
    DesignId, MappedNetId, Parasitics, PortId, SealedTopology, SharedTimingDesign,
    TimingConnection, TimingDesign, TimingInstance, TimingInstanceId, TimingLibrary, TimingModel,
    TimingNet, TimingPort, TimingPortDirection, TimingRegionDelta,
};
use crate::PortBindings;
use opto_ir::mapped::ConnectionSignal;
use std::borrow::Cow;
use std::collections::BTreeMap;

/// Port directions for retained opaque design definitions.
pub type MappedDesignPortDirections =
    BTreeMap<String, BTreeMap<String, opto_ir::word::PortDirection>>;

impl TimingModel {
    /// Builds a timing model from a flat mapped netlist without parasitics.
    ///
    /// # Errors
    ///
    /// Returns an error for hierarchy, port binding, cell linking, topology, or
    /// compact-capacity failures.
    pub fn from_mapped(
        netlist: &opto_ir::mapped::MappedNetlist,
        design_id: DesignId,
        port_bindings: &PortBindings,
        library: TimingLibrary,
    ) -> Result<Self, crate::TimingError> {
        Self::from_mapped_with_parasitics(
            netlist,
            design_id,
            port_bindings,
            library,
            Parasitics::default(),
            &MappedDesignPortDirections::new(),
        )
    }

    /// Builds a timing model from a mapped netlist and parasitic view.
    /// Retained opaque design ports become timing cut boundaries.
    ///
    /// # Errors
    ///
    /// Returns an error for hierarchy, port binding, cell linking, parasitic,
    /// topology, or compact-capacity failures.
    pub fn from_mapped_with_parasitics(
        netlist: &opto_ir::mapped::MappedNetlist,
        design_id: DesignId,
        port_bindings: &PortBindings,
        library: TimingLibrary,
        parasitics: Parasitics,
        design_ports: &MappedDesignPortDirections,
    ) -> Result<Self, crate::TimingError> {
        let (design, topology) =
            mapped_sealed_source(netlist, design_id, port_bindings, design_ports)?;
        let mut model = Self::from_sealed_source(
            design,
            topology,
            library,
            parasitics,
            Some(netlist.generation_id()),
            0,
        )?;
        let bindings = netlist
            .net_ids()
            .filter(|&net| mapped_net_has_timing_presence(netlist, &model.mapped_port_nets, net))
            .map(|net| (net, mapped_net_name_cow(netlist, net)));
        let replaced_binding_memory_bytes = model
            .timing_to_mapped_net
            .owned_memory_bytes()
            .saturating_add(model.mapped_to_timing_net.owned_memory_bytes());
        let dense_bindings = super::access::seal_dense_mapped_bindings(
            &model.graph,
            &mut model.topology,
            netlist.net_slot_count(),
            bindings,
        )?;
        // The previous columns remain resident until both dense replacements
        // have been sealed, including the owned fallback name live in that phase.
        model.construction_scratch_high_water_bytes =
            model.construction_scratch_high_water_bytes.max(
                replaced_binding_memory_bytes
                    .saturating_add(dense_bindings.scratch_high_water_bytes),
            );
        model.timing_to_mapped_net = dense_bindings.timing_to_mapped;
        model.mapped_to_timing_net = dense_bindings.mapped_to_timing;
        model.generation =
            super::TimingGeneration::seal(model.topology.fingerprint(), model.analysis_inputs);
        Ok(model)
    }
}

fn mapped_sealed_source(
    netlist: &opto_ir::mapped::MappedNetlist,
    id: DesignId,
    port_bindings: &PortBindings,
    design_ports: &MappedDesignPortDirections,
) -> Result<(SharedTimingDesign, SealedTopology), crate::TimingError> {
    let ports = mapped_root_ports(netlist, port_bindings, design_ports)?;
    let mut names = crate::analysis::TimingNetNamesBuilder::new();
    let port_nets = ports
        .iter()
        .map(|port| names.intern(port.net.name()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut instance_nets = crate::analysis::InstanceNetArena::builder(netlist.cell_slot_count())?;
    let mut design = CompactTimingDesignBuilder::new(netlist.cell_count());
    let mut row_scratch_high_water_bytes = 0usize;
    for cell in netlist.cell_ids() {
        let index = cell.index();
        let instance = TimingInstanceId::from_raw(index.try_into().map_err(|_| {
            crate::TimingModelError::Capacity {
                resource: "instance ID",
            }
        })?);
        let instance_name = netlist
            .cell_name(cell)
            .ok_or(crate::TimingModelError::MappedCellMissingName { index })?;
        let cell_type = netlist
            .cell_type(cell)
            .ok_or(crate::TimingModelError::MappedCellMissingType { index })?;
        let connections = netlist
            .connections(cell)
            .ok_or(crate::TimingModelError::MappedCellInvalidPinRange { index })?;
        let mut nets = Vec::new();
        nets.try_reserve_exact(connections.len()).map_err(|_| {
            crate::TimingModelError::Capacity {
                resource: "instance-net row",
            }
        })?;
        row_scratch_high_water_bytes = row_scratch_high_water_bytes.max(
            opto_core::resident::slice_bytes::<super::TimingNetId>(connections.len()),
        );
        for connection in connections {
            netlist
                .pin_name(connection)
                .ok_or(crate::TimingModelError::MappedCellUnnamedPin { index })?;
            let net = match connection.signal {
                ConnectionSignal::Net(net) => mapped_net_name_cow(netlist, net),
                ConnectionSignal::Constant(value) => Cow::Borrowed(super::constant_net_name(value)),
            };
            nets.push(names.intern(&net)?);
        }
        design.push(
            instance,
            instance_name,
            cell_type,
            connections.iter().map(|connection| {
                netlist
                    .pin_name(connection)
                    .expect("mapped pin names were validated before compact insertion")
            }),
        )?;
        instance_nets.push(instance, nets.into_iter())?;
    }
    let design = SharedTimingDesign::from_builder(design, id, netlist.name().to_string(), ports);
    Ok((
        design,
        SealedTopology {
            net_names: names.finish(),
            port_nets: port_nets.into_boxed_slice(),
            instance_nets: instance_nets.finish()?,
            construction_scratch_high_water_bytes: row_scratch_high_water_bytes,
        },
    ))
}

impl TimingDesign {
    /// Converts a flat mapped netlist into stable timing-domain records.
    ///
    /// # Errors
    ///
    /// Rejects child design instances, incomplete typed port bindings, invalid
    /// mapped ranges, unnamed objects, and malformed pin ownership.
    pub fn from_mapped(
        netlist: &opto_ir::mapped::MappedNetlist,
        id: DesignId,
        port_bindings: &PortBindings,
    ) -> Result<Self, crate::TimingError> {
        if netlist.design_instance_count() != 0 {
            return Err(crate::TimingModelError::InvalidMappedHierarchy {
                detail: format!(
                    "design '{}' contains child instances; a hierarchy resolver is required",
                    netlist.name()
                ),
            }
            .into());
        }
        let ports = mapped_root_ports(netlist, port_bindings, &MappedDesignPortDirections::new())?;

        let instances = netlist
            .cell_ids()
            .map(|cell| timing_instance_from_mapped(netlist, cell))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            id,
            name: netlist.name().to_string(),
            ports,
            instances,
        })
    }
}

fn mapped_root_ports(
    netlist: &opto_ir::mapped::MappedNetlist,
    port_bindings: &PortBindings,
    design_ports: &MappedDesignPortDirections,
) -> Result<Vec<TimingPort>, crate::TimingError> {
    use opto_ir::mapped::{PortDirection, PortId as MappedPortId};

    if port_bindings.len() != netlist.ports().len() {
        return Err(crate::TimingModelError::MappedPortBindingCount {
            ports: netlist.ports().len(),
            bindings: port_bindings.len(),
        }
        .into());
    }
    let mut ports = Vec::new();
    for (index, port) in netlist.ports().iter().enumerate() {
        let port_id = MappedPortId::from_index(index).map_err(crate::TimingError::Mapped)?;
        let name = netlist
            .port_name(port_id)
            .ok_or(crate::TimingModelError::MappedPortMissingName { index })?;
        let object = port_bindings.get(index).ok_or_else(|| {
            crate::TimingModelError::MappedPortMissingObject {
                name: name.to_string(),
            }
        })?;
        let nets = netlist
            .port_nets(port_id)
            .ok_or(crate::TimingModelError::MappedPortInvalidNetRange { index })?;
        for (bit, &net) in nets.iter().enumerate() {
            let object_name = if nets.len() == 1 {
                name.to_string()
            } else {
                format!("{name}[{bit}]")
            };
            let net_name = mapped_net_name(netlist, net);
            ports.push(TimingPort {
                id: object,
                name: object_name,
                net: TimingNet::mapped(net_name.clone(), net),
                direction: match port.direction {
                    PortDirection::Input => TimingPortDirection::Input,
                    PortDirection::Output => TimingPortDirection::Output,
                    PortDirection::Inout => TimingPortDirection::Inout,
                },
            });
        }
    }
    let mut next_uid = ports
        .iter()
        .map(|port| port.id.uid().get().get())
        .max()
        .unwrap_or(0);
    for instance in netlist.design_instance_ids() {
        let instance_name = netlist.design_instance_name(instance).ok_or_else(|| {
            crate::TimingModelError::InvalidMappedHierarchy {
                detail: "retained design instance has no name".to_string(),
            }
        })?;
        let module = netlist.design_instance_module(instance).ok_or_else(|| {
            crate::TimingModelError::InvalidMappedHierarchy {
                detail: format!("retained design instance '{instance_name}' has no definition"),
            }
        })?;
        for connection in netlist
            .design_instance_connections(instance)
            .ok_or_else(|| crate::TimingModelError::InvalidMappedHierarchy {
                detail: format!("retained design instance '{instance_name}' has no bindings"),
            })?
        {
            let port = netlist.design_connection_port(connection).ok_or_else(|| {
                crate::TimingModelError::InvalidMappedHierarchy {
                    detail: format!(
                        "retained design instance '{instance_name}' has an unnamed port"
                    ),
                }
            })?;
            let direction = design_ports
                .get(module)
                .and_then(|ports| ports.get(port))
                .copied()
                .ok_or_else(|| crate::TimingModelError::InvalidMappedHierarchy {
                    detail: format!(
                        "retained design port '{module}.{port}' has no direction contract"
                    ),
                })?;
            let signals = netlist
                .design_connection_signals(connection)
                .ok_or_else(|| crate::TimingModelError::InvalidMappedHierarchy {
                    detail: format!("retained design port '{instance_name}.{port}' has no signals"),
                })?;
            for (bit, &signal) in signals.iter().enumerate() {
                let ConnectionSignal::Net(net) = signal else {
                    continue;
                };
                next_uid = next_uid
                    .checked_add(1)
                    .ok_or(crate::TimingModelError::Capacity {
                        resource: "opaque design timing-port ID",
                    })?;
                let uid = opto_core::ObjectUid::from_raw(next_uid).ok_or(
                    crate::TimingModelError::Capacity {
                        resource: "opaque design timing-port ID",
                    },
                )?;
                let name = if signals.len() == 1 {
                    format!("{instance_name}/{port}")
                } else {
                    format!("{instance_name}/{port}[{bit}]")
                };
                ports.push(TimingPort {
                    id: PortId::from_uid(uid),
                    name,
                    net: TimingNet::mapped(mapped_net_name(netlist, net), net),
                    direction: match direction {
                        opto_ir::word::PortDirection::Input => TimingPortDirection::Output,
                        opto_ir::word::PortDirection::Output => TimingPortDirection::Input,
                        opto_ir::word::PortDirection::Inout => TimingPortDirection::Inout,
                        opto_ir::word::PortDirection::Ref => {
                            return Err(crate::TimingModelError::InvalidMappedHierarchy {
                                detail: format!(
                                    "retained design port '{module}.{port}' is a reference port"
                                ),
                            }
                            .into());
                        }
                    },
                });
            }
        }
    }
    Ok(ports)
}

fn timing_instance_from_mapped(
    netlist: &opto_ir::mapped::MappedNetlist,
    id: opto_ir::mapped::CellId,
) -> Result<TimingInstance, crate::TimingError> {
    let instance = TimingInstanceId::from_raw(id.index().try_into().map_err(|_| {
        crate::TimingModelError::Capacity {
            resource: "instance ID",
        }
    })?);
    let index = id.index();
    let instance_name = netlist
        .cell_name(id)
        .ok_or(crate::TimingModelError::MappedCellMissingName { index })?;
    let mut connections = Vec::new();
    for connection in netlist
        .connections(id)
        .ok_or(crate::TimingModelError::MappedCellInvalidPinRange { index })?
    {
        let pin = netlist
            .pin_name(connection)
            .ok_or(crate::TimingModelError::MappedCellUnnamedPin { index })?;
        let net = match connection.signal {
            ConnectionSignal::Net(net) => mapped_net_name(netlist, net),
            ConnectionSignal::Constant(value) => super::constant_net_name(value).to_string(),
        };
        connections.push(TimingConnection {
            pin: pin.to_string(),
            net,
        });
    }
    Ok(TimingInstance {
        id: instance,
        name: instance_name.to_string(),
        cell: netlist
            .cell_type(id)
            .ok_or(crate::TimingModelError::MappedCellMissingType { index })?
            .to_string(),
        connections,
    })
}

fn mapped_net_name(netlist: &opto_ir::mapped::MappedNetlist, net: MappedNetId) -> String {
    mapped_net_name_cow(netlist, net).into_owned()
}

fn mapped_net_name_cow(netlist: &opto_ir::mapped::MappedNetlist, net: MappedNetId) -> Cow<'_, str> {
    netlist
        .net_name(net)
        .map_or_else(|| Cow::Owned(format!("$net{}", net.index())), Cow::Borrowed)
}

/// A live mapped net without any connected pin and outside every port has no
/// timing graph node; binding it would fabricate a net, so such nets stay
/// unbound and region edits touching only them are timing no-ops.
fn mapped_net_has_timing_presence(
    netlist: &opto_ir::mapped::MappedNetlist,
    port_nets: &[MappedNetId],
    net: MappedNetId,
) -> bool {
    port_nets.binary_search(&net).is_ok()
        || netlist
            .pins_on_net(net)
            .is_some_and(|mut pins| pins.next().is_some())
}

impl TimingRegionDelta {
    fn bind_mapped_net(
        &mut self,
        net: MappedNetId,
        name: Option<String>,
    ) -> Result<(), crate::TimingError> {
        if self.mapped_net_bindings.insert(net, name).is_some() {
            return Err(crate::TimingModelError::DuplicateMappedNetUpdate { net }.into());
        }
        Ok(())
    }

    /// Converts an applied mapped edit into the exact timing-domain delta.
    ///
    /// The conversion refreshes directly changed cells and expands through
    /// incident cells only for renamed nets, whose name-based timing bindings
    /// change despite stable mapped connectivity.
    ///
    /// # Errors
    ///
    /// Returns an error for missing mapped adjacency/ownership, duplicate delta
    /// entries, or invalid mapped cell records.
    pub fn from_mapped_region(
        netlist: &opto_ir::mapped::MappedNetlist,
        edit: &opto_ir::mapped::AppliedRegionDelta,
        model: &TimingModel,
    ) -> Result<Self, crate::TimingError> {
        if edit.generation_id() != netlist.generation_id()
            || model.mapped_generation != Some(netlist.generation_id())
        {
            return Err(crate::TimingModelError::ForeignMappedRegionEdit.into());
        }
        let affected_nets = edit
            .affected_nets()
            .collect::<std::collections::BTreeSet<_>>();
        let mut affected_cells = edit
            .affected_cells()
            .collect::<std::collections::BTreeSet<_>>();
        let mut delta = Self {
            mapped_generation: Some(netlist.generation_id()),
            ..Self::new()
        };
        for net in edit.renamed_nets() {
            let pins = netlist
                .pins_on_net(net)
                .ok_or(crate::TimingModelError::MissingNetAdjacency { net })?;
            for pin in pins {
                let cell = netlist
                    .pin_owner(pin)
                    .ok_or(crate::TimingModelError::OwnerlessPin { net, pin })?;
                affected_cells.insert(cell);
            }
        }
        for net in affected_nets {
            if !netlist.is_live_net(net) {
                delta.bind_mapped_net(net, None)?;
                continue;
            }
            if !mapped_net_has_timing_presence(netlist, &model.mapped_port_nets, net) {
                delta.bind_mapped_net(net, None)?;
                continue;
            }
            delta.bind_mapped_net(net, Some(mapped_net_name(netlist, net)))?;
        }
        for cell in affected_cells {
            let id = TimingInstanceId::from_raw(u32::try_from(cell.index()).map_err(|_| {
                crate::TimingModelError::Capacity {
                    resource: "mapped cell ID",
                }
            })?);
            if netlist.is_live_cell(cell) {
                delta.set_instance(timing_instance_from_mapped(netlist, cell)?)?;
            } else {
                delta.remove_instance(id)?;
            }
        }
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_library::{TimingArc, TimingCell, test_cells};
    use opto_ir::mapped::{
        ConnectionRef, ConnectionSignal, MappedBuilder, PortDirection, RegionDelta,
    };

    fn timing_library(cells: &[&str]) -> TimingLibrary {
        TimingLibrary {
            cells: test_cells(
                cells
                    .iter()
                    .map(|name| TimingCell {
                        name: (*name).to_string(),
                        arcs: vec![TimingArc::scalar("A", "Y", 0.1)],
                        clock_to_q: Vec::new(),
                        constraints: Vec::new(),
                        pin_capacitance: BTreeMap::new(),
                    })
                    .collect(),
            ),
            ..TimingLibrary::default()
        }
    }

    #[test]
    fn preserves_constant_pin_connections() {
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let data = builder.add_net(Some("d")).unwrap();
        let output = builder.add_net(Some("q")).unwrap();
        builder
            .add_cell(
                "gate_latch",
                "LATCH_H",
                None,
                &[
                    ("D".to_string(), None, ConnectionSignal::Net(data)),
                    ("E".to_string(), None, ConnectionSignal::Constant(true)),
                    ("Q".to_string(), None, ConnectionSignal::Net(output)),
                ],
            )
            .unwrap();
        let mapped = builder.freeze().unwrap();

        let design = TimingDesign::from_mapped(
            &mapped,
            crate::test_design_id(),
            &crate::PortBindings::new([]),
        )
        .unwrap();
        let enable = design.instances[0]
            .connections
            .iter()
            .find(|connection| connection.pin == "E")
            .unwrap();

        assert_eq!(super::super::constant_net_value(&enable.net), Some(true));
    }

    #[test]
    fn preserves_shared_mapped_net_identity_across_port_aliases() {
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let shared = builder.add_net(Some("shared")).unwrap();
        builder
            .add_port("a", PortDirection::Input, &[shared])
            .unwrap();
        builder
            .add_port("y", PortDirection::Output, &[shared])
            .unwrap();
        let mapped = builder.freeze().unwrap();
        let port_bindings =
            crate::PortBindings::new([crate::test_port_id("a"), crate::test_port_id("y")]);

        let design =
            TimingDesign::from_mapped(&mapped, crate::test_design_id(), &port_bindings).unwrap();

        assert_eq!(design.ports[0].name, "a");
        assert_eq!(design.ports[1].name, "y");
        assert_eq!(design.ports[0].net.name(), "shared");
        assert_eq!(design.ports[1].net.name(), "shared");
        assert_eq!(design.ports[0].net.mapped_id(), Some(shared));
        assert_eq!(design.ports[1].net.mapped_id(), Some(shared));

        let model = TimingModel::from_mapped(
            &mapped,
            crate::test_design_id(),
            &port_bindings,
            timing_library(&["BUF"]),
        )
        .unwrap();
        assert_eq!(
            model.graph.port_nets(crate::test_port_id("a")),
            model.graph.port_nets(crate::test_port_id("y"))
        );
        assert_eq!(
            model.mapped_net(
                crate::TimingNetId::from_index(model.graph.net_id("shared").unwrap()).unwrap()
            ),
            Some(shared)
        );
    }

    #[test]
    fn opaque_design_outputs_are_timing_cut_startpoints() {
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let output = builder.add_net(Some("q")).unwrap();
        builder
            .add_port("q", PortDirection::Output, &[output])
            .unwrap();
        builder
            .add_design_instance(
                "memory",
                "SRAM",
                &[("Q".to_string(), vec![ConnectionSignal::Net(output)])],
            )
            .unwrap();
        let mapped = builder.freeze().unwrap();
        let bindings = crate::PortBindings::new([crate::test_port_id("q")]);
        let design_ports = MappedDesignPortDirections::from([(
            "SRAM".to_string(),
            BTreeMap::from([("Q".to_string(), opto_ir::word::PortDirection::Output)]),
        )]);

        TimingModel::from_mapped_with_parasitics(
            &mapped,
            crate::test_design_id(),
            &bindings,
            timing_library(&["BUF"]),
            Parasitics::default(),
            &design_ports,
        )
        .unwrap();
    }

    #[test]
    fn mapped_cells_link_to_unique_timing_library_ids_by_name() {
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let a = builder.add_net(Some("a")).unwrap();
        let y = builder.add_net(Some("y")).unwrap();
        builder
            .add_cell(
                "U0",
                "BUF",
                Some(0),
                &[
                    ("A".to_string(), Some(0), ConnectionSignal::Net(a)),
                    ("Y".to_string(), Some(1), ConnectionSignal::Net(y)),
                ],
            )
            .unwrap();
        let mapped = builder.freeze().unwrap();

        let model = TimingModel::from_mapped(
            &mapped,
            crate::test_design_id(),
            &crate::PortBindings::new([]),
            timing_library(&["OTHER", "BUF"]),
        )
        .unwrap();

        assert_eq!(
            model
                .instance_library_cell_id(TimingInstanceId::from_raw(0))
                .map(crate::LibraryCellId::raw),
            Some(1)
        );
    }

    #[test]
    fn missing_and_ambiguous_library_cells_are_explicit_errors() {
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let a = builder.add_net(Some("a")).unwrap();
        let y = builder.add_net(Some("y")).unwrap();
        builder
            .add_cell(
                "U0",
                "BUF",
                None,
                &[
                    ("A".to_string(), None, ConnectionSignal::Net(a)),
                    ("Y".to_string(), None, ConnectionSignal::Net(y)),
                ],
            )
            .unwrap();
        let mapped = builder.freeze().unwrap();

        let missing = TimingModel::from_mapped(
            &mapped,
            crate::test_design_id(),
            &crate::PortBindings::new([]),
            TimingLibrary::default(),
        )
        .unwrap_err();
        assert!(matches!(
            missing,
            crate::TimingError::Model(crate::TimingModelError::UnknownCell { .. })
        ));

        let ambiguous = TimingModel::from_mapped(
            &mapped,
            crate::test_design_id(),
            &crate::PortBindings::new([]),
            timing_library(&["BUF", "BUF"]),
        )
        .unwrap_err();
        assert!(matches!(
            ambiguous,
            crate::TimingError::Model(crate::TimingModelError::AmbiguousCell { .. })
        ));
    }

    #[test]
    fn pin_reconnect_does_not_refresh_unchanged_high_fanout_cells() {
        const BRANCHES: usize = 64;
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let shared = builder.add_net(Some("shared")).unwrap();
        let replacement = builder.add_net(Some("replacement")).unwrap();
        let mut cells = Vec::new();
        for index in 0..BRANCHES {
            let input = builder.add_net(Some(&format!("b{index}"))).unwrap();
            let output = builder.add_net(Some(&format!("y{index}"))).unwrap();
            cells.push(
                builder
                    .add_cell(
                        &format!("U{index}"),
                        "AND2",
                        None,
                        &[
                            ("A".to_string(), None, ConnectionSignal::Net(shared)),
                            ("B".to_string(), None, ConnectionSignal::Net(input)),
                            ("Y".to_string(), None, ConnectionSignal::Net(output)),
                        ],
                    )
                    .unwrap(),
            );
        }
        let mut mapped = builder.freeze().unwrap();
        let library = TimingLibrary {
            cells: test_cells(vec![TimingCell {
                name: "AND2".to_string(),
                arcs: vec![
                    TimingArc::scalar("A", "Y", 0.1),
                    TimingArc::scalar("B", "Y", 0.1),
                ],
                ..TimingCell::default()
            }]),
            ..TimingLibrary::default()
        };
        let model = TimingModel::from_mapped(
            &mapped,
            crate::test_design_id(),
            &crate::PortBindings::new([]),
            library,
        )
        .unwrap();
        let pin = mapped.pin_ids(cells[0]).unwrap().next().unwrap();
        let snapshot = mapped
            .snapshot_region([cells[0]], [shared, replacement])
            .unwrap();
        let mut edit = RegionDelta::new(snapshot);
        edit.reconnect_pin(pin, ConnectionRef::Net(replacement))
            .unwrap();
        let applied = mapped.apply_region_delta(edit).unwrap();

        let timing = TimingRegionDelta::from_mapped_region(&mapped, &applied, &model).unwrap();
        assert_eq!(timing.updates.len(), 1);
        assert!(timing.updates.contains_key(&TimingInstanceId::from_raw(0)));
    }
}
