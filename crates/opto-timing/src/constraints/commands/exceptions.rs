// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Path exceptions and I/O delay constraints.

use super::*;

impl TimingContext {
    /// Sets selected input/output delay slots on one or more ports.
    ///
    /// Without `add_delay`, all existing delay rows on each port are replaced.
    /// With `add_delay`, the selected slots are merged into the row for the
    /// same reference clock and edge.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-finite delay, an empty port set, an unknown
    /// reference clock, or revision exhaustion.
    ///
    /// # Panics
    ///
    /// Panics only if appending a new delay row fails to make that row
    /// immediately addressable, which would indicate a standard-library vector
    /// invariant violation.
    pub fn set_io_delay(
        &mut self,
        spec: IoDelaySpec,
        ports: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        let IoDelaySpec {
            kind,
            delay,
            clock,
            clock_edge,
            edges,
            corners,
            source_latency_included,
            network_latency_included,
            add_delay,
        } = spec;
        let command = match kind {
            IoDelayKind::Input => "set_input_delay",
            IoDelayKind::Output => "set_output_delay",
        };
        if !delay.is_finite() {
            return Err(crate::ConstraintError::InvalidValue {
                command,
                value: delay,
            }
            .into());
        }
        if ports.is_empty() {
            return Err(crate::ConstraintError::NoObjects { command }.into());
        }
        if let Some(clock) = clock
            && !self.clock_slots.contains_key(&clock)
        {
            return Err(crate::ConstraintError::ClockNotFound { id: clock }.into());
        }
        let next_revision = self.next_revision()?;
        for port in ports.iter().copied().collect::<BTreeSet<_>>() {
            let previous = self.io_delays(kind).get(&port).cloned();
            if let Some(current) = &previous {
                self.remove_references(io_delay_references(port, current, kind));
            }
            let mut rows = if add_delay {
                previous.clone().unwrap_or_default()
            } else {
                Vec::new()
            };
            let row_index = rows
                .iter()
                .position(|row| row.clock == clock && row.clock_edge == clock_edge);
            let row = if let Some(index) = row_index {
                &mut rows[index]
            } else {
                rows.push(IoDelay::new(
                    clock,
                    clock_edge,
                    source_latency_included,
                    network_latency_included,
                ));
                rows.last_mut().expect("a row was just appended")
            };
            row.source_latency_included = source_latency_included;
            row.network_latency_included = network_latency_included;
            for delay_type in [DelayType::Max, DelayType::Min] {
                if !corners.matches(delay_type) {
                    continue;
                }
                for edge in TimingEdge::ALL {
                    if edges.matches(edge) {
                        row.delays[delay_type.index()][edge.index()] = Some(delay);
                    }
                }
            }
            add_index_references(&mut self.references, io_delay_references(port, &rows, kind));
            self.io_delays_mut(kind).insert(port, rows);
            let undo = match kind {
                IoDelayKind::Input => TimingUndo::InputDelays { port, previous },
                IoDelayKind::Output => TimingUndo::OutputDelays { port, previous },
            };
            self.record_undo(undo);
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Borrows the input-delay rows for `port`.
    pub fn input_delays(&self, port: PortId) -> &[IoDelay] {
        self.input_delays.get(&port).map_or(&[], Vec::as_slice)
    }

    /// Borrows the output-delay rows for `port`.
    pub fn output_delays(&self, port: PortId) -> &[IoDelay] {
        self.output_delays.get(&port).map_or(&[], Vec::as_slice)
    }

    /// Removes selected input/output delay slots.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty port set, an unknown reference clock, or
    /// revision exhaustion when at least one slot changes.
    pub fn unset_io_delay(
        &mut self,
        kind: IoDelayKind,
        clock: Option<ClockId>,
        clock_edge: TimingEdge,
        edges: EdgeSelection,
        corners: CornerSelection,
        ports: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        let command = match kind {
            IoDelayKind::Input => "unset_input_delay",
            IoDelayKind::Output => "unset_output_delay",
        };
        if ports.is_empty() {
            return Err(crate::ConstraintError::NoObjects { command }.into());
        }
        if let Some(clock) = clock
            && !self.clock_slots.contains_key(&clock)
        {
            return Err(crate::ConstraintError::ClockNotFound { id: clock }.into());
        }
        let mut edits = Vec::new();
        for port in ports.iter().copied().collect::<BTreeSet<_>>() {
            let Some(previous) = self.io_delays(kind).get(&port).cloned() else {
                continue;
            };
            let mut replacement = previous.clone();
            for row in &mut replacement {
                if clock
                    .is_some_and(|clock| row.clock != Some(clock) || row.clock_edge != clock_edge)
                {
                    continue;
                }
                for delay_type in [DelayType::Max, DelayType::Min] {
                    if !corners.matches(delay_type) {
                        continue;
                    }
                    for edge in TimingEdge::ALL {
                        if edges.matches(edge) {
                            row.delays[delay_type.index()][edge.index()] = None;
                        }
                    }
                }
            }
            replacement.retain(|row| row.delays.iter().flatten().any(Option::is_some));
            if replacement != previous {
                edits.push((port, previous, replacement));
            }
        }
        if edits.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for (port, previous, replacement) in edits {
            self.remove_references(io_delay_references(port, &previous, kind));
            if replacement.is_empty() {
                self.io_delays_mut(kind).remove(&port);
            } else {
                add_index_references(
                    &mut self.references,
                    io_delay_references(port, &replacement, kind),
                );
                self.io_delays_mut(kind).insert(port, replacement);
            }
            let undo = match kind {
                IoDelayKind::Input => TimingUndo::InputDelays {
                    port,
                    previous: Some(previous),
                },
                IoDelayKind::Output => TimingUndo::OutputDelays {
                    port,
                    previous: Some(previous),
                },
            };
            self.record_undo(undo);
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    pub(super) fn io_delays(&self, kind: IoDelayKind) -> &BTreeMap<PortId, Vec<IoDelay>> {
        match kind {
            IoDelayKind::Input => &self.input_delays,
            IoDelayKind::Output => &self.output_delays,
        }
    }

    pub(super) fn io_delays_mut(
        &mut self,
        kind: IoDelayKind,
    ) -> &mut BTreeMap<PortId, Vec<IoDelay>> {
        match kind {
            IoDelayKind::Input => &mut self.input_delays,
            IoDelayKind::Output => &mut self.output_delays,
        }
    }

    pub(super) fn restore_io_delays(
        &mut self,
        port: PortId,
        previous: Option<Vec<IoDelay>>,
        kind: IoDelayKind,
    ) {
        if let Some(current) = self.io_delays_mut(kind).remove(&port) {
            self.remove_references(io_delay_references(port, &current, kind));
        }
        if let Some(previous) = previous {
            add_index_references(
                &mut self.references,
                io_delay_references(port, &previous, kind),
            );
            self.io_delays_mut(kind).insert(port, previous);
        }
    }

    /// Adds a validated path exception.
    ///
    /// # Errors
    ///
    /// Returns an error when the exception has invalid delay, point, edge, or
    /// ordered-through semantics, or when revision or arena capacity is
    /// exhausted.
    pub fn set_path_exception(
        &mut self,
        constraint: PathException,
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.set_path_exception_with_reset(constraint, false)
    }

    /// Adds a path exception and optionally clears exceptions on the same
    /// qualified path points before publishing the new row.
    ///
    /// # Errors
    ///
    /// Returns an error when the exception is invalid, the revision counter is
    /// exhausted, or the compact exception arena cannot grow.
    ///
    /// # Panics
    ///
    /// Panics if a row just inserted, or a reset candidate selected from the
    /// live arena, cannot be resolved during the same exclusive mutation.
    pub fn set_path_exception_with_reset(
        &mut self,
        constraint: PathException,
        reset_path: bool,
    ) -> Result<ConstraintChange, crate::TimingError> {
        validate_path_exception(&constraint)?;
        let next_revision = self.next_revision()?;
        let reset_slots = if reset_path {
            self.path_exception_entries()
                .filter(|(_, existing)| reset_path_matches(existing, &constraint))
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let insertion = self.path_exceptions.insert_tracked(constraint)?;
        let slot = PathExceptionSlot(insertion.slot());
        let constraint = self
            .path_exceptions
            .get_slot(slot.raw())
            .expect("a newly inserted path-exception row is live");
        add_index_references(
            &mut self.references,
            path_exception_references(slot, constraint),
        );
        self.record_undo(TimingUndo::PathExceptionInserted(insertion));
        for reset_slot in reset_slots {
            let references = {
                let existing = self
                    .path_exceptions
                    .get_slot(reset_slot.raw())
                    .expect("reset candidates originate from live path-exception rows");
                path_exception_references(reset_slot, existing).collect::<Vec<_>>()
            };
            self.remove_references(references);
            let removal = self.path_exceptions.remove_tracked(reset_slot.raw());
            self.record_undo(TimingUndo::PathExceptionRemoved(removal));
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Removes path exceptions matching the selected points, edges, and corner.
    ///
    /// # Errors
    ///
    /// Returns an error when no point restriction is supplied or when revision
    /// allocation fails for a non-empty removal.
    ///
    /// # Panics
    ///
    /// Panics if a slot selected from the live exception arena becomes non-live
    /// during the same exclusive mutation.
    pub fn unset_path_exceptions(
        &mut self,
        selection: &PathException,
    ) -> Result<ConstraintChange, crate::TimingError> {
        if selection.from.is_unrestricted()
            && selection.through.is_empty()
            && selection.to.is_unrestricted()
        {
            return Err(crate::ConstraintError::UnrestrictedPathExceptionRemoval.into());
        }
        let slots = self
            .path_exception_entries()
            .filter(|(_, existing)| reset_path_matches(existing, selection))
            .map(|(slot, _)| slot)
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
                .expect("removal slots originate from live exception rows");
            self.remove_references(references);
            let removal = self.path_exceptions.remove_tracked(slot.raw());
            self.record_undo(TimingUndo::PathExceptionRemoved(removal));
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Adds a maximum-delay exception between endpoint sets.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-finite delay, unrestricted or invalid point
    /// filters, revision exhaustion, or compact-arena capacity exhaustion.
    pub fn set_max_delay(
        &mut self,
        delay: f64,
        from: Vec<TimingEndpoint>,
        to: Vec<TimingEndpoint>,
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.set_path_exception(PathException {
            kind: PathExceptionKind::MaxDelay { delay },
            from: ExceptionFilter::new(from),
            through: Vec::new().into_boxed_slice(),
            to: ExceptionFilter::new(to),
            edges: EdgeQualifier::default(),
            corner: ExceptionCorner::Setup,
            ignore_clock_latency: false,
            comment: String::new(),
        })
    }

    /// Borrows path exceptions in insertion order.
    #[must_use]
    pub fn path_exceptions(&self) -> TimingRows<'_, PathException> {
        TimingRows::new(&self.path_exceptions)
    }

    #[must_use]
    /// Returns the tightest maximum-delay constraint targeting `endpoint`.
    pub fn minimum_max_delay_to(&self, endpoint: TimingEndpoint) -> Option<f64> {
        self.path_exceptions
            .iter()
            .filter(|constraint| constraint.to.matches_any(&[endpoint]))
            .filter_map(|constraint| match constraint.kind {
                PathExceptionKind::MaxDelay { delay } => Some(delay),
                _ => None,
            })
            .min_by(f64::total_cmp)
    }
}
