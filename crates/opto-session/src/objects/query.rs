// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::Session;
use opto_db::{
    CellObject, ClockObject, Collection, DesignObject, NetObject, ObjectClass, ObjectLocator,
    PinObject, PortObject, matches_pattern,
};
use opto_library::TargetSequentialKind;
use std::collections::{BTreeMap, BTreeSet};

impl Session {
    /// Visits durable object names without creating process-local collection handles.
    pub fn visit_object_names(&self, class: ObjectClass, mut visit: impl FnMut(&str)) {
        if class == ObjectClass::Design {
            for name in self.state.designs.keys() {
                visit(name);
            }
            return;
        }
        if class == ObjectClass::Clock {
            for clock in self.state.timing.clocks() {
                visit(&clock.name);
            }
            return;
        }
        let Some(design) = self
            .state
            .current_design
            .as_deref()
            .and_then(|name| self.state.designs.get(name))
            .map(crate::DesignView::from_record)
        else {
            return;
        };
        match class {
            ObjectClass::Port => {
                for port in design.ports() {
                    visit(port.name);
                }
            }
            ObjectClass::Cell => {
                for cell in design.cells() {
                    visit(cell.name);
                }
            }
            ObjectClass::Pin => {
                let mut full_name = String::new();
                for cell in design.cells() {
                    for connection in cell.connections() {
                        full_name.clear();
                        full_name.reserve(
                            cell.name
                                .len()
                                .saturating_add(connection.port.len())
                                .saturating_add(1),
                        );
                        full_name.push_str(cell.name);
                        full_name.push('/');
                        full_name.push_str(connection.port);
                        visit(&full_name);
                    }
                }
            }
            ObjectClass::Net => {
                let mut scratch = String::new();
                for net in design.nets() {
                    net.name.with_str(&mut scratch, &mut visit);
                }
            }
            ObjectClass::Design | ObjectClass::Clock => unreachable!(),
        }
    }

    /// Return designs whose names match a shell-style pattern.
    pub fn get_designs(
        &mut self,
        pattern: &str,
    ) -> Result<Collection<DesignObject>, crate::SessionError> {
        let objects = self
            .state
            .designs
            .keys()
            .filter(|name| matches_pattern(name, pattern))
            .cloned()
            .map(|name| ObjectLocator::Design { name })
            .collect();
        self.intern_collection(objects)
    }

    /// Return current-design ports whose names match a shell-style pattern.
    pub fn get_ports(
        &mut self,
        pattern: &str,
    ) -> Result<Collection<PortObject>, crate::SessionError> {
        let objects = {
            let design = self.current()?;
            design
                .ports()
                .filter(|port| matches_pattern(port.name, pattern))
                .map(|port| ObjectLocator::Port {
                    design: design.name().to_string(),
                    name: port.name.to_string(),
                })
                .collect()
        };
        self.intern_collection(objects)
    }

    /// Return current-design input and inout ports, optionally excluding clock sources.
    pub fn all_inputs(
        &mut self,
        no_clocks: bool,
    ) -> Result<Collection<PortObject>, crate::SessionError> {
        let clock_ports = if no_clocks {
            self.state
                .timing
                .clocks()
                .iter()
                .flat_map(|clock| clock.sources.iter().copied())
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        let objects = {
            let design = self.current()?;
            design
                .ports()
                .filter(|port| {
                    matches!(
                        port.direction,
                        opto_db::Direction::Input | opto_db::Direction::Inout
                    )
                })
                .filter_map(|port| {
                    let locator = ObjectLocator::Port {
                        design: design.name().to_string(),
                        name: port.name.to_string(),
                    };
                    (!no_clocks
                        || self
                            .state
                            .objects
                            .get(&locator)
                            .and_then(opto_db::AnyObjectId::downcast::<PortObject>)
                            .is_none_or(|id| !clock_ports.contains(&id)))
                    .then_some(locator)
                })
                .collect()
        };
        self.intern_collection(objects)
    }

    /// Return current-design output and inout ports.
    pub fn all_outputs(&mut self) -> Result<Collection<PortObject>, crate::SessionError> {
        let objects = {
            let design = self.current()?;
            design
                .ports()
                .filter(|port| {
                    matches!(
                        port.direction,
                        opto_db::Direction::Output | opto_db::Direction::Inout
                    )
                })
                .map(|port| ObjectLocator::Port {
                    design: design.name().to_string(),
                    name: port.name.to_string(),
                })
                .collect()
        };
        self.intern_collection(objects)
    }

    /// Return current-design instances backed by sequential target cells.
    pub fn all_registers(
        &mut self,
        edge_triggered: bool,
        level_sensitive: bool,
    ) -> Result<Collection<CellObject>, crate::SessionError> {
        let selection = self.resolution_library_selection();
        let register_kinds = if selection.is_empty() {
            BTreeMap::new()
        } else {
            self.process
                .libraries
                .current()
                .target_cells(&selection)?
                .iter()
                .filter_map(|cell| {
                    let mut flip_flop = false;
                    let mut latch = false;
                    for sequential in cell.sequential() {
                        match sequential.kind() {
                            TargetSequentialKind::FlipFlop => flip_flop = true,
                            TargetSequentialKind::Latch => latch = true,
                        }
                    }
                    (flip_flop || latch).then_some((cell.name().to_string(), (flip_flop, latch)))
                })
                .collect::<BTreeMap<_, _>>()
        };
        let objects = {
            let design = self.current()?;
            design
                .cells()
                .filter(|cell| {
                    register_kinds
                        .get(cell.reference)
                        .is_some_and(|(flip_flop, latch)| {
                            (edge_triggered && *flip_flop) || (level_sensitive && *latch)
                        })
                })
                .map(|cell| ObjectLocator::Cell {
                    design: design.name().to_string(),
                    name: cell.name.to_string(),
                })
                .collect()
        };
        self.intern_collection(objects)
    }

    /// Traverse objects in `handle` and return their matching related ports.
    pub fn get_ports_of_objects(
        &mut self,
        handle: &str,
        pattern: &str,
    ) -> Result<Collection<PortObject>, crate::SessionError> {
        let mut ports = Vec::new();
        for object in self.collection_objects(handle)? {
            self.push_ports_for_object(&object, pattern, &mut ports)?;
        }
        self.intern_collection(ports)
    }

    /// Return current-design cells whose names match a shell-style pattern.
    pub fn get_cells(
        &mut self,
        pattern: &str,
    ) -> Result<Collection<CellObject>, crate::SessionError> {
        let objects = {
            let design = self.current()?;
            design
                .cells()
                .filter(|cell| matches_pattern(cell.name, pattern))
                .map(|cell| ObjectLocator::Cell {
                    design: design.name().to_string(),
                    name: cell.name.to_string(),
                })
                .collect()
        };
        self.intern_collection(objects)
    }

    /// Traverse objects in `handle` and return their matching related cells.
    pub fn get_cells_of_objects(
        &mut self,
        handle: &str,
        pattern: &str,
    ) -> Result<Collection<CellObject>, crate::SessionError> {
        let mut cells = Vec::new();
        for object in self.collection_objects(handle)? {
            self.push_cells_for_object(&object, pattern, &mut cells)?;
        }
        self.intern_collection(cells)
    }

    /// Return current-design pins whose full names match a shell-style pattern.
    pub fn get_pins(
        &mut self,
        pattern: &str,
    ) -> Result<Collection<PinObject>, crate::SessionError> {
        let objects = {
            let design = self.current()?;
            self.pin_objects_for_design(design)
                .into_iter()
                .filter(|pin| matches_pattern(pin.object_name(), pattern))
                .collect()
        };
        self.intern_collection(objects)
    }

    /// Traverse objects in `handle` and return their matching related pins.
    pub fn get_pins_of_objects(
        &mut self,
        handle: &str,
        pattern: &str,
    ) -> Result<Collection<PinObject>, crate::SessionError> {
        let mut pins = Vec::new();
        for object in self.collection_objects(handle)? {
            self.push_pins_for_object(&object, pattern, &mut pins)?;
        }
        self.intern_collection(pins)
    }

    /// Return current-design nets whose names match a shell-style pattern.
    pub fn get_nets(
        &mut self,
        pattern: &str,
    ) -> Result<Collection<NetObject>, crate::SessionError> {
        let objects = {
            let design = self.current()?;
            Self::net_objects_for_design(design, pattern)
        };
        self.intern_collection(objects)
    }

    /// Traverse objects in `handle` and return their matching related nets.
    pub fn get_nets_of_objects(
        &mut self,
        handle: &str,
        pattern: &str,
    ) -> Result<Collection<NetObject>, crate::SessionError> {
        let mut nets = Vec::new();
        for object in self.collection_objects(handle)? {
            self.push_nets_for_object(&object, pattern, &mut nets)?;
        }
        self.intern_collection(nets)
    }

    /// Return clocks whose names match a shell-style pattern.
    pub fn get_clocks(
        &mut self,
        pattern: &str,
    ) -> Result<Collection<ClockObject>, crate::SessionError> {
        let objects = self
            .state
            .timing
            .clocks()
            .iter()
            .filter(|clock| matches_pattern(&clock.name, pattern))
            .map(|clock| ObjectLocator::Clock {
                name: clock.name.clone(),
            })
            .collect();
        self.intern_collection(objects)
    }
}
