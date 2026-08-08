// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SynthesisOptions;
use opto_ir::word;
use opto_library::{
    TargetCellRef, TargetClockGateKind, TargetClockGateRole, TargetPinDirection,
    normalized_cell_area,
};

#[derive(Debug)]
pub(crate) struct ClockGateCell {
    pub(crate) name: String,
    pub(crate) kind: TargetClockGateKind,
    pub(crate) clock_pin: String,
    pub(crate) enable_pin: String,
    pub(crate) output_pin: String,
    area: f64,
}

#[derive(Debug, Default)]
pub(crate) struct ClockGatingCatalog {
    gates: Vec<ClockGateCell>,
}

impl ClockGatingCatalog {
    pub(crate) fn new(options: &SynthesisOptions) -> Self {
        let mut gates = options
            .target_cells
            .iter()
            .filter(|cell| !cell.dont_use())
            .filter_map(clock_gate_cell)
            .collect::<Vec<_>>();
        gates.sort_by(|left, right| {
            left.area
                .total_cmp(&right.area)
                .then_with(|| left.name.cmp(&right.name))
        });
        Self { gates }
    }

    pub(crate) fn gates_any_edge(&self) -> bool {
        !self.gates.is_empty()
    }

    pub(crate) fn gate_for(&self, edge: word::Edge, latch_based: bool) -> Option<&ClockGateCell> {
        self.gates.iter().find(|gate| {
            gate.kind.gates_rising_edge() == matches!(edge, word::Edge::Pos)
                && gate.kind.is_latch_based() == latch_based
        })
    }
}

fn clock_gate_cell(cell: TargetCellRef<'_>) -> Option<ClockGateCell> {
    let kind = cell.clock_gate()?;
    let clock = cell.clock_gate_pin(TargetClockGateRole::Clock)?;
    let enable = cell.clock_gate_pin(TargetClockGateRole::Enable)?;
    let output = cell.clock_gate_pin(TargetClockGateRole::Output)?;
    if clock.direction() != TargetPinDirection::Input
        || enable.direction() != TargetPinDirection::Input
        || output.direction() != TargetPinDirection::Output
    {
        return None;
    }
    Some(ClockGateCell {
        name: cell.name().to_string(),
        kind,
        clock_pin: clock.name().to_string(),
        enable_pin: enable.name().to_string(),
        output_pin: output.name().to_string(),
        area: normalized_cell_area(cell.area()),
    })
}
