// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::required_value;
use crate::LibraryError;

#[derive(Debug, Default)]
pub(super) struct UnitScales {
    pub(super) time: Option<f64>,
    pub(super) current: Option<f64>,
    pub(super) voltage: Option<f64>,
    pub(super) capacitance: Option<f64>,
    pub(super) resistance: Option<f64>,
    pub(super) leakage_power: Option<f64>,
}

#[derive(Clone, Copy)]
pub(super) enum UnitDimension {
    Time,
    Current,
    Voltage,
    Resistance,
    Power,
}

pub(super) fn parse_scalar_unit(
    values: &super::super::syntax::Values<'_>,
    attribute: &'static str,
    dimension: UnitDimension,
) -> Result<f64, LibraryError> {
    let raw = required_value(values, attribute)?;
    let split = raw
        .find(|character: char| character.is_ascii_alphabetic())
        .ok_or_else(|| invalid_unit(attribute, &raw))?;
    let (factor, suffix) = raw.split_at(split);
    let factor = factor
        .trim()
        .parse::<f64>()
        .map_err(|_| invalid_unit(attribute, &raw))?;
    let normalized = suffix.trim().to_ascii_lowercase();
    let scale = match dimension {
        UnitDimension::Time => match normalized.as_str() {
            "s" => 1.0,
            "ms" => 1e-3,
            "us" => 1e-6,
            "ns" => 1e-9,
            "ps" => 1e-12,
            "fs" => 1e-15,
            _ => return Err(invalid_unit(attribute, &raw)),
        },
        UnitDimension::Current => match normalized.as_str() {
            "a" => 1.0,
            "ma" => 1e-3,
            "ua" => 1e-6,
            "na" => 1e-9,
            _ => return Err(invalid_unit(attribute, &raw)),
        },
        UnitDimension::Voltage => match normalized.as_str() {
            "v" => 1.0,
            "mv" => 1e-3,
            _ => return Err(invalid_unit(attribute, &raw)),
        },
        UnitDimension::Resistance => match normalized.as_str() {
            "ohm" => 1.0,
            "kohm" => 1e3,
            _ => return Err(invalid_unit(attribute, &raw)),
        },
        UnitDimension::Power => match normalized.as_str() {
            "w" => 1.0,
            "mw" => 1e-3,
            "uw" => 1e-6,
            "nw" => 1e-9,
            "pw" => 1e-12,
            "fw" => 1e-15,
            _ => return Err(invalid_unit(attribute, &raw)),
        },
    };
    validate_scale(attribute, factor * scale)
}

pub(super) fn parse_capacitance_unit(
    values: &super::super::syntax::Values<'_>,
) -> Result<f64, LibraryError> {
    let Some(factor) = values.first().map(super::super::syntax::Value::decoded) else {
        return Err(LibraryError::MissingValue {
            attribute: "capacitive_load_unit",
        });
    };
    let Some(suffix) = values.get(1).map(super::super::syntax::Value::decoded) else {
        return Err(LibraryError::MissingValue {
            attribute: "capacitive_load_unit",
        });
    };
    let factor = factor
        .parse::<f64>()
        .map_err(|_| invalid_unit("capacitive_load_unit", &factor))?;
    let scale = match suffix.trim().to_ascii_lowercase().as_str() {
        "f" => 1.0,
        "mf" => 1e-3,
        "uf" => 1e-6,
        "nf" => 1e-9,
        "pf" => 1e-12,
        "ff" => 1e-15,
        _ => return Err(invalid_unit("capacitive_load_unit", &suffix)),
    };
    validate_scale("capacitive_load_unit", factor * scale)
}

fn validate_scale(attribute: &'static str, scale: f64) -> Result<f64, LibraryError> {
    if scale.is_finite() && scale > 0.0 {
        Ok(scale)
    } else {
        Err(LibraryError::InvalidTimingModel {
            model: "CCS",
            detail: format!("library attribute '{attribute}' must define a positive finite unit"),
        })
    }
}

fn invalid_unit(attribute: &'static str, value: &str) -> LibraryError {
    LibraryError::InvalidTimingModel {
        model: "CCS",
        detail: format!("unsupported {attribute} value '{value}'"),
    }
}
