// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl TimingContext {
    /// Removes or narrows every constraint referencing `removed`.
    ///
    /// The edit is prepared completely before mutation, then applied under an
    /// owner/revision check.
    ///
    /// # Errors
    ///
    /// Returns an error if replacement rows cannot be allocated, revision
    /// allocation fails, or the prepared edit becomes foreign or stale before
    /// commit.
    pub fn remove_objects(
        &mut self,
        removed: &impl opto_db::ObjectIdSet,
    ) -> Result<(), crate::TimingError> {
        let prepared = self.prepare_object_removal(removed)?;
        self.apply_object_removal(prepared)
    }

    /// Builds the complete touched-row edit while timing state is immutable.
    ///
    /// Row discovery and replacement allocation finish here. Commit follows
    /// these precomputed edits; an active owner-local checkpoint may append a
    /// proportional number of inverse journal entries.
    ///
    /// # Errors
    ///
    /// Returns an error if replacement rows or rollback metadata cannot be
    /// allocated, or if the next revision cannot be reserved.
    ///
    /// # Panics
    ///
    /// Panics if the private reverse-reference index associates a reference
    /// kind with an incompatible persistent object class.
    #[allow(
        clippy::too_many_lines,
        reason = "removal preparation computes the transitive reference closure and a complete \
                  inverse journal without mutating the live context"
    )]
    pub fn prepare_object_removal(
        &self,
        removed: &impl opto_db::ObjectIdSet,
    ) -> Result<PreparedTimingObjectRemoval, crate::TimingError> {
        let mut clock_slots = BTreeSet::new();
        let mut input_transitions = BTreeSet::new();
        let mut loads = BTreeSet::new();
        let mut resistances = BTreeSet::new();
        let mut input_delays = BTreeSet::new();
        let mut output_delays = BTreeSet::new();
        let mut clock_uncertainties = BTreeSet::new();
        let mut case_analysis = BTreeSet::new();
        let mut disabled_timing_targets = BTreeSet::new();
        let mut path_exception_slots = BTreeSet::new();
        let mut max_transition_slots = BTreeSet::new();
        let mut max_capacitance_slots = BTreeSet::new();
        let mut max_fanout_slots = BTreeSet::new();

        let mut collect_references =
            |object: opto_db::AnyObjectId, references: &BTreeSet<TimingReference>| {
                let object = &object;
                for reference in references {
                    match *reference {
                        TimingReference::Clock(slot)
                        | TimingReference::ClockSource(slot)
                        | TimingReference::GeneratedClockMaster(slot)
                        | TimingReference::GeneratedClockSource(slot) => {
                            clock_slots.insert(slot);
                        }
                        TimingReference::InputTransition => {
                            let opto_db::AnyObjectId::Port(port) = object else {
                                unreachable!("input-transition references are owned by ports")
                            };
                            input_transitions.insert(*port);
                        }
                        TimingReference::Load => {
                            let opto_db::AnyObjectId::Port(port) = object else {
                                unreachable!("load references are owned by ports")
                            };
                            loads.insert(*port);
                        }
                        TimingReference::Resistance => {
                            let endpoint = match object {
                                opto_db::AnyObjectId::Port(id) => TimingEndpoint::Port(*id),
                                opto_db::AnyObjectId::Net(id) => TimingEndpoint::Net(*id),
                                _ => {
                                    unreachable!("resistance references are owned by ports or nets")
                                }
                            };
                            resistances.insert(endpoint);
                        }
                        TimingReference::InputDelay(port) => {
                            input_delays.insert(port);
                        }
                        TimingReference::OutputDelay(port) => {
                            output_delays.insert(port);
                        }
                        TimingReference::ClockUncertainty(from, to) => {
                            clock_uncertainties.insert((from, to));
                        }
                        TimingReference::CaseAnalysis => {
                            let endpoint = match object {
                                opto_db::AnyObjectId::Port(id) => TimingEndpoint::Port(*id),
                                opto_db::AnyObjectId::Pin(id) => TimingEndpoint::Pin(*id),
                                _ => {
                                    unreachable!(
                                        "case-analysis references are owned by ports or pins"
                                    )
                                }
                            };
                            case_analysis.insert(endpoint);
                        }
                        TimingReference::DisabledTiming(target) => {
                            disabled_timing_targets.insert(target);
                        }
                        TimingReference::PathExceptionFrom(slot)
                        | TimingReference::PathExceptionThrough(slot)
                        | TimingReference::PathExceptionTo(slot) => {
                            path_exception_slots.insert(slot);
                        }
                        TimingReference::MaxTransition(slot) => {
                            max_transition_slots.insert(slot);
                        }
                        TimingReference::MaxCapacitance(slot) => {
                            max_capacitance_slots.insert(slot);
                        }
                        TimingReference::MaxFanout(slot) => {
                            max_fanout_slots.insert(slot);
                        }
                    }
                }
            };

        if removed.len() <= self.references.len() {
            for object in removed.iter() {
                if let Some(references) = self.references.get(&object) {
                    collect_references(object, references);
                }
            }
        } else {
            for (&object, references) in &self.references {
                if removed.contains(&object) {
                    collect_references(object, references);
                }
            }
        }

        let mut references = BTreeMap::new();
        let clocks = clock_slots
            .into_iter()
            .map(|slot| {
                let clock = self
                    .clocks
                    .get_slot(slot.raw())
                    .expect("the reverse index only references live clock rows");
                let remove_row = removed.contains(&clock.id.erase())
                    || clock.generated.as_ref().is_some_and(|generated| {
                        removed.contains(&generated.master.erase())
                            || removed.contains(&generated.source.erase())
                    });
                collect_removed_references(
                    &mut references,
                    clock_references(slot, clock),
                    removed,
                    remove_row,
                );
                let replacement = (!remove_row).then(|| Clock {
                    id: clock.id,
                    name: clock.name.clone(),
                    period: clock.period,
                    sources: clock
                        .sources
                        .iter()
                        .filter(|source| !removed.contains(&source.erase()))
                        .copied()
                        .collect(),
                    waveform: clock.waveform,
                    comment: clock.comment.clone(),
                    transitions: clock.transitions,
                    source_latencies: clock.source_latencies,
                    network_latencies: clock.network_latencies,
                    propagated: clock.propagated,
                    generated: clock.generated.clone(),
                });
                RowEdit { slot, replacement }
            })
            .collect::<Vec<_>>();

        for port in &input_transitions {
            insert_reference(
                &mut references,
                port.erase(),
                TimingReference::InputTransition,
            );
        }
        for port in &loads {
            insert_reference(&mut references, port.erase(), TimingReference::Load);
        }
        for endpoint in &resistances {
            insert_reference(
                &mut references,
                endpoint.object_id(),
                TimingReference::Resistance,
            );
        }
        let prepare_io_delays = |ports: BTreeSet<PortId>,
                                 rows: &BTreeMap<PortId, Vec<IoDelay>>,
                                 kind: IoDelayKind,
                                 references: &mut BTreeMap<
            opto_db::AnyObjectId,
            BTreeSet<TimingReference>,
        >| {
            ports
                .into_iter()
                .map(|port| {
                    let current = rows
                        .get(&port)
                        .expect("the reverse index only references live port-delay rows");
                    let remove_port = removed.contains(&port.erase());
                    let replacement = (!remove_port)
                        .then(|| {
                            current
                                .iter()
                                .filter(|row| {
                                    row.clock
                                        .is_none_or(|clock| !removed.contains(&clock.erase()))
                                })
                                .cloned()
                                .collect::<Vec<_>>()
                        })
                        .filter(|rows| !rows.is_empty());
                    collect_removed_references(
                        references,
                        io_delay_references(port, current, kind),
                        removed,
                        replacement.is_none(),
                    );
                    MapEdit {
                        key: port,
                        replacement,
                    }
                })
                .collect::<Vec<_>>()
        };
        let input_delays = prepare_io_delays(
            input_delays,
            &self.input_delays,
            IoDelayKind::Input,
            &mut references,
        );
        let output_delays = prepare_io_delays(
            output_delays,
            &self.output_delays,
            IoDelayKind::Output,
            &mut references,
        );
        let clock_uncertainties = self
            .clock_uncertainties
            .iter()
            .filter(|(key, _)| clock_uncertainties.contains(&(key.from, key.to)))
            .map(|(&key, _)| {
                collect_removed_references(
                    &mut references,
                    clock_uncertainty_references(key),
                    removed,
                    true,
                );
                MapEdit {
                    key,
                    replacement: None,
                }
            })
            .collect::<Vec<_>>();
        for endpoint in &case_analysis {
            insert_reference(
                &mut references,
                endpoint.object_id(),
                TimingReference::CaseAnalysis,
            );
        }
        let disabled_timing = self
            .disabled_timing
            .iter()
            .filter(|disabled| disabled_timing_targets.contains(&disabled.target))
            .cloned()
            .collect::<Vec<_>>();
        for target in disabled_timing_targets {
            insert_reference(
                &mut references,
                target.object_id(),
                TimingReference::DisabledTiming(target),
            );
        }

        let path_exceptions =
            path_exception_slots
                .into_iter()
                .map(|slot| {
                    let constraint = self
                        .path_exceptions
                        .get_slot(slot.raw())
                        .expect("the reverse index only references live path-exception rows");
                    let narrow =
                        |filter: &ExceptionFilter| {
                            if filter.is_unrestricted() {
                                ExceptionFilter::unrestricted()
                            } else {
                                ExceptionFilter::new(
                                    filter.objects().iter().copied().filter(|endpoint| {
                                        !endpoint_is_removed(*endpoint, removed)
                                    }),
                                )
                            }
                        };
                    let from = narrow(&constraint.from);
                    let through = constraint.through.iter().map(narrow).collect::<Box<[_]>>();
                    let to = narrow(&constraint.to);
                    let keep =
                        (constraint.from.is_unrestricted() || !from.is_unrestricted())
                            && constraint.through.iter().zip(through.iter()).all(
                                |(before, after)| {
                                    before.is_unrestricted() || !after.is_unrestricted()
                                },
                            )
                            && (constraint.to.is_unrestricted() || !to.is_unrestricted());
                    collect_removed_references(
                        &mut references,
                        path_exception_references(slot, constraint),
                        removed,
                        !keep,
                    );
                    RowEdit {
                        slot,
                        replacement: keep.then_some(PathException {
                            kind: constraint.kind.clone(),
                            from,
                            through,
                            to,
                            edges: constraint.edges.clone(),
                            corner: constraint.corner,
                            ignore_clock_latency: constraint.ignore_clock_latency,
                            comment: constraint.comment.clone(),
                        }),
                    }
                })
                .collect::<Vec<_>>();

        let max_transitions = prepare_design_rule_rows(
            &self.max_transitions,
            max_transition_slots,
            removed,
            &mut references,
        );
        let max_capacitances = prepare_design_rule_rows(
            &self.max_capacitances,
            max_capacitance_slots,
            removed,
            &mut references,
        );
        let max_fanouts = prepare_design_rule_rows(
            &self.max_fanouts,
            max_fanout_slots,
            removed,
            &mut references,
        );

        let changed = !clocks.is_empty()
            || !input_transitions.is_empty()
            || !loads.is_empty()
            || !resistances.is_empty()
            || !input_delays.is_empty()
            || !output_delays.is_empty()
            || !clock_uncertainties.is_empty()
            || !case_analysis.is_empty()
            || !disabled_timing.is_empty()
            || !path_exceptions.is_empty()
            || !max_transitions.is_empty()
            || !max_capacitances.is_empty()
            || !max_fanouts.is_empty();
        let revision = changed.then(|| self.next_revision()).transpose()?;
        #[cfg(test)]
        let inspected_rows = clocks.len()
            + input_transitions.len()
            + loads.len()
            + resistances.len()
            + input_delays.len()
            + output_delays.len()
            + clock_uncertainties.len()
            + case_analysis.len()
            + disabled_timing.len()
            + path_exceptions.len()
            + max_transitions.len()
            + max_capacitances.len()
            + max_fanouts.len();
        Ok(PreparedTimingObjectRemoval {
            owner: self.owner.clone(),
            base_revision: self.revision,
            revision,
            clocks,
            input_transitions: input_transitions.into_iter().collect(),
            loads: loads.into_iter().collect(),
            resistances: resistances.into_iter().collect(),
            input_delays,
            output_delays,
            clock_uncertainties,
            case_analysis: case_analysis.into_iter().collect(),
            disabled_timing,
            path_exceptions,
            max_transitions,
            max_capacitances,
            max_fanouts,
            references,
            #[cfg(test)]
            inspected_rows,
        })
    }

    /// Applies an immutable-state edit token. The token owns every replacement
    /// allocation, and validation remains bound to commit by an exclusive
    /// mutable borrow.
    ///
    /// # Errors
    ///
    /// Returns [`TimingError::ObjectRemovalOwnerMismatch`] for a token prepared
    /// by another context, or [`TimingError::StaleObjectRemoval`] if the context
    /// revision changed after preparation.
    pub fn apply_object_removal(
        &mut self,
        prepared: PreparedTimingObjectRemoval,
    ) -> Result<(), crate::TimingError> {
        self.validate_object_removal(prepared)?.commit();
        Ok(())
    }

    /// Validates token identity and exclusively borrows timing state until the
    /// returned token is committed or dropped.
    ///
    /// # Errors
    ///
    /// Returns [`TimingError::ObjectRemovalOwnerMismatch`] for a foreign token,
    /// or [`TimingError::StaleObjectRemoval`] when its base revision no longer
    /// matches the live context.
    pub fn validate_object_removal(
        &mut self,
        prepared: PreparedTimingObjectRemoval,
    ) -> Result<ValidatedTimingObjectRemoval<'_>, crate::TimingError> {
        if !self.owner.same_owner(&prepared.owner) {
            return Err(crate::TimingError::ObjectRemovalOwnerMismatch);
        }
        if self.revision != prepared.base_revision {
            return Err(crate::TimingError::StaleObjectRemoval {
                prepared: prepared.base_revision,
                current: self.revision,
            });
        }
        Ok(ValidatedTimingObjectRemoval {
            timing: self,
            prepared,
        })
    }

    pub(in crate::constraints) fn commit_object_removal(
        &mut self,
        prepared: PreparedTimingObjectRemoval,
    ) {
        debug_assert!(
            self.owner.same_owner(&prepared.owner) && self.revision == prepared.base_revision
        );
        for (object, references) in prepared.references {
            self.remove_reference_set(object, &references);
        }
        for port in prepared.input_transitions {
            let previous = self.input_transitions.remove(&port);
            self.record_undo(TimingUndo::InputTransition { port, previous });
        }
        for port in prepared.loads {
            let previous = self.loads.remove(&port);
            self.record_undo(TimingUndo::Load { port, previous });
        }
        for endpoint in prepared.resistances {
            let previous = self.resistances.remove(&endpoint);
            self.record_undo(TimingUndo::Resistance { endpoint, previous });
        }
        for edit in prepared.input_delays {
            self.apply_io_delay_edit(edit, IoDelayKind::Input);
        }
        for edit in prepared.output_delays {
            self.apply_io_delay_edit(edit, IoDelayKind::Output);
        }
        for edit in prepared.clock_uncertainties {
            let previous = self.clock_uncertainties.remove(&edit.key);
            self.record_undo(TimingUndo::ClockUncertainty {
                key: edit.key,
                previous,
            });
        }
        for endpoint in prepared.case_analysis {
            let previous = self.case_analysis.remove(&endpoint);
            self.record_undo(TimingUndo::CaseAnalysis { endpoint, previous });
        }
        for disabled in prepared.disabled_timing {
            if self.disabled_timing.remove(&disabled) {
                self.record_undo(TimingUndo::DisabledTimingRemoved(disabled));
            }
        }
        for edit in prepared.clocks {
            if let Some(replacement) = edit.replacement {
                let previous = self.clocks.replace(edit.slot.raw(), replacement);
                self.record_undo(TimingUndo::ClockReplaced {
                    slot: edit.slot,
                    previous,
                });
            } else {
                let removal = self.clocks.remove_tracked(edit.slot.raw());
                self.clock_slots.remove(&removal.value().id);
                self.record_undo(TimingUndo::ClockRemoved(removal));
            }
        }
        for edit in prepared.path_exceptions {
            if let Some(replacement) = edit.replacement {
                let previous = self.path_exceptions.replace(edit.slot.raw(), replacement);
                self.record_undo(TimingUndo::PathExceptionReplaced {
                    slot: edit.slot,
                    previous,
                });
            } else {
                let removal = self.path_exceptions.remove_tracked(edit.slot.raw());
                self.record_undo(TimingUndo::PathExceptionRemoved(removal));
            }
        }
        self.apply_design_rule_edits(DesignRuleKind::MaxTransition, prepared.max_transitions);
        self.apply_design_rule_edits(DesignRuleKind::MaxCapacitance, prepared.max_capacitances);
        self.apply_design_rule_edits(DesignRuleKind::MaxFanout, prepared.max_fanouts);
        if let Some(revision) = prepared.revision {
            self.revision = revision;
        }
    }

    fn apply_design_rule_edits<I: TimingSlot>(
        &mut self,
        kind: DesignRuleKind,
        edits: Vec<RowEdit<I, DesignRuleConstraint>>,
    ) {
        for edit in edits {
            if let Some(replacement) = edit.replacement {
                let previous = self
                    .design_rule_arena_mut(kind)
                    .replace(edit.slot.raw(), replacement);
                self.record_undo(TimingUndo::DesignRuleReplaced {
                    kind,
                    slot: edit.slot.raw(),
                    previous,
                });
            } else {
                let removal = self
                    .design_rule_arena_mut(kind)
                    .remove_tracked(edit.slot.raw());
                self.record_undo(TimingUndo::DesignRuleRemoved { kind, removal });
            }
        }
    }

    fn apply_io_delay_edit(&mut self, edit: MapEdit<PortId, Vec<IoDelay>>, kind: IoDelayKind) {
        let previous = match edit.replacement {
            Some(replacement) => self.io_delays_mut(kind).insert(edit.key, replacement),
            None => self.io_delays_mut(kind).remove(&edit.key),
        };
        let undo = match kind {
            IoDelayKind::Input => TimingUndo::InputDelays {
                port: edit.key,
                previous,
            },
            IoDelayKind::Output => TimingUndo::OutputDelays {
                port: edit.key,
                previous,
            },
        };
        self.record_undo(undo);
    }
}
