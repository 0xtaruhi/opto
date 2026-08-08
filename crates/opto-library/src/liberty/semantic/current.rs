// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::table::parse_float_list;
use super::{GroupRef, ParseContext, Reader, optional_value, required_value};
use crate::LibraryError;
use crate::lookup_table::LookupTableBuilder;
use crate::timing_model::{SampledWaveform, SampledWaveformGrid};
use std::collections::HashSet;

pub(super) fn parse_output_current_template(
    group: &GroupRef<'_>,
    context: &ParseContext<'_>,
) -> Result<String, LibraryError> {
    let name = required_value(&group.arguments, "output_current_template")?.into_owned();
    let mut variables = [None, None, None];
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        let super::super::syntax::StatementKind::Simple(values) = statement.kind else {
            continue;
        };
        match statement.name {
            "variable_1" => variables[0] = optional_value(&values),
            "variable_2" => variables[1] = optional_value(&values),
            "variable_3" => variables[2] = optional_value(&values),
            _ => {}
        }
    }
    let expected = [
        "input_net_transition",
        "total_output_net_capacitance",
        "time",
    ];
    for (index, (actual, expected)) in variables.into_iter().zip(expected).enumerate() {
        if actual.as_deref() != Some(expected) {
            return Err(invalid_ccs(format!(
                "output_current_template '{name}' variable_{} must be '{expected}'",
                index + 1
            )));
        }
    }
    Ok(name)
}

pub(super) fn parse_output_current(
    group: &GroupRef<'_>,
    templates: &HashSet<String>,
    table_builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<SampledWaveformGrid, LibraryError> {
    let mut vectors = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        if let ("vector", super::super::syntax::StatementKind::Group { arguments, body }) =
            (statement.name, statement.kind)
        {
            vectors.push(parse_vector(
                &GroupRef { arguments, body },
                templates,
                context,
            )?);
        }
    }
    if vectors.is_empty() {
        return Err(invalid_ccs("output_current group contains no vectors"));
    }
    let mut index_1 = vectors
        .iter()
        .map(|vector| vector.index_1)
        .collect::<Vec<_>>();
    let mut index_2 = vectors
        .iter()
        .map(|vector| vector.index_2)
        .collect::<Vec<_>>();
    sort_unique(&mut index_1);
    sort_unique(&mut index_2);
    let expected = index_1
        .len()
        .checked_mul(index_2.len())
        .ok_or_else(|| invalid_ccs("output current grid exceeds the host capacity"))?;
    if vectors.len() != expected {
        return Err(invalid_ccs(format!(
            "output current grid requires {expected} vectors but contains {}",
            vectors.len()
        )));
    }
    let mut ordered = std::iter::repeat_with(|| None)
        .take(expected)
        .collect::<Vec<_>>();
    for vector in vectors {
        let row = index_1
            .binary_search_by(|value| value.total_cmp(&vector.index_1))
            .expect("unique CCS input-slew axis contains every vector coordinate");
        let column = index_2
            .binary_search_by(|value| value.total_cmp(&vector.index_2))
            .expect("unique CCS load axis contains every vector coordinate");
        let index = row * index_2.len() + column;
        if ordered[index]
            .replace(SampledWaveform {
                reference_time: vector.reference_time,
                coordinates: vector.times,
                values: vector.currents,
            })
            .is_some()
        {
            return Err(invalid_ccs(format!(
                "duplicate output current vector at input slew {} and load {}",
                vector.index_1, vector.index_2
            )));
        }
    }
    let ordered = ordered
        .into_iter()
        .enumerate()
        .map(|(index, vector)| {
            vector.ok_or_else(|| invalid_ccs(format!("output current vector {index} is missing")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let index_1 = table_builder.intern_axis(&index_1);
    let index_2 = table_builder.intern_axis(&index_2);
    SampledWaveformGrid::from_shared("CCS", index_1, index_2, ordered)
}

struct CurrentVector {
    index_1: f64,
    index_2: f64,
    reference_time: f64,
    times: Vec<f64>,
    currents: Vec<f64>,
}

fn parse_vector(
    group: &GroupRef<'_>,
    templates: &HashSet<String>,
    context: &ParseContext<'_>,
) -> Result<CurrentVector, LibraryError> {
    let template = required_value(&group.arguments, "vector")?;
    if !templates.contains(template.as_ref()) {
        return Err(invalid_ccs(format!(
            "vector references unknown output_current_template '{template}'"
        )));
    }
    let mut index_1 = Vec::new();
    let mut index_2 = Vec::new();
    let mut times = Vec::new();
    let mut currents = Vec::new();
    let mut reference_time = None;
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("reference_time", super::super::syntax::StatementKind::Simple(values)) => {
                let raw = required_value(&values, "reference_time")?;
                reference_time =
                    Some(
                        raw.parse::<f64>()
                            .map_err(|_| LibraryError::InvalidNumber {
                                attribute: "reference_time",
                                value: raw.into_owned(),
                            })?,
                    );
            }
            ("index_1", super::super::syntax::StatementKind::Complex(values)) => {
                index_1 = parse_float_list(&values, "index_1")?;
            }
            ("index_2", super::super::syntax::StatementKind::Complex(values)) => {
                index_2 = parse_float_list(&values, "index_2")?;
            }
            ("index_3", super::super::syntax::StatementKind::Complex(values)) => {
                times = parse_float_list(&values, "index_3")?;
            }
            ("values", super::super::syntax::StatementKind::Complex(values)) => {
                currents = parse_float_list(&values, "values")?;
            }
            _ => {}
        }
    }
    let [index_1] = index_1.as_slice() else {
        return Err(invalid_ccs(
            "every output current vector must contain exactly one index_1 value",
        ));
    };
    let [index_2] = index_2.as_slice() else {
        return Err(invalid_ccs(
            "every output current vector must contain exactly one index_2 value",
        ));
    };
    let reference_time = reference_time
        .ok_or_else(|| invalid_ccs("every output current vector requires reference_time"))?;
    Ok(CurrentVector {
        index_1: *index_1,
        index_2: *index_2,
        reference_time,
        times,
        currents,
    })
}

fn sort_unique(values: &mut Vec<f64>) {
    values.sort_by(f64::total_cmp);
    values.dedup_by(|left, right| left.to_bits() == right.to_bits());
}

fn invalid_ccs(detail: impl Into<String>) -> LibraryError {
    LibraryError::InvalidTimingModel {
        model: "CCS",
        detail: detail.into(),
    }
}
