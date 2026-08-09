// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(name = "report_power", handler = report_power)]
pub(crate) struct ReportPowerArgs {
    #[arg(long = "-cell", conflicts_with = "net")]
    cell: bool,
    #[arg(long = "-net")]
    net: bool,
    #[arg(long = "-flat")]
    flat: bool,
    #[arg(long = "-include_input_nets")]
    include_input_nets: bool,
    #[arg(
        long = "-analysis_effort",
        value_hint = ValueHint::OneOf {
            accepted: &["low", "medium", "high"],
            suggested: &["low"],
        }
    )]
    analysis_effort: Vec<String>,
    #[arg(long = "-groups", unsupported, value_hint = ValueHint::Text)]
    _groups: (),
    #[arg(long = "-only", unsupported, value_hint = ValueHint::Text)]
    _only: (),
    #[arg(long = "-exclude_boundary_nets", unsupported)]
    _exclude_boundary_nets: (),
    #[arg(long = "-verbose", unsupported)]
    _verbose: (),
    #[arg(long = "-nworst", unsupported, value_hint = ValueHint::Text)]
    _nworst: (),
    #[arg(long = "-sort_mode", unsupported, value_hint = ValueHint::Text)]
    _sort_mode: (),
    #[arg(long = "-histogram", unsupported)]
    _histogram: (),
    #[arg(long = "-exclude_leq", unsupported, value_hint = ValueHint::Text)]
    _exclude_leq: (),
    #[arg(long = "-exclude_geq", unsupported, value_hint = ValueHint::Text)]
    _exclude_geq: (),
    #[arg(long = "-hierarchy", unsupported)]
    _hierarchy: (),
    #[arg(long = "-levels", unsupported, value_hint = ValueHint::Text)]
    _levels: (),
    #[arg(long = "-scenarios", unsupported, value_hint = ValueHint::Text)]
    _scenarios: (),
}

#[derive(TclCommand)]
#[command(name = "set_switching_activity", handler = set_switching_activity)]
pub(crate) struct SetSwitchingActivityArgs<'a> {
    #[arg(long = "-static_probability")]
    static_probability: Option<f64>,
    #[arg(long = "-toggle_rate")]
    toggle_rate: Option<f64>,
    #[arg(long = "-rise_ratio")]
    rise_ratio: Option<f64>,
    #[arg(long = "-period")]
    period: Option<f64>,
    #[arg(long = "-state_condition", unsupported, value_hint = ValueHint::Text)]
    _state_condition: (),
    #[arg(long = "-path_sources", unsupported, value_hint = ValueHint::Text)]
    _path_sources: (),
    #[arg(long = "-base_clock", unsupported, value_hint = ValueHint::Clock)]
    _base_clock: (),
    #[arg(long = "-type", unsupported, value_hint = ValueHint::Text)]
    _type: (),
    #[arg(long = "-hierarchy", unsupported)]
    _hierarchy: (),
    #[arg(long = "-scenarios", unsupported, value_hint = ValueHint::Text)]
    _scenarios: (),
    #[arg(positional)]
    objects: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "reset_switching_activity", handler = reset_switching_activity)]
pub(crate) struct ResetSwitchingActivityArgs<'a> {
    #[arg(positional)]
    objects: Vec<TclArg<'a>>,
}

pub(crate) fn set_switching_activity(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetSwitchingActivityArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let mut update = SwitchingActivityUpdate {
        static_probability: args.static_probability,
        toggle_rate: args.toggle_rate,
        rise_ratio: args.rise_ratio,
    };
    let mut objects = Vec::new();
    for object in &args.objects {
        objects.extend(resolve_power_argument(state, interp, command, object)?);
    }
    if update.static_probability.is_none()
        && update.toggle_rate.is_none()
        && update.rise_ratio.is_none()
    {
        return Err(crate::ShellError::command(format!(
            "{command}: specify -static_probability, -toggle_rate, or -rise_ratio"
        )));
    }
    if let Some(period) = args.period
        && (!period.is_finite() || period <= 0.0)
    {
        return Err(crate::ShellError::command(format!(
            "{command}: -period must be positive and finite"
        )));
    }
    if let (Some(toggle_rate), Some(period)) = (update.toggle_rate, args.period) {
        update.toggle_rate = Some(toggle_rate / period);
    } else if args.period.is_some() {
        return Err(crate::ShellError::command(format!(
            "{command}: -period requires -toggle_rate"
        )));
    }
    state
        .session
        .borrow_mut()
        .set_switching_activity(update, &objects)
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn reset_switching_activity(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: ResetSwitchingActivityArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let mut objects = Vec::new();
    for arg in &args.objects {
        objects.extend(resolve_power_argument(state, interp, command, arg)?);
    }
    state
        .session
        .borrow_mut()
        .reset_switching_activity(&objects)
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn report_power(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: ReportPowerArgs,
) -> Result<CommandResult, crate::ShellError> {
    let _ = interp;
    let mut options = ReportPowerOptions::default();
    if args.cell {
        options.kind = PowerReportKind::Cell;
    } else if args.net {
        options.kind = PowerReportKind::Net;
    }
    options.flat = args.flat;
    options.include_input_nets = args.include_input_nets;
    for value in args.analysis_effort {
        match value.as_str() {
            "low" => {}
            "medium" | "high" => {
                return Err(crate::ShellError::command(format!(
                    "{command}: -analysis_effort {value} is not implemented yet"
                )));
            }
            _ => unreachable!("schema validates report_power analysis effort"),
        }
    }
    state
        .session
        .borrow()
        .report_power(&options)
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

fn resolve_power_argument(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<Vec<opto_session::AnyObjectId>, crate::ShellError> {
    if let Some(objects) = state
        .session
        .borrow()
        .power_objects_if_handle(command, value.as_str())?
    {
        return Ok(objects);
    }
    let names = split_tcl_list(interp, value)?;
    state
        .session
        .borrow_mut()
        .resolve_power_objects(command, &names)
        .map_err(crate::ShellError::from)
}
