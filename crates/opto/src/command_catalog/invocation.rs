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
            return Err(crate::ShellError::command(format!(
                "{command}: value must precede options"
            )));
        }
        if let Some(option) = syntax
            .unsupported_options
            .iter()
            .find(|option| option.name == argument.as_ref())
        {
            return Err(crate::ShellError::command(format!(
                "{command}: option '{}' is not implemented yet",
                option.name
            )));
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
            return Err(crate::ShellError::command(format!(
                "{command}: unexpected option '{raw}' after object list"
            )));
        }
        let Some(option) = syntax.options.iter().find(|option| option.name == raw) else {
            if let Some(option) = syntax
                .unsupported_options
                .iter()
                .find(|option| option.name == raw)
            {
                return Err(crate::ShellError::command(format!(
                    "{command}: option '{}' is not implemented yet",
                    option.name
                )));
            }
            if syntax.options.is_empty() && syntax.unsupported_options.is_empty() {
                if syntax.leading_positionals != 0 {
                    return Err(crate::ShellError::command(format!(
                        "{command}: unsupported option '{raw}'"
                    )));
                }
                positional_indices.push(index);
                saw_trailing_positional = true;
                index += 1;
                continue;
            }
            return Err(crate::ShellError::command(format!(
                "{command}: unsupported option '{raw}'"
            )));
        };
        seen_options.push(option.name);
        let value_index = if let Some(value_hint) = option.value {
            explicit_redirect_target |=
                command == "redirect" && matches!(option.name, "-file" | "-variable");
            index = index.checked_add(1).ok_or_else(|| {
                crate::ShellError::command(format!("{command}: argument count overflow"))
            })?;
            if index == args.len() {
                return Err(crate::ShellError::command(format!(
                    "{command}: missing value for {}",
                    option.name
                )));
            }
            if let ValueHint::OneOf { accepted, .. } = value_hint {
                let value = args[index].as_ref();
                if !accepted.contains(&value) {
                    return Err(crate::ShellError::command(format!(
                        "{command}: value for {} must be {}",
                        option.name,
                        accepted.join(" or ")
                    )));
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
            return Err(crate::ShellError::command(format!(
                "{command}: wrong number of arguments: extra positional option '{}'",
                args[extra].as_ref()
            )));
        }
        if let Some(label) = syntax.positional_label {
            return Err(crate::ShellError::command(format!(
                "{command}: missing {label}"
            )));
        }
        return Err(crate::ShellError::command(format!(
            "{command}: wrong number of arguments"
        )));
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
            return Err(crate::ShellError::command(format!(
                "{command}: missing {required} <value>"
            )));
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
            return Err(crate::ShellError::command(format!(
                "{command}: missing {option} for virtual object"
            )));
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
            return Err(crate::ShellError::command(format!(
                "{command}: {left} and {right} are mutually exclusive"
            )));
        }
    }

    Ok(ParsedLayout {
        options,
        positional_indices,
    })
}
