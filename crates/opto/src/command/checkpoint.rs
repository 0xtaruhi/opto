// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Native Opto checkpoint commands.

use super::*;

#[derive(TclCommand)]
#[command(
    name = "save",
    handler = save,
    summary = "Write the complete validated session state to an Opto checkpoint.",
    requires = "The destination path must be writable.",
    example = "save build/top.ock"
)]
pub(crate) struct SaveArgs {
    #[arg(positional, value_hint = ValueHint::File)]
    file: PathBuf,
}

#[derive(TclCommand)]
#[command(
    name = "resume",
    handler = resume,
    summary = "Replace the current session with a validated Opto checkpoint.",
    requires = "The checkpoint must exist and match the current schema and ABI.",
    example = "resume build/top.ock"
)]
pub(crate) struct ResumeArgs {
    #[arg(positional, value_hint = ValueHint::File)]
    file: PathBuf,
}

pub(crate) fn save(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: SaveArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .write_checkpoint_file(&args.file)
        .map(CommandResult::Complete)
        .map_err(Into::into)
}

pub(crate) fn resume(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    args: ResumeArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow_mut()
        .read_checkpoint_file(&args.file)
        .map(CommandResult::Complete)
        .map_err(Into::into)
}
