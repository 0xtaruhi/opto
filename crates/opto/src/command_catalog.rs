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
    summary: &'static str,
    requires: &'static str,
    example: Option<&'static str>,
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
        Some(format_command_help(command.spec, &command.syntax))
    }
}

impl CommandSpec {
    pub(crate) const fn typed(
        name: &'static str,
        handler: ParsedCommandHandler,
        sdc_since: Option<SdcVersion>,
        summary: &'static str,
        requires: &'static str,
        example: Option<&'static str>,
        syntax: fn() -> CommandSyntax,
    ) -> Self {
        Self {
            name,
            executor: handler,
            sdc_since,
            summary,
            requires,
            example,
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
    repeatable: bool,
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
        repeatable: false,
    }
}

pub(crate) const fn typed_value(id: OptionId, name: &'static str, hint: ValueHint) -> OptionHint {
    OptionHint {
        id,
        name,
        value: Some(hint),
        repeatable: false,
    }
}

pub(crate) const fn typed_repeated_value(
    id: OptionId,
    name: &'static str,
    hint: ValueHint,
) -> OptionHint {
    OptionHint {
        id,
        name,
        value: Some(hint),
        repeatable: true,
    }
}

pub(super) fn available_in_sdc(spec: &CommandSpec, version: SdcVersion) -> bool {
    spec.sdc_since.is_some_and(|since| since <= version)
}

fn format_command_help(spec: &CommandSpec, syntax: &CommandSyntax) -> String {
    let name = spec.name;
    let mut text = format!(
        "Command: {name}\n\nSummary:\n  {}\n\nUsage:\n  {}",
        spec.summary,
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
                option_description(option.value),
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
        spec.requires,
        spec.example
            .map_or_else(|| command_example(name, syntax), str::to_owned),
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

fn option_description(value: Option<ValueHint>) -> &'static str {
    match value {
        Some(hint) => positional_description(hint),
        None => "Enable this command behavior.",
    }
}

fn command_example(name: &str, syntax: &CommandSyntax) -> String {
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
