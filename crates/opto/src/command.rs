// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::command_args::TclOption;
use crate::command_catalog::{RegisteredCommand, ValueHint};
use crate::redirect::{RedirectOptions, RedirectTargetKind, command_output};
use crate::runtime::ShellState;
use crate::tcl::{
    TclArg, eval_file, eval_result, eval_script, get_tcl_var, path_to_cstring, set_tcl_var,
    split_tcl_list,
};
use crate::{command_catalog, sdc};
use opto_session::{
    CollectionFilter, ConstraintChange, DelayType, DesignRuleScope, EdgeQualifier, EdgeSelection,
    ExceptionCorner, ExceptionFilter, FilterOperator, FrontendOptions, PathException,
    PathExceptionKind, PowerReportKind, ReportPowerOptions, ReportTimingOptions,
    SwitchingActivityUpdate, SynthesisEffort, SynthesisEvent, TimingEndpoint, TimingObject,
    VerilogLanguage,
};
use opto_tcl_sys::ffi::{TCL_RETURN, TclInterp};
use std::io::{self, Write};
use std::path::PathBuf;

pub(crate) use opto_command_macros::TclCommand;

pub(crate) mod checkpoint;
pub(crate) mod collection;
pub(crate) mod core;
pub(crate) mod database;
pub(crate) mod design;
pub(crate) mod hdl;
pub(crate) mod power;
pub(crate) mod synthesis;
pub(crate) mod timing;

#[derive(Debug)]
pub(super) enum EvalResult {
    Complete(String),
    Exit(i32),
}

pub(super) enum CommandResult {
    Complete(String),
    List(Vec<String>),
    Exit(i32),
}

fn constraint_change_result(change: ConstraintChange) -> CommandResult {
    CommandResult::Complete(
        match change {
            ConstraintChange::Unchanged => "0",
            ConstraintChange::Changed => "1",
        }
        .to_string(),
    )
}

pub(super) fn dispatch(
    state: &ShellState,
    interp: *mut TclInterp,
    registered: &RegisteredCommand,
    args: &[TclArg<'_>],
) -> Result<CommandResult, crate::ShellError> {
    let spec = registered.spec();
    let command = spec.name;
    if let Some(version) = state.domain.get().sdc_version()
        && !command_catalog::available_in_sdc(spec, version)
    {
        return Err(crate::ShellError::command(format!(
            "invalid command name \"{command}\""
        )));
    }
    let invocation =
        command_catalog::parse_invocation(registered, args, state.domain.get().is_sdc())?;
    (spec.executor)(state, interp, spec.name, &invocation)
}

fn command_result_from_eval(result: EvalResult) -> CommandResult {
    match result {
        EvalResult::Complete(value) => CommandResult::Complete(value),
        EvalResult::Exit(code) => CommandResult::Exit(code),
    }
}
