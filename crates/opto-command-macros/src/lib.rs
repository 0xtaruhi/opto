// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Derive support for declaration-driven Tcl command arguments.

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod derive;

#[proc_macro_derive(TclCommand, attributes(arg, command))]
/// Derives the declaration-driven Tcl argument parser for a command structure.
pub fn derive_tcl_command(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    derive::expand_tcl_command(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
