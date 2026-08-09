// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::command::CommandResult;
use crate::command::timing::ReadSdcArgs;
use crate::command_catalog;
use crate::command_catalog::CommandRegistry;
use crate::runtime::ShellState;
use crate::tcl::{
    command_complete as tcl_command_complete, error_line, eval_file, eval_script, make_safe,
    path_to_cstring, register_command_specs, register_validation_command_specs, tcl_result,
};
use opto_tcl_sys::Interpreter;
use opto_tcl_sys::ffi::{TCL_ERROR, TCL_OK, TCL_RETURN, TclInterp};
use std::ffi::CString;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SdcVersion {
    V1_0,
    V1_1,
    V1_2,
    V1_3,
    V1_4,
    V1_5,
    V1_6,
    V1_7,
    V1_8,
    V1_9,
    V2_0,
    V2_1,
    V2_2,
}

impl SdcVersion {
    fn parse(raw: &str) -> Result<Self, crate::ShellError> {
        match raw {
            "1.0" => Ok(Self::V1_0),
            "1.1" => Ok(Self::V1_1),
            "1.2" => Ok(Self::V1_2),
            "1.3" => Ok(Self::V1_3),
            "1.4" => Ok(Self::V1_4),
            "1.5" => Ok(Self::V1_5),
            "1.6" => Ok(Self::V1_6),
            "1.7" => Ok(Self::V1_7),
            "1.8" => Ok(Self::V1_8),
            "1.9" => Ok(Self::V1_9),
            "2.0" => Ok(Self::V2_0),
            "2.1" => Ok(Self::V2_1),
            "2.2" | "latest" => Ok(Self::V2_2),
            _ => Err(crate::ShellError::command(format!(
                "read_sdc: -version must be 1.0 through 2.2 or latest, got '{raw}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvalDomain {
    Shell,
    Sdc { version: SdcVersion },
}

impl EvalDomain {
    pub(super) fn is_sdc(self) -> bool {
        matches!(self, Self::Sdc { .. })
    }

    pub(super) fn sdc_version(self) -> Option<SdcVersion> {
        match self {
            Self::Shell => None,
            Self::Sdc { version, .. } => Some(version),
        }
    }
}

#[derive(Debug)]
struct ReadSdcOptions {
    file: PathBuf,
    echo: bool,
    syntax_only: bool,
    version: SdcVersion,
}

pub(super) fn read_sdc(
    state: &ShellState,
    _interp: *mut TclInterp,
    args: ReadSdcArgs,
) -> Result<CommandResult, crate::ShellError> {
    let options = ReadSdcOptions::from_args(args)?;
    let evaluation = (|| -> Result<_, crate::ShellError> {
        if options.echo {
            echo_file_commands(&options.file)?;
        }
        if options.syntax_only {
            return evaluate_syntax_file(state, &options);
        }
        evaluate_file_transactionally(state, &options)
    })();
    let (code, diagnostic, error_line) = evaluation?;

    let success = code == TCL_OK || code == TCL_RETURN;
    if !success && !diagnostic.is_empty() {
        let line =
            std::fs::read_to_string(&options.file)
                .ok()
                .map(|source| crate::error::ErrorSource {
                    name: options.file.to_string_lossy().into_owned(),
                    text: source,
                    line: error_line,
                    column: None,
                    length: 1,
                });
        if let Some(source) = line.as_ref() {
            crate::diagnostic::print_source_error(&diagnostic, source, state.ui);
        } else {
            eprintln!("{}: {diagnostic}", options.file.display());
        }
    }
    Ok(CommandResult::Complete(
        if success { "1" } else { "0" }.to_string(),
    ))
}

fn evaluate_file_transactionally(
    state: &ShellState,
    options: &ReadSdcOptions,
) -> Result<(i32, String, usize), crate::ShellError> {
    let checkpoint = state.session.borrow_mut().constraint_checkpoint();
    match evaluate_file(state, options) {
        Ok(evaluation) if evaluation.0 == TCL_OK || evaluation.0 == TCL_RETURN => {
            state
                .session
                .borrow_mut()
                .commit_constraint_checkpoint(checkpoint)?;
            Ok(evaluation)
        }
        Ok(evaluation) => {
            state
                .session
                .borrow_mut()
                .restore_constraint_checkpoint(checkpoint)?;
            Ok(evaluation)
        }
        Err(error) => {
            state
                .session
                .borrow_mut()
                .restore_constraint_checkpoint(checkpoint)?;
            Err(error)
        }
    }
}

fn evaluate_file(
    state: &ShellState,
    options: &ReadSdcOptions,
) -> Result<(i32, String, usize), crate::ShellError> {
    let interpreter = Interpreter::new()?;
    register_command_specs(
        interpreter.as_ptr(),
        state,
        state
            .commands
            .iter()
            .filter(|command| command_catalog::available_in_sdc(command.spec(), options.version)),
    )?;
    let previous_domain = state.domain.replace(EvalDomain::Sdc {
        version: options.version,
    });
    let evaluation = (|| -> Result<_, crate::ShellError> {
        state.exit_code.replace(None);
        let path = path_to_cstring(&options.file)?;
        let code = eval_file(interpreter.as_ptr(), &path);
        let diagnostic = tcl_result(interpreter.as_ptr());
        let line = error_line(interpreter.as_ptr());
        Ok((code, diagnostic, line))
    })();
    state.domain.set(previous_domain);
    evaluation
}

fn evaluate_syntax_file(
    state: &ShellState,
    options: &ReadSdcOptions,
) -> Result<(i32, String, usize), crate::ShellError> {
    let interpreter = validation_interpreter(options.version, &state.commands)?;
    let source =
        std::fs::read_to_string(&options.file).map_err(|source| crate::ShellError::FileIo {
            operation: "read_sdc: cannot read",
            path: options.file.clone(),
            source,
        })?;
    if let Err(diagnostic) =
        validate_all_script_paths(&interpreter, &state.commands, &source, options.version, 1)
    {
        return Ok((TCL_ERROR, diagnostic.message, diagnostic.line));
    }
    let code = eval_script(interpreter.as_ptr(), &source)?;
    let diagnostic = tcl_result(interpreter.as_ptr());
    let line = error_line(interpreter.as_ptr());
    Ok((code, diagnostic, line))
}

pub(super) fn validate_script_syntax(
    script: &str,
    version: SdcVersion,
    commands: &CommandRegistry,
) -> Result<(i32, String, usize), crate::ShellError> {
    let interpreter = validation_interpreter(version, commands)?;
    if let Err(diagnostic) = validate_all_script_paths(&interpreter, commands, script, version, 1) {
        return Ok((TCL_ERROR, diagnostic.message, diagnostic.line));
    }
    let code = eval_script(interpreter.as_ptr(), script)?;
    let diagnostic = tcl_result(interpreter.as_ptr());
    let line = error_line(interpreter.as_ptr());
    Ok((code, diagnostic, line))
}

#[derive(Debug)]
struct ValidationDiagnostic {
    message: String,
    line: usize,
}

fn validate_all_script_paths(
    interpreter: &Interpreter,
    registry: &CommandRegistry,
    script: &str,
    version: SdcVersion,
    base_line: usize,
) -> Result<(), ValidationDiagnostic> {
    validate_all_script_paths_at_depth(interpreter, registry, script, version, base_line, 0)
}

fn validate_all_script_paths_at_depth(
    interpreter: &Interpreter,
    registry: &CommandRegistry,
    script: &str,
    version: SdcVersion,
    base_line: usize,
    depth: usize,
) -> Result<(), ValidationDiagnostic> {
    if depth >= 1_000 {
        return Err(ValidationDiagnostic {
            message: "SDC validation exceeded Tcl's nested script limit".to_string(),
            line: base_line,
        });
    }
    let parsed_commands =
        interpreter
            .parse_commands(script)
            .map_err(|error| ValidationDiagnostic {
                message: error.to_string(),
                line: base_line,
            })?;
    for command in parsed_commands {
        let line = base_line
            + script
                .get(..command.byte_offset)
                .unwrap_or_default()
                .chars()
                .filter(|character| *character == '\n')
                .count();
        for word in &command.words {
            for substitution in &word.command_substitutions {
                validate_all_script_paths_at_depth(
                    interpreter,
                    registry,
                    substitution,
                    version,
                    line,
                    depth + 1,
                )?;
            }
        }

        let Some(name) = command
            .words
            .first()
            .and_then(|word| word.literal.as_deref())
        else {
            return Err(ValidationDiagnostic {
                message: "dynamic command names cannot be validated in read_sdc -syntax_only"
                    .to_string(),
                line,
            });
        };
        if let Some(registered) = registry.find(name) {
            if !command_catalog::available_in_sdc(registered.spec(), version) {
                return Err(ValidationDiagnostic {
                    message: format!("invalid command name \"{name}\""),
                    line,
                });
            }
            if let Some(arguments) = literal_arguments(&command.words[1..]) {
                command_catalog::validate_sdc_invocation(registered, &arguments).map_err(
                    |error| ValidationDiagnostic {
                        message: error.to_string(),
                        line,
                    },
                )?;
            }
        } else if !interpreter
            .has_command(name)
            .map_err(|error| ValidationDiagnostic {
                message: error.to_string(),
                line,
            })?
        {
            return Err(ValidationDiagnostic {
                message: format!("invalid command name \"{name}\""),
                line,
            });
        }

        for body in literal_command_bodies(name, &command.words) {
            validate_all_script_paths_at_depth(
                interpreter,
                registry,
                body,
                version,
                line,
                depth + 1,
            )?;
        }
    }
    Ok(())
}

fn literal_arguments(words: &[opto_tcl_sys::ParsedWord]) -> Option<Vec<&str>> {
    words.iter().map(|word| word.literal.as_deref()).collect()
}

fn literal_command_bodies<'a>(name: &str, words: &'a [opto_tcl_sys::ParsedWord]) -> Vec<&'a str> {
    let literal = |index: usize| words.get(index).and_then(|word| word.literal.as_deref());
    match name {
        "if" => {
            let mut bodies = Vec::new();
            let mut index = 1usize;
            while index < words.len() {
                index += 1;
                if literal(index) == Some("then") {
                    index += 1;
                }
                if let Some(body) = literal(index) {
                    bodies.push(body);
                }
                index += 1;
                match literal(index) {
                    Some("elseif") => index += 1,
                    Some("else") => {
                        if let Some(body) = literal(index + 1) {
                            bodies.push(body);
                        }
                        break;
                    }
                    _ => break,
                }
            }
            bodies
        }
        "foreach" | "lmap" => literal(words.len().saturating_sub(1)).into_iter().collect(),
        "while" => literal(2).into_iter().collect(),
        "for" => [literal(1), literal(3), literal(4)]
            .into_iter()
            .flatten()
            .collect(),
        "proc" => literal(3).into_iter().collect(),
        "catch" | "time" => literal(1).into_iter().collect(),
        "eval" if words.len() == 2 => literal(1).into_iter().collect(),
        "namespace" if literal(1) == Some("eval") && words.len() == 4 => {
            literal(3).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

fn validation_interpreter(
    version: SdcVersion,
    commands: &CommandRegistry,
) -> Result<Interpreter, crate::ShellError> {
    let interpreter = Interpreter::new()?;
    let code = make_safe(interpreter.as_ptr());
    if code != TCL_OK {
        return Err(crate::ShellError::command(tcl_result(interpreter.as_ptr())));
    }
    register_validation_command_specs(
        interpreter.as_ptr(),
        commands
            .iter()
            .filter(|command| command_catalog::available_in_sdc(command.spec(), version)),
    )?;
    Ok(interpreter)
}

impl ReadSdcOptions {
    fn from_args(args: ReadSdcArgs) -> Result<Self, crate::ShellError> {
        let version = args
            .version
            .as_deref()
            .map_or(Ok(SdcVersion::V2_2), SdcVersion::parse)?;
        Ok(Self {
            file: args.file,
            echo: args.echo,
            syntax_only: args.syntax_only,
            version,
        })
    }
}

fn echo_file_commands(path: &PathBuf) -> Result<(), crate::ShellError> {
    let text = std::fs::read_to_string(path).map_err(|source| crate::ShellError::FileIo {
        operation: "read_sdc: cannot read",
        path: path.clone(),
        source,
    })?;
    let mut command = String::new();
    for line in text.lines() {
        command.push_str(line);
        command.push('\n');
        if command_complete(&command)? {
            let trimmed = command.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                println!("{trimmed}");
            }
            command.clear();
        }
    }
    if !command.is_empty() {
        return Err(crate::ShellError::command(format!(
            "read_sdc: '{}' ends with an incomplete Tcl command",
            path.display()
        )));
    }
    Ok(())
}

fn command_complete(command: &str) -> Result<bool, crate::ShellError> {
    let command = CString::new(command).map_err(|source| crate::ShellError::Nul {
        context: "Tcl command",
        source,
    })?;
    Ok(tcl_command_complete(&command))
}
