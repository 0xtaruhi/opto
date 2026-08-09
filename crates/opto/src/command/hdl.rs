// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(name = "read_libs", handler = read_libs)]
pub(crate) struct ReadLibsArgs<'a> {
    #[arg(positional, min = 1, value_hint = ValueHint::File)]
    files: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(name = "read_hdl", handler = read_hdl)]
pub(crate) struct ReadHdlArgs<'a> {
    #[arg(long = "-define")]
    defines: Vec<TclArg<'a>>,
    #[arg(long = "-incdir", value_hint = ValueHint::Directory)]
    include_paths: Vec<TclArg<'a>>,
    #[arg(positional, min = 1, value_hint = ValueHint::File)]
    files: Vec<TclArg<'a>>,
}

pub(crate) fn read_libs(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: ReadLibsArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let files = flatten_paths(interp, &args.files)?;
    state
        .session
        .borrow_mut()
        .read_libs(&files)
        .map(CommandResult::Complete)
        .map_err(Into::into)
}

pub(crate) fn read_hdl(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: ReadHdlArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let files = flatten_paths(interp, &args.files)?;
    let mut options = FrontendOptions {
        language: inferred_language(&files),
        ..FrontendOptions::default()
    };
    for raw in &args.include_paths {
        options
            .include_paths
            .extend(split_tcl_list(interp, raw)?.into_iter().map(PathBuf::from));
    }
    for raw in &args.defines {
        for define in split_tcl_list(interp, raw)? {
            let (name, value) = define
                .split_once('=')
                .map_or((define.as_str(), None), |(name, value)| (name, Some(value)));
            if name.is_empty() {
                return Err(crate::ShellError::command(
                    "read_hdl: preprocessor define name is empty",
                ));
            }
            options.defines.push((
                name.to_string(),
                value.map(std::string::ToString::to_string),
            ));
        }
    }
    state
        .session
        .borrow_mut()
        .read_hdl(&files, &options)
        .map(CommandResult::Complete)
        .map_err(Into::into)
}

fn inferred_language(files: &[PathBuf]) -> VerilogLanguage {
    if files.iter().all(|path| {
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("v"))
    }) {
        VerilogLanguage::Verilog2005
    } else {
        VerilogLanguage::SystemVerilog2017
    }
}

fn flatten_paths(
    interp: *mut TclInterp,
    values: &[TclArg<'_>],
) -> Result<Vec<PathBuf>, crate::ShellError> {
    let mut paths = Vec::new();
    for value in values {
        paths.extend(
            split_tcl_list(interp, value)?
                .into_iter()
                .map(PathBuf::from),
        );
    }
    Ok(paths)
}
