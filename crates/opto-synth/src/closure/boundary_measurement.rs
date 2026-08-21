// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Authoritative MMMC boundary measurement on one mapped generation.

use crate::regional::boundary::{Corner, worst_upper_bound};
use crate::{BoundaryResponse, BoundaryResponseRow, EarlyLate, FiniteValue, RiseFall};
use opto_ir::mapped::NetId;
use opto_timing::{DelayType, IncrementalTiming, NetTimingState};
use std::collections::BTreeMap;

pub(crate) struct BoundaryNetObservation {
    pub(crate) semantic_key: [u8; 32],
    pub(crate) nets: Box<[Option<NetId>]>,
}

#[derive(Clone, Copy)]
pub(crate) struct GlobalBoundaryRequest<'a> {
    pub(crate) timing: &'a crate::closure::mmmc::MmmcTiming,
    pub(crate) plans: &'a [crate::RegionCoverPlan],
    pub(crate) observations: &'a [BoundaryNetObservation],
    pub(crate) scenarios: &'a opto_timing::ScenarioSet,
    /// Resolves each contract row's tag to the check it constrains, so a
    /// measurement is projected onto the same lanes as its requirement.
    pub(crate) timing_tags: &'a crate::TimingTagInterner,
    pub(crate) power_evaluator: &'a dyn crate::SynthesisPowerEvaluator,
}

pub(crate) fn measure_global_boundaries(
    request: GlobalBoundaryRequest<'_>,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<(Vec<crate::RegionCoverPlan>, Option<f64>), crate::SynthError> {
    let GlobalBoundaryRequest {
        timing,
        plans,
        observations,
        scenarios,
        timing_tags,
        power_evaluator,
    } = request;
    let observations = observations
        .iter()
        .map(|observation| (observation.semantic_key, observation.nets.as_ref()))
        .collect::<BTreeMap<_, _>>();
    let mut measurements = BTreeMap::new();
    let mut dynamic_powers = Vec::with_capacity(scenarios.scenarios().len());
    for scenario in scenarios.scenarios() {
        let views = timing.scenario_views(scenario.id())?;
        // Each corner is enabled on its own. An uncharacterized early library
        // must not discard the late view, and neither must discard power.
        let early = resolve_view(scenarios, scenario.id(), DelayType::Min, views.early)?;
        let late = resolve_view(scenarios, scenario.id(), DelayType::Max, views.late)?;
        let Some(power_view) = late.or(early) else {
            dynamic_powers.push(None);
            continue;
        };
        let dynamic_power = power_evaluator
            .dynamic_power_watts(runtime, scenario, power_view.model(), &|| {
                power_view
                    .electrical_snapshot()
                    .map_err(|error| error.to_string())
            })
            .map_err(crate::SynthError::Power)?;
        dynamic_powers.push(validated_dynamic_power(
            dynamic_power,
            "global regional power evaluation",
        )?);
        for (&key, nets) in &observations {
            let early_samples = corner_samples(early, nets);
            let late_samples = corner_samples(late, nets);
            measurements.insert(
                (key, scenario.id()),
                GlobalBoundaryMeasurement::new(&early_samples, &late_samples)?,
            );
        }
    }
    let dynamic_power = complete_dynamic_power(&dynamic_powers);
    if measurements.is_empty() {
        return Ok((plans.to_vec(), dynamic_power));
    }
    let plans = plans
        .iter()
        .map(|plan| {
            let responses = plan
                .boundary_response()
                .iter()
                .map(|contract| {
                    let key = contract.port().semantic_key();
                    let prior = plan
                        .measured_response()
                        .iter()
                        .find(|response| response.port_semantic_key == key);
                    let rows = contract
                        .rows()
                        .iter()
                        .map(|row| {
                            let prior_row = prior.and_then(|response| {
                                response.rows.iter().find(|candidate| {
                                    candidate.scenario == row.scenario
                                        && candidate.timing_tag == row.timing_tag
                                })
                            });
                            let Some(measured) = measurements.get(&(key, row.scenario)) else {
                                return Ok(prior_row
                                    .copied()
                                    .unwrap_or_else(|| missing_measurement_row(row)));
                            };
                            let check = timing_tags
                                .get(row.timing_tag)
                                .ok_or_else(|| {
                                    crate::SynthError::invariant(
                                        "measured boundary row references an unknown timing tag",
                                    )
                                })?
                                .check;
                            Ok(measured.response_row(
                                row,
                                check,
                                prior_row.and_then(|row| row.activity),
                            ))
                        })
                        .collect::<Result<Vec<_>, crate::SynthError>>()?;
                    Ok(BoundaryResponse {
                        port_semantic_key: key,
                        rows: rows.into_boxed_slice(),
                    })
                })
                .collect::<Result<Vec<_>, crate::SynthError>>()?;
            let cost = measured_global_cost(plan, &responses)?;
            Ok(plan
                .clone()
                .with_measured_response(responses)
                .with_cost(cost))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    Ok((plans, dynamic_power))
}

fn resolve_view<'a>(
    scenarios: &opto_timing::ScenarioSet,
    scenario: opto_timing::ScenarioId,
    delay_type: DelayType,
    view: Option<crate::closure::mmmc::MmmcViewRef<'a>>,
) -> Result<Option<&'a IncrementalTiming>, crate::SynthError> {
    let expected = scenarios
        .analysis_view_id(scenario, delay_type)
        .ok_or_else(|| crate::SynthError::invariant("scenario has no canonical view"))?;
    match view {
        Some(view) if view.id != expected => Err(crate::SynthError::invariant(
            "MMMC boundary measurement received a mismatched analysis view",
        )),
        Some(view) => Ok(Some(view.timing)),
        None => Ok(None),
    }
}

/// Collects the net states one boundary port occupies in a single corner.
///
/// An absent view yields no samples, which the measurement reports as an absent
/// value rather than as a value borrowed from the other corner.
fn corner_samples(view: Option<&IncrementalTiming>, nets: &[Option<NetId>]) -> Vec<NetTimingState> {
    let Some(view) = view else {
        return Vec::new();
    };
    nets.iter()
        .flatten()
        .filter_map(|&net| view.mapped_net_state(net))
        .collect()
}

/// Rejects a power value an injected evaluator cannot legally return.
pub(crate) fn validated_dynamic_power(
    watts: Option<f64>,
    what: &str,
) -> Result<Option<f64>, crate::SynthError> {
    if watts.is_some_and(|watts| !watts.is_finite() || watts < 0.0) {
        return Err(crate::SynthError::Power(format!(
            "{what} returned an invalid value {watts:?}"
        )));
    }
    Ok(watts)
}

fn complete_dynamic_power(powers: &[Option<f64>]) -> Option<f64> {
    if powers.is_empty() {
        return None;
    }
    powers
        .iter()
        .copied()
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max_by(f64::total_cmp)
}

fn measured_global_cost(
    plan: &crate::RegionCoverPlan,
    responses: &[BoundaryResponse],
) -> Result<crate::RegionPlanCost, crate::SynthError> {
    let mut closure = ClosureCost::default();
    for contract in plan.boundary_response() {
        let response = responses
            .iter()
            .find(|response| response.port_semantic_key == contract.port().semantic_key());
        for row in contract.rows() {
            let measured = response.and_then(|response| {
                response.rows.iter().find(|measured| {
                    measured.scenario == row.scenario && measured.timing_tag == row.timing_tag
                })
            });
            let Some(measured) = measured else {
                closure.missing();
                continue;
            };
            if let Some(input) = row.input {
                for (actual, expected) in [
                    (measured.arrival.early.rise, input.arrival.early.rise),
                    (measured.arrival.early.fall, input.arrival.early.fall),
                    (measured.arrival.late.rise, input.arrival.late.rise),
                    (measured.arrival.late.fall, input.arrival.late.fall),
                    (measured.transition.early.rise, input.transition.early.rise),
                    (measured.transition.early.fall, input.transition.early.fall),
                    (measured.transition.late.rise, input.transition.late.rise),
                    (measured.transition.late.fall, input.transition.late.fall),
                ] {
                    closure.match_contract(actual, expected);
                }
            }
            if let Some(output) = row.output {
                for (actual, required) in [
                    (measured.arrival.early.rise, output.required.early.rise),
                    (measured.arrival.early.fall, output.required.early.fall),
                ] {
                    closure.lower_bound(actual, required);
                }
                for (actual, required) in [
                    (measured.arrival.late.rise, output.required.late.rise),
                    (measured.arrival.late.fall, output.required.late.fall),
                    (
                        measured.transition.early.rise,
                        output.maximum_transition.rise,
                    ),
                    (
                        measured.transition.early.fall,
                        output.maximum_transition.fall,
                    ),
                    (
                        measured.transition.late.rise,
                        output.maximum_transition.rise,
                    ),
                    (
                        measured.transition.late.fall,
                        output.maximum_transition.fall,
                    ),
                    (
                        measured.input_capacitance.early,
                        output.maximum_capacitance.rise,
                    ),
                    (
                        measured.input_capacitance.early,
                        output.maximum_capacitance.fall,
                    ),
                    (
                        measured.input_capacitance.late,
                        output.maximum_capacitance.rise,
                    ),
                    (
                        measured.input_capacitance.late,
                        output.maximum_capacitance.fall,
                    ),
                ] {
                    closure.upper_bound(actual, required);
                }
            }
        }
    }
    let mut cost = plan.cost();
    cost.legal = closure.complete;
    cost.worst_normalized_violation = finite(Some(closure.worst))?
        .ok_or_else(|| crate::SynthError::invariant("global regional violation is not finite"))?;
    cost.minimum_slack = finite(Some(closure.minimum_slack.unwrap_or(0.0)))?.ok_or_else(|| {
        crate::SynthError::invariant("global regional minimum slack is not finite")
    })?;
    cost.total_negative_slack = finite(Some(closure.total_negative_slack))?.ok_or_else(|| {
        crate::SynthError::invariant("global regional negative slack is not finite")
    })?;
    Ok(cost)
}

#[derive(Debug)]
struct ClosureCost {
    worst: f64,
    minimum_slack: Option<f64>,
    total_negative_slack: f64,
    /// Whether every contract lane this plan claims had a measurement to
    /// compare against. A plan that could not be measured is not legal, which
    /// is a different statement from a plan that was measured and violates.
    complete: bool,
}

impl Default for ClosureCost {
    fn default() -> Self {
        Self {
            worst: 0.0,
            minimum_slack: None,
            total_negative_slack: 0.0,
            complete: true,
        }
    }
}

impl ClosureCost {
    fn missing(&mut self) {
        self.complete = false;
        self.record(-f64::MAX, f64::MAX);
    }

    fn match_contract(&mut self, actual: Option<FiniteValue>, expected: Option<FiniteValue>) {
        match (actual, expected) {
            (Some(actual), Some(expected)) => {
                let difference = (actual.get() - expected.get()).abs().min(f64::MAX);
                self.record(
                    -difference,
                    difference / expected.get().abs().max(f64::EPSILON),
                );
            }
            (None, None) => {}
            _ => self.missing(),
        }
    }

    fn lower_bound(&mut self, actual: Option<FiniteValue>, limit: Option<FiniteValue>) {
        match (actual, limit) {
            (_, None) => {}
            (Some(actual), Some(limit)) => {
                let slack = finite_difference(actual.get(), limit.get());
                self.record(
                    slack,
                    (-slack).max(0.0) / limit.get().abs().max(f64::EPSILON),
                );
            }
            (None, Some(_)) => self.missing(),
        }
    }

    fn upper_bound(&mut self, actual: Option<FiniteValue>, limit: Option<FiniteValue>) {
        match (actual, limit) {
            (_, None) => {}
            (Some(actual), Some(limit)) => {
                let slack = finite_difference(limit.get(), actual.get());
                self.record(
                    slack,
                    (-slack).max(0.0) / limit.get().abs().max(f64::EPSILON),
                );
            }
            (None, Some(_)) => self.missing(),
        }
    }

    fn record(&mut self, slack: f64, normalized_violation: f64) {
        self.worst = self.worst.max(normalized_violation.min(f64::MAX));
        self.minimum_slack = Some(
            self.minimum_slack
                .map_or(slack, |current| current.min(slack)),
        );
        if slack < 0.0 {
            self.total_negative_slack =
                (self.total_negative_slack + (-slack).min(f64::MAX)).min(f64::MAX);
        }
    }
}

fn finite_difference(left: f64, right: f64) -> f64 {
    let difference = left - right;
    if difference.is_finite() {
        difference
    } else if left >= right {
        f64::MAX
    } else {
        -f64::MAX
    }
}

fn missing_measurement_row(row: &crate::BoundaryContractRow) -> BoundaryResponseRow {
    let empty_lane = RiseFall::new(None, None);
    let (arrival, transition, activity) = row.input.map_or_else(
        || {
            (
                EarlyLate::new(empty_lane, empty_lane),
                EarlyLate::new(empty_lane, empty_lane),
                None,
            )
        },
        |input| (input.arrival, input.transition, input.activity),
    );
    BoundaryResponseRow {
        scenario: row.scenario,
        timing_tag: row.timing_tag,
        arrival,
        transition,
        input_capacitance: EarlyLate::new(None, None),
        activity,
    }
}

#[derive(Debug, Clone, Copy)]
/// One boundary port's measured state in both MMMC corners.
///
/// The net-level timing state carries no rise/fall or per-tag decomposition, so
/// this holds one scalar per corner and projects it onto the lanes each tag's
/// check constrains.
struct GlobalBoundaryMeasurement {
    early_arrival: Option<FiniteValue>,
    late_arrival: Option<FiniteValue>,
    early_transition: Option<FiniteValue>,
    late_transition: Option<FiniteValue>,
    early_capacitance: Option<FiniteValue>,
    late_capacitance: Option<FiniteValue>,
}

impl GlobalBoundaryMeasurement {
    fn new(early: &[NetTimingState], late: &[NetTimingState]) -> Result<Self, crate::SynthError> {
        let arrival = |corner: Corner, states: &[NetTimingState]| {
            corner.worst_arrival(states.iter().filter_map(|state| state.arrival))
        };
        let transition = |states: &[NetTimingState]| {
            worst_upper_bound(states.iter().filter_map(|state| state.transition))
        };
        let capacitance = |states: &[NetTimingState]| {
            worst_upper_bound(states.iter().map(|state| state.capacitance))
        };
        Ok(Self {
            early_arrival: finite(arrival(Corner::Early, early))?,
            late_arrival: finite(arrival(Corner::Late, late))?,
            early_transition: finite(transition(early))?,
            late_transition: finite(transition(late))?,
            early_capacitance: finite(capacitance(early))?,
            late_capacitance: finite(capacitance(late))?,
        })
    }

    /// Projects this measurement onto the lanes `check` constrains.
    ///
    /// The requirement side uses the same projection, so a measured lane is
    /// populated exactly when the contract lane it is compared against is.
    fn response_row(
        self,
        row: &crate::BoundaryContractRow,
        check: crate::BoundaryCheckKind,
        activity: Option<opto_timing::ScenarioSwitchingActivity>,
    ) -> BoundaryResponseRow {
        BoundaryResponseRow {
            scenario: row.scenario,
            timing_tag: row.timing_tag,
            arrival: crate::regional::boundary::path_timing_lane(
                check,
                self.early_arrival,
                self.late_arrival,
            ),
            transition: crate::regional::boundary::measured_transition_lane(
                check,
                self.early_transition,
                self.late_transition,
            ),
            input_capacitance: match check {
                crate::BoundaryCheckKind::MaxCapacitance => {
                    EarlyLate::new(self.early_capacitance, self.late_capacitance)
                }
                _ => EarlyLate::new(None, None),
            },
            activity,
        }
    }
}

fn finite(value: Option<f64>) -> Result<Option<FiniteValue>, crate::SynthError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_infinite() {
        return Ok(None);
    }
    FiniteValue::new(value).map(Some).map_err(|error| {
        crate::SynthError::invariant(format!(
            "global boundary measurement {value:?} is invalid: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the test verifies the exact finite sentinel used for a missing contract lane"
    )]
    fn measured_value_violates_an_absent_input_contract() {
        let mut closure = ClosureCost::default();
        closure.match_contract(Some(FiniteValue::new(1.0).unwrap()), None);

        assert_eq!(closure.worst, f64::MAX);
        assert_eq!(closure.minimum_slack, Some(-f64::MAX));
        assert_eq!(closure.total_negative_slack, f64::MAX);
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "the test verifies the exact finite sentinel used for a missing measured lane"
    )]
    fn absent_value_violates_a_present_input_contract() {
        let mut closure = ClosureCost::default();
        closure.match_contract(None, Some(FiniteValue::new(1.0).unwrap()));

        assert_eq!(closure.worst, f64::MAX);
        assert_eq!(closure.minimum_slack, Some(-f64::MAX));
        assert_eq!(closure.total_negative_slack, f64::MAX);
    }
}
