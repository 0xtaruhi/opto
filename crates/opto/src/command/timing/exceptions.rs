// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Path-exception and I/O delay command implementations.

use super::*;

pub(super) fn set_path_exception_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: PathExceptionArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut from = None;
    let mut through = Vec::new();
    let mut to = None;
    for option in args.points {
        let (role, edge) = match option.name() {
            "-from" => (PathPointRole::From, EdgeSelection::Both),
            "-rise_from" => (PathPointRole::From, EdgeSelection::Rise),
            "-fall_from" => (PathPointRole::From, EdgeSelection::Fall),
            "-through" => (PathPointRole::Through, EdgeSelection::Both),
            "-rise_through" => (PathPointRole::Through, EdgeSelection::Rise),
            "-fall_through" => (PathPointRole::Through, EdgeSelection::Fall),
            "-to" => (PathPointRole::To, EdgeSelection::Both),
            "-rise_to" => (PathPointRole::To, EdgeSelection::Rise),
            "-fall_to" => (PathPointRole::To, EdgeSelection::Fall),
            _ => unreachable!("path-exception schema uses fixed point options"),
        };
        let endpoints = resolve_path_points(state, interp, command, &option.value(), role)?;
        match role {
            PathPointRole::From => {
                if from.replace((endpoints, edge)).is_some() {
                    return Err(crate::ShellError::command(format!(
                        "{command}: only one -from/-rise_from/-fall_from option is allowed"
                    )));
                }
            }
            PathPointRole::Through => through.push((endpoints, edge)),
            PathPointRole::To => {
                if to.replace((endpoints, edge)).is_some() {
                    return Err(crate::ShellError::command(format!(
                        "{command}: only one -to/-rise_to/-fall_to option is allowed"
                    )));
                }
            }
        }
    }

    let corner = if args.setup == args.hold {
        ExceptionCorner::Both
    } else if args.setup {
        ExceptionCorner::Setup
    } else {
        ExceptionCorner::Hold
    };
    let global_edge = edge_selection(args.rise, args.fall);
    let (from_objects, from_edge) = from.unwrap_or_else(|| (Vec::new(), EdgeSelection::Both));
    let (to_objects, to_edge) = to.unwrap_or_else(|| (Vec::new(), EdgeSelection::Both));
    let (through_filters, through_edges): (Vec<_>, Vec<_>) = through
        .into_iter()
        .map(|(objects, edge)| (ExceptionFilter::new(objects), edge))
        .unzip();
    let kind = match args.kind {
        PathExceptionCommand::FalsePath => PathExceptionKind::FalsePath,
        PathExceptionCommand::MaxDelay(delay) => PathExceptionKind::MaxDelay { delay },
        PathExceptionCommand::MinDelay(delay) => PathExceptionKind::MinDelay { delay },
        PathExceptionCommand::MultiCycle(cycles) => PathExceptionKind::MultiCycle {
            cycles,
            use_end_clock: if args.start {
                false
            } else if args.end {
                true
            } else {
                !args.hold || args.setup
            },
        },
    };
    let corner = match &kind {
        PathExceptionKind::MaxDelay { .. } => ExceptionCorner::Setup,
        PathExceptionKind::MinDelay { .. } => ExceptionCorner::Hold,
        _ => corner,
    };
    let exception = PathException {
        kind,
        from: ExceptionFilter::new(from_objects),
        through: through_filters.into_boxed_slice(),
        to: ExceptionFilter::new(to_objects),
        edges: EdgeQualifier::new(from_edge, through_edges, to_edge, global_edge),
        corner,
        ignore_clock_latency: args.ignore_clock_latency,
        comment: args.comment,
    };
    let mut session = state.session.borrow_mut();
    if command == "unset_path_exceptions" {
        if args.start
            || args.end
            || args.reset_path
            || args.ignore_clock_latency
            || !exception.comment.is_empty()
        {
            return Err(crate::ShellError::command(
                "unset_path_exceptions: unsupported path-setting option",
            ));
        }
        session.unset_path_exceptions(&exception)
    } else {
        session.set_path_exception_with_reset(exception, args.reset_path)
    }
    .map_err(crate::ShellError::from)
}

pub(super) fn resolve_path_points(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    raw: &TclArg<'_>,
    role: PathPointRole,
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
    if endpoints.is_empty() {
        return Err(crate::ShellError::command(format!(
            "{command}: path point '{raw}' resolved to an empty collection"
        )));
    }
    for endpoint in &endpoints {
        let valid = match role {
            PathPointRole::From | PathPointRole::To => !matches!(endpoint, TimingEndpoint::Net(_)),
            PathPointRole::Through => !matches!(endpoint, TimingEndpoint::Clock(_)),
        };
        if !valid {
            return Err(crate::ShellError::command(format!(
                "{command}: object class is not valid for this path point"
            )));
        }
    }
    Ok(endpoints)
}

pub(super) fn set_io_delay_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetIoDelayArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let clock = args
        .clock
        .map(|value| resolve_single_io_clock(state, interp, command, &value))
        .transpose()?;
    let ports = if let Some(ids) = state
        .session
        .borrow()
        .port_ids_if_handle(command, &args.ports)?
    {
        ids
    } else {
        let names = split_tcl_list(interp, &args.ports)?;
        state
            .session
            .borrow_mut()
            .resolve_port_ids(command, &names)?
    };
    let kind = match command {
        "set_input_delay" => opto_session::IoDelayKind::Input,
        "set_output_delay" => opto_session::IoDelayKind::Output,
        _ => unreachable!("I/O delay parser is bound to fixed commands"),
    };
    state
        .session
        .borrow_mut()
        .set_io_delay(
            opto_session::IoDelaySpec {
                kind,
                delay: args.delay,
                clock,
                clock_edge: if args.clock_fall {
                    opto_session::TimingEdge::Fall
                } else {
                    opto_session::TimingEdge::Rise
                },
                edges: opto_session::EdgeSelection::from_flags(args.rise, args.fall),
                corners: opto_session::CornerSelection::from_flags(args.min, args.max),
                source_latency_included: args.source_latency_included,
                network_latency_included: args.network_latency_included,
                add_delay: args.add_delay,
            },
            &ports,
        )
        .map_err(crate::ShellError::from)
}

pub(super) fn unset_io_delay_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: UnsetIoDelayArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let clock = args
        .clock
        .map(|value| resolve_single_io_clock(state, interp, command, &value))
        .transpose()?;
    let ports = if let Some(ids) = state
        .session
        .borrow()
        .port_ids_if_handle(command, &args.ports)?
    {
        ids
    } else {
        let names = split_tcl_list(interp, &args.ports)?;
        state
            .session
            .borrow_mut()
            .resolve_port_ids(command, &names)?
    };
    let kind = if command == "unset_input_delay" {
        opto_session::IoDelayKind::Input
    } else {
        opto_session::IoDelayKind::Output
    };
    state
        .session
        .borrow_mut()
        .unset_io_delay(
            kind,
            clock,
            if args.clock_fall {
                opto_session::TimingEdge::Fall
            } else {
                opto_session::TimingEdge::Rise
            },
            opto_session::EdgeSelection::from_flags(args.rise, args.fall),
            opto_session::CornerSelection::from_flags(args.min, args.max),
            &ports,
        )
        .map_err(crate::ShellError::from)
}

/// Collapse the mutually inclusive `-setup` and `-hold` flags.
pub(super) fn uncertainty_corner(setup: bool, hold: bool) -> opto_session::ExceptionCorner {
    match (setup, hold) {
        (true, false) => opto_session::ExceptionCorner::Setup,
        (false, true) => opto_session::ExceptionCorner::Hold,
        _ => opto_session::ExceptionCorner::Both,
    }
}
