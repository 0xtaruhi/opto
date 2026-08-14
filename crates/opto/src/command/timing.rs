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
#[command(
    name = "read_sdc",
    handler = read_sdc,
    summary = "Read and atomically apply an SDC constraint script.",
    requires = "The referenced SDC file must exist and be readable.",
    example = "read_sdc -version 2.2 constraints/top.sdc"
)]
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
#[command(
    name = "read_parasitics",
    handler = read_parasitics,
    summary = "Read and validate parasitic data for the current design.",
    requires = "The current design and active timing libraries must provide compatible units.",
    example = "read_parasitics -elmore -complete_with none parasitics/top.spef"
)]
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
#[command(
    name = "report_timing",
    handler = report_timing,
    summary = "Analyze and report selected timing paths for the current design.",
    requires = "A current design and linked timing library are required.",
    example = "report_timing -delay_type max -max_paths 10"
)]
pub(crate) struct ReportTimingArgs<'a> {
    #[arg(long = "-from", repeatable, value_hint = ValueHint::Port)]
    from: Vec<TclArg<'a>>,
    #[arg(long = "-to", repeatable, value_hint = ValueHint::Port)]
    to: Vec<TclArg<'a>>,
    #[arg(
        long = "-delay_type",
        value_hint = ValueHint::OneOf {
            accepted: &["max", "min", "min_max"],
            suggested: &["max", "min"],
        }
    )]
    delay: Option<String>,
    #[arg(long = "-max_paths", value_hint = ValueHint::Suggested(&["1", "10"]))]
    max_paths: Option<usize>,
    #[arg(long = "-significant_digits")]
    significant_digits: Option<usize>,
    #[arg(long = "-path", value_hint = ValueHint::Suggested(&["full"]))]
    path: Option<String>,
}

#[derive(TclCommand)]
#[command(
    name = "create_clock",
    handler = create_clock,
    sdc,
    option_or_positional = "-name",
    summary = "Create or update a primary clock constraint.",
    requires = "Referenced source objects must resolve in the current session state.",
    example = "create_clock -period 10 -name sys_clk"
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
#[command(
    name = "write_sdc",
    handler = write_sdc,
    sdc,
    summary = "Write the current constraint state as deterministic SDC.",
    requires = "A current design is required and the destination must be writable.",
    example = "write_sdc build/top.sdc"
)]
pub(crate) struct WriteSdcArgs {
    #[arg(positional, value_hint = ValueHint::File)]
    file: PathBuf,
}

#[derive(TclCommand)]
#[command(
    name = "create_generated_clock",
    handler = create_generated_clock,
    sdc,
    summary = "Create a generated clock derived from a source or master clock.",
    requires = "Source, target, and optional master clock objects must resolve uniquely.",
    example = "create_generated_clock -name clk_div2 -source [get_ports clk] -divide_by 2 [get_pins U_DIV/Q]"
)]
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
    kind = DeleteClockKind::Any,
    variant = "delete_generated_clock",
    variant_kind = DeleteClockKind::Generated,
    handler = delete_clock,
    sdc,
    summary = "Delete selected clocks, optionally restricting deletion to generated clocks.",
    requires = "Every selected clock handle must be live in the current constraint state.",
    example = "delete_clock [get_clocks obsolete_clk]",
    variant_example = "delete_generated_clock [get_clocks clk_div2]"
)]
pub(crate) struct DeleteClockArgs<'a> {
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_input_transition",
    kind = PortConstraintKind::InputTransition,
    variant = "set_load",
    variant_kind = PortConstraintKind::Load,
    variant = "set_drive",
    variant_kind = PortConstraintKind::Drive,
    handler = set_port_constraint,
    sdc,
    summary = "Set a numeric transition, load, or drive constraint on selected ports.",
    variant_summary = "Set an external capacitive load on selected ports.",
    variant_summary = "Set external source resistance on selected input ports.",
    requires = "Selected ports must be live and the value must satisfy the selected constraint kind.",
    example = "set_input_transition -rise -max 0.08 [get_ports data_in]",
    variant_example = "set_load -max 0.05 [get_ports data_out]",
    variant_example = "set_drive -rise 0.20 [get_ports data_in]"
)]
pub(crate) struct PortConstraintCommandArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(
        long = "-min",
        help = "Select the minimum constraint slot for the chosen transition, load, or drive kind."
    )]
    min: bool,
    #[arg(
        long = "-max",
        help = "Select the maximum constraint slot for the chosen transition, load, or drive kind."
    )]
    max: bool,
    #[arg(positional)]
    value: f64,
    #[arg(positional, min = 1, value_hint = ValueHint::Port)]
    objects: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "set_clock_transition",
    handler = set_clock_transition,
    sdc,
    summary = "Set rise or fall transition values on selected clocks.",
    requires = "The transition must be finite and nonnegative in the active timing-library time unit, and clocks must resolve.",
    example = "set_clock_transition 0.10 [get_clocks sys_clk]"
)]
pub(crate) struct SetClockTransitionArgs<'a> {
    #[arg(long = "-rise")]
    rise: bool,
    #[arg(long = "-fall")]
    fall: bool,
    #[arg(long = "-min")]
    min: bool,
    #[arg(long = "-max")]
    max: bool,
    #[arg(
        positional,
        help = "A finite nonnegative transition in the active timing-library time unit."
    )]
    transition: f64,
    #[arg(
        positional,
        min = 1,
        value_hint = ValueHint::Clock,
        help = "One or more live clock names or collection handles."
    )]
    clocks: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "unset_clock_transition",
    handler = unset_clock_transition,
    sdc,
    summary = "Remove explicit transition values from selected clocks.",
    requires = "Selected clocks must be live.",
    example = "unset_clock_transition [get_clocks sys_clk]"
)]
pub(crate) struct UnsetClockTransitionArgs<'a> {
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_clock_latency",
    handler = set_clock_latency,
    sdc,
    summary = "Set source or network latency values on selected clocks.",
    requires = "Latency must be finite and edge, corner, and side selections must be coherent.",
    example = "set_clock_latency -source -early 0.15 [get_clocks sys_clk]"
)]
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
#[command(
    name = "unset_clock_latency",
    handler = unset_clock_latency,
    sdc,
    summary = "Remove source or network latency values from selected clocks.",
    requires = "Selected clocks must be live.",
    example = "unset_clock_latency -source [get_clocks sys_clk]"
)]
pub(crate) struct UnsetClockLatencyArgs<'a> {
    #[arg(long = "-source")]
    source: bool,
    #[arg(long = "-clock", unsupported, value_hint = ValueHint::Clock)]
    _clock: (),
    #[arg(positional, value_hint = ValueHint::Clock)]
    clocks: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_clock_uncertainty",
    handler = set_clock_uncertainty,
    sdc,
    summary = "Set intra-clock or inter-clock setup and hold uncertainty.",
    requires = "Clock selectors must form a valid intra-clock or paired from/to selection.",
    example = "set_clock_uncertainty -setup 0.10 [get_clocks sys_clk]"
)]
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
#[command(
    name = "unset_clock_uncertainty",
    handler = unset_clock_uncertainty,
    sdc,
    summary = "Remove selected setup or hold clock uncertainty constraints.",
    requires = "Clock selectors must form a valid intra-clock or paired from/to selection.",
    example = "unset_clock_uncertainty -setup [get_clocks sys_clk]"
)]
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
#[command(
    name = "set_clock_groups",
    handler = set_clock_groups,
    sdc,
    summary = "Declare logically exclusive, physically exclusive, or asynchronous clock groups.",
    requires = "At least two nonempty clock groups and exactly one relationship kind are required.",
    example = "set_clock_groups -asynchronous -group [get_clocks sys_clk] -group [get_clocks aux_clk]"
)]
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
    #[arg(long = "-group", repeatable, value_hint = ValueHint::Clock)]
    groups: Vec<TclArg<'a>>,
    #[arg(long = "-allow_paths", unsupported)]
    _allow_paths: (),
}

#[derive(TclCommand)]
#[command(
    name = "unset_clock_groups",
    handler = unset_clock_groups,
    sdc,
    summary = "Remove clock-group constraints selected by name, kind, or all.",
    requires = "The selection must identify existing clock-group constraints.",
    example = "unset_clock_groups -name async_domains"
)]
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
#[command(
    name = "set_case_analysis",
    handler = set_case_analysis,
    sdc,
    summary = "Set a constant or transition case-analysis value on selected objects.",
    requires = "Objects must resolve to supported case-analysis endpoints.",
    example = "set_case_analysis 0 [get_ports test_mode]"
)]
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
#[command(
    name = "unset_case_analysis",
    handler = unset_case_analysis,
    sdc,
    summary = "Remove case-analysis constraints from selected objects.",
    requires = "Objects must resolve to supported case-analysis endpoints.",
    example = "unset_case_analysis [get_ports test_mode]"
)]
pub(crate) struct UnsetCaseAnalysisArgs<'a> {
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_logic_zero",
    kind = LogicKind::Zero,
    variant = "set_logic_one",
    variant_kind = LogicKind::One,
    variant = "set_logic_dc",
    variant_kind = LogicKind::DontCare,
    handler = set_logic,
    sdc,
    summary = "Apply a constant-zero, constant-one, or don't-care logic constraint.",
    requires = "Selected objects must resolve to supported logic endpoints.",
    example = "set_logic_zero [get_ports scan_enable]",
    variant_example = "set_logic_one [get_ports scan_enable]",
    variant_example = "set_logic_dc [get_ports test_data]"
)]
pub(crate) struct SetLogicArgs<'a> {
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_disable_timing",
    kind = MutationKind::Set,
    variant = "unset_disable_timing",
    variant_kind = MutationKind::Unset,
    handler = disable_timing,
    sdc,
    summary = "Set or remove disabled timing arcs on selected objects.",
    requires = "Objects and optional arc endpoints must resolve in the current design.",
    example = "set_disable_timing -from A -to Y [get_cells U_TEST]",
    variant_example = "unset_disable_timing -from A -to Y [get_cells U_TEST]"
)]
pub(crate) struct DisableTimingArgs<'a> {
    #[arg(long = "-from")]
    from: Option<String>,
    #[arg(long = "-to")]
    to: Option<String>,
    #[arg(positional)]
    objects: TclArg<'a>,
}

#[derive(TclCommand)]
#[command(
    name = "set_timing_derate",
    handler = set_timing_derate,
    sdc,
    summary = "Set global early or late timing derates by edge, path scope, and delay kind.",
    requires = "The derate must be finite and positive and selectors must identify supported slots.",
    example = "set_timing_derate -late -data -cell_delay 1.05"
)]
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
#[command(
    name = "unset_timing_derate",
    handler = unset_timing_derate,
    sdc,
    summary = "Remove all explicit global timing derates.",
    requires = "A current constraint state is required."
)]
pub(crate) struct UnsetTimingDerateArgs {}

#[derive(TclCommand)]
#[command(
    name = "set_propagated_clock",
    kind = MutationKind::Set,
    variant = "unset_propagated_clock",
    variant_kind = MutationKind::Unset,
    handler = propagated_clock,
    sdc,
    summary = "Set or remove propagated-clock state on selected clocks.",
    requires = "Selected clocks must be live.",
    example = "set_propagated_clock [get_clocks sys_clk]",
    variant_example = "unset_propagated_clock [get_clocks sys_clk]"
)]
pub(crate) struct PropagatedClockArgs<'a> {
    #[arg(positional, min = 1, value_hint = ValueHint::Clock)]
    clocks: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "set_resistance",
    handler = set_resistance,
    sdc,
    summary = "Set finite nonnegative resistance on selected logical nets.",
    requires = "Selected objects must be nets and resistance must be finite and nonnegative.",
    example = "set_resistance -max 0.25 [get_nets bus_*]"
)]
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
    kind = opto_session::IoDelayKind::Input,
    variant = "set_output_delay",
    variant_kind = opto_session::IoDelayKind::Output,
    handler = set_io_delay,
    sdc,
    summary = "Set signed input or output delay slots on selected ports.",
    variant_summary = "Set signed output delay slots on selected ports.",
    requires = "Delay must be finite; ports and the optional reference clock must resolve.",
    example = "set_input_delay -clock [get_clocks sys_clk] -max 1.20 [get_ports data_in]",
    variant_example = "set_output_delay -clock [get_clocks sys_clk] -max 0.80 [get_ports data_out]"
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
    kind = opto_session::IoDelayKind::Input,
    variant = "unset_output_delay",
    variant_kind = opto_session::IoDelayKind::Output,
    handler = unset_io_delay,
    sdc,
    summary = "Remove selected input or output delay slots from ports.",
    variant_summary = "Remove selected output delay slots from ports.",
    requires = "Ports and the optional reference clock must resolve.",
    example = "unset_input_delay -clock [get_clocks sys_clk] -max [get_ports data_in]",
    variant_example = "unset_output_delay -clock [get_clocks sys_clk] -max [get_ports data_out]"
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
    kind = DesignRuleKind::Transition,
    variant = "set_max_capacitance",
    variant_kind = DesignRuleKind::Capacitance,
    handler = set_scoped_design_rule,
    sdc,
    summary = "Set a maximum transition or capacitance design rule on selected objects.",
    variant_summary = "Set a maximum capacitance design rule on selected objects.",
    requires = "The limit must be finite and nonnegative and selected objects must support the rule.",
    example = "set_max_transition 0.20 -data_path [get_ports data_out]",
    variant_example = "set_max_capacitance 0.10 -data_path [get_ports data_out]"
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
#[command(
    name = "set_max_fanout",
    handler = set_max_fanout,
    sdc,
    summary = "Set a maximum fanout design rule on selected objects.",
    requires = "The limit must be finite and nonnegative and selected objects must support fanout limits.",
    example = "set_max_fanout 16 [get_ports data_in]"
)]
pub(crate) struct SetMaxFanoutArgs<'a> {
    #[arg(positional, before_options)]
    limit: f64,
    #[arg(positional, min = 1)]
    objects: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "set_false_path",
    handler = set_false_path,
    sdc,
    summary = "Exclude selected timing paths from setup or hold analysis.",
    requires = "Path-point selectors must resolve to supported timing endpoints.",
    example = "set_false_path -from [get_ports scan_in] -to [get_ports data_out]"
)]
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
#[command(
    name = "unset_path_exceptions",
    handler = unset_path_exceptions,
    sdc,
    summary = "Remove matching false, multicycle, maximum-delay, and minimum-delay exceptions.",
    requires = "Path-point selectors must resolve to supported timing endpoints.",
    example = "unset_path_exceptions -from [get_ports scan_in] -to [get_ports data_out]"
)]
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
    kind = PathDelayKind::Max,
    variant = "set_min_delay",
    variant_kind = PathDelayKind::Min,
    handler = set_path_delay,
    sdc,
    summary = "Set a maximum or minimum delay exception on selected paths.",
    variant_summary = "Set a minimum delay exception on selected paths.",
    requires = "Delay must be finite and path-point selectors must resolve.",
    example = "set_max_delay 2.50 -from [get_ports data_in] -to [get_ports data_out]",
    variant_example = "set_min_delay 0.20 -from [get_ports data_in] -to [get_ports data_out]"
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
#[command(
    name = "set_multicycle_path",
    handler = set_multicycle_path,
    sdc,
    summary = "Set a deterministic setup or hold multicycle path exception.",
    requires = "Cycle count must be positive and path-point selectors must resolve.",
    example = "set_multicycle_path 2 -setup -from [get_ports data_in] -to [get_ports data_out]"
)]
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
#[command(
    name = "report_clock",
    handler = report_clock,
    summary = "Report clocks and their current generated, latency, transition, and uncertainty state.",
    requires = "A current design and constraint state are required."
)]
pub(crate) struct ReportClockArgs {}

#[derive(TclCommand)]
#[command(
    name = "check_timing",
    handler = check_timing,
    summary = "Validate timing constraints and analysis readiness without changing state.",
    requires = "A current design and linked timing libraries are required."
)]
pub(crate) struct CheckTimingArgs {}

fn delete_clock_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: DeleteClockArgs<'_>,
    kind: DeleteClockKind,
) -> Result<ConstraintChange, crate::ShellError> {
    let clocks = resolve_clock_list(state, interp, command, &args.clocks)?;
    state
        .session
        .borrow_mut()
        .delete_clocks(&clocks, matches!(kind, DeleteClockKind::Generated))
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
    kind: MutationKind,
) -> Result<ConstraintChange, crate::ShellError> {
    let mut clocks = Vec::new();
    for arg in &args.clocks {
        clocks.extend(resolve_clock_list(state, interp, command, arg)?);
    }
    state
        .session
        .borrow_mut()
        .set_propagated_clock(matches!(kind, MutationKind::Set), &clocks)
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
pub(crate) enum DeleteClockKind {
    Any,
    Generated,
}

#[derive(Clone, Copy)]
pub(crate) enum PortConstraintKind {
    InputTransition,
    Load,
    Drive,
}

#[derive(Clone, Copy)]
pub(crate) enum LogicKind {
    Zero,
    One,
    DontCare,
}

#[derive(Clone, Copy)]
pub(crate) enum MutationKind {
    Set,
    Unset,
}

#[derive(Clone, Copy)]
pub(crate) enum DesignRuleKind {
    Transition,
    Capacitance,
    Fanout,
}

#[derive(Clone, Copy)]
pub(crate) enum PathDelayKind {
    Max,
    Min,
}

#[derive(Clone, Copy)]
enum PathExceptionCommand {
    FalsePath,
    MaxDelay(f64),
    MinDelay(f64),
    MultiCycle(u32),
}

struct PathExceptionArgs<'a> {
    mutation: MutationKind,
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
    if let Some(delay) = args.delay {
        options.delay_type = match delay.as_str() {
            "max" => DelayType::Max,
            "min" => DelayType::Min,
            "min_max" => {
                return Err(crate::ShellError::command(
                    "report_timing: -delay_type min_max is not implemented yet",
                ));
            }
            _ => unreachable!("derive schema validates report_timing delay type"),
        };
    }
    if let Some(max_paths) = args.max_paths {
        if max_paths == 0 {
            return Err(crate::ShellError::command(
                "report_timing: -max_paths must be greater than zero",
            ));
        }
        options.max_paths = max_paths;
    }
    if let Some(digits) = args.significant_digits {
        if digits > 13 {
            return Err(crate::ShellError::command(format!(
                "report_timing: significant digits value '{digits}' is outside 0..13"
            )));
        }
        options.significant_digits = digits;
    }
    if let Some(path) = args.path
        && path != "full"
    {
        return Err(crate::ShellError::command(format!(
            "report_timing: -path {path} is not implemented yet"
        )));
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

pub(crate) fn read_sdc(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: ReadSdcArgs,
) -> Result<CommandResult, crate::ShellError> {
    sdc::read_sdc(state, interp, args)
}

pub(crate) fn write_sdc(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: WriteSdcArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .write_sdc(&args.file)
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn read_parasitics(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: ReadParasiticsArgs,
) -> Result<CommandResult, crate::ShellError> {
    read_parasitics_command(state, args).map(CommandResult::Complete)
}

pub(crate) fn create_clock(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: CreateClockArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    create_clock_command(state, args).map(CommandResult::Complete)
}

pub(crate) fn create_generated_clock(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: CreateGeneratedClockArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    create_generated_clock_command(state, interp, args).map(CommandResult::Complete)
}

pub(crate) fn delete_clock(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: DeleteClockArgs<'_>,
    kind: DeleteClockKind,
) -> Result<CommandResult, crate::ShellError> {
    delete_clock_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn set_port_constraint(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: PortConstraintCommandArgs<'_>,
    kind: PortConstraintKind,
) -> Result<CommandResult, crate::ShellError> {
    set_port_constraint_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn set_clock_transition(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: SetClockTransitionArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_clock_transition_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn unset_clock_transition(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: UnsetClockTransitionArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    unset_clock_transition_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn set_clock_latency(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: SetClockLatencyArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_clock_latency_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn unset_clock_latency(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: UnsetClockLatencyArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    unset_clock_latency_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn set_clock_uncertainty(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetClockUncertaintyArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    clock_uncertainty_command(state, interp, command, true, args.into())
        .map(constraint_change_result)
}

pub(crate) fn unset_clock_uncertainty(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: UnsetClockUncertaintyArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    clock_uncertainty_command(state, interp, command, false, args.into())
        .map(constraint_change_result)
}

pub(crate) fn set_clock_groups(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: SetClockGroupsArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_clock_groups_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn unset_clock_groups(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: UnsetClockGroupsArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    unset_clock_groups_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn set_case_analysis(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetCaseAnalysisArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    case_analysis_command(state, interp, command, Some(args.value), args.objects)
        .map(constraint_change_result)
}

pub(crate) fn unset_case_analysis(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: UnsetCaseAnalysisArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    case_analysis_command(state, interp, command, None, args.objects).map(constraint_change_result)
}

pub(crate) fn set_logic(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetLogicArgs<'_>,
    kind: LogicKind,
) -> Result<CommandResult, crate::ShellError> {
    set_logic_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn disable_timing(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: DisableTimingArgs<'_>,
    kind: MutationKind,
) -> Result<CommandResult, crate::ShellError> {
    disable_timing_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn set_timing_derate(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: SetTimingDerateArgs,
) -> Result<CommandResult, crate::ShellError> {
    set_timing_derate_command(state, args).map(constraint_change_result)
}

pub(crate) fn unset_timing_derate(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: UnsetTimingDerateArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow_mut()
        .unset_timing_derate()
        .map(constraint_change_result)
        .map_err(crate::ShellError::from)
}

pub(crate) fn propagated_clock(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: PropagatedClockArgs<'_>,
    kind: MutationKind,
) -> Result<CommandResult, crate::ShellError> {
    set_propagated_clock_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn set_resistance(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: SetResistanceArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_resistance_command(state, interp, args).map(constraint_change_result)
}

pub(crate) fn set_io_delay(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetIoDelayArgs<'_>,
    kind: opto_session::IoDelayKind,
) -> Result<CommandResult, crate::ShellError> {
    set_io_delay_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn unset_io_delay(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: UnsetIoDelayArgs<'_>,
    kind: opto_session::IoDelayKind,
) -> Result<CommandResult, crate::ShellError> {
    unset_io_delay_command(state, interp, command, args, kind).map(constraint_change_result)
}

pub(crate) fn set_scoped_design_rule(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: ScopedDesignRuleArgs<'_>,
    kind: DesignRuleKind,
) -> Result<CommandResult, crate::ShellError> {
    let scope = match (args.data_path, args.clock_path) {
        (false, false) => DesignRuleScope::All,
        (true, false) => DesignRuleScope::DataPath,
        (false, true) => DesignRuleScope::ClockPath,
        (true, true) => DesignRuleScope::ClockAndData,
    };
    set_design_rule_command(
        state,
        interp,
        command,
        args.limit,
        &args.objects,
        scope,
        kind,
    )
    .map(constraint_change_result)
}

pub(crate) fn set_max_fanout(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetMaxFanoutArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_design_rule_command(
        state,
        interp,
        command,
        args.limit,
        &args.objects,
        DesignRuleScope::All,
        DesignRuleKind::Fanout,
    )
    .map(constraint_change_result)
}

pub(crate) fn set_path_delay(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetPathDelayArgs<'_>,
    delay_kind: PathDelayKind,
) -> Result<CommandResult, crate::ShellError> {
    let kind = match delay_kind {
        PathDelayKind::Max => PathExceptionCommand::MaxDelay(args.delay),
        PathDelayKind::Min => PathExceptionCommand::MinDelay(args.delay),
    };
    set_path_exception_command(
        state,
        interp,
        command,
        PathExceptionArgs {
            mutation: MutationKind::Set,
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

pub(crate) fn set_false_path(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetFalsePathArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_path_exception_command(
        state,
        interp,
        command,
        PathExceptionArgs {
            mutation: MutationKind::Set,
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

pub(crate) fn unset_path_exceptions(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: UnsetPathExceptionsArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_path_exception_command(
        state,
        interp,
        command,
        PathExceptionArgs {
            mutation: MutationKind::Unset,
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

pub(crate) fn set_multicycle_path(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SetMulticyclePathArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    set_path_exception_command(
        state,
        interp,
        command,
        PathExceptionArgs {
            mutation: MutationKind::Set,
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

pub(crate) fn report_clock(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: ReportClockArgs,
) -> Result<CommandResult, crate::ShellError> {
    Ok(CommandResult::Complete(
        state.session.borrow().report_clock(),
    ))
}

pub(crate) fn check_timing(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: CheckTimingArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .check_timing()
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn report_timing(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: ReportTimingArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    report_timing_command(state, interp, args).map(CommandResult::Complete)
}
