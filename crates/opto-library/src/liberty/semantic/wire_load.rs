// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{GroupRef, ParseContext, Reader, required_value};
use crate::{LibraryError, WireLoadModel, WireLoadTree};

/// Resolves only the interconnect topology; PVT characterization remains in
/// the selected library's numeric cell tables.
pub(super) fn parse_operating_condition_tree(
    group: &GroupRef<'_>,
    context: &ParseContext<'_>,
) -> Result<(String, WireLoadTree), LibraryError> {
    let name = required_value(&group.arguments, "operating_conditions")?.into_owned();
    let mut tree = None;
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        if statement.name != "tree_type" {
            continue;
        }
        let invalid = |detail: &str| LibraryError::UnsupportedConstruct {
            construct: format!("operating_conditions '{name}' tree_type: {detail}"),
        };
        if tree.is_some() {
            return Err(invalid("duplicate attribute"));
        }
        let super::super::syntax::StatementKind::Simple(values) = statement.kind else {
            return Err(invalid("expected a simple attribute"));
        };
        let value = required_value(&values, "tree_type")?;
        tree = Some(match value.as_ref() {
            "balanced_tree" => WireLoadTree::Balanced,
            "worst_case_tree" => WireLoadTree::WorstCase,
            "best_case_tree" => WireLoadTree::BestCase,
            _ => return Err(invalid(&format!("unknown value '{value}'"))),
        });
    }
    Ok((name, tree.unwrap_or_default()))
}

pub(super) fn parse_wire_load(
    group: &GroupRef<'_>,
    context: &ParseContext<'_>,
) -> Result<WireLoadModel, LibraryError> {
    let name = required_value(&group.arguments, "wire_load")?.into_owned();
    let mut capacitance = None;
    let mut resistance = None;
    let mut slope = None;
    let mut fanout_lengths = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("capacitance", super::super::syntax::StatementKind::Simple(values)) => {
                capacitance = Some(parse_value(&values, 0, "wire_load capacitance")?);
            }
            ("resistance", super::super::syntax::StatementKind::Simple(values)) => {
                resistance = Some(parse_value(&values, 0, "wire_load resistance")?);
            }
            ("slope", super::super::syntax::StatementKind::Simple(values)) => {
                slope = Some(parse_value(&values, 0, "wire_load slope")?);
            }
            ("fanout_length", super::super::syntax::StatementKind::Complex(values)) => {
                fanout_lengths.push((
                    parse_value(&values, 0, "wire_load fanout_length fanout")?,
                    parse_value(&values, 1, "wire_load fanout_length length")?,
                ));
            }
            _ => {}
        }
    }
    let missing = |attribute: &str| LibraryError::InvalidWireLoad {
        name: name.clone(),
        detail: format!("missing '{attribute}' attribute"),
    };
    let capacitance = capacitance.ok_or_else(|| missing("capacitance"))?;
    let resistance = resistance.ok_or_else(|| missing("resistance"))?;
    let slope = slope.ok_or_else(|| missing("slope"))?;
    WireLoadModel::new(name, capacitance, resistance, slope, fanout_lengths)
}

fn parse_value(
    values: &super::super::syntax::Values<'_>,
    index: usize,
    attribute: &'static str,
) -> Result<f64, LibraryError> {
    let value = values
        .get(index)
        .ok_or(LibraryError::MissingValue { attribute })?
        .decoded();
    value
        .parse::<f64>()
        .map_err(|_| LibraryError::InvalidNumber {
            attribute,
            value: value.into_owned(),
        })
}
