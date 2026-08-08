// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::mapped::{AppliedRegionDelta, MappedCell, MappedNetlist};
use opto_library::TargetCellSet;
use opto_library::normalized_cell_area;
use opto_timing::{DesignRuleSummary, TimingQualitySummary};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PhysicalObjective {
    pub(super) area: f64,
    pub(super) leakage: Option<f64>,
    pub(super) dynamic: Option<f64>,
    pub(super) cells: usize,
}

pub(super) fn mapped_physical_objective(
    mapped: &MappedNetlist,
    library: &TargetCellSet,
    scenarios: &opto_timing::ScenarioSet,
) -> Result<PhysicalObjective, crate::SynthError> {
    let cells = mapped
        .cell_ids()
        .map(|cell| {
            mapped
                .cell(cell)
                .ok_or_else(|| {
                    crate::SynthError::invariant(format!("mapped cell {cell:?} disappeared"))
                })
                .and_then(|cell| mapped_cell_area(cell, library))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let area = cells.iter().copied().sum::<f64>();
    let leakage = mapped
        .cell_ids()
        .map(|cell| {
            mapped
                .cell(cell)
                .and_then(|cell| mapped_cell_leakage(cell, library, scenarios))
        })
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum());
    Ok(PhysicalObjective {
        area,
        leakage,
        dynamic: None,
        cells: mapped.cell_count(),
    })
}

pub(super) fn physical_objective_after_edit(
    mapped: &MappedNetlist,
    edit: &AppliedRegionDelta,
    current: PhysicalObjective,
    library: &TargetCellSet,
    scenarios: &opto_timing::ScenarioSet,
) -> Result<PhysicalObjective, crate::SynthError> {
    let previous_cells = edit.previous_live_cells().collect::<Vec<_>>();
    let previous_area = previous_cells
        .iter()
        .map(|(_, cell)| mapped_cell_area(cell, library))
        .sum::<Result<f64, _>>()?;
    let current_cells = edit
        .affected_cells()
        .filter_map(|cell| mapped.cell(cell))
        .collect::<Vec<_>>();
    let current_area = current_cells
        .iter()
        .map(|cell| mapped_cell_area(cell, library))
        .sum::<Result<f64, _>>()?;
    let previous_leakage = previous_cells
        .iter()
        .map(|(_, cell)| mapped_cell_leakage(cell, library, scenarios))
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum::<f64>());
    let current_leakage = current_cells
        .iter()
        .map(|cell| mapped_cell_leakage(cell, library, scenarios))
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum::<f64>());
    let cells = current
        .cells
        .checked_sub(previous_cells.len())
        .and_then(|cells| cells.checked_add(current_cells.len()))
        .ok_or_else(|| crate::SynthError::invariant("mapped physical cell count overflow"))?;
    Ok(PhysicalObjective {
        area: current.area - previous_area + current_area,
        leakage: current
            .leakage
            .zip(previous_leakage)
            .zip(current_leakage)
            .map(|((total, previous), next)| total - previous + next),
        dynamic: current.dynamic,
        cells,
    })
}

fn mapped_cell_leakage(
    cell: &MappedCell,
    library: &TargetCellSet,
    scenarios: &opto_timing::ScenarioSet,
) -> Option<f64> {
    let index = cell.library_cell?;
    let name = library.get(index as usize)?.name();
    scenarios
        .scenarios()
        .iter()
        .map(|scenario| {
            let power = scenario
                .power()
                .library()
                .cells
                .iter()
                .find(|cell| cell.name == name)?;
            power.cell_leakage_power.or_else(|| {
                power
                    .leakage_power
                    .iter()
                    .map(|group| group.value)
                    .max_by(f64::total_cmp)
            })
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max_by(f64::total_cmp)
}

fn mapped_cell_area(cell: &MappedCell, library: &TargetCellSet) -> Result<f64, crate::SynthError> {
    let Some(index) = cell.library_cell else {
        return Ok(0.0);
    };
    let target = library.get(index as usize).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "mapped cell references unknown library cell {index}"
        ))
    })?;
    let area = normalized_cell_area(target.area());
    area.is_finite().then_some(area).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "target cell '{}' has no finite area",
            target.name()
        ))
    })
}

pub(super) fn closure_improves(
    candidate: &TimingQualitySummary,
    candidate_rules: DesignRuleSummary,
    candidate_physical: PhysicalObjective,
    current: &TimingQualitySummary,
    current_rules: DesignRuleSummary,
    current_physical: PhysicalObjective,
) -> bool {
    match compare_design_rules(candidate_rules, current_rules) {
        std::cmp::Ordering::Less => return true,
        std::cmp::Ordering::Greater => return false,
        std::cmp::Ordering::Equal => {}
    }
    improves_timing_or_physical(
        candidate.wns(),
        candidate.tns(),
        candidate_physical,
        current.wns(),
        current.tns(),
        current_physical,
    )
}

fn improves_timing_or_physical(
    candidate_wns: Option<f64>,
    candidate_total_negative_slack: f64,
    candidate_physical: PhysicalObjective,
    current_wns: Option<f64>,
    current_total_negative_slack: f64,
    current_physical: PhysicalObjective,
) -> bool {
    match (candidate_wns, current_wns) {
        (Some(candidate), Some(current)) => {
            let candidate_violates = candidate < 0.0;
            let current_violates = current < 0.0;
            match (candidate_violates, current_violates) {
                (false, true) => return true,
                (true, false) => return false,
                (true, true) => match candidate.total_cmp(&current) {
                    std::cmp::Ordering::Greater => return true,
                    std::cmp::Ordering::Less => return false,
                    std::cmp::Ordering::Equal => match candidate_total_negative_slack
                        .total_cmp(&current_total_negative_slack)
                    {
                        std::cmp::Ordering::Greater => return true,
                        std::cmp::Ordering::Less => return false,
                        std::cmp::Ordering::Equal => {}
                    },
                },
                (false, false) => {}
            }
        }
        (None, None) => {}
        (Some(_), None) | (None, Some(_)) => return false,
    }
    improves_physical(candidate_physical, current_physical)
}

fn compare_design_rules(
    candidate: DesignRuleSummary,
    current: DesignRuleSummary,
) -> std::cmp::Ordering {
    candidate
        .worst_ratio()
        .total_cmp(&current.worst_ratio())
        .then_with(|| candidate.total_excess().total_cmp(&current.total_excess()))
        .then_with(|| candidate.violations().cmp(&current.violations()))
}

pub(super) fn improves_physical(candidate: PhysicalObjective, current: PhysicalObjective) -> bool {
    match candidate.area.total_cmp(&current.area) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => match (candidate.leakage, current.leakage) {
            (Some(candidate_leakage), Some(current_leakage)) => {
                match candidate_leakage.total_cmp(&current_leakage) {
                    std::cmp::Ordering::Less => true,
                    std::cmp::Ordering::Greater => false,
                    std::cmp::Ordering::Equal => compare_dynamic_then_cells(candidate, current),
                }
            }
            _ => compare_dynamic_then_cells(candidate, current),
        },
    }
}

fn compare_dynamic_then_cells(candidate: PhysicalObjective, current: PhysicalObjective) -> bool {
    match (candidate.dynamic, current.dynamic) {
        (Some(candidate_dynamic), Some(current_dynamic)) => {
            match candidate_dynamic.total_cmp(&current_dynamic) {
                std::cmp::Ordering::Less => true,
                std::cmp::Ordering::Greater => false,
                std::cmp::Ordering::Equal => candidate.cells < current.cells,
            }
        }
        _ => candidate.cells < current.cells,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::mapped::{MappedBuilder, RegionDelta};
    use opto_library::TargetCell;

    fn physical(area: f64, cells: usize) -> PhysicalObjective {
        PhysicalObjective {
            area,
            leakage: None,
            dynamic: None,
            cells,
        }
    }

    fn scenarios() -> opto_timing::ScenarioSet {
        opto_timing::ScenarioSet::single(
            std::sync::Arc::new(opto_timing::TimingContext::default()),
            std::sync::Arc::new(opto_timing::TimingLibrary::default()),
            opto_timing::Parasitics::default(),
        )
    }

    #[test]
    fn violated_timing_is_repaired_before_physical_recovery() {
        assert!(improves_timing_or_physical(
            Some(0.0),
            0.0,
            physical(20.0, 20),
            Some(-0.1),
            -0.1,
            physical(10.0, 10),
        ));
        assert!(!improves_timing_or_physical(
            Some(-0.01),
            -0.01,
            physical(1.0, 1),
            Some(0.0),
            0.0,
            physical(10.0, 10),
        ));
    }

    #[test]
    fn met_timing_recovers_area_then_cell_count_without_chasing_margin() {
        assert!(improves_timing_or_physical(
            Some(0.1),
            0.0,
            physical(9.0, 20),
            Some(1.0),
            0.0,
            physical(10.0, 10),
        ));
        assert!(improves_timing_or_physical(
            None,
            0.0,
            physical(10.0, 9),
            None,
            0.0,
            physical(10.0, 10),
        ));
        assert!(!improves_timing_or_physical(
            Some(1.0),
            0.0,
            physical(11.0, 1),
            Some(0.1),
            0.0,
            physical(10.0, 10),
        ));
    }

    #[test]
    fn equal_area_prefers_measured_leakage_before_cell_count() {
        let current = PhysicalObjective {
            area: 10.0,
            leakage: Some(4.0),
            dynamic: Some(2.0),
            cells: 1,
        };
        let lower_leakage = PhysicalObjective {
            area: 10.0,
            leakage: Some(3.0),
            dynamic: Some(2.0),
            cells: 2,
        };
        assert!(improves_physical(lower_leakage, current));
        assert!(!improves_physical(current, lower_leakage));

        let unmeasured = PhysicalObjective {
            area: 10.0,
            leakage: None,
            dynamic: None,
            cells: 1,
        };
        assert!(!improves_physical(lower_leakage, unmeasured));
    }

    #[test]
    fn equal_area_and_leakage_prefer_measured_dynamic_power() {
        let current = PhysicalObjective {
            area: 10.0,
            leakage: Some(3.0),
            dynamic: Some(4.0),
            cells: 1,
        };
        let lower_dynamic = PhysicalObjective {
            area: 10.0,
            leakage: Some(3.0),
            dynamic: Some(2.0),
            cells: 2,
        };
        assert!(improves_physical(lower_dynamic, current));
        assert!(!improves_physical(current, lower_dynamic));

        let unmeasured = PhysicalObjective {
            area: 10.0,
            leakage: Some(3.0),
            dynamic: None,
            cells: 1,
        };
        assert!(!improves_physical(lower_dynamic, unmeasured));
    }

    #[test]
    fn region_edit_updates_physical_objective_without_a_global_rescan() {
        let library: TargetCellSet = vec![
            TargetCell {
                dont_use: false,
                usage: opto_library::TargetCellUsage::default(),
                name: "LARGE".to_string(),
                area: Some(3.0),
                pins: Vec::new(),
                sequential: Vec::new(),
                clock_gate: None,
                memory: None,
            },
            TargetCell {
                dont_use: false,
                usage: opto_library::TargetCellUsage::default(),
                name: "SMALL".to_string(),
                area: Some(1.0),
                pins: Vec::new(),
                sequential: Vec::new(),
                clock_gate: None,
                memory: None,
            },
        ]
        .into();
        let mut builder = MappedBuilder::new("top", opto_ir::RevisionId::INITIAL).unwrap();
        let cell = builder.add_cell("U0", "LARGE", Some(0), &[]).unwrap();
        let mut mapped = builder.freeze().unwrap();
        let scenarios = scenarios();
        let baseline = mapped_physical_objective(&mapped, &library, &scenarios).unwrap();
        let snapshot = mapped.snapshot_region([cell], []).unwrap();
        let mut delta = RegionDelta::new(snapshot);
        delta.replace_cell(cell, "SMALL", Some(1)).unwrap();
        let edit = mapped.apply_region_delta(delta).unwrap();

        let incremental =
            physical_objective_after_edit(&mapped, &edit, baseline, &library, &scenarios).unwrap();
        let complete = mapped_physical_objective(&mapped, &library, &scenarios).unwrap();

        assert_eq!(incremental, physical(1.0, 1));
        assert_eq!(incremental, complete);
    }
}
