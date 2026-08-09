// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::command::EvalResult;
use crate::command_catalog::CommandRegistry;
use crate::sdc::EvalDomain;
use crate::tcl::{
    error_line, eval_file, eval_result, eval_script, path_to_cstring, register_command_specs,
};
use opto_session::Session;
use opto_tcl_sys::Interpreter;
use opto_tcl_sys::ffi::TclInterp;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::io;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// Product-shell startup inputs after command-line normalization.
#[derive(Debug, Clone, Default)]
pub struct ShellArgs {
    /// Tcl script file to execute, mutually exclusive with [`Self::command`].
    pub script: Option<PathBuf>,
    /// In-memory Tcl command to execute, mutually exclusive with [`Self::script`].
    pub command: Option<String>,
    /// Skip every `.opto.tcl` file.
    pub no_init: bool,
    /// Skip the `.opto.tcl` file in the user's home directory.
    pub no_home_init: bool,
    /// Skip the `.opto.tcl` file in the current working directory.
    pub no_local_init: bool,
    /// Terminal color and theme choices.
    pub ui: crate::UiOptions,
}

/// Entry point for batch, pipe, and interactive shell execution.
#[derive(Debug)]
pub struct Shell;

pub(super) struct ShellState {
    pub(super) session: RefCell<Session>,
    pub(super) exit_code: RefCell<Option<i32>>,
    pub(super) pending_command_error: RefCell<Option<(String, crate::ShellError)>>,
    pub(super) domain: Cell<EvalDomain>,
    pub(super) ui: crate::UiOptions,
    pub(super) interactive: bool,
    pub(super) commands: CommandRegistry,
}

impl Shell {
    /// Runs one shell invocation and returns its process-style exit status.
    ///
    /// The supplied [`Session`] becomes the sole mutable product state for the
    /// invocation, and `commands` defines its exact public Tcl command surface.
    /// A command or script error is returned as [`crate::ShellError`]; an explicit
    /// Tcl `exit` is returned as an integer status.
    pub fn run(
        args: ShellArgs,
        session: Session,
        commands: CommandRegistry,
    ) -> Result<i32, crate::ShellError> {
        let interactive = args.command.is_none()
            && args.script.is_none()
            && io::stdin().is_terminal()
            && io::stdout().is_terminal();
        let mut runtime = Runtime::new(session)?;
        runtime.state.ui = args.ui;
        runtime.state.interactive = interactive;
        runtime.register_registry(commands)?;
        runtime.source_setup_files(&args)?;

        if let Some(command) = args.command {
            match runtime.eval(&command)? {
                EvalResult::Complete(result) => {
                    if !result.is_empty() {
                        println!("{result}");
                    }
                    Ok(0)
                }
                EvalResult::Exit(code) => Ok(code),
            }
        } else if let Some(script) = args.script {
            match runtime.eval_file(&script)? {
                EvalResult::Complete(_) => Ok(0),
                EvalResult::Exit(code) => Ok(code),
            }
        } else if interactive {
            runtime.repl(args.ui)
        } else {
            runtime.run_stdin()
        }
    }
}

/// Validate an in-memory SDC script using the same command catalog and Tcl
/// interpreter as `read_sdc`, without committing constraint changes.
pub fn validate_sdc_syntax(script: &str) -> Result<(), crate::ShellError> {
    let version = crate::sdc::SdcVersion::V2_2;
    let mut commands = CommandRegistry::new();
    commands.register_group(crate::commands::SDC)?;
    let (code, diagnostic, line) = crate::sdc::validate_script_syntax(script, version, &commands)?;
    if code == opto_tcl_sys::ffi::TCL_OK || code == opto_tcl_sys::ffi::TCL_RETURN {
        Ok(())
    } else {
        Err(crate::ShellError::command(diagnostic).with_source("<sdc>", script, line))
    }
}

pub(super) struct Runtime {
    interpreter: Interpreter,
    pub(super) state: Box<ShellState>,
}

impl Runtime {
    pub(super) fn new(session: Session) -> Result<Self, crate::ShellError> {
        Ok(Self {
            interpreter: Interpreter::new()?,
            state: Box::new(ShellState {
                session: RefCell::new(session),
                exit_code: RefCell::new(None),
                pending_command_error: RefCell::new(None),
                domain: Cell::new(EvalDomain::Shell),
                ui: crate::UiOptions::default(),
                interactive: false,
                commands: CommandRegistry::new(),
            }),
        })
    }

    pub(super) fn interp(&self) -> *mut TclInterp {
        self.interpreter.as_ptr()
    }

    fn source_setup_files(&mut self, args: &ShellArgs) -> Result<(), crate::ShellError> {
        if args.no_init {
            return Ok(());
        }

        let mut files = Vec::new();
        if !args.no_home_init
            && let Some(home) = std::env::var_os("HOME")
        {
            files.push(PathBuf::from(home).join(".opto.tcl"));
        }
        if !args.no_local_init {
            files.push(
                std::env::current_dir()
                    .map_err(crate::ShellError::CurrentDirectory)?
                    .join(".opto.tcl"),
            );
        }

        let mut sourced = HashSet::new();
        for file in files {
            if file.is_file() && sourced.insert(file.clone()) {
                match self.eval_file(&file)? {
                    EvalResult::Complete(_) => {}
                    EvalResult::Exit(code) => {
                        return Err(crate::ShellError::command(format!(
                            "{} exited initialization with status {code}",
                            file.display()
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn register_registry(&mut self, commands: CommandRegistry) -> Result<(), crate::ShellError> {
        self.state.commands = commands;
        register_command_specs(self.interp(), &self.state, self.state.commands.iter())
    }

    #[cfg(test)]
    pub(super) fn register_commands(&mut self) -> Result<(), crate::ShellError> {
        let mut commands = CommandRegistry::new();
        commands.register_group(crate::commands::ALL)?;
        self.register_registry(commands)
    }

    pub(super) fn eval(&mut self, script: &str) -> Result<EvalResult, crate::ShellError> {
        self.state.exit_code.replace(None);
        self.state.pending_command_error.replace(None);
        let code = eval_script(self.interp(), script)?;
        let line = error_line(self.interp());
        eval_result(&self.state, self.interp(), code)
            .map_err(|error| error.with_source("<command>", script, line))
    }

    fn eval_file(&mut self, path: &Path) -> Result<EvalResult, crate::ShellError> {
        self.state.exit_code.replace(None);
        self.state.pending_command_error.replace(None);
        let c_path = path_to_cstring(path)?;
        let code = eval_file(self.interp(), &c_path);
        let line = error_line(self.interp());
        eval_result(&self.state, self.interp(), code).map_err(
            |error| match std::fs::read_to_string(path) {
                Ok(source) => error.with_source(path.to_string_lossy(), source, line),
                Err(_) => error,
            },
        )
    }

    fn repl(&mut self, ui: crate::UiOptions) -> Result<i32, crate::ShellError> {
        crate::ui::run_repl(self, ui)
    }

    fn run_stdin(&mut self) -> Result<i32, crate::ShellError> {
        let stdin = io::stdin();
        let mut command = String::new();
        loop {
            let mut line = String::new();
            let read = stdin
                .read_line(&mut line)
                .map_err(|source| crate::ShellError::Output {
                    action: "failed to read REPL input",
                    source,
                })?;
            if read == 0 {
                if !command.trim().is_empty() {
                    if !crate::ui::command_complete(&command)? {
                        return Err(crate::ShellError::command(
                            "standard input ends with an incomplete Tcl command",
                        ));
                    }
                    match self.eval(&command)? {
                        EvalResult::Complete(result) if !result.is_empty() => println!("{result}"),
                        EvalResult::Complete(_) => {}
                        EvalResult::Exit(code) => return Ok(code),
                    }
                }
                return Ok(0);
            }
            command.push_str(&line);
            if !crate::ui::command_complete(&command)? {
                continue;
            }
            match self.eval(&command) {
                Ok(EvalResult::Complete(result)) if !result.is_empty() => println!("{result}"),
                Ok(EvalResult::Complete(_)) => {}
                Ok(EvalResult::Exit(code)) => return Ok(code),
                Err(err) => crate::diagnostic::print_error(&err, self.state.ui),
            }
            command.clear();
        }
    }
}
