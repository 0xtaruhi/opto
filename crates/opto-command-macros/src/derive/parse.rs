// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Attribute and Rust-type parsing for derived Tcl command schemas.

use super::model::{CommandConfig, FieldConfig, Repetition, Shape};
use quote::format_ident;
use std::collections::BTreeSet;
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, ExprPath, Field, Fields, GenericArgument, LitInt, LitStr, PathArguments,
    Type,
};

pub(super) fn named_fields(
    input: &DeriveInput,
) -> syn::Result<&syn::punctuated::Punctuated<Field, syn::token::Comma>> {
    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => Ok(&fields.named),
            _ => Err(syn::Error::new_spanned(
                input,
                "TclCommand requires a struct with named fields",
            )),
        },
        _ => Err(syn::Error::new_spanned(
            input,
            "TclCommand can only be derived for structs",
        )),
    }
}

pub(super) fn parse_fields(
    fields: &syn::punctuated::Punctuated<Field, syn::token::Comma>,
) -> syn::Result<Vec<FieldConfig<'_>>> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| FieldConfig::parse(index, field))
        .collect()
}

impl CommandConfig {
    pub(super) fn parse(input: &DeriveInput) -> syn::Result<Self> {
        let mut names: Vec<LitStr> = Vec::new();
        let mut handler: Option<ExprPath> = None;
        let mut kinds: Vec<ExprPath> = Vec::new();
        let mut sdc = false;
        let mut sdc_no_positionals = false;
        let mut option_or_positional: Option<LitStr> = None;
        let mut summary: Option<LitStr> = None;
        let mut requires: Option<LitStr> = None;
        let mut example: Option<LitStr> = None;
        let mut variant_summaries = Vec::new();
        let mut variant_requires = Vec::new();
        let mut variant_examples = Vec::new();
        let mut validation: ExprPath =
            syn::parse_quote!(crate::command_catalog::ValidationBehavior::Noop);
        let mut positional_if_any = Vec::new();
        let mut positional_present = None;
        let mut positional_absent = None;
        let mut command_attributes = 0usize;
        for attribute in &input.attrs {
            if !attribute.path().is_ident("command") {
                continue;
            }
            command_attributes += 1;
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") || meta.path.is_ident("variant") {
                    names.push(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("kind") || meta.path.is_ident("variant_kind") {
                    kinds.push(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("handler") {
                    if handler.is_some() {
                        return Err(meta.error("duplicate command handler"));
                    }
                    handler = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("sdc") {
                    sdc = true;
                    Ok(())
                } else if meta.path.is_ident("sdc_no_positionals") {
                    sdc_no_positionals = true;
                    Ok(())
                } else if meta.path.is_ident("option_or_positional") {
                    option_or_positional = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("summary") {
                    summary = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("variant_summary") {
                    variant_summaries.push(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("requires") {
                    requires = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("variant_requires") {
                    variant_requires.push(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("example") {
                    example = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("variant_example") {
                    variant_examples.push(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("validation") {
                    validation = meta.value()?.parse()?;
                    Ok(())
                } else if meta.path.is_ident("positional_if_any") {
                    let names: LitStr = meta.value()?.parse()?;
                    positional_if_any.extend(
                        names
                            .value()
                            .split(',')
                            .map(|name| LitStr::new(name.trim(), names.span())),
                    );
                    Ok(())
                } else if meta.path.is_ident("positional_present") {
                    let value: LitInt = meta.value()?.parse()?;
                    positional_present = Some(value.base10_parse()?);
                    Ok(())
                } else if meta.path.is_ident("positional_absent") {
                    let value: LitInt = meta.value()?.parse()?;
                    positional_absent = Some(value.base10_parse()?);
                    Ok(())
                } else {
                    Err(meta.error("unsupported command attribute"))
                }
            })?;
        }
        if command_attributes != 1 {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "TclCommand requires exactly one #[command(...)] attribute",
            ));
        }
        if names.is_empty() {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "command requires name = \"...\"",
            ));
        }
        let handler = handler.ok_or_else(|| {
            syn::Error::new_spanned(&input.ident, "command requires handler = path")
        })?;
        let summary = summary.ok_or_else(|| {
            syn::Error::new_spanned(&input.ident, "public command requires an explicit summary")
        })?;
        let requires = requires.ok_or_else(|| {
            syn::Error::new_spanned(
                &input.ident,
                "public command requires explicit preconditions",
            )
        })?;
        if !kinds.is_empty() && kinds.len() != names.len() {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "command kind and variant_kind values must match name and variant values",
            ));
        }
        for (values, label) in [
            (&variant_summaries, "variant_summary"),
            (&variant_requires, "variant_requires"),
            (&variant_examples, "variant_example"),
        ] {
            if !values.is_empty() && values.len() != names.len().saturating_sub(1) {
                return Err(syn::Error::new_spanned(
                    &input.ident,
                    format!("{label} values must cover every declared variant"),
                ));
            }
        }
        if sdc_no_positionals && !sdc {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "sdc_no_positionals requires sdc",
            ));
        }
        let conditional_count = usize::from(!positional_if_any.is_empty())
            + usize::from(positional_present.is_some())
            + usize::from(positional_absent.is_some());
        if conditional_count != 0 && conditional_count != 3 {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "positional_if_any, positional_present, and positional_absent must be declared together",
            ));
        }
        let mut unique_names = BTreeSet::new();
        for name in &names {
            if !unique_names.insert(name.value()) {
                return Err(syn::Error::new_spanned(name, "duplicate command name"));
            }
        }
        Ok(Self {
            names,
            kinds,
            handler,
            sdc,
            sdc_no_positionals,
            option_or_positional,
            summary,
            requires,
            example,
            variant_summaries,
            variant_requires,
            variant_examples,
            validation,
            positional_if_any,
            positional_present,
            positional_absent,
        })
    }
}

/// Every option name an SDC path-exception command accepts for a path point.
///
/// Timing exception commands share one point vocabulary. Defining it here keeps
/// the commands from drifting apart option by option.
const PATH_POINT_OPTIONS: [&str; 9] = [
    "-from",
    "-rise_from",
    "-fall_from",
    "-through",
    "-rise_through",
    "-fall_through",
    "-to",
    "-rise_to",
    "-fall_to",
];

impl<'a> FieldConfig<'a> {
    pub(super) fn parse(index: usize, field: &'a Field) -> syn::Result<Self> {
        let ident = field.ident.as_ref().expect("named fields have identifiers");
        let (shape, value_ty) = type_shape(&field.ty);
        let mut config = Self {
            index,
            ident,
            ty: &field.ty,
            value_ty,
            shape,
            names: Vec::new(),
            positional: false,
            unsupported: false,
            before_options: false,
            value_hint: None,
            label: None,
            help: None,
            conflicts_with: Vec::new(),
            min: None,
            max: None,
            repetition: Repetition::Single,
        };
        let mut edge_aliases = false;
        for attribute in &field.attrs {
            if !attribute.path().is_ident("arg") {
                continue;
            }
            config.parse_attribute(attribute, &mut edge_aliases)?;
        }
        config.add_edge_aliases(field, edge_aliases)?;
        Ok(config)
    }

    fn parse_attribute(
        &mut self,
        attribute: &syn::Attribute,
        edge_aliases: &mut bool,
    ) -> syn::Result<()> {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("long") {
                self.names.push(meta.value()?.parse()?);
            } else if meta.path.is_ident("edge_aliases") {
                *edge_aliases = true;
            } else if meta.path.is_ident("path_points") {
                self.repetition = Repetition::PathPoints;
                let span = meta.path.span();
                self.names.extend(
                    PATH_POINT_OPTIONS
                        .iter()
                        .map(|name| LitStr::new(name, span)),
                );
            } else if meta.path.is_ident("positional") {
                self.positional = true;
            } else if meta.path.is_ident("unsupported") {
                self.unsupported = true;
            } else if meta.path.is_ident("before_options") {
                self.before_options = true;
            } else if meta.path.is_ident("repeatable") {
                self.repetition = Repetition::Repeatable;
            } else if meta.path.is_ident("value_hint") {
                self.value_hint = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("label") {
                if self.label.is_some() {
                    return Err(meta.error("duplicate argument label"));
                }
                let label: LitStr = meta.value()?.parse()?;
                if label.value().is_empty() {
                    return Err(syn::Error::new_spanned(
                        label,
                        "argument label cannot be empty",
                    ));
                }
                self.label = Some(label);
            } else if meta.path.is_ident("help") {
                if self.help.is_some() {
                    return Err(meta.error("duplicate argument help"));
                }
                self.help = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("conflicts_with") {
                let field: LitStr = meta.value()?.parse()?;
                self.conflicts_with.push(format_ident!("{}", field.value()));
            } else if meta.path.is_ident("min") {
                let value: LitInt = meta.value()?.parse()?;
                self.min = Some(value.base10_parse()?);
            } else if meta.path.is_ident("max") {
                let value: LitInt = meta.value()?.parse()?;
                self.max = Some(value.base10_parse()?);
            } else {
                return Err(meta.error("unsupported arg attribute"));
            }
            Ok(())
        })
    }

    fn add_edge_aliases(&mut self, field: &Field, enabled: bool) -> syn::Result<()> {
        if !enabled {
            return Ok(());
        }
        let Some(base) = self.names.first().cloned() else {
            return Err(syn::Error::new_spanned(
                field,
                "edge_aliases needs a long option to derive its rise and fall names",
            ));
        };
        let Some(stem) = base.value().strip_prefix('-').map(str::to_string) else {
            return Err(syn::Error::new_spanned(
                base,
                "edge_aliases needs a long option that starts with '-'",
            ));
        };
        self.names
            .push(LitStr::new(&format!("-rise_{stem}"), base.span()));
        self.names
            .push(LitStr::new(&format!("-fall_{stem}"), base.span()));
        Ok(())
    }
}

pub(super) fn is_numeric_type(ty: &Type) -> bool {
    [
        "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
        "u128", "usize",
    ]
    .iter()
    .any(|name| is_type(ty, name))
}

pub(super) fn is_type(ty: &Type, expected: &str) -> bool {
    matches!(
        ty,
        Type::Path(path)
            if path.path.segments.last().is_some_and(|segment| segment.ident == expected)
    )
}

fn type_shape(ty: &Type) -> (Shape, &Type) {
    if is_type(ty, "bool") {
        return (Shape::Bool, ty);
    }
    if matches!(ty, Type::Tuple(tuple) if tuple.elems.is_empty()) {
        return (Shape::Unit, ty);
    }
    if let Some(inner) = wrapper_type(ty, "Option") {
        return (Shape::Optional, inner);
    }
    if let Some(inner) = wrapper_type(ty, "Vec") {
        return (Shape::Repeated, inner);
    }
    (Shape::Required, ty)
}

fn wrapper_type<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    match arguments.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}
