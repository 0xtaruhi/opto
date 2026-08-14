// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-field validation for derived Tcl command schemas.

use super::model::{CommandConfig, FieldConfig, Repetition, Shape};
use quote::quote;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn validate_option_or_positional(
    command: &CommandConfig,
    fields: &[FieldConfig<'_>],
) -> syn::Result<()> {
    if let Some(option) = &command.option_or_positional
        && !fields.iter().any(|field| {
            !field.unsupported
                && field
                    .names
                    .iter()
                    .any(|name| name.value() == option.value())
        })
    {
        return Err(syn::Error::new_spanned(
            option,
            "option_or_positional must name a supported option",
        ));
    }
    Ok(())
}

pub(super) fn validate_fields(fields: &[FieldConfig<'_>]) -> syn::Result<()> {
    let mut names = BTreeSet::new();
    let field_names = fields
        .iter()
        .map(|field| (field.ident.to_string(), field.index))
        .collect::<BTreeMap<_, _>>();
    let mut saw_optional = false;
    let mut saw_repeated = false;
    let mut saw_nonleading_positional = false;
    for field in fields {
        validate_field_contract(field)?;
        validate_field_references(field, &mut names, &field_names)?;
        if !field.positional {
            continue;
        }
        if field.before_options {
            if saw_nonleading_positional {
                return Err(syn::Error::new_spanned(
                    field.ident,
                    "before_options positional arguments must form a leading prefix",
                ));
            }
        } else {
            saw_nonleading_positional = true;
        }
        match field.shape {
            Shape::Required if saw_optional || saw_repeated => {
                return Err(syn::Error::new_spanned(
                    field.ident,
                    "required positional arguments must precede optional/repeated arguments",
                ));
            }
            Shape::Optional if saw_repeated => {
                return Err(syn::Error::new_spanned(
                    field.ident,
                    "optional positional arguments must precede repeated arguments",
                ));
            }
            Shape::Optional => saw_optional = true,
            Shape::Repeated if saw_repeated => {
                return Err(syn::Error::new_spanned(
                    field.ident,
                    "only one repeated positional field is allowed",
                ));
            }
            Shape::Repeated => saw_repeated = true,
            Shape::Bool | Shape::Unit => {
                return Err(syn::Error::new_spanned(
                    field.ident,
                    "positional arguments cannot be bool or unit",
                ));
            }
            Shape::Required => {}
        }
    }
    Ok(())
}

pub(super) fn validate_field_contract(field: &FieldConfig<'_>) -> syn::Result<()> {
    if field.positional != field.names.is_empty() {
        return Err(syn::Error::new_spanned(
            field.ident,
            "each argument needs either positional or long attributes",
        ));
    }
    if field.label.is_some() && !field.positional {
        return Err(syn::Error::new_spanned(
            field.ident,
            "argument labels are only valid for positional fields",
        ));
    }
    if field.unsupported && field.positional {
        return Err(syn::Error::new_spanned(
            field.ident,
            "unsupported arguments must be named options",
        ));
    }
    if field.unsupported && field.shape != Shape::Unit {
        return Err(syn::Error::new_spanned(
            field.ty,
            "unsupported option fields must use the unit type ()",
        ));
    }
    if field.before_options && (!field.positional || !matches!(field.shape, Shape::Required)) {
        return Err(syn::Error::new_spanned(
            field.ident,
            "before_options requires a required positional argument",
        ));
    }
    if field.repetition == Repetition::Repeatable
        && (field.positional || !matches!(field.shape, Shape::Repeated))
    {
        return Err(syn::Error::new_spanned(
            field.ident,
            "repeatable requires a repeated named option field",
        ));
    }
    if (field.min.is_some() || field.max.is_some())
        && !(field.positional && matches!(field.shape, Shape::Repeated))
    {
        return Err(syn::Error::new_spanned(
            field.ident,
            "min/max are only valid for repeated positional arguments",
        ));
    }
    if let (Some(min), Some(max)) = (field.min, field.max)
        && min > max
    {
        return Err(syn::Error::new_spanned(
            field.ident,
            "positional min cannot exceed max",
        ));
    }
    Ok(())
}

pub(super) fn validate_field_references(
    field: &FieldConfig<'_>,
    names: &mut BTreeSet<String>,
    field_names: &BTreeMap<String, usize>,
) -> syn::Result<()> {
    for name in &field.names {
        if !name.value().starts_with('-') {
            return Err(syn::Error::new_spanned(
                name,
                "option names must start with '-'",
            ));
        }
        if !names.insert(name.value()) {
            return Err(syn::Error::new_spanned(name, "duplicate option name"));
        }
    }
    for conflict in &field.conflicts_with {
        if !field_names.contains_key(&conflict.to_string()) {
            return Err(syn::Error::new_spanned(
                conflict,
                "conflicts_with names an unknown field",
            ));
        }
    }
    Ok(())
}

pub(super) fn conflict_groups(
    fields: &[FieldConfig<'_>],
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let by_name = fields
        .iter()
        .map(|field| (field.ident.to_string(), field))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut groups = Vec::new();
    for field in fields {
        for conflict in &field.conflicts_with {
            let other = by_name.get(&conflict.to_string()).ok_or_else(|| {
                syn::Error::new_spanned(conflict, "conflicts_with names an unknown field")
            })?;
            let pair = if field.index < other.index {
                (field.index, other.index)
            } else {
                (other.index, field.index)
            };
            if seen.insert(pair) {
                let left = field.id_tokens();
                let right = other.id_tokens();
                groups.push(quote!(&[#left, #right]));
            }
        }
    }
    Ok(groups)
}
