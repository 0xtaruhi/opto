// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! NLDM, CCS, and ECSM timing and waveform models.
//!
//! Scalar delay/slew tables provide the common analysis interface. CCS and
//! ECSM retain validated waveform data for consumers that need receiver
//! capacitance or normalized driver shapes.

use crate::{LibraryError, LookupTable, TimingEdge};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod waveform;

pub use waveform::{NormalizedDriverWaveform, SampledWaveform, SampledWaveformGrid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Characterization family used by a timing arc.
pub enum TimingModelKind {
    /// Non-linear delay model.
    Nldm,
    /// Composite current source model.
    Ccs,
    /// Effective-current source model.
    Ecsm,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
/// Voltage thresholds used to interpret timing waveforms.
pub struct TimingThresholds {
    /// Input switching threshold by [`TimingEdge::index`].
    pub input: [f64; 2],
    /// Output switching threshold by [`TimingEdge::index`].
    pub output: [f64; 2],
    /// Lower slew threshold by edge.
    pub slew_lower: [f64; 2],
    /// Upper slew threshold by edge.
    pub slew_upper: [f64; 2],
    /// Scale from library slew definition to reported slew.
    pub slew_derate: f64,
}

impl Default for TimingThresholds {
    fn default() -> Self {
        Self {
            input: [0.5; 2],
            output: [0.5; 2],
            slew_lower: [0.2; 2],
            slew_upper: [0.8; 2],
            slew_derate: 1.0,
        }
    }
}

impl TimingThresholds {
    pub(crate) fn validate(self, model: &'static str) -> Result<Self, LibraryError> {
        for (name, values) in [
            ("input threshold", self.input),
            ("output threshold", self.output),
            ("slew lower threshold", self.slew_lower),
            ("slew upper threshold", self.slew_upper),
        ] {
            if values
                .into_iter()
                .any(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
            {
                return Err(invalid_model(
                    model,
                    format!("{name} must be a finite fraction between zero and one"),
                ));
            }
        }
        for edge in TimingEdge::ALL {
            let index = edge.index();
            if self.slew_lower[index] >= self.slew_upper[index] {
                return Err(invalid_model(
                    model,
                    format!("{} slew thresholds are not increasing", edge_name(edge)),
                ));
            }
        }
        if !self.slew_derate.is_finite() || self.slew_derate <= 0.0 {
            return Err(invalid_model(
                model,
                "slew_derate_from_library must be positive and finite",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// NLDM propagation-delay and output-transition tables.
pub struct NldmTimingModel {
    /// Rising output delay.
    pub cell_rise: Option<LookupTable>,
    /// Falling output delay.
    pub cell_fall: Option<LookupTable>,
    /// Rising output transition.
    pub rise_transition: Option<LookupTable>,
    /// Falling output transition.
    pub fall_transition: Option<LookupTable>,
}

impl NldmTimingModel {
    /// Constructs an NLDM model from its optional edge tables.
    #[must_use]
    pub fn new(
        cell_rise: Option<LookupTable>,
        cell_fall: Option<LookupTable>,
        rise_transition: Option<LookupTable>,
        fall_transition: Option<LookupTable>,
    ) -> Self {
        Self {
            cell_rise,
            cell_fall,
            rise_transition,
            fall_transition,
        }
    }

    fn delay_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        match edge {
            TimingEdge::Rise => self.cell_rise.as_ref(),
            TimingEdge::Fall => self.cell_fall.as_ref(),
        }
    }

    fn transition_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        match edge {
            TimingEdge::Rise => self.rise_transition.as_ref(),
            TimingEdge::Fall => self.fall_transition.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
/// CCS segmented receiver-capacitance tables.
pub struct ReceiverCapacitanceModel {
    /// First segment for a rising input.
    pub segment_1_rise: Option<LookupTable>,
    /// First segment for a falling input.
    pub segment_1_fall: Option<LookupTable>,
    /// Second segment for a rising input.
    pub segment_2_rise: Option<LookupTable>,
    /// Second segment for a falling input.
    pub segment_2_fall: Option<LookupTable>,
}

impl ReceiverCapacitanceModel {
    #[must_use]
    /// Returns `true` when no segment table is present.
    pub fn is_empty(&self) -> bool {
        self.segment_1_rise.is_none()
            && self.segment_1_fall.is_none()
            && self.segment_2_rise.is_none()
            && self.segment_2_fall.is_none()
    }

    #[must_use]
    /// Interpolates and averages available capacitance segments.
    pub fn capacitance_at(
        &self,
        edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        let (first, second) = match edge {
            TimingEdge::Rise => (&self.segment_1_rise, &self.segment_2_rise),
            TimingEdge::Fall => (&self.segment_1_fall, &self.segment_2_fall),
        };
        average_optional(
            first
                .as_ref()
                .and_then(|table| table.value_at(input_transition, output_load)),
            second
                .as_ref()
                .and_then(|table| table.value_at(input_transition, output_load)),
        )
    }

    pub(crate) fn validate(&self, model: &'static str) -> Result<(), LibraryError> {
        for (name, table) in [
            ("receiver_capacitance1_rise", &self.segment_1_rise),
            ("receiver_capacitance1_fall", &self.segment_1_fall),
            ("receiver_capacitance2_rise", &self.segment_2_rise),
            ("receiver_capacitance2_fall", &self.segment_2_fall),
        ] {
            if let Some(table) = table {
                validate_capacitance_table(model, name, table)?;
            }
        }
        Ok(())
    }

    pub(crate) fn depends_on_output_load(&self) -> bool {
        [
            &self.segment_1_rise,
            &self.segment_1_fall,
            &self.segment_2_rise,
            &self.segment_2_fall,
        ]
        .into_iter()
        .flatten()
        .any(|table| !table.index_2.is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
/// ECSM pin-level receiver-capacitance tables.
pub struct EcsmPinReceiverCapacitanceModel {
    /// Rising-input capacitance table.
    pub rise: Option<LookupTable>,
    /// Falling-input capacitance table.
    pub fall: Option<LookupTable>,
}

impl EcsmPinReceiverCapacitanceModel {
    pub(crate) fn validate(&self) -> Result<(), LibraryError> {
        for (name, table) in [
            ("pin rise ecsm_capacitance", &self.rise),
            ("pin fall ecsm_capacitance", &self.fall),
        ] {
            if let Some(table) = table {
                validate_capacitance_table("ECSM", name, table)?;
                if !table.index_2.is_empty() {
                    return Err(invalid_model(
                        "ECSM",
                        format!("{name} cannot depend on an output load"),
                    ));
                }
            }
        }
        Ok(())
    }

    #[must_use]
    /// Returns `true` when neither edge is characterized.
    pub fn is_empty(&self) -> bool {
        self.rise.is_none() && self.fall.is_none()
    }

    #[must_use]
    /// Interpolates receiver capacitance for an input edge and transition.
    pub fn capacitance_at(&self, edge: TimingEdge, input_transition: Option<f64>) -> Option<f64> {
        match edge {
            TimingEdge::Rise => &self.rise,
            TimingEdge::Fall => &self.fall,
        }
        .as_ref()
        .and_then(|table| table.value_at(input_transition, None))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Pin receiver-capacitance characterization selected by model family.
pub enum PinReceiverCapacitanceModel {
    /// CCS segmented model.
    Ccs(ReceiverCapacitanceModel),
    /// ECSM pin-level model.
    Ecsm(EcsmPinReceiverCapacitanceModel),
}

impl PinReceiverCapacitanceModel {
    #[must_use]
    /// Interpolates capacitance for an input edge and transition.
    pub fn capacitance_at(&self, edge: TimingEdge, input_transition: Option<f64>) -> Option<f64> {
        match self {
            Self::Ccs(model) => model.capacitance_at(edge, input_transition, None),
            Self::Ecsm(model) => model.capacitance_at(edge, input_transition),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Composite current source timing model.
pub struct CcsTimingModel {
    /// Waveform interpretation thresholds.
    pub thresholds: TimingThresholds,
    /// Scalar delay and slew anchor tables.
    pub scalar: NldmTimingModel,
    /// Scale converting integrated charge to normalized voltage.
    pub charge_to_normalized_voltage: f64,
    /// Segmented receiver-capacitance characterization.
    pub receiver_capacitance: ReceiverCapacitanceModel,
    /// Rising-output current waveforms.
    pub output_current_rise: Option<SampledWaveformGrid>,
    /// Falling-output current waveforms.
    pub output_current_fall: Option<SampledWaveformGrid>,
}

impl CcsTimingModel {
    /// Validates and constructs a CCS model.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::InvalidTimingModel`] for invalid thresholds,
    /// scale factors, missing scalar anchors, capacitance tables, or current
    /// waveform coordinates.
    ///
    /// # Panics
    ///
    /// Panics only if a waveform grid passes its structural validation but then
    /// fails to expose a vector in the validated offset range.
    pub fn new(
        thresholds: TimingThresholds,
        charge_to_normalized_voltage: f64,
        scalar: NldmTimingModel,
        receiver_capacitance: ReceiverCapacitanceModel,
        output_current_rise: Option<SampledWaveformGrid>,
        output_current_fall: Option<SampledWaveformGrid>,
    ) -> Result<Self, LibraryError> {
        let thresholds = thresholds.validate("CCS")?;
        if !charge_to_normalized_voltage.is_finite() || charge_to_normalized_voltage <= 0.0 {
            return Err(invalid_model(
                "CCS",
                "library current, time, capacitance, and voltage units must define a positive scale",
            ));
        }
        if output_current_rise.is_none() && output_current_fall.is_none() {
            return Err(invalid_model(
                "CCS",
                "no output current vectors were provided",
            ));
        }
        validate_scalar_anchors(
            "CCS",
            &scalar,
            output_current_rise.is_some(),
            output_current_fall.is_some(),
        )?;
        receiver_capacitance.validate("CCS")?;
        for grid in [&output_current_rise, &output_current_fall]
            .into_iter()
            .flatten()
        {
            for index in 0..grid.waveform_count() {
                let waveform = grid
                    .waveform(index)
                    .expect("validated waveform offsets cover every CCS vector");
                if waveform
                    .coordinates
                    .windows(2)
                    .any(|pair| pair[0] >= pair[1])
                {
                    return Err(invalid_model(
                        "CCS",
                        format!("current waveform {index} time samples are not increasing"),
                    ));
                }
            }
        }
        Ok(Self {
            thresholds,
            scalar,
            charge_to_normalized_voltage,
            receiver_capacitance,
            output_current_rise,
            output_current_fall,
        })
    }

    fn delay_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        self.scalar.delay_table(edge)
    }

    fn transition_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        self.scalar.transition_table(edge)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Effective-current source timing model.
pub struct EcsmTimingModel {
    /// Waveform interpretation thresholds.
    pub thresholds: TimingThresholds,
    /// Scalar delay and slew anchor tables.
    pub scalar: NldmTimingModel,
    /// Rising-output voltage waveforms.
    pub waveform_rise: Option<SampledWaveformGrid>,
    /// Falling-output voltage waveforms.
    pub waveform_fall: Option<SampledWaveformGrid>,
    /// Effective capacitance for a rising output.
    pub capacitance_rise: Option<LookupTable>,
    /// Effective capacitance for a falling output.
    pub capacitance_fall: Option<LookupTable>,
}

impl EcsmTimingModel {
    /// Validates and constructs an ECSM model.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::InvalidTimingModel`] for invalid thresholds,
    /// missing waveforms or scalar anchors, invalid capacitance, or
    /// non-increasing voltage waveform samples.
    ///
    /// # Panics
    ///
    /// Panics only if a waveform grid passes its structural validation but then
    /// fails to expose a vector in the validated offset range.
    pub fn new(
        thresholds: TimingThresholds,
        scalar: NldmTimingModel,
        waveform_rise: Option<SampledWaveformGrid>,
        waveform_fall: Option<SampledWaveformGrid>,
        capacitance_rise: Option<LookupTable>,
        capacitance_fall: Option<LookupTable>,
    ) -> Result<Self, LibraryError> {
        let thresholds = thresholds.validate("ECSM")?;
        if waveform_rise.is_none() && waveform_fall.is_none() {
            return Err(invalid_model("ECSM", "no voltage waveforms were provided"));
        }
        validate_scalar_anchors(
            "ECSM",
            &scalar,
            waveform_rise.is_some(),
            waveform_fall.is_some(),
        )?;
        for (name, table) in [
            ("rise ecsm_capacitance", &capacitance_rise),
            ("fall ecsm_capacitance", &capacitance_fall),
        ] {
            if let Some(table) = table {
                validate_capacitance_table("ECSM", name, table)?;
            }
        }
        for grid in [&waveform_rise, &waveform_fall].into_iter().flatten() {
            for index in 0..grid.waveform_count() {
                let waveform = grid
                    .waveform(index)
                    .expect("validated waveform offsets cover every ECSM vector");
                if waveform.values.windows(2).any(|pair| pair[0] >= pair[1]) {
                    return Err(invalid_model(
                        "ECSM",
                        format!("voltage waveform {index} time samples are not increasing"),
                    ));
                }
            }
        }
        Ok(Self {
            thresholds,
            scalar,
            waveform_rise,
            waveform_fall,
            capacitance_rise,
            capacitance_fall,
        })
    }

    fn delay_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        self.scalar.delay_table(edge)
    }

    fn transition_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        self.scalar.transition_table(edge)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Propagation model attached to one target timing arc.
pub enum ArcDelayModel {
    /// NLDM scalar characterization.
    Nldm(NldmTimingModel),
    /// CCS current-source characterization.
    Ccs(CcsTimingModel),
    /// ECSM voltage-waveform characterization.
    Ecsm(EcsmTimingModel),
}

impl ArcDelayModel {
    #[must_use]
    /// Returns the characterization model family.
    pub const fn kind(&self) -> TimingModelKind {
        match self {
            Self::Nldm(_) => TimingModelKind::Nldm,
            Self::Ccs(_) => TimingModelKind::Ccs,
            Self::Ecsm(_) => TimingModelKind::Ecsm,
        }
    }

    #[must_use]
    /// Returns the greatest available default rise/fall delay.
    pub fn default_delay(&self) -> Option<f64> {
        crate::max_optional_f64(
            self.delay_table(TimingEdge::Rise)
                .and_then(LookupTable::default_value),
            self.delay_table(TimingEdge::Fall)
                .and_then(LookupTable::default_value),
        )
    }

    #[must_use]
    /// Returns the greatest available default rise/fall transition.
    pub fn default_transition(&self) -> Option<f64> {
        crate::max_optional_f64(
            self.transition_table(TimingEdge::Rise)
                .and_then(LookupTable::default_value),
            self.transition_table(TimingEdge::Fall)
                .and_then(LookupTable::default_value),
        )
    }

    #[must_use]
    /// Returns the greatest output load covered by every available delay table.
    ///
    /// A timing-driven constructor can use this as the characterized search
    /// domain instead of inventing a technology-independent fanout limit.
    pub fn maximum_characterized_output_load(&self) -> Option<f64> {
        [
            self.delay_table(TimingEdge::Rise),
            self.delay_table(TimingEdge::Fall),
        ]
        .into_iter()
        .flatten()
        .filter_map(LookupTable::maximum_output_load)
        .min_by(f64::total_cmp)
    }

    #[must_use]
    /// Interpolates propagation delay for an output edge.
    pub fn delay_at(
        &self,
        edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        self.delay_table(edge)
            .and_then(|table| table.value_at(input_transition, output_load))
    }

    #[must_use]
    /// Interpolates output transition for an output edge.
    pub fn transition_at(
        &self,
        edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        self.transition_table(edge)
            .and_then(|table| table.value_at(input_transition, output_load))
    }

    #[must_use]
    /// Interpolates effective receiver capacitance for an edge pair.
    pub fn receiver_capacitance_at(
        &self,
        input_edge: TimingEdge,
        output_edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<f64> {
        match self {
            Self::Nldm(_) => None,
            Self::Ccs(model) => {
                model
                    .receiver_capacitance
                    .capacitance_at(input_edge, input_transition, output_load)
            }
            Self::Ecsm(model) => match output_edge {
                TimingEdge::Rise => model.capacitance_rise.as_ref(),
                TimingEdge::Fall => model.capacitance_fall.as_ref(),
            }
            .and_then(|table| table.value_at(input_transition, output_load)),
        }
    }

    #[must_use]
    /// Interpolates and integrates a CCS current waveform.
    ///
    /// Returns `None` for non-CCS models or unusable waveform data.
    pub fn driver_waveform_at(
        &self,
        edge: TimingEdge,
        input_transition: Option<f64>,
        output_load: Option<f64>,
    ) -> Option<NormalizedDriverWaveform> {
        let Self::Ccs(model) = self else {
            return None;
        };
        let grid = match edge {
            TimingEdge::Rise => model.output_current_rise.as_ref(),
            TimingEdge::Fall => model.output_current_fall.as_ref(),
        }?;
        let current = grid.interpolated_waveform(input_transition, output_load)?;
        if current.coordinates.len() < 2 {
            return None;
        }
        let mut charge = Vec::with_capacity(current.coordinates.len());
        charge.push(0.0);
        let mut accumulated = 0.0;
        for index in 1..current.coordinates.len() {
            let step = current.coordinates[index] - current.coordinates[index - 1];
            accumulated += f64::midpoint(current.values[index - 1], current.values[index]) * step;
            charge.push(accumulated);
        }
        let final_charge = *charge.last()?;
        if !final_charge.is_finite() || final_charge.abs() <= f64::MIN_POSITIVE {
            return None;
        }
        let direction = final_charge.signum();
        let scale = model.charge_to_normalized_voltage;
        let final_voltage = final_charge.abs() * scale;
        let normalization = if final_voltage > f64::MIN_POSITIVE {
            final_voltage
        } else {
            1.0
        };
        let mut previous = 0.0_f64;
        let normalized_voltage = charge
            .into_iter()
            .map(|charge| {
                let value = (direction * charge * scale / normalization).clamp(0.0, 1.0);
                previous = previous.max(value);
                previous
            })
            .collect();
        Some(NormalizedDriverWaveform {
            times: current.coordinates,
            normalized_voltage,
        })
    }

    fn delay_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        match self {
            Self::Nldm(model) => model.delay_table(edge),
            Self::Ccs(model) => model.delay_table(edge),
            Self::Ecsm(model) => model.delay_table(edge),
        }
    }

    fn transition_table(&self, edge: TimingEdge) -> Option<&LookupTable> {
        match self {
            Self::Nldm(model) => model.transition_table(edge),
            Self::Ccs(model) => model.transition_table(edge),
            Self::Ecsm(model) => model.transition_table(edge),
        }
    }
}

fn interpolation_span(axis: &[f64], query: Option<f64>) -> (usize, usize, f64) {
    if axis.len() <= 1 {
        return (0, 0, 0.0);
    }
    let query = query.unwrap_or(axis[0]);
    if query <= axis[0] {
        return (0, 0, 0.0);
    }
    let last = axis.len() - 1;
    if query >= axis[last] {
        return (last, last, 0.0);
    }
    let upper = axis.partition_point(|value| *value < query);
    let lower = upper - 1;
    let ratio = (query - axis[lower]) / (axis[upper] - axis[lower]);
    (lower, upper, ratio)
}

fn sample_waveform(waveform: WaveformRef<'_>, coordinate: f64) -> f64 {
    if coordinate <= waveform.coordinates[0] {
        return waveform.values[0];
    }
    let last = waveform.coordinates.len() - 1;
    if coordinate >= waveform.coordinates[last] {
        return waveform.values[last];
    }
    let upper = waveform
        .coordinates
        .partition_point(|value| *value < coordinate);
    let lower = upper - 1;
    let ratio = (coordinate - waveform.coordinates[lower])
        / (waveform.coordinates[upper] - waveform.coordinates[lower]);
    waveform.values[lower] + ratio * (waveform.values[upper] - waveform.values[lower])
}

#[derive(Clone, Copy)]
struct WaveformRef<'a> {
    coordinates: &'a [f64],
    values: &'a [f64],
}

fn validate_axis(
    model: &'static str,
    name: &'static str,
    axis: &[f64],
) -> Result<(), LibraryError> {
    if axis.iter().any(|value| !value.is_finite()) {
        return Err(invalid_model(
            model,
            format!("{name} contains a non-finite value"),
        ));
    }
    if axis.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_model(
            model,
            format!("{name} must be strictly increasing"),
        ));
    }
    Ok(())
}

fn validate_capacitance_table(
    model: &'static str,
    name: &str,
    table: &LookupTable,
) -> Result<(), LibraryError> {
    validate_axis(model, "capacitance index_1", &table.index_1)?;
    validate_axis(model, "capacitance index_2", &table.index_2)?;
    let expected = table
        .index_1
        .len()
        .max(1)
        .checked_mul(table.index_2.len().max(1))
        .ok_or_else(|| invalid_model(model, format!("{name} dimensions exceed host capacity")))?;
    if table.values.len() != expected {
        return Err(invalid_model(
            model,
            format!(
                "{name} requires {expected} values but contains {}",
                table.values.len()
            ),
        ));
    }
    if table
        .values
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(invalid_model(
            model,
            format!("{name} values must be finite and non-negative"),
        ));
    }
    Ok(())
}

fn validate_scalar_anchors(
    model: &'static str,
    scalar: &NldmTimingModel,
    has_rise_waveform: bool,
    has_fall_waveform: bool,
) -> Result<(), LibraryError> {
    for (edge, has_waveform) in [
        (TimingEdge::Rise, has_rise_waveform),
        (TimingEdge::Fall, has_fall_waveform),
    ] {
        if has_waveform
            && (scalar.delay_table(edge).is_none() || scalar.transition_table(edge).is_none())
        {
            return Err(invalid_model(
                model,
                format!(
                    "a {model} {} waveform requires both scalar cell delay and transition tables",
                    edge_name(edge)
                ),
            ));
        }
    }
    Ok(())
}

fn strictly_monotonic(values: &[f64]) -> bool {
    let increasing = values.windows(2).all(|pair| pair[0] < pair[1]);
    let decreasing = values.windows(2).all(|pair| pair[0] > pair[1]);
    increasing || decreasing
}

fn average_optional(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(f64::midpoint(left, right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn invalid_model(model: &'static str, detail: impl Into<String>) -> LibraryError {
    LibraryError::InvalidTimingModel {
        model,
        detail: detail.into(),
    }
}

fn edge_name(edge: TimingEdge) -> &'static str {
    match edge {
        TimingEdge::Rise => "rise",
        TimingEdge::Fall => "fall",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecsm_uses_scalar_delay_and_slew_tables() {
        let grid = SampledWaveformGrid::new(
            "ECSM",
            vec![1.0],
            vec![1.0],
            vec![SampledWaveform {
                reference_time: 0.0,
                coordinates: vec![0.0, 0.5, 1.0],
                values: vec![0.0, 0.4, 1.0],
            }],
        )
        .unwrap();
        let scalar = NldmTimingModel::new(
            Some(LookupTable::scalar(0.7)),
            None,
            Some(LookupTable::scalar(0.9)),
            None,
        );
        let model = EcsmTimingModel::new(
            TimingThresholds::default(),
            scalar,
            Some(grid),
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            model.delay_table(TimingEdge::Rise).unwrap().default_value(),
            Some(0.7)
        );
        assert_eq!(
            model
                .transition_table(TimingEdge::Rise)
                .unwrap()
                .default_value(),
            Some(0.9)
        );
    }

    #[test]
    fn ccs_uses_scalar_delay_and_slew_tables() {
        let grid = SampledWaveformGrid::new(
            "CCS",
            vec![1.0],
            vec![1.0],
            vec![SampledWaveform {
                reference_time: 0.0,
                coordinates: vec![0.0, 1.0],
                values: vec![1.0, 1.0],
            }],
        )
        .unwrap();
        let scalar = NldmTimingModel::new(
            Some(LookupTable::scalar(0.7)),
            None,
            Some(LookupTable::scalar(0.9)),
            None,
        );
        let model = CcsTimingModel::new(
            TimingThresholds::default(),
            1.0,
            scalar,
            ReceiverCapacitanceModel::default(),
            Some(grid),
            None,
        )
        .unwrap();

        assert_eq!(
            model.delay_table(TimingEdge::Rise).unwrap().default_value(),
            Some(0.7)
        );
        assert_eq!(
            model
                .transition_table(TimingEdge::Rise)
                .unwrap()
                .default_value(),
            Some(0.9)
        );

        let waveform = ArcDelayModel::Ccs(model)
            .driver_waveform_at(TimingEdge::Rise, Some(1.0), Some(1.0))
            .unwrap();
        assert_eq!(waveform.times, vec![0.0, 1.0]);
        assert_eq!(waveform.normalized_voltage, vec![0.0, 1.0]);
    }
}
