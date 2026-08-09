// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::transaction::ResolvedOperation;
use super::{
    AppliedRegionDelta, BTreeSet, CellId, ConnectionSignal, MappedNetlist, NetId, PinId, PinLinks,
    RegionConflict, RegionSnapshot,
};

pub(super) fn validate_signal(
    netlist: &MappedNetlist,
    snapshot: &RegionSnapshot,
    signal: ConnectionSignal,
    new_nets: &BTreeSet<NetId>,
) -> Result<(), RegionConflict> {
    let ConnectionSignal::Net(net) = signal else {
        return Ok(());
    };
    if new_nets.contains(&net) {
        return Ok(());
    }
    if !netlist.is_live_net(net) {
        return Err(RegionConflict::invalid(format!(
            "region delta references removed net {net:?}"
        )));
    }
    if !snapshot.contains_net(net) {
        return Err(RegionConflict::invalid(format!(
            "region delta references net {net:?} outside its snapshot"
        )));
    }
    Ok(())
}

pub(super) fn operation_names(operations: &[ResolvedOperation]) -> impl Iterator<Item = &str> {
    operations.iter().flat_map(|operation| {
        let mut names = Vec::new();
        match operation {
            ResolvedOperation::AddNet { name, .. } | ResolvedOperation::RenameNet { name, .. } => {
                names.extend(name.as_deref());
            }
            ResolvedOperation::AddCell { spec, .. } => {
                names.push(spec.name.as_str());
                names.push(spec.cell_type.as_str());
                names.extend(spec.connections.iter().map(|(pin, _, _)| pin.as_str()));
            }
            ResolvedOperation::ReplaceCell { cell_type, .. } => names.push(cell_type),
            ResolvedOperation::RenameCell { name, .. } => names.push(name),
            ResolvedOperation::RemoveCell(_)
            | ResolvedOperation::RemoveNet(_)
            | ResolvedOperation::ReconnectPin { .. } => {}
        }
        names
    })
}

pub(super) fn save_cell(netlist: &MappedNetlist, applied: &mut AppliedRegionDelta, cell: CellId) {
    applied
        .old_cells
        .entry(cell)
        .or_insert(netlist.cells[cell.index()]);
}

pub(super) fn save_net(netlist: &MappedNetlist, applied: &mut AppliedRegionDelta, net: NetId) {
    if net.index() < applied.old_net_len {
        applied
            .old_nets
            .entry(net)
            .or_insert(netlist.nets[net.index()]);
    }
}

pub(super) fn save_net_pins(netlist: &MappedNetlist, applied: &mut AppliedRegionDelta, net: NetId) {
    if net.index() < applied.old_net_len {
        applied
            .old_net_pins
            .entry(net)
            .or_insert(netlist.net_pins[net.index()]);
    }
}

pub(super) fn save_pin_links(
    netlist: &MappedNetlist,
    applied: &mut AppliedRegionDelta,
    pin: PinId,
) {
    if pin.index() < applied.old_connection_len {
        applied
            .old_pin_links
            .entry(pin)
            .or_insert(netlist.pin_links[pin.index()]);
    }
}

pub(super) fn unlink_pin(
    netlist: &mut MappedNetlist,
    applied: &mut AppliedRegionDelta,
    pin: PinId,
) {
    let ConnectionSignal::Net(net) = netlist.connections[pin.index()].signal else {
        unreachable!("only net-connected pins enter adjacency unlinking");
    };
    let links = netlist.pin_links[pin.index()];
    save_net_pins(netlist, applied, net);
    save_pin_links(netlist, applied, pin);
    if let Some(previous) = links.previous {
        save_pin_links(netlist, applied, previous);
        netlist.pin_links[previous.index()].next = links.next;
    } else {
        netlist.net_pins[net.index()].head = links.next;
    }
    if let Some(next) = links.next {
        save_pin_links(netlist, applied, next);
        netlist.pin_links[next.index()].previous = links.previous;
    } else {
        netlist.net_pins[net.index()].tail = links.previous;
    }
    netlist.pin_links[pin.index()] = PinLinks::default();
}

pub(super) fn link_pin(
    netlist: &mut MappedNetlist,
    applied: &mut AppliedRegionDelta,
    net: NetId,
    pin: PinId,
) {
    let tail = netlist.net_pins[net.index()].tail;
    save_net_pins(netlist, applied, net);
    save_pin_links(netlist, applied, pin);
    if let Some(tail) = tail {
        save_pin_links(netlist, applied, tail);
        netlist.pin_links[tail.index()].next = Some(pin);
    } else {
        netlist.net_pins[net.index()].head = Some(pin);
    }
    netlist.pin_links[pin.index()] = PinLinks {
        previous: tail,
        next: None,
    };
    netlist.net_pins[net.index()].tail = Some(pin);
}

pub(super) fn touch_signal_net(
    netlist: &mut MappedNetlist,
    applied: &mut AppliedRegionDelta,
    signal: ConnectionSignal,
    revision: u64,
) {
    let ConnectionSignal::Net(net) = signal else {
        return;
    };
    save_net(netlist, applied, net);
    netlist.nets[net.index()].version = revision;
}
