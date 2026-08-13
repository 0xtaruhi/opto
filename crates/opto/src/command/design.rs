// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(
    name = "elaborate",
    handler = elaborate,
    summary = "Elaborate an ingested HDL definition and make it the current design.",
    requires = "The named definition must have been ingested with read_hdl.",
    example = "elaborate top"
)]
pub(crate) struct ElaborateArgs {
    #[arg(positional, value_hint = ValueHint::Design)]
    design: String,
}

#[derive(TclCommand)]
#[command(
    name = "check_design",
    handler = check_design,
    summary = "Validate the current design without changing it.",
    requires = "A current elaborated or synthesized design is required."
)]
pub(crate) struct CheckDesignArgs {}

#[derive(TclCommand)]
#[command(
    name = "write_hdl",
    handler = write_hdl,
    summary = "Write the current design as deterministic mapped Verilog.",
    requires = "A current elaborated or synthesized design is required.",
    example = "write_hdl mapped.v"
)]
pub(crate) struct WriteHdlArgs {
    #[arg(long = "-hierarchy")]
    hierarchy: bool,
    #[arg(positional, value_hint = ValueHint::File)]
    file: PathBuf,
}

pub(crate) fn elaborate(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: ElaborateArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow_mut()
        .elaborate(&args.design)
        .map(CommandResult::Complete)
        .map_err(Into::into)
}

pub(crate) fn check_design(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: CheckDesignArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .check_design()
        .map(CommandResult::Complete)
        .map_err(Into::into)
}

pub(crate) fn write_hdl(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: WriteHdlArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .write_hdl_file(&args.file, args.hierarchy)
        .map(CommandResult::Complete)
        .map_err(Into::into)
}
