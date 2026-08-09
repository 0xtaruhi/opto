// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{PowerLibrary, TargetCellSet};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Exact structural schema required to reuse a sealed timing graph.
///
/// The digest covers ordered cell, pin, timing-arc, Boolean-function, and
/// sequential semantics. Numeric delay, capacitance, area, and power data are
/// deliberately excluded, so equal schemas are the fail-closed condition for
/// sharing topology across characterized views.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimingTopologySchema {
    fingerprint: [u8; 32],
    canonical: Arc<[u8]>,
}

impl TimingTopologySchema {
    #[must_use]
    /// Returns the structural fingerprint bytes.
    pub const fn bytes(&self) -> [u8; 32] {
        self.fingerprint
    }
}

#[derive(Debug, Default, Clone)]
/// Materialized timing and power view selected for one synthesis session.
pub struct TimingLibrary {
    /// Selected Liberty library name.
    pub name: Option<String>,
    /// Selected operating-condition group.
    pub operating_conditions: Option<String>,
    /// Selected wire-load name.
    pub wire_load: Option<String>,
    /// Selected wire-load application mode.
    pub wire_load_mode: Option<String>,
    /// Resolved wire-load model.
    pub wire_load_model: Option<WireLoadModel>,
    /// SI scale factors parsed from the selected library.
    pub units: TimingLibraryUnits,
    /// Power characterization aligned with the target cells.
    pub power: PowerLibrary,
    /// Canonical synthesis-facing target cells.
    pub cells: TargetCellSet,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Piecewise-linear Liberty wire-load estimate.
pub struct WireLoadModel {
    /// Wire-load group name.
    pub name: String,
    capacitance_per_length: f64,
    resistance_per_length: f64,
    slope: f64,
    fanout_lengths: Arc<[(f64, f64)]>,
}

impl WireLoadModel {
    /// Validates and constructs a wire-load model.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LibraryError::InvalidWireLoad`] for non-finite or
    /// negative coefficients, invalid fanout points, or unsorted fanouts.
    pub fn new(
        name: String,
        capacitance_per_length: f64,
        resistance_per_length: f64,
        slope: f64,
        fanout_lengths: Vec<(f64, f64)>,
    ) -> Result<Self, crate::LibraryError> {
        let invalid = |detail: &str| crate::LibraryError::InvalidWireLoad {
            name: name.clone(),
            detail: detail.to_string(),
        };
        if [capacitance_per_length, resistance_per_length, slope]
            .into_iter()
            .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err(invalid(
                "capacitance, resistance, and slope must be finite and nonnegative",
            ));
        }
        if fanout_lengths.iter().any(|(fanout, length)| {
            !fanout.is_finite() || *fanout <= 0.0 || !length.is_finite() || *length < 0.0
        }) {
            return Err(invalid(
                "fanout_length values require a positive fanout and nonnegative length",
            ));
        }
        if fanout_lengths.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(invalid("fanout_length fanouts must be strictly increasing"));
        }
        Ok(Self {
            name,
            capacitance_per_length,
            resistance_per_length,
            slope,
            fanout_lengths: fanout_lengths.into(),
        })
    }

    #[must_use]
    /// Estimates total capacitance for `fanout`.
    pub fn capacitance_at(&self, fanout: f64) -> f64 {
        self.capacitance_per_length * self.length_at(fanout)
    }

    #[must_use]
    /// Estimates total resistance for `fanout`.
    pub fn resistance_at(&self, fanout: f64) -> f64 {
        self.resistance_per_length * self.length_at(fanout)
    }

    fn length_at(&self, fanout: f64) -> f64 {
        if !fanout.is_finite() || fanout <= 0.0 {
            return 0.0;
        }
        let Some(&(first_fanout, first_length)) = self.fanout_lengths.first() else {
            return self.slope * fanout;
        };
        if fanout <= first_fanout {
            return first_length * fanout / first_fanout;
        }
        for pair in self.fanout_lengths.windows(2) {
            let [(lower_fanout, lower_length), (upper_fanout, upper_length)] = pair else {
                unreachable!("window length is fixed");
            };
            if fanout <= *upper_fanout {
                let ratio = (fanout - lower_fanout) / (upper_fanout - lower_fanout);
                return lower_length + ratio * (upper_length - lower_length);
            }
        }
        let &(last_fanout, last_length) = self
            .fanout_lengths
            .last()
            .expect("nonempty fanout table has a last point");
        last_length + self.slope * (fanout - last_fanout)
    }
}

impl TimingLibrary {
    /// Returns the exact graph-structure schema for this timing view.
    #[must_use]
    pub fn topology_schema(&self) -> TimingTopologySchema {
        let canonical = self.cells.timing_topology_schema();
        TimingTopologySchema {
            fingerprint: *blake3::hash(&canonical).as_bytes(),
            canonical,
        }
    }

    /// Deterministic heap bytes retained by this materialized selection view.
    ///
    /// This covers selection metadata, overlays, wire-load points, and outer
    /// power-group storage. Canonical target-cell and power-cell arenas remain
    /// owned by the `LibraryStore` artifact boundary and are deliberately
    /// excluded.
    #[must_use]
    pub fn retained_view_memory_bytes(&self) -> usize {
        [
            self.name.as_deref(),
            self.operating_conditions.as_deref(),
            self.wire_load.as_deref(),
            self.wire_load_mode.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|text| opto_core::resident::allocation_bytes(text.len()))
        .sum::<usize>()
        .saturating_add(
            self.wire_load_model
                .as_ref()
                .map_or(0, WireLoadModel::retained_view_memory_bytes),
        )
        .saturating_add(self.cells.retained_view_memory_bytes())
        .saturating_add(self.power.cells.retained_view_memory_bytes())
    }

    /// Fingerprints the resolved timing view consumed by synthesis. Raw
    /// selection spelling and unrelated loaded libraries are deliberately not
    /// represented; provider order and first-match resolution are already
    /// reflected by the materialized view.
    #[must_use]
    pub fn content_fingerprint(&self) -> crate::LibraryFingerprint {
        #[derive(Serialize)]
        struct TimingView<'a> {
            name: &'a Option<String>,
            operating_conditions: &'a Option<String>,
            wire_load: &'a Option<String>,
            wire_load_mode: &'a Option<String>,
            wire_load_model: &'a Option<WireLoadModel>,
            units: TimingLibraryUnits,
            cells: crate::LibraryFingerprint,
        }

        crate::fingerprint_serializable(&TimingView {
            name: &self.name,
            operating_conditions: &self.operating_conditions,
            wire_load: &self.wire_load,
            wire_load_mode: &self.wire_load_mode,
            wire_load_model: &self.wire_load_model,
            units: self.units,
            cells: self.cells.content_fingerprint(),
        })
    }

    /// Fingerprints every library input consumed by timing and power
    /// analysis. Synthesis intentionally continues to use
    /// [`Self::content_fingerprint`], so power-only changes do not invalidate
    /// synthesized netlists.
    #[must_use]
    pub fn analysis_fingerprint(&self) -> crate::LibraryFingerprint {
        #[derive(Serialize)]
        struct AnalysisView {
            domain: &'static str,
            timing: crate::LibraryFingerprint,
            power: crate::LibraryFingerprint,
        }

        crate::fingerprint_serializable(&AnalysisView {
            domain: "opto.library.analysis-view.v1",
            timing: self.content_fingerprint(),
            power: self.power.content_fingerprint(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PowerCell, PowerCellSet};

    #[test]
    fn power_changes_only_the_analysis_fingerprint() {
        let mut changed = TimingLibrary::default();
        changed.power.cells = PowerCellSet::from(vec![PowerCell {
            name: "BUF".to_string(),
            cell_leakage_power: Some(1.0),
            leakage_power: Vec::new(),
            pins: Vec::new(),
        }]);
        let original = TimingLibrary::default();

        assert_eq!(
            original.content_fingerprint(),
            changed.content_fingerprint()
        );
        assert_ne!(
            original.analysis_fingerprint(),
            changed.analysis_fingerprint()
        );
    }
}

impl WireLoadModel {
    fn retained_view_memory_bytes(&self) -> usize {
        opto_core::resident::allocation_bytes(self.name.len()).saturating_add(
            opto_core::resident::slice_bytes::<(f64, f64)>(self.fanout_lengths.len()),
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
/// SI scale factors for numeric values in a timing library.
pub struct TimingLibraryUnits {
    /// Seconds represented by one Liberty time unit.
    pub time_seconds: Option<f64>,
    /// Farads represented by one Liberty capacitance unit.
    pub capacitance_farads: Option<f64>,
    /// Ohms represented by one Liberty resistance unit.
    pub resistance_ohms: Option<f64>,
}

impl TimingLibraryUnits {
    #[must_use]
    /// Converts a numeric Liberty resistance into the internal
    /// time-unit-per-capacitance-unit representation used by STA.
    pub fn normalize_resistance(self, resistance: f64) -> f64 {
        let (Some(time), Some(capacitance)) = (self.time_seconds, self.capacitance_farads) else {
            return resistance;
        };
        let resistance_unit = self.resistance_ohms.unwrap_or(time / capacitance);
        resistance * resistance_unit * capacitance / time
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Class of a sequential timing constraint.
pub enum TimingCheckKind {
    /// Setup-time constraint.
    Setup,
    /// Hold-time constraint.
    Hold,
    /// Asynchronous-control recovery constraint.
    Recovery,
    /// Asynchronous-control removal constraint.
    Removal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Polarity relationship between the input and output of a timing arc.
pub enum TimingSense {
    /// Output transitions in the same direction as the input.
    PositiveUnate,
    /// Output transitions in the opposite direction.
    NegativeUnate,
    /// Either output direction may result.
    NonUnate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
/// Direction of a signal transition.
pub enum TimingEdge {
    /// Rising transition.
    Rise,
    /// Falling transition.
    Fall,
}

impl TimingSense {
    #[must_use]
    /// Returns possible output edges for one input edge.
    pub fn output_edges(self, input_edge: TimingEdge) -> &'static [TimingEdge] {
        match (self, input_edge) {
            (Self::PositiveUnate, edge) => match edge {
                TimingEdge::Rise => &[TimingEdge::Rise],
                TimingEdge::Fall => &[TimingEdge::Fall],
            },
            (Self::NegativeUnate, edge) => match edge {
                TimingEdge::Rise => &[TimingEdge::Fall],
                TimingEdge::Fall => &[TimingEdge::Rise],
            },
            (Self::NonUnate, _) => &[TimingEdge::Rise, TimingEdge::Fall],
        }
    }
}

impl TimingEdge {
    /// Both transition directions in stable index order.
    pub const ALL: [Self; 2] = [Self::Rise, Self::Fall];

    #[must_use]
    /// Returns `0` for rise and `1` for fall.
    pub const fn index(self) -> usize {
        self as usize
    }

    #[must_use]
    /// Returns the compact suffix used in timing reports.
    pub const fn report_suffix(self) -> &'static str {
        match self {
            Self::Rise => "r",
            Self::Fall => "f",
        }
    }
}
