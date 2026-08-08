// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::super::search::{LibraryCover, LibraryCoverSource};
use super::ScenarioBinding;
use crate::BoundaryContract;

pub(crate) struct ActivityEvaluation {
    outputs: Box<[opto_timing::ScenarioSwitchingActivity]>,
    pub(crate) switched_capacitance: f64,
}

pub(crate) fn evaluate_activity(
    cover: &LibraryCover,
    bindings: &[ScenarioBinding],
    inputs: &[Option<opto_timing::ScenarioSwitchingActivity>],
) -> Result<Option<ActivityEvaluation>, crate::SynthError> {
    if inputs.iter().any(Option::is_none) {
        return Ok(None);
    }
    if bindings.len() != cover.cells.len() {
        return Err(crate::SynthError::invariant(
            "scenario binding arena does not align with regional activity",
        ));
    }
    let inputs = inputs.iter().copied().flatten().collect::<Vec<_>>();
    let mut cells = Vec::<[Option<opto_timing::ScenarioSwitchingActivity>; 2]>::new();
    let mut switched_capacitance = 0.0;
    for (cell, binding) in cover.cells.iter().zip(bindings) {
        let source_activities = cell
            .sources
            .iter()
            .copied()
            .map(|source| source_activity(source, &inputs, &cells))
            .collect::<Result<Vec<_>, _>>()?;
        for (source_index, load) in binding.input_loads() {
            switched_capacitance += source_activities
                .get(source_index)
                .ok_or_else(|| {
                    crate::SynthError::invariant("cover input load is outside its signature")
                })?
                .toggle_rate()
                * load;
        }
        let first = truth_activity(cell.truth, &source_activities)?;
        let second = cell
            .second_truth
            .map(|truth| truth_activity(truth, &source_activities))
            .transpose()?;
        cells.push([Some(first), second]);
    }
    let outputs = cover
        .outputs
        .iter()
        .copied()
        .map(|source| source_activity(source, &inputs, &cells))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    Ok(Some(ActivityEvaluation {
        outputs,
        switched_capacitance,
    }))
}

fn source_activity(
    source: LibraryCoverSource,
    inputs: &[opto_timing::ScenarioSwitchingActivity],
    cells: &[[Option<opto_timing::ScenarioSwitchingActivity>; 2]],
) -> Result<opto_timing::ScenarioSwitchingActivity, crate::SynthError> {
    match source {
        LibraryCoverSource::Constant(value) => {
            Ok(
                opto_timing::ScenarioSwitchingActivity::new(f64::from(value), 0.0, 0.5)
                    .expect("constant activity is valid"),
            )
        }
        LibraryCoverSource::Input(index) => inputs.get(index).copied().ok_or_else(|| {
            crate::SynthError::invariant("cover activity references an unknown regional input")
        }),
        LibraryCoverSource::Cell(index) => cells
            .get(index)
            .and_then(|outputs| outputs[0])
            .ok_or_else(|| crate::SynthError::invariant("cover activity cell is unknown")),
        LibraryCoverSource::CellSecond(index) => cells
            .get(index)
            .and_then(|outputs| outputs[1])
            .ok_or_else(|| {
                crate::SynthError::invariant("cover secondary activity cell is unknown")
            }),
    }
}

fn truth_activity(
    truth: crate::boolean::logic::TruthTable,
    inputs: &[opto_timing::ScenarioSwitchingActivity],
) -> Result<opto_timing::ScenarioSwitchingActivity, crate::SynthError> {
    if truth.input_count != inputs.len() {
        return Err(crate::SynthError::invariant(
            "cover truth and activity signature are misaligned",
        ));
    }
    let assignment_count = 1usize << inputs.len();
    let mut probability = 0.0;
    for assignment in 0..assignment_count {
        if truth.bit(assignment) {
            probability += assignment_probability(inputs, assignment, None);
        }
    }
    let mut toggle_rate = 0.0;
    for (input, activity) in inputs.iter().enumerate() {
        let mask = 1usize << input;
        let mut influence = 0.0;
        for assignment in 0..assignment_count {
            if assignment & mask == 0 && truth.bit(assignment) != truth.bit(assignment | mask) {
                influence += assignment_probability(inputs, assignment, Some(input));
            }
        }
        toggle_rate += activity.toggle_rate() * influence;
    }
    opto_timing::ScenarioSwitchingActivity::new(probability, toggle_rate, 0.5).ok_or_else(|| {
        crate::SynthError::invariant("propagated regional switching activity is invalid")
    })
}

fn assignment_probability(
    inputs: &[opto_timing::ScenarioSwitchingActivity],
    assignment: usize,
    excluded: Option<usize>,
) -> f64 {
    inputs
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != excluded)
        .map(|(index, activity)| {
            if assignment & (1usize << index) == 0 {
                1.0 - activity.static_probability()
            } else {
                activity.static_probability()
            }
        })
        .product()
}

pub(crate) fn maximum_output_activity(
    evaluation: &ActivityEvaluation,
    indices: &[usize],
) -> Option<opto_timing::ScenarioSwitchingActivity> {
    indices
        .iter()
        .filter_map(|&index| evaluation.outputs.get(index).copied())
        .max_by(|left, right| left.toggle_rate().total_cmp(&right.toggle_rate()))
}

pub(crate) fn boundary_input_activities(
    contracts: &[BoundaryContract],
    contracts_by_input: &opto_core::PackedRows<usize>,
    scenario: opto_timing::ScenarioId,
) -> Vec<Option<opto_timing::ScenarioSwitchingActivity>> {
    (0..contracts_by_input.row_count())
        .map(|input_index| {
            contracts_by_input
                .row(input_index)
                .iter()
                .find_map(|&contract_index| {
                    let rows = contracts[contract_index].rows();
                    let first = rows.partition_point(|row| row.scenario < scenario);
                    rows.get(first)
                        .filter(|row| row.scenario == scenario)
                        .and_then(|row| row.input)
                        .and_then(|input| input.activity)
                })
        })
        .collect()
}
