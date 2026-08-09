// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{BooleanFunction, LookupTable};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
/// Power characterization selected for one analysis session.
pub struct PowerLibrary {
    /// SI scale factors and nominal voltage.
    pub units: PowerLibraryUnits,
    /// Canonical power-cell records.
    pub cells: PowerCellSet,
}

impl PowerLibrary {
    /// Fingerprints the canonical power view consumed by analysis.
    ///
    /// The logical cell sequence is serialized directly so arena grouping is
    /// an allocation detail rather than part of the semantic identity.
    #[must_use]
    pub fn content_fingerprint(&self) -> crate::LibraryFingerprint {
        #[derive(Serialize)]
        struct PowerView<'a> {
            units: PowerLibraryUnits,
            cells: FingerprintPowerCells<'a>,
        }

        crate::fingerprint_serializable(&PowerView {
            units: self.units,
            cells: FingerprintPowerCells(&self.cells),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
/// SI scale factors used by Liberty power characterization.
pub struct PowerLibraryUnits {
    /// Seconds represented by one time unit.
    pub time_seconds: Option<f64>,
    /// Farads represented by one capacitance unit.
    pub capacitance_farads: Option<f64>,
    /// Volts represented by one voltage unit.
    pub voltage_volts: Option<f64>,
    /// Watts represented by one leakage-power unit.
    pub leakage_power_watts: Option<f64>,
    /// Nominal voltage expressed in the library's voltage unit.
    pub nominal_voltage: Option<f64>,
}

impl PowerLibraryUnits {
    #[must_use]
    /// Returns the SI energy coefficient `C × V²`, when all units are known.
    pub fn dynamic_energy_joules(self) -> Option<f64> {
        let voltage = self.voltage_volts? * self.nominal_voltage?;
        Some(self.capacitance_farads? * voltage * voltage)
    }

    #[must_use]
    /// Returns the SI dynamic-power coefficient `C × V² / T`.
    pub fn dynamic_power_watts(self) -> Option<f64> {
        Some(self.dynamic_energy_joules()? / self.time_seconds?)
    }
}

#[derive(Debug, Clone, Default)]
/// Read-only, arena-grouped sequence of canonical [`PowerCell`] records.
pub struct PowerCellSet {
    groups: Arc<[Arc<[PowerCell]>]>,
}

impl PowerCellSet {
    pub(crate) fn retained_view_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<Arc<[PowerCell]>>(self.groups.len())
    }

    pub(crate) fn from_groups(groups: Vec<Arc<[PowerCell]>>) -> Self {
        Self {
            groups: groups.into(),
        }
    }

    /// Iterates over power cells in canonical library order.
    pub fn iter(&self) -> impl Clone + Iterator<Item = &PowerCell> {
        self.groups.iter().flat_map(|group| group.iter())
    }

    #[must_use]
    /// Returns the power cell at a logical sequence index.
    pub fn get(&self, mut index: usize) -> Option<&PowerCell> {
        for group in self.groups.iter() {
            if index < group.len() {
                return group.get(index);
            }
            index = index.checked_sub(group.len())?;
        }
        None
    }
}

impl From<Vec<PowerCell>> for PowerCellSet {
    fn from(cells: Vec<PowerCell>) -> Self {
        if cells.is_empty() {
            Self::default()
        } else {
            Self::from_groups(vec![cells.into()])
        }
    }
}

struct FingerprintPowerCells<'a>(&'a PowerCellSet);

impl Serialize for FingerprintPowerCells<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let cells = self.0.iter();
        let mut sequence = serializer.serialize_seq(Some(cells.clone().count()))?;
        for cell in cells {
            sequence.serialize_element(cell)?;
        }
        sequence.end()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Leakage and internal-power characterization for one library cell.
pub struct PowerCell {
    /// Cell name matching the timing-library cell.
    pub name: String,
    /// Default cell leakage power.
    pub cell_leakage_power: Option<f64>,
    /// State-dependent leakage groups.
    pub leakage_power: Vec<LeakagePower>,
    /// Per-pin internal-power groups.
    pub pins: Vec<PinPower>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Leakage power under an optional Boolean condition.
pub struct LeakagePower {
    /// State condition under which this value applies.
    pub when: Option<BooleanFunction>,
    /// Leakage value in library units.
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Internal-power characterization attached to one pin.
pub struct PinPower {
    /// Pin name within its cell.
    pub name: String,
    /// Conditional internal-power groups.
    pub internal_power: Vec<InternalPower>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Conditional energy tables for a pin transition.
pub struct InternalPower {
    /// Related pin whose transition triggers the energy.
    pub related_pin: Option<String>,
    /// Optional Boolean condition.
    pub when: Option<BooleanFunction>,
    /// Energy table for a rising transition.
    pub rise_power: Option<LookupTable>,
    /// Energy table for a falling transition.
    pub fall_power: Option<LookupTable>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(name: &str, leakage: f64) -> PowerCell {
        PowerCell {
            name: name.to_string(),
            cell_leakage_power: Some(leakage),
            leakage_power: Vec::new(),
            pins: Vec::new(),
        }
    }

    #[test]
    fn fingerprint_ignores_power_cell_arena_grouping() {
        let first = cell("A", 1.0);
        let second = cell("B", 2.0);
        let contiguous = PowerLibrary {
            units: PowerLibraryUnits::default(),
            cells: vec![first.clone(), second.clone()].into(),
        };
        let grouped = PowerLibrary {
            units: PowerLibraryUnits::default(),
            cells: PowerCellSet::from_groups(vec![
                vec![first].into_boxed_slice().into(),
                vec![second].into_boxed_slice().into(),
            ]),
        };

        assert_eq!(
            contiguous.content_fingerprint(),
            grouped.content_fingerprint()
        );
    }
}
