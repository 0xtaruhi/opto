// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(test)]
use crate::DesignRecord;
use crate::{
    Session, SessionError,
    design_view::{CellView, ConnectionView, DesignView},
};
#[cfg(test)]
use opto_db::DesignIndex;
#[cfg(test)]
use opto_db::RevisionId;
use opto_db::{ObjectLocator, matches_pattern};
#[cfg(test)]
use opto_ir::rtl::RtlModule;
use std::collections::BTreeSet;

impl Session {
    #[cfg(test)]
    pub(crate) fn install_design_fresh(
        &mut self,
        source: RtlModule,
        source_revision: RevisionId,
        design: DesignIndex,
    ) -> Result<(), crate::SessionError> {
        let prepared_detach = self
            .state
            .designs
            .get(&design.name)
            .map(DesignRecord::prepare_synthesis_detach)
            .transpose()?;
        crate::transaction::reconcile_source_objects(self, std::slice::from_ref(&design))?;
        let previous_incremental = if let Some(prepared) = prepared_detach {
            let record = self
                .state
                .designs
                .get_mut(&design.name)
                .expect("prepared design still exists during commit");
            record.commit_synthesis_detach(prepared);
            record.incremental_snapshot.take()
        } else {
            None
        };
        let name = design.name.clone();
        let mut record = DesignRecord::new(source, source_revision, design);
        if let Some(snapshot) = previous_incremental {
            record.incremental_snapshot = Some(snapshot);
        }
        self.state.designs.insert(name, record);
        self.clear_stale_analysis_generation();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn update_design_preserving_objects(
        &mut self,
        design: DesignIndex,
    ) -> Result<(), crate::SessionError> {
        crate::transaction::reconcile_source_changes(
            self,
            &[],
            std::slice::from_ref(&design),
            &[],
        )?;
        let name = design.name.clone();
        let record = self
            .state
            .designs
            .get_mut(&name)
            .expect("updated design must exist in the design store");
        record.object_index = design;
        record.mapped_object_index = None;
        Ok(())
    }

    pub(in crate::objects) fn pin_objects_for_design(
        &self,
        design: DesignView<'_>,
    ) -> Vec<ObjectLocator> {
        let mut pins = Vec::new();
        for cell in design.cells() {
            self.push_pins_for_cell(design, cell, "*", &mut pins);
        }
        pins
    }

    pub(in crate::objects) fn push_pins_for_cell(
        &self,
        design: DesignView<'_>,
        cell: CellView<'_>,
        pattern: &str,
        pins: &mut Vec<ObjectLocator>,
    ) {
        for pin_name in self.cell_pin_names(cell) {
            let full_name = format!("{}/{pin_name}", cell.name);
            if matches_pattern(&full_name, pattern) {
                push_object(
                    pins,
                    ObjectLocator::Pin {
                        design: design.name().to_string(),
                        cell: cell.name.to_string(),
                        name: pin_name,
                        full_name,
                    },
                );
            }
        }
    }

    pub(in crate::objects) fn cell_pin_names(&self, cell: CellView<'_>) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(reference) = self
            .state
            .designs
            .get(cell.reference)
            .map(DesignView::from_record)
        {
            for port in reference.ports() {
                push_string(&mut names, port.name.to_string());
            }
        }
        for connection in cell.connections() {
            push_string(&mut names, connection.port.to_string());
        }
        deduplicate_preserving_order(names)
    }

    pub(in crate::objects) fn net_objects_for_design(
        design: DesignView<'_>,
        pattern: &str,
    ) -> Vec<ObjectLocator> {
        let mut objects = Vec::new();
        let used_names = Self::used_signal_names(design);

        for net in design.nets() {
            let name = net.name.into_string();
            Self::push_net_name(design.name(), &name, pattern, &mut objects);
        }
        for port in design.ports() {
            let port_name = port.name;
            if used_names.iter().any(|name| name == port_name) {
                Self::push_net_name(design.name(), port_name, pattern, &mut objects);
            }
        }
        for name in used_names {
            Self::push_net_name(design.name(), &name, pattern, &mut objects);
        }

        objects
    }

    pub(in crate::objects) fn used_signal_names(design: DesignView<'_>) -> Vec<String> {
        design
            .used_signal_names()
            .map(ToString::to_string)
            .collect()
    }

    pub(in crate::objects) fn push_ports_for_object(
        &self,
        object: &ObjectLocator,
        pattern: &str,
        ports: &mut Vec<ObjectLocator>,
    ) -> Result<(), crate::SessionError> {
        match object {
            ObjectLocator::Port { design, name } => {
                Self::push_port_name(design, name, pattern, ports);
            }
            ObjectLocator::Net { design, name } => {
                let design = self.design_by_name(design)?;
                Self::push_visible_port_name(design, name, pattern, ports);
            }
            ObjectLocator::Pin {
                design, cell, name, ..
            } => {
                let design = self.design_by_name(design)?;
                let cell = find_cell(design, cell)?;
                if let Some(connection) = cell.connection_by_name(name) {
                    for name in connection.signal_names() {
                        let name = name.into_string();
                        Self::push_visible_port_name(design, &name, pattern, ports);
                    }
                }
            }
            ObjectLocator::Design { .. }
            | ObjectLocator::Cell { .. }
            | ObjectLocator::Clock { .. } => {}
        }
        Ok(())
    }

    pub(in crate::objects) fn push_cells_for_object(
        &self,
        object: &ObjectLocator,
        pattern: &str,
        cells: &mut Vec<ObjectLocator>,
    ) -> Result<(), crate::SessionError> {
        match object {
            ObjectLocator::Cell { design, name } => {
                Self::push_cell_name(design, name, pattern, cells);
            }
            ObjectLocator::Pin { design, cell, .. } => {
                Self::push_cell_name(design, cell, pattern, cells);
            }
            ObjectLocator::Port { design, name } | ObjectLocator::Net { design, name } => {
                let design = self.design_by_name(design)?;
                for cell in design.cells() {
                    if cell.connections().any(|connection| {
                        connection.signal_names().any(|signal| signal.eq_str(name))
                    }) {
                        Self::push_cell_name(design.name(), cell.name, pattern, cells);
                    }
                }
            }
            ObjectLocator::Design { .. } | ObjectLocator::Clock { .. } => {}
        }
        Ok(())
    }

    pub(in crate::objects) fn push_pins_for_object(
        &self,
        object: &ObjectLocator,
        pattern: &str,
        pins: &mut Vec<ObjectLocator>,
    ) -> Result<(), crate::SessionError> {
        match object {
            ObjectLocator::Design { name } => {
                let design = self.design_by_name(name)?;
                for pin in self.pin_objects_for_design(design) {
                    if matches_pattern(pin.object_name(), pattern) {
                        push_object(pins, pin);
                    }
                }
            }
            ObjectLocator::Cell { design, name } => {
                let design = self.design_by_name(design)?;
                let cell = find_cell(design, name)?;
                self.push_pins_for_cell(design, cell, pattern, pins);
            }
            ObjectLocator::Pin { full_name, .. } => {
                if matches_pattern(full_name, pattern) {
                    push_object(pins, object.clone());
                }
            }
            ObjectLocator::Port { design, name } | ObjectLocator::Net { design, name } => {
                let design = self.design_by_name(design)?;
                for cell in design.cells() {
                    for connection in cell.connections() {
                        if connection.signal_names().any(|signal| signal.eq_str(name)) {
                            let full_name = format!("{}/{}", cell.name, connection.port);
                            if matches_pattern(&full_name, pattern) {
                                push_object(
                                    pins,
                                    ObjectLocator::Pin {
                                        design: design.name().to_string(),
                                        cell: cell.name.to_string(),
                                        name: connection.port.to_string(),
                                        full_name,
                                    },
                                );
                            }
                        }
                    }
                }
            }
            ObjectLocator::Clock { .. } => {}
        }
        Ok(())
    }

    pub(in crate::objects) fn push_nets_for_object(
        &self,
        object: &ObjectLocator,
        pattern: &str,
        nets: &mut Vec<ObjectLocator>,
    ) -> Result<(), crate::SessionError> {
        match object {
            ObjectLocator::Design { name } => {
                let design = self.design_by_name(name)?;
                for net in Self::net_objects_for_design(design, pattern) {
                    push_object(nets, net);
                }
            }
            ObjectLocator::Cell { design, name } => {
                let design = self.design_by_name(design)?;
                let cell = find_cell(design, name)?;
                for connection in cell.connections() {
                    Self::push_connection_net_names(design, connection, pattern, nets);
                }
            }
            ObjectLocator::Pin {
                design, cell, name, ..
            } => {
                let design = self.design_by_name(design)?;
                let cell = find_cell(design, cell)?;
                if let Some(connection) = cell.connection_by_name(name) {
                    Self::push_connection_net_names(design, connection, pattern, nets);
                }
            }
            ObjectLocator::Port { design, name } | ObjectLocator::Net { design, name } => {
                let design = self.design_by_name(design)?;
                if Self::is_visible_net_name(design, name) {
                    Self::push_net_name(design.name(), name, pattern, nets);
                }
            }
            ObjectLocator::Clock { .. } => {}
        }
        Ok(())
    }

    pub(in crate::objects) fn push_visible_port_name(
        design: DesignView<'_>,
        port_name: &str,
        pattern: &str,
        ports: &mut Vec<ObjectLocator>,
    ) {
        if design.port_by_name(port_name).is_some() {
            Self::push_port_name(design.name(), port_name, pattern, ports);
        }
    }

    pub(in crate::objects) fn push_port_name(
        design_name: &str,
        port_name: &str,
        pattern: &str,
        ports: &mut Vec<ObjectLocator>,
    ) {
        if matches_pattern(port_name, pattern) {
            push_object(
                ports,
                ObjectLocator::Port {
                    design: design_name.to_string(),
                    name: port_name.to_string(),
                },
            );
        }
    }

    pub(in crate::objects) fn push_cell_name(
        design_name: &str,
        cell_name: &str,
        pattern: &str,
        cells: &mut Vec<ObjectLocator>,
    ) {
        if matches_pattern(cell_name, pattern) {
            push_object(
                cells,
                ObjectLocator::Cell {
                    design: design_name.to_string(),
                    name: cell_name.to_string(),
                },
            );
        }
    }

    pub(in crate::objects) fn push_connection_net_names(
        design: DesignView<'_>,
        connection: ConnectionView<'_>,
        pattern: &str,
        nets: &mut Vec<ObjectLocator>,
    ) {
        for name in connection.signal_names() {
            let name = name.into_string();
            Self::push_net_name(design.name(), &name, pattern, nets);
        }
    }

    pub(in crate::objects) fn push_net_name(
        design_name: &str,
        net_name: &str,
        pattern: &str,
        nets: &mut Vec<ObjectLocator>,
    ) {
        if matches_pattern(net_name, pattern) {
            push_object(
                nets,
                ObjectLocator::Net {
                    design: design_name.to_string(),
                    name: net_name.to_string(),
                },
            );
        }
    }

    pub(in crate::objects) fn is_visible_net_name(design: DesignView<'_>, net_name: &str) -> bool {
        design.is_visible_net_name(net_name)
    }

    pub(in crate::objects) fn net_width(design: DesignView<'_>, net_name: &str) -> Option<u32> {
        design
            .net_by_name(net_name)
            .map(|net| net.width)
            .or_else(|| {
                if Self::is_visible_net_name(design, net_name) {
                    design
                        .port_by_name(net_name)
                        .map(|port| port.width)
                        .or(Some(1))
                } else {
                    None
                }
            })
    }
}

fn find_cell<'a>(design: DesignView<'a>, name: &str) -> Result<CellView<'a>, SessionError> {
    design.cell_by_name(name).ok_or_else(|| {
        SessionError::state(format!(
            "cell '{name}' is missing from design '{}'",
            design.name()
        ))
    })
}

fn push_object(objects: &mut Vec<ObjectLocator>, object: ObjectLocator) {
    objects.push(object);
}

fn push_string(values: &mut Vec<String>, value: String) {
    values.push(value);
}

fn deduplicate_preserving_order<T: Clone + Ord>(values: Vec<T>) -> Vec<T> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}
