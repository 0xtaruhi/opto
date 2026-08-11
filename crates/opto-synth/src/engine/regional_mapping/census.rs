// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Generation-local implementation metrics maintained from regional deltas.

use super::{
    ImplementationCensus, MappedCellSource, RegionalIr, RegionalMappedState, RegionalMapper,
    ScenarioLeakageCensus,
};
use opto_ir::mapped::CellId;

pub(super) struct CensusContribution {
    library_area_all: f64,
    managed_cell_count: u64,
    leakage_by_scenario: Box<[ScenarioLeakageCensus]>,
}

impl ImplementationCensus {
    pub(super) fn area(&self) -> f64 {
        self.library_area_all
    }

    pub(super) fn managed_leakage(&self) -> Option<f64> {
        self.leakage_by_scenario
            .iter()
            .map(|scenario| (scenario.unknown_cells == 0).then_some(scenario.known_total))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max_by(f64::total_cmp)
    }

    pub(super) fn replaced(
        &self,
        removed: &CensusContribution,
        added: &CensusContribution,
    ) -> Result<Self, crate::SynthError> {
        if self.leakage_by_scenario.len() != removed.leakage_by_scenario.len()
            || self.leakage_by_scenario.len() != added.leakage_by_scenario.len()
        {
            return Err(crate::SynthError::invariant(
                "incremental implementation census lost an MMMC scenario",
            ));
        }
        let managed_cell_count = self
            .managed_cell_count
            .checked_sub(removed.managed_cell_count)
            .and_then(|count| count.checked_add(added.managed_cell_count))
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "incremental implementation census cell count is inconsistent",
                )
            })?;
        let leakage_by_scenario = self
            .leakage_by_scenario
            .iter()
            .zip(&removed.leakage_by_scenario)
            .zip(&added.leakage_by_scenario)
            .map(|((current, removed), added)| {
                let unknown_cells = current
                    .unknown_cells
                    .checked_sub(removed.unknown_cells)
                    .and_then(|count| count.checked_add(added.unknown_cells))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "incremental implementation leakage census is inconsistent",
                        )
                    })?;
                Ok(ScenarioLeakageCensus {
                    known_total: current.known_total - removed.known_total + added.known_total,
                    unknown_cells,
                })
            })
            .collect::<Result<Box<[_]>, crate::SynthError>>()?;
        Ok(Self {
            library_area_all: self.library_area_all - removed.library_area_all
                + added.library_area_all,
            managed_cell_count,
            leakage_by_scenario,
            static_key: self.static_key,
        })
    }
}

impl RegionalMapper<'_> {
    pub(super) fn full_implementation_census(
        &self,
        ir: &RegionalIr<'_>,
        mapped: &RegionalMappedState,
    ) -> Result<ImplementationCensus, crate::SynthError> {
        let mut library_area_all = 0.0;
        let mut implementation = Vec::new();
        let mut static_implementation = Vec::new();
        for cell_id in mapped.netlist.cell_ids() {
            library_area_all += self.mapped_cell_area(&mapped.netlist, cell_id)?;
            let source = mapped
                .cell_sources
                .get(cell_id.index())
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    crate::SynthError::invariant("mapped cell census has no provenance source")
                })?;
            let managed = match source {
                MappedCellSource::Instance(instance) => !self
                    .config
                    .source_instances
                    .is_source_instance(ir.module, *instance)?,
                MappedCellSource::Value { .. } | MappedCellSource::Region { .. } => true,
            };
            if managed {
                let cell = Self::implementation_cell(&mapped.netlist, cell_id)?;
                if !matches!(source, MappedCellSource::Region { .. }) {
                    static_implementation.push(cell.clone());
                }
                implementation.push(cell);
            }
        }
        implementation.sort();
        static_implementation.sort();
        let totals = self.census_from_implementation(library_area_all, &implementation)?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/regional/static-implementation/v1\0");
        digest.update(&(static_implementation.len() as u64).to_le_bytes());
        for cell in &static_implementation {
            digest.update(&(cell.cell_name.len() as u64).to_le_bytes());
            digest.update(cell.cell_name.as_bytes());
            digest.update(&cell.pin_count.to_le_bytes());
        }
        Ok(ImplementationCensus {
            library_area_all: totals.library_area_all,
            managed_cell_count: totals.managed_cell_count,
            leakage_by_scenario: totals.leakage_by_scenario,
            static_key: *digest.finalize().as_bytes(),
        })
    }

    pub(super) fn census_contribution(
        &self,
        mapped: &opto_ir::mapped::MappedNetlist,
        cells: impl IntoIterator<Item = CellId>,
    ) -> Result<CensusContribution, crate::SynthError> {
        let mut library_area_all = 0.0;
        let mut implementation = Vec::new();
        for cell in cells {
            library_area_all += self.mapped_cell_area(mapped, cell)?;
            implementation.push(Self::implementation_cell(mapped, cell)?);
        }
        implementation.sort();
        self.census_from_implementation(library_area_all, &implementation)
    }

    fn census_from_implementation(
        &self,
        library_area_all: f64,
        implementation: &[crate::regional::RegionImplementationCell],
    ) -> Result<CensusContribution, crate::SynthError> {
        let managed_cell_count = u64::try_from(implementation.len())
            .map_err(|_| crate::SynthError::capacity("mapped implementation cell count"))?;
        let mut leakage_by_scenario =
            vec![ScenarioLeakageCensus::default(); self.config.scenarios.scenarios().len()];
        for cell in implementation {
            for (scenario, leakage) in leakage_by_scenario
                .iter_mut()
                .zip(self.response_models.leakage_by_scenario(&cell.cell_name))
            {
                match leakage {
                    Some(leakage) => scenario.known_total += leakage,
                    None => {
                        scenario.unknown_cells =
                            scenario.unknown_cells.checked_add(1).ok_or_else(|| {
                                crate::SynthError::capacity("mapped implementation leakage census")
                            })?;
                    }
                }
            }
        }
        Ok(CensusContribution {
            library_area_all,
            managed_cell_count,
            leakage_by_scenario: leakage_by_scenario.into_boxed_slice(),
        })
    }

    fn mapped_cell_area(
        &self,
        mapped: &opto_ir::mapped::MappedNetlist,
        cell_id: CellId,
    ) -> Result<f64, crate::SynthError> {
        let cell = mapped.cell(cell_id).ok_or_else(|| {
            crate::SynthError::invariant("live mapped cell disappeared during census")
        })?;
        let Some(library_cell) = cell.library_cell else {
            return Ok(0.0);
        };
        let target = self
            .config
            .options
            .target_cells
            .get(library_cell as usize)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "mapped cell references an unknown target-library cell",
                )
            })?;
        let area = opto_library::normalized_cell_area(target.area());
        if !area.is_finite() {
            return Err(crate::SynthError::mapping(format!(
                "selected target cell '{}' has no finite area",
                target.name()
            )));
        }
        Ok(area)
    }

    fn implementation_cell(
        mapped: &opto_ir::mapped::MappedNetlist,
        cell_id: CellId,
    ) -> Result<crate::regional::RegionImplementationCell, crate::SynthError> {
        Ok(crate::regional::RegionImplementationCell {
            cell_name: mapped
                .cell_type(cell_id)
                .ok_or_else(|| {
                    crate::SynthError::invariant("mapped implementation cell has no type name")
                })?
                .into(),
            pin_count: u32::try_from(
                mapped
                    .connections(cell_id)
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "mapped implementation cell has no connections",
                        )
                    })?
                    .len(),
            )
            .map_err(|_| crate::SynthError::capacity("mapped implementation pin count"))?,
        })
    }
}
