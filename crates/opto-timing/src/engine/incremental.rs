// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional dirty-cone timing updates for speculative mapped edits.
//!
//! A region update mutates the timing topology, propagation state, endpoint
//! closure, and electrical-rule index as one logical transaction. [`RegionEdit`]
//! owns the inverse data for all four layers. Callers must either commit it or
//! roll it back before applying an unrelated edit.

use crate::analysis::{
    ClosureEdit, ClosureIndex, PropagationState, all_net_timing_states,
    analyze_propagation_quality, append_propagation_net, compact_paths_if_needed,
    electrical_snapshot as build_electrical_snapshot, net_timing_state, nets_with_slack_at_most,
    propagate_all, propagate_all_with_path_tracking, propagation_net_count,
    remove_last_propagation_net, restore_propagation, synchronize_required_from_nets,
    update_propagation_from_nets,
};
use crate::{
    DesignRuleKind, DesignRuleSummary, DesignRuleViolation, NetTimingState, PinTimingState,
    ReportTimingOptions, TargetCellRef, TargetPinDirection, TimingAnalysis, TimingContext,
    TimingEdge, TimingInstanceId, TimingModel, TimingQuality,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod design_rules;
use design_rules::{DesignRuleEdit, DesignRuleIndex, DesignRuleInputs, design_rule_kinds};

mod memory;
mod transaction;
pub use memory::IncrementalTimingMemory;

#[derive(Debug)]
/// Mutable timing session optimized for transactional region evaluation.
///
/// The session retains one shared constraint context and owns its timing model with
/// propagation, closure, and DRC state for the same generation. It is
/// intentionally single-owner; parallelism happens inside bounded propagation.
pub struct IncrementalTiming {
    timing: Arc<TimingContext>,
    pub(super) model: TimingModel,
    options: ReportTimingOptions,
    propagation: PropagationState,
    closure: ClosureIndex,
    constraints: ConstraintIndex,
    design_rules: DesignRuleIndex,
    required_dirty: Vec<bool>,
    region_edit_active: bool,
    region_commit_prepared: bool,
    runtime: Option<opto_runtime::ExecutionContext>,
    construction_scratch_high_water_bytes: usize,
    construction_high_water_bytes: usize,
    electrical_snapshot: Mutex<Option<crate::TimingElectricalSnapshot>>,
}

#[derive(Debug)]
struct ConstraintIndex {
    design_rule_limits: BTreeMap<DesignRuleKind, Vec<Option<f64>>>,
    has_design_rule_limits: bool,
}

impl ConstraintIndex {
    fn owned_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<(DesignRuleKind, Vec<Option<f64>>, [usize; 4])>(
            self.design_rule_limits.len(),
        )
        .saturating_add(
            self.design_rule_limits
                .values()
                .map(|limits| opto_core::resident::slice_bytes::<Option<f64>>(limits.len()))
                .sum::<usize>(),
        )
    }

    fn build(timing: &TimingContext, model: &TimingModel) -> Self {
        // Synth object-scoped constraints once into dense per-net limits.
        // Keeping this projection outside the propagation loop avoids repeated
        // database-object and clock-scope resolution on every candidate edit.
        if design_rule_kinds()
            .into_iter()
            .all(|kind| timing.design_rule_constraints(kind).is_empty())
        {
            return Self {
                design_rule_limits: BTreeMap::new(),
                has_design_rule_limits: false,
            };
        }
        let net_count = model.graph.net_count();
        let mut design_rule_limits = BTreeMap::new();
        for kind in [
            DesignRuleKind::MaxTransition,
            DesignRuleKind::MaxCapacitance,
            DesignRuleKind::MaxFanout,
        ] {
            let mut limits = vec![None; net_count];
            for constraint in timing.design_rule_constraints(kind) {
                for object in &constraint.objects {
                    let nets: Box<dyn Iterator<Item = usize> + '_> = match object {
                        crate::TimingObject::Design(id) if *id == model.design.id() => {
                            Box::new(0..net_count)
                        }
                        crate::TimingObject::Port { id, design, .. }
                            if *design == model.design.id() =>
                        {
                            Box::new(model.graph.port_nets(*id).iter().map(|net| net.index()))
                        }
                        crate::TimingObject::Clock(id) => {
                            let Some(clock) = timing.clock(*id) else {
                                continue;
                            };
                            Box::new(
                                model
                                    .graph
                                    .clock_scope_nets(&clock.sources, constraint.scope)
                                    .into_iter(),
                            )
                        }
                        _ => continue,
                    };
                    for net in nets {
                        let limit = &mut limits[net];
                        *limit = Some(limit.map_or(constraint.limit, |current: f64| {
                            current.min(constraint.limit)
                        }));
                    }
                }
            }
            design_rule_limits.insert(kind, limits);
        }
        let has_design_rule_limits = design_rule_limits
            .values()
            .any(|limits| limits.iter().any(Option::is_some));
        Self {
            design_rule_limits,
            has_design_rule_limits,
        }
    }

    fn design_rule_limit(&self, kind: DesignRuleKind, net: usize) -> Option<f64> {
        self.design_rule_limits
            .get(&kind)
            .and_then(|limits| limits.get(net))
            .copied()
            .flatten()
    }
}

#[must_use = "an applied timing region edit must be committed or rolled back"]
#[derive(Debug)]
/// Rollback journal for one applied [`TimingRegionDelta`](crate::TimingRegionDelta).
///
/// The token belongs to the session that created it and represents topology,
/// propagation, closure, DRC, and deferred-required-time changes together.
pub struct RegionEdit {
    edit: crate::InstanceRegionModelEdit,
    propagation: crate::analysis::PropagationEdit,
    closure: ClosureEdit,
    design_rules: DesignRuleEdit,
    required_dirty: Vec<usize>,
    recomputed_nets: usize,
    electrical_snapshot: Option<crate::TimingElectricalSnapshot>,
}

impl RegionEdit {
    #[must_use]
    /// Returns the number of recomputed timing nets.
    pub const fn recomputed_nets(&self) -> usize {
        self.recomputed_nets
    }
}

impl IncrementalTiming {
    #[must_use]
    /// Borrows the current generation-stamped model, including speculative
    /// topology edits owned by this timing transaction.
    pub fn model(&self) -> &TimingModel {
        &self.model
    }

    fn refresh_constraint_index(&mut self) {
        self.constraints = ConstraintIndex::build(&self.timing, &self.model);
    }

    /// Builds a session retaining detailed predecessor chains for reports.
    ///
    /// # Errors
    ///
    /// Returns an error if full propagation, endpoint closure, or DRC indexing
    /// cannot be constructed.
    pub fn new(
        timing: impl Into<Arc<TimingContext>>,
        model: TimingModel,
        options: ReportTimingOptions,
    ) -> Result<Self, crate::TimingError> {
        Self::new_with_path_tracking(timing.into(), model, options, true, None)
    }

    /// Builds an incremental engine that retains scalar arrival, transition,
    /// closure, and design-rule state without allocating report path chains
    /// during optimization. Detailed path queries rebuild predecessor chains
    /// on demand from the current model generation.
    ///
    /// # Errors
    ///
    /// Returns an error if propagation, endpoint closure, design-rule indexing,
    /// runtime execution, or construction memory accounting fails.
    pub fn new_for_optimization(
        timing: impl Into<Arc<TimingContext>>,
        model: TimingModel,
        options: ReportTimingOptions,
        runtime: opto_runtime::ExecutionContext,
    ) -> Result<Self, crate::TimingError> {
        Self::new_with_path_tracking(timing.into(), model, options, false, Some(runtime))
    }

    /// Builds an optimization engine with a separately limited construction
    /// context while retaining `runtime` for later incremental work.
    ///
    /// This lets an outer scheduler parallelize independent model builds
    /// without permanently imposing its nested serial limit on the completed
    /// engine.
    ///
    /// # Errors
    ///
    /// Returns an error if propagation, endpoint closure, design-rule indexing,
    /// build-context execution, or construction memory accounting fails.
    pub fn new_for_optimization_with_build_context(
        timing: impl Into<Arc<TimingContext>>,
        model: TimingModel,
        options: ReportTimingOptions,
        build_context: &opto_runtime::ExecutionContext,
        runtime: opto_runtime::ExecutionContext,
    ) -> Result<Self, crate::TimingError> {
        let mut engine = Self::new_with_path_tracking(
            timing.into(),
            model,
            options,
            false,
            Some(build_context.clone()),
        )?;
        engine.runtime = Some(runtime);
        Ok(engine)
    }

    fn new_with_path_tracking(
        timing: Arc<TimingContext>,
        model: TimingModel,
        options: ReportTimingOptions,
        track_paths: bool,
        runtime: Option<opto_runtime::ExecutionContext>,
    ) -> Result<Self, crate::TimingError> {
        let propagation = if track_paths {
            propagate_all(&timing, &model, &options)?
        } else {
            propagate_all_with_path_tracking(&timing, &model, &options, false, runtime.as_ref())?
        };
        let closure =
            ClosureIndex::build(&timing, &model, &options, &propagation, runtime.as_ref())?;
        let constraints = ConstraintIndex::build(&timing, &model);
        let design_rules = DesignRuleIndex::build(DesignRuleInputs {
            timing: &timing,
            model: &model,
            options: &options,
            propagation: &propagation,
            constraints: &constraints,
        });
        let net_count = propagation_net_count(&model);
        let construction_scratch_high_water_bytes = model.construction_scratch_high_water_bytes();
        let model_construction_high_water_bytes = model
            .resident_memory_bytes()
            .checked_add(construction_scratch_high_water_bytes)
            .ok_or(crate::TimingModelError::Capacity {
                resource: "timing model construction memory",
            })?;
        let mut timing = Self {
            timing,
            model,
            options,
            propagation,
            closure,
            constraints,
            design_rules,
            required_dirty: vec![false; net_count],
            region_edit_active: false,
            region_commit_prepared: false,
            runtime,
            construction_scratch_high_water_bytes,
            construction_high_water_bytes: 0,
            electrical_snapshot: Mutex::new(None),
        };
        timing.construction_high_water_bytes = timing
            .resident_memory_bytes()
            .max(model_construction_high_water_bytes);
        Ok(timing)
    }

    /// Reconstructs and returns the current worst timing path.
    ///
    /// # Errors
    ///
    /// Returns an error if no reportable path exists or path state is invalid.
    pub fn analyze(&self) -> Result<TimingAnalysis, crate::TimingError> {
        self.quality().map(TimingQuality::into_worst)
    }

    /// Computes detailed timing quality from retained propagation state.
    ///
    /// # Errors
    ///
    /// Returns an error if predecessor propagation must be rebuilt and fails,
    /// or if path reconstruction finds inconsistent tags or no valid endpoint.
    pub fn quality(&self) -> Result<TimingQuality, crate::TimingError> {
        if self.propagation.tracks_paths() {
            return analyze_propagation_quality(
                &self.timing,
                &self.model,
                &self.options,
                &self.propagation,
            );
        }
        let propagation = propagate_all_with_path_tracking(
            &self.timing,
            &self.model,
            &self.options,
            true,
            self.runtime.as_ref(),
        )?;
        analyze_propagation_quality(&self.timing, &self.model, &self.options, &propagation)
    }

    /// Returns allocation-light closure quality for optimization decisions,
    /// or `None` when this view constrains no path.
    pub fn quality_summary(&self) -> Option<crate::TimingQualitySummary> {
        self.closure.summary()
    }

    /// Returns the maximum and sum of selected data-endpoint path arrivals.
    ///
    /// This allocation-free, linear endpoint reduction excludes clock pulse
    /// width checks. Each endpoint contributes its retained worst-slack path;
    /// this is a tie-break for equal-cost optimization, not an alternative
    /// feasibility metric or a maximum over every possible timing tag.
    #[must_use]
    pub fn data_path_arrivals(&self) -> (f64, f64) {
        self.closure.data_arrivals()
    }

    /// Reclaims predecessor nodes left unreachable by committed incremental
    /// edits. Rollback journals must not be live when this is called.
    ///
    /// # Errors
    ///
    /// Returns an error while a region edit is active, or when compact path
    /// arenas cannot be allocated consistently.
    pub fn compact_paths_if_needed(&mut self) -> Result<(), crate::TimingError> {
        self.require_no_region_edit("compact timing paths")?;
        compact_paths_if_needed(&mut self.propagation)
    }

    /// Returns the linked cell name for a stable instance ID.
    pub fn instance_cell(&self, instance: TimingInstanceId) -> Option<&str> {
        self.model.instance_cell(instance)
    }

    /// Returns every combinational instance whose output participates in a
    /// timing cone at or below `slack_limit`. Design order is preserved so
    /// parallel candidate generation can merge deterministically.
    ///
    /// # Errors
    ///
    /// Returns an error if deferred required-time propagation cannot be
    /// synchronized before the critical frontier is queried.
    pub fn instances_with_slack_at_most(
        &mut self,
        slack_limit: f64,
    ) -> Result<Vec<TimingInstanceId>, crate::TimingError> {
        self.synchronize_requireds()?;
        let critical_nets = nets_with_slack_at_most(
            &self.model,
            &self.propagation,
            self.options.delay_type,
            slack_limit,
        );
        Ok(self
            .model
            .instances()
            .filter(|instance| {
                let Some(cell) = self.model.graph.cell(&self.model.library, instance.id()) else {
                    return false;
                };
                instance.connections().any(|connection| {
                    let Some(pin) = cell.pins().find(|pin| pin.name() == connection.pin) else {
                        return false;
                    };
                    matches!(
                        pin.direction(),
                        TargetPinDirection::Output | TargetPinDirection::Inout
                    ) && critical_nets.contains(&connection.net)
                })
            })
            .map(crate::TimingInstanceRef::id)
            .collect())
    }

    /// Returns persistent mapped identities for every net at or below
    /// `slack_limit`.
    ///
    /// This is the canonical timing frontier for synthesis transforms that
    /// operate on mapped nets. Consumers must not reconstruct it from the
    /// inputs of critical instances: doing so also selects unrelated clock,
    /// reset, scan, and enable nets incident to those instances.
    ///
    /// # Errors
    ///
    /// Returns an error if deferred required-time propagation cannot be
    /// synchronized before persistent mapped identities are collected.
    pub fn mapped_nets_with_slack_at_most(
        &mut self,
        slack_limit: f64,
    ) -> Result<Vec<opto_ir::mapped::NetId>, crate::TimingError> {
        self.synchronize_requireds()?;
        Ok(nets_with_slack_at_most(
            &self.model,
            &self.propagation,
            self.options.delay_type,
            slack_limit,
        )
        .into_iter()
        .filter_map(|net| self.model.mapped_net(net))
        .collect())
    }

    /// Returns current state for a net resolved by external name.
    pub fn net_state(&self, name: &str) -> Option<NetTimingState> {
        net_timing_state(
            &self.timing,
            &self.model,
            &self.propagation,
            self.options.delay_type,
            name,
        )
    }

    /// Returns current state for a net using its persistent mapped identity.
    #[must_use]
    pub fn mapped_net_state(&self, net: opto_ir::mapped::NetId) -> Option<NetTimingState> {
        let timing_net = self.model.mapped_timing_net(net)?;
        crate::analysis::net_timing_state_by_index(
            &self.timing,
            &self.model,
            &self.propagation,
            self.options.delay_type,
            timing_net.index(),
        )
    }

    /// Captures generation-stamped state for every graph net.
    pub fn net_states(&self) -> crate::TimingNetStates {
        crate::TimingNetStates::new(
            self.model.generation(),
            all_net_timing_states(
                &self.timing,
                &self.model,
                &self.propagation,
                self.options.delay_type,
            ),
        )
    }

    /// Returns the current compact electrical state without materializing
    /// timing-report rows or names.
    ///
    /// # Errors
    ///
    /// Returns an error if the compact electrical snapshot cannot be built from
    /// the retained propagation and topology state.
    pub fn electrical_snapshot(
        &self,
    ) -> Result<crate::TimingElectricalSnapshot, crate::TimingError> {
        let mut cached = match self.electrical_snapshot.lock() {
            Ok(cached) => cached,
            Err(poisoned) => {
                self.electrical_snapshot.clear_poison();
                poisoned.into_inner()
            }
        };
        if let Some(snapshot) = cached.as_ref() {
            return Ok(snapshot.clone());
        }
        let snapshot = build_electrical_snapshot(
            &self.timing,
            &self.model,
            &self.propagation,
            self.options.delay_type,
        )?;
        *cached = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Returns timing/electrical state for one named instance pin.
    pub fn pin_state(&self, instance: TimingInstanceId, pin: &str) -> Option<PinTimingState> {
        let instance_data = self.model.instance_ref(instance)?;
        let connection = instance_data
            .connections()
            .find(|connection| connection.pin == pin)?;
        let cell = self
            .model
            .graph
            .cell(&self.model.library, instance_data.id())?;
        let pin_data = cell.pins().find(|candidate| candidate.name() == pin)?;
        let net_name = self.model.net_name(connection.net)?;
        let net = self.net_state(&net_name)?;
        Some(PinTimingState {
            instance,
            name: format!("{}/{}", instance_data.name(), pin_data.name()),
            pin: pin_data.name().to_string(),
            net: net_name.into_owned(),
            direction: pin_data.direction(),
            arrival: net.arrival,
            transition: net.transition,
            capacitance: pin_data.design_input_capacitance(),
            fanout_load: pin_data.design_fanout_load(),
        })
    }

    /// Materializes timing/electrical state for all linked instance pins.
    pub fn pin_states(&self) -> Vec<PinTimingState> {
        let net_states = self
            .net_states()
            .into_iter()
            .map(|state| (state.name.clone(), state))
            .collect::<BTreeMap<_, _>>();
        let mut states = Vec::new();
        for instance in self.model.instances() {
            let Some(cell) = self.model.graph.cell(&self.model.library, instance.id()) else {
                continue;
            };
            let instance_name = instance.name();
            for connection in instance.connections() {
                let Some(pin) = cell.pins().find(|pin| pin.name() == connection.pin) else {
                    continue;
                };
                let Some(net_name) = self.model.net_name(connection.net) else {
                    continue;
                };
                let Some(net) = net_states.get(net_name.as_ref()) else {
                    continue;
                };
                states.push(PinTimingState {
                    instance: instance.id(),
                    name: format!("{instance_name}/{}", pin.name()),
                    pin: pin.name().to_string(),
                    net: net_name.into_owned(),
                    direction: pin.direction(),
                    arrival: net.arrival,
                    transition: net.transition,
                    capacitance: pin.design_input_capacitance(),
                    fanout_load: pin.design_fanout_load(),
                });
            }
        }
        states
    }

    /// Estimates a candidate replacement under the current input slew and load.
    ///
    /// Returns `None` if pins, arcs, or propagated states needed by the
    /// estimate are unavailable.
    pub fn estimate_cell(
        &self,
        instance: TimingInstanceId,
        candidate: TargetCellRef<'_>,
    ) -> Option<crate::CellTimingEstimate> {
        let instance = self.model.instance_ref(instance)?;
        let connections = instance
            .connections()
            .map(|connection| (connection.pin, connection.net))
            .collect::<BTreeMap<_, _>>();
        let input_capacitance = candidate
            .pins()
            .filter(|pin| {
                matches!(
                    pin.direction(),
                    TargetPinDirection::Input | TargetPinDirection::Inout
                )
            })
            .filter_map(opto_library::TargetPinRef::max_capacitance)
            .sum::<f64>();
        let mut delay = None::<f64>;
        let mut transition = None::<f64>;
        for output in candidate.pins().filter(|pin| {
            matches!(
                pin.direction(),
                TargetPinDirection::Output | TargetPinDirection::Inout
            )
        }) {
            let output_net = connections.get(output.name()).copied()?;
            let output_name = self.model.net_name(output_net)?;
            let output_load = self.net_state(&output_name)?.capacitance;
            for arc in output.timing_arcs() {
                let input_net = connections.get(arc.related_pin()).copied()?;
                let input_name = self.model.net_name(input_net)?;
                let input_transition = self.net_state(&input_name)?.transition;
                for edge in TimingEdge::ALL {
                    if let Some(value) = arc.delay_at(edge, input_transition, Some(output_load)) {
                        delay = Some(delay.map_or(value, |current| current.max(value)));
                    }
                    if let Some(value) =
                        arc.transition_at(edge, input_transition, Some(output_load))
                    {
                        transition = Some(transition.map_or(value, |current| current.max(value)));
                    }
                }
            }
        }
        Some(crate::CellTimingEstimate {
            delay: delay?,
            transition: transition.unwrap_or(f64::INFINITY),
            input_capacitance,
        })
    }

    /// Conservatively tests whether any characterized metric could improve.
    ///
    /// Missing correspondence or characterization returns `true`, ensuring this
    /// inexpensive filter never rejects a potentially useful replacement.
    #[allow(
        clippy::too_many_lines,
        reason = "the screening predicate evaluates the complete load, slew, arc, and slack bound \
                  so it cannot approve a replacement from a partial estimate"
    )]
    pub fn replacement_can_improve_timing(
        &self,
        instance: TimingInstanceId,
        candidate: TargetCellRef<'_>,
    ) -> bool {
        let Some(instance) = self.model.instance_ref(instance) else {
            return true;
        };
        let Some(current) = self.model.graph.cell(&self.model.library, instance.id()) else {
            return true;
        };
        let connections = instance
            .connections()
            .map(|connection| (connection.pin, connection.net))
            .collect::<BTreeMap<_, _>>();
        for candidate_pin in candidate.pins().filter(|pin| {
            matches!(
                pin.direction(),
                TargetPinDirection::Input | TargetPinDirection::Inout
            )
        }) {
            let Some(current_pin) = current
                .pins()
                .find(|pin| pin.name() == candidate_pin.name())
            else {
                return true;
            };
            if candidate_pin.capacitance().unwrap_or(0.0) < current_pin.capacitance().unwrap_or(0.0)
            {
                return true;
            }
        }
        let mut compared_arc = false;
        for candidate_output in candidate.pins().filter(|pin| {
            matches!(
                pin.direction(),
                TargetPinDirection::Output | TargetPinDirection::Inout
            )
        }) {
            let Some(current_output) = current
                .pins()
                .find(|pin| pin.name() == candidate_output.name())
            else {
                return true;
            };
            let Some(output_net) = connections.get(candidate_output.name()).copied() else {
                return true;
            };
            let Some(output_name) = self.model.net_name(output_net) else {
                return true;
            };
            let Some(output_state) = self.net_state(&output_name) else {
                return true;
            };
            for candidate_arc in candidate_output.timing_arcs() {
                let Some(current_arc) = current_output.timing_arcs().find(|arc| {
                    arc.related_pin() == candidate_arc.related_pin()
                        && arc.timing_type() == candidate_arc.timing_type()
                        && arc.timing_sense() == candidate_arc.timing_sense()
                }) else {
                    return true;
                };
                let Some(input_net) = connections.get(candidate_arc.related_pin()).copied() else {
                    return true;
                };
                let Some(input_name) = self.model.net_name(input_net) else {
                    return true;
                };
                let Some(input_state) = self.net_state(&input_name) else {
                    return true;
                };
                for edge in TimingEdge::ALL {
                    let candidate_delay = candidate_arc.delay_at(
                        edge,
                        input_state.transition,
                        Some(output_state.capacitance),
                    );
                    let current_delay = current_arc.delay_at(
                        edge,
                        input_state.transition,
                        Some(output_state.capacitance),
                    );
                    let candidate_transition = candidate_arc.transition_at(
                        edge,
                        input_state.transition,
                        Some(output_state.capacitance),
                    );
                    let current_transition = current_arc.transition_at(
                        edge,
                        input_state.transition,
                        Some(output_state.capacitance),
                    );
                    match (candidate_delay, current_delay) {
                        (Some(candidate), Some(current)) if candidate < current => return true,
                        (Some(_), Some(_)) => compared_arc = true,
                        _ => return true,
                    }
                    match (candidate_transition, current_transition) {
                        (Some(candidate), Some(current)) if candidate < current => return true,
                        (Some(_), Some(_)) | (None, None) => {}
                        _ => return true,
                    }
                }
            }
        }
        !compared_arc
    }

    /// Returns current electrical violations in deterministic severity order.
    pub fn design_rule_violations(&self) -> Vec<DesignRuleViolation> {
        self.design_rules.violations(&self.model)
    }

    #[must_use]
    /// Returns the incremental DRC objective without rescanning all nets.
    pub fn design_rule_summary(&self) -> DesignRuleSummary {
        self.design_rules.summary()
    }

    /// Applies a region delta after synchronizing deferred required times.
    ///
    /// # Errors
    ///
    /// On failure, all partially changed timing layers are rolled back. A
    /// rollback failure is returned as [`crate::TimingError::Rollback`].
    pub fn apply_region_delta(
        &mut self,
        delta: crate::TimingRegionDelta,
    ) -> Result<RegionEdit, crate::TimingError> {
        self.require_no_region_edit("apply another region edit")?;
        self.synchronize_requireds()?;
        self.apply_region_delta_inner(delta, false)
    }

    /// Applies a speculative optimization edit without recomputing the
    /// backward required-time cone. After commit, required times are
    /// synchronized by the next critical-frontier query. Closure evaluates
    /// each endpoint directly from current arrivals and constraints, so its
    /// `QoR` summary and the design-rule summary remain current immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if another region edit is active or any model,
    /// propagation, closure, design-rule, or cache update fails. Partial
    /// changes are rolled back; a rollback failure is wrapped in
    /// [`crate::TimingError::Rollback`].
    pub fn apply_optimization_region_delta(
        &mut self,
        delta: crate::TimingRegionDelta,
    ) -> Result<RegionEdit, crate::TimingError> {
        self.require_no_region_edit("apply another region edit")?;
        self.apply_region_delta_inner(delta, true)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the inner region transaction coordinates model, propagation, closure, cache, and \
                  metric journals and must either publish or roll them all back"
    )]
    fn apply_region_delta_inner(
        &mut self,
        delta: crate::TimingRegionDelta,
        defer_required: bool,
    ) -> Result<RegionEdit, crate::TimingError> {
        // Mutation order is deliberate:
        //   topology -> topological order -> propagation -> closure -> DRC.
        // Each later layer depends on the preceding one. Every fallible step
        // restores all earlier layers before returning, so the caller never
        // observes a half-applied region.
        let old_net_count = propagation_net_count(&self.model);
        let (edit, dirty) = self.model.apply_instance_region(delta)?;
        let changed_structure = edit.changes_structure();
        let affected_instances = edit.affected_instances().collect::<Vec<_>>();
        let new_net_count = propagation_net_count(&self.model);
        for appended in 0..new_net_count.saturating_sub(old_net_count) {
            // Region edits append arena entries but never renumber existing
            // nets. Mirror those appended slots before propagation runs.
            if let Err(error) = append_propagation_net(&mut self.propagation) {
                for _ in 0..appended {
                    remove_last_propagation_net(&mut self.propagation);
                    self.required_dirty.pop();
                }
                if let Err(rollback) = self.model.rollback_instance_region(edit) {
                    return Err(crate::TimingError::Rollback {
                        operation: "incremental timing propagation growth",
                        primary: Box::new(error),
                        rollback: Box::new(rollback),
                    });
                }
                return Err(error);
            }
            self.required_dirty.push(false);
        }
        if let Err(error) = self.model.graph.ensure_topological_order() {
            if let Err(rollback) = self.model.rollback_instance_region(edit) {
                return Err(crate::TimingError::Rollback {
                    operation: "incremental timing order update",
                    primary: Box::new(error),
                    rollback: Box::new(rollback),
                });
            }
            for _ in old_net_count..new_net_count {
                remove_last_propagation_net(&mut self.propagation);
                self.required_dirty.pop();
            }
            return Err(error);
        }
        let (recomputed_nets, propagation_edit) = match update_propagation_from_nets(
            &self.timing,
            &self.model,
            &self.options,
            &mut self.propagation,
            &dirty,
            defer_required,
            self.runtime.as_ref(),
        ) {
            Ok(recomputed) => recomputed,
            Err(error) => {
                if let Err(rollback) = self.model.rollback_instance_region(edit) {
                    return Err(crate::TimingError::Rollback {
                        operation: "incremental timing model update",
                        primary: Box::new(error),
                        rollback: Box::new(rollback),
                    });
                }
                for _ in old_net_count..new_net_count {
                    remove_last_propagation_net(&mut self.propagation);
                    self.required_dirty.pop();
                }
                return Err(error);
            }
        };
        let deferred_required = propagation_edit.deferred_required().to_vec();
        if changed_structure {
            self.refresh_constraint_index();
        }
        let changed_nets = propagation_edit.changed_nets();
        let closure = match self.closure.update(
            crate::analysis::ClosureUpdateContext {
                timing: &self.timing,
                model: &self.model,
                options: &self.options,
                propagation: &self.propagation,
                runtime: self.runtime.as_ref(),
            },
            &changed_nets,
            changed_structure.then_some(affected_instances.as_slice()),
        ) {
            Ok(closure) => closure,
            Err(error) => {
                if let Err(rollback) = self.model.rollback_instance_region(edit) {
                    return Err(crate::TimingError::Rollback {
                        operation: "incremental timing closure update",
                        primary: Box::new(error),
                        rollback: Box::new(rollback),
                    });
                }
                restore_propagation(&mut self.propagation, propagation_edit);
                for _ in old_net_count..new_net_count {
                    remove_last_propagation_net(&mut self.propagation);
                    self.required_dirty.pop();
                }
                if changed_structure {
                    self.refresh_constraint_index();
                }
                return Err(error);
            }
        };
        let design_rules = self.design_rules.update(
            DesignRuleInputs {
                timing: &self.timing,
                model: &self.model,
                options: &self.options,
                propagation: &self.propagation,
                constraints: &self.constraints,
            },
            &changed_nets,
            changed_structure,
        );
        let mut required_dirty = Vec::new();
        // Optimization evaluates many forward-delay candidates that never need
        // a graph-frontier slack query. Endpoint closure does not consume these
        // merged required rows. Record the exact deferred seeds once; the next
        // critical-frontier query synchronizes their reverse cone.
        for net in deferred_required {
            let dirty = self
                .required_dirty
                .get_mut(net)
                .expect("deferred required-time net must belong to the timing graph");
            if !*dirty {
                *dirty = true;
                required_dirty.push(net);
            }
        }
        self.region_edit_active = true;
        self.region_commit_prepared = false;
        let electrical_snapshot = self
            .electrical_snapshot
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        Ok(RegionEdit {
            edit,
            propagation: propagation_edit,
            closure,
            design_rules,
            required_dirty,
            recomputed_nets,
            electrical_snapshot,
        })
    }

    fn synchronize_requireds(&mut self) -> Result<(), crate::TimingError> {
        self.require_no_region_edit("synchronize required times")?;
        let seeds = self
            .required_dirty
            .iter()
            .enumerate()
            .filter_map(|(net, dirty)| dirty.then_some(net))
            .collect::<Vec<_>>();
        if seeds.is_empty() {
            return Ok(());
        }
        // Required times propagate against the current topology. Revalidating
        // order here also protects callers that deferred the backward phase
        // across several committed optimization edits.
        self.model.graph.ensure_topological_order()?;
        synchronize_required_from_nets(
            &self.timing,
            &self.model,
            &self.options,
            &mut self.propagation,
            &seeds,
            self.runtime.as_ref(),
        )?;
        self.required_dirty.fill(false);
        Ok(())
    }

    fn require_no_region_edit(&self, operation: &'static str) -> Result<(), crate::TimingError> {
        if self.region_edit_active {
            Err(crate::TimingEngineError::ActiveRegionEdit { operation }.into())
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(crate) fn closure_slot_counts(&self) -> (usize, usize) {
        self.closure.slot_counts()
    }

    #[cfg(test)]
    pub(crate) fn topological_generation(&self) -> u64 {
        self.model.graph.topological_generation()
    }
}
