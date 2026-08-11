// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! The regional mapping epoch driver.
//!
//! Seeds region plans, commits and measures one mapped generation at a time, and refines
//! the rows the coordinator marks dirty until the objective stops improving.

use super::{
    BestMapping, CombinationalCellCatalog, MappedCellSource, MappedObjective, MappedRegionArtifact,
    MappedRegionFootprint, MeasuredEpoch, RegionalIr, RegionalMappedState, RegionalMapper,
    RegionalMappingOutcome, RegionalPlans, SynthesisProgress, WordMappedSignals,
    boundary_observation_values, materialize, resolve_boundary_nets,
};
use crate::mapping::MappedOutput;
use opto_ir::mapped::{CellId, RegionDelta};
use std::collections::BTreeSet;

struct PreparedRegion {
    row: usize,
    artifact: MappedRegionArtifact,
    origins: crate::artifact::implementation::OriginSetId,
}

struct PreparedArtifact {
    row: usize,
    artifact: MappedRegionArtifact,
}

impl RegionalMapper<'_> {
    fn combinational_catalog(&self) -> &CombinationalCellCatalog {
        &self.config.mapping_context.combinational_catalog
    }

    /// Commits, measures, and refines regional plans until the coordinator
    /// stops asking for another epoch, then publishes the best candidate.
    pub(super) fn run_epochs(
        &self,
        ir: &mut RegionalIr<'_>,
        state: &mut RegionalPlans,
        observer: &mut dyn FnMut(SynthesisProgress),
    ) -> Result<RegionalMappingOutcome, crate::SynthError> {
        let mut coordinator = crate::regional::RegionalEpochCoordinator::new(self.config.effort);
        let mut mapped = self.build_initial_generation(ir, state)?;
        let mut best = None::<BestMapping>;
        loop {
            let epoch = coordinator.epoch();
            let (plans, bindings) = {
                let _profile = self
                    .trace
                    .span(|| format!("initial_mapping.epoch[{epoch}].snapshot"));
                (state.plans.clone(), state.bindings.clone())
            };
            let measured = self.measure_epoch(state, &mut mapped, &plans, epoch)?;
            let census = mapped.implementation_census.as_ref().ok_or_else(|| {
                crate::SynthError::invariant("mapped implementation census was not initialized")
            })?;
            let objective = MappedObjective::from_plans(
                &measured.plans,
                measured.global_dynamic_power,
                census.area(),
                census.managed_leakage(),
                census.managed_cell_count,
                census.static_key,
                measured.timing_quality,
            )?;
            let current_is_best = best
                .as_ref()
                .is_none_or(|best| objective.better_than(&best.objective));
            if current_is_best {
                best = Some(BestMapping {
                    objective,
                    plans: measured.plans.clone().into_boxed_slice(),
                    bindings: bindings.clone().into_boxed_slice(),
                });
            }
            let decision = coordinator.evaluate(&measured.plans);
            // A remap that moves no contract would re-measure identical plans
            // and spend an epoch of budget for nothing.
            let decision = match decision {
                crate::regional::EpochDecision::Remap(dirty) => {
                    state.plans = measured.plans;
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
                                    state.plans[index].payload().to_vec(),
                                    state.bindings[index].clone(),
                                )
                            })
                            .collect::<Vec<_>>();
                        self.refresh_contracts(state, &changed)?;
                        let topology_changed = previous
                            .into_iter()
                            .filter_map(|(index, payload, binding)| {
                                (state.plans[index].payload() != payload
                                    || state.bindings[index] != binding)
                                    .then_some(index)
                            })
                            .collect::<Vec<_>>();
                        self.replace_regions(ir, state, &mut mapped, &topology_changed)?;
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
                    if !current_is_best {
                        let changed = plans
                            .iter()
                            .zip(&bindings)
                            .zip(best.plans.iter().zip(&best.bindings))
                            .enumerate()
                            .filter_map(
                                |(
                                    index,
                                    ((current, binding), (checkpoint, checkpoint_binding)),
                                )| {
                                    (current.payload() != checkpoint.payload()
                                        || binding != checkpoint_binding)
                                        .then_some(index)
                                },
                            )
                            .collect::<Vec<_>>();
                        state.plans = best.plans.to_vec();
                        state.bindings = best.bindings.to_vec();
                        self.replace_regions(ir, state, &mut mapped, &changed)?;
                    }
                    let boundary_repair_schema =
                        crate::regional::BoundaryRepairSchema::new(self.regions, &state.plans)?;
                    self.restore_boundary_repairs(ir, &boundary_repair_schema, &mut mapped)?;
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
                        plans: best.plans,
                        plan_journal: state.take_plan_journal()?,
                        epochs: coordinator.completed_epochs(),
                        mapped,
                        timing,
                        boundary_repair_schema,
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
        ir: &mut RegionalIr<'_>,
        plans: &RegionalPlans,
    ) -> Result<RegionalMappedState, crate::SynthError> {
        let boundary_values = boundary_observation_values(self.regions, ir.region_ownership)?;
        let mut observed_values =
            materialize::region_delta::regional_binding_values(&plans.bindings).into_vec();
        observed_values.extend(materialize::sequential_binding_values(ir.module)?);
        observed_values.extend(
            boundary_values
                .iter()
                .flat_map(|(_, values)| values.iter().copied()),
        );
        observed_values.sort_unstable();
        observed_values.dedup();
        let materialize::MappedSubstrate {
            netlist,
            cell_sources: substrate_sources,
            observed_nets,
        } = materialize::build_mapped_substrate(materialize::MappedSubstrateRequest {
            module: ir.module,
            options: self.config.options,
            design_references: self.config.design_references,
            reference_ports: self.config.reference_ports,
            source_instances: self.config.source_instances,
            base_revision: self.config.base_revision,
            observed_values: &observed_values,
        })?;
        let signals =
            WordMappedSignals::from_observations(ir.module, &observed_values, &observed_nets)?;
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
            cell_sources,
            implementation_census: None,
            signals,
            boundary_nets: boundary_nets.into_boxed_slice(),
            footprints: std::iter::repeat_with(|| None)
                .take(plans.plans.len())
                .collect(),
            boundary_footprints: Vec::new(),
            timing: None,
        };
        let sequential = materialize::MappedSequentialArtifact::from_module(
            ir.module,
            &mapped.signals,
            self.regions,
            ir.region_ownership,
            &self.config,
        )?;
        let rows = (0..plans.plans.len()).collect::<Vec<_>>();
        let regions = self.prepare_regions(ir, plans, &mapped, &rows)?;
        self.apply_regions(&mut mapped, &regions, Some(&sequential))?;
        let census = self.full_implementation_census(ir, &mapped)?;
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
    fn measure_epoch(
        &self,
        state: &RegionalPlans,
        mapped: &mut RegionalMappedState,
        plans: &[crate::RegionCoverPlan],
        epoch: u32,
    ) -> Result<MeasuredEpoch, crate::SynthError> {
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
        Ok(MeasuredEpoch {
            plans,
            global_dynamic_power,
            timing_quality,
        })
    }

    fn replace_regions(
        &self,
        ir: &mut RegionalIr<'_>,
        state: &RegionalPlans,
        mapped: &mut RegionalMappedState,
        rows: &[usize],
    ) -> Result<(), crate::SynthError> {
        if rows.is_empty() {
            return Ok(());
        }
        let regions = self.prepare_regions(ir, state, mapped, rows)?;
        self.apply_regions(mapped, &regions, None)
    }

    fn prepare_regions(
        &self,
        ir: &mut RegionalIr<'_>,
        state: &RegionalPlans,
        mapped: &RegionalMappedState,
        rows: &[usize],
    ) -> Result<Vec<PreparedRegion>, crate::SynthError> {
        if rows.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(crate::SynthError::invariant(
                "regional materialization rows are not strictly ordered",
            ));
        }
        let module = ir.module;
        let region_ownership = ir.region_ownership;
        let provenance = &mut *ir.provenance;
        let mut prepared_regions = Vec::with_capacity(rows.len());
        self.runtime.commit_indexed(
            rows.len(),
            |slot| {
                let row = rows[slot];
                let _profile = self
                    .trace
                    .span(|| format!("initial_mapping.region[{row}].materialization"));
                let plan = state.plans.get(row).ok_or_else(|| {
                    crate::SynthError::invariant("regional artifact row is out of range")
                })?;
                let binding = state.bindings.get(row).ok_or_else(|| {
                    crate::SynthError::invariant("regional artifact binding is out of range")
                })?;
                let artifact = MappedRegionArtifact::from_library_plan(
                    plan,
                    binding,
                    region_ownership,
                    &mapped.signals,
                    self.combinational_catalog(),
                    &self.config.options.target_cells,
                )?;
                if artifact.region() != plan.region() {
                    return Err(crate::SynthError::invariant(
                        "mapped artifact belongs to another synthesis region",
                    ));
                }
                Ok::<_, crate::SynthError>(PreparedArtifact { row, artifact })
            },
            |_, prepared| {
                let origins = provenance.origins_for_operation_cover(
                    module,
                    prepared.artifact.roots(),
                    prepared.artifact.leaves(),
                )?;
                prepared_regions.push(PreparedRegion {
                    row: prepared.row,
                    artifact: prepared.artifact,
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
            .snapshot_region(cells, nets)
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

    fn restore_boundary_repairs(
        &self,
        ir: &mut RegionalIr<'_>,
        schema: &crate::regional::BoundaryRepairSchema,
        mapped: &mut RegionalMappedState,
    ) -> Result<(), crate::SynthError> {
        use materialize::boundary_delta::PreparedBoundaryRepair;

        if self.boundary_repairs.is_empty() {
            return Ok(());
        }
        if !mapped.boundary_footprints.is_empty() {
            return Err(crate::SynthError::invariant(
                "boundary repairs were restored more than once into one mapped generation",
            ));
        }
        let prepared = PreparedBoundaryRepair::prepare_all(
            self.boundary_repairs,
            schema,
            &mapped.netlist,
            &mapped.cell_sources,
            ir.provenance,
            &self.config.options.target_cells,
        )?;
        if prepared.is_empty() {
            return Ok(());
        }
        let cells = prepared
            .iter()
            .flat_map(|repair| repair.required_cells().iter().copied())
            .collect::<BTreeSet<_>>();
        let nets = prepared
            .iter()
            .flat_map(|repair| repair.required_nets().iter().copied())
            .collect::<BTreeSet<_>>();
        let snapshot = mapped
            .netlist
            .snapshot_region(cells, nets)
            .map_err(crate::SynthError::from)?;
        let mut delta = RegionDelta::new(snapshot);
        let pending = prepared
            .iter()
            .map(|repair| repair.append_to_delta(&mut delta))
            .collect::<Result<Vec<_>, _>>()?;

        let RegionalMappedState {
            netlist,
            cell_sources,
            boundary_footprints,
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
                    crate::SynthError::invariant(
                        "fresh boundary-repair restore snapshot became stale",
                    )
                })?;
        let publications = pending
            .into_iter()
            .map(|pending| pending.resolve(transaction.mapped_edit()))
            .collect::<Result<Vec<_>, _>>();
        let publications = match publications {
            Ok(publications) => publications,
            Err(error) => return transaction.abort(error, "boundary-repair restore"),
        };
        let mut sources = publications
            .iter()
            .flat_map(|publication| publication.sources.iter().copied())
            .collect::<Vec<_>>();
        sources.sort_unstable_by_key(|(cell, _)| *cell);
        if sources.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            || sources.iter().any(|&(cell, _)| {
                !transaction.mapped().is_live_cell(cell)
                    || cell_sources.get(cell.index()).is_some_and(Option::is_some)
            })
        {
            return transaction.abort(
                crate::SynthError::invariant(
                    "boundary-repair publication collides with mapped provenance",
                ),
                "boundary-repair restore",
            );
        }
        for publication in &publications {
            if let Err(error) = publication
                .footprint
                .validate_generation(transaction.mapped())
            {
                return transaction.abort(error, "boundary-repair restore");
            }
        }
        transaction.commit_with("boundary-repair restore", |netlist, _| {
            cell_sources.resize(netlist.cell_slot_count(), None);
            for (cell, source) in sources {
                cell_sources[cell.index()] = Some(source);
            }
            boundary_footprints.extend(
                publications
                    .into_iter()
                    .map(|publication| publication.footprint),
            );
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
        state: &mut RegionalPlans,
        dirty: &[crate::RegionRowId],
        epoch: u32,
    ) -> Result<Box<[crate::RegionRowId]>, crate::SynthError> {
        let plans = std::mem::take(&mut state.plans);
        let changed = state.contracts.reallocate_dirty(dirty, &plans, epoch);
        state.plans = plans;
        changed
    }

    /// Rebinds measured contracts without reopening frozen regional topology.
    fn refresh_contracts(
        &self,
        state: &mut RegionalPlans,
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
            state.contexts[row.index()] = context;
        }
        for &row in dirty {
            let index = row.index();
            let plan = state.plans[index].clone().with_context_and_contracts(
                state.contexts[index],
                state.contracts.contracts(row).to_vec(),
            );
            state.journal_compacted_plan(index, &plan)?;
            state.plans[index] = plan;
        }
        Ok(())
    }

    fn region_context(
        &self,
        state: &RegionalPlans,
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
        for footprint in &self.boundary_footprints {
            footprint.validate_generation(&self.netlist)?;
            footprint.validate_sources(&self.cell_sources)?;
        }
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
