// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Drive, load, transition and design-rule constraints.

use super::super::index::*;
use super::super::*;
use super::*;

impl TimingContext {
    /// Sets a finite nonnegative external capacitive load on ports.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite load, an empty port set,
    /// or revision exhaustion.
    pub fn set_load(
        &mut self,
        load: f64,
        objects: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.set_load_slots(load, EdgeSelection::Both, CornerSelection::Both, objects)
    }

    /// Sets selected rise/fall and min/max external-load slots.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite load, an empty port set,
    /// or revision exhaustion.
    pub fn set_load_slots(
        &mut self,
        load: f64,
        edges: EdgeSelection,
        corners: CornerSelection,
        objects: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        validate_timing_constraint("set_load", load, objects)?;
        let next_revision = self.next_revision()?;
        for object in objects.iter().copied().collect::<BTreeSet<_>>() {
            let previous = self.loads.get(&object).copied();
            let mut slots = previous.unwrap_or_else(PortValueSlots::empty);
            set_port_value_slots(&mut slots, load, edges, corners);
            self.loads.insert(object, slots);
            if previous.is_none() {
                self.add_reference(object.erase(), TimingReference::Load);
            }
            self.record_undo(TimingUndo::Load {
                port: object,
                previous,
            });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Sets external source resistance on top-level input ports.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite resistance, an empty port
    /// set, or revision exhaustion.
    pub fn set_drive(
        &mut self,
        resistance: f64,
        edges: EdgeSelection,
        corners: CornerSelection,
        ports: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        let endpoints = ports
            .iter()
            .copied()
            .map(TimingEndpoint::Port)
            .collect::<Vec<_>>();
        self.set_resistance_slots("set_drive", resistance, edges, corners, &endpoints)
    }

    /// Sets explicit resistance on logical nets.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite resistance, an empty net
    /// set, or revision exhaustion.
    pub fn set_resistance(
        &mut self,
        resistance: f64,
        corners: CornerSelection,
        nets: &[NetId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        let endpoints = nets
            .iter()
            .copied()
            .map(TimingEndpoint::Net)
            .collect::<Vec<_>>();
        self.set_resistance_slots(
            "set_resistance",
            resistance,
            EdgeSelection::Both,
            corners,
            &endpoints,
        )
    }

    pub(super) fn set_resistance_slots(
        &mut self,
        command: &'static str,
        resistance: f64,
        edges: EdgeSelection,
        corners: CornerSelection,
        endpoints: &[TimingEndpoint],
    ) -> Result<ConstraintChange, crate::TimingError> {
        validate_timing_constraint(command, resistance, endpoints)?;
        let next_revision = self.next_revision()?;
        for endpoint in endpoints.iter().copied().collect::<BTreeSet<_>>() {
            let previous = self.resistances.get(&endpoint).copied();
            let mut slots = previous.unwrap_or_else(PortValueSlots::empty);
            set_port_value_slots(&mut slots, resistance, edges, corners);
            self.resistances.insert(endpoint, slots);
            if previous.is_none() {
                self.add_reference(endpoint.object_id(), TimingReference::Resistance);
            }
            self.record_undo(TimingUndo::Resistance { endpoint, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Sets a finite nonnegative transition on a nonempty port set.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite transition, an empty port
    /// set, or revision exhaustion.
    pub fn set_input_transition(
        &mut self,
        transition: f64,
        objects: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.set_input_transition_slots(
            transition,
            EdgeSelection::Both,
            CornerSelection::Both,
            objects,
        )
    }

    /// Sets selected rise/fall and min/max input-transition slots.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite transition, an empty port
    /// set, or revision exhaustion.
    pub fn set_input_transition_slots(
        &mut self,
        transition: f64,
        edges: EdgeSelection,
        corners: CornerSelection,
        objects: &[PortId],
    ) -> Result<ConstraintChange, crate::TimingError> {
        validate_timing_constraint("set_input_transition", transition, objects)?;
        let next_revision = self.next_revision()?;
        for object in objects.iter().copied().collect::<BTreeSet<_>>() {
            let previous = self.input_transitions.get(&object).copied();
            let mut slots = previous.unwrap_or_else(PortValueSlots::empty);
            set_port_value_slots(&mut slots, transition, edges, corners);
            self.input_transitions.insert(object, slots);
            if previous.is_none() {
                self.add_reference(object.erase(), TimingReference::InputTransition);
            }
            self.record_undo(TimingUndo::InputTransition {
                port: object,
                previous,
            });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Adds a maximum-transition rule for the selected scope.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite limit, an empty object
    /// set, a path-specific scope containing non-clock objects, revision
    /// exhaustion, or compact-arena capacity exhaustion.
    pub fn set_max_transition(
        &mut self,
        limit: f64,
        objects: &[TimingObject],
        scope: DesignRuleScope,
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.add_design_rule(DesignRuleKind::MaxTransition, limit, objects, scope)
    }

    /// Adds a maximum-capacitance rule for the selected scope.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite limit, an empty object
    /// set, a path-specific scope containing non-clock objects, revision
    /// exhaustion, or compact-arena capacity exhaustion.
    pub fn set_max_capacitance(
        &mut self,
        limit: f64,
        objects: &[TimingObject],
        scope: DesignRuleScope,
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.add_design_rule(DesignRuleKind::MaxCapacitance, limit, objects, scope)
    }

    /// Adds a maximum-fanout rule over both clock and data paths.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative or non-finite limit, an empty object
    /// set, revision exhaustion, or compact-arena capacity exhaustion.
    pub fn set_max_fanout(
        &mut self,
        limit: f64,
        objects: &[TimingObject],
    ) -> Result<ConstraintChange, crate::TimingError> {
        self.add_design_rule(
            DesignRuleKind::MaxFanout,
            limit,
            objects,
            DesignRuleScope::All,
        )
    }

    pub(super) fn add_design_rule(
        &mut self,
        kind: DesignRuleKind,
        limit: f64,
        objects: &[TimingObject],
        scope: DesignRuleScope,
    ) -> Result<ConstraintChange, crate::TimingError> {
        let command = match kind {
            DesignRuleKind::MaxTransition => "set_max_transition",
            DesignRuleKind::MaxCapacitance => "set_max_capacitance",
            DesignRuleKind::MaxFanout => "set_max_fanout",
        };
        validate_design_rule_objects(command, limit, objects)?;
        if scope != DesignRuleScope::All
            && objects
                .iter()
                .any(|object| object.kind() != TimingObjectKind::Clock)
        {
            return Err(crate::ConstraintError::ClockPathRequiresClockObjects { command }.into());
        }
        let next_revision = self.next_revision()?;
        let constraint = DesignRuleConstraint {
            limit,
            objects: objects.into(),
            scope,
        };
        let insertion = match kind {
            DesignRuleKind::MaxTransition => insert_design_rule::<MaxTransitionSlot>(
                &mut self.max_transitions,
                &mut self.references,
                constraint,
            )?,
            DesignRuleKind::MaxCapacitance => insert_design_rule::<MaxCapacitanceSlot>(
                &mut self.max_capacitances,
                &mut self.references,
                constraint,
            )?,
            DesignRuleKind::MaxFanout => insert_design_rule::<MaxFanoutSlot>(
                &mut self.max_fanouts,
                &mut self.references,
                constraint,
            )?,
        };
        self.record_undo(TimingUndo::DesignRuleInserted { kind, insertion });
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Borrows design-rule constraints of `kind` in insertion order.
    #[must_use]
    pub fn design_rule_constraints(
        &self,
        kind: DesignRuleKind,
    ) -> TimingRows<'_, DesignRuleConstraint> {
        let rows = match kind {
            DesignRuleKind::MaxTransition => &self.max_transitions,
            DesignRuleKind::MaxCapacitance => &self.max_capacitances,
            DesignRuleKind::MaxFanout => &self.max_fanouts,
        };
        TimingRows::new(rows)
    }

    pub(super) fn design_rule_arena_mut(
        &mut self,
        kind: DesignRuleKind,
    ) -> &mut OrderedArena<DesignRuleConstraint> {
        match kind {
            DesignRuleKind::MaxTransition => &mut self.max_transitions,
            DesignRuleKind::MaxCapacitance => &mut self.max_capacitances,
            DesignRuleKind::MaxFanout => &mut self.max_fanouts,
        }
    }

    pub(super) fn add_design_rule_references(&mut self, kind: DesignRuleKind, slot: RawSlot) {
        let reference = design_rule_reference(kind, slot);
        let constraint = match kind {
            DesignRuleKind::MaxTransition => self.max_transitions.get_slot(slot),
            DesignRuleKind::MaxCapacitance => self.max_capacitances.get_slot(slot),
            DesignRuleKind::MaxFanout => self.max_fanouts.get_slot(slot),
        }
        .expect("a restored design-rule row is live");
        add_index_references(
            &mut self.references,
            design_rule_references(constraint, reference),
        );
    }

    /// Sets a logic or transition case value on ports or pins.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty selection, any target other than a port or
    /// pin, or revision exhaustion.
    pub fn set_case_analysis(
        &mut self,
        value: CaseAnalysisValue,
        endpoints: &[TimingEndpoint],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if endpoints.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "set_case_analysis",
            }
            .into());
        }
        if endpoints
            .iter()
            .any(|endpoint| !matches!(endpoint, TimingEndpoint::Port(_) | TimingEndpoint::Pin(_)))
        {
            return Err(crate::ConstraintError::InvalidCaseAnalysisObject.into());
        }
        let next_revision = self.next_revision()?;
        for endpoint in endpoints.iter().copied().collect::<BTreeSet<_>>() {
            let previous = self.case_analysis.insert(endpoint, value);
            if previous.is_none() {
                self.add_reference(endpoint.object_id(), TimingReference::CaseAnalysis);
            }
            self.record_undo(TimingUndo::CaseAnalysis { endpoint, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Removes case analysis from selected ports or pins.
    ///
    /// # Errors
    ///
    /// Returns an error if revision allocation fails for a non-empty removal.
    pub fn unset_case_analysis(
        &mut self,
        endpoints: &[TimingEndpoint],
    ) -> Result<ConstraintChange, crate::TimingError> {
        let existing = endpoints
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|endpoint| self.case_analysis.contains_key(endpoint))
            .collect::<Vec<_>>();
        if existing.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for endpoint in existing {
            let previous = self.case_analysis.remove(&endpoint);
            self.remove_reference(endpoint.object_id(), TimingReference::CaseAnalysis);
            self.record_undo(TimingUndo::CaseAnalysis { endpoint, previous });
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Disables timing through selected ports, pins, cells, or library cells.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty constraint set or revision exhaustion when
    /// at least one new row is inserted.
    pub fn set_disable_timing(
        &mut self,
        constraints: &[DisabledTiming],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if constraints.is_empty() {
            return Err(crate::ConstraintError::NoObjects {
                command: "set_disable_timing",
            }
            .into());
        }
        let additions = constraints
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|disabled| !self.disabled_timing.contains(disabled))
            .collect::<Vec<_>>();
        if additions.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for disabled in additions {
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
            self.record_undo(TimingUndo::DisabledTimingInserted(disabled));
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Removes selected disabled-timing rows.
    ///
    /// # Errors
    ///
    /// Returns an error if revision allocation fails for a non-empty removal.
    pub fn unset_disable_timing(
        &mut self,
        constraints: &[DisabledTiming],
    ) -> Result<ConstraintChange, crate::TimingError> {
        let removals = constraints
            .iter()
            .filter(|disabled| self.disabled_timing.contains(*disabled))
            .cloned()
            .collect::<BTreeSet<_>>();
        if removals.is_empty() {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        for disabled in removals {
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
            self.record_undo(TimingUndo::DisabledTimingRemoved(disabled));
        }
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Sets global timing derates for selected delay, path, edge, and OCV classes.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-finite derate, a side that does not select
    /// exactly one of early or late, or revision exhaustion when values change.
    pub fn set_timing_derate(
        &mut self,
        derate: f64,
        side: LatencySide,
        edges: EdgeSelection,
        scope: DesignRuleScope,
        kinds: &[TimingDerateKind],
    ) -> Result<ConstraintChange, crate::TimingError> {
        if !derate.is_finite() {
            return Err(crate::ConstraintError::InvalidValue {
                command: "set_timing_derate",
                value: derate,
            }
            .into());
        }
        let early_late_index = match side {
            LatencySide::Early => 0,
            LatencySide::Late => 1,
            LatencySide::Both => {
                return Err(crate::ConstraintError::InvalidTimingDerateSelection.into());
            }
        };
        let kinds = if kinds.is_empty() {
            vec![TimingDerateKind::NetDelay, TimingDerateKind::CellDelay]
        } else {
            kinds.to_vec()
        };
        let mut replacement = self.timing_derates;
        for kind in kinds {
            for path_index in 0..2 {
                let selected = match scope {
                    DesignRuleScope::All | DesignRuleScope::ClockAndData => true,
                    DesignRuleScope::ClockPath => path_index == 0,
                    DesignRuleScope::DataPath => path_index == 1,
                };
                if !selected {
                    continue;
                }
                for edge in TimingEdge::ALL {
                    if !edges.matches(edge) {
                        continue;
                    }
                    replacement.0[kind.index()][path_index][early_late_index][edge.index()] =
                        derate;
                }
            }
        }
        if replacement == self.timing_derates {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        let previous = std::mem::replace(&mut self.timing_derates, replacement);
        self.record_undo(TimingUndo::TimingDerates(previous));
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }

    /// Removes all global timing derates.
    ///
    /// # Errors
    ///
    /// Returns an error if revision allocation fails when non-default derates
    /// are present.
    pub fn unset_timing_derate(&mut self) -> Result<ConstraintChange, crate::TimingError> {
        let replacement = TimingDerates::default();
        if self.timing_derates == replacement {
            return Ok(ConstraintChange::Unchanged);
        }
        let next_revision = self.next_revision()?;
        let previous = std::mem::replace(&mut self.timing_derates, replacement);
        self.record_undo(TimingUndo::TimingDerates(previous));
        self.revision = next_revision;
        Ok(ConstraintChange::Changed)
    }
}
