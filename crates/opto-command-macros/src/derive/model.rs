// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Parsed model for a derived Tcl command schema.

use syn::{Expr, ExprPath, Ident, LitStr, Type};

pub(super) struct CommandConfig {
    pub(super) names: Vec<LitStr>,
    pub(super) kinds: Vec<ExprPath>,
    pub(super) handler: ExprPath,
    pub(super) sdc: bool,
    pub(super) sdc_no_positionals: bool,
    pub(super) option_or_positional: Option<LitStr>,
    pub(super) summary: LitStr,
    pub(super) requires: LitStr,
    pub(super) example: Option<LitStr>,
    pub(super) variant_summaries: Vec<LitStr>,
    pub(super) variant_requires: Vec<LitStr>,
    pub(super) variant_examples: Vec<LitStr>,
    pub(super) validation: ExprPath,
    pub(super) positional_if_any: Vec<LitStr>,
    pub(super) positional_present: Option<usize>,
    pub(super) positional_absent: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    Bool,
    Unit,
    Required,
    Optional,
    Repeated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Repetition {
    Single,
    Repeatable,
    PathPoints,
}

pub(super) struct FieldConfig<'a> {
    pub(super) index: usize,
    pub(super) ident: &'a Ident,
    pub(super) ty: &'a Type,
    pub(super) value_ty: &'a Type,
    pub(super) shape: Shape,
    pub(super) names: Vec<LitStr>,
    pub(super) positional: bool,
    pub(super) unsupported: bool,
    pub(super) before_options: bool,
    pub(super) value_hint: Option<Expr>,
    pub(super) label: Option<LitStr>,
    pub(super) help: Option<LitStr>,
    pub(super) conflicts_with: Vec<Ident>,
    pub(super) min: Option<usize>,
    pub(super) max: Option<usize>,
    pub(super) repetition: Repetition,
}
