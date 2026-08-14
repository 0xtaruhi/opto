// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{RegionCoverPlan, RegionRowId, SynthesisEffort};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EpochDecision {
    Converged,
    Remap(Box<[RegionRowId]>),
    Exhausted(Box<[RegionRowId]>),
}

#[derive(Debug)]
pub(crate) struct RegionalEpochCoordinator {
    epoch: u32,
    maximum_epochs: u32,
}

impl RegionalEpochCoordinator {
    pub(crate) fn new(effort: SynthesisEffort) -> Self {
        Self {
            epoch: 0,
            maximum_epochs: match effort {
                SynthesisEffort::Low => 1,
                SynthesisEffort::Medium => 3,
                SynthesisEffort::High => 6,
            },
        }
    }

    pub(crate) const fn epoch(&self) -> u32 {
        self.epoch
    }

    pub(crate) const fn completed_epochs(&self) -> usize {
        self.epoch as usize + 1
    }

    pub(crate) fn evaluate(&mut self, plans: &[RegionCoverPlan]) -> EpochDecision {
        let mut dirty = plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| {
                plan.cost().worst_normalized_violation.get() > 0.0
                    || violates_measured_contract(plan)
            })
            .filter_map(|(row, _)| RegionRowId::from_index(row).ok())
            .collect::<BTreeSet<_>>();
        dirty.extend(cross_boundary_mismatches(plans));
        let dirty = dirty.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if dirty.is_empty() {
            return EpochDecision::Converged;
        }
        if self.epoch + 1 >= self.maximum_epochs {
            return EpochDecision::Exhausted(dirty);
        }
        self.epoch += 1;
        EpochDecision::Remap(dirty)
    }
}

fn cross_boundary_mismatches(plans: &[RegionCoverPlan]) -> BTreeSet<RegionRowId> {
    let mut inputs = BTreeMap::<[u8; 32], BoundarySideRef<'_>>::new();
    let mut outputs = BTreeMap::<[u8; 32], BoundarySideRef<'_>>::new();
    for (row, plan) in plans.iter().enumerate() {
        let Ok(row) = RegionRowId::from_index(row) else {
            continue;
        };
        for contract in plan.boundary_response() {
            let key = contract.port().semantic_key();
            let side = BoundarySideRef {
                row,
                contract,
                response: plan
                    .measured_response()
                    .iter()
                    .find(|response| response.port_semantic_key == key),
            };
            match contract.port().direction() {
                crate::RegionPortDirection::Input => {
                    inputs.insert(key, side);
                }
                crate::RegionPortDirection::Output => {
                    outputs.insert(key, side);
                }
            }
        }
    }
    let mut dirty = BTreeSet::new();
    for (key, input) in inputs {
        let Some(output) = outputs.get(&key) else {
            continue;
        };
        if boundary_pair_mismatches(&input, output) {
            dirty.insert(input.row);
            dirty.insert(output.row);
        }
    }
    dirty
}

fn boundary_pair_mismatches(input: &BoundarySideRef<'_>, output: &BoundarySideRef<'_>) -> bool {
    let Some(input_response) = input.response else {
        return true;
    };
    let Some(output_response) = output.response else {
        return true;
    };
    for input_row in input.contract.rows() {
        let Some(input_contract) = input_row.input else {
            continue;
        };
        let output_measured = output_response.rows.iter().find(|row| {
            row.scenario == input_row.scenario && row.timing_tag == input_row.timing_tag
        });
        let Some(output_measured) = output_measured else {
            return true;
        };
        if output_measured.arrival != input_contract.arrival
            || output_measured.transition != input_contract.transition
            || output_measured.activity != input_contract.activity
        {
            return true;
        }
    }
    for output_row in output.contract.rows() {
        let Some(output_contract) = output_row.output else {
            continue;
        };
        let input_measured = input_response.rows.iter().find(|row| {
            row.scenario == output_row.scenario && row.timing_tag == output_row.timing_tag
        });
        let Some(input_measured) = input_measured else {
            return true;
        };
        if input_measured.input_capacitance != output_contract.capacitance {
            return true;
        }
        for (actual, limit) in [
            (
                input_measured.input_capacitance.early,
                output_contract.maximum_capacitance.rise,
            ),
            (
                input_measured.input_capacitance.early,
                output_contract.maximum_capacitance.fall,
            ),
            (
                input_measured.input_capacitance.late,
                output_contract.maximum_capacitance.rise,
            ),
            (
                input_measured.input_capacitance.late,
                output_contract.maximum_capacitance.fall,
            ),
        ] {
            if actual
                .zip(limit)
                .is_some_and(|(actual, limit)| actual > limit)
            {
                return true;
            }
        }
    }
    false
}

struct BoundarySideRef<'a> {
    row: RegionRowId,
    contract: &'a crate::BoundaryContract,
    response: Option<&'a crate::BoundaryResponse>,
}

fn violates_measured_contract(plan: &RegionCoverPlan) -> bool {
    for response in plan.measured_response() {
        let Some(contract) = plan
            .boundary_response()
            .iter()
            .find(|contract| contract.port().semantic_key() == response.port_semantic_key)
        else {
            return true;
        };
        for measured in &response.rows {
            let Some(required) = contract.rows().iter().find(|row| {
                row.scenario == measured.scenario && row.timing_tag == measured.timing_tag
            }) else {
                return true;
            };
            let Some(required) = required.output else {
                continue;
            };
            for (arrival, limit) in [
                (measured.arrival.early.rise, required.required.early.rise),
                (measured.arrival.early.fall, required.required.early.fall),
            ] {
                if arrival
                    .zip(limit)
                    .is_some_and(|(actual, limit)| actual < limit)
                {
                    return true;
                }
            }
            for (arrival, limit) in [
                (measured.arrival.late.rise, required.required.late.rise),
                (measured.arrival.late.fall, required.required.late.fall),
            ] {
                if arrival
                    .zip(limit)
                    .is_some_and(|(actual, limit)| actual > limit)
                {
                    return true;
                }
            }
            for (transition, limit) in [
                (
                    measured.transition.early.rise,
                    required.maximum_transition.rise,
                ),
                (
                    measured.transition.early.fall,
                    required.maximum_transition.fall,
                ),
                (
                    measured.transition.late.rise,
                    required.maximum_transition.rise,
                ),
                (
                    measured.transition.late.fall,
                    required.maximum_transition.fall,
                ),
            ] {
                if transition
                    .zip(limit)
                    .is_some_and(|(actual, limit)| actual > limit)
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FiniteValue, RegionAnchorId, RegionContextKey, RegionPlanCost, RegionRevision};

    fn plan(violation: f64) -> RegionCoverPlan {
        let zero = FiniteValue::new(0.0).unwrap();
        RegionCoverPlan::empty_for_test(
            RegionAnchorId::from_bytes_for_test([1; 32]),
            RegionRevision::from_bytes_for_test([2; 32]),
            RegionContextKey::from_bytes_for_test([3; 32]),
            RegionPlanCost {
                legal: true,
                worst_normalized_violation: FiniteValue::new(violation).unwrap(),
                minimum_slack: zero,
                total_negative_slack: zero,
                area: zero,
                leakage_power: None,
                dynamic_power: None,
                cell_count: 0,
                stable_plan_key: [0; 32],
            },
        )
    }

    #[test]
    fn converges_only_after_every_regional_contract_is_met() {
        let mut epochs = RegionalEpochCoordinator::new(SynthesisEffort::Medium);
        assert!(matches!(
            epochs.evaluate(&[plan(0.1)]),
            EpochDecision::Remap(_)
        ));
        assert_eq!(epochs.completed_epochs(), 2);
        assert_eq!(epochs.evaluate(&[plan(0.0)]), EpochDecision::Converged);
    }

    #[test]
    fn effort_limit_exhaustion_retains_the_ordered_dirty_rows() {
        let mut epochs = RegionalEpochCoordinator::new(SynthesisEffort::Medium);
        assert!(matches!(
            epochs.evaluate(&[plan(0.2)]),
            EpochDecision::Remap(_)
        ));
        assert!(matches!(
            epochs.evaluate(&[plan(0.1)]),
            EpochDecision::Remap(_)
        ));
        assert_eq!(
            epochs.evaluate(&[plan(0.1)]),
            EpochDecision::Exhausted(vec![RegionRowId::from_index(0).unwrap()].into_boxed_slice())
        );
    }
}
