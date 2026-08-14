// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{CommandSyntax, OptionId, PositionalLexeme, RegisteredCommand, ValueHint};
use crate::tcl::TclArg;

#[derive(Clone, Copy)]
pub(crate) struct InvocationOption<'a> {
    id: OptionId,
    name: &'static str,
    value: Option<TclArg<'a>>,
}

/// One schema-validated command invocation.
///
/// Options retain Tcl object identity so collection and list arguments can be
/// consumed without converting them back from strings in command handlers.
pub(crate) struct ParsedInvocation<'a> {
    options: Vec<InvocationOption<'a>>,
    positionals: Vec<TclArg<'a>>,
}

impl<'a> ParsedInvocation<'a> {
    pub(crate) fn has_option(&self, id: OptionId) -> bool {
        self.options.iter().any(|option| option.id == id)
    }

    pub(crate) fn option_values<'b>(
        &'b self,
        id: OptionId,
    ) -> impl Iterator<Item = TclArg<'a>> + 'b {
        self.options
            .iter()
            .filter_map(move |option| (option.id == id).then_some(option.value).flatten())
    }

    pub(crate) fn option_occurrences<'b>(
        &'b self,
        id: OptionId,
    ) -> impl Iterator<Item = (&'static str, TclArg<'a>)> + 'b {
        self.options
            .iter()
            .filter_map(move |option| (option.id == id).then_some((option.name, option.value?)))
    }

    pub(crate) fn last_option(&self, id: OptionId) -> Option<(&'static str, TclArg<'a>)> {
        self.options
            .iter()
            .rev()
            .find_map(|option| (option.id == id).then_some((option.name, option.value?)))
    }

    pub(crate) fn positionals(&self) -> &[TclArg<'a>] {
        &self.positionals
    }
}

struct ParsedOptionIndex {
    id: OptionId,
    name: &'static str,
    value_index: Option<usize>,
}

struct ParsedLayout {
    options: Vec<ParsedOptionIndex>,
    positional_indices: Vec<usize>,
}

struct LayoutParser<'a, T> {
    command: &'a str,
    args: &'a [T],
    syntax: &'a CommandSyntax,
    sdc: bool,
    index: usize,
    options: Vec<ParsedOptionIndex>,
    positional_indices: Vec<usize>,
    seen_option_names: Vec<&'static str>,
    seen_option_ids: Vec<OptionId>,
}

pub(crate) fn parse_invocation<'a>(
    command: &RegisteredCommand,
    args: &[TclArg<'a>],
    sdc: bool,
) -> Result<ParsedInvocation<'a>, crate::ShellError> {
    let layout = parse_layout(command.spec().name, args, command.syntax(), sdc)?;
    Ok(ParsedInvocation {
        options: layout
            .options
            .into_iter()
            .map(|option| InvocationOption {
                id: option.id,
                name: option.name,
                value: option.value_index.map(|index| args[index]),
            })
            .collect(),
        positionals: layout
            .positional_indices
            .into_iter()
            .map(|index| args[index])
            .collect(),
    })
}

#[cfg(test)]
pub(crate) fn validate_invocation<T: AsRef<str>>(
    command: &RegisteredCommand,
    args: &[T],
) -> Result<(), crate::ShellError> {
    parse_layout(command.spec().name, args, command.syntax(), false).map(|_| ())
}

pub(crate) fn validate_sdc_invocation<T: AsRef<str>>(
    command: &RegisteredCommand,
    args: &[T],
) -> Result<(), crate::ShellError> {
    parse_layout(command.spec().name, args, command.syntax(), true).map(|_| ())
}

fn parse_layout<T: AsRef<str>>(
    command: &str,
    args: &[T],
    syntax: &CommandSyntax,
    sdc: bool,
) -> Result<ParsedLayout, crate::ShellError> {
    LayoutParser::new(command, args, syntax, sdc).parse()
}

impl<'a, T: AsRef<str>> LayoutParser<'a, T> {
    fn new(command: &'a str, args: &'a [T], syntax: &'a CommandSyntax, sdc: bool) -> Self {
        Self {
            command,
            args,
            syntax,
            sdc,
            index: 0,
            options: Vec::new(),
            positional_indices: Vec::new(),
            seen_option_names: Vec::new(),
            seen_option_ids: Vec::new(),
        }
    }

    fn parse(mut self) -> Result<ParsedLayout, crate::ShellError> {
        self.parse_leading_positionals()?;
        self.parse_options_and_positionals()?;
        self.validate_schema_contracts()?;
        Ok(ParsedLayout {
            options: self.options,
            positional_indices: self.positional_indices,
        })
    }

    fn parse_leading_positionals(&mut self) -> Result<(), crate::ShellError> {
        for _ in 0..self.syntax.leading_positionals() {
            let Some(argument) = self.args.get(self.index) else {
                break;
            };
            if self
                .syntax
                .options
                .iter()
                .any(|option| option.name == argument.as_ref())
            {
                return Err(self.error(
                    format!("{}: value must precede options", self.command),
                    None,
                ));
            }
            if let Some(option) = self
                .syntax
                .unsupported_options
                .iter()
                .find(|option| option.name == argument.as_ref())
            {
                return Err(self.unsupported_option(option.name));
            }
            self.positional_indices.push(self.index);
            self.index += 1;
        }
        Ok(())
    }

    fn parse_options_and_positionals(&mut self) -> Result<(), crate::ShellError> {
        let mut saw_trailing_positional = false;
        let mut options_terminated = false;
        while self.index < self.args.len() {
            let raw = self.args[self.index].as_ref();
            if !options_terminated && raw == "--" {
                options_terminated = true;
                self.index += 1;
                continue;
            }
            let negative_numeric = raw.starts_with('-')
                && self
                    .syntax
                    .positional_at(self.positional_indices.len())
                    .is_some_and(|positional| {
                        positional.lexeme == PositionalLexeme::Numeric && raw.parse::<f64>().is_ok()
                    });
            if options_terminated || !raw.starts_with('-') || negative_numeric {
                self.positional_indices.push(self.index);
                saw_trailing_positional = true;
                self.index += 1;
                continue;
            }
            if self.syntax.leading_positionals() != 0 && saw_trailing_positional {
                return Err(self.error(
                    format!(
                        "{}: unexpected option '{raw}' after object list",
                        self.command
                    ),
                    None,
                ));
            }
            let Some(option) = self.syntax.options.iter().find(|option| option.name == raw) else {
                if let Some(option) = self
                    .syntax
                    .unsupported_options
                    .iter()
                    .find(|option| option.name == raw)
                {
                    return Err(self.unsupported_option(option.name));
                }
                if self.syntax.options.is_empty() && self.syntax.unsupported_options.is_empty() {
                    if self.syntax.leading_positionals() != 0 {
                        return Err(self.unsupported_option_error(raw));
                    }
                    self.positional_indices.push(self.index);
                    saw_trailing_positional = true;
                    self.index += 1;
                    continue;
                }
                return Err(self.unsupported_option_error(raw));
            };
            if !option.repeatable
                && self
                    .options
                    .iter()
                    .any(|seen: &ParsedOptionIndex| seen.id == option.id)
            {
                return Err(self.error(
                    format!(
                        "{}: option '{}' may be specified only once",
                        self.command, option.name
                    ),
                    None,
                ));
            }
            self.seen_option_names.push(option.name);
            self.seen_option_ids.push(option.id);
            let value_index = if let Some(value_hint) = option.value {
                self.index = self.index.checked_add(1).ok_or_else(|| {
                    self.error(format!("{}: argument count overflow", self.command), None)
                })?;
                if self.index == self.args.len() {
                    return Err(self.error(
                        format!("{}: missing value for {}", self.command, option.name),
                        None,
                    ));
                }
                if let ValueHint::OneOf { accepted, .. } = value_hint {
                    let value = self.args[self.index].as_ref();
                    if !accepted.contains(&value) {
                        return Err(self.error(
                            format!(
                                "{}: value for {} must be {}",
                                self.command,
                                option.name,
                                accepted.join(" or ")
                            ),
                            None,
                        ));
                    }
                }
                Some(self.index)
            } else {
                None
            };
            self.options.push(ParsedOptionIndex {
                id: option.id,
                name: option.name,
                value_index,
            });
            self.index += 1;
        }
        Ok(())
    }

    fn validate_schema_contracts(&self) -> Result<(), crate::ShellError> {
        validate_positional_count(
            self.command,
            self.args,
            self.syntax,
            self.sdc,
            &self.seen_option_ids,
            &self.positional_indices,
        )?;
        self.validate_required_options()?;
        self.validate_option_or_positional()?;
        self.validate_exclusive_options()
    }

    fn validate_required_options(&self) -> Result<(), crate::ShellError> {
        for required in self.syntax.required_options {
            let required_hint = self
                .syntax
                .options
                .iter()
                .find(|option| option.name == *required)
                .expect("required option must be part of command syntax");
            let present = if required_hint.id == OptionId::Untracked {
                self.seen_option_names.contains(required)
            } else {
                self.options
                    .iter()
                    .any(|option| option.id == required_hint.id)
            };
            if !present {
                return Err(self.error(
                    format!("{}: missing {required} <value>", self.command),
                    None,
                ));
            }
        }
        Ok(())
    }

    fn validate_option_or_positional(&self) -> Result<(), crate::ShellError> {
        let Some(option) = self.syntax.option_or_positional else {
            return Ok(());
        };
        if !self.positional_indices.is_empty() {
            return Ok(());
        }
        let option_hint = self
            .syntax
            .options
            .iter()
            .find(|hint| hint.name == option)
            .expect("option-or-positional must be part of command syntax");
        let present = if option_hint.id == OptionId::Untracked {
            self.seen_option_names.contains(&option)
        } else {
            self.options.iter().any(|seen| seen.id == option_hint.id)
        };
        if !present {
            return Err(self.error(
                format!("{}: missing {option} for virtual object", self.command),
                None,
            ));
        }
        Ok(())
    }

    fn validate_exclusive_options(&self) -> Result<(), crate::ShellError> {
        for group in self.syntax.mutually_exclusive_options {
            let mut present = group.iter().filter_map(|id| {
                self.options
                    .iter()
                    .find(|option| option.id == *id)
                    .map(|option| option.name)
            });
            if let (Some(left), Some(right)) = (present.next(), present.next()) {
                return Err(self.error(
                    format!(
                        "{}: {left} and {right} are mutually exclusive",
                        self.command
                    ),
                    None,
                ));
            }
        }
        Ok(())
    }

    fn unsupported_option(&self, option: &str) -> crate::ShellError {
        self.error(
            format!("{}: option '{option}' is not implemented yet", self.command),
            None,
        )
    }

    fn unsupported_option_error(&self, option: &str) -> crate::ShellError {
        self.error(
            format!("{}: unsupported option '{option}'", self.command),
            Some(option),
        )
    }

    fn error(&self, message: String, unknown_option: Option<&str>) -> crate::ShellError {
        invocation_error(self.command, message, self.syntax, unknown_option)
    }
}

fn validate_positional_count<T: AsRef<str>>(
    command: &str,
    args: &[T],
    syntax: &CommandSyntax,
    sdc: bool,
    seen_options: &[OptionId],
    positionals: &[usize],
) -> Result<(), crate::ShellError> {
    let Some(arity) = syntax.positional_arity(seen_options, sdc) else {
        return Ok(());
    };
    if arity.accepts(positionals.len()) {
        return Ok(());
    }
    if let Some(&extra) = positionals.get(arity.max) {
        return Err(invocation_error(
            command,
            format!(
                "{command}: wrong number of arguments: extra positional option '{}'",
                args[extra].as_ref()
            ),
            syntax,
            None,
        ));
    }
    if let Some(label) = syntax
        .positional_at(positionals.len())
        .map(|positional| positional.name)
    {
        return Err(invocation_error(
            command,
            format!("{command}: wrong number of arguments: missing {label}"),
            syntax,
            None,
        ));
    }
    Err(invocation_error(
        command,
        format!("{command}: wrong number of arguments"),
        syntax,
        None,
    ))
}

fn invocation_error(
    command: &str,
    message: String,
    syntax: &CommandSyntax,
    unknown_option: Option<&str>,
) -> crate::ShellError {
    let suggestion = unknown_option.and_then(|unknown| closest_option(unknown, syntax));
    let help = suggestion.map_or_else(
        || format!("run 'help {command}' to see accepted arguments and examples"),
        |suggestion| {
            format!(
                "did you mean '{suggestion}'? Run 'help {command}' to see accepted arguments and examples"
            )
        },
    );
    crate::ShellError::usage(message, help)
}

fn closest_option<'a>(unknown: &str, syntax: &'a CommandSyntax) -> Option<&'a str> {
    syntax
        .options
        .iter()
        .chain(&syntax.unsupported_options)
        .map(|option| (edit_distance(unknown, option.name), option.name))
        .min_by_key(|(distance, option)| (*distance, *option))
        .filter(|(distance, _)| *distance <= 2)
        .map(|(_, option)| option)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.chars().count()).collect::<Vec<_>>();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(previous.len());
        current.push(left_index + 1);
        for (right_index, right_char) in right.chars().enumerate() {
            current.push(
                (previous[right_index + 1] + 1)
                    .min(current[right_index] + 1)
                    .min(previous[right_index] + usize::from(left_char != right_char)),
            );
        }
        previous = current;
    }
    previous.last().copied().unwrap_or(left.chars().count())
}
