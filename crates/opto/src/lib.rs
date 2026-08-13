// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Product shell for the single `opto` executable.
//!
//! [`Shell`] combines the Tcl interpreter, command catalog, synthesis
//! [`Session`](opto_session::Session), diagnostics, and terminal presentation.
//! Batch scripts and the interactive UI execute the same command handlers; UI
//! choices such as [`ColorMode`] and [`Theme`] never alter command semantics or
//! report contents.
//!
//! The executable parses command-line arguments directly through its typed
//! `Cli::try_parse()` entry point. Unsupported commands and options are reported
//! explicitly instead of routing through compatibility fallbacks.

#![allow(
    clippy::wildcard_imports,
    reason = "command modules intentionally consume the closed command prelude defined by their parent"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "the command dispatcher transfers ownership of each macro-parsed argument record to its handler"
)]
#![allow(
    clippy::struct_excessive_bools,
    reason = "typed command records preserve independent command switches without flag packing"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "public shell entry points share the documented ShellError command and source-diagnostic model"
)]
#![allow(
    clippy::unnecessary_wraps,
    reason = "command handlers use one fallible ABI so macro-generated dispatch remains uniform"
)]
#![allow(
    clippy::similar_names,
    reason = "the Tcl callback adapter retains the native interp/objc/objv terminology"
)]

mod command;
mod command_args;
mod command_catalog;
pub mod commands;
mod diagnostic;
mod error;
mod presentation;
mod redirect;
mod runtime;
mod sdc;
mod tcl;
mod ui;

pub use command_catalog::{CommandDefinition, CommandGroup, CommandRegistry};
pub use diagnostic::print_error;
pub use error::ShellError;
pub use runtime::{Shell, ShellArgs, validate_sdc_syntax};
pub use ui::{ColorMode, Theme, UiOptions};

#[cfg(test)]
use command::EvalResult;
#[cfg(test)]
use command::synthesis::synthesis_event_text;
#[cfg(test)]
use opto_session::{Session, SynthesisEffort, SynthesisEvent};
#[cfg(test)]
use runtime::Runtime;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
#[path = "../tests/support/tcl.rs"]
mod test_tcl;
#[cfg(test)]
use test_tcl::{tcl_path_text, tcl_path_word};
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod sdc_tests;
