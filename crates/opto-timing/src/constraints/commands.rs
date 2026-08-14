// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::index::*;
use super::*;

mod clocks;
mod environment;
mod exceptions;
mod removal;
mod sdc;

impl TimingContext {
    /// Creates an empty constraint context at the initial revision.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a nested transaction without copying constraint storage.
    ///
    /// The returned token must be committed or rolled back. Closing an
    /// ancestor also closes every checkpoint nested beneath it.
    pub fn checkpoint(&mut self) -> TimingCheckpoint {
        if self.transactions.is_empty() {
            debug_assert!(self.journal.is_empty());
        }
        let identity = opto_core::OwnerToken::fresh();
        self.transactions.push(identity.clone());
        TimingCheckpoint {
            owner: self.owner.clone(),
            identity,
            journal_len: self.journal.len(),
            revision: self.revision,
        }
    }

    /// Checks checkpoint ownership and liveness without changing this context.
    ///
    /// # Errors
    ///
    /// Returns [`TimingError::CheckpointOwnerMismatch`] for a token created by
    /// another context, or [`TimingError::StaleCheckpoint`] after the token or
    /// one of its ancestors has already been closed.
    pub fn validate_checkpoint(
        &self,
        checkpoint: &TimingCheckpoint,
    ) -> Result<(), crate::TimingError> {
        self.checkpoint_position(checkpoint).map(|_| ())
    }

    /// Keeps changes since `checkpoint` and closes it and its descendants.
    ///
    /// An inner commit retains its inverse records so an active ancestor can
    /// still roll the whole transaction back. Committing the outermost
    /// checkpoint drops the journal.
    ///
    /// # Errors
    ///
    /// Returns [`TimingError::CheckpointOwnerMismatch`] for a foreign token or
    /// [`TimingError::StaleCheckpoint`] when the transaction is no longer live.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "committing consumes the checkpoint capability so callers cannot intentionally \
                  reuse a closed transaction token"
    )]
    pub fn commit_checkpoint(
        &mut self,
        checkpoint: TimingCheckpoint,
    ) -> Result<(), crate::TimingError> {
        let position = self.checkpoint_position(&checkpoint)?;
        self.transactions.truncate(position);
        if self.transactions.is_empty() {
            self.journal = Vec::new();
        }
        Ok(())
    }

    /// Replays inverse records back to `checkpoint` and closes its descendants.
    ///
    /// # Errors
    ///
    /// Returns [`TimingError::CheckpointOwnerMismatch`] for a foreign token or
    /// [`TimingError::StaleCheckpoint`] when the transaction is no longer live.
    ///
    /// # Panics
    ///
    /// Panics if a live checkpoint's validated journal boundary is violated by
    /// internal storage corruption.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "rolling back consumes the checkpoint capability so callers cannot intentionally \
                  reuse a closed transaction token"
    )]
    pub fn rollback_checkpoint(
        &mut self,
        checkpoint: TimingCheckpoint,
    ) -> Result<(), crate::TimingError> {
        let position = self.checkpoint_position(&checkpoint)?;
        while self.journal.len() > checkpoint.journal_len {
            let undo = self
                .journal
                .pop()
                .expect("checkpoint validation bounded the timing journal");
            self.apply_undo(undo);
        }
        self.revision = checkpoint.revision;
        self.transactions.truncate(position);
        if self.transactions.is_empty() {
            debug_assert!(self.journal.is_empty());
            self.journal = Vec::new();
        }
        Ok(())
    }

    fn checkpoint_position(
        &self,
        checkpoint: &TimingCheckpoint,
    ) -> Result<usize, crate::TimingError> {
        if !self.owner.same_owner(&checkpoint.owner) {
            return Err(crate::TimingError::CheckpointOwnerMismatch);
        }
        let position = self
            .transactions
            .iter()
            .position(|identity| identity.same_owner(&checkpoint.identity))
            .ok_or(crate::TimingError::StaleCheckpoint)?;
        if checkpoint.journal_len > self.journal.len() {
            return Err(crate::TimingError::StaleCheckpoint);
        }
        Ok(position)
    }

    fn record_undo(&mut self, undo: TimingUndo) {
        if !self.transactions.is_empty() {
            self.journal.push(undo);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive inverse-record dispatch is the transaction journal's single \
                  restoration table; keeping it centralized makes coverage auditable"
    )]
    fn apply_undo(&mut self, undo: TimingUndo) {
        match undo {
            TimingUndo::ClockInserted(insertion) => {
                let slot = ClockSlot(insertion.slot());
                let clock = self.clocks.undo_insertion(insertion);
                self.remove_references(clock_references(slot, &clock));
                self.clock_slots.remove(&clock.id);
            }
            TimingUndo::ClockRemoved(removal) => {
                let slot = ClockSlot(removal.slot());
                self.clocks.restore_removal(removal);
                let clock = self
                    .clocks
                    .get_slot(slot.raw())
                    .expect("a restored clock row is live");
                self.clock_slots.insert(clock.id, slot);
                add_index_references(&mut self.references, clock_references(slot, clock));
            }
            TimingUndo::ClockReplaced { slot, previous } => {
                let current = self.clocks.replace(slot.raw(), previous);
                self.remove_references(clock_references(slot, &current));
                let restored = self
                    .clocks
                    .get_slot(slot.raw())
                    .expect("a restored clock row is live");
                add_index_references(&mut self.references, clock_references(slot, restored));
            }
            TimingUndo::InputTransition { port, previous } => {
                restore_map_value(
                    &mut self.input_transitions,
                    &mut self.references,
                    port,
                    previous,
                    TimingReference::InputTransition,
                );
            }
            TimingUndo::Load { port, previous } => {
                restore_map_value(
                    &mut self.loads,
                    &mut self.references,
                    port,
                    previous,
                    TimingReference::Load,
                );
            }
            TimingUndo::Resistance { endpoint, previous } => {
                if let Some(previous) = previous {
                    self.resistances.insert(endpoint, previous);
                    self.add_reference(endpoint.object_id(), TimingReference::Resistance);
                } else {
                    self.resistances.remove(&endpoint);
                    self.remove_reference(endpoint.object_id(), TimingReference::Resistance);
                }
            }
            TimingUndo::InputDelays { port, previous } => {
                self.restore_io_delays(port, previous, IoDelayKind::Input);
            }
            TimingUndo::OutputDelays { port, previous } => {
                self.restore_io_delays(port, previous, IoDelayKind::Output);
            }
            TimingUndo::ClockUncertainty { key, previous } => {
                let current = self.clock_uncertainties.remove(&key);
                if current.is_some() {
                    self.remove_references(clock_uncertainty_references(key));
                }
                if let Some(previous) = previous {
                    self.clock_uncertainties.insert(key, previous);
                    add_index_references(&mut self.references, clock_uncertainty_references(key));
                }
            }
            TimingUndo::CaseAnalysis { endpoint, previous } => {
                if let Some(previous) = previous {
                    self.case_analysis.insert(endpoint, previous);
                    self.add_reference(endpoint.object_id(), TimingReference::CaseAnalysis);
                } else {
                    self.case_analysis.remove(&endpoint);
                    self.remove_reference(endpoint.object_id(), TimingReference::CaseAnalysis);
                }
            }
            TimingUndo::DisabledTimingInserted(disabled) => {
                self.disabled_timing.remove(&disabled);
                if !self
                    .disabled_timing
                    .iter()
                    .any(|stored| stored.target == disabled.target)
                {
                    self.remove_reference(
                        disabled.target.object_id(),
                        TimingReference::DisabledTiming(disabled.target),
                    );
                }
            }
            TimingUndo::DisabledTimingRemoved(disabled) => {
                let first_for_target = !self
                    .disabled_timing
                    .iter()
                    .any(|stored| stored.target == disabled.target);
                self.disabled_timing.insert(disabled.clone());
                if first_for_target {
                    self.add_reference(
                        disabled.target.object_id(),
                        TimingReference::DisabledTiming(disabled.target),
                    );
                }
            }
            TimingUndo::TimingDerates(previous) => {
                self.timing_derates = previous;
            }
            TimingUndo::PathExceptionInserted(insertion) => {
                let slot = PathExceptionSlot(insertion.slot());
                let constraint = self.path_exceptions.undo_insertion(insertion);
                self.remove_references(path_exception_references(slot, &constraint));
            }
            TimingUndo::PathExceptionRemoved(removal) => {
                let slot = PathExceptionSlot(removal.slot());
                self.path_exceptions.restore_removal(removal);
                let constraint = self
                    .path_exceptions
                    .get_slot(slot.raw())
                    .expect("a restored path-exception row is live");
                add_index_references(
                    &mut self.references,
                    path_exception_references(slot, constraint),
                );
            }
            TimingUndo::PathExceptionReplaced { slot, previous } => {
                let current = self.path_exceptions.replace(slot.raw(), previous);
                self.remove_references(path_exception_references(slot, &current));
                let restored = self
                    .path_exceptions
                    .get_slot(slot.raw())
                    .expect("a restored path-exception row is live");
                add_index_references(
                    &mut self.references,
                    path_exception_references(slot, restored),
                );
            }
            TimingUndo::DesignRuleInserted { kind, insertion } => {
                let slot = insertion.slot();
                let constraint = self.design_rule_arena_mut(kind).undo_insertion(insertion);
                self.remove_references(design_rule_references(
                    &constraint,
                    design_rule_reference(kind, slot),
                ));
            }
            TimingUndo::DesignRuleRemoved { kind, removal } => {
                let slot = removal.slot();
                self.design_rule_arena_mut(kind).restore_removal(removal);
                self.add_design_rule_references(kind, slot);
            }
            TimingUndo::DesignRuleReplaced {
                kind,
                slot,
                previous,
            } => {
                let current = self.design_rule_arena_mut(kind).replace(slot, previous);
                self.remove_references(design_rule_references(
                    &current,
                    design_rule_reference(kind, slot),
                ));
                self.add_design_rule_references(kind, slot);
            }
        }
    }

    #[must_use]
    /// Returns the revision used to reject stale derived state.
    pub fn revision(&self) -> RevisionId {
        self.revision
    }

    #[must_use]
    /// Returns whether any constraint can influence synthesis optimization.
    pub fn has_optimization_constraints(&self) -> bool {
        !self.clocks.is_empty()
            || !self.input_transitions.is_empty()
            || !self.loads.is_empty()
            || !self.resistances.is_empty()
            || !self.input_delays.is_empty()
            || !self.output_delays.is_empty()
            || !self.clock_uncertainties.is_empty()
            || !self.case_analysis.is_empty()
            || !self.disabled_timing.is_empty()
            || self.timing_derates != TimingDerates::default()
            || !self.path_exceptions.is_empty()
            || !self.max_transitions.is_empty()
            || !self.max_capacitances.is_empty()
            || !self.max_fanouts.is_empty()
    }

    #[must_use]
    /// Returns whether clocks or path-delay exceptions constrain STA paths.
    pub fn has_path_constraints(&self) -> bool {
        !self.clocks.is_empty()
            || !self.input_delays.is_empty()
            || !self.output_delays.is_empty()
            || !self.path_exceptions.is_empty()
    }

    pub(crate) fn clock_entries(&self) -> impl Iterator<Item = (ClockSlot, &Clock)> {
        self.clocks
            .entries()
            .map(|(slot, clock)| (ClockSlot(slot), clock))
    }

    /// Fingerprints every timing value visible to synthesis while excluding
    /// the mutation revision used only for stale-publication checks.
    ///
    /// # Panics
    ///
    /// Panics if serialization into the infallible in-memory hash writer fails;
    /// all members of the private fingerprint record have fixed encodings.
    #[must_use]
    pub fn synthesis_fingerprint(&self) -> TimingFingerprint {
        #[derive(Serialize)]
        struct SynthesisTimingInputs<'a> {
            clocks: &'a OrderedArena<Clock>,
            input_transitions: &'a BTreeMap<PortId, PortValueSlots>,
            loads: &'a BTreeMap<PortId, PortValueSlots>,
            resistances: &'a BTreeMap<TimingEndpoint, PortValueSlots>,
            input_delays: &'a BTreeMap<PortId, Vec<IoDelay>>,
            output_delays: &'a BTreeMap<PortId, Vec<IoDelay>>,
            clock_uncertainties: &'a BTreeMap<ClockUncertaintyKey, f64>,
            case_analysis: &'a BTreeMap<TimingEndpoint, CaseAnalysisValue>,
            disabled_timing: &'a BTreeSet<DisabledTiming>,
            timing_derates: TimingDerates,
            path_exceptions: &'a OrderedArena<PathException>,
            max_transitions: &'a OrderedArena<DesignRuleConstraint>,
            max_capacitances: &'a OrderedArena<DesignRuleConstraint>,
            max_fanouts: &'a OrderedArena<DesignRuleConstraint>,
        }

        let inputs = SynthesisTimingInputs {
            clocks: &self.clocks,
            input_transitions: &self.input_transitions,
            loads: &self.loads,
            resistances: &self.resistances,
            input_delays: &self.input_delays,
            output_delays: &self.output_delays,
            clock_uncertainties: &self.clock_uncertainties,
            case_analysis: &self.case_analysis,
            disabled_timing: &self.disabled_timing,
            timing_derates: self.timing_derates,
            path_exceptions: &self.path_exceptions,
            max_transitions: &self.max_transitions,
            max_capacitances: &self.max_capacitances,
            max_fanouts: &self.max_fanouts,
        };
        let mut digest = blake3::Hasher::new();
        digest.update(TIMING_FINGERPRINT_DOMAIN);
        opto_archive::encode_into_std_write(&inputs, &mut digest)
            .expect("validated timing synthesis inputs are serializable");
        TimingFingerprint(*digest.finalize().as_bytes())
    }

    #[must_use]
    /// Validates that every persisted object ID is live in `registry`.
    pub fn checkpoint_objects_are_valid(&self, registry: &opto_db::ObjectRegistry) -> bool {
        let resolves = |object| registry.resolve(object).is_some();
        self.clocks.iter().all(|clock| {
            resolves(clock.id.erase())
                && clock.sources.iter().all(|source| resolves(source.erase()))
                && clock.generated.as_ref().is_none_or(|generated| {
                    resolves(generated.master.erase()) && resolves(generated.source.erase())
                })
        }) && self
            .input_transitions
            .keys()
            .chain(self.loads.keys())
            .all(|port| resolves(port.erase()))
            && self
                .resistances
                .keys()
                .all(|endpoint| resolves(endpoint.object_id()))
            && self
                .input_delays
                .iter()
                .chain(&self.output_delays)
                .all(|(port, rows)| {
                    resolves(port.erase())
                        && rows
                            .iter()
                            .all(|row| row.clock.is_none_or(|clock| resolves(clock.erase())))
                })
            && self
                .clock_uncertainties
                .keys()
                .all(|key| resolves(key.from.erase()) && resolves(key.to.erase()))
            && self
                .case_analysis
                .keys()
                .all(|endpoint| resolves(endpoint.object_id()))
            && self
                .disabled_timing
                .iter()
                .all(|disabled| resolves(disabled.target.object_id()))
            && self.path_exceptions.iter().all(|constraint| {
                std::iter::once(&constraint.from)
                    .chain(constraint.through.iter())
                    .chain(std::iter::once(&constraint.to))
                    .flat_map(ExceptionFilter::objects)
                    .all(|endpoint| resolves(endpoint.object_id()))
            })
            && [
                &self.max_transitions,
                &self.max_capacitances,
                &self.max_fanouts,
            ]
            .into_iter()
            .flatten()
            .flat_map(|constraint| constraint.objects.iter())
            .all(|object| resolves(object.object_id()))
    }

    /// Diagnoses missing clocks, input delays, and constrained endpoints.
    ///
    /// The diagnostic is observational: it does not run propagation or mutate
    /// either the constraint context or the sealed timing model.
    #[must_use]
    pub fn analyze_check_timing(&self, model: &TimingModel) -> crate::CheckTimingAnalysis {
        analysis::check_timing(self, model)
    }

    pub(crate) fn clock_uncertainty(
        &self,
        from: ClockId,
        from_edge: TimingEdge,
        to: ClockId,
        to_edge: TimingEdge,
        delay_type: DelayType,
    ) -> f64 {
        self.clock_uncertainties
            .iter()
            .filter(|(key, _)| {
                key.from == from
                    && key.from_edge.matches(from_edge)
                    && key.to == to
                    && key.to_edge.matches(to_edge)
                    && key.delay_type == delay_type
            })
            .map(|(_, value)| *value)
            .max_by(f64::total_cmp)
            .unwrap_or(0.0)
    }

    pub(crate) fn case_analysis_allows(&self, endpoint: TimingEndpoint, edge: TimingEdge) -> bool {
        match self.case_analysis.get(&endpoint) {
            None => true,
            Some(CaseAnalysisValue::Zero | CaseAnalysisValue::One) => false,
            Some(CaseAnalysisValue::Rise) => edge == TimingEdge::Rise,
            Some(CaseAnalysisValue::Fall) => edge == TimingEdge::Fall,
        }
    }

    pub(crate) fn timing_endpoint_is_disabled(&self, endpoint: TimingEndpoint) -> bool {
        self.disabled_timing.iter().any(|disabled| {
            disabled.target == endpoint && disabled.from.is_none() && disabled.to.is_none()
        })
    }

    pub(crate) fn timing_arc_is_disabled(
        &self,
        targets: &[TimingEndpoint],
        from: &str,
        to: &str,
    ) -> bool {
        self.disabled_timing.iter().any(|disabled| {
            targets.contains(&disabled.target)
                && disabled.from.as_deref().is_none_or(|filter| filter == from)
                && disabled.to.as_deref().is_none_or(|filter| filter == to)
        })
    }

    pub(crate) fn timing_derate(
        &self,
        kind: TimingDerateKind,
        clock_path: bool,
        edge: TimingEdge,
        delay_type: DelayType,
    ) -> f64 {
        let early_late_index = usize::from(delay_type == DelayType::Max);
        self.timing_derates.0[kind.index()][usize::from(!clock_path)][early_late_index]
            [edge.index()]
    }

    pub(crate) fn resistance(
        &self,
        endpoint: TimingEndpoint,
        edge: TimingEdge,
        delay_type: DelayType,
    ) -> f64 {
        self.resistances
            .get(&endpoint)
            .and_then(|slots| slots.value(edge, delay_type))
            .unwrap_or(0.0)
    }

    pub(crate) fn path_exception_entries(
        &self,
    ) -> impl Iterator<Item = (PathExceptionSlot, &PathException)> {
        self.path_exceptions
            .entries()
            .map(|(slot, constraint)| (PathExceptionSlot(slot), constraint))
    }

    pub(crate) fn path_exception_by_slot(&self, slot: PathExceptionSlot) -> Option<&PathException> {
        self.path_exceptions.get_slot(slot.raw())
    }

    pub(crate) fn clock(&self, id: ClockId) -> Option<&Clock> {
        self.clock_slots
            .get(&id)
            .and_then(|slot| self.clocks.get_slot(slot.raw()))
    }

    pub(crate) fn clock_entry(&self, id: ClockId) -> Option<(ClockSlot, &Clock)> {
        self.clock_slots
            .get(&id)
            .copied()
            .and_then(|slot| self.clocks.get_slot(slot.raw()).map(|clock| (slot, clock)))
    }

    pub(crate) fn clock_by_slot(&self, slot: ClockSlot) -> Option<&Clock> {
        self.clocks.get_slot(slot.raw())
    }

    #[must_use]
    /// Returns the shortest period among clocks sourced by `source`.
    pub fn minimum_clock_period_on(&self, source: PortId) -> Option<f64> {
        self.clocks
            .iter()
            .filter(|clock| clock.sources.contains(&source))
            .map(|clock| clock.period)
            .min_by(f64::total_cmp)
    }

    /// Returns the tightest positive top-level delay budget that may guide
    /// synthesis before endpoint-specific static timing is available.
    #[must_use]
    pub fn minimum_synthesis_delay(&self) -> Option<f64> {
        self.clocks
            .iter()
            .map(|clock| clock.period)
            .chain(
                self.path_exceptions
                    .iter()
                    .filter_map(|constraint| match constraint.kind {
                        PathExceptionKind::MaxDelay { delay } => Some(delay),
                        _ => None,
                    }),
            )
            .filter(|delay| delay.is_finite() && *delay > 0.0)
            .min_by(f64::total_cmp)
    }

    #[must_use]
    /// Returns the configured input transition for `port`.
    pub fn input_transition_on(&self, port: PortId) -> Option<f64> {
        self.input_transitions
            .get(&port)
            .copied()
            .and_then(PortValueSlots::maximum)
    }

    #[must_use]
    /// Returns the configured external load for `port`.
    pub fn load_on(&self, port: PortId) -> Option<f64> {
        self.loads
            .get(&port)
            .copied()
            .and_then(PortValueSlots::maximum)
    }
}

fn set_port_value_slots(
    slots: &mut PortValueSlots,
    value: f64,
    edges: EdgeSelection,
    corners: CornerSelection,
) {
    for delay_type in [DelayType::Max, DelayType::Min] {
        if !corners.matches(delay_type) {
            continue;
        }
        for edge in TimingEdge::ALL {
            if edges.matches(edge) {
                slots.0[delay_type.index()][edge.index()] = Some(value);
            }
        }
    }
}

fn edge_selections_overlap(left: EdgeSelection, right: EdgeSelection) -> bool {
    TimingEdge::ALL
        .into_iter()
        .any(|edge| left.matches(edge) && right.matches(edge))
}

fn concrete_edge_selections(selection: EdgeSelection) -> &'static [EdgeSelection] {
    match selection {
        EdgeSelection::Rise => &[EdgeSelection::Rise],
        EdgeSelection::Fall => &[EdgeSelection::Fall],
        EdgeSelection::Both => &[EdgeSelection::Rise, EdgeSelection::Fall],
    }
}

fn generated_master_edge_time(master: &Clock, edge_number: u32) -> f64 {
    let zero_based = edge_number - 1;
    let cycle = zero_based / 2;
    let edge = if zero_based.is_multiple_of(2) {
        TimingEdge::Rise
    } else {
        TimingEdge::Fall
    };
    f64::from(cycle) * master.period + master.edge_time(edge)
}

fn reset_path_matches(existing: &PathException, replacement: &PathException) -> bool {
    let corner_overlaps = matches!(
        (existing.corner, replacement.corner),
        (ExceptionCorner::Both, _)
            | (_, ExceptionCorner::Both)
            | (ExceptionCorner::Setup, ExceptionCorner::Setup)
            | (ExceptionCorner::Hold, ExceptionCorner::Hold)
    );
    corner_overlaps
        && existing.from == replacement.from
        && existing.through == replacement.through
        && existing.to == replacement.to
        && existing.edges == replacement.edges
}
