// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{SynthesisOptions, SynthesisReport};
use std::collections::BTreeMap;

pub(super) fn synthesis_report(
    netlist: &opto_ir::mapped::MappedNetlist,
    options: &SynthesisOptions,
) -> SynthesisReport {
    use opto_ir::mapped::PortId;

    let cell_area = options
        .target_cells
        .iter()
        .filter_map(|cell| cell.area().map(|area| (cell.name(), area)))
        .collect::<BTreeMap<_, _>>();
    let total_cell_area = netlist
        .cell_ids()
        .filter_map(|cell| netlist.cell_type(cell))
        .filter_map(|cell_type| cell_area.get(cell_type))
        .sum();
    SynthesisReport {
        design: netlist.name().to_string(),
        ports: netlist
            .ports()
            .iter()
            .enumerate()
            .filter_map(|(index, _)| PortId::from_index(index).ok())
            .filter_map(|port| netlist.port_nets(port))
            .map(<[opto_ir::mapped::NetId]>::len)
            .sum(),
        cells: netlist.cell_count() + netlist.design_instance_count(),
        nets: netlist.net_count(),
        total_cell_area,
    }
}
