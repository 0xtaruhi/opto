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
    pub validation: ValidationBehavior,
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
        metadata: CommandMetadata,
        syntax: fn() -> CommandSyntax,
    ) -> Self {
        Self {
            name,
            executor: handler,
            sdc_since,
            summary: metadata.summary,
            requires: metadata.requires,
            example: metadata.example,
            validation: metadata.validation,
            syntax,
        }
    }

    fn command_syntax(&self) -> CommandSyntax {
        (self.syntax)()
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CommandMetadata {
    pub summary: &'static str,
    pub requires: &'static str,
    pub example: Option<&'static str>,
    pub validation: ValidationBehavior,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ValidationBehavior {
    Noop,
    SourceFile,
    ReturnFromScript,
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
    help: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PositionalLexeme {
    Text,
    Numeric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PositionalHint {
    pub name: &'static str,
    pub value: ValueHint,
    pub lexeme: PositionalLexeme,
    pub min: usize,
    pub max: usize,
    pub before_options: bool,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PositionalPolicy {
    Declared,
    ConditionalOnAnyOption {
        options: &'static [OptionId],
        present: PositionalArity,
        absent: PositionalArity,
    },
}

#[derive(Debug, Clone)]
pub(super) struct CommandSyntax {
    pub options: Vec<OptionHint>,
    pub unsupported_options: Vec<OptionHint>,
    pub positionals: Vec<PositionalHint>,
    pub positional_arity: Option<PositionalArity>,
    pub sdc_positional_arity: Option<PositionalArity>,
    pub required_options: &'static [&'static str],
    pub option_or_positional: Option<&'static str>,
    pub positional_policy: PositionalPolicy,
    pub mutually_exclusive_options: &'static [&'static [OptionId]],
}

impl CommandSyntax {
    fn leading_positionals(&self) -> usize {
        self.positionals
            .iter()
            .take_while(|positional| positional.before_options)
            .count()
    }

    pub(crate) fn positional_at(&self, index: usize) -> Option<&PositionalHint> {
        let mut start = 0usize;
        for positional in &self.positionals {
            let end = start.saturating_add(positional.max);
            if index < end {
                return Some(positional);
            }
            start = end;
        }
        None
    }

    fn positional_arity(&self, seen_options: &[OptionId], sdc: bool) -> Option<PositionalArity> {
        if sdc && self.sdc_positional_arity.is_some() {
            return self.sdc_positional_arity;
        }
        match self.positional_policy {
            PositionalPolicy::Declared => self.positional_arity,
            PositionalPolicy::ConditionalOnAnyOption {
                options,
                present,
                absent,
            } => Some(if seen_options.iter().any(|seen| options.contains(seen)) {
                present
            } else {
                absent
            }),
        }
    }
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

pub(crate) const fn typed_flag(id: OptionId, name: &'static str, help: &'static str) -> OptionHint {
    OptionHint {
        id,
        name,
        value: None,
        repeatable: false,
        help,
    }
}

pub(crate) const fn typed_value(
    id: OptionId,
    name: &'static str,
    hint: ValueHint,
    help: &'static str,
) -> OptionHint {
    OptionHint {
        id,
        name,
        value: Some(hint),
        repeatable: false,
        help,
    }
}

pub(crate) const fn typed_repeated_value(
    id: OptionId,
    name: &'static str,
    hint: ValueHint,
    help: &'static str,
) -> OptionHint {
    OptionHint {
        id,
        name,
        value: Some(hint),
        repeatable: true,
        help,
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
    if !syntax.positionals.is_empty() {
        text.push_str("\n\nArguments:");
        for positional in &syntax.positionals {
            write!(text, "\n  <{}> — {}", positional.name, positional.help)
                .expect("writing to a String cannot fail");
        }
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
                option.name, option.help,
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
    for positional in &syntax.positionals {
        if positional.min == 0 {
            write!(usage, " [<{}>]", positional.name).expect("writing to a String cannot fail");
        } else {
            write!(usage, " <{}>", positional.name).expect("writing to a String cannot fail");
        }
        if positional.max == usize::MAX || positional.max > 1 {
            usage.push_str("...");
        }
    }
    if let PositionalPolicy::ConditionalOnAnyOption {
        options,
        present,
        absent,
    } = syntax.positional_policy
    {
        let option_names = options
            .iter()
            .filter_map(|id| {
                syntax
                    .options
                    .iter()
                    .find(|option| option.id == *id)
                    .map(|option| option.name)
            })
            .collect::<Vec<_>>()
            .join(" or ");
        write!(
            usage,
            " ({} with {option_names}; {} otherwise)",
            positional_arity_label(present),
            positional_arity_label(absent),
        )
        .expect("writing to a String cannot fail");
    }
    usage
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
    let minimum = match syntax.positional_policy {
        PositionalPolicy::Declared => syntax.positionals.iter().map(|hint| hint.min).sum(),
        PositionalPolicy::ConditionalOnAnyOption { absent, .. } => absent.min,
    };
    for index in 0..minimum {
        let hint = syntax
            .positional_at(index)
            .or_else(|| syntax.positionals.last())
            .expect("a positive positional arity has a positional hint");
        write!(example, " {}", example_value(hint.value)).expect("writing to a String cannot fail");
    }
    example
}

fn positional_arity_label(arity: PositionalArity) -> String {
    if arity.min == arity.max {
        let suffix = if arity.min == 1 { "" } else { "s" };
        return format!("{} positional{suffix}", arity.min);
    }
    if arity.max == usize::MAX {
        return format!("at least {} positionals", arity.min);
    }
    format!("{} to {} positionals", arity.min, arity.max)
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
