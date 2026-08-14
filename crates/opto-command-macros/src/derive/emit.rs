// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Rust token emission for validated Tcl command schemas.

use super::model::{CommandConfig, FieldConfig, Repetition, Shape};
use super::parse::{is_numeric_type, is_type, named_fields, parse_fields};
use super::validate::{conflict_groups, validate_fields, validate_option_or_positional};
use quote::{format_ident, quote};
use syn::{DeriveInput, Ident, LitStr};

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
    let positionals = fields
        .iter()
        .filter(|field| field.positional)
        .map(FieldConfig::positional_hint_tokens)
        .collect::<Vec<_>>();
    let (positional_min, positional_max) = positional_arity(fields);
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
    let positional_policy = command.positional_policy_tokens(fields)?;
    let sdc_positional_arity = if command.sdc_no_positionals {
        quote!(Some(crate::command_catalog::PositionalArity::exactly(0)))
    } else {
        quote!(None)
    };
    Ok(quote! {
        crate::command_catalog::CommandSyntax {
            options: vec![#(#option_hints),*],
            unsupported_options: vec![#(#unsupported_hints),*],
            positionals: vec![#(#positionals),*],
            positional_arity: Some(
                crate::command_catalog::PositionalArity::range(#positional_min, #positional_max),
            ),
            sdc_positional_arity: #sdc_positional_arity,
            required_options: &[#(#required_options),*],
            option_or_positional: #option_or_positional,
            positional_policy: #positional_policy,
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

pub(crate) fn expand_tcl_command(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let fields = parse_fields(named_fields(input)?)?;
    let command = CommandConfig::parse(input)?;
    validate_fields(&fields)?;
    validate_option_or_positional(&command, &fields)?;
    let requires_example = fields.iter().any(|field| field.positional)
        || fields
            .iter()
            .filter(|field| !field.positional && !field.unsupported)
            .map(|field| field.names.len())
            .sum::<usize>()
            > 1;
    if requires_example && command.example.is_none() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "commands with positional arguments or multiple options require an explicit example",
        ));
    }
    if command.names.len() > 1 && command.variant_examples.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "commands with variants require an explicit example for every variant",
        ));
    }

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
    let validation = &command.validation;
    let command_specs = command
        .names
        .iter()
        .enumerate()
        .map(|(index, command_name)| {
            let summary = index
                .checked_sub(1)
                .and_then(|index| command.variant_summaries.get(index))
                .cloned()
                .unwrap_or_else(|| command.summary.clone());
            let requires = index
                .checked_sub(1)
                .and_then(|index| command.variant_requires.get(index))
                .cloned()
                .unwrap_or_else(|| command.requires.clone());
            let example = index
                .checked_sub(1)
                .and_then(|index| command.variant_examples.get(index))
                .or(command.example.as_ref())
                .map_or_else(|| quote!(None), |value| quote!(Some(#value)));
            quote! {
                crate::command_catalog::CommandSpec::typed(
                    #command_name,
                    #dispatch,
                    #sdc_since,
                    crate::command_catalog::CommandMetadata {
                        summary: #summary,
                        requires: #requires,
                        example: #example,
                        validation: #validation,
                    },
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

impl CommandConfig {
    fn positional_policy_tokens(
        &self,
        fields: &[FieldConfig<'_>],
    ) -> syn::Result<proc_macro2::TokenStream> {
        if self.positional_if_any.is_empty() {
            return Ok(quote!(crate::command_catalog::PositionalPolicy::Declared));
        }
        let mut options = Vec::new();
        for name in &self.positional_if_any {
            let field = fields
                .iter()
                .find(|field| {
                    field
                        .names
                        .iter()
                        .any(|candidate| candidate.value() == name.value())
                })
                .ok_or_else(|| {
                    syn::Error::new_spanned(name, "positional_if_any names an unknown option")
                })?;
            options.push(field.id_tokens());
        }
        let present = self
            .positional_present
            .expect("validated conditional arity");
        let absent = self.positional_absent.expect("validated conditional arity");
        Ok(quote! {
            crate::command_catalog::PositionalPolicy::ConditionalOnAnyOption {
                options: &[#(#options),*],
                present: crate::command_catalog::PositionalArity::exactly(#present),
                absent: crate::command_catalog::PositionalArity::exactly(#absent),
            }
        })
    }
}

impl FieldConfig<'_> {
    pub(super) fn id_tokens(&self) -> proc_macro2::TokenStream {
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
        let help = self.help_text();
        self.names
            .iter()
            .map(|name| match self.shape {
                Shape::Bool | Shape::Unit if self.value_hint.is_none() => {
                    quote!(crate::command_catalog::typed_flag(#id, #name, #help))
                }
                Shape::Repeated
                    if self.repetition == Repetition::Repeatable
                        || self.repetition == Repetition::PathPoints =>
                {
                    quote!(crate::command_catalog::typed_repeated_value(#id, #name, #hint, #help))
                }
                _ => quote!(crate::command_catalog::typed_value(#id, #name, #hint, #help)),
            })
            .collect()
    }

    fn positional_hint_tokens(&self) -> proc_macro2::TokenStream {
        let name = self.label.as_ref().map_or_else(
            || LitStr::new(&self.ident.to_string(), self.ident.span()),
            Clone::clone,
        );
        let value = self.hint_tokens();
        let lexeme = if is_numeric_type(self.value_ty) {
            quote!(crate::command_catalog::PositionalLexeme::Numeric)
        } else {
            quote!(crate::command_catalog::PositionalLexeme::Text)
        };
        let (min, max) = match self.shape {
            Shape::Required => (1usize, quote!(1usize)),
            Shape::Optional => (0usize, quote!(1usize)),
            Shape::Repeated => {
                let min = self.min.unwrap_or(0);
                let max = self
                    .max
                    .map_or_else(|| quote!(usize::MAX), |max| quote!(#max));
                (min, max)
            }
            Shape::Bool | Shape::Unit => unreachable!("validated positional shape"),
        };
        let before_options = self.before_options;
        let help = self.help_text();
        quote! {
            crate::command_catalog::PositionalHint {
                name: #name,
                value: #value,
                lexeme: #lexeme,
                min: #min,
                max: #max,
                before_options: #before_options,
                help: #help,
            }
        }
    }

    fn help_text(&self) -> LitStr {
        self.help.clone().unwrap_or_else(|| {
            let identifier = self.ident.to_string();
            let words = identifier.trim_start_matches('_').replace('_', " ");
            let text = match identifier.trim_start_matches('_') {
                "rise" => "Apply the command to rising transitions.".to_string(),
                "fall" => "Apply the command to falling transitions.".to_string(),
                "min" => "Apply the command to the minimum analysis corner.".to_string(),
                "max" => "Apply the command to the maximum analysis corner.".to_string(),
                "early" => "Apply the command to the early analysis side.".to_string(),
                "late" => "Apply the command to the late analysis side.".to_string(),
                "setup" => "Apply the command to setup analysis.".to_string(),
                "hold" => "Apply the command to hold analysis.".to_string(),
                "from" => "Select the path startpoint objects.".to_string(),
                "through" => "Select path-through objects in traversal order.".to_string(),
                "to" => "Select the path endpoint objects.".to_string(),
                "filter" => "Filter matching objects by a typed property expression.".to_string(),
                "of_objects" => "Query objects related to this object collection.".to_string(),
                _ if self.positional => format!("The command's {words} argument."),
                _ if matches!(self.shape, Shape::Bool | Shape::Unit)
                    && self.value_hint.is_none() =>
                {
                    format!("Enable the {words} mode described by this command.")
                }
                _ => format!("The value for the {words} option."),
            };
            LitStr::new(&text, self.ident.span())
        })
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
            Shape::Bool => quote!(#ident: invocation.has_option(#id)),
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
