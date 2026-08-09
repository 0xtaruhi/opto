// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Replayable, allocation-bounded design-object inventory.

use crate::{DesignStore, DesignView, SessionError};
use opto_db::{
    DesignIndex, NameId, ObjectReconcileDesign, ObjectReconcileMode, ObjectReconcileSource,
    ResolvedObject,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DesignObjectScope {
    DesignAndPorts,
    Complete,
}

#[derive(Debug)]
enum DesignObjectInventory<'a> {
    Source {
        definitions: &'a DesignStore,
        design: &'a DesignIndex,
        scope: DesignObjectScope,
        ports: Box<[NameId]>,
        cells: Box<[u32]>,
        nets: Box<[NameId]>,
    },
    Mapped {
        definitions: &'a DesignStore,
        design: DesignView<'a>,
        scope: DesignObjectScope,
    },
}

impl<'a> DesignObjectInventory<'a> {
    fn new(
        definitions: &'a DesignStore,
        design: DesignView<'a>,
        scope: DesignObjectScope,
    ) -> Result<Self, SessionError> {
        let Some(source) = design.source_index() else {
            return Ok(Self::Mapped {
                definitions,
                design,
                scope,
            });
        };
        let design = source;
        let mut ports = design
            .ports
            .iter()
            .map(|port| port.name)
            .collect::<Vec<_>>();
        sort_names(design, &mut ports);

        let (cells, nets) = if scope == DesignObjectScope::Complete {
            let mut cells = (0..design.cells.len())
                .map(|index| {
                    u32::try_from(index).map_err(|_| {
                        SessionError::capacity(
                            "design object inventory exceeds 32-bit cell-index capacity",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            cells.sort_unstable_by(|left, right| {
                let left = design.name_str(design.cells[*left as usize].name);
                let right = design.name_str(design.cells[*right as usize].name);
                left.cmp(right).then_with(|| left.len().cmp(&right.len()))
            });

            let mut nets = design
                .nets
                .iter()
                .map(|net| net.name)
                .chain(design.used_signals.iter().copied())
                .collect::<Vec<_>>();
            sort_names(design, &mut nets);
            (cells.into_boxed_slice(), nets.into_boxed_slice())
        } else {
            (Box::default(), Box::default())
        };

        Ok(Self::Source {
            definitions,
            design,
            scope,
            ports: ports.into_boxed_slice(),
            cells,
            nets,
        })
    }

    fn visit_design(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>)) {
        let name = match self {
            Self::Source { design, .. } => design.name.as_str(),
            Self::Mapped { design, .. } => design.name(),
        };
        visitor(ResolvedObject::Design { name });
    }

    fn visit_ports(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>)) {
        match self {
            Self::Source { design, ports, .. } => {
                for &name in ports {
                    visitor(ResolvedObject::Port {
                        design: &design.name,
                        name: design.name_str(name),
                    });
                }
            }
            Self::Mapped { design, .. } => {
                let (_, index) = design
                    .mapped_parts()
                    .expect("mapped inventory owns a mapped view");
                let mut previous = None;
                for &row in index.ports_by_name() {
                    let port = design
                        .port(row as usize)
                        .expect("mapped sidecar port row is valid");
                    if previous == Some(port.name) {
                        continue;
                    }
                    visitor(ResolvedObject::Port {
                        design: design.name(),
                        name: port.name,
                    });
                    previous = Some(port.name);
                }
            }
        }
    }

    fn visit_cells(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>)) {
        match self {
            Self::Source {
                design,
                scope,
                cells,
                ..
            } => {
                if *scope != DesignObjectScope::Complete {
                    return;
                }
                let mut previous = None;
                for &index in cells {
                    let name = design.name_str(design.cells[index as usize].name);
                    if previous == Some(name) {
                        continue;
                    }
                    visitor(ResolvedObject::Cell {
                        design: &design.name,
                        name,
                    });
                    previous = Some(name);
                }
            }
            Self::Mapped { design, scope, .. } => {
                if *scope != DesignObjectScope::Complete {
                    return;
                }
                let (_, index) = design
                    .mapped_parts()
                    .expect("mapped inventory owns a mapped view");
                let mut previous = None;
                for &row in index.cells_by_name() {
                    let cell = design
                        .cell(row as usize)
                        .expect("mapped sidecar cell row is valid");
                    if previous == Some(cell.name) {
                        continue;
                    }
                    visitor(ResolvedObject::Cell {
                        design: design.name(),
                        name: cell.name,
                    });
                    previous = Some(cell.name);
                }
            }
        }
    }

    fn visit_pins(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>)) {
        match self {
            Self::Source {
                definitions,
                design,
                scope,
                cells,
                ..
            } => visit_source_pins(definitions, design, *scope, cells, visitor),
            Self::Mapped {
                definitions,
                design,
                scope,
            } => {
                if *scope != DesignObjectScope::Complete {
                    return;
                }
                let (_, index) = design
                    .mapped_parts()
                    .expect("mapped inventory owns a mapped view");
                let mut pins = Vec::new();
                let mut full_name = String::new();
                for &row in index.cells_by_name() {
                    let cell = design
                        .cell(row as usize)
                        .expect("mapped sidecar cell row is valid");
                    pins.clear();
                    if let Some(reference) = definitions.get(cell.reference) {
                        pins.extend(
                            DesignView::from_record(reference)
                                .ports()
                                .map(|port| port.name),
                        );
                    }
                    pins.extend(cell.connections().map(|connection| connection.port));
                    pins.sort_unstable();
                    pins.dedup();
                    for &name in &pins {
                        write_pin_name(&mut full_name, cell.name, name);
                        visitor(ResolvedObject::Pin {
                            design: design.name(),
                            cell: cell.name,
                            name,
                            full_name: &full_name,
                        });
                    }
                }
            }
        }
    }

    fn visit_nets(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>)) {
        match self {
            Self::Source {
                design,
                scope,
                nets,
                ..
            } => {
                if *scope != DesignObjectScope::Complete {
                    return;
                }
                for &name in nets {
                    visitor(ResolvedObject::Net {
                        design: &design.name,
                        name: design.name_str(name),
                    });
                }
            }
            Self::Mapped { design, scope, .. } => {
                if *scope != DesignObjectScope::Complete {
                    return;
                }
                let (_, index) = design
                    .mapped_parts()
                    .expect("mapped inventory owns a mapped view");
                let mut scratch = String::new();
                let mut previous = None;
                for &row in index.nets_by_name() {
                    let net = design
                        .net(row as usize)
                        .expect("mapped sidecar net row is valid");
                    if previous == Some(net.name) {
                        continue;
                    }
                    net.name.with_str(&mut scratch, |name| {
                        visitor(ResolvedObject::Net {
                            design: design.name(),
                            name,
                        });
                    });
                    previous = Some(net.name);
                }
            }
        }
    }
}

fn visit_source_pins(
    definitions: &DesignStore,
    design: &DesignIndex,
    scope: DesignObjectScope,
    cells: &[u32],
    visitor: &mut dyn FnMut(ResolvedObject<'_>),
) {
    if scope != DesignObjectScope::Complete {
        return;
    }
    let mut pins = Vec::new();
    let mut full_name = String::new();
    let mut start = 0usize;
    while start < cells.len() {
        let cell = &design.cells[cells[start] as usize];
        let cell_name = design.name_str(cell.name);
        let mut end = start + 1;
        while end < cells.len() && design.name_eq(design.cells[cells[end] as usize].name, cell_name)
        {
            end += 1;
        }

        pins.clear();
        for &index in &cells[start..end] {
            let cell = &design.cells[index as usize];
            if let Some(reference) = definitions.get(design.name_str(cell.reference)) {
                pins.extend(
                    DesignView::from_record(reference)
                        .ports()
                        .map(|port| port.name),
                );
            }
            pins.extend(
                cell.connections
                    .iter()
                    .map(|connection| design.name_str(connection.port)),
            );
        }
        pins.sort_unstable();
        pins.dedup();
        for &name in &pins {
            write_pin_name(&mut full_name, cell_name, name);
            visitor(ResolvedObject::Pin {
                design: &design.name,
                cell: cell_name,
                name,
                full_name: &full_name,
            });
        }
        start = end;
    }
}

fn write_pin_name(full_name: &mut String, cell: &str, pin: &str) {
    full_name.clear();
    full_name.reserve(cell.len().saturating_add(pin.len()).saturating_add(1));
    full_name.push_str(cell);
    full_name.push('/');
    full_name.push_str(pin);
}

#[derive(Debug)]
struct LifecycleDesign<'a> {
    name: &'a str,
    mode: ObjectReconcileMode,
    inventory: Option<DesignObjectInventory<'a>>,
}

/// Canonically ordered batch used for all three registry passes: retention
/// discovery, fallible preflight, and deterministic commit.
#[derive(Debug, Default)]
pub(crate) struct DesignObjectBatch<'a> {
    designs: Vec<LifecycleDesign<'a>>,
}

impl<'a> DesignObjectBatch<'a> {
    #[cfg(test)]
    pub(crate) fn push_remove(&mut self, name: &'a str) {
        self.designs.push(LifecycleDesign {
            name,
            mode: ObjectReconcileMode::Replace,
            inventory: None,
        });
    }

    pub(crate) fn push_design(
        &mut self,
        definitions: &'a DesignStore,
        design: DesignView<'a>,
        mode: ObjectReconcileMode,
        scope: DesignObjectScope,
    ) -> Result<(), SessionError> {
        self.designs.push(LifecycleDesign {
            name: design.name(),
            mode,
            inventory: Some(DesignObjectInventory::new(definitions, design, scope)?),
        });
        Ok(())
    }

    pub(crate) fn seal(&mut self) -> Result<(), SessionError> {
        self.designs.sort_unstable_by_key(|design| design.name);
        if let Some(pair) = self
            .designs
            .windows(2)
            .find(|pair| pair[0].name == pair[1].name)
        {
            return Err(SessionError::state(format!(
                "object lifecycle transaction contains duplicate design '{}'",
                pair[0].name
            )));
        }
        Ok(())
    }
}

impl ObjectReconcileSource for DesignObjectBatch<'_> {
    fn design_count(&self) -> usize {
        self.designs.len()
    }

    fn design(&self, index: usize) -> ObjectReconcileDesign<'_> {
        let design = &self.designs[index];
        ObjectReconcileDesign {
            name: design.name,
            mode: design.mode,
        }
    }

    fn visit(&self, visitor: &mut dyn FnMut(ResolvedObject<'_>)) {
        for design in &self.designs {
            if let Some(inventory) = &design.inventory {
                inventory.visit_design(visitor);
            }
        }
        for design in &self.designs {
            if let Some(inventory) = &design.inventory {
                inventory.visit_ports(visitor);
            }
        }
        for design in &self.designs {
            if let Some(inventory) = &design.inventory {
                inventory.visit_cells(visitor);
            }
        }
        for design in &self.designs {
            if let Some(inventory) = &design.inventory {
                inventory.visit_pins(visitor);
            }
        }
        for design in &self.designs {
            if let Some(inventory) = &design.inventory {
                inventory.visit_nets(visitor);
            }
        }
    }
}

fn sort_names(design: &DesignIndex, names: &mut Vec<NameId>) {
    names.sort_unstable_by(|left, right| design.name_str(*left).cmp(design.name_str(*right)));
    names.dedup();
}
