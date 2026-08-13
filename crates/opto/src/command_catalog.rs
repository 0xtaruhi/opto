// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::sdc::SdcVersion;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

mod invocation;

#[cfg(test)]
pub(super) use invocation::validate_invocation;
pub(super) use invocation::{ParsedInvocation, parse_invocation, validate_sdc_invocation};

pub(super) type ParsedCommandHandler =
    for<'a> fn(
        &crate::runtime::ShellState,
        *mut opto_tcl_sys::ffi::TclInterp,
        &'static str,
        &ParsedInvocation<'a>,
    ) -> Result<crate::command::CommandResult, crate::ShellError>;

#[derive(Debug, Clone, Copy)]
pub(super) struct CommandSpec {
    pub name: &'static str,
    pub executor: ParsedCommandHandler,
    pub sdc_since: Option<SdcVersion>,
    syntax: fn() -> CommandSyntax,
}

/// One public Tcl command that a binary may choose to register.
#[derive(Debug, Clone, Copy)]
pub struct CommandDefinition {
    name: &'static str,
    definitions: fn() -> &'static [CommandSpec],
}

impl CommandDefinition {
    pub(crate) const fn new(
        name: &'static str,
        definitions: fn() -> &'static [CommandSpec],
    ) -> Self {
        Self { name, definitions }
    }

    fn spec(self) -> &'static CommandSpec {
        (self.definitions)()
            .iter()
            .find(|spec| spec.name == self.name)
            .unwrap_or_else(|| panic!("command definition '{}' has no matching spec", self.name))
    }
}

/// A reusable batch of individually selectable Tcl commands.
#[derive(Debug, Clone, Copy)]
pub struct CommandGroup {
    commands: &'static [CommandDefinition],
}

impl CommandGroup {
    /// Creates a group from command definitions.
    #[must_use]
    pub const fn new(commands: &'static [CommandDefinition]) -> Self {
        Self { commands }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RegisteredCommand {
    spec: &'static CommandSpec,
    syntax: CommandSyntax,
}

impl RegisteredCommand {
    pub(crate) fn spec(&self) -> &'static CommandSpec {
        self.spec
    }

    pub(crate) fn syntax(&self) -> &CommandSyntax {
        &self.syntax
    }
}

/// The exact Tcl command surface selected by one executable.
#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    commands: Vec<RegisteredCommand>,
    by_name: BTreeMap<&'static str, usize>,
}

impl CommandRegistry {
    /// Creates an empty command registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one public Tcl command.
    pub fn register(&mut self, command: CommandDefinition) -> Result<&mut Self, crate::ShellError> {
        self.register_definitions(std::slice::from_ref(&command))?;
        Ok(self)
    }

    /// Registers every command in a reusable group.
    pub fn register_group(&mut self, group: CommandGroup) -> Result<&mut Self, crate::ShellError> {
        self.register_definitions(group.commands)?;
        Ok(self)
    }

    fn register_definitions(
        &mut self,
        definitions: &[CommandDefinition],
    ) -> Result<(), crate::ShellError> {
        let mut pending_names = BTreeSet::new();
        let mut pending = Vec::with_capacity(definitions.len());
        for definition in definitions {
            let spec = definition.spec();
            if self.by_name.contains_key(spec.name) || !pending_names.insert(spec.name) {
                return Err(crate::ShellError::command(format!(
                    "command '{}' is already registered",
                    spec.name
                )));
            }
            pending.push(RegisteredCommand {
                spec,
                syntax: spec.command_syntax(),
            });
        }
        for command in pending {
            self.by_name.insert(command.spec.name, self.commands.len());
            self.commands.push(command);
        }
        Ok(())
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &RegisteredCommand> {
        self.commands.iter()
    }

    pub(crate) fn find(&self, name: &str) -> Option<&RegisteredCommand> {
        self.by_name.get(name).map(|index| &self.commands[*index])
    }

    pub(crate) fn help_text(&self) -> String {
        let mut text = String::from("Opto commands:");
        for commands in self.commands.chunks(8) {
            text.push_str("\n  ");
            for (index, command) in commands.iter().enumerate() {
                if index != 0 {
                    text.push(' ');
                }
                text.push_str(command.spec.name);
            }
        }
        text
    }

    pub(crate) fn command_help_text(&self, name: &str) -> Option<String> {
        let command = self.find(name)?;
        Some(format_command_help(command.spec.name, &command.syntax))
    }
}

impl CommandSpec {
    pub(crate) const fn typed(
        name: &'static str,
        handler: ParsedCommandHandler,
        sdc_since: Option<SdcVersion>,
        syntax: fn() -> CommandSyntax,
    ) -> Self {
        Self {
            name,
            executor: handler,
            sdc_since,
            syntax,
        }
    }

    fn command_syntax(&self) -> CommandSyntax {
        (self.syntax)()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueHint {
    Text,
    File,
    Directory,
    Design,
    Port,
    Cell,
    Pin,
    Net,
    Clock,
    OneOf {
        accepted: &'static [&'static str],
        suggested: &'static [&'static str],
    },
    Suggested(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OptionId {
    Untracked,
    Derived(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OptionHint {
    id: OptionId,
    pub name: &'static str,
    pub value: Option<ValueHint>,
}

#[derive(Debug, Clone)]
pub(super) struct CommandSyntax {
    pub options: Vec<OptionHint>,
    pub unsupported_options: Vec<OptionHint>,
    pub positional: Option<ValueHint>,
    pub positional_label: Option<&'static str>,
    pub positional_arity: Option<PositionalArity>,
    pub sdc_positional_arity: Option<PositionalArity>,
    pub required_options: &'static [&'static str],
    pub option_or_positional: Option<&'static str>,
    pub leading_positionals: usize,
    pub mutually_exclusive_options: &'static [&'static [OptionId]],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PositionalArity {
    pub min: usize,
    pub max: usize,
}

impl PositionalArity {
    pub(crate) const fn exactly(count: usize) -> Self {
        Self {
            min: count,
            max: count,
        }
    }

    pub(crate) const fn range(min: usize, max: usize) -> Self {
        Self { min, max }
    }

    const fn accepts(self, count: usize) -> bool {
        count >= self.min && count <= self.max
    }
}

pub(crate) const fn typed_flag(id: OptionId, name: &'static str) -> OptionHint {
    OptionHint {
        id,
        name,
        value: None,
    }
}

pub(crate) const fn typed_value(id: OptionId, name: &'static str, hint: ValueHint) -> OptionHint {
    OptionHint {
        id,
        name,
        value: Some(hint),
    }
}

pub(super) fn available_in_sdc(spec: &CommandSpec, version: SdcVersion) -> bool {
    spec.sdc_since.is_some_and(|since| since <= version)
}

fn format_command_help(name: &str, syntax: &CommandSyntax) -> String {
    let mut text = format!(
        "Command: {name}\n\nSummary:\n  {}\n\nUsage:\n  {}",
        command_summary(name),
        command_usage(name, syntax),
    );
    if let Some(positional) = syntax.positional {
        write!(
            text,
            "\n\nArguments:\n  <{}> — {}",
            value_hint_label(positional),
            positional_description(positional),
        )
        .expect("writing to a String cannot fail");
    }
    if !syntax.options.is_empty() || !syntax.unsupported_options.is_empty() {
        text.push_str("\n\nOptions:");
        for option in &syntax.options {
            let value = option
                .value
                .map_or_else(String::new, |hint| format!(" <{}>", value_hint_label(hint)));
            let required = if syntax.required_options.contains(&option.name) {
                " [required]"
            } else {
                ""
            };
            write!(
                text,
                "\n  {}{value}{required} — {}",
                option.name,
                option_description(option.name, option.value),
            )
            .expect("writing to a String cannot fail");
        }
        for option in &syntax.unsupported_options {
            let value = option
                .value
                .map_or_else(String::new, |hint| format!(" <{}>", value_hint_label(hint)));
            write!(
                text,
                "\n  {}{value} (not implemented) — using it is an error",
                option.name,
            )
            .expect("writing to a String cannot fail");
        }
    }
    write!(
        text,
        "\n\nRequires:\n  {}\n\nExample:\n  {}",
        command_precondition(name),
        command_example(name, syntax),
    )
    .expect("writing to a String cannot fail");
    text
}

fn command_usage(name: &str, syntax: &CommandSyntax) -> String {
    let mut usage = name.to_string();
    if !syntax.options.is_empty() || !syntax.unsupported_options.is_empty() {
        usage.push_str(" [options]");
    }
    if let Some(positional) = syntax.positional {
        let label = value_hint_label(positional).to_ascii_lowercase();
        let arity = syntax
            .positional_arity
            .unwrap_or(PositionalArity::exactly(0));
        if arity.min == 0 {
            write!(usage, " [<{label}>]").expect("writing to a String cannot fail");
        } else {
            write!(usage, " <{label}>").expect("writing to a String cannot fail");
        }
        if arity.max == usize::MAX || arity.max > 1 {
            usage.push_str("...");
        }
    }
    usage
}

fn command_summary(name: &str) -> String {
    let words = name.replace('_', " ");
    if name == "help" {
        return "List registered commands or explain one command's public syntax.".to_string();
    }
    if name == "elaborate" {
        return "Elaborate an ingested HDL definition and make it the current design.".to_string();
    }
    if name == "synth" {
        return "Synthesize the current design through Opto's single mapping pipeline.".to_string();
    }
    if name == "redirect" {
        return "Evaluate a Tcl command and redirect its result to a file or variable.".to_string();
    }
    let (verb, object) = words.split_once(' ').unwrap_or((&words, "session state"));
    let action = match verb {
        "read" => "Read",
        "write" => "Write",
        "report" => "Report",
        "get" => "Query",
        "set" => "Set",
        "unset" | "reset" | "delete" => "Remove",
        "all" => "Return all",
        "check" => "Check",
        "create" => "Create",
        "save" => "Save",
        "resume" => "Restore",
        "source" => "Evaluate",
        "echo" => "Return",
        "exit" => "Exit from",
        _ => "Execute",
    };
    format!("{action} {object} using the documented Opto command contract.")
}

fn positional_description(hint: ValueHint) -> &'static str {
    match hint {
        ValueHint::File => "One or more filesystem paths.",
        ValueHint::Directory => "A filesystem directory.",
        ValueHint::Design => "A design name already known to the session.",
        ValueHint::Port => "A port name or port collection.",
        ValueHint::Cell => "A cell name or cell collection.",
        ValueHint::Pin => "A pin name or pin collection.",
        ValueHint::Net => "A net name or net collection.",
        ValueHint::Clock => "A clock name or clock collection.",
        ValueHint::OneOf { .. } | ValueHint::Suggested(_) => "A value from the displayed set.",
        ValueHint::Text => "A Tcl value required by this command.",
    }
}

fn option_description(name: &str, value: Option<ValueHint>) -> &'static str {
    match name {
        "-rise" => "Apply the operation to rising transitions.",
        "-fall" => "Apply the operation to falling transitions.",
        "-min" => "Select the minimum or early analysis view.",
        "-max" => "Select the maximum or late analysis view.",
        "-from" => "Restrict path starts to the supplied objects.",
        "-through" => "Require paths to pass through the supplied objects in order.",
        "-to" => "Restrict path endpoints to the supplied objects.",
        "-quiet" => "Suppress nonessential command output.",
        "-hierarchical" | "-hierarchy" => "Include hierarchical objects or structure.",
        "-file" => "Use the supplied filesystem path.",
        "-variable" => "Store the result in the supplied Tcl variable.",
        "-append" => "Append instead of replacing existing output.",
        "-period" => "Set the clock period in active library time units.",
        "-name" => "Assign the supplied public object name.",
        _ if value.is_some() => "Provide the typed value shown for this option.",
        _ => "Enable this command behavior.",
    }
}

fn command_precondition(name: &str) -> &'static str {
    match name {
        "read_hdl" | "read_libs" | "read_sdc" | "source" | "resume" => {
            "The referenced input must exist and be readable."
        }
        "elaborate" => "The named definition must have been ingested with read_hdl.",
        "synth" => "A current elaborated design and a non-empty target library are required.",
        name if name.starts_with("report_") || name.starts_with("write_") => {
            "A compatible current design or analysis state must already exist."
        }
        name if name.starts_with("set_")
            || name.starts_with("unset_")
            || name.starts_with("create_")
            || name.starts_with("delete_") =>
        {
            "Referenced objects must resolve in the current session state."
        }
        _ => "No additional precondition beyond the arguments shown above.",
    }
}

fn command_example(name: &str, syntax: &CommandSyntax) -> String {
    match name {
        "read_hdl" => return "read_hdl rtl/top.sv".to_string(),
        "read_libs" => return "read_libs cells.lib".to_string(),
        "elaborate" => return "elaborate top".to_string(),
        "synth" => return "synth".to_string(),
        "create_clock" => return "create_clock -period 10 -name sys_clk".to_string(),
        "report_timing" => return "report_timing -max_paths 10".to_string(),
        "write_hdl" => return "write_hdl mapped.v".to_string(),
        "help" => return "help read_hdl".to_string(),
        _ => {}
    }
    let mut example = name.to_string();
    for required in syntax.required_options {
        if let Some(option) = syntax
            .options
            .iter()
            .find(|option| option.name == *required)
        {
            write!(example, " {}", option.name).expect("writing to a String cannot fail");
            if let Some(value) = option.value {
                write!(example, " {}", example_value(value))
                    .expect("writing to a String cannot fail");
            }
        }
    }
    if let Some(positional) = syntax.positional
        && syntax.positional_arity.is_some_and(|arity| arity.min != 0)
    {
        write!(example, " {}", example_value(positional)).expect("writing to a String cannot fail");
    }
    example
}

fn example_value(hint: ValueHint) -> &'static str {
    match hint {
        ValueHint::File => "path.ext",
        ValueHint::Directory => "directory",
        ValueHint::Design => "top",
        ValueHint::Port => "ports",
        ValueHint::Cell => "cells",
        ValueHint::Pin => "pins",
        ValueHint::Net => "nets",
        ValueHint::Clock => "clocks",
        ValueHint::OneOf { suggested, .. } | ValueHint::Suggested(suggested) => {
            suggested.first().copied().unwrap_or("value")
        }
        ValueHint::Text => "value",
    }
}

fn value_hint_label(hint: ValueHint) -> String {
    match hint {
        ValueHint::OneOf { suggested, .. } | ValueHint::Suggested(suggested) => suggested.join("|"),
        _ => format!("{hint:?}"),
    }
}

#[cfg(test)]
mod tests;
