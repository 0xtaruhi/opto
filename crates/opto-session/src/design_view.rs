// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Borrowed object views over source RTL and the canonical mapped artifact.
//!
//! A mapped design is never expanded into [`opto_db::DesignIndex`]. The
//! published [`MappedNetlist`] remains the sole owner of mapped names,
//! instances, connections, and signals; [`MappedObjectIndex`] retains only
//! compact slot orderings used for exact lookup and canonical registry replay.

use crate::{SessionError, state::DesignRecord};
use opto_db::{DesignIndex, Direction, NameId};
use opto_ir::mapped::{
    CellId, ConnectionSignal, DesignInstanceConnection, DesignInstanceId, MappedGenerationId,
    MappedNetlist, NetId, PinConnection, PortDirection, PortId,
};
use opto_runtime::ExecutionContext;
use std::cmp::Ordering;

/// Compact derived lookup state for one immutable published mapped netlist.
///
/// Each entry is only a mapped slot ID. User-visible strings and structural
/// rows stay canonical in `MappedNetlist`.
#[derive(Debug)]
pub(crate) struct MappedObjectIndex {
    generation: MappedGenerationId,
    ports_by_name: Box<[u32]>,
    cells_by_name: Box<[u32]>,
    nets_by_name: Box<[u32]>,
}

impl MappedObjectIndex {
    pub(crate) fn new(
        mapped: &MappedNetlist,
        runtime: &ExecutionContext,
    ) -> Result<Self, SessionError> {
        let mut ports = Vec::new();
        ports
            .try_reserve_exact(mapped.ports().len())
            .map_err(|_| SessionError::capacity("mapped object port index allocation failed"))?;
        for index in 0..mapped.ports().len() {
            ports.push(compact_index(index, "mapped object port index")?);
        }
        runtime.sort_unstable_by(&mut ports, |left, right| {
            mapped_port_name(mapped, *left)
                .cmp(mapped_port_name(mapped, *right))
                .then_with(|| left.cmp(right))
        });

        let cell_count = mapped
            .cell_count()
            .checked_add(mapped.design_instance_count())
            .ok_or_else(|| SessionError::capacity("mapped object cell index capacity"))?;
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(cell_count)
            .map_err(|_| SessionError::capacity("mapped object cell index allocation failed"))?;
        for row in 0..cell_count {
            cells.push(compact_index(row, "mapped object cell index")?);
        }
        runtime.sort_unstable_by(&mut cells, |left, right| {
            mapped_cell_name(mapped, *left)
                .cmp(mapped_cell_name(mapped, *right))
                .then_with(|| left.cmp(right))
        });

        let mut nets = Vec::new();
        nets.try_reserve_exact(mapped.net_count())
            .map_err(|_| SessionError::capacity("mapped object net index allocation failed"))?;
        for net in mapped.net_ids() {
            nets.push(compact_index(net.index(), "mapped object net index")?);
        }
        runtime.sort_unstable_by(&mut nets, |left, right| {
            mapped_net_object_name(mapped, *left)
                .cmp(&mapped_net_object_name(mapped, *right))
                .then_with(|| left.cmp(right))
        });

        Ok(Self {
            generation: mapped.generation_id(),
            ports_by_name: ports.into_boxed_slice(),
            cells_by_name: cells.into_boxed_slice(),
            nets_by_name: nets.into_boxed_slice(),
        })
    }

    fn port(&self, mapped: &MappedNetlist, name: &str) -> Option<u32> {
        let index = self
            .ports_by_name
            .partition_point(|row| mapped_port_name(mapped, *row) < name);
        self.ports_by_name
            .get(index)
            .copied()
            .filter(|row| mapped_port_name(mapped, *row) == name)
    }

    fn cell(&self, mapped: &MappedNetlist, name: &str) -> Option<u32> {
        let index = self
            .cells_by_name
            .partition_point(|row| mapped_cell_name(mapped, *row) < name);
        self.cells_by_name
            .get(index)
            .copied()
            .filter(|row| mapped_cell_name(mapped, *row) == name)
    }

    fn net(&self, mapped: &MappedNetlist, name: &str) -> Option<u32> {
        let index = self
            .nets_by_name
            .partition_point(|row| mapped_net_object_name(mapped, *row).cmp_str(name).is_lt());
        self.nets_by_name
            .get(index)
            .copied()
            .filter(|row| mapped_net_object_name(mapped, *row).eq_str(name))
    }

    pub(crate) fn ports_by_name(&self) -> &[u32] {
        &self.ports_by_name
    }

    pub(crate) fn cells_by_name(&self) -> &[u32] {
        &self.cells_by_name
    }

    pub(crate) fn nets_by_name(&self) -> &[u32] {
        &self.nets_by_name
    }
}

fn compact_index(index: usize, resource: &'static str) -> Result<u32, SessionError> {
    u32::try_from(index).map_err(|_| SessionError::capacity(resource))
}

fn mapped_port_name(mapped: &MappedNetlist, row: u32) -> &str {
    mapped
        .port_name(PortId::from_index(row as usize).expect("indexed mapped port ID fits"))
        .expect("indexed mapped port has a valid name")
}

fn mapped_net_object_name(mapped: &MappedNetlist, row: u32) -> NetName<'_> {
    let net = NetId::from_index(row as usize).expect("indexed mapped net ID fits");
    mapped
        .net_name(net)
        .map_or(NetName::Anonymous(row), NetName::Borrowed)
}

fn mapped_cell_name(mapped: &MappedNetlist, row: u32) -> &str {
    let row = row as usize;
    if row < mapped.cell_count() {
        mapped.cell_name(CellId::from_index(row).expect("indexed mapped cell ID fits"))
    } else {
        mapped.design_instance_name(
            DesignInstanceId::from_index(row - mapped.cell_count())
                .expect("indexed mapped design-instance ID fits"),
        )
    }
    .expect("indexed mapped instance has a valid name")
}

#[derive(Debug, Clone, Copy)]
enum DesignBackend<'a> {
    Source(&'a DesignIndex),
    Mapped {
        mapped: &'a MappedNetlist,
        index: &'a MappedObjectIndex,
    },
}

/// Allocation-free common object view over a source or mapped design.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DesignView<'a> {
    backend: DesignBackend<'a>,
}

impl<'a> DesignView<'a> {
    pub(crate) fn source(design: &'a DesignIndex) -> Self {
        Self {
            backend: DesignBackend::Source(design),
        }
    }

    pub(crate) fn mapped(mapped: &'a MappedNetlist, index: &'a MappedObjectIndex) -> Self {
        assert_eq!(
            mapped.generation_id(),
            index.generation,
            "mapped object sidecar belongs to another artifact generation"
        );
        Self {
            backend: DesignBackend::Mapped { mapped, index },
        }
    }

    pub(crate) fn from_record(record: &'a DesignRecord) -> Self {
        match &record.mapped_object_index {
            Some(index) => Self::mapped(
                record
                    .synthesized
                    .as_ref()
                    .expect("mapped object selection owns a synthesis artifact")
                    .mapped(),
                index,
            ),
            None => Self::source(&record.object_index),
        }
    }

    pub(crate) fn name(self) -> &'a str {
        match self.backend {
            DesignBackend::Source(design) => &design.name,
            DesignBackend::Mapped { mapped, .. } => mapped.name(),
        }
    }

    pub(crate) fn port_count(self) -> usize {
        match self.backend {
            DesignBackend::Source(design) => design.ports.len(),
            DesignBackend::Mapped { mapped, .. } => mapped.ports().len(),
        }
    }

    pub(crate) fn ports(self) -> impl ExactSizeIterator<Item = PortView<'a>> {
        (0..self.port_count()).map(move |row| {
            self.port(row)
                .expect("a row below the design port count exists")
        })
    }

    pub(crate) fn port(self, row: usize) -> Option<PortView<'a>> {
        match self.backend {
            DesignBackend::Source(design) => {
                let port = design.ports.get(row)?;
                Some(PortView {
                    name: design.name_str(port.name),
                    direction: port.direction,
                    width: port.width,
                })
            }
            DesignBackend::Mapped { mapped, .. } => {
                let id = PortId::from_index(row).ok()?;
                let port = mapped.ports().get(row)?;
                Some(PortView {
                    name: mapped.port_name(id)?,
                    direction: match port.direction {
                        PortDirection::Input => Direction::Input,
                        PortDirection::Output => Direction::Output,
                        PortDirection::Inout => Direction::Inout,
                    },
                    width: mapped.port_nets(id)?.len().try_into().ok()?,
                })
            }
        }
    }

    pub(crate) fn port_by_name(self, name: &str) -> Option<PortView<'a>> {
        match self.backend {
            DesignBackend::Source(design) => {
                let port = design.port_by_name(name)?;
                Some(PortView {
                    name: design.name_str(port.name),
                    direction: port.direction,
                    width: port.width,
                })
            }
            DesignBackend::Mapped { mapped, index } => {
                self.port(index.port(mapped, name)? as usize)
            }
        }
    }

    pub(crate) fn cell_count(self) -> usize {
        match self.backend {
            DesignBackend::Source(design) => design.cells.len(),
            DesignBackend::Mapped { mapped, .. } => mapped
                .cell_count()
                .checked_add(mapped.design_instance_count())
                .expect("mapped object index preflighted the combined cell count"),
        }
    }

    pub(crate) fn cells(self) -> impl ExactSizeIterator<Item = CellView<'a>> {
        (0..self.cell_count()).map(move |row| {
            self.cell(row)
                .expect("a row below the design cell count exists")
        })
    }

    pub(crate) fn cell(self, row: usize) -> Option<CellView<'a>> {
        match self.backend {
            DesignBackend::Source(design) => {
                let cell = design.cells.get(row)?;
                Some(CellView {
                    name: design.name_str(cell.name),
                    reference: design.name_str(cell.reference),
                    connections: CellConnections::Source {
                        design,
                        rows: &cell.connections,
                    },
                })
            }
            DesignBackend::Mapped { mapped, .. } if row < mapped.cell_count() => {
                mapped_target_cell(mapped, row)
            }
            DesignBackend::Mapped { mapped, .. } => {
                mapped_design_cell(mapped, row.checked_sub(mapped.cell_count())?)
            }
        }
    }

    pub(crate) fn cell_by_name(self, name: &str) -> Option<CellView<'a>> {
        match self.backend {
            DesignBackend::Source(design) => {
                let cell = design.cell_by_name(name)?;
                Some(CellView {
                    name: design.name_str(cell.name),
                    reference: design.name_str(cell.reference),
                    connections: CellConnections::Source {
                        design,
                        rows: &cell.connections,
                    },
                })
            }
            DesignBackend::Mapped { mapped, index } => {
                self.cell(index.cell(mapped, name)? as usize)
            }
        }
    }

    pub(crate) fn net_count(self) -> usize {
        match self.backend {
            DesignBackend::Source(design) => design.nets.len(),
            DesignBackend::Mapped { mapped, .. } => mapped.net_count(),
        }
    }

    pub(crate) fn nets(self) -> impl ExactSizeIterator<Item = NetView<'a>> {
        (0..self.net_count()).map(move |row| {
            self.net(row)
                .expect("a row below the design net count exists")
        })
    }

    pub(crate) fn net(self, row: usize) -> Option<NetView<'a>> {
        match self.backend {
            DesignBackend::Source(design) => {
                let net = design.nets.get(row)?;
                Some(NetView {
                    name: NetName::Borrowed(design.name_str(net.name)),
                    width: net.width,
                })
            }
            DesignBackend::Mapped { mapped, .. } => {
                let net = NetId::from_index(row).ok()?;
                let anonymous = u32::try_from(row).ok()?;
                Some(NetView {
                    name: mapped
                        .net_name(net)
                        .map_or(NetName::Anonymous(anonymous), NetName::Borrowed),
                    width: 1,
                })
            }
        }
    }

    pub(crate) fn net_by_name(self, name: &str) -> Option<NetView<'a>> {
        match self.backend {
            DesignBackend::Source(design) => {
                let net = design.net_by_name(name)?;
                Some(NetView {
                    name: NetName::Borrowed(design.name_str(net.name)),
                    width: net.width,
                })
            }
            DesignBackend::Mapped { mapped, index } => self.net(index.net(mapped, name)? as usize),
        }
    }

    pub(crate) fn used_signal_names(self) -> UsedSignalIter<'a> {
        match self.backend {
            DesignBackend::Source(design) => UsedSignalIter {
                design: Some(design),
                rows: design.used_signals.iter(),
            },
            DesignBackend::Mapped { .. } => UsedSignalIter {
                design: None,
                rows: [].iter(),
            },
        }
    }

    pub(crate) fn is_visible_net_name(self, name: &str) -> bool {
        match self.backend {
            DesignBackend::Source(design) => design.is_visible_net_name(name),
            DesignBackend::Mapped { .. } => self.net_by_name(name).is_some(),
        }
    }

    pub(crate) fn mapped_parts(self) -> Option<(&'a MappedNetlist, &'a MappedObjectIndex)> {
        match self.backend {
            DesignBackend::Mapped { mapped, index } => Some((mapped, index)),
            DesignBackend::Source(_) => None,
        }
    }

    pub(crate) fn source_index(self) -> Option<&'a DesignIndex> {
        match self.backend {
            DesignBackend::Source(design) => Some(design),
            DesignBackend::Mapped { .. } => None,
        }
    }
}

fn mapped_target_cell(mapped: &MappedNetlist, row: usize) -> Option<CellView<'_>> {
    let id = CellId::from_index(row).ok()?;
    Some(CellView {
        name: mapped.cell_name(id)?,
        reference: mapped.cell_type(id)?,
        connections: CellConnections::MappedTarget {
            mapped,
            rows: mapped.connections(id)?,
        },
    })
}

fn mapped_design_cell(mapped: &MappedNetlist, row: usize) -> Option<CellView<'_>> {
    let id = DesignInstanceId::from_index(row).ok()?;
    Some(CellView {
        name: mapped.design_instance_name(id)?,
        reference: mapped.design_instance_module(id)?,
        connections: CellConnections::MappedDesign {
            mapped,
            rows: mapped.design_instance_connections(id)?,
        },
    })
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PortView<'a> {
    pub(crate) name: &'a str,
    pub(crate) direction: Direction,
    pub(crate) width: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NetView<'a> {
    pub(crate) name: NetName<'a>,
    pub(crate) width: u32,
}

/// Borrowed or deterministically derived mapped net object name.
///
/// The anonymous form is intentionally just a compact slot ID. Callers that
/// need an owned Tcl object name materialize it at their output boundary;
/// replay code reuses one scratch string across every row.
#[derive(Debug, Clone, Copy)]
pub(crate) enum NetName<'a> {
    Borrowed(&'a str),
    Anonymous(u32),
}

impl NetName<'_> {
    pub(crate) fn cmp_str(self, other: &str) -> Ordering {
        match self {
            Self::Borrowed(name) => name.cmp(other),
            Self::Anonymous(row) => {
                let mut bytes = [0u8; 12];
                anonymous_net_bytes(row, &mut bytes).cmp(other.as_bytes())
            }
        }
    }

    pub(crate) fn eq_str(self, other: &str) -> bool {
        self.cmp_str(other) == Ordering::Equal
    }

    pub(crate) fn into_string(self) -> String {
        match self {
            Self::Borrowed(name) => name.to_string(),
            Self::Anonymous(row) => {
                let mut name = String::with_capacity(12);
                push_anonymous_net_name(&mut name, row);
                name
            }
        }
    }

    pub(crate) fn with_str<R>(self, scratch: &mut String, use_name: impl FnOnce(&str) -> R) -> R {
        match self {
            Self::Borrowed(name) => use_name(name),
            Self::Anonymous(row) => {
                scratch.clear();
                push_anonymous_net_name(scratch, row);
                use_name(scratch)
            }
        }
    }
}

impl PartialEq for NetName<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NetName<'_> {}

impl PartialOrd for NetName<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NetName<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Self::Borrowed(left), Self::Borrowed(right)) => left.cmp(right),
            (Self::Anonymous(left), Self::Anonymous(right)) => {
                let mut left_bytes = [0u8; 12];
                let mut right_bytes = [0u8; 12];
                anonymous_net_bytes(left, &mut left_bytes)
                    .cmp(anonymous_net_bytes(right, &mut right_bytes))
            }
            (Self::Anonymous(left), Self::Borrowed(right)) => {
                let mut bytes = [0u8; 12];
                anonymous_net_bytes(left, &mut bytes).cmp(right.as_bytes())
            }
            (Self::Borrowed(left), Self::Anonymous(right)) => {
                let mut bytes = [0u8; 12];
                left.as_bytes().cmp(anonymous_net_bytes(right, &mut bytes))
            }
        }
    }
}

fn anonymous_net_bytes(row: u32, bytes: &mut [u8; 12]) -> &[u8] {
    bytes[0] = b'_';
    bytes[1] = b'n';
    let mut divisor = 1u32;
    while divisor <= row / 10 {
        divisor *= 10;
    }
    let mut remaining = row;
    let mut length = 2usize;
    loop {
        bytes[length] =
            b'0' + u8::try_from(remaining / divisor).expect("one decimal digit always fits in u8");
        length += 1;
        remaining %= divisor;
        if divisor == 1 {
            break;
        }
        divisor /= 10;
    }
    &bytes[..length]
}

fn push_anonymous_net_name(name: &mut String, row: u32) {
    let mut bytes = [0u8; 12];
    let bytes = anonymous_net_bytes(row, &mut bytes);
    name.push_str(std::str::from_utf8(bytes).expect("anonymous net name is ASCII"));
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CellView<'a> {
    pub(crate) name: &'a str,
    pub(crate) reference: &'a str,
    connections: CellConnections<'a>,
}

impl<'a> CellView<'a> {
    pub(crate) fn connections(self) -> ConnectionIter<'a> {
        ConnectionIter {
            rows: self.connections,
            index: 0,
        }
    }

    pub(crate) fn connection_by_name(self, name: &str) -> Option<ConnectionView<'a>> {
        self.connections()
            .find(|connection| connection.port == name)
    }
}

#[derive(Debug, Clone, Copy)]
enum CellConnections<'a> {
    Source {
        design: &'a DesignIndex,
        rows: &'a [opto_db::CellConnection],
    },
    MappedTarget {
        mapped: &'a MappedNetlist,
        rows: &'a [PinConnection],
    },
    MappedDesign {
        mapped: &'a MappedNetlist,
        rows: &'a [DesignInstanceConnection],
    },
}

pub(crate) struct ConnectionIter<'a> {
    rows: CellConnections<'a>,
    index: usize,
}

impl<'a> Iterator for ConnectionIter<'a> {
    type Item = ConnectionView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.index;
        self.index += 1;
        match self.rows {
            CellConnections::Source { design, rows } => {
                let connection = rows.get(row)?;
                Some(ConnectionView {
                    port: design.name_str(connection.port),
                    signals: ConnectionSignals::Source {
                        design,
                        rows: &connection.signals,
                    },
                })
            }
            CellConnections::MappedTarget { mapped, rows } => {
                let connection = rows.get(row)?;
                Some(ConnectionView {
                    port: mapped.pin_name(connection)?,
                    signals: ConnectionSignals::MappedScalar {
                        mapped,
                        signal: connection.signal,
                    },
                })
            }
            CellConnections::MappedDesign { mapped, rows } => {
                let connection = rows.get(row)?;
                Some(ConnectionView {
                    port: mapped.design_connection_port(connection)?,
                    signals: ConnectionSignals::MappedVector {
                        mapped,
                        rows: mapped.design_connection_signals(connection)?,
                    },
                })
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = match self.rows {
            CellConnections::Source { rows, .. } => rows.len(),
            CellConnections::MappedTarget { rows, .. } => rows.len(),
            CellConnections::MappedDesign { rows, .. } => rows.len(),
        };
        let remaining = length.saturating_sub(self.index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ConnectionIter<'_> {}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnectionView<'a> {
    pub(crate) port: &'a str,
    signals: ConnectionSignals<'a>,
}

impl<'a> ConnectionView<'a> {
    pub(crate) fn signal_names(self) -> SignalNameIter<'a> {
        SignalNameIter {
            signals: self.signals,
            index: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ConnectionSignals<'a> {
    Source {
        design: &'a DesignIndex,
        rows: &'a [NameId],
    },
    MappedScalar {
        mapped: &'a MappedNetlist,
        signal: ConnectionSignal,
    },
    MappedVector {
        mapped: &'a MappedNetlist,
        rows: &'a [ConnectionSignal],
    },
}

pub(crate) struct SignalNameIter<'a> {
    signals: ConnectionSignals<'a>,
    index: usize,
}

impl<'a> Iterator for SignalNameIter<'a> {
    type Item = NetName<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.signals {
            ConnectionSignals::Source { design, rows } => {
                let name = *rows.get(self.index)?;
                self.index += 1;
                Some(NetName::Borrowed(design.name_str(name)))
            }
            ConnectionSignals::MappedScalar { mapped, signal } => {
                if self.index != 0 {
                    return None;
                }
                self.index = 1;
                match signal {
                    ConnectionSignal::Net(net) => Some(mapped.net_name(net).map_or(
                        NetName::Anonymous(
                            u32::try_from(net.index()).expect("mapped net IDs are compact"),
                        ),
                        NetName::Borrowed,
                    )),
                    ConnectionSignal::Constant(_) => None,
                }
            }
            ConnectionSignals::MappedVector { mapped, rows } => {
                while let Some(signal) = rows.get(self.index).copied() {
                    self.index += 1;
                    if let ConnectionSignal::Net(net) = signal {
                        return Some(mapped.net_name(net).map_or(
                            NetName::Anonymous(
                                u32::try_from(net.index()).expect("mapped net IDs are compact"),
                            ),
                            NetName::Borrowed,
                        ));
                    }
                }
                None
            }
        }
    }
}

pub(crate) struct UsedSignalIter<'a> {
    design: Option<&'a DesignIndex>,
    rows: std::slice::Iter<'a, NameId>,
}

impl<'a> Iterator for UsedSignalIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let name = *self.rows.next()?;
        Some(
            self.design
                .expect("only source designs have used-signal rows")
                .name_str(name),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rows.size_hint()
    }
}

impl ExactSizeIterator for UsedSignalIter<'_> {}
