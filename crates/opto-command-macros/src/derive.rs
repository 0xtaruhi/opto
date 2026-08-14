// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Tcl command schema derivation.

mod emit;
mod model;
mod parse;
mod validate;

pub(crate) use emit::expand_tcl_command;

#[cfg(test)]
mod tests {
    use super::*;
    use syn::DeriveInput;

    #[test]
    fn accepts_a_clap_style_typed_command_definition() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(
                name = "sample",
                kind = SampleKind::Primary,
                variant = "sample_variant",
                variant_kind = SampleKind::Variant,
                handler = execute,
                sdc,
                summary = "Run a sample command.",
                requires = "Sample inputs must be valid.",
                example = "sample -period 1 object",
                variant_summary = "Run the sample variant.",
                variant_requires = "Variant inputs must be valid.",
                variant_example = "sample_variant -period 1 object"
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
            #[command(
                name = "sample",
                handler = execute,
                summary = "Run a sample command.",
                requires = "Sample inputs must be valid.",
                example = "sample -period 1"
            )]
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
            #[command(
                name = "sample",
                handler = execute,
                summary = "Run a sample command.",
                requires = "Sample inputs must be valid.",
                example = "sample 1 object"
            )]
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
            #[command(
                name = "sample",
                handler = execute,
                summary = "Run a sample command.",
                requires = "Sample inputs must be valid.",
                example = "sample 1"
            )]
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
            #[command(
                name = "sample",
                handler = execute,
                summary = "Run a sample command.",
                requires = "Sample inputs must be valid.",
                example = "sample object"
            )]
            struct SampleArgs {
                #[arg(positional, min = 1)]
                object: String,
            }
        };
        let error = expand_tcl_command(&input).expect_err("scalar bounds must be rejected");
        assert!(error.to_string().contains("only valid for repeated"));
    }

    #[test]
    fn rejects_public_commands_without_help_contracts() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(name = "sample", handler = execute)]
            struct SampleArgs {}
        };
        assert!(
            expand_tcl_command(&input)
                .unwrap_err()
                .to_string()
                .contains("explicit summary")
        );
    }

    #[test]
    fn rejects_complex_commands_without_explicit_examples() {
        let input: DeriveInput = syn::parse_quote! {
            #[command(
                name = "sample",
                handler = execute,
                summary = "Run a sample command.",
                requires = "Sample inputs must be valid."
            )]
            struct SampleArgs {
                #[arg(positional)]
                object: String,
            }
        };
        assert!(
            expand_tcl_command(&input)
                .unwrap_err()
                .to_string()
                .contains("require an explicit example")
        );
    }
}
