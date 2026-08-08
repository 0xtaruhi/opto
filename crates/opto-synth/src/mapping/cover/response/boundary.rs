// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Boundary value projection and per-port measurement for cover responses.

use super::{
    ArrivalReduction, BoundaryContract, BoundaryInputMeasurements, BoundaryValueIndex, EarlyLate,
    LaneEvaluation, Measurement, RiseFall, optional_finite, word,
};
use crate::mapping::cover::AnalyzedRegionOutput;

impl BoundaryValueIndex {
    pub(crate) fn build(
        subject_inputs: &[word::ValueId],
        output_values: &[AnalyzedRegionOutput],
        contracts: &[BoundaryContract],
        regional_slice: &crate::mapping::logic_partition::RegionLogicSlice,
    ) -> Result<Self, crate::SynthError> {
        const ABSENT: u32 = u32::MAX;

        let mut input_position_count = subject_inputs
            .iter()
            .map(|value| value.index())
            .max()
            .map_or(0, |index| index + 1);
        for contract in contracts
            .iter()
            .filter(|contract| contract.port().direction() == crate::RegionPortDirection::Input)
        {
            for value in regional_slice.boundary_input_bits(contract.port().semantic_key())? {
                input_position_count = input_position_count.max(value.index() + 1);
            }
        }
        let mut input_positions = vec![ABSENT; input_position_count];
        for (index, value) in subject_inputs.iter().copied().enumerate() {
            input_positions[value.index()] = u32::try_from(index).map_err(|_| {
                crate::SynthError::capacity("regional input count exceeds 32-bit capacity")
            })?;
        }

        let mut output_position_count = output_values
            .iter()
            .flat_map(|output| output.values.iter())
            .map(|value| value.index())
            .max()
            .map_or(0, |index| index + 1);
        for contract in contracts
            .iter()
            .filter(|contract| contract.port().direction() == crate::RegionPortDirection::Output)
        {
            for value in regional_slice.boundary_output_bits(contract.port().semantic_key())? {
                output_position_count = output_position_count.max(value.index() + 1);
            }
        }
        let mut output_positions = vec![ABSENT; output_position_count];
        for (index, output) in output_values.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| {
                crate::SynthError::capacity("regional output count exceeds 32-bit capacity")
            })?;
            for value in output.values.iter().copied() {
                let position = &mut output_positions[value.index()];
                if *position == ABSENT {
                    *position = index;
                }
            }
        }

        let input_rows = contracts
            .iter()
            .map(|contract| {
                if contract.port().direction() == crate::RegionPortDirection::Input {
                    project_boundary_positions(
                        regional_slice.boundary_input_bits(contract.port().semantic_key())?,
                        &input_positions,
                        ABSENT,
                        "input",
                    )
                } else {
                    Ok(Vec::new())
                }
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let inputs_by_contract =
            opto_core::PackedRows::try_from_row_iter(input_rows).map_err(|_| {
                crate::SynthError::capacity("regional boundary-input index exceeds 32-bit capacity")
            })?;
        let output_rows = contracts
            .iter()
            .map(|contract| {
                if contract.port().direction() == crate::RegionPortDirection::Output {
                    project_boundary_positions(
                        regional_slice.boundary_output_bits(contract.port().semantic_key())?,
                        &output_positions,
                        ABSENT,
                        "output",
                    )
                } else {
                    Ok(Vec::new())
                }
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let outputs_by_contract =
            opto_core::PackedRows::try_from_row_iter(output_rows).map_err(|_| {
                crate::SynthError::capacity(
                    "regional boundary-output index exceeds 32-bit capacity",
                )
            })?;
        let contracts_by_input = opto_core::PackedRows::try_from_entries(
            subject_inputs.len(),
            (0..contracts.len()).flat_map(|contract_index| {
                inputs_by_contract
                    .row(contract_index)
                    .iter()
                    .copied()
                    .map(move |input_index| (input_index, contract_index))
            }),
        )
        .map_err(|_| {
            crate::SynthError::capacity("regional input-contract index exceeds 32-bit capacity")
        })?;

        Ok(Self {
            outputs_by_contract,
            inputs_by_contract,
            contracts_by_input,
        })
    }
}

pub(crate) fn project_boundary_positions(
    bits: &[word::ValueId],
    positions: &[u32],
    absent: u32,
    direction: &str,
) -> Result<Vec<usize>, crate::SynthError> {
    let mut projected = Vec::new();
    for &value in bits {
        let position = positions.get(value.index()).copied().ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "regional boundary {direction} bit is outside its cover-position index"
            ))
        })?;
        if position != absent {
            projected.push(position as usize);
        }
    }
    Ok(projected)
}

pub(crate) fn boundary_input_measurements(
    contracts: &[BoundaryContract],
    contracts_by_input: &opto_core::PackedRows<usize>,
    scenario: opto_timing::ScenarioId,
    timing_tag: crate::TimingTagId,
) -> BoundaryInputMeasurements {
    let input_count = contracts_by_input.row_count();
    let mut measurements = BoundaryInputMeasurements {
        early_rise: Vec::with_capacity(input_count),
        early_fall: Vec::with_capacity(input_count),
        late_rise: Vec::with_capacity(input_count),
        late_fall: Vec::with_capacity(input_count),
    };
    for input_index in 0..input_count {
        let input_contracts = contracts_by_input.row(input_index);
        let contract_input = input_contracts.iter().find_map(|&contract_index| {
            exact_contract_row(&contracts[contract_index], scenario, timing_tag)
                .and_then(|row| row.input)
        });
        let active = contract_input.is_some() || input_contracts.is_empty();
        for (target, late, fall) in [
            (&mut measurements.early_rise, false, false),
            (&mut measurements.early_fall, false, true),
            (&mut measurements.late_rise, true, false),
            (&mut measurements.late_fall, true, true),
        ] {
            let lane = |values: EarlyLate<RiseFall<Option<crate::FiniteValue>>>| {
                let edge = if late { values.late } else { values.early };
                if fall { edge.fall } else { edge.rise }
            };
            target.push(Measurement {
                // Region-local hard sources start at zero. A true regional
                // boundary participates only when this exact sparse tag row
                // exists; another tag must never be injected as arrival zero.
                active,
                arrival: contract_input
                    .and_then(|input| lane(input.arrival))
                    .map_or(0.0, crate::FiniteValue::get),
                transition: contract_input
                    .and_then(|input| lane(input.transition))
                    .map_or(0.0, crate::FiniteValue::get),
            });
        }
    }
    measurements
}

pub(crate) fn boundary_output_loads(
    output_count: usize,
    contracts: &[BoundaryContract],
    output_indices: &opto_core::PackedRows<usize>,
    scenario: opto_timing::ScenarioId,
    timing_tag: crate::TimingTagId,
    late: bool,
) -> Vec<f64> {
    let mut loads = vec![0.0f64; output_count];
    for (contract_index, contract) in contracts.iter().enumerate() {
        let load = exact_contract_row(contract, scenario, timing_tag)
            .and_then(|row| row.output)
            .and_then(|output| {
                if late {
                    output.capacitance.late
                } else {
                    output.capacitance.early
                }
            })
            .map_or(0.0, crate::FiniteValue::get);
        for &index in output_indices.row(contract_index) {
            if let Some(current) = loads.get_mut(index) {
                *current = current.max(load);
            }
        }
    }
    loads
}

pub(crate) fn exact_contract_row(
    contract: &BoundaryContract,
    scenario: opto_timing::ScenarioId,
    timing_tag: crate::TimingTagId,
) -> Option<&crate::BoundaryContractRow> {
    contract
        .rows()
        .binary_search_by_key(&(scenario, timing_tag), |row| {
            (row.scenario, row.timing_tag)
        })
        .ok()
        .map(|index| &contract.rows()[index])
}

pub(crate) fn reduced_output(
    evaluation: &LaneEvaluation,
    indices: &[usize],
    select: impl Fn(Measurement) -> f64,
    reduction: ArrivalReduction,
) -> Result<Option<crate::FiniteValue>, crate::SynthError> {
    let values = indices
        .iter()
        .filter_map(|&index| evaluation.outputs.get(index).copied())
        .filter(|measurement| measurement.active)
        .map(select);
    optional_finite(match reduction {
        ArrivalReduction::Earliest => values.min_by(f64::total_cmp),
        ArrivalReduction::Latest => values.max_by(f64::total_cmp),
    })
}

pub(crate) fn maximum_input_capacitance(
    evaluation: &LaneEvaluation,
    indices: &[usize],
) -> Result<Option<crate::FiniteValue>, crate::SynthError> {
    optional_finite(
        indices
            .iter()
            .filter_map(|&index| evaluation.input_capacitances.get(index).copied())
            .max_by(f64::total_cmp),
    )
}
