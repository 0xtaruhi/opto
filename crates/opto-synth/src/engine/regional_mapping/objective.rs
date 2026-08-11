// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Ordered global objective and retained ownership for regional epochs.

pub(super) struct BestMapping {
    pub(super) objective: MappedObjective,
    pub(super) plans: Box<[crate::RegionCoverPlan]>,
    pub(super) bindings: Box<[crate::mapping::RegionPlanBinding]>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MappedObjective {
    legal: bool,
    timing_violations: usize,
    worst_normalized_violation: crate::FiniteValue,
    minimum_slack: crate::FiniteValue,
    total_negative_slack: crate::FiniteValue,
    pub(super) area: crate::FiniteValue,
    leakage_power: Option<crate::FiniteValue>,
    dynamic_power: Option<crate::FiniteValue>,
    cell_count: u64,
    stable_key: [u8; 32],
}

impl MappedObjective {
    pub(super) fn from_plans(
        plans: &[crate::RegionCoverPlan],
        global_dynamic_power: Option<f64>,
        implementation_area: f64,
        implementation_leakage: Option<f64>,
        implementation_cell_count: u64,
        static_implementation_key: [u8; 32],
        timing_quality: Option<opto_timing::TimingQualitySummary>,
    ) -> Result<Self, crate::SynthError> {
        let costs = plans
            .iter()
            .map(crate::RegionCoverPlan::cost)
            .collect::<Vec<_>>();
        let finite = |value| {
            crate::FiniteValue::new(value)
                .map_err(|error| crate::SynthError::invariant(error.to_string()))
        };
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/regional/mapped-objective/v2\0");
        for cost in &costs {
            digest.update(&cost.stable_plan_key);
        }
        digest.update(&static_implementation_key);
        let projected_minimum_slack = costs
            .iter()
            .map(|cost| cost.minimum_slack.get())
            .min_by(f64::total_cmp)
            .unwrap_or(0.0);
        let projected_total_negative_slack =
            saturated_sum(costs.iter().map(|cost| cost.total_negative_slack.get()));
        let (timing_violations, minimum_slack, total_negative_slack) = timing_quality.map_or_else(
            || (0, projected_minimum_slack, projected_total_negative_slack),
            |quality| {
                (
                    quality.violating_paths(),
                    quality.wns().unwrap_or(0.0),
                    -quality.tns(),
                )
            },
        );
        Ok(Self {
            legal: costs.iter().all(|cost| cost.legal),
            timing_violations,
            worst_normalized_violation: finite(
                costs
                    .iter()
                    .map(|cost| cost.worst_normalized_violation.get())
                    .max_by(f64::total_cmp)
                    .unwrap_or(0.0),
            )?,
            minimum_slack: finite(minimum_slack)?,
            total_negative_slack: finite(total_negative_slack)?,
            area: finite(implementation_area)?,
            leakage_power: implementation_leakage
                .or_else(|| complete_optional_sum(costs.iter().map(|cost| cost.leakage_power)))
                .map(finite)
                .transpose()?,
            dynamic_power: global_dynamic_power
                .or_else(|| complete_optional_sum(costs.iter().map(|cost| cost.dynamic_power)))
                .map(finite)
                .transpose()?,
            cell_count: implementation_cell_count,
            stable_key: *digest.finalize().as_bytes(),
        })
    }

    pub(super) fn better_than(&self, other: &Self) -> bool {
        other
            .legal
            .cmp(&self.legal)
            .then_with(|| self.timing_violations.cmp(&other.timing_violations))
            .then_with(|| self.total_negative_slack.cmp(&other.total_negative_slack))
            .then_with(|| other.minimum_slack.cmp(&self.minimum_slack))
            .then_with(|| {
                self.worst_normalized_violation
                    .cmp(&other.worst_normalized_violation)
            })
            .then_with(|| self.area.cmp(&other.area))
            .then_with(|| compare_optional_finite(self.leakage_power, other.leakage_power))
            .then_with(|| compare_optional_finite(self.dynamic_power, other.dynamic_power))
            .then_with(|| self.cell_count.cmp(&other.cell_count))
            .then_with(|| self.stable_key.cmp(&other.stable_key))
            .is_lt()
    }
}

fn saturated_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    values
        .into_iter()
        .fold(0.0, |total, value| (total + value).min(f64::MAX))
}

fn complete_optional_sum(
    values: impl IntoIterator<Item = Option<crate::FiniteValue>>,
) -> Option<f64> {
    values.into_iter().try_fold(0.0, |total, value| {
        value.map(|value| (total + value.get()).min(f64::MAX))
    })
}

fn compare_optional_finite(
    left: Option<crate::FiniteValue>,
    right: Option<crate::FiniteValue>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => std::cmp::Ordering::Equal,
    }
}
