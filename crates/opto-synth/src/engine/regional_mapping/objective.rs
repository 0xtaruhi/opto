// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Ordered global objective and retained ownership for regional epochs.

use crate::closure::objective::{ClosureQuality, PhysicalObjective, compare_physical};

pub(super) struct BestMapping {
    pub(super) objective: MappedObjective,
    pub(super) plans: Box<[crate::RegionCoverPlan]>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MappedObjective {
    legal: bool,
    closure: Option<ClosureQuality>,
    projected_worst_normalized_violation: crate::FiniteValue,
    projected_minimum_slack: crate::FiniteValue,
    projected_total_negative_slack: crate::FiniteValue,
    physical: PhysicalObjective,
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
        closure: Option<ClosureQuality>,
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
        digest.update(b"opto/regional/mapped-objective/v3\0");
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
        let leakage = implementation_leakage
            .or_else(|| complete_optional_sum(costs.iter().map(|cost| cost.leakage_power)))
            .map(finite)
            .transpose()?
            .map(crate::FiniteValue::get);
        let dynamic = global_dynamic_power
            .or_else(|| complete_optional_sum(costs.iter().map(|cost| cost.dynamic_power)))
            .map(finite)
            .transpose()?
            .map(crate::FiniteValue::get);
        Ok(Self {
            legal: costs.iter().all(|cost| cost.legal),
            closure,
            projected_worst_normalized_violation: finite(
                costs
                    .iter()
                    .map(|cost| cost.worst_normalized_violation.get())
                    .max_by(f64::total_cmp)
                    .unwrap_or(0.0),
            )?,
            projected_minimum_slack: finite(projected_minimum_slack)?,
            projected_total_negative_slack: finite(projected_total_negative_slack)?,
            physical: PhysicalObjective {
                area: finite(implementation_area)?.get(),
                leakage,
                dynamic,
                cells: usize::try_from(implementation_cell_count).map_err(|_| {
                    crate::SynthError::invariant(
                        "mapped implementation cell count does not fit this host",
                    )
                })?,
            },
            stable_key: *digest.finalize().as_bytes(),
        })
    }

    pub(super) fn area(self) -> f64 {
        self.physical.area
    }

    pub(super) fn better_than(&self, other: &Self) -> bool {
        other
            .legal
            .cmp(&self.legal)
            .then_with(|| match (self.closure, other.closure) {
                (Some(candidate), Some(current)) => candidate.compare(current),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => self
                    .projected_worst_normalized_violation
                    .cmp(&other.projected_worst_normalized_violation)
                    .then_with(|| {
                        other
                            .projected_minimum_slack
                            .cmp(&self.projected_minimum_slack)
                    })
                    .then_with(|| {
                        self.projected_total_negative_slack
                            .cmp(&other.projected_total_negative_slack)
                    }),
            })
            .then_with(|| compare_physical(self.physical, other.physical))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn objective(wns: f64, tns: f64, paths: usize, area: f64) -> MappedObjective {
        MappedObjective::from_plans(
            &[],
            None,
            area,
            None,
            0,
            [0; 32],
            Some(ClosureQuality::new(
                opto_timing::TimingQualitySummary::aggregate(0.0, Some(wns), tns, paths),
                opto_timing::DesignRuleSummary::aggregate(0.0, 0.0, 0),
            )),
        )
        .unwrap()
    }

    #[test]
    fn exact_checkpoint_orders_wns_before_tns_paths_and_area() {
        let better_wns = objective(-0.1, -100.0, 100, 20.0);
        let fewer_violations = objective(-1.0, -1.0, 1, 10.0);

        assert!(better_wns.better_than(&fewer_violations));
        assert!(!fewer_violations.better_than(&better_wns));
    }
}
