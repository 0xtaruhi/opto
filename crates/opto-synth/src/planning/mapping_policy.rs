// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellCost {
    pub(crate) area: f64,
    pub(crate) delay: f64,
    pub(crate) transition: f64,
    pub(crate) input_capacitance: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct MappingCost {
    pub(crate) area: f64,
    pub(crate) delay: f64,
    pub(crate) transition: f64,
    pub(crate) electrical_delay: f64,
    pub(crate) electrical_transition: f64,
    pub(crate) depth: u32,
    pub(crate) input_capacitance: f64,
}

impl MappingCost {
    pub(crate) const fn zero() -> Self {
        Self {
            area: 0.0,
            delay: 0.0,
            transition: 0.0,
            electrical_delay: 0.0,
            electrical_transition: 0.0,
            depth: 0,
            input_capacitance: 0.0,
        }
    }

    pub(crate) fn cell(self, cell: CellCost) -> Self {
        self.cell_with_electrical(cell, cell)
    }

    pub(crate) fn cell_with_electrical(self, cell: CellCost, electrical: CellCost) -> Self {
        Self {
            area: self.area + cell.area,
            delay: self.delay + cell.delay,
            transition: cell.transition,
            electrical_delay: self.electrical_delay + electrical.delay,
            electrical_transition: electrical.transition,
            depth: self.depth + 1,
            input_capacitance: self.input_capacitance + cell.input_capacitance,
        }
    }

    pub(crate) fn combine(self, other: Self) -> Self {
        Self {
            area: self.area + other.area,
            delay: self.delay.max(other.delay),
            transition: self.transition.max(other.transition),
            electrical_delay: self.electrical_delay.max(other.electrical_delay),
            electrical_transition: self.electrical_transition.max(other.electrical_transition),
            depth: self.depth.max(other.depth),
            input_capacitance: self.input_capacitance + other.input_capacitance,
        }
    }
}

/// Returns `Less` when `left` is the preferred unconstrained implementation.
pub(crate) fn compare_mapping_cost(left: MappingCost, right: MappingCost) -> Ordering {
    left.area
        .total_cmp(&right.area)
        .then_with(|| left.delay.total_cmp(&right.delay))
        .then_with(|| left.transition.total_cmp(&right.transition))
        .then_with(|| left.electrical_delay.total_cmp(&right.electrical_delay))
        .then_with(|| {
            left.electrical_transition
                .total_cmp(&right.electrical_transition)
        })
        .then_with(|| left.depth.cmp(&right.depth))
        .then_with(|| left.input_capacitance.total_cmp(&right.input_capacitance))
}

fn compare_violating_cost(left: MappingCost, right: MappingCost) -> Ordering {
    left.electrical_delay
        .total_cmp(&right.electrical_delay)
        .then_with(|| {
            left.electrical_transition
                .total_cmp(&right.electrical_transition)
        })
        .then_with(|| left.delay.total_cmp(&right.delay))
        .then_with(|| left.transition.total_cmp(&right.transition))
        .then_with(|| left.depth.cmp(&right.depth))
        .then_with(|| left.area.total_cmp(&right.area))
        .then_with(|| left.input_capacitance.total_cmp(&right.input_capacitance))
}

pub(crate) fn compare_cell_cost(left: CellCost, right: CellCost) -> Ordering {
    compare_mapping_cost(
        MappingCost::zero().cell(left),
        MappingCost::zero().cell(right),
    )
}

/// Compares implementations that already satisfy the same local requirement.
///
/// Feasibility consumes the requirement; positive margin cannot buy area.
/// Arrival remains a deterministic tie-break so an exactly equal-area choice
/// does not discard free timing margin.
pub(crate) fn compare_feasible_area(
    candidate_area: f64,
    candidate_arrival: f64,
    current_area: f64,
    current_arrival: f64,
) -> Ordering {
    candidate_area
        .total_cmp(&current_area)
        .then_with(|| candidate_arrival.total_cmp(&current_arrival))
}

/// Constrained choices first become feasible, minimize area once both meet the
/// required time, and minimize delay while both still violate it.
pub(crate) fn compare_mapping_cost_with_required_time(
    required: f64,
    candidate: MappingCost,
    current: MappingCost,
) -> Ordering {
    if !required.is_finite() {
        return compare_mapping_cost(candidate, current);
    }
    match (
        candidate.electrical_delay <= required,
        current.electrical_delay <= required,
    ) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (true, true) => compare_feasible_area(
            candidate.area,
            candidate.electrical_delay,
            current.area,
            current.electrical_delay,
        )
        .then_with(|| {
            candidate
                .electrical_transition
                .total_cmp(&current.electrical_transition)
        })
        .then_with(|| candidate.delay.total_cmp(&current.delay))
        .then_with(|| candidate.transition.total_cmp(&current.transition))
        .then_with(|| candidate.depth.cmp(&current.depth))
        .then_with(|| {
            candidate
                .input_capacitance
                .total_cmp(&current.input_capacitance)
        }),
        (false, false) => compare_violating_cost(candidate, current),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(area: f64, delay: f64) -> MappingCost {
        MappingCost {
            area,
            delay,
            electrical_delay: delay,
            ..MappingCost::zero()
        }
    }

    #[test]
    fn timing_closure_recovers_area_after_the_budget_is_met() {
        let small_late = cost(1.0, 1.1);
        let large_met = cost(4.0, 0.9);
        assert!(compare_mapping_cost_with_required_time(1.0, large_met, small_late).is_lt());

        let small_met = cost(1.0, 1.5);
        let large_met = cost(4.0, 1.0);
        assert!(compare_mapping_cost_with_required_time(2.0, small_met, large_met).is_lt());

        let smaller_slower = cost(9.0, 1.2);
        let larger_faster = cost(10.0, 1.0);
        assert!(
            compare_mapping_cost_with_required_time(2.0, smaller_slower, larger_faster).is_lt()
        );
    }

    #[test]
    fn violated_budget_uses_load_dependent_delay_before_nominal_delay() {
        let nominally_fast = MappingCost {
            delay: 0.5,
            electrical_delay: 2.0,
            ..MappingCost::zero()
        };
        let electrically_fast = MappingCost {
            delay: 1.0,
            electrical_delay: 0.8,
            ..MappingCost::zero()
        };
        assert!(
            compare_mapping_cost_with_required_time(0.1, electrically_fast, nominally_fast).is_lt()
        );
    }
}
