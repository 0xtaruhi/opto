// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod activity;
mod evaluation;
mod state;

pub(crate) use self::state::{PowerAnalysisState, PowerUpdateCounts};
use crate::PowerError;
use opto_runtime::ExecutionContext;
use opto_timing::{TimingElectricalSnapshot, TimingGeneration, TimingModel, TimingNetId};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
/// Validated probabilistic switching model for one timing net.
///
/// Construct values with [`Self::new`]. Deserialization enforces the same
/// finite-value, probability-range, and nonnegative-rate invariants.
pub struct SwitchingActivity {
    /// Probability that the net is logically high.
    static_probability: f64,
    /// Expected transitions per timing-library time unit.
    toggle_rate: f64,
    /// Fraction of transitions that are rising.
    rise_ratio: f64,
}

impl SwitchingActivity {
    /// Validates and constructs switching activity.
    ///
    /// # Errors
    ///
    /// Probabilities must be finite and in `[0, 1]`; toggle rate must be finite
    /// and nonnegative.
    pub fn new(
        static_probability: f64,
        toggle_rate: f64,
        rise_ratio: f64,
    ) -> Result<Self, PowerError> {
        let activity = Self {
            static_probability,
            toggle_rate,
            rise_ratio,
        };
        activity.validate()?;
        Ok(activity)
    }

    fn validate(self) -> Result<(), PowerError> {
        if !self.static_probability.is_finite() || !(0.0..=1.0).contains(&self.static_probability) {
            return Err(PowerError::InvalidActivity {
                detail: "static probability must be finite and in 0..=1".to_string(),
            });
        }
        if !self.toggle_rate.is_finite() || self.toggle_rate < 0.0 {
            return Err(PowerError::InvalidActivity {
                detail: "toggle rate must be finite and nonnegative".to_string(),
            });
        }
        if !self.rise_ratio.is_finite() || !(0.0..=1.0).contains(&self.rise_ratio) {
            return Err(PowerError::InvalidActivity {
                detail: "rise ratio must be finite and in 0..=1".to_string(),
            });
        }
        Ok(())
    }

    #[must_use]
    /// Returns the probability that the net is logically high.
    pub const fn static_probability(self) -> f64 {
        self.static_probability
    }

    #[must_use]
    /// Returns expected transitions per timing-library time unit.
    pub const fn toggle_rate(self) -> f64 {
        self.toggle_rate
    }

    #[must_use]
    /// Returns the fraction of transitions that are rising.
    pub const fn rise_ratio(self) -> f64 {
        self.rise_ratio
    }

    #[must_use]
    /// Returns an unknown but non-switching default state.
    pub const fn quiescent() -> Self {
        Self {
            static_probability: 0.5,
            toggle_rate: 0.0,
            rise_ratio: 0.5,
        }
    }

    #[must_use]
    /// Returns deterministic activity for a constant logic value.
    pub const fn constant(value: bool) -> Self {
        Self {
            static_probability: if value { 1.0 } else { 0.0 },
            toggle_rate: 0.0,
            rise_ratio: 0.5,
        }
    }
}

#[derive(Deserialize)]
struct SwitchingActivityFields {
    static_probability: f64,
    toggle_rate: f64,
    rise_ratio: f64,
}

impl<'de> Deserialize<'de> for SwitchingActivity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = SwitchingActivityFields::deserialize(deserializer)?;
        Self::new(
            fields.static_probability,
            fields.toggle_rate,
            fields.rise_ratio,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Sorted explicit activity overrides for one timing-model generation.
pub struct ActivityAnnotations {
    generation: TimingGeneration,
    values: Box<[(TimingNetId, SwitchingActivity)]>,
}

impl ActivityAnnotations {
    /// Sorts, deduplicates, and validates activity annotations.
    ///
    /// Identical duplicate entries collapse; conflicting duplicates are
    /// rejected.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError::InvalidActivity`] for out-of-range probability,
    /// rise ratio, or toggle rate, and
    /// [`PowerError::ConflictingActivityAnnotation`] when one net has distinct
    /// explicit values.
    pub fn new(
        generation: TimingGeneration,
        entries: impl IntoIterator<Item = (TimingNetId, SwitchingActivity)>,
    ) -> Result<Self, PowerError> {
        let mut values = entries.into_iter().collect::<Vec<_>>();
        for &(_, activity) in &values {
            activity.validate()?;
        }
        values.sort_unstable_by_key(|&(net, _)| net);
        if let Some(pair) = values
            .windows(2)
            .find(|pair| pair[0].0 == pair[1].0 && pair[0].1 != pair[1].1)
        {
            return Err(PowerError::ConflictingActivityAnnotation {
                net: pair[0].0.raw(),
            });
        }
        values.dedup_by_key(|(net, _)| *net);
        Ok(Self {
            generation,
            values: values.into_boxed_slice(),
        })
    }

    #[must_use]
    /// Returns the timing generation to which net IDs belong.
    pub const fn generation(&self) -> TimingGeneration {
        self.generation
    }

    #[must_use]
    /// Returns whether this annotation set contains an explicit value for `net`.
    pub fn contains(&self, net: TimingNetId) -> bool {
        self.values
            .binary_search_by_key(&net, |&(candidate, _)| candidate)
            .is_ok()
    }

    pub(crate) fn get(&self, net: TimingNetId) -> Option<SwitchingActivity> {
        self.values
            .binary_search_by_key(&net, |&(net, _)| net)
            .ok()
            .map(|index| self.values[index].1)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (TimingNetId, SwitchingActivity)> + '_ {
        self.values.iter().copied()
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = TimingNetId> + '_ {
        self.values.iter().map(|&(net, _)| net)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Provenance of a net's effective switching activity.
pub enum ActivityOrigin {
    /// Explicit user annotation.
    Annotated,
    /// Derived through combinational propagation.
    Propagated,
    /// Default activity used when neither source is available.
    Default,
}

#[derive(Debug, Clone, PartialEq)]
/// Borrowed switching contribution and activity for one net.
pub struct NetPower<'a> {
    /// Timing-model net name borrowed from the matching generation.
    pub name: Cow<'a, str>,
    /// Effective activity.
    pub activity: SwitchingActivity,
    /// How the activity was obtained.
    pub origin: ActivityOrigin,
    /// Total net capacitance in library units.
    pub capacitance: f64,
    /// Capacitive switching power in watts.
    pub switching_watts: f64,
    /// Whether a primary input drives the net.
    pub input_port: bool,
}

#[derive(Debug, Clone, PartialEq)]
/// Borrowed internal, switching, and leakage power for one instance.
pub struct CellPower<'a> {
    /// Timing-model instance name borrowed from the matching generation.
    pub name: Cow<'a, str>,
    /// Library cell name.
    pub reference: &'a str,
    /// Liberty internal power in watts.
    pub internal_watts: f64,
    /// Output-net switching power attributed to the cell.
    pub switching_watts: f64,
    /// State-dependent or default leakage in watts.
    pub leakage_watts: f64,
    /// Whether the cell contains sequential state.
    pub sequential: bool,
}

impl CellPower<'_> {
    #[must_use]
    /// Returns internal plus switching power.
    pub fn dynamic_watts(&self) -> f64 {
        self.internal_watts + self.switching_watts
    }

    #[must_use]
    /// Returns dynamic plus leakage power.
    pub fn total_watts(&self) -> f64 {
        self.dynamic_watts() + self.leakage_watts
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
/// Design-wide power totals in watts.
pub struct PowerSummary {
    /// Sum of Liberty internal power.
    pub internal_watts: f64,
    /// Sum of capacitive net switching power.
    pub switching_watts: f64,
    /// Sum of cell leakage power.
    pub leakage_watts: f64,
}

impl PowerSummary {
    #[must_use]
    /// Returns internal plus switching power.
    pub fn dynamic_watts(self) -> f64 {
        self.internal_watts + self.switching_watts
    }

    #[must_use]
    /// Returns dynamic plus leakage power.
    pub fn total_watts(self) -> f64 {
        self.dynamic_watts() + self.leakage_watts
    }
}

#[derive(Debug, Clone, PartialEq)]
/// Library identity printed in a power report.
pub struct PowerLibraryReference {
    /// Declared library name.
    pub name: String,
    /// Source path or alias, when retained.
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellPowerValue {
    pub(crate) internal: f64,
    pub(crate) switching: f64,
    pub(crate) leakage: f64,
}

#[derive(Debug, Clone)]
pub(super) struct PowerAnalysisData {
    pub(super) generation: TimingGeneration,
    pub(super) electrical: TimingElectricalSnapshot,
    pub(super) design: String,
    pub(super) libraries: Arc<[PowerLibraryReference]>,
    pub(super) operating_conditions: Option<String>,
    pub(super) wire_load_mode: Option<String>,
    pub(super) voltage: f64,
    pub(super) voltage_unit_volts: f64,
    pub(super) time_unit_seconds: f64,
    pub(super) capacitance_unit_farads: f64,
    pub(super) dynamic_power_unit_watts: f64,
    pub(super) leakage_power_unit_watts: f64,
    activities: Arc<[state::NetActivity]>,
    pub(super) net_switching_watts: Arc<[f64]>,
    pub(super) cells: Arc<[CellPowerValue]>,
    topology: Arc<state::PowerTopology>,
    pub(super) summary: PowerSummary,
}

#[derive(Debug, Clone)]
/// Immutable Arc-backed compact result of one power analysis.
///
/// Dense numeric columns live in the shared snapshot. Per-net and per-cell
/// report rows are borrowed lazily from a generation-matched timing model, so
/// neither cache hits nor summary queries clone object names or report DTOs.
pub struct PowerAnalysis {
    pub(crate) data: Arc<PowerAnalysisData>,
}

impl PartialEq for PowerAnalysis {
    fn eq(&self, other: &Self) -> bool {
        self.data.generation == other.data.generation
            && self.data.design == other.data.design
            && self.data.libraries == other.data.libraries
            && self.data.operating_conditions == other.data.operating_conditions
            && self.data.wire_load_mode == other.data.wire_load_mode
            && self.data.voltage.to_bits() == other.data.voltage.to_bits()
            && self.data.voltage_unit_volts.to_bits() == other.data.voltage_unit_volts.to_bits()
            && self.data.time_unit_seconds.to_bits() == other.data.time_unit_seconds.to_bits()
            && self.data.capacitance_unit_farads.to_bits()
                == other.data.capacitance_unit_farads.to_bits()
            && self.data.dynamic_power_unit_watts.to_bits()
                == other.data.dynamic_power_unit_watts.to_bits()
            && self.data.leakage_power_unit_watts.to_bits()
                == other.data.leakage_power_unit_watts.to_bits()
            && self.data.activities == other.data.activities
            && self.data.net_switching_watts == other.data.net_switching_watts
            && self.data.cells == other.data.cells
            && self.data.summary == other.data.summary
    }
}

impl PowerAnalysis {
    /// Evaluates one complete immutable power result from a timing topology
    /// and generation-matched net state.
    ///
    /// # Errors
    ///
    /// Returns an error for generation mismatch, invalid activity or electrical
    /// state, missing library units/cells/pins, unsupported Boolean-function
    /// size, cyclic propagation, checked-capacity exhaustion, or runtime failure.
    pub fn analyze(
        runtime: &ExecutionContext,
        model: &TimingModel,
        electrical: &TimingElectricalSnapshot,
        annotations: &ActivityAnnotations,
    ) -> Result<Self, PowerError> {
        Ok(Self::analyze_state(runtime, model, electrical, annotations)?.analysis)
    }

    pub(crate) fn analyze_state(
        runtime: &ExecutionContext,
        model: &TimingModel,
        electrical: &TimingElectricalSnapshot,
        annotations: &ActivityAnnotations,
    ) -> Result<PowerAnalysisState, PowerError> {
        PowerAnalysisState::analyze(runtime, model, electrical, annotations)
    }

    #[must_use]
    /// Returns the timing generation consumed by this analysis.
    pub fn generation(&self) -> TimingGeneration {
        self.data.generation
    }

    #[must_use]
    /// Returns the design name captured by this analysis.
    pub fn design(&self) -> &str {
        &self.data.design
    }

    #[must_use]
    /// Returns the ordered library identities printed in reports.
    pub fn libraries(&self) -> &[PowerLibraryReference] {
        &self.data.libraries
    }

    #[must_use]
    /// Returns the selected operating-condition name, when available.
    pub fn operating_conditions(&self) -> Option<&str> {
        self.data.operating_conditions.as_deref()
    }

    #[must_use]
    /// Returns the selected wire-load mode, when available.
    pub fn wire_load_mode(&self) -> Option<&str> {
        self.data.wire_load_mode.as_deref()
    }

    #[must_use]
    /// Returns the analysis voltage in library voltage units.
    pub fn voltage(&self) -> f64 {
        self.data.voltage
    }

    #[must_use]
    /// Returns the number of volts represented by one library voltage unit.
    pub fn voltage_unit_volts(&self) -> f64 {
        self.data.voltage_unit_volts
    }

    #[must_use]
    /// Returns the number of seconds represented by one library time unit.
    pub fn time_unit_seconds(&self) -> f64 {
        self.data.time_unit_seconds
    }

    #[must_use]
    /// Returns the number of farads represented by one capacitance unit.
    pub fn capacitance_unit_farads(&self) -> f64 {
        self.data.capacitance_unit_farads
    }

    #[must_use]
    /// Returns the watt scale used for dynamic power values.
    pub fn dynamic_power_unit_watts(&self) -> f64 {
        self.data.dynamic_power_unit_watts
    }

    #[must_use]
    /// Returns the watt scale used for leakage power values.
    pub fn leakage_power_unit_watts(&self) -> f64 {
        self.data.leakage_power_unit_watts
    }

    #[must_use]
    /// Returns the aggregate power totals in watts.
    pub fn summary(&self) -> PowerSummary {
        self.data.summary
    }

    /// Returns borrowed per-cell report rows in deterministic design order.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError::GenerationMismatch`] unless `model` is the same
    /// sealed generation and has the same dense cell/net shape as the analysis.
    pub fn cells<'a>(
        &'a self,
        model: &'a TimingModel,
    ) -> Result<impl ExactSizeIterator<Item = CellPower<'a>> + 'a, PowerError> {
        self.validate_model(model)?;
        Ok(model
            .instances()
            .zip(self.data.cells.iter().copied())
            .zip(self.data.topology.sequential.iter().copied())
            .map(|((instance, value), sequential)| CellPower {
                name: instance.name(),
                reference: instance.cell(),
                internal_watts: value.internal,
                switching_watts: value.switching,
                leakage_watts: value.leakage,
                sequential,
            }))
    }

    /// Returns borrowed per-net report rows in deterministic dense-net order.
    ///
    /// # Errors
    ///
    /// Returns [`PowerError::GenerationMismatch`] unless `model` is the same
    /// sealed generation and has the same dense cell/net shape as the analysis.
    ///
    /// # Panics
    ///
    /// Panics if a validated generation-matched timing/electrical snapshot is
    /// internally missing a report net or its name.
    pub fn nets<'a>(
        &'a self,
        model: &'a TimingModel,
    ) -> Result<impl ExactSizeIterator<Item = NetPower<'a>> + 'a, PowerError> {
        self.validate_model(model)?;
        Ok(self.data.topology.report_nets.iter().copied().map(|net| {
            let row = net.raw() as usize;
            let electrical = self
                .data
                .electrical
                .get(net)
                .expect("validated power snapshot covers every report net");
            let activity = self.data.activities[row];
            NetPower {
                name: model
                    .net_name(net)
                    .expect("power report nets belong to the timing model"),
                activity: activity.value,
                origin: activity.origin,
                capacitance: electrical.capacitance,
                switching_watts: self.data.net_switching_watts[row],
                input_port: model.net_is_input_port(net),
            }
        }))
    }

    /// Replaces only small report metadata while sharing every dense column.
    #[must_use]
    pub fn with_libraries(self, libraries: Vec<PowerLibraryReference>) -> Self {
        let mut data = (*self.data).clone();
        data.libraries = libraries.into();
        Self {
            data: Arc::new(data),
        }
    }

    fn validate_model(&self, model: &TimingModel) -> Result<(), PowerError> {
        if model.generation() != self.data.generation
            || model.net_count() != self.data.activities.len()
            || model.instance_count() != self.data.cells.len()
        {
            return Err(PowerError::GenerationMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_activity_accepts_closed_probability_boundaries() {
        assert_eq!(
            SwitchingActivity::new(0.0, 0.0, 1.0).unwrap(),
            SwitchingActivity {
                static_probability: 0.0,
                toggle_rate: 0.0,
                rise_ratio: 1.0,
            }
        );
    }

    #[test]
    fn switching_activity_rejects_non_finite_and_out_of_range_values() {
        for result in [
            SwitchingActivity::new(f64::NAN, 0.0, 0.5),
            SwitchingActivity::new(-0.1, 0.0, 0.5),
            SwitchingActivity::new(1.1, 0.0, 0.5),
            SwitchingActivity::new(0.5, -0.1, 0.5),
            SwitchingActivity::new(0.5, f64::INFINITY, 0.5),
            SwitchingActivity::new(0.5, 0.0, 1.1),
        ] {
            assert!(result.is_err());
        }
    }

    #[test]
    fn switching_activity_deserialization_revalidates_fields() {
        let fields = [
            ("static_probability", 0.5),
            ("toggle_rate", -0.1),
            ("rise_ratio", 0.5),
        ];
        let deserializer = serde::de::value::MapDeserializer::<_, serde::de::value::Error>::new(
            fields.into_iter(),
        );
        let error = SwitchingActivity::deserialize(deserializer).unwrap_err();
        assert!(error.to_string().contains("toggle rate"));
    }

    #[test]
    fn quiescent_activity_has_no_transitions() {
        let activity = SwitchingActivity::quiescent();
        assert_eq!(activity.static_probability, 0.5);
        assert_eq!(activity.toggle_rate, 0.0);
        assert_eq!(activity.rise_ratio, 0.5);
    }

    #[test]
    fn power_totals_compose_dynamic_and_leakage_components() {
        let cell = CellPower {
            name: Cow::Borrowed("u0"),
            reference: "INV",
            internal_watts: 1.25,
            switching_watts: 2.5,
            leakage_watts: 0.25,
            sequential: false,
        };
        assert_eq!(cell.dynamic_watts(), 3.75);
        assert_eq!(cell.total_watts(), 4.0);

        let summary = PowerSummary {
            internal_watts: 1.25,
            switching_watts: 2.5,
            leakage_watts: 0.25,
        };
        assert_eq!(summary.dynamic_watts(), 3.75);
        assert_eq!(summary.total_watts(), 4.0);
    }
}
