// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{GroupRef, ParseContext, Reader, required_value};
use crate::timing_model::{SampledWaveform, SampledWaveformGrid};
use crate::{LibraryError, LookupTable, lookup_table::LookupTableBuilder};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug)]
pub(super) struct TableTemplate {
    index_1: Arc<[f64]>,
    index_2: Arc<[f64]>,
}

pub(super) struct ParsedTransition {
    pub(super) table: Option<LookupTable>,
    pub(super) waveforms: Option<SampledWaveformGrid>,
    pub(super) capacitance: Option<LookupTable>,
}

pub(super) fn parse_template(
    group: &GroupRef<'_>,
    group_name: &'static str,
    context: &ParseContext<'_>,
) -> Result<(String, TableTemplate), LibraryError> {
    let name = required_value(&group.arguments, group_name)?.into_owned();
    let mut index_1 = Vec::new();
    let mut index_2 = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("index_1", super::super::syntax::StatementKind::Complex(values)) => {
                index_1 = parse_float_list(&values, "index_1")?;
            }
            ("index_2", super::super::syntax::StatementKind::Complex(values)) => {
                index_2 = parse_float_list(&values, "index_2")?;
            }
            _ => {}
        }
    }
    Ok((
        name,
        TableTemplate {
            index_1: Arc::from(index_1),
            index_2: Arc::from(index_2),
        },
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "transition parsing keeps scalar, CCS, and ECSM alternatives in one exclusivity check"
)]
pub(super) fn parse_transition(
    group: &GroupRef<'_>,
    templates: &HashMap<String, TableTemplate>,
    ecsm_templates: &HashMap<String, TableTemplate>,
    builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<ParsedTransition, LibraryError> {
    let template = group
        .arguments
        .first()
        .map(super::super::syntax::Value::decoded)
        .as_deref()
        .and_then(|name| templates.get(name));
    let mut index_1 = None;
    let mut index_2 = None;
    let mut values = Vec::new();
    let mut waveforms = Vec::new();
    let mut waveform_set = None;
    let mut capacitance_values = None;
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("index_1", super::super::syntax::StatementKind::Complex(items)) => {
                index_1 = Some(parse_float_list(&items, "index_1")?);
            }
            ("index_2", super::super::syntax::StatementKind::Complex(items)) => {
                index_2 = Some(parse_float_list(&items, "index_2")?);
            }
            ("values", super::super::syntax::StatementKind::Complex(items)) => {
                values = parse_float_list(&items, "values")?;
            }
            ("ecsm_waveform", super::super::syntax::StatementKind::Group { arguments, body }) => {
                let (index, waveform) =
                    parse_ecsm_waveform(&GroupRef { arguments, body }, context)?;
                waveforms.push((index, waveform));
            }
            (
                "ecsm_waveform_set",
                super::super::syntax::StatementKind::Group { arguments, body },
            ) => {
                if waveform_set.is_some() || !waveforms.is_empty() {
                    return Err(invalid_ecsm(
                        "a transition group cannot mix ecsm_waveform and ecsm_waveform_set",
                    ));
                }
                waveform_set = Some(parse_ecsm_waveform_set(
                    &GroupRef { arguments, body },
                    ecsm_templates,
                    context,
                )?);
            }
            (
                "ecsm_capacitance",
                super::super::syntax::StatementKind::Group { arguments: _, body },
            ) => {
                if capacitance_values.is_some() {
                    return Err(invalid_ecsm(
                        "a transition group contains duplicate ecsm_capacitance data",
                    ));
                }
                capacitance_values = Some(parse_ecsm_capacitance(body, context)?);
            }
            _ => {}
        }
    }
    let index_1 = index_1
        .map(Arc::<[f64]>::from)
        .or_else(|| template.map(|item| Arc::clone(&item.index_1)))
        .unwrap_or_else(|| Arc::from([]));
    let index_2 = index_2
        .map(Arc::<[f64]>::from)
        .or_else(|| template.map(|item| Arc::clone(&item.index_2)))
        .unwrap_or_else(|| Arc::from([]));
    let index_1 = builder.intern_axis(&index_1);
    let index_2 = builder.intern_axis(&index_2);
    let table = (!values.is_empty()).then(|| builder.build(&index_1, &index_2, &values));
    let grid_size = index_1
        .len()
        .max(1)
        .checked_mul(index_2.len().max(1))
        .ok_or_else(|| invalid_ecsm("waveform grid exceeds the host capacity"))?;
    let waveforms = if let Some((coordinates, values)) = waveform_set {
        let expected_values = grid_size
            .checked_mul(coordinates.len())
            .ok_or_else(|| invalid_ecsm("waveform set exceeds the host capacity"))?;
        if coordinates.len() < 2 || values.len() != expected_values {
            return Err(invalid_ecsm(format!(
                "ecsm_waveform_set requires {} values but contains {}",
                expected_values,
                values.len()
            )));
        }
        let inputs = values
            .chunks_exact(coordinates.len())
            .map(|row| SampledWaveform {
                reference_time: 0.0,
                coordinates: coordinates.clone(),
                values: row.to_vec(),
            })
            .collect();
        Some(SampledWaveformGrid::from_shared(
            "ECSM",
            Arc::clone(&index_1),
            Arc::clone(&index_2),
            inputs,
        )?)
    } else if waveforms.is_empty() {
        None
    } else {
        let mut ordered = std::iter::repeat_with(|| None)
            .take(grid_size)
            .collect::<Vec<_>>();
        for (index, waveform) in waveforms {
            let slot = ordered.get_mut(index).ok_or_else(|| {
                invalid_ecsm(format!("waveform index {index} is outside the grid"))
            })?;
            if slot.replace(waveform).is_some() {
                return Err(invalid_ecsm(format!(
                    "waveform index {index} is defined more than once"
                )));
            }
        }
        let ordered = ordered
            .into_iter()
            .enumerate()
            .map(|(index, waveform)| {
                waveform.ok_or_else(|| invalid_ecsm(format!("waveform index {index} is missing")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Some(SampledWaveformGrid::from_shared(
            "ECSM",
            Arc::clone(&index_1),
            Arc::clone(&index_2),
            ordered,
        )?)
    };
    let capacitance = capacitance_values
        .filter(|values| !values.is_empty())
        .map(|values| builder.build(&index_1, &index_2, &values));
    Ok(ParsedTransition {
        table,
        waveforms,
        capacitance,
    })
}

pub(super) fn parse_table(
    group: &GroupRef<'_>,
    templates: &HashMap<String, TableTemplate>,
    builder: &mut LookupTableBuilder,
    context: &ParseContext<'_>,
) -> Result<Option<LookupTable>, LibraryError> {
    let template_name = group
        .arguments
        .first()
        .map(super::super::syntax::Value::decoded);
    let template = template_name
        .as_deref()
        .and_then(|name| templates.get(name));
    let mut index_1 = None;
    let mut index_2 = None;
    let mut values = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            ("index_1", super::super::syntax::StatementKind::Complex(items)) => {
                index_1 = Some(parse_float_list(&items, "index_1")?);
            }
            ("index_2", super::super::syntax::StatementKind::Complex(items)) => {
                index_2 = Some(parse_float_list(&items, "index_2")?);
            }
            ("values", super::super::syntax::StatementKind::Complex(items)) => {
                values = parse_float_list(&items, "values")?;
            }
            _ => {}
        }
    }
    if values.is_empty() {
        return Ok(None);
    }
    let index_1 = index_1
        .as_deref()
        .or_else(|| template.map(|item| &*item.index_1));
    let index_2 = index_2
        .as_deref()
        .or_else(|| template.map(|item| &*item.index_2));
    Ok(Some(builder.build(
        index_1.unwrap_or_default(),
        index_2.unwrap_or_default(),
        &values,
    )))
}

pub(super) fn parse_float_list(
    values: &super::super::syntax::Values<'_>,
    attribute: &'static str,
) -> Result<Vec<f64>, LibraryError> {
    let mut parsed = Vec::new();
    for value in values {
        let decoded = value.decoded();
        for item in decoded
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
        {
            parsed.push(
                item.parse::<f64>()
                    .map_err(|_| LibraryError::InvalidNumber {
                        attribute,
                        value: item.to_owned(),
                    })?,
            );
        }
    }
    Ok(parsed)
}

fn parse_ecsm_waveform(
    group: &GroupRef<'_>,
    context: &ParseContext<'_>,
) -> Result<(usize, SampledWaveform), LibraryError> {
    let raw_index = required_value(&group.arguments, "ecsm_waveform")?;
    let index = raw_index
        .parse::<usize>()
        .map_err(|_| LibraryError::InvalidNumber {
            attribute: "ecsm_waveform",
            value: raw_index.into_owned(),
        })?;
    let mut coordinates = Vec::new();
    let mut values = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            (
                "index_1",
                super::super::syntax::StatementKind::Simple(items)
                | super::super::syntax::StatementKind::Complex(items),
            ) => coordinates = parse_float_list(&items, "index_1")?,
            (
                "values",
                super::super::syntax::StatementKind::Simple(items)
                | super::super::syntax::StatementKind::Complex(items),
            ) => values = parse_float_list(&items, "values")?,
            _ => {}
        }
    }
    Ok((
        index,
        SampledWaveform {
            reference_time: 0.0,
            coordinates,
            values,
        },
    ))
}

fn parse_ecsm_waveform_set(
    group: &GroupRef<'_>,
    templates: &HashMap<String, TableTemplate>,
    context: &ParseContext<'_>,
) -> Result<(Vec<f64>, Vec<f64>), LibraryError> {
    let template = group
        .arguments
        .first()
        .map(super::super::syntax::Value::decoded)
        .as_deref()
        .and_then(|name| templates.get(name));
    let mut coordinates = None;
    let mut values = Vec::new();
    let mut reader = Reader::new(group.body, context);
    while let Some(statement) = reader.next()? {
        match (statement.name, statement.kind) {
            (
                "index_1",
                super::super::syntax::StatementKind::Simple(items)
                | super::super::syntax::StatementKind::Complex(items),
            ) => coordinates = Some(parse_float_list(&items, "index_1")?),
            (
                "values",
                super::super::syntax::StatementKind::Simple(items)
                | super::super::syntax::StatementKind::Complex(items),
            ) => values = parse_float_list(&items, "values")?,
            _ => {}
        }
    }
    let coordinates = coordinates
        .or_else(|| template.map(|item| item.index_1.to_vec()))
        .unwrap_or_default();
    Ok((coordinates, values))
}

fn parse_ecsm_capacitance(
    body: super::super::syntax::SourceSlice<'_>,
    context: &ParseContext<'_>,
) -> Result<Vec<f64>, LibraryError> {
    let mut values = Vec::new();
    let mut reader = Reader::new(body, context);
    while let Some(statement) = reader.next()? {
        if statement.name == "values"
            && let super::super::syntax::StatementKind::Simple(items)
            | super::super::syntax::StatementKind::Complex(items) = statement.kind
        {
            values = parse_float_list(&items, "values")?;
        }
    }
    Ok(values)
}

fn invalid_ecsm(detail: impl Into<String>) -> LibraryError {
    LibraryError::InvalidTimingModel {
        model: "ECSM",
        detail: detail.into(),
    }
}
