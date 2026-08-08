// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::search::LibraryCoverCell;
use super::{LibraryCover, LibraryCoverBinding, LibraryCoverSource};
use crate::mapping::library::{CellBinding, CombinationalCellCatalog, JointCellBinding};
use crate::planning::mapping_policy::CellCost;
use crate::{BoundaryContract, BoundaryResponse, BoundaryResponseRow, EarlyLate, RiseFall};
use hashbrown::HashMap;
use opto_ir::word;
use std::collections::BTreeSet;

mod activity;
use activity::{boundary_input_activities, evaluate_activity, maximum_output_activity};

mod boundary;
use boundary::{
    boundary_input_measurements, boundary_output_loads, maximum_input_capacitance, reduced_output,
};

#[derive(Debug, Clone, Copy)]
struct Measurement {
    active: bool,
    arrival: f64,
    transition: f64,
}

struct LaneEvaluation {
    outputs: Box<[Measurement]>,
    input_capacitances: Box<[f64]>,
}

#[derive(Debug, Clone, Copy)]
enum ArrivalReduction {
    Earliest,
    Latest,
}

struct TimingTagEvaluation {
    early_rise: LaneEvaluation,
    early_fall: LaneEvaluation,
    late_rise: LaneEvaluation,
    late_fall: LaneEvaluation,
}

struct BoundaryInputMeasurements {
    early_rise: Vec<Measurement>,
    early_fall: Vec<Measurement>,
    late_rise: Vec<Measurement>,
    late_fall: Vec<Measurement>,
}

struct BoundaryValueIndex {
    outputs_by_contract: opto_core::PackedRows<usize>,
    inputs_by_contract: opto_core::PackedRows<usize>,
    contracts_by_input: opto_core::PackedRows<usize>,
}

pub(crate) struct MeasuredCoverResponse {
    pub(crate) boundaries: Vec<BoundaryResponse>,
    pub(crate) dynamic_power: Option<f64>,
}

pub(crate) struct BoundaryScore {
    pub(crate) worst_normalized_violation: f64,
    pub(crate) minimum_slack: f64,
    pub(crate) total_negative_slack: f64,
}

pub(crate) struct CoverResponseModels<'a> {
    scenarios: &'a opto_timing::ScenarioSet,
    catalogs: Box<[ScenarioCatalog]>,
}

struct ScenarioCatalog {
    early: CombinationalCellCatalog,
    late: CombinationalCellCatalog,
    leakage_by_cell: HashMap<Box<str>, f64>,
}

impl<'a> CoverResponseModels<'a> {
    pub(crate) fn new(scenarios: &'a opto_timing::ScenarioSet) -> Self {
        let catalogs = scenarios
            .scenarios()
            .iter()
            .map(|scenario| ScenarioCatalog {
                early: CombinationalCellCatalog::from_cells(
                    &scenario.early_library().cells,
                    crate::SynthesisDiagnostics::default(),
                ),
                late: CombinationalCellCatalog::from_cells(
                    &scenario.late_library().cells,
                    crate::SynthesisDiagnostics::default(),
                ),
                leakage_by_cell: scenario
                    .power()
                    .library()
                    .cells
                    .iter()
                    .filter_map(|cell| {
                        cell.cell_leakage_power
                            .or_else(|| {
                                cell.leakage_power
                                    .iter()
                                    .map(|group| group.value)
                                    .max_by(f64::total_cmp)
                            })
                            .map(|leakage| (cell.name.clone().into_boxed_str(), leakage))
                    })
                    .collect(),
            })
            .collect();
        Self {
            scenarios,
            catalogs,
        }
    }

    pub(crate) fn leakage_by_scenario<'b>(
        &'b self,
        cell_name: &'b str,
    ) -> impl ExactSizeIterator<Item = Option<f64>> + 'b {
        self.catalogs
            .iter()
            .map(move |scenario| scenario.leakage_by_cell.get(cell_name).copied())
    }

    pub(crate) fn regional_leakage(
        &self,
        cover: &LibraryCover,
        catalog: &CombinationalCellCatalog,
    ) -> Option<f64> {
        self.catalogs
            .iter()
            .map(|scenario| {
                cover.cells.iter().try_fold(0.0, |total, cell| {
                    let name = match cell.binding {
                        LibraryCoverBinding::Single(binding) => catalog.binding_cell_name(binding),
                        LibraryCoverBinding::Joint(binding) => {
                            catalog.joint_binding_cell_name(binding)
                        }
                    };
                    Some(total + scenario.leakage_by_cell.get(name)?)
                })
            })
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max_by(f64::total_cmp)
    }
}

pub(crate) fn measure(
    subject_inputs: &[word::ValueId],
    output_values: &[super::AnalyzedRegionOutput],
    cover: &LibraryCover,
    contracts: &[BoundaryContract],
    regional_slice: &crate::mapping::logic_partition::RegionLogicSlice,
    models: &CoverResponseModels<'_>,
) -> Result<MeasuredCoverResponse, crate::SynthError> {
    let mut responses = contracts
        .iter()
        .map(|contract| BoundaryResponse {
            port_semantic_key: contract.port().semantic_key(),
            rows: Box::new([]),
        })
        .collect::<Vec<_>>();
    let boundary_index =
        BoundaryValueIndex::build(subject_inputs, output_values, contracts, regional_slice)?;
    let mut rows = vec![Vec::new(); contracts.len()];
    let mut dynamic_power = Vec::new();
    for (scenario, scenario_catalog) in models
        .scenarios
        .scenarios()
        .iter()
        .zip(models.catalogs.iter())
    {
        let early_bindings = scenario_bindings(cover, &scenario_catalog.early)?;
        let late_bindings = scenario_bindings(cover, &scenario_catalog.late)?;
        let timing_tags = contracts
            .iter()
            .flat_map(BoundaryContract::rows)
            .filter(|row| row.scenario == scenario.id())
            .map(|row| row.timing_tag)
            .collect::<BTreeSet<_>>();
        let mut timing_evaluations = Vec::with_capacity(timing_tags.len());
        for timing_tag in timing_tags {
            let early_loads = boundary_output_loads(
                output_values.len(),
                contracts,
                &boundary_index.outputs_by_contract,
                scenario.id(),
                timing_tag,
                false,
            );
            let late_loads = boundary_output_loads(
                output_values.len(),
                contracts,
                &boundary_index.outputs_by_contract,
                scenario.id(),
                timing_tag,
                true,
            );
            let input_measurements = boundary_input_measurements(
                contracts,
                &boundary_index.contracts_by_input,
                scenario.id(),
                timing_tag,
            );
            timing_evaluations.push((
                timing_tag,
                TimingTagEvaluation {
                    early_rise: evaluate_lane(
                        cover,
                        &scenario_catalog.early,
                        &early_bindings,
                        &input_measurements.early_rise,
                        &early_loads,
                        ArrivalReduction::Earliest,
                    )?,
                    early_fall: evaluate_lane(
                        cover,
                        &scenario_catalog.early,
                        &early_bindings,
                        &input_measurements.early_fall,
                        &early_loads,
                        ArrivalReduction::Earliest,
                    )?,
                    late_rise: evaluate_lane(
                        cover,
                        &scenario_catalog.late,
                        &late_bindings,
                        &input_measurements.late_rise,
                        &late_loads,
                        ArrivalReduction::Latest,
                    )?,
                    late_fall: evaluate_lane(
                        cover,
                        &scenario_catalog.late,
                        &late_bindings,
                        &input_measurements.late_fall,
                        &late_loads,
                        ArrivalReduction::Latest,
                    )?,
                },
            ));
        }
        let input_activities =
            boundary_input_activities(contracts, &boundary_index.contracts_by_input, scenario.id());
        let activity = evaluate_activity(cover, &late_bindings, &input_activities)?;
        if let (Some(activity), Some(coefficient)) = (
            &activity,
            scenario.power().library().units.dynamic_power_watts(),
        ) {
            let mut switched_capacitance = activity.switched_capacitance;
            for (contract_index, contract) in contracts.iter().enumerate() {
                let Some(output_activity) = maximum_output_activity(
                    activity,
                    boundary_index.outputs_by_contract.row(contract_index),
                ) else {
                    continue;
                };
                let load = contract
                    .rows()
                    .iter()
                    .filter(|row| row.scenario == scenario.id())
                    .filter_map(|row| row.output)
                    .flat_map(|output| [output.capacitance.early, output.capacitance.late])
                    .flatten()
                    .map(crate::FiniteValue::get)
                    .max_by(f64::total_cmp)
                    .unwrap_or(0.0);
                switched_capacitance += output_activity.toggle_rate() * load;
            }
            dynamic_power.push(switched_capacitance * coefficient);
        }
        for (contract_index, contract) in contracts.iter().enumerate() {
            for contract_row in contract
                .rows()
                .iter()
                .filter(|row| row.scenario == scenario.id())
            {
                let evaluation = timing_evaluations
                    .binary_search_by_key(&contract_row.timing_tag, |(tag, _)| *tag)
                    .ok()
                    .map(|index| &timing_evaluations[index].1)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional response has no exact scenario/tag evaluation",
                        )
                    })?;
                let arrival = if contract_row.output.is_some() {
                    EarlyLate::new(
                        RiseFall::new(
                            reduced_output(
                                &evaluation.early_rise,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.arrival,
                                ArrivalReduction::Earliest,
                            )?,
                            reduced_output(
                                &evaluation.early_fall,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.arrival,
                                ArrivalReduction::Earliest,
                            )?,
                        ),
                        RiseFall::new(
                            reduced_output(
                                &evaluation.late_rise,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.arrival,
                                ArrivalReduction::Latest,
                            )?,
                            reduced_output(
                                &evaluation.late_fall,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.arrival,
                                ArrivalReduction::Latest,
                            )?,
                        ),
                    )
                } else {
                    EarlyLate::new(RiseFall::new(None, None), RiseFall::new(None, None))
                };
                let transition = if contract_row.output.is_some() {
                    EarlyLate::new(
                        RiseFall::new(
                            reduced_output(
                                &evaluation.early_rise,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.transition,
                                ArrivalReduction::Latest,
                            )?,
                            reduced_output(
                                &evaluation.early_fall,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.transition,
                                ArrivalReduction::Latest,
                            )?,
                        ),
                        RiseFall::new(
                            reduced_output(
                                &evaluation.late_rise,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.transition,
                                ArrivalReduction::Latest,
                            )?,
                            reduced_output(
                                &evaluation.late_fall,
                                boundary_index.outputs_by_contract.row(contract_index),
                                |row| row.transition,
                                ArrivalReduction::Latest,
                            )?,
                        ),
                    )
                } else {
                    EarlyLate::new(RiseFall::new(None, None), RiseFall::new(None, None))
                };
                let input_capacitance = if contract_row.input.is_some() {
                    EarlyLate::new(
                        maximum_input_capacitance(
                            &evaluation.early_rise,
                            boundary_index.inputs_by_contract.row(contract_index),
                        )?,
                        maximum_input_capacitance(
                            &evaluation.late_rise,
                            boundary_index.inputs_by_contract.row(contract_index),
                        )?,
                    )
                } else {
                    EarlyLate::new(None, None)
                };
                let activity = contract_row.output.and_then(|_| {
                    activity.as_ref().and_then(|activity| {
                        maximum_output_activity(
                            activity,
                            boundary_index.outputs_by_contract.row(contract_index),
                        )
                    })
                });
                rows[contract_index].push(BoundaryResponseRow {
                    scenario: scenario.id(),
                    timing_tag: contract_row.timing_tag,
                    arrival,
                    transition,
                    input_capacitance,
                    activity,
                });
            }
        }
    }
    for (response, rows) in responses.iter_mut().zip(rows) {
        response.rows = rows.into_boxed_slice();
    }
    Ok(MeasuredCoverResponse {
        boundaries: responses,
        dynamic_power: (dynamic_power.len() == models.scenarios.scenarios().len())
            .then(|| dynamic_power.into_iter().max_by(f64::total_cmp))
            .flatten(),
    })
}

pub(crate) fn score_boundaries(
    contracts: &[BoundaryContract],
    responses: &[BoundaryResponse],
    timing_tags: &crate::TimingTagInterner,
) -> Result<BoundaryScore, crate::SynthError> {
    let mut worst_normalized_violation = 0.0f64;
    let mut minimum_slack = None::<f64>;
    let mut total_negative_slack = 0.0f64;
    for contract in contracts {
        if contract.port().direction() != crate::RegionPortDirection::Output {
            continue;
        }
        let response = responses
            .iter()
            .find(|response| response.port_semantic_key == contract.port().semantic_key())
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional output contract has no measured boundary response",
                )
            })?;
        for contract_row in contract.rows() {
            let output = contract_row.output.ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional output contract row has no output constraints",
                )
            })?;
            let measured = response
                .rows
                .iter()
                .find(|row| {
                    row.scenario == contract_row.scenario
                        && row.timing_tag == contract_row.timing_tag
                })
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "regional output contract row has no exact measured response",
                    )
                })?;
            let check = timing_tags
                .get(contract_row.timing_tag)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "regional boundary score references an unknown timing tag",
                    )
                })?
                .check;
            match check {
                crate::BoundaryCheckKind::Setup | crate::BoundaryCheckKind::Recovery => {
                    for (required, arrival) in [
                        (output.required.late.rise, measured.arrival.late.rise),
                        (output.required.late.fall, measured.arrival.late.fall),
                    ] {
                        score_path_lane(
                            required,
                            arrival,
                            false,
                            &mut worst_normalized_violation,
                            &mut minimum_slack,
                            &mut total_negative_slack,
                        );
                    }
                }
                crate::BoundaryCheckKind::Hold | crate::BoundaryCheckKind::Removal => {
                    for (required, arrival) in [
                        (output.required.early.rise, measured.arrival.early.rise),
                        (output.required.early.fall, measured.arrival.early.fall),
                    ] {
                        score_path_lane(
                            required,
                            arrival,
                            true,
                            &mut worst_normalized_violation,
                            &mut minimum_slack,
                            &mut total_negative_slack,
                        );
                    }
                }
                crate::BoundaryCheckKind::MaxTransition => {
                    for (limit, actual) in [
                        (
                            output.maximum_transition.rise,
                            maximum_finite([
                                measured.transition.early.rise,
                                measured.transition.late.rise,
                            ]),
                        ),
                        (
                            output.maximum_transition.fall,
                            maximum_finite([
                                measured.transition.early.fall,
                                measured.transition.late.fall,
                            ]),
                        ),
                    ] {
                        score_upper_limit(limit, actual, &mut worst_normalized_violation)?;
                    }
                }
                crate::BoundaryCheckKind::MaxCapacitance => {
                    for (limit, actual) in [
                        (
                            output.maximum_capacitance.rise,
                            maximum_finite([output.capacitance.early, output.capacitance.late]),
                        ),
                        (
                            output.maximum_capacitance.fall,
                            maximum_finite([output.capacitance.early, output.capacitance.late]),
                        ),
                    ] {
                        score_upper_limit(limit, actual, &mut worst_normalized_violation)?;
                    }
                }
                crate::BoundaryCheckKind::MaxFanout => {
                    score_upper_limit(
                        output.maximum_fanout,
                        maximum_finite([output.fanout_load.early, output.fanout_load.late]),
                        &mut worst_normalized_violation,
                    )?;
                }
                crate::BoundaryCheckKind::PulseWidth => {
                    // Pulse width is a local clock-pin relation. It is
                    // evaluated by STA rather than inferred from a
                    // combinational region's data-boundary response.
                }
            }
        }
    }
    Ok(BoundaryScore {
        worst_normalized_violation,
        minimum_slack: minimum_slack.unwrap_or(0.0),
        total_negative_slack,
    })
}

fn score_path_lane(
    required: Option<crate::FiniteValue>,
    arrival: Option<crate::FiniteValue>,
    lower_bound: bool,
    worst_normalized_violation: &mut f64,
    minimum_slack: &mut Option<f64>,
    total_negative_slack: &mut f64,
) {
    let Some(required) = required else {
        return;
    };
    let Some(arrival) = arrival else {
        // A sparse scenario/tag row can be inactive for this exact regional
        // cone. It contributes no local path score; global STA remains the
        // authority after direct mapped-region commit.
        return;
    };
    let required = required.get();
    let slack = if lower_bound {
        arrival.get() - required
    } else {
        required - arrival.get()
    };
    *minimum_slack = Some(minimum_slack.map_or(slack, |current| current.min(slack)));
    if slack < 0.0 {
        *total_negative_slack += -slack;
        *worst_normalized_violation =
            (*worst_normalized_violation).max((-slack) / required.abs().max(f64::EPSILON));
    }
}

fn score_upper_limit(
    limit: Option<crate::FiniteValue>,
    actual: Option<crate::FiniteValue>,
    worst_normalized_violation: &mut f64,
) -> Result<(), crate::SynthError> {
    let Some(limit) = limit else { return Ok(()) };
    let actual = actual.ok_or_else(|| {
        crate::SynthError::invariant("constrained regional electrical lane has no measured value")
    })?;
    let violation = actual.get() - limit.get();
    if violation > 0.0 {
        *worst_normalized_violation =
            (*worst_normalized_violation).max(violation / limit.get().abs().max(f64::EPSILON));
    }
    Ok(())
}

fn maximum_finite(
    values: impl IntoIterator<Item = Option<crate::FiniteValue>>,
) -> Option<crate::FiniteValue> {
    values.into_iter().flatten().max()
}

fn evaluate_lane(
    cover: &LibraryCover,
    catalog: &CombinationalCellCatalog,
    bindings: &[ScenarioBinding],
    inputs: &[Measurement],
    output_loads: &[f64],
    arrival_reduction: ArrivalReduction,
) -> Result<LaneEvaluation, crate::SynthError> {
    if output_loads.len() != cover.outputs.len() {
        return Err(crate::SynthError::invariant(
            "regional endpoint loads do not align with cover outputs",
        ));
    }
    if bindings.len() != cover.cells.len() {
        return Err(crate::SynthError::invariant(
            "scenario binding arena does not align with the regional cover",
        ));
    }
    let mut input_capacitances = vec![0.0f64; inputs.len()];
    let mut cell_output_loads = vec![[0.0f64; 2]; cover.cells.len()];
    for (&source, &load) in cover.outputs.iter().zip(output_loads) {
        add_source_load(
            source,
            load,
            &mut input_capacitances,
            &mut cell_output_loads,
        )?;
    }
    for (cell, binding) in cover.cells.iter().zip(bindings) {
        for (source_index, load) in binding.input_loads() {
            let source = cell.sources.get(source_index).copied().ok_or_else(|| {
                crate::SynthError::invariant("regional cover input load is outside its signature")
            })?;
            add_source_load(
                source,
                load,
                &mut input_capacitances,
                &mut cell_output_loads,
            )?;
        }
    }
    let mut cell_outputs = Vec::<[Option<Measurement>; 2]>::with_capacity(cover.cells.len());
    for (cell_index, (cell, binding)) in cover.cells.iter().zip(bindings).enumerate() {
        let sources = cell
            .sources
            .iter()
            .copied()
            .map(|source| source_measurement(source, inputs, &cell_outputs))
            .collect::<Result<Vec<_>, _>>()?;
        let input_arrival = reduce_measurements(&sources, |value| value.arrival, arrival_reduction);
        let signature_transitions = sources
            .iter()
            .map(|value| value.transition)
            .collect::<Vec<_>>();
        let costs = binding.estimate(
            catalog,
            &signature_transitions,
            cell_output_loads[cell_index],
        );
        let first = Measurement {
            active: input_arrival.is_some(),
            arrival: input_arrival.unwrap_or(0.0) + costs[0].delay,
            transition: costs[0].transition,
        };
        let second = cell.second_node.is_some().then(|| Measurement {
            active: input_arrival.is_some(),
            arrival: input_arrival.unwrap_or(0.0) + costs[1].delay,
            transition: costs[1].transition,
        });
        cell_outputs.push([Some(first), second]);
    }
    let outputs = cover
        .outputs
        .iter()
        .copied()
        .map(|source| source_measurement(source, inputs, &cell_outputs))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    Ok(LaneEvaluation {
        outputs,
        input_capacitances: input_capacitances.into_boxed_slice(),
    })
}

#[derive(Debug, Clone)]
pub(crate) enum ScenarioBinding {
    Single {
        binding: CellBinding,
        input_loads: Vec<(usize, f64)>,
    },
    Joint {
        binding: JointCellBinding,
        input_loads: Vec<(usize, f64)>,
    },
}

impl ScenarioBinding {
    pub(crate) fn input_loads(&self) -> impl Iterator<Item = (usize, f64)> + '_ {
        match self {
            Self::Single { input_loads, .. } | Self::Joint { input_loads, .. } => {
                input_loads.iter().copied()
            }
        }
    }

    fn estimate(
        &self,
        catalog: &CombinationalCellCatalog,
        signature_transitions: &[f64],
        output_loads: [f64; 2],
    ) -> [CellCost; 2] {
        match self {
            Self::Single { binding, .. } => {
                let cost =
                    catalog.estimate_binding(*binding, signature_transitions, output_loads[0]);
                [cost, cost]
            }
            Self::Joint { binding, .. } => {
                catalog.estimate_joint_outputs(*binding, signature_transitions, output_loads)
            }
        }
    }
}

fn scenario_bindings(
    cover: &LibraryCover,
    catalog: &CombinationalCellCatalog,
) -> Result<Vec<ScenarioBinding>, crate::SynthError> {
    cover
        .cells
        .iter()
        .map(|cell| scenario_binding(catalog, cell))
        .collect()
}

fn scenario_binding(
    catalog: &CombinationalCellCatalog,
    cell: &LibraryCoverCell,
) -> Result<ScenarioBinding, crate::SynthError> {
    match cell.binding {
        LibraryCoverBinding::Single(_) => {
            let binding = catalog
                .binding_for_identity(cell.truth, &cell.binding_identity)
                .ok_or_else(|| {
                    crate::SynthError::mapping(
                        "regional cover binding is absent from an active scenario library",
                    )
                })?;
            Ok(ScenarioBinding::Single {
                binding,
                input_loads: catalog.binding_input_loads(binding),
            })
        }
        LibraryCoverBinding::Joint(_) => {
            let second = cell.second_truth.ok_or_else(|| {
                crate::SynthError::invariant("joint regional cover has no secondary truth")
            })?;
            let binding = catalog
                .joint_binding_for_identity((cell.truth, second), &cell.binding_identity)
                .ok_or_else(|| {
                    crate::SynthError::mapping(
                        "regional joint cover binding is absent from an active scenario library",
                    )
                })?;
            Ok(ScenarioBinding::Joint {
                binding,
                input_loads: catalog.joint_input_loads(binding),
            })
        }
    }
}

fn add_source_load(
    source: LibraryCoverSource,
    load: f64,
    input_capacitances: &mut [f64],
    cell_output_loads: &mut [[f64; 2]],
) -> Result<(), crate::SynthError> {
    if load.is_nan() || load < 0.0 {
        return Err(crate::SynthError::invariant(
            "regional cover load is NaN or negative",
        ));
    }
    match source {
        LibraryCoverSource::Constant(_) => {}
        LibraryCoverSource::Input(index) => {
            let target = input_capacitances.get_mut(index).ok_or_else(|| {
                crate::SynthError::invariant("regional cover load references an unknown input")
            })?;
            *target += load;
        }
        LibraryCoverSource::Cell(index) => {
            let target = cell_output_loads.get_mut(index).ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional cover load references an unknown cell output",
                )
            })?;
            target[0] += load;
        }
        LibraryCoverSource::CellSecond(index) => {
            let target = cell_output_loads.get_mut(index).ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional cover load references an unknown secondary cell output",
                )
            })?;
            target[1] += load;
        }
    }
    Ok(())
}

fn reduce_measurements(
    values: &[Measurement],
    select: impl Fn(Measurement) -> f64,
    reduction: ArrivalReduction,
) -> Option<f64> {
    let values = values
        .iter()
        .copied()
        .filter(|value| value.active)
        .map(select);
    match reduction {
        ArrivalReduction::Earliest => values.min_by(f64::total_cmp),
        ArrivalReduction::Latest => values.max_by(f64::total_cmp),
    }
}

fn source_measurement(
    source: LibraryCoverSource,
    inputs: &[Measurement],
    cells: &[[Option<Measurement>; 2]],
) -> Result<Measurement, crate::SynthError> {
    match source {
        LibraryCoverSource::Constant(_) => Ok(Measurement {
            active: true,
            arrival: 0.0,
            transition: 0.0,
        }),
        LibraryCoverSource::Input(index) => inputs.get(index).copied().ok_or_else(|| {
            crate::SynthError::invariant("cover references an unknown regional input")
        }),
        LibraryCoverSource::Cell(index) => cells
            .get(index)
            .and_then(|outputs| outputs[0])
            .ok_or_else(|| crate::SynthError::invariant("cover references an unknown local cell")),
        LibraryCoverSource::CellSecond(index) => cells
            .get(index)
            .and_then(|outputs| outputs[1])
            .ok_or_else(|| {
                crate::SynthError::invariant("cover references an unknown secondary cell output")
            }),
    }
}

fn optional_finite(value: Option<f64>) -> Result<Option<crate::FiniteValue>, crate::SynthError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_nan() {
        return Err(crate::SynthError::invariant(
            "regional boundary response is NaN",
        ));
    }
    if value.is_infinite() {
        return Ok(None);
    }
    crate::FiniteValue::new(value)
        .map(Some)
        .map_err(|error| crate::SynthError::invariant(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_timing_tag_reduction_ignores_inactive_inputs() {
        let measurements = [
            Measurement {
                active: false,
                arrival: -100.0,
                transition: 0.0,
            },
            Measurement {
                active: true,
                arrival: 5.0,
                transition: 0.0,
            },
        ];

        assert_eq!(
            reduce_measurements(
                &measurements,
                |measurement| measurement.arrival,
                ArrivalReduction::Earliest,
            ),
            Some(5.0)
        );
        assert_eq!(
            reduce_measurements(
                &measurements,
                |measurement| measurement.arrival,
                ArrivalReduction::Latest,
            ),
            Some(5.0)
        );
        assert_eq!(
            reduce_measurements(
                &measurements[..1],
                |measurement| measurement.arrival,
                ArrivalReduction::Earliest,
            ),
            None
        );
    }
}
