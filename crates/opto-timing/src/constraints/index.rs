// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Reverse-index maintenance for transactional timing constraints.
//!
//! Every constraint mutation updates its primary owner and object-reference
//! index together. Preparation may remove or rewrite rows, but rollback must
//! restore both representations so object deletion never depends on which
//! query or command ran first.

use super::*;

impl PreparedTimingObjectRemoval {
    #[cfg(test)]
    pub(crate) fn inspected_rows(&self) -> usize {
        self.inspected_rows
    }
}

pub(super) type ReferencePair = (opto_db::AnyObjectId, TimingReference);

pub(super) fn insert_reference(
    references: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    object: opto_db::AnyObjectId,
    reference: TimingReference,
) {
    references.entry(object).or_default().insert(reference);
}

pub(super) fn remove_index_reference(
    references: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    object: opto_db::AnyObjectId,
    reference: TimingReference,
) {
    let Some(stored) = references.get_mut(&object) else {
        return;
    };
    stored.remove(&reference);
    if stored.is_empty() {
        references.remove(&object);
    }
}

/// Restores a port-keyed value together with its reverse reference.
pub(super) fn restore_map_value<T>(
    values: &mut BTreeMap<PortId, T>,
    references: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    port: PortId,
    previous: Option<T>,
    reference: TimingReference,
) {
    match previous {
        Some(previous) => {
            if values.insert(port, previous).is_none() {
                insert_reference(references, port.erase(), reference);
            }
        }
        None => {
            if values.remove(&port).is_some() {
                remove_index_reference(references, port.erase(), reference);
            }
        }
    }
}

pub(super) fn design_rule_reference(kind: DesignRuleKind, slot: RawSlot) -> TimingReference {
    match kind {
        DesignRuleKind::MaxTransition => TimingReference::MaxTransition(MaxTransitionSlot(slot)),
        DesignRuleKind::MaxCapacitance => TimingReference::MaxCapacitance(MaxCapacitanceSlot(slot)),
        DesignRuleKind::MaxFanout => TimingReference::MaxFanout(MaxFanoutSlot(slot)),
    }
}

pub(super) fn add_index_references(
    index: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    references: impl IntoIterator<Item = ReferencePair>,
) {
    for (object, reference) in references {
        insert_reference(index, object, reference);
    }
}

pub(super) fn clock_references(
    slot: ClockSlot,
    clock: &Clock,
) -> impl Iterator<Item = ReferencePair> + '_ {
    std::iter::once((clock.id.erase(), TimingReference::Clock(slot)))
        .chain(
            clock
                .sources
                .iter()
                .map(move |source| (source.erase(), TimingReference::ClockSource(slot))),
        )
        .chain(clock.generated.iter().flat_map(move |generated| {
            [
                (
                    generated.master.erase(),
                    TimingReference::GeneratedClockMaster(slot),
                ),
                (
                    generated.source.erase(),
                    TimingReference::GeneratedClockSource(slot),
                ),
            ]
        }))
}

pub(super) fn io_delay_references(
    port: PortId,
    rows: &[IoDelay],
    kind: IoDelayKind,
) -> impl Iterator<Item = ReferencePair> + '_ {
    let reference = match kind {
        IoDelayKind::Input => TimingReference::InputDelay(port),
        IoDelayKind::Output => TimingReference::OutputDelay(port),
    };
    std::iter::once((port.erase(), reference)).chain(
        rows.iter()
            .filter_map(|row| row.clock)
            .map(move |clock| (clock.erase(), reference)),
    )
}

pub(super) fn clock_uncertainty_references(
    key: ClockUncertaintyKey,
) -> impl Iterator<Item = ReferencePair> {
    let reference = TimingReference::ClockUncertainty(key.from, key.to);
    [(key.from.erase(), reference), (key.to.erase(), reference)].into_iter()
}

pub(super) fn endpoint_object(endpoint: TimingEndpoint) -> opto_db::AnyObjectId {
    endpoint.object_id()
}

pub(super) fn path_exception_references(
    slot: PathExceptionSlot,
    constraint: &PathException,
) -> impl Iterator<Item = ReferencePair> + '_ {
    constraint
        .from
        .objects()
        .iter()
        .map(move |endpoint| {
            (
                endpoint_object(*endpoint),
                TimingReference::PathExceptionFrom(slot),
            )
        })
        .chain(constraint.through.iter().flat_map(move |filter| {
            filter.objects().iter().map(move |endpoint| {
                (
                    endpoint_object(*endpoint),
                    TimingReference::PathExceptionThrough(slot),
                )
            })
        }))
        .chain(constraint.to.objects().iter().map(move |endpoint| {
            (
                endpoint_object(*endpoint),
                TimingReference::PathExceptionTo(slot),
            )
        }))
}

pub(super) fn design_rule_references(
    constraint: &DesignRuleConstraint,
    reference: TimingReference,
) -> impl Iterator<Item = ReferencePair> + '_ {
    constraint
        .objects
        .iter()
        .map(move |object| (object.object_id(), reference))
}

pub(super) fn collect_removed_references<R: opto_db::ObjectIdSet + ?Sized>(
    output: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    references: impl IntoIterator<Item = ReferencePair>,
    removed: &R,
    remove_row: bool,
) {
    for (object, reference) in references
        .into_iter()
        .filter(|(object, _)| remove_row || removed.contains(object))
    {
        insert_reference(output, object, reference);
    }
}

pub(super) fn insert_design_rule<I: DesignRuleSlot>(
    arena: &mut OrderedArena<DesignRuleConstraint>,
    references: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
    constraint: DesignRuleConstraint,
) -> Result<ArenaInsertion, crate::TimingError> {
    let insertion = arena.insert_tracked(constraint)?;
    let slot = I::from_raw(insertion.slot());
    let constraint = arena
        .get_slot(slot.raw())
        .expect("a newly inserted design-rule row is live");
    add_index_references(
        references,
        design_rule_references(constraint, slot.reference()),
    );
    Ok(insertion)
}

/// Prepares deterministic row rewrites after removing referenced objects.
///
/// Empty constraints are deleted; surviving object rows preserve their
/// original order. Reverse references are updated against the prepared view
/// and are restored from the returned edits if the outer transaction fails.
pub(super) fn prepare_design_rule_rows<I: DesignRuleSlot, R: opto_db::ObjectIdSet + ?Sized>(
    arena: &OrderedArena<DesignRuleConstraint>,
    slots: BTreeSet<I>,
    removed: &R,
    references: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
) -> Vec<RowEdit<I, DesignRuleConstraint>> {
    slots
        .into_iter()
        .map(|slot| {
            let constraint = arena
                .get_slot(slot.raw())
                .expect("the reverse index only references live design-rule rows");
            let objects = constraint
                .objects
                .iter()
                .filter(|object| !removed.contains(&object.object_id()))
                .cloned()
                .collect::<Box<[_]>>();
            let keep = !objects.is_empty();
            collect_removed_references(
                references,
                design_rule_references(constraint, slot.reference()),
                removed,
                !keep,
            );
            RowEdit {
                slot,
                replacement: keep.then_some(DesignRuleConstraint {
                    limit: constraint.limit,
                    objects,
                    scope: constraint.scope,
                }),
            }
        })
        .collect()
}

pub(super) fn index_design_rule_arena<I: DesignRuleSlot>(
    arena: &OrderedArena<DesignRuleConstraint>,
    references: &mut BTreeMap<opto_db::AnyObjectId, BTreeSet<TimingReference>>,
) {
    for (raw, constraint) in arena.entries() {
        let slot = I::from_raw(raw);
        add_index_references(
            references,
            design_rule_references(constraint, slot.reference()),
        );
    }
}

impl TimingContext {
    pub(super) fn add_reference(
        &mut self,
        object: opto_db::AnyObjectId,
        reference: TimingReference,
    ) {
        insert_reference(&mut self.references, object, reference);
    }

    pub(super) fn remove_reference(
        &mut self,
        object: opto_db::AnyObjectId,
        reference: TimingReference,
    ) {
        remove_index_reference(&mut self.references, object, reference);
    }

    pub(super) fn remove_reference_set(
        &mut self,
        object: opto_db::AnyObjectId,
        removed: &BTreeSet<TimingReference>,
    ) {
        let empty = {
            let references = self
                .references
                .get_mut(&object)
                .expect("a prepared timing edit removes indexed references");
            references.retain(|stored| !removed.contains(stored));
            references.is_empty()
        };
        if empty {
            self.references.remove(&object);
        }
    }

    pub(super) fn remove_references(
        &mut self,
        references: impl IntoIterator<Item = ReferencePair>,
    ) {
        for (object, reference) in references {
            self.remove_reference(object, reference);
        }
    }

    pub(super) fn rebuild_indexes(&mut self) -> Result<(), String> {
        let mut clock_slots = BTreeMap::new();
        let mut references = BTreeMap::new();
        for (raw, clock) in self.clocks.entries() {
            let slot = ClockSlot(raw);
            if clock_slots.insert(clock.id, slot).is_some() {
                return Err(format!(
                    "timing context contains duplicate clock ID {:?}",
                    clock.id
                ));
            }
            for (object, reference) in clock_references(slot, clock) {
                insert_reference(&mut references, object, reference);
            }
        }
        for port in self.input_transitions.keys() {
            insert_reference(
                &mut references,
                port.erase(),
                TimingReference::InputTransition,
            );
        }
        for port in self.loads.keys() {
            insert_reference(&mut references, port.erase(), TimingReference::Load);
        }
        for endpoint in self.resistances.keys() {
            insert_reference(
                &mut references,
                endpoint.object_id(),
                TimingReference::Resistance,
            );
        }
        for (&port, rows) in &self.input_delays {
            add_index_references(
                &mut references,
                io_delay_references(port, rows, IoDelayKind::Input),
            );
        }
        for (&port, rows) in &self.output_delays {
            add_index_references(
                &mut references,
                io_delay_references(port, rows, IoDelayKind::Output),
            );
        }
        for &key in self.clock_uncertainties.keys() {
            add_index_references(&mut references, clock_uncertainty_references(key));
        }
        for endpoint in self.case_analysis.keys() {
            insert_reference(
                &mut references,
                endpoint.object_id(),
                TimingReference::CaseAnalysis,
            );
        }
        for disabled in &self.disabled_timing {
            insert_reference(
                &mut references,
                disabled.target.object_id(),
                TimingReference::DisabledTiming(disabled.target),
            );
        }
        for (raw, constraint) in self.path_exceptions.entries() {
            let slot = PathExceptionSlot(raw);
            for (object, reference) in path_exception_references(slot, constraint) {
                insert_reference(&mut references, object, reference);
            }
        }
        index_design_rule_arena::<MaxTransitionSlot>(&self.max_transitions, &mut references);
        index_design_rule_arena::<MaxCapacitanceSlot>(&self.max_capacitances, &mut references);
        index_design_rule_arena::<MaxFanoutSlot>(&self.max_fanouts, &mut references);
        self.clock_slots = clock_slots;
        self.references = references;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn path_exception_slot_capacity(&self) -> usize {
        self.path_exceptions.slot_capacity()
    }

    #[cfg(test)]
    pub(crate) fn transaction_metrics(&self) -> (usize, usize) {
        (self.transactions.len(), self.journal.len())
    }
}

pub(super) fn endpoint_is_removed<R: opto_db::ObjectIdSet + ?Sized>(
    endpoint: TimingEndpoint,
    removed: &R,
) -> bool {
    removed.contains(&endpoint.object_id())
}

impl TimingContext {
    pub(super) fn next_revision(&self) -> Result<RevisionId, crate::TimingError> {
        self.revision.next().map_err(crate::TimingError::Revision)
    }
}

pub(crate) fn bus_base_name(name: &str) -> Option<&str> {
    let bracket = name.rfind('[')?;
    name.ends_with(']').then_some(&name[..bracket])
}

pub(super) fn validate_timing_constraint<T>(
    command: &'static str,
    value: f64,
    objects: &[T],
) -> Result<(), crate::TimingError> {
    if !value.is_finite() || value < 0.0 {
        return Err(crate::ConstraintError::InvalidValue { command, value }.into());
    }
    if objects.is_empty() {
        return Err(crate::ConstraintError::NoObjects { command }.into());
    }
    Ok(())
}

pub(super) fn validate_design_rule_objects(
    command: &'static str,
    value: f64,
    objects: &[TimingObject],
) -> Result<(), crate::TimingError> {
    if !value.is_finite() || value < 0.0 {
        return Err(crate::ConstraintError::InvalidValue { command, value }.into());
    }
    if objects.is_empty() {
        return Err(crate::ConstraintError::NoObjects { command }.into());
    }
    Ok(())
}

pub(super) fn validate_path_exception(
    constraint: &PathException,
) -> Result<(), crate::TimingError> {
    let command = match constraint.kind {
        PathExceptionKind::FalsePath => "set_false_path",
        PathExceptionKind::MultiCycle { cycles, .. } => {
            if cycles == 0 {
                return Err(crate::ConstraintError::InvalidMulticycle { cycles }.into());
            }
            "set_multicycle_path"
        }
        PathExceptionKind::MaxDelay { delay } => {
            if !delay.is_finite() || delay < 0.0 {
                return Err(crate::ConstraintError::InvalidPathDelay {
                    command: "set_max_delay",
                    delay,
                }
                .into());
            }
            "set_max_delay"
        }
        PathExceptionKind::MinDelay { delay } => {
            if !delay.is_finite() || delay < 0.0 {
                return Err(crate::ConstraintError::InvalidPathDelay {
                    command: "set_min_delay",
                    delay,
                }
                .into());
            }
            "set_min_delay"
        }
    };
    if constraint.edges.through.len() != constraint.through.len() {
        return Err(crate::ConstraintError::ThroughEdgeCountMismatch {
            command,
            filters: constraint.through.len(),
            edges: constraint.edges.through.len(),
        }
        .into());
    }
    if constraint
        .through
        .iter()
        .any(ExceptionFilter::is_unrestricted)
    {
        return Err(crate::ConstraintError::EmptyThroughFilter { command }.into());
    }
    if constraint.through.len() > usize::from(u16::MAX) {
        return Err(crate::ConstraintError::TooManyThroughFilters {
            command,
            count: constraint.through.len(),
        }
        .into());
    }
    if matches!(
        constraint.kind,
        PathExceptionKind::FalsePath | PathExceptionKind::MultiCycle { .. }
    ) && constraint.is_unrestricted()
    {
        return Err(crate::ConstraintError::UnrestrictedPathException { command }.into());
    }
    Ok(())
}
