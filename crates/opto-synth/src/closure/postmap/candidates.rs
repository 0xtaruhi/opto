// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::SynthesisOptions;
use opto_ir::mapped::{CellId, MappedNetlist};
#[cfg(test)]
use opto_library::TargetCell;
use opto_library::normalized_cell_area;
use opto_library::{TargetCellRef, TargetPinRef, cells_are_replacement_compatible};
use opto_runtime::{ExecutionContext, Task, TaskKey};
use std::collections::BTreeSet;

type PinSwap = (u16, u16);
type PinSwapsByCell = Box<[Box<[PinSwap]>]>;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SizingRegion {
    pub(super) cell: CellId,
    pub(super) monotonic_candidates: Vec<usize>,
    pub(super) tradeoff_candidates: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct PostmapCellCatalog {
    replacements: Box<[Box<[usize]>]>,
    buffers: Box<[usize]>,
    pin_swaps: PinSwapsByCell,
    mfs_functions: hashbrown::HashMap<String, super::mfs::CellFunction>,
    area_resynthesis: super::mfs::ResynthesisCells,
    timing_resynthesis: super::mfs::ResynthesisCells,
}

impl PostmapCellCatalog {
    pub(crate) fn new(options: &SynthesisOptions) -> Self {
        let mut replacements = Vec::with_capacity(options.target_cells.len());
        let mut pin_swaps = Vec::with_capacity(options.target_cells.len());
        for (current_index, current) in options.target_cells.iter().enumerate() {
            replacements.push(if current.is_synthesis_eligible() {
                options
                    .target_cells
                    .synthesis_cells()
                    .filter(|(candidate_index, candidate)| {
                        *candidate_index != current_index
                            && cells_are_replacement_compatible(current, *candidate)
                    })
                    .map(|(index, _)| index)
                    .collect()
            } else {
                Box::default()
            });
            let input_pins = current
                .pins()
                .enumerate()
                .filter(|(_, pin)| {
                    matches!(
                        pin.direction(),
                        opto_library::TargetPinDirection::Input
                            | opto_library::TargetPinDirection::Inout
                    )
                })
                .collect::<Vec<_>>();
            let mut swaps = Vec::new();
            if current.is_synthesis_eligible() {
                for first in 0..input_pins.len() {
                    for second in first + 1..input_pins.len() {
                        let (first_index, first_pin) = input_pins[first];
                        let (second_index, second_pin) = input_pins[second];
                        if opto_library::cell_input_pins_are_symmetric(
                            current,
                            first_pin.name(),
                            second_pin.name(),
                        ) && pin_swap_changes_timing(
                            current,
                            first_pin.name(),
                            second_pin.name(),
                        ) {
                            let first = u16::try_from(first_index)
                                .expect("validated target pin index fits in 16 bits");
                            let second = u16::try_from(second_index)
                                .expect("validated target pin index fits in 16 bits");
                            swaps.push((first, second));
                        }
                    }
                }
            }
            pin_swaps.push(swaps.into_boxed_slice());
        }
        let mut buffers = options
            .target_cells
            .synthesis_cells()
            .filter(|(_, cell)| is_positive_buffer(*cell))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        buffers.sort_by(|&left, &right| {
            let left = options
                .target_cells
                .get(left)
                .expect("buffer index is valid");
            let right = options
                .target_cells
                .get(right)
                .expect("buffer index is valid");
            worst_default_delay(left)
                .total_cmp(&worst_default_delay(right))
                .then_with(|| {
                    normalized_cell_area(left.area()).total_cmp(&normalized_cell_area(right.area()))
                })
                .then_with(|| left.name().cmp(right.name()))
        });
        let mfs_functions = super::mfs::cell_functions(&options.target_cells);
        let area_resynthesis =
            super::mfs::resynthesis_cells(&mfs_functions, super::mfs::ResynthesisObjective::Area);
        let timing_resynthesis =
            super::mfs::resynthesis_cells(&mfs_functions, super::mfs::ResynthesisObjective::Timing);
        Self {
            replacements: replacements.into_boxed_slice(),
            buffers: buffers.into_boxed_slice(),
            pin_swaps: pin_swaps.into_boxed_slice(),
            mfs_functions,
            area_resynthesis,
            timing_resynthesis,
        }
    }

    fn replacements(&self, current: usize) -> &[usize] {
        self.replacements.get(current).map_or(&[], Box::as_ref)
    }

    pub(crate) fn buffers(&self) -> &[usize] {
        &self.buffers
    }

    pub(crate) fn pin_swaps(&self, cell: usize) -> &[PinSwap] {
        self.pin_swaps.get(cell).map_or(&[], Box::as_ref)
    }

    pub(super) fn mfs_functions(&self) -> &hashbrown::HashMap<String, super::mfs::CellFunction> {
        &self.mfs_functions
    }

    pub(super) fn mfs_resynthesis(
        &self,
        objective: super::mfs::ResynthesisObjective,
    ) -> &super::mfs::ResynthesisCells {
        match objective {
            super::mfs::ResynthesisObjective::Area => &self.area_resynthesis,
            super::mfs::ResynthesisObjective::Timing => &self.timing_resynthesis,
        }
    }
}

pub(super) fn sizing_regions(
    runtime: &ExecutionContext,
    cells: impl IntoIterator<Item = CellId>,
    mapped: &MappedNetlist,
    options: &SynthesisOptions,
    catalog: &PostmapCellCatalog,
    area_recovery: bool,
    timing: Option<&crate::closure::mmmc::MmmcTiming>,
) -> Result<Vec<SizingRegion>, crate::SynthError> {
    let mut visited = BTreeSet::new();
    let mut inputs = Vec::new();
    for cell in cells {
        if !visited.insert(cell) {
            continue;
        }
        let mapped_cell = mapped.cell(cell).ok_or_else(|| {
            crate::SynthError::invariant(format!("sizing references non-live mapped cell {cell:?}"))
        })?;
        let Some(current) = mapped_cell.library_cell else {
            continue;
        };
        inputs.push((cell, current as usize));
    }
    let tasks = inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| {
            Ok(Task::new(
                TaskKey::new(
                    4,
                    ordinal
                        .try_into()
                        .map_err(|_| crate::SynthError::capacity("sizing task key overflow"))?,
                ),
                input,
            ))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?;
    runtime.map_ordered(tasks, |(cell, current_index)| {
        let current = options.target_cells.get(current_index).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "mapped cell references unknown library index {current_index}"
            ))
        })?;
        let (monotonic_candidates, tradeoff_candidates) = compatible_cells(
            options,
            catalog,
            current_index,
            current,
            area_recovery,
            timing,
            cell,
        );
        Ok(SizingRegion {
            cell,
            monotonic_candidates,
            tradeoff_candidates,
        })
    })
}

fn compatible_cells(
    options: &SynthesisOptions,
    catalog: &PostmapCellCatalog,
    current_index: usize,
    current: TargetCellRef<'_>,
    area_recovery: bool,
    timing: Option<&crate::closure::mmmc::MmmcTiming>,
    cell: CellId,
) -> (Vec<usize>, Vec<usize>) {
    let current_area = normalized_cell_area(current.area());
    let timing_instance = u32::try_from(cell.index())
        .ok()
        .map(opto_timing::TimingInstanceId::from_raw);
    let mut candidates = catalog
        .replacements(current_index)
        .iter()
        .filter_map(|&index| {
            let candidate = options.target_cells.get(index)?;
            ((!area_recovery || normalized_cell_area(candidate.area()) < current_area)
                && (area_recovery
                    || timing.is_none_or(|timing| {
                        timing_instance.is_none_or(|instance| {
                            timing.replacement_can_improve_timing(instance, candidate)
                        })
                    })))
            .then(|| CandidateEstimate::new(index, current, candidate, timing, timing_instance))
        })
        .collect::<Vec<_>>();
    if !area_recovery {
        retain_timing_pareto_frontier(&mut candidates);
    }
    let current_estimate =
        CandidateEstimate::new(current_index, current, current, timing, timing_instance);
    candidates.sort_by(|left, right| {
        let timing_order = left
            .delay
            .total_cmp(&right.delay)
            .then_with(|| left.transition.total_cmp(&right.transition))
            .then_with(|| left.input_capacitance.total_cmp(&right.input_capacitance));
        if area_recovery {
            left.area.total_cmp(&right.area).then(timing_order)
        } else {
            timing_order.then_with(|| left.area.total_cmp(&right.area))
        }
        .then_with(|| left.name.cmp(right.name))
    });
    let mut monotonic = Vec::new();
    let mut tradeoffs = Vec::new();
    for candidate in candidates {
        let target = if !area_recovery && candidate.electrically_dominates(&current_estimate) {
            &mut monotonic
        } else {
            &mut tradeoffs
        };
        target.push(candidate.index);
    }
    (monotonic, tradeoffs)
}

#[derive(Debug, Clone, PartialEq)]
struct CandidateEstimate<'a> {
    index: usize,
    name: &'a str,
    delay: f64,
    transition: f64,
    input_capacitance: f64,
    input_loads: Box<[f64]>,
    area: f64,
    exact_timing: bool,
}

impl<'a> CandidateEstimate<'a> {
    fn new(
        index: usize,
        current: TargetCellRef<'_>,
        candidate: TargetCellRef<'a>,
        timing: Option<&crate::closure::mmmc::MmmcTiming>,
        instance: Option<opto_timing::TimingInstanceId>,
    ) -> Self {
        let estimate = timing.and_then(|timing| {
            instance.and_then(|instance| timing.estimate_cell(instance, candidate))
        });
        let input_loads = current
            .pins()
            .filter(|pin| {
                matches!(
                    pin.direction(),
                    opto_library::TargetPinDirection::Input
                        | opto_library::TargetPinDirection::Inout
                )
            })
            .map(|current_pin| {
                candidate
                    .pins()
                    .find(|pin| pin.name() == current_pin.name())
                    .and_then(TargetPinRef::max_capacitance)
                    .map_or(0.0, finite_or_infinity)
            })
            .collect::<Box<[_]>>();
        let exact_timing = estimate.is_some() && input_loads.iter().all(|load| load.is_finite());
        Self {
            index,
            name: candidate.name(),
            delay: finite_or_infinity(
                estimate.map_or_else(|| worst_default_delay(candidate), |value| value.delay),
            ),
            transition: finite_or_infinity(
                estimate.map_or(f64::INFINITY, |value| value.transition),
            ),
            input_capacitance: finite_or_infinity(
                estimate.map_or(f64::INFINITY, |value| value.input_capacitance),
            ),
            input_loads,
            area: normalized_cell_area(candidate.area()),
            exact_timing,
        }
    }

    fn dominates(&self, other: &Self) -> bool {
        self.exact_timing
            && other.exact_timing
            && self.delay <= other.delay
            && self.transition <= other.transition
            && self.input_loads.len() == other.input_loads.len()
            && self
                .input_loads
                .iter()
                .zip(&other.input_loads)
                .all(|(this, other)| this <= other)
            && self.area <= other.area
            && (self.delay < other.delay
                || self.transition < other.transition
                || self
                    .input_loads
                    .iter()
                    .zip(&other.input_loads)
                    .any(|(this, other)| this < other)
                || self.area < other.area)
    }

    fn electrically_dominates(&self, other: &Self) -> bool {
        self.exact_timing
            && other.exact_timing
            && self.delay <= other.delay
            && self.transition <= other.transition
            && self.input_loads.len() == other.input_loads.len()
            && self
                .input_loads
                .iter()
                .zip(&other.input_loads)
                .all(|(this, other)| this <= other)
            && (self.delay < other.delay
                || self.transition < other.transition
                || self
                    .input_loads
                    .iter()
                    .zip(&other.input_loads)
                    .any(|(this, other)| this < other))
    }
}

fn finite_or_infinity(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        f64::INFINITY
    }
}

fn retain_timing_pareto_frontier(candidates: &mut Vec<CandidateEstimate<'_>>) {
    let dominated = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            candidates
                .iter()
                .enumerate()
                .any(|(other_index, other)| other_index != index && other.dominates(candidate))
        })
        .collect::<Vec<_>>();
    let mut ordinal = 0usize;
    candidates.retain(|_| {
        let keep = !dominated[ordinal];
        ordinal += 1;
        keep
    });
}

pub(super) fn pin_swap_changes_timing(cell: TargetCellRef<'_>, first: &str, second: &str) -> bool {
    let Some(first_pin) = cell.pins().find(|pin| pin.name() == first) else {
        return true;
    };
    let Some(second_pin) = cell.pins().find(|pin| pin.name() == second) else {
        return true;
    };
    if first_pin.capacitance() != second_pin.capacitance()
        || first_pin.rise_capacitance() != second_pin.rise_capacitance()
        || first_pin.fall_capacitance() != second_pin.fall_capacitance()
        || first_pin.receiver_capacitance() != second_pin.receiver_capacitance()
        || first_pin.fanout_load() != second_pin.fanout_load()
    {
        return true;
    }
    cell.pins()
        .filter(|pin| {
            matches!(
                pin.direction(),
                opto_library::TargetPinDirection::Output | opto_library::TargetPinDirection::Inout
            )
        })
        .any(|output| !related_arcs_match(output, first, second))
}

fn related_arcs_match(output: TargetPinRef<'_>, first: &str, second: &str) -> bool {
    let first_arcs = output
        .timing_arcs()
        .filter(|arc| arc.related_pin() == first)
        .collect::<Vec<_>>();
    let mut second_arcs = output
        .timing_arcs()
        .filter(|arc| arc.related_pin() == second)
        .collect::<Vec<_>>();
    if first_arcs.len() != second_arcs.len() {
        return false;
    }
    for first in first_arcs {
        let Some(index) = second_arcs
            .iter()
            .position(|second| timing_arcs_equal(first, *second))
        else {
            return false;
        };
        second_arcs.swap_remove(index);
    }
    true
}

fn timing_arcs_equal(
    left: opto_library::TargetTimingArcRef<'_>,
    right: opto_library::TargetTimingArcRef<'_>,
) -> bool {
    left.timing_type() == right.timing_type()
        && left.timing_sense() == right.timing_sense()
        && left.delay_model() == right.delay_model()
        && left.rise_constraint() == right.rise_constraint()
        && left.fall_constraint() == right.fall_constraint()
}

fn worst_default_delay(cell: TargetCellRef<'_>) -> f64 {
    cell.pins()
        .flat_map(TargetPinRef::timing_arcs)
        .filter_map(opto_library::TargetTimingArcRef::default_delay)
        .max_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn is_positive_buffer(cell: TargetCellRef<'_>) -> bool {
    if cell.sequential().next().is_some() {
        return false;
    }
    let inputs = cell
        .pins()
        .filter(|pin| pin.direction() == opto_library::TargetPinDirection::Input)
        .collect::<Vec<_>>();
    let outputs = cell
        .pins()
        .filter(|pin| pin.direction() == opto_library::TargetPinDirection::Output)
        .collect::<Vec<_>>();
    let ([input], [output]) = (inputs.as_slice(), outputs.as_slice()) else {
        return false;
    };
    output.function().is_some_and(|function| {
        function.eval(&mut |name| (name == input.name()).then_some(false)) == Some(false)
            && function.eval(&mut |name| (name == input.name()).then_some(true)) == Some(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(name: &str) -> TargetCell {
        let pin = |name: &str, direction, function: Option<&str>| opto_library::TargetPin {
            name: name.to_string(),
            direction,
            function: function.map(|function| crate::BooleanFunction::parse(function).unwrap()),
            three_state: None,
            capacitance: Some(1.0),
            rise_capacitance: None,
            fall_capacitance: None,
            receiver_capacitance: None,
            fanout_load: None,
            next_state_type: None,
            timing_arcs: Vec::new(),
            clock_gate_role: None,
        };
        TargetCell {
            name: name.to_string(),
            area: Some(1.0),
            dont_use: false,
            usage: opto_library::TargetCellUsage::default(),
            pins: vec![
                pin("A", opto_library::TargetPinDirection::Input, None),
                pin("Y", opto_library::TargetPinDirection::Output, Some("A")),
            ],
            sequential: Vec::new(),
            clock_gate: None,
            memory: None,
        }
    }

    #[test]
    fn postmap_catalog_never_selects_forbidden_cells() {
        let current = buffer("CURRENT");
        let eligible = buffer("ELIGIBLE");
        let mut dont_use = buffer("DONT_USE");
        dont_use.dont_use = true;
        let mut isolation = buffer("ISOLATION");
        isolation.usage = opto_library::TargetCellUsage::ISOLATION;
        let mut level_shifter = buffer("LEVEL_SHIFTER");
        level_shifter.usage = opto_library::TargetCellUsage::LEVEL_SHIFTER;
        let mut clock_gate = buffer("CLOCK_GATE");
        clock_gate.usage = opto_library::TargetCellUsage::INTEGRATED_CLOCK_GATING;
        let mut always_on = buffer("ALWAYS_ON");
        always_on.usage = opto_library::TargetCellUsage::ALWAYS_ON;
        let options = SynthesisOptions {
            target_cells: vec![
                current,
                eligible,
                dont_use,
                isolation,
                level_shifter,
                clock_gate,
                always_on,
            ]
            .into(),
        };

        let catalog = PostmapCellCatalog::new(&options);

        assert_eq!(catalog.replacements(0), [1]);
        assert_eq!(catalog.buffers(), [0, 1]);
    }

    fn estimate(
        index: usize,
        delay: f64,
        transition: f64,
        input_capacitance: f64,
        area: f64,
    ) -> CandidateEstimate<'static> {
        CandidateEstimate {
            index,
            name: "candidate",
            delay,
            transition,
            input_capacitance,
            input_loads: vec![input_capacitance].into_boxed_slice(),
            area,
            exact_timing: true,
        }
    }

    #[test]
    fn timing_pareto_frontier_removes_only_fully_dominated_cells() {
        let mut candidates = vec![
            estimate(0, 1.0, 1.0, 1.0, 1.0),
            estimate(1, 2.0, 2.0, 2.0, 2.0),
            estimate(2, 0.5, 2.0, 1.0, 2.0),
            estimate(3, 2.0, 0.5, 2.0, 2.0),
            estimate(4, 2.0, 2.0, 0.5, 2.0),
            estimate(5, 2.0, 2.0, 2.0, 0.5),
        ];

        retain_timing_pareto_frontier(&mut candidates);

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.index)
                .collect::<Vec<_>>(),
            vec![0, 2, 3, 4, 5]
        );
    }

    #[test]
    fn timing_pareto_frontier_preserves_candidates_without_exact_sta() {
        let mut unknown = estimate(1, 2.0, f64::INFINITY, f64::INFINITY, 2.0);
        unknown.exact_timing = false;
        let mut candidates = vec![estimate(0, 1.0, 1.0, 1.0, 1.0), unknown];

        retain_timing_pareto_frontier(&mut candidates);

        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn monotonic_sizing_requires_every_electrical_dimension_to_improve() {
        let current = estimate(0, 2.0, 2.0, 2.0, 1.0);
        assert!(estimate(1, 1.0, 2.0, 2.0, 4.0).electrically_dominates(&current));
        assert!(!estimate(2, 1.0, 2.1, 2.0, 1.0).electrically_dominates(&current));
        assert!(!estimate(3, 1.0, 2.0, 2.1, 1.0).electrically_dominates(&current));
    }
}
