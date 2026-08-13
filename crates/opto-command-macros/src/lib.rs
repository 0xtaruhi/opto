// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Derive support for declaration-driven Tcl command arguments.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use std::collections::{BTreeMap, BTreeSet};
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Expr, ExprPath, Field, Fields, GenericArgument, Ident, LitInt, LitStr,
    PathArguments, Type, parse_macro_input,
};

#[proc_macro_derive(TclCommand, attributes(arg, command))]
/// Derives the declaration-driven Tcl argument parser for a command structure.
pub fn derive_tcl_command(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_tcl_command(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn named_fields(
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

fn parse_fields(
    fields: &syn::punctuated::Punctuated<Field, syn::token::Comma>,
) -> syn::Result<Vec<FieldConfig<'_>>> {
    fields
        .iter()
        .enumerate()
        .map(|(index, field)| FieldConfig::parse(index, field))
        .collect()
}

fn validate_option_or_positional(
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

fn command_syntax_tokens(
    command: &CommandConfig,
    fields: &[FieldConfig<'_>],
) -> syn::Result<proc_macro2::TokenStream> {
    let option_hints = fields
        .iter()
        .filter(|field| !field.positional && !field.unsupported)
        .flat_map(FieldConfig::option_hint_tokens)
        .collect::<Vec<_>>();
    let unsupported_hints = fields
        .iter()
        .filter(|field| field.unsupported)
        .flat_map(FieldConfig::option_hint_tokens)
        .collect::<Vec<_>>();
    let positional = fields.iter().find(|field| field.positional);
    let positional_hint = positional.map_or_else(
        || quote!(None),
        |field| {
            let hint = field.hint_tokens();
            quote!(Some(#hint))
        },
    );
    let positional_label = positional
        .and_then(|field| field.label.as_ref())
        .map_or_else(|| quote!(None), |label| quote!(Some(#label)));
    let (positional_min, positional_max) = positional_arity(fields);
    let leading_positionals = fields.iter().filter(|field| field.before_options).count();
    let required_options = fields
        .iter()
        .filter(|field| field.is_required_option())
        .map(|field| field.names[0].clone())
        .collect::<Vec<_>>();
    let option_or_positional = command
        .option_or_positional
        .as_ref()
        .map_or_else(|| quote!(None), |name| quote!(Some(#name)));
    let conflict_groups = conflict_groups(fields)?;
    let sdc_positional_arity = if command.sdc_no_positionals {
        quote!(Some(crate::command_catalog::PositionalArity::exactly(0)))
    } else {
        quote!(None)
    };
    Ok(quote! {
        crate::command_catalog::CommandSyntax {
            options: vec![#(#option_hints),*],
            unsupported_options: vec![#(#unsupported_hints),*],
            positional: #positional_hint,
            positional_label: #positional_label,
            positional_arity: Some(
                crate::command_catalog::PositionalArity::range(#positional_min, #positional_max),
            ),
            sdc_positional_arity: #sdc_positional_arity,
            required_options: &[#(#required_options),*],
            option_or_positional: #option_or_positional,
            leading_positionals: #leading_positionals,
            mutually_exclusive_options: &[#(#conflict_groups),*],
        }
    })
}

struct GenericContext {
    impl_generics: proc_macro2::TokenStream,
    ty_generics: proc_macro2::TokenStream,
    where_clause: proc_macro2::TokenStream,
    trait_impl_generics: proc_macro2::TokenStream,
    trait_lifetime: proc_macro2::TokenStream,
    has_lifetime: bool,
}

fn generic_context(input: &DeriveInput) -> syn::Result<GenericContext> {
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let existing_lifetime = input
        .generics
        .lifetimes()
        .next()
        .map(|value| &value.lifetime);
    if input.generics.lifetimes().count() > 1
        || input.generics.type_params().next().is_some()
        || input.generics.const_params().next().is_some()
    {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "TclCommand supports at most one lifetime parameter",
        ));
    }
    let (trait_impl_generics, trait_lifetime) = if let Some(lifetime) = existing_lifetime {
        (quote!(#impl_generics), quote!(#lifetime))
    } else {
        let parameters = &input.generics.params;
        let generics = if parameters.is_empty() {
            quote!(<'__tcl>)
        } else {
            quote!(<'__tcl, #parameters>)
        };
        (generics, quote!('__tcl))
    };
    Ok(GenericContext {
        impl_generics: quote!(#impl_generics),
        ty_generics: quote!(#ty_generics),
        where_clause: quote!(#where_clause),
        trait_impl_generics,
        trait_lifetime,
        has_lifetime: existing_lifetime.is_some(),
    })
}

fn constructor_tokens(
    fields: &[FieldConfig<'_>],
    trait_lifetime: &proc_macro2::TokenStream,
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    let mut positional_index = 0usize;
    let mut constructors = Vec::with_capacity(fields.len());
    for field in fields {
        constructors.push(field.constructor_tokens(trait_lifetime, positional_index)?);
        if field.positional && !matches!(field.shape, Shape::Repeated) {
            positional_index += 1;
        }
    }
    Ok(constructors)
}

fn command_definitions(
    command: &CommandConfig,
    schema_type: &proc_macro2::TokenStream,
) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    command
        .names
        .iter()
        .map(|command_name| {
            let constant = command_constant_ident(command_name)?;
            let documentation = LitStr::new(
                &format!("Registers the Tcl command `{}`.", command_name.value()),
                command_name.span(),
            );
            Ok(quote! {
                #[doc = #documentation]
                pub const #constant: crate::command_catalog::CommandDefinition =
                    crate::command_catalog::CommandDefinition::new(
                        #command_name,
                        #schema_type::command_specs,
                    );
            })
        })
        .collect()
}

fn expand_tcl_command(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let fields = parse_fields(named_fields(input)?)?;
    let command = CommandConfig::parse(input)?;
    validate_fields(&fields)?;
    validate_option_or_positional(&command, &fields)?;

    let syntax = command_syntax_tokens(&command, &fields)?;
    let generics = generic_context(input)?;
    let constructors = constructor_tokens(&fields, &generics.trait_lifetime)?;
    let name = &input.ident;
    let dispatch = format_ident!("__opto_dispatch_{}", snake_case(&name.to_string()));
    let handler = &command.handler;
    let kind_argument = if command.kinds.is_empty() {
        quote!()
    } else {
        let arms = command
            .names
            .iter()
            .zip(&command.kinds)
            .map(|(name, kind)| quote!(#name => #kind,));
        quote!(, match command {
            #(#arms)*
            _ => unreachable!("registered command name is generated from this schema"),
        })
    };
    let invocation_type = if generics.has_lifetime {
        quote!(#name<'__tcl>)
    } else {
        quote!(#name)
    };
    let schema_type = if generics.has_lifetime {
        quote!(#name::<'static>)
    } else {
        quote!(#name)
    };
    let sdc_since = if command.sdc {
        quote!(Some(crate::sdc::SdcVersion::V1_0))
    } else {
        quote!(None)
    };
    let command_specs = command.names.iter().map(|command_name| {
        let summary = command.summary.clone().unwrap_or_else(|| {
            LitStr::new(
                &format!("Execute the public `{}` command.", command_name.value()),
                command_name.span(),
            )
        });
        let requires = command.requires.clone().unwrap_or_else(|| {
            LitStr::new(
                "The declared arguments and referenced session objects must be valid.",
                command_name.span(),
            )
        });
        let example = command
            .example
            .as_ref()
            .map_or_else(|| quote!(None), |value| quote!(Some(#value)));
        quote! {
            crate::command_catalog::CommandSpec::typed(
                #command_name,
                #dispatch,
                #sdc_since,
                #summary,
                #requires,
                #example,
                #schema_type::command_syntax,
            )
        }
    });
    let command_definitions = command_definitions(&command, &schema_type)?;
    let GenericContext {
        impl_generics,
        ty_generics,
        where_clause,
        trait_impl_generics,
        trait_lifetime,
        ..
    } = generics;

    Ok(quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            pub(crate) fn command_syntax() -> crate::command_catalog::CommandSyntax {
                #syntax
            }
        }

        impl #trait_impl_generics crate::command_args::TclArgs<#trait_lifetime>
            for #name #ty_generics #where_clause
        {
            fn from_invocation(
                command: &str,
                invocation: &crate::command_catalog::ParsedInvocation<#trait_lifetime>,
            ) -> Result<Self, crate::ShellError> {
                Ok(Self {
                    #(#constructors),*
                })
            }
        }

        fn #dispatch<'__tcl>(
            state: &crate::runtime::ShellState,
            interp: *mut opto_tcl_sys::ffi::TclInterp,
            command: &'static str,
            invocation: &crate::command_catalog::ParsedInvocation<'__tcl>,
        ) -> Result<crate::command::CommandResult, crate::ShellError> {
            let arguments = <#invocation_type as crate::command_args::TclArgs<'__tcl>>::from_invocation(
                command,
                invocation,
            )?;
            #handler(state, interp, command, arguments #kind_argument)
        }

        impl #impl_generics #name #ty_generics #where_clause {
            pub(crate) fn command_specs() -> &'static [crate::command_catalog::CommandSpec] {
                const SPECS: &[crate::command_catalog::CommandSpec] = &[
                    #(#command_specs),*
                ];
                SPECS
            }
        }

        #(#command_definitions)*
    })
}

fn command_constant_ident(command_name: &LitStr) -> syn::Result<Ident> {
    let name = command_name.value().to_ascii_uppercase();
    syn::parse_str(&name).map_err(|_| {
        syn::Error::new_spanned(
            command_name,
            "command name cannot be represented as a Rust registration constant",
        )
    })
}

struct CommandConfig {
    names: Vec<LitStr>,
    kinds: Vec<ExprPath>,
    handler: ExprPath,
    sdc: bool,
    sdc_no_positionals: bool,
    option_or_positional: Option<LitStr>,
    summary: Option<LitStr>,
    requires: Option<LitStr>,
    example: Option<LitStr>,
}

impl CommandConfig {
    fn parse(input: &DeriveInput) -> syn::Result<Self> {
        let mut names: Vec<LitStr> = Vec::new();
        let mut handler: Option<ExprPath> = None;
        let mut kinds: Vec<ExprPath> = Vec::new();
        let mut sdc = false;
        let mut sdc_no_positionals = false;
        let mut option_or_positional: Option<LitStr> = None;
        let mut summary: Option<LitStr> = None;
        let mut requires: Option<LitStr> = None;
        let mut example: Option<LitStr> = None;
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
                } else if meta.path.is_ident("requires") {
                    requires = Some(meta.value()?.parse()?);
                    Ok(())
                } else if meta.path.is_ident("example") {
                    example = Some(meta.value()?.parse()?);
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
        if !kinds.is_empty() && kinds.len() != names.len() {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "command kind and variant_kind values must match name and variant values",
            ));
        }
        if sdc_no_positionals && !sdc {
            return Err(syn::Error::new_spanned(
                &input.ident,
                "sdc_no_positionals requires sdc",
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
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Bool,
    Unit,
    Required,
    Optional,
    Repeated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Repetition {
    Single,
    Repeatable,
    PathPoints,
}

struct FieldConfig<'a> {
    index: usize,
    ident: &'a Ident,
    ty: &'a Type,
    value_ty: &'a Type,
    shape: Shape,
    names: Vec<LitStr>,
    positional: bool,
    unsupported: bool,
    before_options: bool,
    value_hint: Option<Expr>,
    label: Option<LitStr>,
    conflicts_with: Vec<Ident>,
    min: Option<usize>,
    max: Option<usize>,
    repetition: Repetition,
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
    fn parse(index: usize, field: &'a Field) -> syn::Result<Self> {
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

    fn id_tokens(&self) -> proc_macro2::TokenStream {
        let index = u16::try_from(self.index + 1).expect("command argument count exceeds u16");
        quote!(crate::command_catalog::OptionId::Derived(#index))
    }

    fn hint_tokens(&self) -> proc_macro2::TokenStream {
        self.value_hint.as_ref().map_or_else(
            || quote!(crate::command_catalog::ValueHint::Text),
            |hint| quote!(#hint),
        )
    }

    fn option_hint_tokens(&self) -> Vec<proc_macro2::TokenStream> {
        let id = self.id_tokens();
        let hint = self.hint_tokens();
        self.names
            .iter()
            .map(|name| match self.shape {
                Shape::Bool | Shape::Unit if self.value_hint.is_none() => {
                    quote!(crate::command_catalog::typed_flag(#id, #name))
                }
                Shape::Repeated
                    if self.repetition == Repetition::Repeatable
                        || self.repetition == Repetition::PathPoints =>
                {
                    quote!(crate::command_catalog::typed_repeated_value(#id, #name, #hint))
                }
                _ => quote!(crate::command_catalog::typed_value(#id, #name, #hint)),
            })
            .collect()
    }

    fn is_required_option(&self) -> bool {
        !self.positional && !self.unsupported && matches!(self.shape, Shape::Required)
    }

    fn constructor_tokens(
        &self,
        trait_lifetime: &proc_macro2::TokenStream,
        positional_index: usize,
    ) -> syn::Result<proc_macro2::TokenStream> {
        let ident = self.ident;
        if self.unsupported {
            return Ok(quote!(#ident: ()));
        }
        if self.positional {
            return self.positional_constructor(trait_lifetime, positional_index);
        }
        let id = self.id_tokens();
        let label = &self.names[0];
        let value_ty = self.value_ty;
        let named_value = is_type(value_ty, "TclOption");
        Ok(match self.shape {
            Shape::Bool => quote!(
                #ident: invocation.has_option(#id)
            ),
            Shape::Optional if named_value => quote!(
                #ident: invocation
                    .last_option(#id)
                    .map(|(name, value)| crate::command_args::TclOption::new(name, value))
            ),
            Shape::Optional => quote!(
                #ident: invocation
                    .last_option(#id)
                    .map(|(name, value)| {
                        <#value_ty as crate::command_args::FromTclValue<#trait_lifetime>>::from_tcl_value(
                            command,
                            name,
                            value,
                        )
                    })
                    .transpose()?
            ),
            Shape::Repeated if named_value => quote!(
                #ident: invocation
                    .option_occurrences(#id)
                    .map(|(name, value)| crate::command_args::TclOption::new(name, value))
                    .collect()
            ),
            Shape::Repeated => quote!(
                #ident: invocation
                    .option_values(#id)
                    .map(|value| {
                        <#value_ty as crate::command_args::FromTclValue<#trait_lifetime>>::from_tcl_value(
                            command,
                            #label,
                            value,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
            ),
            Shape::Required if named_value => quote!(
                #ident: {
                    let (name, value) = invocation.last_option(#id).ok_or_else(|| {
                        crate::ShellError::command(format!(
                            "{command}: missing {} <value>",
                            #label,
                        ))
                    })?;
                    crate::command_args::TclOption::new(name, value)
                }
            ),
            Shape::Required => quote!(
                #ident: {
                    let (name, value) = invocation.last_option(#id).ok_or_else(|| {
                        crate::ShellError::command(format!(
                            "{command}: missing {} <value>",
                            #label,
                        ))
                    })?;
                    <#value_ty as crate::command_args::FromTclValue<#trait_lifetime>>::from_tcl_value(
                        command,
                        name,
                        value,
                    )?
                }
            ),
            Shape::Unit => {
                return Err(syn::Error::new_spanned(
                    self.ty,
                    "unit fields must be marked unsupported",
                ));
            }
        })
    }

    fn positional_constructor(
        &self,
        trait_lifetime: &proc_macro2::TokenStream,
        positional_index: usize,
    ) -> syn::Result<proc_macro2::TokenStream> {
        let ident = self.ident;
        let value_ty = self.value_ty;
        let index = positional_index;
        Ok(match self.shape {
            Shape::Required => quote!(
                #ident: <#value_ty as crate::command_args::FromTclValue<#trait_lifetime>>::from_tcl_value(
                    command,
                    stringify!(#ident),
                    invocation.positionals()[#index],
                )?
            ),
            Shape::Optional => quote!(
                #ident: invocation
                    .positionals()
                    .get(#index)
                    .copied()
                    .map(|value| {
                        <#value_ty as crate::command_args::FromTclValue<#trait_lifetime>>::from_tcl_value(
                            command,
                            stringify!(#ident),
                            value,
                        )
                    })
                    .transpose()?
            ),
            Shape::Repeated => quote!(
                #ident: invocation.positionals()[#index..]
                    .iter()
                    .copied()
                    .map(|value| {
                        <#value_ty as crate::command_args::FromTclValue<#trait_lifetime>>::from_tcl_value(
                            command,
                            stringify!(#ident),
                            value,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?
            ),
            Shape::Bool | Shape::Unit => {
                return Err(syn::Error::new_spanned(
                    self.ty,
                    "positional fields cannot be bool or unit",
                ));
            }
        })
    }
}

fn validate_fields(fields: &[FieldConfig<'_>]) -> syn::Result<()> {
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

fn validate_field_contract(field: &FieldConfig<'_>) -> syn::Result<()> {
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

fn validate_field_references(
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

fn positional_arity(fields: &[FieldConfig<'_>]) -> (usize, proc_macro2::TokenStream) {
    let positional = fields
        .iter()
        .filter(|field| field.positional)
        .collect::<Vec<_>>();
    let fixed_required = positional
        .iter()
        .filter(|field| field.shape == Shape::Required)
        .count();
    let repeated = positional
        .iter()
        .find(|field| field.shape == Shape::Repeated);
    let min = fixed_required + repeated.and_then(|field| field.min).unwrap_or(0);
    let max = if let Some(repeated) = repeated {
        let max = repeated.max.unwrap_or(usize::MAX);
        quote!(#max)
    } else {
        let optional = positional
            .iter()
            .filter(|field| field.shape == Shape::Optional)
            .count();
        let max = fixed_required + optional;
        quote!(#max)
    };
    (min, max)
}

fn conflict_groups(fields: &[FieldConfig<'_>]) -> syn::Result<Vec<proc_macro2::TokenStream>> {
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

fn is_type(ty: &Type, expected: &str) -> bool {
    matches!(
        ty,
        Type::Path(path)
            if path.path.segments.last().is_some_and(|segment| segment.ident == expected)
    )
}

fn snake_case(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() {
            if index != 0 {
                output.push('_');
            }
            output.extend(character.to_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_clap_style_typed_command_definition() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(
                name = "sample",
                kind = SampleKind::Primary,
                variant = "sample_variant",
                variant_kind = SampleKind::Variant,
                handler = execute,
                sdc
            )]
            struct SampleArgs<'a> {
                #[arg(long = "-period")]
                period: f64,
                #[arg(long = "-through")]
                through: Vec<TclArg<'a>>,
                #[arg(positional, min = 1)]
                objects: Vec<TclArg<'a>>,
            }
        };
        assert!(expand_tcl_command(&input).is_ok());
    }

    #[test]
    fn rejects_an_alias_for_a_required_field() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(name = "sample", handler = execute)]
            struct SampleArgs {
                #[arg(long = "-period", alias = "-p")]
                period: f64,
            }
        };
        assert!(expand_tcl_command(&input).is_err());
    }

    #[test]
    fn accepts_a_leading_positional_before_options() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(name = "sample", handler = execute)]
            struct SampleArgs {
                #[arg(positional, before_options)]
                value: f64,
                #[arg(long = "-mode")]
                mode: Option<String>,
                #[arg(positional, min = 1)]
                objects: Vec<String>,
            }
        };
        assert!(expand_tcl_command(&input).is_ok());
    }

    #[test]
    fn rejects_before_options_on_an_optional_positional() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(name = "sample", handler = execute)]
            struct SampleArgs {
                #[arg(positional, before_options)]
                value: Option<f64>,
            }
        };
        let error =
            expand_tcl_command(&input).expect_err("optional leading value must be rejected");
        assert!(error.to_string().contains("requires a required positional"));
    }

    #[test]
    fn rejects_positional_bounds_on_a_scalar() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(name = "sample", handler = execute)]
            struct SampleArgs {
                #[arg(positional, min = 1)]
                object: String,
            }
        };
        let error = expand_tcl_command(&input).expect_err("scalar bounds must be rejected");
        assert!(error.to_string().contains("only valid for repeated"));
    }
}
