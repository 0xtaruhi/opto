// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Clock creation and the constraints attached to a clock object.

use super::super::index::*;
use super::super::*;
use super::*;

impl TimingContext {
    /// Creates or replaces a clock while preserving transition overrides.
    ///
    /// # Errors
    ///
    /// Returns an error if revision allocation or compact arena insertion fails.
    pub fn create_clock(&mut self, id: ClockId, spec: ClockSpec) -> Result<(), crate::TimingError> {
        self.create_clock_with_add(id, spec, false)
    }

    /// Creates, replaces, or extends a clock source set.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision counter or compact clock arena is
    /// exhausted.
    ///
    /// # Panics
    ///
    /// Panics if the private clock-ID index points at a non-live arena row.
    pub fn create_clock_with_add(
        &mut self,
        id: ClockId,
        spec: ClockSpec,
        add: bool,
    ) -> Result<(), crate::TimingError> {
        let next_revision = self.next_revision()?;
        if let Some(slot) = self.clock_slots.get(&id).copied() {
            let current = self
                .clocks
                .get_slot(slot.raw())
                .expect("the clock index only references live rows");
            let sources = if add {
                current
                    .sources
                    .iter()
                    .copied()
                    .chain(spec.sources)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            } else {
                spec.sources
            };
            let replacement = Clock {
                id,
                name: spec.name,
                period: spec.period,
                sources: sources.into(),
                waveform: spec.waveform,
                comment: spec.comment,
                transitions: current.transitions,
                source_latencies: current.source_latencies,
                network_latencies: current.network_latencies,
                propagated: current.propagated,
                generated: None,
            };
            let previous = self.clocks.replace(slot.raw(), replacement);
            self.remove_references(clock_references(slot, &previous));
            let current = self
                .clocks
                .get_slot(slot.raw())
                .expect("the replaced clock row remains live");
            add_index_references(&mut self.references, clock_references(slot, current));
            self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        } else {
            let clock = Clock {
                id,
                name: spec.name,
                period: spec.period,
                sources: spec.sources.into(),
                waveform: spec.waveform,
                comment: spec.comment,
                transitions: [[None, None], [None, None]],
                source_latencies: [[[None, None], [None, None]]; 2],
                network_latencies: [[None, None], [None, None]],
                propagated: false,
                generated: None,
            };
            let insertion = self.clocks.insert_tracked(clock)?;
            let slot = ClockSlot(insertion.slot());
            self.clock_slots.insert(id, slot);
            let clock = self
                .clocks
                .get_slot(slot.raw())
                .expect("a newly inserted clock row is live");
            add_index_references(&mut self.references, clock_references(slot, clock));
            self.record_undo(TimingUndo::ClockInserted(insertion));
        }
        self.revision = next_revision;
        Ok(())
    }

    /// Creates a generated clock on top-level target ports.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty target set, a missing or cyclic master,
    /// incompatible transform options, a non-finite derived waveform, revision
    /// exhaustion, or compact-arena capacity exhaustion.
    ///
    /// # Panics
    ///
    /// Panics if a clock ID validated as live no longer resolves in the private
    /// clock arena during the same exclusive mutation.
    #[allow(
        clippy::too_many_lines,
        reason = "generated-clock creation validates the complete transform, ancestor chain, \
                  derived waveform, source ownership, and replacement journal before publication"
    )]
    pub fn create_generated_clock(
        &mut self,
        id: ClockId,
        name: String,
        targets: Vec<PortId>,
        generated: GeneratedClock,
        add: bool,
    ) -> Result<(), crate::TimingError> {
        if targets.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "create_generated_clock",
            }
            .into());
        }
        let modes = usize::from(generated.divide_by.is_some())
            + usize::from(generated.multiply_by.is_some())
            + usize::from(generated.edges.is_some());
        if modes > 1 || (generated.combinational && modes != 0) {
            return Err(crate::ConstraintError::InvalidGeneratedClockOptions.into());
        }
        if generated.divide_by == Some(0) || generated.multiply_by == Some(0) {
            return Err(crate::ConstraintError::InvalidGeneratedClockOptions.into());
        }
        if generated
            .duty_cycle
            .is_some_and(|duty| !duty.is_finite() || duty <= 0.0 || duty >= 100.0)
            || generated
                .edge_shift
                .is_some_and(|shifts| shifts.into_iter().any(|shift| !shift.is_finite()))
        {
            return Err(crate::ConstraintError::InvalidGeneratedClockOptions.into());
        }
        let mut ancestor = generated.master;
        let mut visited = BTreeSet::new();
        loop {
            if ancestor == id || !visited.insert(ancestor) {
                return Err(crate::ConstraintError::InvalidGeneratedClockOptions.into());
            }
            let Some(parent) = self
                .clock(ancestor)
                .and_then(|clock| clock.generated.as_ref())
                .map(|generated| generated.master)
            else {
                break;
            };
            ancestor = parent;
        }
        let master = self
            .clock(generated.master)
            .ok_or(crate::ConstraintError::ClockNotFound {
                id: generated.master,
            })?
            .clone();
        let (period, waveform) = if let Some(edges) = generated.edges {
            if !(edges[0] < edges[1] && edges[1] < edges[2]) {
                return Err(crate::ConstraintError::InvalidGeneratedClockOptions.into());
            }
            let shifts = generated.edge_shift.unwrap_or([0.0; 3]);
            let times = [
                generated_master_edge_time(&master, edges[0]) + shifts[0],
                generated_master_edge_time(&master, edges[1]) + shifts[1],
                generated_master_edge_time(&master, edges[2]) + shifts[2],
            ];
            let period = times[2] - times[0];
            let high = times[1] - times[0];
            if !period.is_finite() || !high.is_finite() || period <= 0.0 || high <= 0.0 {
                return Err(crate::ConstraintError::InvalidGeneratedClockOptions.into());
            }
            let waveform = if generated.invert {
                (high, period)
            } else {
                (0.0, high)
            };
            (period, waveform)
        } else {
            let divide = f64::from(generated.divide_by.unwrap_or(1));
            let multiply = f64::from(generated.multiply_by.unwrap_or(1));
            let period = master.period * divide / multiply;
            let duty = generated.duty_cycle.unwrap_or(50.0) / 100.0;
            let waveform = if generated.invert {
                (period * duty, period)
            } else {
                (0.0, period * duty)
            };
            (period, waveform)
        };
        let mut spec = ClockSpec::new(name, period, targets, Some(waveform))?;
        spec.comment.clone_from(&generated.comment);
        let generated_revision = self
            .next_revision()?
            .next()
            .map_err(crate::TimingError::Revision)?;
        self.create_clock_with_add(id, spec, add)?;
        let slot = *self
            .clock_slots
            .get(&id)
            .expect("the generated clock was just created");
        let previous = self
            .clocks
            .get_slot(slot.raw())
            .expect("the generated clock slot is live")
            .clone();
        self.clocks
            .get_slot_mut(slot.raw())
            .expect("the generated clock slot is live")
            .generated = Some(generated);
        let clock = self
            .clocks
            .get_slot(slot.raw())
            .expect("the generated clock slot is live");
        add_index_references(&mut self.references, clock_references(slot, clock));
        self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        self.revision = generated_revision;
        Ok(())
    }

    /// Returns the number of live clock constraints.
    #[must_use]
    pub fn clock_count(&self) -> usize {
        self.clocks.len()
    }

    /// Returns whether `id` names a live clock constraint.
    #[must_use]
    pub fn contains_clock(&self, id: ClockId) -> bool {
        self.clock_slots.contains_key(&id)
    }

    /// Returns whether `id` is a generated clock.
    #[must_use]
    pub fn is_generated_clock(&self, id: ClockId) -> bool {
        self.clock(id)
            .is_some_and(|clock| clock.generated.is_some())
    }

    /// Expands selected clocks to generated-clock dependents that cannot remain valid.
    #[must_use]
    pub fn clock_removal_closure(&self, clocks: &[ClockId]) -> BTreeSet<opto_db::AnyObjectId> {
        let mut removed = clocks.iter().map(|id| id.erase()).collect::<BTreeSet<_>>();
        loop {
            let mut changed = false;
            for clock in &self.clocks {
                if !removed.contains(&clock.id.erase())
                    && clock
                        .generated
                        .as_ref()
                        .is_some_and(|generated| removed.contains(&generated.master.erase()))
                {
                    changed |= removed.insert(clock.id.erase());
                }
            }
            if !changed {
                return removed;
            }
        }
    }

    /// Borrows live clocks in insertion order.
    #[must_use]
    pub fn clocks(&self) -> TimingRows<'_, Clock> {
        TimingRows::new(&self.clocks)
    }

    /// Return clock rows with source port IDs resolved at the session boundary.
    pub fn clock_report(
        &self,
        mut resolve_port: impl FnMut(PortId) -> Option<String>,
    ) -> Vec<crate::ClockReportRow> {
        self.clocks
            .iter()
            .map(|clock| crate::ClockReportRow {
                name: clock.name.clone(),
                period: clock.period,
                waveform: clock.waveform,
                sources: clock
                    .sources
                    .iter()
                    .filter_map(|source| resolve_port(*source))
                    .collect(),
            })
            .collect()
    }

    /// Sets selected rise/fall and min/max transition slots on clocks.
    ///
    /// Empty edge or delay selections mean both.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-finite transition, an empty clock selection,
    /// an unknown clock ID, or revision exhaustion.
    ///
    /// # Panics
    ///
    /// Panics if the private clock-ID index points at a non-live arena row.
    pub fn set_clock_transition(
        &mut self,
        transition: f64,
        edges: EdgeSelection,
        corners: CornerSelection,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        validate_timing_constraint("set_clock_transition", transition, clocks)?;
        let clock_slots = clocks
            .iter()
            .map(|id| {
                self.clock_slots
                    .get(id)
                    .copied()
                    .ok_or(crate::ConstraintError::ClockNotFound { id: *id })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_revision = self.next_revision()?;
        for slot in clock_slots {
            let previous = self
                .clocks
                .get_slot(slot.raw())
                .expect("the clock index only references live rows")
                .clone();
            let clock = self
                .clocks
                .get_slot_mut(slot.raw())
                .expect("the clock index only references live rows");
            for delay_type in [DelayType::Max, DelayType::Min] {
                if !corners.matches(delay_type) {
                    continue;
                }
                if edges.matches(TimingEdge::Rise) {
                    clock.transitions[delay_type.index()][TimingEdge::Rise.index()] =
                        Some(transition);
                }
                if edges.matches(TimingEdge::Fall) {
                    clock.transitions[delay_type.index()][TimingEdge::Fall.index()] =
                        Some(transition);
                }
            }
            self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Resolves clock identities to their storage slots, in slot order.
    fn resolve_clock_slots(
        &self,
        clocks: &[ClockId],
    ) -> Result<BTreeSet<ClockSlot>, crate::TimingError> {
        clocks
            .iter()
            .map(|id| {
                self.clock_slots
                    .get(id)
                    .copied()
                    .ok_or_else(|| crate::ConstraintError::ClockNotFound { id: *id }.into())
            })
            .collect()
    }

    /// Removes all explicitly configured rise/fall and min/max transitions
    /// from the selected clocks in one revisioned update.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty selection, an unknown clock ID, or
    /// revision exhaustion when at least one slot changes.
    ///
    /// # Panics
    ///
    /// Panics if the private clock-ID index points at a non-live arena row.
    pub fn unset_clock_transition(
        &mut self,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if clocks.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "unset_clock_transition",
            }
            .into());
        }
        let slots = self.resolve_clock_slots(clocks)?;
        let changed = slots.iter().copied().filter(|slot| {
            self.clocks
                .get_slot(slot.raw())
                .is_some_and(|clock| clock.transitions.iter().flatten().any(Option::is_some))
        });
        let changed = changed.collect::<Vec<_>>();
        if changed.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for slot in changed {
            let previous = self
                .clocks
                .get_slot(slot.raw())
                .expect("the clock index only references live rows")
                .clone();
            self.clocks
                .get_slot_mut(slot.raw())
                .expect("the clock index only references live rows")
                .transitions = [[None; 2]; 2];
            self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Sets selected clock source or network latency slots.
    ///
    /// # Errors
    ///
    /// Returns an error for non-finite delay, an empty or unknown clock
    /// selection, early/late qualifiers on network latency, or revision
    /// exhaustion.
    ///
    /// # Panics
    ///
    /// Panics if the private clock-ID index points at a non-live arena row.
    pub fn set_clock_latency(
        &mut self,
        delay: f64,
        source: bool,
        edges: EdgeSelection,
        corners: CornerSelection,
        side: LatencySide,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if !delay.is_finite() {
            return Err(crate::ConstraintError::InvalidValue {
                command: "set_clock_latency",
                value: delay,
            }
            .into());
        }
        if clocks.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "set_clock_latency",
            }
            .into());
        }
        if !source && side != LatencySide::Both {
            return Err(crate::ConstraintError::InvalidClockLatencySelection.into());
        }
        let clock_slots = clocks
            .iter()
            .map(|id| {
                self.clock_slots
                    .get(id)
                    .copied()
                    .ok_or(crate::ConstraintError::ClockNotFound { id: *id })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_revision = self.next_revision()?;
        for slot in clock_slots {
            let previous = self
                .clocks
                .get_slot(slot.raw())
                .expect("the clock index only references live rows")
                .clone();
            let clock = self
                .clocks
                .get_slot_mut(slot.raw())
                .expect("the clock index only references live rows");
            for delay_type in [DelayType::Max, DelayType::Min] {
                if !corners.matches(delay_type) {
                    continue;
                }
                for edge in TimingEdge::ALL {
                    if !edges.matches(edge) {
                        continue;
                    }
                    if source {
                        for index in 0..2 {
                            if side.covers(index) {
                                clock.source_latencies[delay_type.index()][index][edge.index()] =
                                    Some(delay);
                            }
                        }
                    } else {
                        clock.network_latencies[delay_type.index()][edge.index()] = Some(delay);
                    }
                }
            }
            self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Removes source or network latency from selected clocks.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty selection, an unknown clock ID, or
    /// revision exhaustion when at least one latency changes.
    ///
    /// # Panics
    ///
    /// Panics if the private clock-ID index points at a non-live arena row.
    pub fn unset_clock_latency(
        &mut self,
        source: bool,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if clocks.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "unset_clock_latency",
            }
            .into());
        }
        let slots = self.resolve_clock_slots(clocks)?;
        let changed = slots
            .into_iter()
            .filter(|slot| {
                let clock = self
                    .clocks
                    .get_slot(slot.raw())
                    .expect("the clock index only references live rows");
                if source {
                    clock
                        .source_latencies
                        .iter()
                        .flatten()
                        .flatten()
                        .any(Option::is_some)
                } else {
                    clock
                        .network_latencies
                        .iter()
                        .flatten()
                        .any(Option::is_some)
                }
            })
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for slot in changed {
            let previous = self
                .clocks
                .get_slot(slot.raw())
                .expect("the clock index only references live rows")
                .clone();
            let clock = self
                .clocks
                .get_slot_mut(slot.raw())
                .expect("the clock index only references live rows");
            if source {
                clock.source_latencies = [[[None; 2]; 2]; 2];
            } else {
                clock.network_latencies = [[None; 2]; 2];
            }
            self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Marks clocks as propagated or ideal.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty selection, an unknown clock ID, or
    /// revision exhaustion.
    ///
    /// # Panics
    ///
    /// Panics if the private clock-ID index points at a non-live arena row.
    pub fn set_propagated_clock(
        &mut self,
        propagated: bool,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if clocks.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: if propagated {
                    "set_propagated_clock"
                } else {
                    "unset_propagated_clock"
                },
            }
            .into());
        }
        let slots = clocks
            .iter()
            .map(|id| {
                self.clock_slots
                    .get(id)
                    .copied()
                    .ok_or(crate::ConstraintError::ClockNotFound { id: *id })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let next_revision = self.next_revision()?;
        for slot in slots {
            let previous = self
                .clocks
                .get_slot(slot.raw())
                .expect("the clock index only references live rows")
                .clone();
            self.clocks
                .get_slot_mut(slot.raw())
                .expect("the clock index only references live rows")
                .propagated = propagated;
            self.record_undo(TimingUndo::ClockReplaced { slot, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Sets intra- or inter-clock uncertainty for selected edges and corners.
    ///
    /// # Errors
    ///
    /// Returns an error for negative or non-finite uncertainty, an empty source
    /// or destination selection, an unknown clock ID, or revision exhaustion.
    pub fn set_clock_uncertainty(
        &mut self,
        uncertainty: f64,
        from: &[ClockId],
        from_edge: EdgeSelection,
        to: &[ClockId],
        to_edge: EdgeSelection,
        corner: ExceptionCorner,
    ) -> Result<ConstraintChange, crate::TimingError> {
        if !uncertainty.is_finite() || uncertainty < 0.0 {
            return Err(crate::ConstraintError::InvalidValue {
                command: "set_clock_uncertainty",
                value: uncertainty,
            }
            .into());
        }
        if from.is_empty() || to.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "set_clock_uncertainty",
            }
            .into());
        }
        for id in from.iter().chain(to) {
            if !self.clock_slots.contains_key(id) {
                return Err(crate::ConstraintError::ClockNotFound { id: *id }.into());
            }
        }
        let next_revision = self.next_revision()?;
        let from_edges = concrete_edge_selections(from_edge);
        let to_edges = concrete_edge_selections(to_edge);
        for from in from.iter().copied().collect::<BTreeSet<_>>() {
            for to in to.iter().copied().collect::<BTreeSet<_>>() {
                for from_edge in from_edges.iter().copied() {
                    for to_edge in to_edges.iter().copied() {
                        for delay_type in [DelayType::Max, DelayType::Min] {
                            if !corner.matches(delay_type) {
                                continue;
                            }
                            let key = ClockUncertaintyKey {
                                from,
                                from_edge,
                                to,
                                to_edge,
                                delay_type,
                            };
                            let previous = self.clock_uncertainties.insert(key, uncertainty);
                            if previous.is_none() {
                                add_index_references(
                                    &mut self.references,
                                    clock_uncertainty_references(key),
                                );
                            }
                            self.record_undo(TimingUndo::ClockUncertainty { key, previous });
                        }
                    }
                }
            }
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Removes uncertainty rows matching the selected clock relation.
    ///
    /// # Errors
    ///
    /// Returns an error when either clock selection is empty or when revision
    /// allocation fails for a non-empty removal.
    pub fn unset_clock_uncertainty(
        &mut self,
        from: &[ClockId],
        from_edge: EdgeSelection,
        to: &[ClockId],
        to_edge: EdgeSelection,
        corner: ExceptionCorner,
    ) -> Result<ConstraintChange, crate::TimingError> {
        if from.is_empty() || to.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "unset_clock_uncertainty",
            }
            .into());
        }
        let from = from.iter().copied().collect::<BTreeSet<_>>();
        let to = to.iter().copied().collect::<BTreeSet<_>>();
        let keys = self
            .clock_uncertainties
            .keys()
            .copied()
            .filter(|key| {
                from.contains(&key.from)
                    && to.contains(&key.to)
                    && edge_selections_overlap(from_edge, key.from_edge)
                    && edge_selections_overlap(to_edge, key.to_edge)
                    && corner.matches(key.delay_type)
            })
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for key in keys {
            let previous = self.clock_uncertainties.remove(&key);
            self.remove_references(clock_uncertainty_references(key));
            self.record_undo(TimingUndo::ClockUncertainty { key, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Makes paths between different clock groups false.
    ///
    /// # Errors
    ///
    /// Returns an error unless at least two non-empty groups are supplied, or
    /// when any generated false-path constraint fails validation or insertion.
    pub fn set_clock_groups(
        &mut self,
        kind: ClockGroupKind,
        name: &str,
        groups: &[Vec<ClockId>],
        comment: &str,
    ) -> Result<ConstraintChange, crate::TimingError> {
        if groups.len() < 2 || groups.iter().any(Vec::is_empty) {
            return Err(crate::ConstraintError::InvalidClockGroups.into());
        }
        let marker = format!("\0opto-clock-group:{}:{}\0{}", kind.marker(), name, comment);
        for (left_index, left) in groups.iter().enumerate() {
            for right in &groups[left_index + 1..] {
                for (from, to) in [(left, right), (right, left)] {
                    self.set_path_exception(PathException {
                        kind: PathExceptionKind::FalsePath,
                        from: ExceptionFilter::new(from.iter().copied().map(TimingEndpoint::Clock)),
                        through: Vec::new().into_boxed_slice(),
                        to: ExceptionFilter::new(to.iter().copied().map(TimingEndpoint::Clock)),
                        edges: EdgeQualifier::default(),
                        corner: ExceptionCorner::Both,
                        ignore_clock_latency: false,
                        comment: marker.clone(),
                    })?;
                }
            }
        }
        Ok(ConstraintChange::Changed)
    }

    /// Removes clock-group path cuts selected by kind and name.
    ///
    /// # Errors
    ///
    /// Returns an error if revision allocation fails for a non-empty removal.
    ///
    /// # Panics
    ///
    /// Panics if a slot selected from the live path-exception iterator becomes
    /// non-live during the same exclusive mutation.
    pub fn unset_clock_groups(
        &mut self,
        kind: ClockGroupKind,
        names: Option<&BTreeSet<String>>,
    ) -> Result<ConstraintChange, crate::TimingError> {
        let prefix = format!("\0opto-clock-group:{}:", kind.marker());
        let slots = self
            .path_exception_entries()
            .filter_map(|(slot, exception)| {
                let rest = exception.comment.strip_prefix(&prefix)?;
                let (name, _) = rest.split_once('\0')?;
                names
                    .is_none_or(|names| names.contains(name))
                    .then_some(slot)
            })
            .collect::<Vec<_>>();
        if slots.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for slot in slots {
            let references = self
                .path_exceptions
                .get_slot(slot.raw())
                .map(|exception| path_exception_references(slot, exception).collect::<Vec<_>>())
                .expect("clock-group slots originate from live rows");
            self.remove_references(references);
            let removal = self.path_exceptions.remove_tracked(slot.raw());
            self.record_undo(TimingUndo::PathExceptionRemoved(removal));
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }
}
