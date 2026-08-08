// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::timing_model;
use crate::{
    CaseAnalysisValue, ClockGroupKind, ConstraintChange, DesignRuleScope, DisabledTiming,
    EdgeSelection, IoDelayKind, ReportTimingOptions, Session, SessionError, TimingDerateKind,
    TimingEdge, TimingEndpoint, TimingObject,
};
use opto_db::{ClockId, ClockObject, NetId, ObjectLocator, PortId};
use opto_timing::{ClockSpec, CornerSelection, ExceptionCorner, IoDelaySpec, LatencySide};
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

fn create_clock_uncommitted(
    session: &mut Session,
    clock: ClockSpec,
    add: bool,
) -> Result<String, SessionError> {
    let message = format!("Created clock '{}' period {:.3}", clock.name, clock.period);
    let id = session
        .state
        .objects
        .intern(ObjectLocator::Clock {
            name: clock.name.clone(),
        })
        .map_err(SessionError::Registry)?
        .downcast::<ClockObject>()
        .ok_or_else(|| SessionError::state("create_clock: registry returned a non-clock ID"))?;
    session.state.timing.create_clock_with_add(id, clock, add)?;
    Ok(message)
}

fn create_generated_clock_uncommitted(
    session: &mut Session,
    name: &str,
    targets: Vec<PortId>,
    generated: opto_timing::GeneratedClock,
    add: bool,
) -> Result<String, SessionError> {
    let id = session
        .state
        .objects
        .intern(ObjectLocator::Clock {
            name: name.to_string(),
        })
        .map_err(SessionError::Registry)?
        .downcast::<ClockObject>()
        .ok_or_else(|| {
            SessionError::state("create_generated_clock: registry returned a non-clock ID")
        })?;
    session
        .state
        .timing
        .create_generated_clock(id, name.to_string(), targets, generated, add)?;
    Ok(format!("Created generated clock '{name}'"))
}
impl Session {
    /// Create a clock on the selected source ports.
    pub fn create_clock(
        &mut self,
        name: &str,
        period: f64,
        sources: Vec<PortId>,
        waveform: Option<(f64, f64)>,
    ) -> Result<String, SessionError> {
        self.create_clock_with_options(ClockSpec::new(name, period, sources, waveform)?, false)
    }

    /// Create a clock with DC-compatible source-extension and comment options.
    pub fn create_clock_with_options(
        &mut self,
        clock: ClockSpec,
        add: bool,
    ) -> Result<String, SessionError> {
        let checkpoint = self.constraint_checkpoint();
        let result = create_clock_uncommitted(self, clock, add);
        match result {
            Ok(message) => {
                self.commit_constraint_checkpoint(checkpoint)?;
                Ok(message)
            }
            Err(error) => {
                self.restore_constraint_checkpoint(checkpoint)?;
                Err(error)
            }
        }
    }

    /// Create a generated clock on top-level target ports.
    pub fn create_generated_clock(
        &mut self,
        name: &str,
        targets: Vec<PortId>,
        generated: opto_timing::GeneratedClock,
        add: bool,
    ) -> Result<String, SessionError> {
        let checkpoint = self.constraint_checkpoint();
        let result = create_generated_clock_uncommitted(self, name, targets, generated, add);
        match result {
            Ok(message) => {
                self.commit_constraint_checkpoint(checkpoint)?;
                Ok(message)
            }
            Err(error) => {
                self.restore_constraint_checkpoint(checkpoint)?;
                Err(error)
            }
        }
    }

    /// Delete selected clocks and generated clocks that depend on them.
    pub fn delete_clocks(
        &mut self,
        clocks: &[ClockId],
        generated_only: bool,
    ) -> Result<ConstraintChange, SessionError> {
        if clocks.is_empty() {
            return Err(SessionError::state(
                "delete_clock: clock collection is empty",
            ));
        }
        for clock in clocks {
            if !self.state.timing.contains_clock(*clock) {
                return Err(SessionError::state(format!(
                    "delete_clock: clock ID {clock:?} has no live constraint"
                )));
            }
            if generated_only && !self.state.timing.is_generated_clock(*clock) {
                return Err(SessionError::state(format!(
                    "delete_generated_clock: clock ID {clock:?} is not generated"
                )));
            }
        }
        let removed = self.state.timing.clock_removal_closure(clocks);
        super::super::transaction::delete_objects(self, &removed)?;
        Ok(ConstraintChange::Changed)
    }

    /// Render all clocks in stable object order.
    pub fn report_clock(&self) -> String {
        let clocks = self.state.timing.clock_report(|id| {
            self.state
                .objects
                .resolve(id.erase())
                .map(|object| object.object_name().to_string())
        });
        opto_formats::report_clock(&clocks).render_plain()
    }

    /// Write the live typed timing constraints as executable SDC.
    pub fn write_sdc(&self, path: &Path) -> Result<String, SessionError> {
        let contents = self.state.timing.write_sdc(|id| {
            self.state
                .objects
                .resolve(id)
                .map(|object| object.object_name().to_string())
        })?;
        super::atomic_file::write_atomically(path, "write_sdc", |file| {
            file.write_all(contents.as_bytes())
                .map_err(|source| SessionError::Io {
                    operation: "write_sdc",
                    path: path.to_path_buf(),
                    source,
                })
        })?;
        Ok(path.display().to_string())
    }

    /// Validate timing constraints and report unconstrained endpoints.
    pub fn check_timing(&self) -> Result<String, SessionError> {
        let model = timing_model::current_timing_model(self)?;
        let analysis = self.state.timing.analyze_check_timing(&model);
        Ok(opto_formats::report_timing_checks(&analysis).render_plain())
    }

    /// Set input slew on top-level input or inout ports.
    pub fn set_input_transition(
        &mut self,
        transition: f64,
        objects: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_input_transition(transition, objects)
            .map_err(Into::into)
    }

    /// Set selected rise/fall and min/max input-transition slots.
    pub fn set_input_transition_slots(
        &mut self,
        transition: f64,
        rise: bool,
        fall: bool,
        min: bool,
        max: bool,
        objects: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_input_transition_slots(
                transition,
                EdgeSelection::from_flags(rise, fall),
                CornerSelection::from_flags(min, max),
                objects,
            )
            .map_err(Into::into)
    }

    /// Set selected rise/fall and min/max clock-transition components.
    pub fn set_clock_transition(
        &mut self,
        transition: f64,
        rise: bool,
        fall: bool,
        min: bool,
        max: bool,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_clock_transition(
                transition,
                opto_timing::EdgeSelection::from_flags(rise, fall),
                opto_timing::CornerSelection::from_flags(min, max),
                clocks,
            )
            .map_err(Into::into)
    }

    /// Remove explicitly configured transitions from selected clocks.
    pub fn unset_clock_transition(
        &mut self,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_clock_transition(clocks)
            .map_err(Into::into)
    }

    /// Set selected source or network clock-latency slots.
    pub fn set_clock_latency(
        &mut self,
        delay: f64,
        source: bool,
        edges: EdgeSelection,
        corners: CornerSelection,
        side: LatencySide,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_clock_latency(delay, source, edges, corners, side, clocks)
            .map_err(Into::into)
    }

    /// Remove source or network latency from selected clocks.
    pub fn unset_clock_latency(
        &mut self,
        source: bool,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_clock_latency(source, clocks)
            .map_err(Into::into)
    }

    /// Mark clocks as propagated or ideal.
    pub fn set_propagated_clock(
        &mut self,
        propagated: bool,
        clocks: &[ClockId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_propagated_clock(propagated, clocks)
            .map_err(Into::into)
    }

    /// Set intra- or inter-clock uncertainty.
    pub fn set_clock_uncertainty(
        &mut self,
        uncertainty: f64,
        from: &[ClockId],
        from_edge: EdgeSelection,
        to: &[ClockId],
        to_edge: EdgeSelection,
        corner: ExceptionCorner,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_clock_uncertainty(uncertainty, from, from_edge, to, to_edge, corner)
            .map_err(Into::into)
    }

    /// Remove matching clock uncertainty relationships.
    pub fn unset_clock_uncertainty(
        &mut self,
        from: &[ClockId],
        from_edge: EdgeSelection,
        to: &[ClockId],
        to_edge: EdgeSelection,
        corner: ExceptionCorner,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_clock_uncertainty(from, from_edge, to, to_edge, corner)
            .map_err(Into::into)
    }

    /// Declare mutually exclusive or asynchronous clock groups.
    pub fn set_clock_groups(
        &mut self,
        kind: ClockGroupKind,
        name: &str,
        groups: &[Vec<ClockId>],
        comment: &str,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_clock_groups(kind, name, groups, comment)
            .map_err(Into::into)
    }

    /// Remove named or all clock-group relationships of one kind.
    pub fn unset_clock_groups(
        &mut self,
        kind: ClockGroupKind,
        names: Option<&BTreeSet<String>>,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_clock_groups(kind, names)
            .map_err(Into::into)
    }

    /// Set case analysis on ports or pins.
    pub fn set_case_analysis(
        &mut self,
        value: CaseAnalysisValue,
        endpoints: &[TimingEndpoint],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_case_analysis(value, endpoints)
            .map_err(Into::into)
    }

    /// Remove case analysis from ports or pins.
    pub fn unset_case_analysis(
        &mut self,
        endpoints: &[TimingEndpoint],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_case_analysis(endpoints)
            .map_err(Into::into)
    }

    /// Disable selected timing arcs through ports, pins, or leaf cells.
    pub fn set_disable_timing(
        &mut self,
        constraints: &[DisabledTiming],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_disable_timing(constraints)
            .map_err(Into::into)
    }

    /// Remove selected disabled-timing rows.
    pub fn unset_disable_timing(
        &mut self,
        constraints: &[DisabledTiming],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_disable_timing(constraints)
            .map_err(Into::into)
    }

    /// Set global OCV derates used by timing propagation and checks.
    pub fn set_timing_derate(
        &mut self,
        derate: f64,
        side: LatencySide,
        edges: EdgeSelection,
        scope: DesignRuleScope,
        kinds: &[TimingDerateKind],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_timing_derate(derate, side, edges, scope, kinds)
            .map_err(Into::into)
    }

    /// Remove every global timing derate.
    pub fn unset_timing_derate(&mut self) -> Result<ConstraintChange, SessionError> {
        self.state.timing.unset_timing_derate().map_err(Into::into)
    }

    /// Set external capacitive load on top-level output ports.
    pub fn set_load(
        &mut self,
        load: f64,
        objects: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_load(load, objects)
            .map_err(Into::into)
    }

    /// Set selected rise/fall and min/max external-load slots.
    pub fn set_load_slots(
        &mut self,
        load: f64,
        rise: bool,
        fall: bool,
        min: bool,
        max: bool,
        objects: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_load_slots(
                load,
                EdgeSelection::from_flags(rise, fall),
                CornerSelection::from_flags(min, max),
                objects,
            )
            .map_err(Into::into)
    }

    /// Set external source resistance on input ports.
    pub fn set_drive(
        &mut self,
        resistance: f64,
        rise: bool,
        fall: bool,
        min: bool,
        max: bool,
        ports: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_drive(
                resistance,
                opto_timing::EdgeSelection::from_flags(rise, fall),
                opto_timing::CornerSelection::from_flags(min, max),
                ports,
            )
            .map_err(Into::into)
    }

    /// Set explicit resistance on logical nets.
    pub fn set_resistance(
        &mut self,
        resistance: f64,
        min: bool,
        max: bool,
        nets: &[NetId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_resistance(resistance, CornerSelection::from_flags(min, max), nets)
            .map_err(Into::into)
    }

    /// Remove selected input/output delay slots.
    pub fn unset_io_delay(
        &mut self,
        kind: IoDelayKind,
        clock: Option<ClockId>,
        clock_edge: TimingEdge,
        edges: EdgeSelection,
        corners: CornerSelection,
        ports: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_io_delay(kind, clock, clock_edge, edges, corners, ports)
            .map_err(Into::into)
    }

    /// Set clock-relative or unclocked input/output delays on top-level ports.
    pub fn set_io_delay(
        &mut self,
        spec: IoDelaySpec,
        ports: &[PortId],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_io_delay(spec, ports)
            .map_err(Into::into)
    }

    /// Set a maximum transition design rule on resolved timing objects.
    pub fn set_max_transition(
        &mut self,
        limit: f64,
        objects: &[TimingObject],
        scope: DesignRuleScope,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_max_transition(limit, objects, scope)
            .map_err(Into::into)
    }

    /// Set a maximum capacitance design rule on resolved timing objects.
    pub fn set_max_capacitance(
        &mut self,
        limit: f64,
        objects: &[TimingObject],
        scope: DesignRuleScope,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_max_capacitance(limit, objects, scope)
            .map_err(Into::into)
    }

    /// Set a maximum fanout design rule on resolved timing objects.
    pub fn set_max_fanout(
        &mut self,
        limit: f64,
        objects: &[TimingObject],
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_max_fanout(limit, objects)
            .map_err(Into::into)
    }

    /// Constrain maximum delay between endpoint sets.
    pub fn set_max_delay(
        &mut self,
        delay: f64,
        from: Vec<TimingEndpoint>,
        to: Vec<TimingEndpoint>,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_max_delay(delay, from, to)
            .map_err(Into::into)
    }

    /// Adds a validated false-path, multicycle, or path-delay exception.
    pub fn set_path_exception(
        &mut self,
        exception: opto_timing::PathException,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_path_exception(exception)
            .map_err(Into::into)
    }

    /// Adds a path exception after optionally resetting the same qualified path.
    pub fn set_path_exception_with_reset(
        &mut self,
        exception: opto_timing::PathException,
        reset_path: bool,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .set_path_exception_with_reset(exception, reset_path)
            .map_err(Into::into)
    }

    /// Remove path exceptions matching the selected points, edges, and corner.
    pub fn unset_path_exceptions(
        &mut self,
        selection: &opto_timing::PathException,
    ) -> Result<ConstraintChange, SessionError> {
        self.state
            .timing
            .unset_path_exceptions(selection)
            .map_err(Into::into)
    }

    /// Analyze and render timing paths for the current design.
    pub fn report_timing(&self, options: &ReportTimingOptions) -> Result<String, SessionError> {
        let model = timing_model::current_timing_model(self)?;
        let analyses =
            self.process
                .timing_engine
                .analyze_paths(&self.state.timing, model, options)?;
        Ok(opto_formats::report_timing(&analyses).render_plain())
    }
}
