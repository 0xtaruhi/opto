// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{CommandSyntax, OptionId, PositionalArity, RegisteredCommand, ValueHint};
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
    let mut index = 0usize;
    let mut options = Vec::new();
    let mut positional_indices = Vec::new();
    let mut explicit_redirect_target = false;
    let mut seen_options = Vec::new();
    for _ in 0..syntax.leading_positionals {
        let Some(argument) = args.get(index) else {
            break;
        };
        if syntax
            .options
            .iter()
            .any(|option| option.name == argument.as_ref())
        {
            return Err(invocation_error(
                command,
                format!("{command}: value must precede options"),
                syntax,
                None,
            ));
        }
        if let Some(option) = syntax
            .unsupported_options
            .iter()
            .find(|option| option.name == argument.as_ref())
        {
            return Err(invocation_error(
                command,
                format!("{command}: option '{}' is not implemented yet", option.name),
                syntax,
                None,
            ));
        }
        positional_indices.push(index);
        index += 1;
    }
    let mut saw_trailing_positional = false;
    while index < args.len() {
        let raw = args[index].as_ref();
        if !raw.starts_with('-') {
            positional_indices.push(index);
            saw_trailing_positional = true;
            index += 1;
            continue;
        }
        if syntax.leading_positionals != 0 && saw_trailing_positional {
            return Err(invocation_error(
                command,
                format!("{command}: unexpected option '{raw}' after object list"),
                syntax,
                None,
            ));
        }
        let Some(option) = syntax.options.iter().find(|option| option.name == raw) else {
            if let Some(option) = syntax
                .unsupported_options
                .iter()
                .find(|option| option.name == raw)
            {
                return Err(invocation_error(
                    command,
                    format!("{command}: option '{}' is not implemented yet", option.name),
                    syntax,
                    None,
                ));
            }
            if syntax.options.is_empty() && syntax.unsupported_options.is_empty() {
                if syntax.leading_positionals != 0 {
                    return Err(invocation_error(
                        command,
                        format!("{command}: unsupported option '{raw}'"),
                        syntax,
                        Some(raw),
                    ));
                }
                positional_indices.push(index);
                saw_trailing_positional = true;
                index += 1;
                continue;
            }
            return Err(invocation_error(
                command,
                format!("{command}: unsupported option '{raw}'"),
                syntax,
                Some(raw),
            ));
        };
        seen_options.push(option.name);
        let value_index = if let Some(value_hint) = option.value {
            explicit_redirect_target |=
                command == "redirect" && matches!(option.name, "-file" | "-variable");
            index = index.checked_add(1).ok_or_else(|| {
                invocation_error(
                    command,
                    format!("{command}: argument count overflow"),
                    syntax,
                    None,
                )
            })?;
            if index == args.len() {
                return Err(invocation_error(
                    command,
                    format!("{command}: missing value for {}", option.name),
                    syntax,
                    None,
                ));
            }
            if let ValueHint::OneOf { accepted, .. } = value_hint {
                let value = args[index].as_ref();
                if !accepted.contains(&value) {
                    return Err(invocation_error(
                        command,
                        format!(
                            "{command}: value for {} must be {}",
                            option.name,
                            accepted.join(" or ")
                        ),
                        syntax,
                        None,
                    ));
                }
            }
            Some(index)
        } else {
            None
        };
        options.push(ParsedOptionIndex {
            id: option.id,
            name: option.name,
            value_index,
        });
        index += 1;
    }

    let positional_arity = if command == "redirect" {
        Some(PositionalArity::exactly(if explicit_redirect_target {
            1
        } else {
            2
        }))
    } else if sdc {
        syntax.sdc_positional_arity.or(syntax.positional_arity)
    } else {
        syntax.positional_arity
    };
    if let Some(arity) = positional_arity
        && !arity.accepts(positional_indices.len())
    {
        if let Some(&extra) = positional_indices.get(arity.max) {
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
        if let Some(label) = syntax.positional_label {
            return Err(invocation_error(
                command,
                format!("{command}: missing {label}"),
                syntax,
                None,
            ));
        }
        return Err(invocation_error(
            command,
            format!("{command}: wrong number of arguments"),
            syntax,
            None,
        ));
    }
    for required in syntax.required_options {
        let required_hint = syntax
            .options
            .iter()
            .find(|option| option.name == *required)
            .expect("required option must be part of command syntax");
        let present = if required_hint.id == OptionId::Untracked {
            seen_options.contains(required)
        } else {
            options.iter().any(|option| option.id == required_hint.id)
        };
        if !present {
            return Err(invocation_error(
                command,
                format!("{command}: missing {required} <value>"),
                syntax,
                None,
            ));
        }
    }
    if let Some(option) = syntax.option_or_positional
        && positional_indices.is_empty()
    {
        let option_hint = syntax
            .options
            .iter()
            .find(|hint| hint.name == option)
            .expect("option-or-positional must be part of command syntax");
        let present = if option_hint.id == OptionId::Untracked {
            seen_options.contains(&option)
        } else {
            options.iter().any(|seen| seen.id == option_hint.id)
        };
        if !present {
            return Err(invocation_error(
                command,
                format!("{command}: missing {option} for virtual object"),
                syntax,
                None,
            ));
        }
    }
    for group in syntax.mutually_exclusive_options {
        let mut present = group.iter().filter_map(|id| {
            options
                .iter()
                .find(|option| option.id == *id)
                .map(|option| option.name)
        });
        if let (Some(left), Some(right)) = (present.next(), present.next()) {
            return Err(invocation_error(
                command,
                format!("{command}: {left} and {right} are mutually exclusive"),
                syntax,
                None,
            ));
        }
    }

    Ok(ParsedLayout {
        options,
        positional_indices,
    })
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
