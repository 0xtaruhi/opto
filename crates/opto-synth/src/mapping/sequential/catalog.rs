// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(test)]
use crate::TargetCell;
use crate::mapping::{MappedCell, MappedInputConnection, MappedOutputConnection};
use crate::planning::mapping_policy::{CellCost, MappingCost, compare_mapping_cost};
use crate::{
    SynthesisOptions, TargetCellRef, TargetPinDirection, TargetPinRef, TargetSequentialKind,
    TargetSequentialRef,
};
use opto_ir::word;
use opto_library::{TargetTimingType, TimingCheckKind, TimingEdge};
use std::cmp::Ordering;
use std::collections::BTreeSet;

pub(crate) mod cells;

use cells::{
    AsyncControl, AsyncResetRequest, LatchCell, SequentialCell, SequentialEnableCell,
    SequentialInvertedOutput, SequentialTiming, asynchronous_controls, cell_cost, compare_cells,
    compare_latch_cells, enable_flip_flop_pattern, is_input_pin, latch_cell, next_state_input_pins,
    sequential_timing, state_output_pins,
};

#[derive(Debug, Default)]
pub(crate) struct SequentialTimingProjection {
    rows: Box<[(word::ValueId, SequentialTiming)]>,
}

impl SequentialTimingProjection {
    pub(crate) fn build(
        module: &word::WordModule,
        sequential: &SequentialCellCatalog,
        combinational: &crate::mapping::library::CombinationalCellCatalog,
    ) -> Result<Self, crate::SynthError> {
        let observability = crate::word::uses::netlist_observability(module)?;
        let mut rows = Vec::new();
        for operation in module.operations() {
            let word::OpKind::Register(register) = &operation.kind else {
                continue;
            };
            if !observability.observes_value(operation.result)? {
                continue;
            }
            let selected = sequential.select_register(module, register, combinational)?;
            rows.push((operation.result, selected.timing()));
        }
        rows.sort_unstable_by_key(|&(value, _)| value);
        Ok(Self {
            rows: rows.into_boxed_slice(),
        })
    }

    fn timing(&self, value: word::ValueId) -> Option<SequentialTiming> {
        self.rows
            .binary_search_by_key(&value, |&(value, _)| value)
            .ok()
            .map(|index| self.rows[index].1)
    }

    pub(crate) fn clock_to_q(&self, value: word::ValueId) -> Option<f64> {
        self.timing(value).and_then(|timing| timing.clock_to_q)
    }

    pub(crate) fn output_transition(&self, value: word::ValueId) -> Option<f64> {
        self.timing(value)
            .and_then(|timing| timing.output_transition)
    }

    pub(crate) fn setup(&self, value: word::ValueId) -> Option<f64> {
        self.timing(value).and_then(|timing| timing.setup)
    }
}

pub(crate) enum SelectedRegisterCell<'a> {
    Simple(&'a SequentialCell),
    Enabled(&'a SequentialEnableCell),
}

impl SelectedRegisterCell<'_> {
    fn timing(&self) -> SequentialTiming {
        match self {
            Self::Simple(cell) => cell.timing,
            Self::Enabled(cell) => cell.timing,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct SequentialCellCatalog {
    cells: Vec<SequentialCell>,
    enable_cells: Vec<SequentialEnableCell>,
    latch_cells: Vec<LatchCell>,
}

impl SequentialCellCatalog {
    pub(crate) fn new(options: &SynthesisOptions) -> Self {
        let mut catalog = Self::default();
        for (_, cell) in options.target_cells.synthesis_cells() {
            for sequential in cell.sequential() {
                if sequential.kind() == TargetSequentialKind::Latch {
                    if let Some(latch) = latch_cell(cell, sequential) {
                        catalog.latch_cells.push(latch);
                    }
                    continue;
                }
                if sequential.kind() != TargetSequentialKind::FlipFlop
                    || sequential.enable().is_some()
                {
                    continue;
                }
                let Some((clock_pin, clock_positive)) = sequential
                    .clocked_on()
                    .and_then(crate::BooleanFunctionRef::as_literal)
                else {
                    continue;
                };
                let Some(next_state) = sequential.next_state() else {
                    continue;
                };
                let Some((output_pin, inverted_output_pin)) = state_output_pins(cell, sequential)
                else {
                    continue;
                };
                if !is_input_pin(cell, clock_pin) {
                    continue;
                }
                let Some(resets) = asynchronous_controls(cell, sequential) else {
                    continue;
                };
                let reset_pins = resets
                    .iter()
                    .map(|control| control.pin.as_str())
                    .collect::<Vec<_>>();
                let edge = if clock_positive {
                    word::Edge::Pos
                } else {
                    word::Edge::Neg
                };
                let Some(data_pins) = next_state_input_pins(cell, sequential, next_state) else {
                    if let Some(pattern) = enable_flip_flop_pattern(cell, sequential, next_state) {
                        let pin_names = [pattern.data_pin, pattern.enable_pin];
                        let cost = cell_cost(cell, &pin_names, clock_pin, &reset_pins, output_pin);
                        let inverted_output =
                            inverted_output_pin.map(|pin| SequentialInvertedOutput {
                                pin: pin.name().to_string(),
                                cost: cell_cost(cell, &pin_names, clock_pin, &reset_pins, pin),
                            });
                        catalog.enable_cells.push(SequentialEnableCell {
                            cell_name: cell.name().to_string(),
                            data_pin: pattern.data_pin.to_string(),
                            enable_pin: pattern.enable_pin.to_string(),
                            enable_active_high: pattern.enable_active_high,
                            clock_pin: clock_pin.to_string(),
                            output_pin: output_pin.name().to_string(),
                            inverted_output,
                            resets,
                            edge,
                            cost,
                            timing: sequential_timing(
                                cell,
                                pattern.data_pin,
                                clock_pin,
                                output_pin,
                                edge,
                            ),
                        });
                    }
                    continue;
                };
                let data_pin_names = data_pins.iter().map(|pin| pin.name()).collect::<Vec<_>>();
                let cost = cell_cost(cell, &data_pin_names, clock_pin, &reset_pins, output_pin);
                let inverted_output = inverted_output_pin.map(|pin| SequentialInvertedOutput {
                    pin: pin.name().to_string(),
                    cost: cell_cost(cell, &data_pin_names, clock_pin, &reset_pins, pin),
                });
                let Some((data_pin, true)) = next_state.as_literal() else {
                    continue;
                };
                let candidate = SequentialCell {
                    cell_name: cell.name().to_string(),
                    data_pin: data_pin.to_string(),
                    clock_pin: clock_pin.to_string(),
                    output_pin: output_pin.name().to_string(),
                    inverted_output,
                    resets,
                    edge,
                    cost,
                    timing: sequential_timing(cell, data_pin, clock_pin, output_pin, edge),
                };
                catalog.cells.push(candidate);
            }
        }
        catalog.cells.sort_by(|left, right| {
            (left.edge as u8)
                .cmp(&(right.edge as u8))
                .then_with(|| left.resets.cmp(&right.resets))
                .then_with(|| compare_cells(left, right))
        });
        catalog.latch_cells.sort_by(|left, right| {
            left.resets
                .cmp(&right.resets)
                .then_with(|| compare_latch_cells(left, right))
        });
        catalog
    }

    pub(crate) fn best(
        &self,
        edge: word::Edge,
        resets: &[AsyncResetRequest],
        inverted_output: bool,
        inverter: Option<CellCost>,
    ) -> Option<&SequentialCell> {
        self.cells
            .iter()
            .filter(|cell| {
                cell.edge == edge
                    && cell
                        .resets
                        .iter()
                        .map(AsyncControl::request)
                        .eq(resets.iter().copied())
            })
            .filter(|cell| cell.mapping_cost(inverted_output, inverter).is_some())
            .min_by(|left, right| {
                let left_cost = left
                    .mapping_cost(inverted_output, inverter)
                    .expect("filtered sequential candidate has an implementation cost");
                let right_cost = right
                    .mapping_cost(inverted_output, inverter)
                    .expect("filtered sequential candidate has an implementation cost");
                compare_mapping_cost(left_cost, right_cost).then_with(|| compare_cells(left, right))
            })
    }

    pub(crate) fn has_enable_cell(&self, edge: word::Edge, resets: &[AsyncResetRequest]) -> bool {
        self.enable_cells.iter().any(|cell| {
            cell.edge == edge
                && cell
                    .resets
                    .iter()
                    .map(AsyncControl::request)
                    .eq(resets.iter().copied())
        })
    }

    pub(crate) fn select_register<'a>(
        &'a self,
        module: &word::WordModule,
        register: &word::RegisterOp,
        combinational: &crate::mapping::library::CombinationalCellCatalog,
    ) -> Result<SelectedRegisterCell<'a>, crate::SynthError> {
        let resets = super::async_reset_requests(module, &register.resets)?;
        if let Some(enable) = register.enable {
            return self
                .best_enable(
                    register.edge,
                    &resets,
                    enable.active_high,
                    false,
                    super::enable_inverter_cost(module, enable.value, combinational),
                )
                .map(SelectedRegisterCell::Enabled)
                .ok_or_else(|| {
                    crate::SynthError::mapping("target library has no compatible enabled DFF")
                });
        }
        self.best(register.edge, &resets, false, None)
            .map(SelectedRegisterCell::Simple)
            .ok_or_else(|| crate::SynthError::mapping("target library has no compatible DFF"))
    }

    pub(crate) fn best_enable(
        &self,
        edge: word::Edge,
        resets: &[AsyncResetRequest],
        enable_active_high: bool,
        inverted_output: bool,
        inverter: Option<CellCost>,
    ) -> Option<&SequentialEnableCell> {
        self.enable_cells
            .iter()
            .filter(|cell| {
                cell.edge == edge
                    && cell
                        .resets
                        .iter()
                        .map(AsyncControl::request)
                        .eq(resets.iter().copied())
            })
            .filter(|cell| {
                cell.mapping_cost(enable_active_high, inverted_output, inverter)
                    .is_some()
            })
            .min_by(|left, right| {
                let left_cost = left
                    .mapping_cost(enable_active_high, inverted_output, inverter)
                    .expect("filtered sequential candidate has an implementation cost");
                let right_cost = right
                    .mapping_cost(enable_active_high, inverted_output, inverter)
                    .expect("filtered sequential candidate has an implementation cost");
                compare_mapping_cost(left_cost, right_cost)
                    .then_with(|| left.cell_name.cmp(&right.cell_name))
            })
    }

    pub(crate) fn best_latch(
        &self,
        resets: &[AsyncResetRequest],
        enable_active_high: bool,
        inverted_output: bool,
        inverter: Option<CellCost>,
    ) -> Option<&LatchCell> {
        self.latch_cells
            .iter()
            .filter(|cell| {
                cell.resets
                    .iter()
                    .map(AsyncControl::request)
                    .eq(resets.iter().copied())
            })
            .filter(|cell| {
                cell.mapping_cost(enable_active_high, inverted_output, inverter)
                    .is_some()
            })
            .min_by(|left, right| {
                let left_cost = left
                    .mapping_cost(enable_active_high, inverted_output, inverter)
                    .expect("filtered latch candidate has an implementation cost");
                let right_cost = right
                    .mapping_cost(enable_active_high, inverted_output, inverter)
                    .expect("filtered latch candidate has an implementation cost");
                compare_mapping_cost(left_cost, right_cost)
                    .then_with(|| compare_latch_cells(left, right))
            })
    }
}

#[cfg(test)]
mod tests;
