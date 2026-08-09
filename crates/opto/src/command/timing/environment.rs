// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Environment, design-rule and analysis-mode command implementations.

use super::*;

pub(super) fn set_port_constraint_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: PortConstraintCommandArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut ports = Vec::new();
    for object in &args.objects {
        if let Some(ids) = state.session.borrow().port_ids_if_handle(command, object)? {
            ports.extend(ids);
        } else {
            let names = split_tcl_list(interp, object)?;
            ports.extend(
                state
                    .session
                    .borrow_mut()
                    .resolve_port_ids(command, &names)?,
            );
        }
    }
    let mut session = state.session.borrow_mut();
    match command {
        "set_input_transition" => session.set_input_transition_slots(
            args.value, args.rise, args.fall, args.min, args.max, &ports,
        ),
        "set_load" => {
            session.set_load_slots(args.value, args.rise, args.fall, args.min, args.max, &ports)
        }
        "set_drive" => {
            session.set_drive(args.value, args.rise, args.fall, args.min, args.max, &ports)
        }
        _ => unreachable!("port constraint definition uses fixed command names"),
    }
    .map_err(crate::ShellError::from)
}

pub(super) fn set_resistance_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: SetResistanceArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let endpoints = if let Some(endpoints) = state
        .session
        .borrow()
        .timing_endpoints_if_handle("set_resistance", &args.nets)?
    {
        endpoints
    } else {
        let names = split_tcl_list(interp, &args.nets)?;
        state
            .session
            .borrow_mut()
            .resolve_timing_endpoints("set_resistance", &names)?
    };
    let nets = endpoints
        .into_iter()
        .map(|endpoint| match endpoint {
            TimingEndpoint::Net(net) => Ok(net),
            _ => Err(crate::ShellError::command(
                "set_resistance: expected a net collection",
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    state
        .session
        .borrow_mut()
        .set_resistance(args.resistance, args.min, args.max, &nets)
        .map_err(crate::ShellError::from)
}

pub(super) fn set_design_rule_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    limit: f64,
    object_args: &[TclArg<'_>],
    scope: DesignRuleScope,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut objects = Vec::new();
    for object in object_args {
        objects.extend(resolve_design_rule_objects(state, interp, command, object)?);
    }
    let mut session = state.session.borrow_mut();
    let result = match command {
        "set_max_transition" => session.set_max_transition(limit, &objects, scope),
        "set_max_capacitance" => session.set_max_capacitance(limit, &objects, scope),
        "set_max_fanout" => session.set_max_fanout(limit, &objects),
        _ => {
            return Err(crate::ShellError::command(format!(
                "{command}: internal design-rule dispatch error"
            )));
        }
    };
    result.map_err(crate::ShellError::from)
}

pub(super) fn set_timing_derate_command(
    state: &ShellState,
    args: SetTimingDerateArgs,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut kinds = Vec::new();
    if args.net_delay {
        kinds.push(opto_session::TimingDerateKind::NetDelay);
    }
    if args.cell_delay {
        kinds.push(opto_session::TimingDerateKind::CellDelay);
    }
    if args.cell_check {
        kinds.push(opto_session::TimingDerateKind::CellCheck);
    }
    state
        .session
        .borrow_mut()
        .set_timing_derate(
            args.derate,
            opto_session::LatencySide::from_flags(args.early, args.late),
            opto_session::EdgeSelection::from_flags(args.rise, args.fall),
            derate_scope(args.clock, args.data),
            &kinds,
        )
        .map_err(crate::ShellError::from)
}

pub(super) fn case_analysis_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    value: Option<String>,
    objects: TclArg<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let value = value
        .map(|value| match value.as_str() {
            "0" | "zero" => Ok(opto_session::CaseAnalysisValue::Zero),
            "1" | "one" => Ok(opto_session::CaseAnalysisValue::One),
            "rise" | "rising" => Ok(opto_session::CaseAnalysisValue::Rise),
            "fall" | "falling" => Ok(opto_session::CaseAnalysisValue::Fall),
            value => Err(crate::ShellError::command(format!(
                "set_case_analysis: invalid value '{value}'"
            ))),
        })
        .transpose()?;
    let endpoints = resolve_case_analysis_endpoints(state, interp, command, &objects)?;
    let mut session = state.session.borrow_mut();
    match value {
        Some(value) => session.set_case_analysis(value, &endpoints),
        None => session.unset_case_analysis(&endpoints),
    }
    .map_err(crate::ShellError::from)
}

pub(super) fn disable_timing_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: DisableTimingArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    if args.from.len() > 1 {
        return Err(crate::ShellError::command(format!(
            "{command}: -from specified more than once"
        )));
    }
    if args.to.len() > 1 {
        return Err(crate::ShellError::command(format!(
            "{command}: -to specified more than once"
        )));
    }
    let from = args.from.into_iter().next();
    let to = args.to.into_iter().next();
    let endpoints = if let Some(endpoints) = state
        .session
        .borrow()
        .timing_endpoints_if_handle(command, &args.objects)?
    {
        endpoints
    } else {
        let names = split_tcl_list(interp, &args.objects)?;
        state
            .session
            .borrow_mut()
            .resolve_timing_endpoints(command, &names)?
    };
    if endpoints.is_empty()
        || endpoints.iter().any(|endpoint| {
            !matches!(
                endpoint,
                TimingEndpoint::Cell(_) | TimingEndpoint::Pin(_) | TimingEndpoint::Port(_)
            )
        })
    {
        return Err(crate::ShellError::command(format!(
            "{command}: expected a nonempty cell/port/pin collection"
        )));
    }
    let constraints = endpoints
        .into_iter()
        .map(|target| {
            let (from, to) = if matches!(target, TimingEndpoint::Cell(_)) {
                (from.clone(), to.clone())
            } else {
                (None, None)
            };
            opto_session::DisabledTiming { target, from, to }
        })
        .collect::<Vec<_>>();
    let mut session = state.session.borrow_mut();
    if command == "set_disable_timing" {
        session.set_disable_timing(&constraints)
    } else {
        session.unset_disable_timing(&constraints)
    }
    .map_err(crate::ShellError::from)
}

pub(super) fn set_logic_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetLogicArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let endpoints = resolve_case_analysis_endpoints(state, interp, command, &args.objects)?;
    let mut session = state.session.borrow_mut();
    match command {
        "set_logic_zero" => {
            session.set_case_analysis(opto_session::CaseAnalysisValue::Zero, &endpoints)
        }
        "set_logic_one" => {
            session.set_case_analysis(opto_session::CaseAnalysisValue::One, &endpoints)
        }
        "set_logic_dc" => session.unset_case_analysis(&endpoints),
        _ => unreachable!("logic parser is bound to fixed commands"),
    }
    .map_err(crate::ShellError::from)
}

pub(super) fn resolve_case_analysis_endpoints(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    raw: &TclArg<'_>,
) -> Result<Vec<TimingEndpoint>, crate::ShellError> {
    let endpoints = if let Some(endpoints) = state
        .session
        .borrow()
        .timing_endpoints_if_handle(command, raw)?
    {
        endpoints
    } else {
        let names = split_tcl_list(interp, raw)?;
        state
            .session
            .borrow_mut()
            .resolve_timing_endpoints(command, &names)?
    };
    if endpoints.is_empty()
        || endpoints
            .iter()
            .any(|endpoint| !matches!(endpoint, TimingEndpoint::Port(_) | TimingEndpoint::Pin(_)))
    {
        return Err(crate::ShellError::command(format!(
            "{command}: expected a nonempty port/pin collection"
        )));
    }
    Ok(endpoints)
}

pub(super) fn resolve_design_rule_objects(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<Vec<TimingObject>, crate::ShellError> {
    if let Some(objects) = state
        .session
        .borrow()
        .design_rule_objects_if_handle(command, value.as_str())?
    {
        if objects.is_empty() {
            return Err(crate::ShellError::command(format!(
                "{command}: object collection '{value}' is empty"
            )));
        }
        return Ok(objects);
    }
    let names = split_tcl_list(interp, value)?;
    if names.is_empty() {
        return Err(crate::ShellError::command(format!(
            "{command}: empty object list"
        )));
    }
    state
        .session
        .borrow_mut()
        .resolve_design_rule_objects(command, &names)
        .map_err(crate::ShellError::from)
}

/// Collapse the mutually inclusive `-clock` and `-data` derating flags.
pub(super) fn derate_scope(clock: bool, data: bool) -> opto_session::DesignRuleScope {
    match (clock, data) {
        (true, false) => opto_session::DesignRuleScope::ClockPath,
        (false, true) => opto_session::DesignRuleScope::DataPath,
        _ => opto_session::DesignRuleScope::All,
    }
}

pub(super) fn resolve_port_list(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<Vec<opto_session::PortId>, crate::ShellError> {
    if let Some(ids) = state.session.borrow().port_ids_if_handle(command, value)? {
        if ids.is_empty() {
            return Err(crate::ShellError::command(format!(
                "{command}: port collection is empty"
            )));
        }
        Ok(ids)
    } else {
        let names = split_tcl_list(interp, value)?;
        state
            .session
            .borrow_mut()
            .resolve_port_ids(command, &names)
            .map_err(crate::ShellError::from)
    }
}
