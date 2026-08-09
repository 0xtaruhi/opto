// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Command-line bootstrap for the single `opto` product executable.

use clap::{Parser, ValueEnum, error::ErrorKind};
use opto::{
    ColorMode, CommandRegistry, Shell, ShellArgs, ShellError, Theme, UiOptions, commands,
    print_error,
};
use opto_session::{Session, SessionConfig, SynthesisConfig, SynthesisDiagnostics};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use thiserror::Error;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Error)]
enum OptoError {
    #[error(transparent)]
    Cli(#[from] clap::Error),
    #[error("failed to print CLI help: {0}")]
    CliOutput(#[source] std::io::Error),
    #[error("{source}")]
    Shell {
        #[source]
        source: ShellError,
        ui: UiOptions,
    },
    #[error(transparent)]
    Session(#[from] opto_session::SessionError),
}

#[derive(Debug, Parser)]
#[command(name = "opto", version, about = "Synthesis and design-analysis shell")]
struct Cli {
    /// Execute a Tcl script.
    #[arg(short = 'f', value_name = "script.tcl", conflicts_with = "command")]
    script: Option<PathBuf>,

    /// Execute Tcl commands.
    #[arg(short = 'x', value_name = "commands", conflicts_with = "script")]
    command: Option<String>,

    /// Do not load any `.opto.tcl` initialization files.
    #[arg(long = "no-init")]
    no_init: bool,

    /// Do not load the home `.opto.tcl` file.
    #[arg(long = "no-home-init")]
    no_home_init: bool,

    /// Do not load the local `.opto.tcl` file.
    #[arg(long = "no-local-init")]
    no_local_init: bool,

    /// Maximum number of synthesis worker threads.
    #[arg(long, value_name = "count")]
    threads: Option<NonZeroUsize>,

    /// Control colored terminal output.
    #[arg(long, value_enum, default_value_t = CliColorMode::Auto)]
    color: CliColorMode,

    /// Select the interactive terminal theme.
    #[arg(long, value_enum, default_value_t = CliTheme::Dark)]
    theme: CliTheme,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliTheme {
    Dark,
    Light,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(OptoError::Shell { source, ui }) => {
            print_error(&source, ui);
            std::process::ExitCode::from(1)
        }
        Err(err) => {
            eprintln!("error: {err}");
            std::process::ExitCode::from(1)
        }
    }
}

fn run() -> Result<i32, OptoError> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            err.print().map_err(OptoError::CliOutput)?;
            return Ok(0);
        }
        Err(err) => return Err(OptoError::Cli(err)),
    };

    let shell_args = ShellArgs {
        script: cli.script,
        command: cli.command,
        no_init: cli.no_init,
        no_home_init: cli.no_home_init,
        no_local_init: cli.no_local_init,
        ui: UiOptions {
            color: match cli.color {
                CliColorMode::Auto => ColorMode::Auto,
                CliColorMode::Always => ColorMode::Always,
                CliColorMode::Never => ColorMode::Never,
            },
            theme: match cli.theme {
                CliTheme::Dark => Theme::Dark,
                CliTheme::Light => Theme::Light,
            },
        },
    };

    let session = Session::with_config(SessionConfig {
        max_threads: cli.threads.map(NonZeroUsize::get),
        synthesis: synthesis_diagnostics_from_environment(),
    })?;
    let ui = shell_args.ui;
    let commands = opto_commands().map_err(|source| OptoError::Shell { source, ui })?;
    Shell::run(shell_args, session, commands).map_err(|source| OptoError::Shell { source, ui })
}

fn opto_commands() -> Result<CommandRegistry, ShellError> {
    let mut registry = CommandRegistry::new();
    registry.register(commands::GET_DB)?;
    registry.register(commands::SET_DB)?;
    registry.register(commands::READ_LIBS)?;
    registry.register(commands::READ_HDL)?;
    registry.register(commands::RESUME)?;
    registry.register(commands::ELABORATE)?;
    registry.register(commands::CHECK_DESIGN)?;
    registry.register(commands::SYNTH)?;
    registry.register(commands::REPORT_AREA)?;
    registry.register(commands::REPORT_QOR)?;
    registry.register(commands::REPORT_RESOURCES)?;
    registry.register(commands::SET_SWITCHING_ACTIVITY)?;
    registry.register(commands::RESET_SWITCHING_ACTIVITY)?;
    registry.register(commands::HELP)?;
    registry.register(commands::ECHO)?;
    registry.register(commands::REDIRECT)?;
    registry.register(commands::WRITE_HDL)?;
    registry.register(commands::SAVE)?;
    registry.register(commands::REPORT_POWER)?;
    registry.register(commands::READ_PARASITICS)?;
    registry.register(commands::REPORT_CLOCK)?;
    registry.register(commands::CHECK_TIMING)?;
    registry.register(commands::REPORT_TIMING)?;
    registry.register_group(commands::SDC)?;
    Ok(registry)
}

fn synthesis_diagnostics_from_environment() -> SynthesisConfig {
    SynthesisConfig {
        diagnostics: SynthesisDiagnostics {
            timing: std::env::var_os("OPTO_DEBUG_TIMING").is_some(),
            joint_cells: std::env::var_os("OPTO_DEBUG_JOINTS").is_some(),
            mfs: std::env::var_os("OPTO_DEBUG_MFS").is_some(),
            check_incremental: std::env::var_os("OPTO_CHECK_INCREMENTAL").is_some(),
        },
    }
}
