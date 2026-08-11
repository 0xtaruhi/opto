// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    BTreeSet, CellCost, MappedCell, MappedInputConnection, MappedOutputConnection, MappingCost,
    Ordering, TargetCellRef, TargetPinDirection, TargetPinRef, TargetSequentialRef,
    TargetTimingType, TimingCheckKind, TimingEdge, word,
};
use crate::planning::mapping_policy::compare_cell_cost;
use smallvec::{SmallVec, smallvec};

pub(crate) type AsyncControls = SmallVec<[AsyncControl; 1]>;
pub(crate) type AsyncResetRequests = SmallVec<[AsyncResetRequest; 2]>;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SequentialTiming {
    pub(crate) clock_to_q: Option<f64>,
    pub(crate) output_transition: Option<f64>,
    pub(crate) setup: Option<f64>,
}

/// The reset controls and outputs every sequential cell shape shares.
#[derive(Clone, Copy)]
pub(crate) struct SequentialTail<'a> {
    pub(crate) cell_name: &'a str,
    pub(crate) resets: &'a AsyncControls,
    pub(crate) reset_values: &'a [word::ValueId],
    pub(crate) output_pin: &'a str,
    pub(crate) target: word::ValueId,
    pub(crate) inverted_output: Option<&'a SequentialInvertedOutput>,
    pub(crate) inverted_target: Option<word::ValueId>,
}

/// Appends the asynchronous reset inputs to `input_connections` and builds the
/// output connections, producing the finished mapped cell.
///
/// Flip-flops, enable flip-flops, and latches differ only in their leading data
/// inputs; everything from the resets onward is identical.
fn finish_mapped_cell(
    mut input_connections: SmallVec<[MappedInputConnection; 4]>,
    tail: SequentialTail<'_>,
) -> MappedCell {
    assert_eq!(tail.resets.len(), tail.reset_values.len());
    for (control, &value) in tail.resets.iter().zip(tail.reset_values) {
        input_connections.push(MappedInputConnection {
            pin: control.pin.clone(),
            value,
        });
    }
    let mut output_connections = smallvec![MappedOutputConnection {
        pin: tail.output_pin.to_string(),
        value: tail.target,
    }];
    if let (Some(output), Some(value)) = (tail.inverted_output, tail.inverted_target) {
        output_connections.push(MappedOutputConnection {
            pin: output.pin.clone(),
            value,
        });
    }
    MappedCell {
        cell_name: tail.cell_name.to_string(),
        input_connections,
        output_connections,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SequentialCell {
    pub(crate) cell_name: String,
    pub(crate) data_pin: String,
    pub(crate) clock_pin: String,
    pub(crate) output_pin: String,
    pub(crate) inverted_output: Option<SequentialInvertedOutput>,
    pub(crate) resets: AsyncControls,
    pub(crate) edge: word::Edge,
    pub(crate) cost: CellCost,
    pub(crate) timing: SequentialTiming,
}

impl SequentialCell {
    pub(crate) fn mapping_cost(
        &self,
        inverted_output: bool,
        inverter: Option<CellCost>,
    ) -> Option<MappingCost> {
        sequential_mapping_cost(
            self.cost,
            self.inverted_output.as_ref(),
            inverted_output,
            inverter,
        )
    }

    pub(crate) fn mapped_cell(
        &self,
        data: word::ValueId,
        clock: word::ValueId,
        resets: &[word::ValueId],
        target: word::ValueId,
        inverted_target: Option<word::ValueId>,
    ) -> MappedCell {
        let input_connections = smallvec![
            MappedInputConnection {
                pin: self.data_pin.clone(),
                value: data,
            },
            MappedInputConnection {
                pin: self.clock_pin.clone(),
                value: clock,
            },
        ];
        finish_mapped_cell(
            input_connections,
            SequentialTail {
                cell_name: &self.cell_name,
                resets: &self.resets,
                reset_values: resets,
                output_pin: &self.output_pin,
                target,
                inverted_output: self.inverted_output.as_ref(),
                inverted_target,
            },
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SequentialEnableCell {
    pub(crate) cell_name: String,
    pub(crate) data_pin: String,
    pub(crate) enable_pin: String,
    pub(crate) enable_active_high: bool,
    pub(crate) clock_pin: String,
    pub(crate) output_pin: String,
    pub(crate) inverted_output: Option<SequentialInvertedOutput>,
    pub(crate) resets: AsyncControls,
    pub(crate) edge: word::Edge,
    pub(crate) cost: CellCost,
    pub(crate) timing: SequentialTiming,
}

impl SequentialEnableCell {
    pub(crate) fn enable_active_high(&self) -> bool {
        self.enable_active_high
    }

    pub(crate) fn mapping_cost(
        &self,
        enable_active_high: bool,
        inverted_output: bool,
        inverter: Option<CellCost>,
    ) -> Option<MappingCost> {
        let mut cost = sequential_mapping_cost(
            self.cost,
            self.inverted_output.as_ref(),
            inverted_output,
            inverter,
        )?;
        if enable_active_high != self.enable_active_high {
            cost = cost.cell(inverter?);
        }
        Some(cost)
    }

    pub(crate) fn mapped_cell(
        &self,
        data: word::ValueId,
        enable: word::ValueId,
        clock: word::ValueId,
        resets: &[word::ValueId],
        target: word::ValueId,
        inverted_target: Option<word::ValueId>,
    ) -> MappedCell {
        let input_connections = smallvec![
            MappedInputConnection {
                pin: self.data_pin.clone(),
                value: data,
            },
            MappedInputConnection {
                pin: self.enable_pin.clone(),
                value: enable,
            },
            MappedInputConnection {
                pin: self.clock_pin.clone(),
                value: clock,
            },
        ];
        finish_mapped_cell(
            input_connections,
            SequentialTail {
                cell_name: &self.cell_name,
                resets: &self.resets,
                reset_values: resets,
                output_pin: &self.output_pin,
                target,
                inverted_output: self.inverted_output.as_ref(),
                inverted_target,
            },
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LatchCell {
    pub(crate) cell_name: String,
    pub(crate) data_pin: String,
    pub(crate) enable_pin: String,
    pub(crate) enable_active_high: bool,
    pub(crate) output_pin: String,
    pub(crate) inverted_output: Option<SequentialInvertedOutput>,
    pub(crate) resets: AsyncControls,
    pub(crate) cost: CellCost,
}

impl LatchCell {
    pub(crate) fn enable_active_high(&self) -> bool {
        self.enable_active_high
    }

    pub(crate) fn mapping_cost(
        &self,
        enable_active_high: bool,
        inverted_output: bool,
        inverter: Option<CellCost>,
    ) -> Option<MappingCost> {
        let mut cost = sequential_mapping_cost(
            self.cost,
            self.inverted_output.as_ref(),
            inverted_output,
            inverter,
        )?;
        if enable_active_high != self.enable_active_high {
            cost = cost.cell(inverter?);
        }
        Some(cost)
    }

    pub(crate) fn mapped_cell(
        &self,
        data: word::ValueId,
        enable: word::ValueId,
        resets: &[word::ValueId],
        target: word::ValueId,
        inverted_target: Option<word::ValueId>,
    ) -> MappedCell {
        let input_connections = smallvec![
            MappedInputConnection {
                pin: self.data_pin.clone(),
                value: data,
            },
            MappedInputConnection {
                pin: self.enable_pin.clone(),
                value: enable,
            },
        ];
        finish_mapped_cell(
            input_connections,
            SequentialTail {
                cell_name: &self.cell_name,
                resets: &self.resets,
                reset_values: resets,
                output_pin: &self.output_pin,
                target,
                inverted_output: self.inverted_output.as_ref(),
                inverted_target,
            },
        )
    }
}

pub(crate) struct EnablePattern<'a> {
    pub(crate) data_pin: &'a str,
    pub(crate) enable_pin: &'a str,
    pub(crate) enable_active_high: bool,
}

pub(crate) fn enable_flip_flop_pattern<'function>(
    cell: TargetCellRef<'function>,
    sequential: TargetSequentialRef<'function>,
    next_state: crate::BooleanFunctionRef<'function>,
) -> Option<EnablePattern<'function>> {
    let mut names = BTreeSet::new();
    collect_function_pins(next_state, &mut names);
    let (state_names, pin_names): (Vec<&str>, Vec<&str>) = names
        .iter()
        .partition(|name| sequential.state_variables().any(|state| state == **name));
    let [state_name] = state_names.as_slice() else {
        return None;
    };
    let [first, second] = pin_names.as_slice() else {
        return None;
    };
    let pins = [*first, *second];
    for name in pins {
        let pin = cell
            .pins()
            .find(|pin| pin.name() == name && pin.direction() == TargetPinDirection::Input)?;
        if matches!(
            pin.next_state_type(),
            Some(crate::TargetNextStateType::ScanIn | crate::TargetNextStateType::ScanEnable)
        ) {
            return None;
        }
    }
    for (enable_name, data_name) in [(pins[0], pins[1]), (pins[1], pins[0])] {
        for enable_active_high in [true, false] {
            let matches = (0u8..8).all(|assignment| {
                let enable = assignment & 1 != 0;
                let data = assignment & 2 != 0;
                let state = assignment & 4 != 0;
                let expected = if enable == enable_active_high {
                    data
                } else {
                    state
                };
                next_state.eval(&mut |name| {
                    if name == enable_name {
                        Some(enable)
                    } else if name == data_name {
                        Some(data)
                    } else if name == *state_name {
                        Some(state)
                    } else {
                        None
                    }
                }) == Some(expected)
            });
            if matches {
                return Some(EnablePattern {
                    data_pin: data_name,
                    enable_pin: enable_name,
                    enable_active_high,
                });
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub(crate) struct SequentialInvertedOutput {
    pub(crate) pin: String,
    pub(crate) cost: CellCost,
}

pub(crate) fn latch_cell(
    cell: TargetCellRef<'_>,
    sequential: TargetSequentialRef<'_>,
) -> Option<LatchCell> {
    let (enable_pin, enable_active_high) = sequential.enable()?.as_literal()?;
    let (data_pin, data_positive) = sequential.next_state()?.as_literal()?;
    if !data_positive
        || data_pin == enable_pin
        || !is_input_pin(cell, data_pin)
        || !is_input_pin(cell, enable_pin)
    {
        return None;
    }
    let (output_pin, inverted_output_pin) = state_output_pins(cell, sequential)?;
    let resets = asynchronous_controls(cell, sequential)?;
    let data_pins = [data_pin];
    let cost = cell_cost(
        cell,
        &data_pins,
        enable_pin,
        &resets
            .iter()
            .map(|control| control.pin.as_str())
            .collect::<Vec<_>>(),
        output_pin,
    );
    let inverted_output = inverted_output_pin.map(|pin| SequentialInvertedOutput {
        pin: pin.name().to_string(),
        cost: cell_cost(
            cell,
            &data_pins,
            enable_pin,
            &resets
                .iter()
                .map(|control| control.pin.as_str())
                .collect::<Vec<_>>(),
            pin,
        ),
    });
    Some(LatchCell {
        cell_name: cell.name().to_string(),
        data_pin: data_pin.to_string(),
        enable_pin: enable_pin.to_string(),
        enable_active_high,
        output_pin: output_pin.name().to_string(),
        inverted_output,
        resets,
        cost,
    })
}

pub(crate) fn sequential_mapping_cost(
    direct: CellCost,
    complementary_output: Option<&SequentialInvertedOutput>,
    requires_inversion: bool,
    inverter: Option<CellCost>,
) -> Option<MappingCost> {
    let mut cost = MappingCost::zero().cell(direct);
    if !requires_inversion {
        return Some(cost);
    }
    if let Some(complementary_output) = complementary_output {
        cost.delay = cost.delay.max(complementary_output.cost.delay);
        cost.transition = cost.transition.max(complementary_output.cost.transition);
        cost.input_capacitance = cost
            .input_capacitance
            .max(complementary_output.cost.input_capacitance);
        return Some(cost);
    }
    Some(cost.cell(inverter?))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct AsyncControl {
    pub(crate) pin: String,
    pub(crate) active_high: bool,
    pub(crate) reset_value: bool,
}

impl AsyncControl {
    pub(crate) fn request(&self) -> AsyncResetRequest {
        AsyncResetRequest {
            active_high: self.active_high,
            reset_value: self.reset_value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AsyncResetRequest {
    pub(crate) active_high: bool,
    pub(crate) reset_value: bool,
}

pub(crate) fn asynchronous_controls(
    cell: TargetCellRef<'_>,
    sequential: TargetSequentialRef<'_>,
) -> Option<AsyncControls> {
    let mut controls = AsyncControls::new();
    if let Some(clear) = sequential.clear() {
        controls.push(control_from_literal(cell, clear, false)?);
    }
    if let Some(preset) = sequential.preset() {
        controls.push(control_from_literal(cell, preset, true)?);
    }
    Some(controls)
}

pub(crate) fn control_from_literal(
    cell: TargetCellRef<'_>,
    function: crate::BooleanFunctionRef<'_>,
    reset_value: bool,
) -> Option<AsyncControl> {
    let (pin, active_high) = function.as_literal()?;
    if !is_input_pin(cell, pin) {
        return None;
    }
    Some(AsyncControl {
        pin: pin.to_string(),
        active_high,
        reset_value,
    })
}

pub(crate) fn state_output_pins<'a>(
    cell: TargetCellRef<'a>,
    sequential: TargetSequentialRef<'a>,
) -> Option<(TargetPinRef<'a>, Option<TargetPinRef<'a>>)> {
    let direct_state = sequential.state_variables().next()?;
    let inverted_state = sequential.state_variables().nth(1);
    let mut direct = None;
    let mut inverted = None;
    for pin in cell
        .pins()
        .filter(|pin| pin.direction() == TargetPinDirection::Output)
    {
        let Some((state, positive)) = pin
            .function()
            .and_then(crate::BooleanFunctionRef::as_literal)
        else {
            continue;
        };
        if state == direct_state && positive {
            direct.get_or_insert(pin);
        } else if (state == direct_state && !positive) || inverted_state == Some(state) && positive
        {
            inverted.get_or_insert(pin);
        }
    }
    Some((direct?, inverted))
}

pub(crate) fn next_state_input_pins<'a>(
    cell: TargetCellRef<'a>,
    sequential: TargetSequentialRef<'a>,
    function: crate::BooleanFunctionRef<'a>,
) -> Option<Vec<TargetPinRef<'a>>> {
    let mut names = BTreeSet::new();
    collect_function_pins(function, &mut names);
    if names.is_empty()
        || names
            .iter()
            .any(|name| sequential.state_variables().any(|state| state == *name))
    {
        return None;
    }
    let pins = cell
        .pins()
        .filter(|pin| pin.direction() == TargetPinDirection::Input && names.contains(pin.name()))
        .collect::<Vec<_>>();
    if pins.iter().any(|pin| {
        matches!(
            pin.next_state_type(),
            Some(crate::TargetNextStateType::ScanIn | crate::TargetNextStateType::ScanEnable)
        )
    }) {
        return None;
    }
    (pins.len() == names.len() && pins.len() <= crate::boolean::logic::MAX_MATCH_INPUTS)
        .then_some(pins)
}

pub(crate) fn collect_function_pins<'a>(
    function: crate::BooleanFunctionRef<'a>,
    names: &mut BTreeSet<&'a str>,
) {
    function.for_each_pin(&mut |name| {
        names.insert(name);
    });
}

pub(crate) fn is_input_pin(cell: TargetCellRef<'_>, name: &str) -> bool {
    cell.pins()
        .any(|pin| pin.name() == name && pin.direction() == TargetPinDirection::Input)
}

pub(crate) fn cell_cost(
    cell: TargetCellRef<'_>,
    data_pins: &[&str],
    clock_pin: &str,
    reset_pins: &[&str],
    output_pin: TargetPinRef<'_>,
) -> CellCost {
    let mut delay = 0.0f64;
    let mut transition = 0.0f64;
    for arc in output_pin
        .timing_arcs()
        .filter(|arc| arc.related_pin() == clock_pin)
    {
        delay = delay.max(arc.default_delay().unwrap_or(0.0));
        transition = transition.max(arc.default_transition().unwrap_or(0.0));
    }
    let input_capacitance = cell
        .pins()
        .filter(|pin| {
            data_pins.contains(&pin.name())
                || pin.name() == clock_pin
                || reset_pins.contains(&pin.name())
        })
        .filter_map(|pin| pin.max_capacitance().filter(|value| value.is_finite()))
        .sum();
    CellCost {
        area: cell
            .area()
            .filter(|value| value.is_finite())
            .unwrap_or(f64::INFINITY),
        delay,
        transition,
        input_capacitance,
    }
}

pub(crate) fn sequential_timing(
    cell: TargetCellRef<'_>,
    data_pin: &str,
    clock_pin: &str,
    output_pin: TargetPinRef<'_>,
    edge: word::Edge,
) -> SequentialTiming {
    let clock_edge = match edge {
        word::Edge::Pos => TimingEdge::Rise,
        word::Edge::Neg => TimingEdge::Fall,
    };
    let mut timing = SequentialTiming::default();
    for arc in output_pin.timing_arcs().filter(|arc| {
        arc.related_pin() == clock_pin
            && arc.timing_type() == TargetTimingType::ClockToQ(clock_edge)
    }) {
        timing.clock_to_q = max_optional(timing.clock_to_q, arc.default_delay());
        timing.output_transition = max_optional(timing.output_transition, arc.default_transition());
    }
    if let Some(pin) = cell.pins().find(|pin| pin.name() == data_pin) {
        for arc in pin.timing_arcs().filter(|arc| {
            arc.related_pin() == clock_pin
                && arc.timing_type()
                    == TargetTimingType::Check {
                        kind: TimingCheckKind::Setup,
                        clock_edge,
                    }
        }) {
            let constraint = max_optional(
                arc.rise_constraint()
                    .and_then(opto_library::LookupTable::default_value),
                arc.fall_constraint()
                    .and_then(opto_library::LookupTable::default_value),
            );
            timing.setup = max_optional(timing.setup, constraint);
        }
    }
    timing
}

fn max_optional(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

pub(crate) fn compare_cells(left: &SequentialCell, right: &SequentialCell) -> Ordering {
    debug_assert_eq!(left.edge, right.edge);
    compare_cell_cost(left.cost, right.cost)
        .then_with(|| left.cell_name.cmp(&right.cell_name))
        .then_with(|| left.data_pin.cmp(&right.data_pin))
        .then_with(|| left.clock_pin.cmp(&right.clock_pin))
        .then_with(|| left.output_pin.cmp(&right.output_pin))
}

pub(crate) fn compare_latch_cells(left: &LatchCell, right: &LatchCell) -> Ordering {
    compare_cell_cost(left.cost, right.cost)
        .then_with(|| left.cell_name.cmp(&right.cell_name))
        .then_with(|| left.data_pin.cmp(&right.data_pin))
        .then_with(|| left.enable_pin.cmp(&right.enable_pin))
        .then_with(|| left.output_pin.cmp(&right.output_pin))
}
