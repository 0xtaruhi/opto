// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(
    name = "help",
    handler = help,
    summary = "List registered commands or explain one command's public syntax.",
    requires = "No design or library state is required.",
    example = "help read_hdl"
)]
pub(crate) struct HelpArgs {
    #[arg(positional)]
    command: Option<String>,
}

#[derive(TclCommand)]
#[command(
    name = "echo",
    handler = echo,
    summary = "Return the supplied Tcl words as one space-separated string.",
    requires = "No design or library state is required.",
    example = "echo synthesis complete"
)]
pub(crate) struct EchoArgs<'a> {
    #[arg(positional)]
    words: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "redirect",
    handler = redirect,
    summary = "Evaluate a Tcl command and redirect its result to a file or variable.",
    requires = "The nested command must be valid; file targets must be writable.",
    example = "redirect -file reports/area.rpt {report_area}",
    positional_if_any = "-file,-variable",
    positional_present = 1,
    positional_absent = 2
)]
pub(crate) struct RedirectArgs<'a> {
    #[arg(long = "-append")]
    append: bool,
    #[arg(long = "-tee")]
    tee: bool,
    #[arg(long = "-file", conflicts_with = "variable", value_hint = ValueHint::File)]
    file: Option<String>,
    #[arg(long = "-variable")]
    variable: Option<String>,
    #[arg(long = "-channel", unsupported, value_hint = ValueHint::Text)]
    _channel: (),
    #[arg(long = "-compress", unsupported)]
    _compress: (),
    #[arg(long = "-bg", unsupported)]
    _bg: (),
    #[arg(long = "-max_cores", unsupported)]
    _max_cores: (),
    #[arg(
        positional,
        min = 1,
        max = 2,
        label = "script",
        help = "The nested Tcl script. Without -file or -variable, precede it with the output target."
    )]
    positionals: Vec<TclArg<'a>>,
}

#[derive(TclCommand)]
#[command(
    name = "source",
    handler = source,
    sdc,
    validation = crate::command_catalog::ValidationBehavior::SourceFile,
    summary = "Evaluate commands from a Tcl script in the current session.",
    requires = "The referenced Tcl input must exist and be readable.",
    example = "source scripts/setup.tcl"
)]
pub(crate) struct SourceArgs {
    #[arg(positional, value_hint = ValueHint::File)]
    path: PathBuf,
}

#[derive(TclCommand)]
#[command(
    name = "exit",
    handler = exit,
    sdc,
    validation = crate::command_catalog::ValidationBehavior::ReturnFromScript,
    summary = "Stop the current script and return an optional process status.",
    requires = "The optional status must be an integer.",
    example = "exit 0"
)]
pub(crate) struct ExitArgs {
    #[arg(positional)]
    code: Option<i32>,
}

fn source_command(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &str,
    path: &std::path::Path,
) -> Result<CommandResult, crate::ShellError> {
    state.exit_code.replace(None);
    let c_path = path_to_cstring(path)?;
    let code = eval_file(interp, &c_path);
    if code == TCL_RETURN && state.domain.get().is_sdc() {
        return Ok(CommandResult::Exit(0));
    }
    eval_result(state, interp, code).map(command_result_from_eval)
}

fn redirect_command(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &str,
    args: RedirectArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let (target_kind, target, script) = match (args.file, args.variable) {
        (Some(target), None) => {
            let [script] = args.positionals.as_slice() else {
                return Err(crate::ShellError::command(format!(
                    "{command}: expected ?-append? ?-tee? ?-file|-variable? target command_string"
                )));
            };
            (RedirectTargetKind::File, target, script.to_string())
        }
        (None, Some(target)) => {
            let [script] = args.positionals.as_slice() else {
                return Err(crate::ShellError::command(format!(
                    "{command}: expected ?-append? ?-tee? ?-file|-variable? target command_string"
                )));
            };
            (RedirectTargetKind::Variable, target, script.to_string())
        }
        (None, None) => {
            let [target, script] = args.positionals.as_slice() else {
                return Err(crate::ShellError::command(format!(
                    "{command}: expected ?-append? ?-tee? ?-file|-variable? target command_string"
                )));
            };
            (
                RedirectTargetKind::File,
                target.to_string(),
                script.to_string(),
            )
        }
        (Some(_), Some(_)) => unreachable!("derive schema validates redirect target conflict"),
    };
    let options = RedirectOptions {
        target_kind,
        target,
        script,
        append: args.append,
        tee: args.tee,
    };

    state.exit_code.replace(None);
    let code = eval_script(interp, &options.script)?;
    match eval_result(state, interp, code)? {
        EvalResult::Complete(value) => {
            let output = command_output(&value);
            match options.target_kind {
                RedirectTargetKind::File => options.write_output(&output)?,
                RedirectTargetKind::Variable => {
                    let output = if options.append {
                        let mut previous =
                            get_tcl_var(interp, &options.target)?.unwrap_or_default();
                        previous.push_str(&output);
                        previous
                    } else {
                        output.clone()
                    };
                    set_tcl_var(interp, &options.target, &output)?;
                }
            }
            if options.tee {
                io::stdout()
                    .write_all(output.as_bytes())
                    .map_err(|source| crate::ShellError::Output {
                        action: "redirect: tee failed",
                        source,
                    })?;
            }
            Ok(CommandResult::Complete(String::new()))
        }
        EvalResult::Exit(code) => Ok(CommandResult::Exit(code)),
    }
}

fn exit_command(command: &str, code: Option<i32>) -> Result<CommandResult, crate::ShellError> {
    let code = code.unwrap_or(0);
    if !(0..=255).contains(&code) {
        return Err(crate::ShellError::command(format!(
            "{command}: return code {code} is outside 0..255"
        )));
    }
    Ok(CommandResult::Exit(code))
}

pub(crate) fn help(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: HelpArgs,
) -> Result<CommandResult, crate::ShellError> {
    let _ = interp;
    let text = match args.command {
        None => state.commands.help_text(),
        Some(name) => state
            .commands
            .command_help_text(&name)
            .ok_or_else(|| crate::ShellError::command(format!("help: unknown command '{name}'")))?,
    };
    Ok(CommandResult::Complete(text))
}

pub(crate) fn echo(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: EchoArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let _ = (state, interp, command);
    Ok(CommandResult::Complete(
        args.words
            .iter()
            .map(TclArg::as_str)
            .collect::<Vec<_>>()
            .join(" "),
    ))
}

pub(crate) fn redirect(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: RedirectArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    redirect_command(state, interp, command, args)
}

pub(crate) fn source(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: SourceArgs,
) -> Result<CommandResult, crate::ShellError> {
    source_command(state, interp, command, &args.path)
}

pub(crate) fn exit(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    args: ExitArgs,
) -> Result<CommandResult, crate::ShellError> {
    let _ = (state, interp);
    exit_command(command, args.code)
}
