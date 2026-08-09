// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

mod clocks;
mod environment;
mod exceptions;

use clocks::*;
use environment::*;
use exceptions::*;

const SDC_VERSIONS: &[&str] = &[
    "1.0", "1.1", "1.2", "1.3", "1.4", "1.5", "1.6", "1.7", "1.8", "1.9", "2.0", "2.1", "2.2",
    "latest",
];

#[derive(TclCommand)]
#[command(name = "read_sdc", handler = read_sdc)]
pub(crate) struct ReadSdcArgs {
    #[arg(long = "-echo")]
    pub(crate) echo: bool,
    #[arg(long = "-syntax_only")]
    pub(crate) syntax_only: bool,
    #[arg(
        long = "-version",
        value_hint = ValueHint::OneOf {
            accepted: SDC_VERSIONS,
            suggested: SDC_VERSIONS,
        }
    )]
    pub(crate) version: Option<String>,
    #[arg(positional, value_hint = ValueHint::File)]
    pub(crate) file: PathBuf,
}

#[derive(TclCommand)]
#[command(name = "read_parasitics", handler = read_parasitics)]
pub(crate) struct ReadParasiticsArgs {
    #[arg(long = "-elmore", conflicts_with = "arnoldi")]
    elmore: bool,
    #[arg(long = "-arnoldi")]
    arnoldi: bool,
    #[arg(long = "-increment")]
    increment: bool,
    #[arg(long = "-pin_cap_included")]
    pin_cap_included: bool,
    #[arg(long = "-net_cap_only")]
    net_cap_only: bool,
    #[arg(long = "-syntax_only")]
    syntax_only: bool,
    #[arg(long = "-verbose")]
    verbose: bool,
    #[arg(
        long = "-complete_with",
        value_hint = ValueHint::OneOf {
            accepted: &["none", "zero", "wlm"],
            suggested: &["none", "zero", "wlm"],
        }
    )]
    complete_with: Option<String>,
    #[arg(long = "-path")]
    path: Option<String>,
    #[arg(long = "-strip_path")]
    strip_path: Option<String>,
    #[arg(positional, min = 1, value_hint = ValueHint::File)]
    files: Vec<PathBuf>,
}

#[derive(TclCommand)]
#[command(name = "report_timing", handler = report_timing)]
pub(crate) struct ReportTimingArgs<'a> {
    #[arg(long = "-from", value_hint = ValueHint::Port)]
    from: Vec<TclArg<'a>>,
    #[arg(long = "-to", value_hint = ValueHint::Port)]
    to: Vec<TclArg<'a>>,
    #[arg(
        long = "-delay",
        alias = "-delay_type",
        value_hint = ValueHint::OneOf {
            accepted: &["max", "min", "min_max"],
            suggested: &["max", "min"],
        }
    )]
    delay: Vec<TclOption<'a>>,
    #[arg(long = "-max_paths", value_hint = ValueHint::Suggested(&["1"]))]
    max_paths: Vec<usize>,
    #[arg(long = "-significant_digits")]
    significant_digits: Vec<usize>,
    #[arg(long = "-path", value_hint = ValueHint::Suggested(&["full"]))]
    path: Vec<String>,
}

#[derive(TclCommand)]
#[command(
    name = "create_clock",
    handler = create_clock,
    sdc,
    option_or_positional = "-name"
)]
pub(crate) struct CreateClockArgs<'a> {
    #[arg(long = "-period")]
    period: f64,
    #[arg(long = "-name")]
    name: Option<String>,
    #[arg(long = "-waveform")]
    waveform: Option<TclArg<'a>>,
    #[arg(long = "-add")]
    add: bool,
    #[arg(long = "-comment")]
    comment: Option<String>,
    #[arg(positional, value_hint = ValueHint::Port)]
    sources: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "write_sdc", handler = write_sdc, sdc)]
pub(crate) struct WriteSdcArgs {
    #[arg(positional, value_hint = ValueHint::File)]
    file: PathBuf,
}

#[derive(TclCommand)]
#[command(name = "create_generated_clock", handler = create_generated_clock, sdc)]
pub(crate) struct CreateGeneratedClockArgs<'a> {
    #[arg(long = "-name")]
    name: Option<String>,
    #[arg(long = "-source", value_hint = ValueHint::Port)]
    source: TclArg<'a>,
    #[arg(long = "-master_clock", value_hint = ValueHint::Clock)]
    master_clock: Option<TclArg<'a>>,
    #[arg(long = "-divide_by")]
    divide_by: Option<u32>,
    #[arg(long = "-multiply_by")]
    multiply_by: Option<u32>,
    #[arg(long = "-duty_cycle")]
    duty_cycle: Option<f64>,
    #[arg(long = "-invert")]
    invert: bool,
    #[arg(long = "-edges")]
    edges: Option<TclArg<'a>>,
    #[arg(long = "-edge_shift")]
    edge_shift: Option<TclArg<'a>>,
    #[arg(long = "-combinational")]
    combinational: bool,
    #[arg(long = "-add")]
    add: bool,
    #[arg(long = "-comment")]
    comment: Option<String>,
    #[arg(positional, value_hint = ValueHint::Port)]
    targets: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "delete_clock",
    alias = "delete_generated_clock",
    handler = delete_clock,
    sdc
)]
pub(crate) struct DeleteClockArgs<'a> {
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_input_transition",
    alias = "set_load",
    alias = "set_drive",
    handler = set_port_constraint,
    sdc
)]
pub(crate) struct PortConstraintCommandArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(positional)]
    value: f64,
    #[arg(positional, min = 1, value_hint = ValueHint::Port)]
    objects: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "set_clock_transition", handler = set_clock_transition, sdc)]
pub(crate) struct SetClockTransitionArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(positional)]
    transition: f64,
    #[arg(positional, min = 1, value_hint = ValueHint::Clock)]
    clocks: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "unset_clock_transition", handler = unset_clock_transition, sdc)]
pub(crate) struct UnsetClockTransitionArgs<'a> {
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(name = "set_clock_latency", handler = set_clock_latency, sdc)]
pub(crate) struct SetClockLatencyArgs<'a> {
    #[arg(long = "-source")]
    source: bool,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(long = "-early")]
    early: bool,
    #[arg(long = "-late")]
    late: bool,
    #[arg(positional)]
    delay: f64,
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(name = "unset_clock_latency", handler = unset_clock_latency, sdc)]
pub(crate) struct UnsetClockLatencyArgs<'a> {
    #[arg(long = "-source")]
    source: bool,
    #[arg(long = "-clock", unsupported, value_hint = ValueHint::Clock)]
    _clock: (),
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(name = "set_clock_uncertainty", handler = set_clock_uncertainty, sdc)]
pub(crate) struct SetClockUncertaintyArgs<'a> {
    #[arg(long = "-from", edge_aliases, value_hint = ValueHint::Clock)]
    from: Vec<TclOption<'a>>,
    #[arg(long = "-to", edge_aliases, value_hint = ValueHint::Clock)]
    to: Vec<TclOption<'a>>,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-setup")]
    setup: bool,
    #[arg(long = "-hold")]
    hold: bool,
    #[arg(positional, min = 1, max = 2)]
    positionals: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "unset_clock_uncertainty", handler = unset_clock_uncertainty, sdc)]
pub(crate) struct UnsetClockUncertaintyArgs<'a> {
    #[arg(long = "-from", edge_aliases, value_hint = ValueHint::Clock)]
    from: Vec<TclOption<'a>>,
    #[arg(long = "-to", edge_aliases, value_hint = ValueHint::Clock)]
    to: Vec<TclOption<'a>>,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-setup")]
    setup: bool,
    #[arg(long = "-hold")]
    hold: bool,
    #[arg(positional, max = 1)]
    positionals: Vec<TclArg<'a>>,
}

struct ClockUncertaintyCommandArgs<'a> {
    from: Vec<TclOption<'a>>,
    to: Vec<TclOption<'a>>,
    rise: bool,
    fall: bool,
    setup: bool,
    hold: bool,
    positionals: Vec<TclArg<'a>>,
}

impl<'a> From<SetClockUncertaintyArgs<'a>> for ClockUncertaintyCommandArgs<'a> {
    fn from(args: SetClockUncertaintyArgs<'a>) -> Self {
        Self {
            from: args.from,
            to: args.to,
            rise: args.rise,
            fall: args.fall,
            setup: args.setup,
            hold: args.hold,
            positionals: args.positionals,
        }
    }
}

impl<'a> From<UnsetClockUncertaintyArgs<'a>> for ClockUncertaintyCommandArgs<'a> {
    fn from(args: UnsetClockUncertaintyArgs<'a>) -> Self {
        Self {
            from: args.from,
            to: args.to,
            rise: args.rise,
            fall: args.fall,
            setup: args.setup,
            hold: args.hold,
            positionals: args.positionals,
        }
    }
}

#[derive(TclCommand)]
#[command(name = "set_clock_groups", handler = set_clock_groups, sdc)]
pub(crate) struct SetClockGroupsArgs<'a> {
    #[arg(long = "-name")]
    name: Option<String>,
    #[arg(
        long = "-logically_exclusive",
        conflicts_with = "physically_exclusive",
        conflicts_with = "asynchronous"
    )]
    logically_exclusive: bool,
    #[arg(long = "-physically_exclusive", conflicts_with = "asynchronous")]
    physically_exclusive: bool,
    #[arg(long = "-asynchronous")]
    asynchronous: bool,
    #[arg(long = "-comment")]
    comment: Option<String>,
    #[arg(long = "-group", value_hint = ValueHint::Clock)]
    groups: Vec<TclArg<'a>>,
    #[arg(long = "-allow_paths", unsupported)]
    _allow_paths: (),
}

#[derive(TclCommand)]
#[command(name = "unset_clock_groups", handler = unset_clock_groups, sdc)]
pub(crate) struct UnsetClockGroupsArgs<'a> {
    #[arg(
        long = "-logically_exclusive",
        conflicts_with = "physically_exclusive",
        conflicts_with = "asynchronous"
    )]
    logically_exclusive: bool,
    #[arg(long = "-physically_exclusive", conflicts_with = "asynchronous")]
    physically_exclusive: bool,
    #[arg(long = "-asynchronous")]
    asynchronous: bool,
    #[arg(long = "-name", conflicts_with = "all")]
    name: Option<TclArg<'a>>,
    #[arg(long = "-all")]
    all: bool,
}

#[derive(TclCommand)]
#[command(name = "set_case_analysis", handler = set_case_analysis, sdc)]
pub(crate) struct SetCaseAnalysisArgs<'a> {
    #[arg(
        positional,
        value_hint = ValueHint::OneOf {
            accepted: &["0", "zero", "1", "one", "rise", "rising", "fall", "falling"],
            suggested: &["0", "1", "rise", "fall"],
        }
    )]
    value: String,
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(name = "unset_case_analysis", handler = unset_case_analysis, sdc)]
pub(crate) struct UnsetCaseAnalysisArgs<'a> {
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_logic_zero",
    alias = "set_logic_one",
    alias = "set_logic_dc",
    handler = set_logic,
    sdc
)]
pub(crate) struct SetLogicArgs<'a> {
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_disable_timing",
    alias = "unset_disable_timing",
    handler = disable_timing,
    sdc
)]
pub(crate) struct DisableTimingArgs<'a> {
    #[arg(long = "-from")]
    from: Vec<String>,
    #[arg(long = "-to")]
    to: Vec<String>,
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(name = "set_timing_derate", handler = set_timing_derate, sdc)]
pub(crate) struct SetTimingDerateArgs {
    #[arg(long = "-early")]
    early: bool,
    #[arg(long = "-late")]
    late: bool,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-clock")]
    clock: bool,
    #[arg(long = "-data")]
    data: bool,
    #[arg(long = "-net_delay")]
    net_delay: bool,
    #[arg(long = "-cell_delay")]
    cell_delay: bool,
    #[arg(long = "-cell_check")]
    cell_check: bool,
    #[arg(positional)]
    derate: f64,
}

#[derive(TclCommand)]
#[command(name = "unset_timing_derate", handler = unset_timing_derate, sdc)]
pub(crate) struct UnsetTimingDerateArgs {}

#[derive(TclCommand)]
#[command(
    name = "set_propagated_clock",
    alias = "unset_propagated_clock",
    handler = propagated_clock,
    sdc
)]
pub(crate) struct PropagatedClockArgs<'a> {
    #[arg(positional, min = 1, value_hint = ValueHint::Clock)]
    clocks: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "set_resistance", handler = set_resistance, sdc)]
pub(crate) struct SetResistanceArgs<'a> {
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(positional)]
    resistance: f64,
    #[arg(positional, value_hint = ValueHint::Net)]
    nets: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_input_delay",
    alias = "set_output_delay",
    handler = set_io_delay,
    sdc
)]
pub(crate) struct SetIoDelayArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(long = "-clock", value_hint = ValueHint::Clock)]
    clock: Option<TclArg<'a>>,
    #[arg(long = "-clock_fall")]
    clock_fall: bool,
    #[arg(long = "-source_latency_included")]
    source_latency_included: bool,
    #[arg(long = "-network_latency_included")]
    network_latency_included: bool,
    #[arg(long = "-add_delay")]
    add_delay: bool,
    #[arg(long = "-reference_pin", unsupported, value_hint = ValueHint::Pin)]
    _reference_pin: (),
    #[arg(positional)]
    delay: f64,
    #[arg(positional, value_hint = ValueHint::Port)]
    ports: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "unset_input_delay",
    alias = "unset_output_delay",
    handler = unset_io_delay,
    sdc
)]
pub(crate) struct UnsetIoDelayArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(long = "-clock", value_hint = ValueHint::Clock)]
    clock: Option<TclArg<'a>>,
    #[arg(long = "-clock_fall")]
    clock_fall: bool,
    #[arg(positional, value_hint = ValueHint::Port)]
    ports: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_max_transition",
    alias = "set_max_capacitance",
    handler = set_scoped_design_rule,
    sdc
)]
pub(crate) struct ScopedDesignRuleArgs<'a> {
    #[arg(long = "-data_path")]
    data_path: bool,
    #[arg(long = "-clock_path")]
    clock_path: bool,
    #[arg(positional, before_options)]
    limit: f64,
    #[arg(positional, min = 1)]
    objects: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "set_max_fanout", handler = set_max_fanout, sdc)]
pub(crate) struct SetMaxFanoutArgs<'a> {
    #[arg(positional, before_options)]
    limit: f64,
    #[arg(positional, min = 1)]
    objects: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "set_false_path", handler = set_false_path, sdc)]
pub(crate) struct SetFalsePathArgs<'a> {
    #[arg(long = "-setup")]
    setup: bool,
    #[arg(long = "-hold")]
    hold: bool,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-reset_path")]
    reset_path: bool,
    #[arg(long = "-comment")]
    comment: Option<String>,
    #[arg(path_points)]
    points: Vec<TclOption<'a>>,
}

#[derive(TclCommand)]
#[command(name = "unset_path_exceptions", handler = unset_path_exceptions, sdc)]
pub(crate) struct UnsetPathExceptionsArgs<'a> {
    #[arg(long = "-setup")]
    setup: bool,
    #[arg(long = "-hold")]
    hold: bool,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(path_points)]
    points: Vec<TclOption<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "set_max_delay",
    alias = "set_min_delay",
    handler = set_path_delay,
    sdc
)]
pub(crate) struct SetPathDelayArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-reset_path")]
    reset_path: bool,
    #[arg(long = "-ignore_clock_latency")]
    ignore_clock_latency: bool,
    #[arg(long = "-comment")]
    comment: Option<String>,
    #[arg(long = "-group_path", unsupported, value_hint = ValueHint::Text)]
    _group_path: (),
    #[arg(path_points)]
    points: Vec<TclOption<'a>>,
    #[arg(positional, before_options)]
    delay: f64,
}

#[derive(TclCommand)]
#[command(name = "set_multicycle_path", handler = set_multicycle_path, sdc)]
pub(crate) struct SetMulticyclePathArgs<'a> {
    #[arg(long = "-setup")]
    setup: bool,
    #[arg(long = "-hold")]
    hold: bool,
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-start", conflicts_with = "end")]
    start: bool,
    #[arg(long = "-end")]
    end: bool,
    #[arg(long = "-reset_path")]
    reset_path: bool,
    #[arg(long = "-comment")]
    comment: Option<String>,
    #[arg(path_points)]
    points: Vec<TclOption<'a>>,
    #[arg(positional, before_options)]
    cycles: u32,
}

#[derive(TclCommand)]
#[command(name = "report_clock", handler = report_clock)]
pub(crate) struct ReportClockArgs {}

#[derive(TclCommand)]
#[command(name = "check_timing", handler = check_timing)]
pub(crate) struct CheckTimingArgs {}

fn delete_clock_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: DeleteClockArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let clocks = resolve_clock_list(state, interp, command, &args.clocks)?;
    state
        .session
        .borrow_mut()
        .delete_clocks(&clocks, command == "delete_generated_clock")
        .map_err(crate::ShellError::from)
}

fn resolve_single_port(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<opto_session::PortId, crate::ShellError> {
    let ports = resolve_port_list(state, interp, command, value)?;
    match ports.as_slice() {
        [port] => Ok(*port),
        _ => Err(crate::ShellError::command(format!(
            "{command}: expected exactly one port"
        ))),
    }
}

fn parse_generated_u32_triple(
    interp: *mut TclInterp,
    value: &TclArg<'_>,
    option: &str,
) -> Result<[u32; 3], crate::ShellError> {
    let values = split_tcl_list(interp, value)?;
    if values.len() != 3 {
        return Err(crate::ShellError::command(format!(
            "create_generated_clock: {option} requires three values"
        )));
    }
    values
        .iter()
        .map(|value| {
            value.parse::<u32>().map_err(|_| {
                crate::ShellError::parse(format!("create_generated_clock: invalid edge '{value}'"))
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| [values[0], values[1], values[2]])
}

fn parse_generated_f64_triple(
    interp: *mut TclInterp,
    value: &TclArg<'_>,
    option: &str,
) -> Result<[f64; 3], crate::ShellError> {
    let values = split_tcl_list(interp, value)?;
    if values.len() != 3 {
        return Err(crate::ShellError::command(format!(
            "create_generated_clock: {option} requires three values"
        )));
    }
    values
        .iter()
        .map(|value| {
            value.parse::<f64>().map_err(|_| {
                crate::ShellError::parse(format!(
                    "create_generated_clock: invalid edge shift '{value}'"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|values| [values[0], values[1], values[2]])
}

fn set_propagated_clock_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: PropagatedClockArgs<'_>,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut clocks = Vec::new();
    for arg in &args.clocks {
        clocks.extend(resolve_clock_list(state, interp, command, arg)?);
    }
    state
        .session
        .borrow_mut()
        .set_propagated_clock(command == "set_propagated_clock", &clocks)
        .map_err(crate::ShellError::from)
}

fn clock_uncertainty_selector<'a>(
    command: &str,
    role: &str,
    mut options: Vec<TclOption<'a>>,
) -> Result<(Option<TclArg<'a>>, EdgeSelection), crate::ShellError> {
    if options.len() > 1 {
        return Err(crate::ShellError::command(format!(
            "{command}: duplicate {role} clock selector"
        )));
    }
    let Some(option) = options.pop() else {
        return Ok((None, EdgeSelection::Both));
    };
    let edge = match option.name() {
        "-rise_from" | "-rise_to" => EdgeSelection::Rise,
        "-fall_from" | "-fall_to" => EdgeSelection::Fall,
        "-from" | "-to" => EdgeSelection::Both,
        _ => unreachable!("clock uncertainty schema uses fixed selectors"),
    };
    Ok((Some(option.value()), edge))
}

#[derive(Clone, Copy)]
enum PathPointRole {
    From,
    Through,
    To,
}

#[derive(Clone, Copy)]
enum PathExceptionCommand {
    FalsePath,
    MaxDelay(f64),
    MinDelay(f64),
    MultiCycle(u32),
}

struct PathExceptionArgs<'a> {
    kind: PathExceptionCommand,
    points: Vec<TclOption<'a>>,
    setup: bool,
    hold: bool,
    rise: bool,
    fall: bool,
    start: bool,
    end: bool,
    reset_path: bool,
    ignore_clock_latency: bool,
    comment: String,
}

fn edge_selection(rise: bool, fall: bool) -> EdgeSelection {
    match (rise, fall) {
        (true, false) => EdgeSelection::Rise,
        (false, true) => EdgeSelection::Fall,
        (false, false) | (true, true) => EdgeSelection::Both,
    }
}

fn report_timing_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: ReportTimingArgs<'_>,
) -> Result<String, crate::ShellError> {
    let mut options = ReportTimingOptions::default();
    for raw in &args.from {
        options
            .from
            .extend(resolve_object_names(state, interp, "report_timing", raw)?);
    }
    for raw in &args.to {
        options
            .to
            .extend(resolve_object_names(state, interp, "report_timing", raw)?);
    }
    for delay in args.delay {
        options.delay_type = match delay.value().as_str() {
            "max" => DelayType::Max,
            "min" => DelayType::Min,
            "min_max" => {
                return Err(crate::ShellError::command(format!(
                    "report_timing: {} min_max is not implemented yet",
                    delay.name()
                )));
            }
            _ => unreachable!("derive schema validates report_timing delay type"),
        };
    }
    for max_paths in args.max_paths {
        if max_paths != 1 {
            return Err(crate::ShellError::command(format!(
                "report_timing: -max_paths {max_paths} is not implemented yet"
            )));
        }
    }
    for digits in args.significant_digits {
        if digits > 13 {
            return Err(crate::ShellError::command(format!(
                "report_timing: significant digits value '{digits}' is outside 0..13"
            )));
        }
        options.significant_digits = digits;
    }
    for path in args.path {
        if path != "full" {
            return Err(crate::ShellError::command(format!(
                "report_timing: -path {path} is not implemented yet"
            )));
        }
    }

    state
        .session
        .borrow()
        .report_timing(&options)
        .map_err(crate::ShellError::from)
}

fn read_parasitics_command(
    state: &ShellState,
    args: ReadParasiticsArgs,
) -> Result<String, crate::ShellError> {
    let mut options = opto_session::ReadParasiticsOptions::default();
    if args.elmore {
        options.delay_model = opto_session::ParasiticDelayModel::Elmore;
    } else if args.arnoldi {
        options.delay_model = opto_session::ParasiticDelayModel::Arnoldi;
    }
    options.increment = args.increment;
    options.pin_capacitance_included = args.pin_cap_included;
    options.net_capacitance_only = args.net_cap_only;
    options.syntax_only = args.syntax_only;
    options.verbose = args.verbose;
    if let Some(completion) = args.complete_with {
        options.completion = match completion.as_str() {
            "none" => None,
            "zero" => Some(opto_session::ReadParasiticsCompletion::Zero),
            "wlm" => Some(opto_session::ReadParasiticsCompletion::WireLoad),
            _ => unreachable!("schema validates read_parasitics completion"),
        };
    }
    options.path = args.path;
    options.strip_path = args.strip_path;
    state
        .session
        .borrow_mut()
        .read_parasitics(&args.files, &options)
        .map_err(crate::ShellError::from)
}

pub(super) fn resolve_object_names(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    value: &TclArg<'_>,
) -> Result<Vec<String>, crate::ShellError> {
    if let Some(names) = state
        .session
        .borrow()
        .collection_object_names_if_handle(value.as_str())?
    {
        if names.is_empty() {
            return Err(crate::ShellError::command(format!(
                "{command}: object collection '{value}' is empty"
            )));
        }
        return Ok(names);
    }
    let names = split_tcl_list(interp, value)?;
    if names.is_empty() {
        return Err(crate::ShellError::command(format!(
            "{command}: empty object list"
        )));
    }
    Ok(names)
}

macro_rules! timing_handler {
    ($name:ident, $arguments:ty, |$state:ident, $interp:ident, $command:ident, $args:ident| $body:block) => {
        pub(crate) fn $name(
            state: &ShellState,
            interp: *mut TclInterp,
            command: &'static str,
            arguments: $arguments,
        ) -> Result<CommandResult, crate::ShellError> {
            let ($state, $interp, $command, $args) = (state, interp, command, arguments);
            $body
        }
    };
}
timing_handler!(read_sdc, ReadSdcArgs, |state, interp, command, args| {
    let _ = command;
    sdc::read_sdc(state, interp, args)
});

timing_handler!(write_sdc, WriteSdcArgs, |state, _interp, _command, args| {
    state
        .session
        .borrow()
        .write_sdc(&args.file)
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
});

timing_handler!(
    read_parasitics,
    ReadParasiticsArgs,
    |state, interp, command, args| {
        let _ = (interp, command);
        read_parasitics_command(state, args).map(CommandResult::Complete)
    }
);

timing_handler!(
    create_clock,
    CreateClockArgs<'_>,
    |state, interp, command, args| {
        let _ = (interp, command);
        create_clock_command(state, args).map(CommandResult::Complete)
    }
);

timing_handler!(
    create_generated_clock,
    CreateGeneratedClockArgs<'_>,
    |state, interp, _command, args| {
        create_generated_clock_command(state, interp, args).map(CommandResult::Complete)
    }
);

timing_handler!(
    delete_clock,
    DeleteClockArgs<'_>,
    |state, interp, command, args| {
        delete_clock_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_port_constraint,
    PortConstraintCommandArgs<'_>,
    |state, interp, command, args| {
        set_port_constraint_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_clock_transition,
    SetClockTransitionArgs<'_>,
    |state, interp, command, args| {
        let _ = command;
        set_clock_transition_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    unset_clock_transition,
    UnsetClockTransitionArgs<'_>,
    |state, interp, _command, args| {
        unset_clock_transition_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_clock_latency,
    SetClockLatencyArgs<'_>,
    |state, interp, _command, args| {
        set_clock_latency_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    unset_clock_latency,
    UnsetClockLatencyArgs<'_>,
    |state, interp, _command, args| {
        unset_clock_latency_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_clock_uncertainty,
    SetClockUncertaintyArgs<'_>,
    |state, interp, command, args| {
        clock_uncertainty_command(state, interp, command, true, args.into())
            .map(constraint_change_result)
    }
);

timing_handler!(
    unset_clock_uncertainty,
    UnsetClockUncertaintyArgs<'_>,
    |state, interp, command, args| {
        clock_uncertainty_command(state, interp, command, false, args.into())
            .map(constraint_change_result)
    }
);

timing_handler!(
    set_clock_groups,
    SetClockGroupsArgs<'_>,
    |state, interp, _command, args| {
        set_clock_groups_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    unset_clock_groups,
    UnsetClockGroupsArgs<'_>,
    |state, interp, _command, args| {
        unset_clock_groups_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_case_analysis,
    SetCaseAnalysisArgs<'_>,
    |state, interp, command, args| {
        case_analysis_command(state, interp, command, Some(args.value), args.objects)
            .map(constraint_change_result)
    }
);

timing_handler!(
    unset_case_analysis,
    UnsetCaseAnalysisArgs<'_>,
    |state, interp, command, args| {
        case_analysis_command(state, interp, command, None, args.objects)
            .map(constraint_change_result)
    }
);

timing_handler!(
    set_logic,
    SetLogicArgs<'_>,
    |state, interp, command, args| {
        set_logic_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    disable_timing,
    DisableTimingArgs<'_>,
    |state, interp, command, args| {
        disable_timing_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_timing_derate,
    SetTimingDerateArgs,
    |state, _interp, _command, args| {
        set_timing_derate_command(state, args).map(constraint_change_result)
    }
);

timing_handler!(
    unset_timing_derate,
    UnsetTimingDerateArgs,
    |state, _interp, _command, _args| {
        state
            .session
            .borrow_mut()
            .unset_timing_derate()
            .map(constraint_change_result)
            .map_err(crate::ShellError::from)
    }
);

timing_handler!(
    propagated_clock,
    PropagatedClockArgs<'_>,
    |state, interp, command, args| {
        set_propagated_clock_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_resistance,
    SetResistanceArgs<'_>,
    |state, interp, _command, args| {
        set_resistance_command(state, interp, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_io_delay,
    SetIoDelayArgs<'_>,
    |state, interp, command, args| {
        set_io_delay_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    unset_io_delay,
    UnsetIoDelayArgs<'_>,
    |state, interp, command, args| {
        unset_io_delay_command(state, interp, command, args).map(constraint_change_result)
    }
);

timing_handler!(
    set_scoped_design_rule,
    ScopedDesignRuleArgs<'_>,
    |state, interp, command, args| {
        let scope = match (args.data_path, args.clock_path) {
            (false, false) => DesignRuleScope::All,
            (true, false) => DesignRuleScope::DataPath,
            (false, true) => DesignRuleScope::ClockPath,
            (true, true) => DesignRuleScope::ClockAndData,
        };
        set_design_rule_command(state, interp, command, args.limit, &args.objects, scope)
            .map(constraint_change_result)
    }
);

timing_handler!(
    set_max_fanout,
    SetMaxFanoutArgs<'_>,
    |state, interp, command, args| {
        set_design_rule_command(
            state,
            interp,
            command,
            args.limit,
            &args.objects,
            DesignRuleScope::All,
        )
        .map(constraint_change_result)
    }
);

timing_handler!(
    set_path_delay,
    SetPathDelayArgs<'_>,
    |state, interp, command, args| {
        let kind = match command {
            "set_max_delay" => PathExceptionCommand::MaxDelay(args.delay),
            "set_min_delay" => PathExceptionCommand::MinDelay(args.delay),
            _ => unreachable!("path-delay parser is bound to fixed commands"),
        };
        set_path_exception_command(
            state,
            interp,
            command,
            PathExceptionArgs {
                kind,
                points: args.points,
                setup: false,
                hold: false,
                rise: args.rise,
                fall: args.fall,
                start: false,
                end: false,
                reset_path: args.reset_path,
                ignore_clock_latency: args.ignore_clock_latency,
                comment: args.comment.unwrap_or_default(),
            },
        )
        .map(constraint_change_result)
    }
);

timing_handler!(
    set_false_path,
    SetFalsePathArgs<'_>,
    |state, interp, command, args| {
        set_path_exception_command(
            state,
            interp,
            command,
            PathExceptionArgs {
                kind: PathExceptionCommand::FalsePath,
                points: args.points,
                setup: args.setup,
                hold: args.hold,
                rise: args.rise,
                fall: args.fall,
                start: false,
                end: false,
                reset_path: args.reset_path,
                ignore_clock_latency: false,
                comment: args.comment.unwrap_or_default(),
            },
        )
        .map(constraint_change_result)
    }
);

timing_handler!(
    unset_path_exceptions,
    UnsetPathExceptionsArgs<'_>,
    |state, interp, command, args| {
        set_path_exception_command(
            state,
            interp,
            command,
            PathExceptionArgs {
                kind: PathExceptionCommand::FalsePath,
                points: args.points,
                setup: args.setup,
                hold: args.hold,
                rise: args.rise,
                fall: args.fall,
                start: false,
                end: false,
                reset_path: false,
                ignore_clock_latency: false,
                comment: String::new(),
            },
        )
        .map(constraint_change_result)
    }
);

timing_handler!(
    set_multicycle_path,
    SetMulticyclePathArgs<'_>,
    |state, interp, command, args| {
        set_path_exception_command(
            state,
            interp,
            command,
            PathExceptionArgs {
                kind: PathExceptionCommand::MultiCycle(args.cycles),
                points: args.points,
                setup: args.setup,
                hold: args.hold,
                rise: args.rise,
                fall: args.fall,
                start: args.start,
                end: args.end,
                reset_path: args.reset_path,
                ignore_clock_latency: false,
                comment: args.comment.unwrap_or_default(),
            },
        )
        .map(constraint_change_result)
    }
);

timing_handler!(
    report_clock,
    ReportClockArgs,
    |state, _interp, _command, _args| {
        Ok(CommandResult::Complete(
            state.session.borrow().report_clock(),
        ))
    }
);

timing_handler!(
    check_timing,
    CheckTimingArgs,
    |state, _interp, _command, _args| {
        state
            .session
            .borrow()
            .check_timing()
            .map(CommandResult::Complete)
            .map_err(crate::ShellError::from)
    }
);

timing_handler!(
    report_timing,
    ReportTimingArgs<'_>,
    |state, interp, command, args| {
        let _ = command;
        report_timing_command(state, interp, args).map(CommandResult::Complete)
    }
);
