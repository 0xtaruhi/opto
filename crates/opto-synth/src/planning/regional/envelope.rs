// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Target- and scenario-aware regional cost breakpoints built before lowering.

use crate::SynthesisRegionGraph;
use crate::planning::mapping_policy::CellCost;
use crate::planning::provider::StructuralEstimate;

const SCORE_SCALE: f64 = 1_000_000.0;

#[derive(Debug, Clone, Copy)]
struct CostPoint {
    late_delay: f64,
    area: f64,
    wiring: f64,
    leakage: Option<f64>,
    dynamic: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct RegionCostEnvelope {
    delays: Box<[f64]>,
}

impl RegionCostEnvelope {
    pub(crate) fn estimated_delay(&self, scenario: usize) -> f64 {
        self.delays[scenario].max(0.0)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct StructuralTargetModel {
    scenarios: Box<[ScenarioScale]>,
}

#[derive(Debug, Clone)]
struct ScenarioScale {
    name: Box<str>,
    early_cells: opto_library::TargetCellSet,
    late_cells: opto_library::TargetCellSet,
    early: Option<CellCost>,
    late: Option<CellCost>,
    leakage_per_unit: Option<f64>,
    dynamic_per_unit: Option<f64>,
    budget: Option<f64>,
}

impl StructuralTargetModel {
    #[allow(
        clippy::cast_precision_loss,
        reason = "cell counts are averaged into approximate physical cost models"
    )]
    pub(crate) fn build(
        scenarios: &opto_timing::ScenarioSet,
        mut representative_cost: impl FnMut(&opto_library::TargetCellSet) -> Option<CellCost>,
    ) -> Self {
        let scenarios = scenarios
            .scenarios()
            .iter()
            .map(|scenario| {
                let characterized_early = representative_cost(&scenario.early_library().cells);
                let characterized_late = representative_cost(&scenario.late_library().cells);
                let early = characterized_early;
                let late = characterized_late;
                let leakage = scenario
                    .power()
                    .library()
                    .cells
                    .iter()
                    .filter_map(|cell| {
                        cell.cell_leakage_power.or_else(|| {
                            cell.leakage_power
                                .iter()
                                .map(|group| group.value)
                                .max_by(f64::total_cmp)
                        })
                    })
                    .collect::<Vec<_>>();
                let leakage_per_unit = (!leakage.is_empty())
                    .then(|| leakage.iter().sum::<f64>() / leakage.len() as f64);
                let dynamic_per_unit = scenario
                    .power()
                    .activity_fingerprint()
                    .and_then(|_| scenario.power().library().units.dynamic_power_watts());
                ScenarioScale {
                    name: scenario.name().into(),
                    early_cells: scenario.early_library().cells.clone(),
                    late_cells: scenario.late_library().cells.clone(),
                    early,
                    late,
                    leakage_per_unit,
                    dynamic_per_unit,
                    budget: scenario.constraints().minimum_synthesis_delay(),
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { scenarios }
    }

    pub(crate) fn score(
        &self,
        estimate: StructuralEstimate,
    ) -> Result<(u64, u64, u64), crate::SynthError> {
        self.score_by(estimate, |scenario| scenario.budget)
    }

    pub(crate) fn score_for_budget(
        &self,
        estimate: StructuralEstimate,
        budget: Option<f64>,
    ) -> Result<(u64, u64, u64), crate::SynthError> {
        self.score_by(estimate, |_| budget)
    }

    fn score_by(
        &self,
        estimate: StructuralEstimate,
        budget: impl Fn(&ScenarioScale) -> Option<f64>,
    ) -> Result<(u64, u64, u64), crate::SynthError> {
        let mut timing = 0.0f64;
        let mut physical = 0.0f64;
        for scenario in &self.scenarios {
            let point = point_for_estimate(estimate, scenario)?;
            timing = timing.max(budget(scenario).map_or(0.0, |budget| {
                ((point.late_delay - budget) / budget.max(f64::EPSILON)).max(0.0)
            }));
            let power = point.leakage.unwrap_or(0.0) + point.dynamic.unwrap_or(0.0);
            physical = physical.max(point.area + point.wiring + power);
        }
        Ok((
            quantize(timing),
            quantize(physical),
            u64::from(estimate.logic_depth),
        ))
    }

    pub(crate) fn score_macro(&self, area: f64, delay: f64) -> (u64, u64, u64) {
        let violation = self
            .scenarios
            .iter()
            .filter_map(|scenario| scenario.budget)
            .map(|budget| ((delay - budget) / budget.max(f64::EPSILON)).max(0.0))
            .fold(0.0f64, f64::max);
        (quantize(violation), quantize(area), quantize(delay))
    }

    pub(crate) fn has_characterized_logic_costs(&self) -> bool {
        self.scenarios
            .iter()
            .all(|scenario| scenario.early.is_some() && scenario.late.is_some())
    }

    pub(crate) fn characterized_macro_delay(&self, cell_name: &str) -> Option<f64> {
        let mut worst = None::<f64>;
        for scenario in &self.scenarios {
            for cells in [&scenario.early_cells, &scenario.late_cells] {
                let cell = cells.iter().find(|cell| cell.name() == cell_name)?;
                let delay = cell
                    .pins()
                    .filter(|pin| pin.direction() == opto_library::TargetPinDirection::Output)
                    .flat_map(opto_library::TargetPinRef::timing_arcs)
                    .filter_map(opto_library::TargetTimingArcRef::default_delay)
                    .filter(|delay| delay.is_finite() && *delay >= 0.0)
                    .max_by(f64::total_cmp)?;
                worst = Some(worst.map_or(delay, |current| current.max(delay)));
            }
        }
        worst
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RegionCostEnvelopeSet {
    rows: Box<[RegionCostEnvelope]>,
}

impl RegionCostEnvelopeSet {
    pub(crate) fn build(regions: &SynthesisRegionGraph, target: &StructuralTargetModel) -> Self {
        let rows = regions
            .regions()
            .iter()
            .copied()
            .map(|region| envelope_for_estimate(region.structural_estimate(), target))
            .collect::<Vec<_>>();
        Self {
            rows: rows.into_boxed_slice(),
        }
    }

    pub(crate) fn budget_weights(&self) -> Box<[Box<[f64]>]> {
        self.rows
            .iter()
            .map(|row| {
                (0..row.delays.len())
                    .map(|scenario| row.estimated_delay(scenario))
                    .collect()
            })
            .collect()
    }
}

fn envelope_for_estimate(
    estimate: StructuralEstimate,
    target: &StructuralTargetModel,
) -> RegionCostEnvelope {
    let depth = f64::from(estimate.logic_depth);
    let delays = target
        .scenarios
        .iter()
        .map(|scale| depth * scale.late.map_or(1.0, |cost| cost.delay))
        .collect();
    RegionCostEnvelope { delays }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "structural counts feed an intentionally approximate physical cost model"
)]
fn point_for_estimate(
    estimate: StructuralEstimate,
    scale: &ScenarioScale,
) -> Result<CostPoint, crate::SynthError> {
    if estimate.logic_depth == 0 {
        return Ok(CostPoint {
            late_delay: 0.0,
            area: 0.0,
            wiring: 0.0,
            leakage: Some(0.0),
            dynamic: Some(0.0),
        });
    }
    let late = scale
        .late
        .ok_or_else(|| missing_characterized_logic_cost(scale, "late"))?;
    let depth = f64::from(estimate.logic_depth);
    let logic = estimate.logic_units as f64;
    let wiring = estimate.wiring_units as f64;
    Ok(CostPoint {
        late_delay: depth * late.delay,
        area: logic * late.area,
        wiring: wiring * late.input_capacitance,
        leakage: scale.leakage_per_unit.map(|value| logic * value),
        dynamic: scale
            .dynamic_per_unit
            .map(|value| (logic + wiring) * late.input_capacitance * value),
    })
}

fn missing_characterized_logic_cost(scale: &ScenarioScale, view: &str) -> crate::SynthError {
    crate::SynthError::mapping(format!(
        "scenario '{}' has no characterized combinational cells in its {view} timing library",
        scale.name
    ))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "nonnegative finite scores are rounded after explicit saturation to the u64 range"
)]
fn quantize(value: f64) -> u64 {
    if !value.is_finite() || value < 0.0 {
        return u64::MAX;
    }
    let scaled = value * SCORE_SCALE;
    if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled.round() as u64
    }
}
