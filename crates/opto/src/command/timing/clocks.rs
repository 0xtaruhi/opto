// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Clock command implementations.

use super::*;

pub(super) fn create_clock_command(
    state: &ShellState,
    args: CreateClockArgs<'_>,
) -> Result<String, crate::ShellError> {
    let mut sources = Vec::new();
    for source in args.sources {
        sources.extend(resolve_clock_sources(state, source.as_str())?);
    }
    let name = match args.name {
        Some(name) => name,
        None if sources.len() == 1 => state.session.borrow().port_name(sources[0])?,
        None if sources.is_empty() => {
            return Err(crate::ShellError::command(
                "create_clock: missing -name for virtual clock",
            ));
        }
        None => {
            return Err(crate::ShellError::command(
                "create_clock: -name is required for multiple sources",
            ));
        }
    };
    let waveform = args
        .waveform
        .map(|waveform| parse_waveform(&waveform))
        .transpose()?;
    let mut clock = opto_session::ClockSpec::new(&name, args.period, sources, waveform)
        .map_err(opto_session::SessionError::from)
        .map_err(crate::ShellError::from)?;
    clock.comment = args.comment.unwrap_or_default();
    state
        .session
        .borrow_mut()
        .create_clock_with_options(clock, args.add)
        .map_err(crate::ShellError::from)
}

pub(super) fn create_generated_clock_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: CreateGeneratedClockArgs<'_>,
) -> Result<String, crate::ShellError> {
    let source = resolve_single_port(state, interp, "create_generated_clock", &args.source)?;
    let targets = resolve_port_list(state, interp, "create_generated_clock", &args.targets)?;
    let master = if let Some(master) = args.master_clock {
        resolve_single_io_clock(state, interp, "create_generated_clock", &master)?
    } else {
        match state.session.borrow().clocks_on_port(source).as_slice() {
            [master] => *master,
            [] => {
                return Err(crate::ShellError::command(
                    "create_generated_clock: no master clock found on -source",
                ));
            }
            _ => {
                return Err(crate::ShellError::command(
                    "create_generated_clock: -master_clock is required for a multi-clock source",
                ));
            }
        }
    };
    let name = match args.name {
        Some(name) => name,
        None if targets.len() == 1 => state.session.borrow().port_name(targets[0])?,
        None => {
            return Err(crate::ShellError::command(
                "create_generated_clock: -name is required for multiple targets",
            ));
        }
    };
    state
        .session
        .borrow_mut()
        .create_generated_clock(
            &name,
            targets,
            opto_session::GeneratedClock {
                master,
                source,
                divide_by: args.divide_by,
                multiply_by: args.multiply_by,
                duty_cycle: args.duty_cycle,
                invert: args.invert,
                edges: args
                    .edges
                    .map(|value| parse_generated_u32_triple(interp, &value, "-edges"))
                    .transpose()?,
                edge_shift: args
                    .edge_shift
                    .map(|value| parse_generated_f64_triple(interp, &value, "-edge_shift"))
                    .transpose()?,
                combinational: args.combinational,
                comment: args.comment.unwrap_or_default(),
            },
            args.add,
        )
        .map_err(crate::ShellError::from)
}

pub(super) fn set_clock_transition_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: SetClockTransitionArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut clocks = Vec::new();
    for arg in &args.clocks {
        if let Some(ids) = state
            .session
            .borrow()
            .clock_ids_if_handle("set_clock_transition", arg)?
        {
            clocks.extend(ids);
        } else {
            let names = split_tcl_list(interp, arg)?;
            clocks.extend(
                state
                    .session
                    .borrow()
                    .resolve_clock_ids("set_clock_transition", &names)?,
            );
        }
    }
    state
        .session
        .borrow_mut()
        .set_clock_transition(
            args.transition,
            args.rise,
            args.fall,
            args.min,
            args.max,
            &clocks,
        )
        .map_err(crate::ShellError::from)
}

pub(super) fn unset_clock_transition_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: UnsetClockTransitionArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let clocks = resolve_clock_list(state, interp, "unset_clock_transition", &args.clocks)?;
    state
        .session
        .borrow_mut()
        .unset_clock_transition(&clocks)
        .map_err(crate::ShellError::from)
}

pub(super) fn set_clock_latency_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: SetClockLatencyArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let clocks = resolve_clock_list(state, interp, "set_clock_latency", &args.clocks)?;
    state
        .session
        .borrow_mut()
        .set_clock_latency(
            args.delay,
            args.source,
            opto_session::EdgeSelection::from_flags(args.rise, args.fall),
            opto_session::CornerSelection::from_flags(args.min, args.max),
            opto_session::LatencySide::from_flags(args.early, args.late),
            &clocks,
        )
        .map_err(crate::ShellError::from)
}

pub(super) fn unset_clock_latency_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: UnsetClockLatencyArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let clocks = resolve_clock_list(state, interp, "unset_clock_latency", &args.clocks)?;
    state
        .session
        .borrow_mut()
        .unset_clock_latency(args.source, &clocks)
        .map_err(crate::ShellError::from)
}

pub(super) fn clock_uncertainty_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    set: bool,
    args: ClockUncertaintyCommandArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let (from_arg, from_edge) = clock_uncertainty_selector(command, "from", args.from)?;
    let (to_arg, mut to_edge) = clock_uncertainty_selector(command, "to", args.to)?;
    let inter_clock = from_arg.is_some() || to_arg.is_some();
    if from_arg.is_some() != to_arg.is_some() {
        return Err(crate::ShellError::command(format!(
            "{command}: -from and -to must be used together"
        )));
    }
    let expected = usize::from(set) + usize::from(!inter_clock);
    if args.positionals.len() != expected {
        return Err(crate::ShellError::command(format!(
            "{command}: expected {expected} positional argument(s)"
        )));
    }
    let (from, to) = if let (Some(from), Some(to)) = (from_arg, to_arg) {
        if args.rise != args.fall {
            to_edge = if args.rise {
                opto_session::EdgeSelection::Rise
            } else {
                opto_session::EdgeSelection::Fall
            };
        }
        (
            resolve_clock_list(state, interp, command, &from)?,
            resolve_clock_list(state, interp, command, &to)?,
        )
    } else {
        if args.rise || args.fall {
            return Err(crate::ShellError::command(format!(
                "{command}: -rise/-fall requires -from/-to"
            )));
        }
        let clocks =
            resolve_clock_list(state, interp, command, &args.positionals[usize::from(set)])?;
        (clocks.clone(), clocks)
    };
    let mut session = state.session.borrow_mut();
    if set {
        let uncertainty = args.positionals[0].parse::<f64>().map_err(|_| {
            crate::ShellError::parse(format!(
                "{command}: invalid uncertainty '{}'",
                args.positionals[0]
            ))
        })?;
        session.set_clock_uncertainty(
            uncertainty,
            &from,
            from_edge,
            &to,
            to_edge,
            uncertainty_corner(args.setup, args.hold),
        )
    } else {
        session.unset_clock_uncertainty(
            &from,
            from_edge,
            &to,
            to_edge,
            uncertainty_corner(args.setup, args.hold),
        )
    }
    .map_err(crate::ShellError::from)
}

pub(super) fn set_clock_groups_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: SetClockGroupsArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let kind = match (
        args.logically_exclusive,
        args.physically_exclusive,
        args.asynchronous,
    ) {
        (true, false, false) => opto_session::ClockGroupKind::LogicallyExclusive,
        (false, true, false) => opto_session::ClockGroupKind::PhysicallyExclusive,
        (false, false, true) => opto_session::ClockGroupKind::Asynchronous,
        (false, false, false) => {
            return Err(crate::ShellError::command(
                "set_clock_groups: one relationship option must be specified",
            ));
        }
        _ => unreachable!("derive schema validates clock-group relationship conflict"),
    };
    let mut groups = Vec::new();
    for group in &args.groups {
        groups.push(resolve_clock_list(
            state,
            interp,
            "set_clock_groups",
            group,
        )?);
    }
    state
        .session
        .borrow_mut()
        .set_clock_groups(
            kind,
            args.name.as_deref().unwrap_or_default(),
            &groups,
            args.comment.as_deref().unwrap_or_default(),
        )
        .map_err(crate::ShellError::from)
}

pub(super) fn unset_clock_groups_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: UnsetClockGroupsArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let kind = match (
        args.logically_exclusive,
        args.physically_exclusive,
        args.asynchronous,
    ) {
        (true, false, false) => opto_session::ClockGroupKind::LogicallyExclusive,
        (false, true, false) => opto_session::ClockGroupKind::PhysicallyExclusive,
        (false, false, true) => opto_session::ClockGroupKind::Asynchronous,
        (false, false, false) => {
            return Err(crate::ShellError::command(
                "unset_clock_groups: one relationship option must be specified",
            ));
        }
        _ => unreachable!("derive schema validates clock-group relationship conflict"),
    };
    if args.all == args.name.is_some() {
        return Err(crate::ShellError::command(
            "unset_clock_groups: exactly one of -all or -name is required",
        ));
    }
    let names = args
        .name
        .map(|value| {
            split_tcl_list(interp, &value)
                .map(|names| names.into_iter().collect::<std::collections::BTreeSet<_>>())
        })
        .transpose()?;
    state
        .session
        .borrow_mut()
        .unset_clock_groups(kind, names.as_ref())
        .map_err(crate::ShellError::from)
}

pub(super) fn resolve_clock_sources(
    state: &ShellState,
    value: &str,
) -> Result<Vec<opto_session::PortId>, crate::ShellError> {
    if let Some(ids) = state
        .session
        .borrow()
        .port_ids_if_handle("create_clock", value)?
    {
        if ids.is_empty() {
            return Err(crate::ShellError::command(format!(
                "create_clock: source collection '{value}' is empty"
            )));
        }
        Ok(ids)
    } else {
        state
            .session
            .borrow_mut()
            .resolve_port_ids("create_clock", &[value.to_string()])
            .map_err(crate::ShellError::from)
    }
}

pub(super) fn resolve_clock_list(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<Vec<opto_session::ClockId>, crate::ShellError> {
    if let Some(ids) = state.session.borrow().clock_ids_if_handle(command, value)? {
        if ids.is_empty() {
            return Err(crate::ShellError::command(format!(
                "{command}: clock collection is empty"
            )));
        }
        Ok(ids)
    } else {
        let names = split_tcl_list(interp, value)?;
        state
            .session
            .borrow()
            .resolve_clock_ids(command, &names)
            .map_err(crate::ShellError::from)
    }
}

pub(super) fn resolve_single_io_clock(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<opto_session::ClockId, crate::ShellError> {
    let clocks = if let Some(ids) = state.session.borrow().clock_ids_if_handle(command, value)? {
        ids
    } else {
        let names = split_tcl_list(interp, value)?;
        state.session.borrow().resolve_clock_ids(command, &names)?
    };
    match clocks.as_slice() {
        [clock] => Ok(*clock),
        [] => Err(crate::ShellError::command(format!(
            "{command}: -clock resolved to an empty collection"
        ))),
        _ => Err(crate::ShellError::command(format!(
            "{command}: -clock requires exactly one clock"
        ))),
    }
}

pub(super) fn parse_waveform(raw: &str) -> Result<(f64, f64), crate::ShellError> {
    let values = raw
        .split_whitespace()
        .map(|value| {
            value.parse::<f64>().map_err(|_| {
                crate::ShellError::parse(format!("create_clock: invalid waveform value '{value}'"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    match values.as_slice() {
        [rise, fall] => Ok((*rise, *fall)),
        _ => Err(crate::ShellError::command(format!(
            "create_clock: invalid waveform '{{{raw}}}'"
        ))),
    }
}
