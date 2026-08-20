// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! The regional mapping epoch driver.
//!
//! Seeds region plans, commits and measures one mapped generation at a time, and refines
//! the rows the coordinator marks dirty until the objective stops improving.

use super::{
    BestMapping, CombinationalCellCatalog, MappedCellSource, MappedObjective, MappedRegionArtifact,
    MappedRegionFootprint, RegionalMappedState, RegionalMapper, RegionalMappingOutcome,
    RegionalMappingState, SynthesisProgress, WordMappedSignals, boundary_observation_values,
    materialize, resolve_boundary_nets,
};
use crate::mapping::MappedOutput;
use opto_ir::mapped::{CellId, RegionDelta};
use std::collections::BTreeSet;

struct PreparedRegion {
    row: usize,
    artifact: MappedRegionArtifact,
    origins: crate::artifact::implementation::OriginSetId,
}

impl RegionalMapper<'_> {
    fn combinational_catalog(&self) -> &CombinationalCellCatalog {
        &self.config.mapping_context.combinational_catalog
    }

    /// Commits, measures, and refines regional plans until the coordinator
    /// stops asking for another epoch, then publishes the best candidate.
    pub(super) fn run_epochs(
        &self,
        state: &mut RegionalMappingState<'_>,
        observer: &mut dyn FnMut(SynthesisProgress),
    ) -> Result<RegionalMappingOutcome, crate::SynthError> {
        let mut coordinator = crate::regional::RegionalEpochCoordinator::new(self.config.effort);
        let mut mapped = self.build_initial_generation(state)?;
        let mut best = None::<BestMapping>;
        loop {
            let epoch = coordinator.epoch();
            let rows = {
                let _profile = self
                    .trace
                    .span(|| format!("initial_mapping.epoch[{epoch}].snapshot"));
                state.rows.clone()
            };
            let plans = rows.iter().map(|row| row.plan.clone()).collect::<Vec<_>>();
            let (measured_plans, global_dynamic_power, timing_quality) =
                self.measure_epoch(state, &mut mapped, &plans, epoch)?;
            let census = mapped.implementation_census.as_ref().ok_or_else(|| {
                crate::SynthError::invariant("mapped implementation census was not initialized")
            })?;
            let objective = MappedObjective::from_plans(
                &measured_plans,
                global_dynamic_power,
                census.area(),
                census.managed_leakage(),
                census.managed_cell_count,
                census.static_key,
                timing_quality,
            )?;
            let current_is_best = best
                .as_ref()
                .is_none_or(|best| objective.better_than(&best.objective));
            if current_is_best {
                let checkpoint_rows = measured_plans
                    .iter()
                    .cloned()
                    .zip(rows.iter().map(|row| row.binding.clone()))
                    .map(|(plan, binding)| super::RegionalPlanRow { plan, binding })
                    .collect();
                best = Some(BestMapping {
                    objective,
                    rows: checkpoint_rows,
                });
            }
            let decision = coordinator.evaluate(&measured_plans);
            // A remap that moves no contract would re-measure identical plans
            // and spend an epoch of budget for nothing.
            let decision = match decision {
                crate::regional::EpochDecision::Remap(dirty) => {
                    for (row, plan) in state.rows.iter_mut().zip(measured_plans) {
                        row.plan = plan;
                    }
                    let changed = Self::reallocate_contracts(state, &dirty, epoch)?;
                    if changed.is_empty() {
                        crate::regional::EpochDecision::Converged
                    } else {
                        let previous = changed
                            .iter()
                            .map(|row| {
                                let index = row.index();
                                (
                                    index,
                                    state.rows[index].plan.payload().to_vec(),
                                    state.rows[index].binding.clone(),
                                )
                            })
                            .collect::<Vec<_>>();
                        self.refresh_contracts(state, &changed)?;
                        let topology_changed = previous
                            .into_iter()
                            .filter_map(|(index, payload, binding)| {
                                (state.rows[index].plan.payload() != payload
                                    || state.rows[index].binding != binding)
                                    .then_some(index)
                            })
                            .collect::<Vec<_>>();
                        self.replace_regions(state, &mut mapped, &topology_changed)?;
                        continue;
                    }
                }
                decision => decision,
            };
            match decision {
                crate::regional::EpochDecision::Converged
                | crate::regional::EpochDecision::Exhausted(_) => {
                    let best = best.take().ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional epoch completion has no legal mapped candidate",
                        )
                    })?;
                    let best_rows = best.rows;
                    if current_is_best {
                        state.rows = best_rows.to_vec();
                    } else {
                        let changed = rows
                            .iter()
                            .zip(&best_rows)
                            .enumerate()
                            .filter_map(|(index, (current, checkpoint))| {
                                (current.plan.payload() != checkpoint.plan.payload()
                                    || current.binding != checkpoint.binding)
                                    .then_some(index)
                            })
                            .collect::<Vec<_>>();
                        state.rows = best_rows.to_vec();
                        self.replace_regions(state, &mut mapped, &changed)?;
                    }
                    let selected_plans = best_rows
                        .into_iter()
                        .map(|row| row.plan)
                        .collect::<Box<[_]>>();
                    match mapped.timing.as_mut() {
                        Some(timing) => observer(SynthesisProgress::timing_candidate(
                            crate::OptimizationPhase::TechnologyMapping,
                            best.objective.area.get(),
                            mapped.netlist.cell_count(),
                            &timing.metrics()?.analysis,
                            coordinator.completed_epochs(),
                        )),
                        None => observer(SynthesisProgress::candidate(
                            crate::OptimizationPhase::TechnologyMapping,
                            best.objective.area.get(),
                            mapped.netlist.cell_count(),
                        )),
                    }
                    let (mapped, timing) = mapped.finish()?;
                    return Ok(RegionalMappingOutcome {
                        plans: selected_plans,
                        plan_journal: state.take_plan_journal()?,
                        epochs: coordinator.completed_epochs(),
                        mapped,
                        timing,
                    });
                }
                crate::regional::EpochDecision::Remap(_) => {
                    return Err(crate::SynthError::invariant(
                        "regional remap decision survived its own handler",
                    ));
                }
            }
        }
    }

    fn build_initial_generation(
        &self,
        state: &mut RegionalMappingState<'_>,
    ) -> Result<RegionalMappedState, crate::SynthError> {
        let boundary_values = boundary_observation_values(self.regions, state.region_ownership)?;
        let mut observed_values = materialize::region_delta::regional_binding_values(
            state.rows.iter().map(|row| &row.binding),
        )
        .into_vec();
        observed_values.extend(materialize::sequential_binding_values(
            state.module,
            state.sequential_operations,
        )?);
        observed_values.extend(
            boundary_values
                .iter()
                .flat_map(|(_, values)| values.iter().copied()),
        );
        observed_values.sort_unstable();
        observed_values.dedup();
        let (
            materialize::MappedOutput {
                netlist,
                cell_sources: substrate_sources,
            },
            observed_nets,
        ) = materialize::build_mapped_substrate(materialize::MappedSubstrateRequest {
            module: state.module,
            options: self.config.options,
            design_references: self.config.design_references,
            reference_ports: self.config.reference_ports,
            source_instances: self.config.source_instances,
            base_revision: self.config.base_revision,
            observed_values: &observed_values,
        })?;
        let signals =
            WordMappedSignals::from_observations(state.module, &observed_values, &observed_nets)?;
        let connectivity = materialize::FrozenObservableConnectivity::capture_substrate(
            &netlist,
            &self.config.options.target_cells,
            self.config.reference_ports,
        )?;
        let boundary_nets = resolve_boundary_nets(&signals, &boundary_values)?;
        let substrate_cell_count = substrate_sources.len();
        let mut cell_sources = vec![None; netlist.cell_slot_count()];
        for (cell, source) in substrate_sources {
            let slot = cell_sources.get_mut(cell.index()).ok_or_else(|| {
                crate::SynthError::invariant("mapped substrate source is out of range")
            })?;
            if slot.replace(source).is_some() {
                return Err(crate::SynthError::invariant(
                    "mapped substrate cell has duplicate provenance",
                ));
            }
        }
        if substrate_cell_count != netlist.cell_count() {
            return Err(crate::SynthError::invariant(
                "mapped substrate has a cell without provenance",
            ));
        }
        let mut mapped = RegionalMappedState {
            netlist,
            connectivity,
            cell_sources,
            implementation_census: None,
            signals,
            boundary_nets: boundary_nets.into_boxed_slice(),
            footprints: std::iter::repeat_with(|| None)
                .take(state.rows.len())
                .collect(),
            timing: None,
        };
        let sequential = materialize::MappedSequentialArtifact::from_module(
            state.module,
            &mapped.signals,
            self.regions,
            state.sequential_operations,
            &self.config,
        )?;
        let rows = (0..state.rows.len()).collect::<Vec<_>>();
        let regions = self.prepare_regions(state, &mapped, &rows)?;
        self.apply_regions(&mut mapped, &regions, Some(&sequential))?;
        let census = self.full_implementation_census(state, &mapped)?;
        mapped.implementation_census = Some(census);
        mapped.timing = crate::closure::mmmc::MmmcTiming::new(
            &mapped.netlist,
            self.config.design_id,
            self.config.port_bindings,
            &self.config.object_bindings,
            self.config.scenarios,
            self.runtime,
        )?;
        if let Some(timing) = &mapped.timing {
            let memory = timing.memory_usage();
            crate::api::diagnostics::trace!(
                self.trace,
                "initial_mapping.mmmc_memory",
                "resident_bytes={} construction_scratch_high_water_bytes={} construction_high_water_bytes={}",
                memory.resident_bytes,
                memory.construction_scratch_high_water_bytes,
                memory.construction_high_water_bytes,
            );
        }
        Ok(mapped)
    }

    /// Measures a candidate directly on the long-lived mapped generation.
    #[expect(
        clippy::type_complexity,
        reason = "named local destructuring is clearer than a one-use measurement carrier"
    )]
    fn measure_epoch(
        &self,
        state: &RegionalMappingState<'_>,
        mapped: &mut RegionalMappedState,
        plans: &[crate::RegionCoverPlan],
        epoch: u32,
    ) -> Result<
        (
            Vec<crate::RegionCoverPlan>,
            Option<f64>,
            Option<opto_timing::TimingQualitySummary>,
        ),
        crate::SynthError,
    > {
        let (plans, global_dynamic_power) = {
            let _profile = self
                .trace
                .span(|| format!("initial_mapping.epoch[{epoch}].boundary_measurement"));
            match mapped.timing.as_ref() {
                Some(timing) => crate::closure::measure_global_boundaries(
                    crate::closure::GlobalBoundaryRequest {
                        timing,
                        plans,
                        observations: &mapped.boundary_nets,
                        scenarios: self.config.scenarios,
                        timing_tags: state.contracts.timing_tags(),
                        power_evaluator: self.config.power_evaluator,
                    },
                    self.runtime,
                )?,
                None => (plans.to_vec(), None),
            }
        };
        let timing_quality = mapped
            .timing
            .as_mut()
            .map(|timing| timing.metrics().map(|metrics| metrics.analysis))
            .transpose()?;
        Ok((plans, global_dynamic_power, timing_quality))
    }

    fn replace_regions(
        &self,
        state: &mut RegionalMappingState<'_>,
        mapped: &mut RegionalMappedState,
        rows: &[usize],
    ) -> Result<(), crate::SynthError> {
        if rows.is_empty() {
            return Ok(());
        }
        let regions = self.prepare_regions(state, mapped, rows)?;
        self.apply_regions(mapped, &regions, None)
    }

    fn prepare_regions(
        &self,
        state: &mut RegionalMappingState<'_>,
        mapped: &RegionalMappedState,
        materialization_rows: &[usize],
    ) -> Result<Vec<PreparedRegion>, crate::SynthError> {
        if materialization_rows
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(crate::SynthError::invariant(
                "regional materialization rows are not strictly ordered",
            ));
        }
        let module = state.module;
        let region_ownership = state.region_ownership;
        let plan_rows = &state.rows;
        let provenance = &mut *state.provenance;
        let mut prepared_regions = Vec::with_capacity(materialization_rows.len());
        self.runtime.commit_indexed(
            materialization_rows.len(),
            |slot| {
                let row = materialization_rows[slot];
                let _profile = self
                    .trace
                    .span(|| format!("initial_mapping.region[{row}].materialization"));
                let state_row = plan_rows.get(row).ok_or_else(|| {
                    crate::SynthError::invariant("regional artifact row is out of range")
                })?;
                let artifact = MappedRegionArtifact::from_library_plan(
                    &state_row.plan,
                    &state_row.binding,
                    region_ownership,
                    &mapped.signals,
                    self.combinational_catalog(),
                    &self.config.options.target_cells,
                )?;
                if artifact.region() != state_row.plan.region() {
                    return Err(crate::SynthError::invariant(
                        "mapped artifact belongs to another synthesis region",
                    ));
                }
                Ok::<_, crate::SynthError>((row, artifact))
            },
            |_, (row, artifact)| {
                let origins = provenance.origins_for_operation_cover(
                    module,
                    artifact.roots(),
                    artifact.leaves(),
                )?;
                prepared_regions.push(PreparedRegion {
                    row,
                    artifact,
                    origins,
                });
                Ok(())
            },
        )?;
        Ok(prepared_regions)
    }

    fn apply_regions(
        &self,
        mapped: &mut RegionalMappedState,
        regions: &[PreparedRegion],
        sequential: Option<&materialize::MappedSequentialArtifact>,
    ) -> Result<(), crate::SynthError> {
        let application_kind = if sequential.is_some() {
            "initial"
        } else {
            "replacement"
        };
        let mut cells = BTreeSet::new();
        let mut nets = BTreeSet::new();
        if let Some(sequential) = sequential {
            nets.extend(sequential.required_nets().iter().copied());
        }
        for region in regions {
            let previous = mapped
                .footprints
                .get(region.row)
                .ok_or_else(|| {
                    crate::SynthError::invariant("regional footprint row is out of range")
                })?
                .as_ref();
            cells.extend(region.artifact.required_cells(previous)?);
            nets.extend(region.artifact.required_nets(previous)?);
        }
        let removed = regions
            .iter()
            .filter_map(|region| mapped.footprints[region.row].as_ref())
            .flat_map(MappedRegionFootprint::cells)
            .copied()
            .collect::<BTreeSet<_>>();
        for &cell in &removed {
            if !matches!(
                mapped
                    .cell_sources
                    .get(cell.index())
                    .and_then(Option::as_ref),
                Some(MappedCellSource::Region { .. })
            ) {
                return Err(crate::SynthError::invariant(
                    "replaced regional cell has no regional provenance source",
                ));
            }
        }
        let removed_census = mapped
            .implementation_census
            .as_ref()
            .map(|_| self.census_contribution(&mapped.netlist, removed.iter().copied()))
            .transpose()?;
        let snapshot = mapped
            .netlist
            .snapshot_region(cells, nets.iter().copied())
            .map_err(crate::SynthError::from)?;
        let mut delta = RegionDelta::new(snapshot);
        let pending_sequential = sequential
            .map(|sequential| sequential.append_to_delta(&mut delta))
            .transpose()?;
        let pending_regions = regions
            .iter()
            .map(|region| {
                let previous = mapped.footprints[region.row].as_ref();
                region
                    .artifact
                    .append_to_delta(&mut delta, previous)
                    .map(|pending| {
                        (
                            region.row,
                            region.artifact.region(),
                            region.origins,
                            pending,
                        )
                    })
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;

        let RegionalMappedState {
            netlist,
            connectivity,
            cell_sources,
            implementation_census,
            footprints,
            timing,
            ..
        } = mapped;
        let mut no_timing = [];
        let owners = timing.as_mut().map_or(
            no_timing.as_mut_slice(),
            crate::closure::mmmc::MmmcTiming::owners_mut,
        );
        let transaction =
            crate::closure::mapped_timing::MappedTimingTransaction::begin(netlist, owners, delta)?
                .ok_or_else(|| {
                    crate::SynthError::invariant("fresh regional mapped snapshot became stale")
                })?;
        let sequential_sources = match pending_sequential {
            Some(pending) => match pending.resolve(transaction.mapped_edit()) {
                Ok(sources) => sources,
                Err(error) => {
                    return transaction.abort(error, "initial sequential materialization");
                }
            },
            None => Box::new([]),
        };
        let mut publications = Vec::with_capacity(pending_regions.len());
        for (row, owner, origins, pending) in pending_regions {
            let footprint = match pending.resolve(transaction.mapped_edit()) {
                Ok(footprint) => footprint,
                Err(error) => return transaction.abort(error, "regional materialization"),
            };
            let artifact = regions
                .iter()
                .find(|region| region.row == row)
                .map(|region| &region.artifact)
                .ok_or_else(|| {
                    crate::SynthError::invariant("regional publication lost its prepared artifact")
                })?;
            if let Err(error) = artifact.validate_materialization(&footprint, transaction.mapped())
            {
                return transaction.abort(error, "regional artifact materialization");
            }
            publications.push((row, owner, origins, footprint));
        }

        let mut new_sources = sequential_sources.into_vec();
        for (_, owner, origins, footprint) in &publications {
            new_sources.extend(footprint.cells().iter().copied().map(|cell| {
                (
                    cell,
                    MappedCellSource::Region {
                        origins: *origins,
                        owner: *owner,
                    },
                )
            }));
        }
        new_sources.sort_unstable_by_key(|(cell, _)| *cell);
        if new_sources.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return transaction.abort(
                crate::SynthError::invariant("regional publication contains duplicate cells"),
                "regional publication",
            );
        }
        for &(cell, _) in &new_sources {
            if !transaction.mapped().is_live_cell(cell)
                || cell_sources.get(cell.index()).is_some_and(Option::is_some)
            {
                return transaction.abort(
                    crate::SynthError::invariant(
                        "new regional cell collides with existing provenance",
                    ),
                    "regional publication",
                );
            }
        }
        let next_census = match (implementation_census.as_ref(), removed_census.as_ref()) {
            (Some(current), Some(removed)) => {
                let added = match self.census_contribution(
                    transaction.mapped(),
                    new_sources.iter().map(|(cell, _)| *cell),
                ) {
                    Ok(added) => added,
                    Err(error) => return transaction.abort(error, "regional publication"),
                };
                match current.replaced(removed, &added) {
                    Ok(census) => Some(census),
                    Err(error) => return transaction.abort(error, "regional publication"),
                }
            }
            (None, None) => None,
            _ => {
                return transaction.abort(
                    crate::SynthError::invariant(
                        "regional publication lost its incremental census state",
                    ),
                    "regional publication",
                );
            }
        };
        transaction.commit_with("regional publication", |netlist, _| {
            connectivity
            .validate_affected(
                netlist,
                &self.config.options.target_cells,
                nets.iter().copied(),
            )
            .map_err(|error| {
                crate::SynthError::invariant(format!(
                    "{application_kind} regional transaction produced invalid connectivity: {error}"
                ))
            })?;
            cell_sources.resize(netlist.cell_slot_count(), None);
            for cell in removed {
                cell_sources[cell.index()] = None;
            }
            for (cell, source) in new_sources {
                cell_sources[cell.index()] = Some(source);
            }
            for (row, _, _, footprint) in publications {
                footprints[row] = Some(footprint);
            }
            if let Some(census) = next_census {
                *implementation_census = Some(census);
            }
            Ok(())
        })
    }

    /// Folds this epoch's measurements back into the contracts and reports the
    /// rows whose contracts actually moved.
    ///
    /// This reads only regional state, never the immutable Word module, so the
    /// caller can decide whether another epoch is worth committing before it
    /// changes the shared mapped generation.
    fn reallocate_contracts(
        state: &mut RegionalMappingState<'_>,
        dirty: &[crate::RegionRowId],
        epoch: u32,
    ) -> Result<Box<[crate::RegionRowId]>, crate::SynthError> {
        state
            .contracts
            .reallocate_dirty(dirty, state.rows.iter().map(|row| &row.plan), epoch)
    }

    /// Rebinds measured contracts without reopening frozen regional topology.
    fn refresh_contracts(
        &self,
        state: &mut RegionalMappingState<'_>,
        dirty: &[crate::RegionRowId],
    ) -> Result<(), crate::SynthError> {
        let contexts = dirty
            .iter()
            .copied()
            .map(|row| {
                self.region_context(state, row)
                    .map(|context| (row, context))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        for (row, context) in contexts {
            let index = row.index();
            let plan = state.rows[index]
                .plan
                .clone()
                .with_context_and_contracts(context, state.contracts.contracts(row).to_vec());
            state.journal_compacted_plan(index, &plan)?;
            state.rows[index].plan = plan;
        }
        Ok(())
    }

    fn region_context(
        &self,
        state: &RegionalMappingState<'_>,
        row: crate::RegionRowId,
    ) -> Result<crate::RegionContextKey, crate::SynthError> {
        let region = self
            .regions
            .region(row)
            .ok_or_else(|| crate::SynthError::invariant("dirty regional row is out of range"))?;
        let predecessors = self
            .regions
            .predecessors(region)
            .iter()
            .map(|&predecessor| {
                self.regions
                    .region(predecessor)
                    .ok_or_else(|| {
                        crate::SynthError::invariant("regional predecessor row is out of range")
                    })
                    .map(|region| region.revision().bytes())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::RegionContextKey::seal(
            region.revision(),
            state.contracts.contracts(row),
            self.config.scenarios.generation(),
            self.config
                .options
                .target_cells
                .content_fingerprint()
                .bytes(),
            self.config.effort,
            &predecessors,
        ))
    }
}

impl RegionalMappedState {
    fn finish(
        self,
    ) -> Result<(MappedOutput, Option<crate::closure::mmmc::MmmcTiming>), crate::SynthError> {
        let Self {
            netlist,
            cell_sources,
            timing,
            ..
        } = self;
        let mut sources = Vec::with_capacity(netlist.cell_count());
        for cell in netlist.cell_ids() {
            let source = cell_sources
                .get(cell.index())
                .and_then(Option::as_ref)
                .copied()
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "final mapped generation has a cell without provenance",
                    )
                })?;
            sources.push((cell, source));
        }
        if cell_sources.iter().enumerate().any(|(index, source)| {
            source.is_some()
                && CellId::from_index(index)
                    .ok()
                    .is_none_or(|cell| !netlist.is_live_cell(cell))
        }) {
            return Err(crate::SynthError::invariant(
                "final mapped provenance retains a tombstoned cell",
            ));
        }
        Ok((
            MappedOutput {
                netlist,
                cell_sources: sources.into_boxed_slice(),
            },
            timing,
        ))
    }
}
