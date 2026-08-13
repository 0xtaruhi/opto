// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Arc, Deserialize, LibraryError, Serialize, WaveformRef, interpolation_span, invalid_model,
    sample_waveform, strictly_monotonic, validate_axis,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Rectangular grid of variable-length sampled waveforms.
///
/// Axis coordinates and all waveform samples are validated and stored in
/// contiguous shared arrays.
pub struct SampledWaveformGrid {
    index_1: Arc<[f64]>,
    index_2: Arc<[f64]>,
    offsets: Arc<[u32]>,
    reference_times: Arc<[f64]>,
    coordinates: Arc<[f64]>,
    values: Arc<[f64]>,
}

#[derive(Debug, Clone, PartialEq)]
/// Owned sampled current or voltage waveform.
pub struct SampledWaveform {
    /// Library reference time associated with the waveform.
    pub reference_time: f64,
    /// Strictly monotonic sample coordinates.
    pub coordinates: Vec<f64>,
    /// Sample values corresponding one-to-one with `coordinates`.
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
/// Driver waveform normalized to monotonically increasing voltage.
pub struct NormalizedDriverWaveform {
    /// Monotonically increasing sample times.
    pub times: Vec<f64>,
    /// Normalized voltage samples clamped to `[0, 1]`.
    pub normalized_voltage: Vec<f64>,
}

impl SampledWaveformGrid {
    /// Validates and packs a rectangular waveform grid.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::InvalidTimingModel`] when axes, grid dimensions,
    /// sample counts, coordinates, or numeric values are invalid.
    pub fn new(
        model: &'static str,
        index_1: Vec<f64>,
        index_2: Vec<f64>,
        waveforms: Vec<SampledWaveform>,
    ) -> Result<Self, LibraryError> {
        Self::from_shared(model, index_1.into(), index_2.into(), waveforms)
    }

    /// Validates and packs a grid while retaining shared characterization axes.
    pub(crate) fn from_shared(
        model: &'static str,
        index_1: Arc<[f64]>,
        index_2: Arc<[f64]>,
        waveforms: Vec<SampledWaveform>,
    ) -> Result<Self, LibraryError> {
        validate_axis(model, "index_1", &index_1)?;
        validate_axis(model, "index_2", &index_2)?;
        let expected = index_1
            .len()
            .max(1)
            .checked_mul(index_2.len().max(1))
            .ok_or_else(|| {
                invalid_model(model, "waveform grid dimensions exceed the host capacity")
            })?;
        if waveforms.len() != expected {
            return Err(invalid_model(
                model,
                format!(
                    "waveform grid requires {expected} vectors but contains {}",
                    waveforms.len()
                ),
            ));
        }
        let mut offsets = Vec::with_capacity(waveforms.len() + 1);
        let mut reference_times = Vec::with_capacity(waveforms.len());
        let mut coordinates = Vec::new();
        let mut values = Vec::new();
        offsets.push(0);
        for waveform in waveforms {
            if !waveform.reference_time.is_finite() {
                return Err(invalid_model(
                    model,
                    "waveform reference time is not finite",
                ));
            }
            if waveform.coordinates.len() < 2 || waveform.coordinates.len() != waveform.values.len()
            {
                return Err(invalid_model(
                    model,
                    "every waveform must contain at least two coordinate/value samples",
                ));
            }
            if waveform
                .coordinates
                .iter()
                .chain(&waveform.values)
                .any(|value| !value.is_finite())
            {
                return Err(invalid_model(model, "waveform samples must be finite"));
            }
            if !strictly_monotonic(&waveform.coordinates) {
                return Err(invalid_model(
                    model,
                    "waveform coordinates must be strictly monotonic",
                ));
            }
            reference_times.push(waveform.reference_time);
            coordinates.extend(waveform.coordinates);
            values.extend(waveform.values);
            offsets.push(u32::try_from(coordinates.len()).map_err(|_| {
                invalid_model(
                    model,
                    "waveform sample storage exceeds the 32-bit arena capacity",
                )
            })?);
        }
        Ok(Self {
            index_1,
            index_2,
            offsets: offsets.into(),
            reference_times: reference_times.into(),
            coordinates: coordinates.into(),
            values: values.into(),
        })
    }

    #[must_use]
    /// Returns the first interpolation axis.
    pub fn index_1(&self) -> &[f64] {
        &self.index_1
    }

    #[must_use]
    /// Returns the second interpolation axis.
    pub fn index_2(&self) -> &[f64] {
        &self.index_2
    }

    #[must_use]
    /// Returns the number of sampled waveforms in the grid.
    pub fn waveform_count(&self) -> usize {
        self.reference_times.len()
    }

    pub(super) fn waveform(&self, index: usize) -> Option<WaveformRef<'_>> {
        let start = usize::try_from(*self.offsets.get(index)?).ok()?;
        let end = usize::try_from(*self.offsets.get(index + 1)?).ok()?;
        Some(WaveformRef {
            coordinates: self.coordinates.get(start..end)?,
            values: self.values.get(start..end)?,
        })
    }

    /// Bilinearly combines corner waveforms on their sorted coordinate union.
    ///
    /// Corner waveforms may use different sample coordinates. Each is sampled
    /// onto the union before weights are applied, avoiding index-wise blending
    /// of physically different time or voltage points.
    pub(super) fn interpolated_waveform(
        &self,
        index_1: Option<f64>,
        index_2: Option<f64>,
    ) -> Option<SampledWaveform> {
        let first = interpolation_span(&self.index_1, index_1);
        let second = interpolation_span(&self.index_2, index_2);
        let columns = self.index_2.len().max(1);
        let corners = [
            (first.0, second.0, (1.0 - first.2) * (1.0 - second.2)),
            (first.0, second.1, (1.0 - first.2) * second.2),
            (first.1, second.0, first.2 * (1.0 - second.2)),
            (first.1, second.1, first.2 * second.2),
        ];
        let mut selected = Vec::new();
        for (row, column, weight) in corners {
            if weight == 0.0 && !selected.is_empty() {
                continue;
            }
            let index = row.checked_mul(columns)?.checked_add(column)?;
            let waveform = self.waveform(index)?;
            if let Some((_, existing_weight)) = selected
                .iter_mut()
                .find(|(existing, _): &&mut (usize, f64)| *existing == index)
            {
                *existing_weight += weight;
            } else {
                selected.push((index, weight));
            }
            let _ = waveform;
        }
        let mut coordinates = selected
            .iter()
            .flat_map(|(index, _)| {
                self.waveform(*index)
                    .into_iter()
                    .flat_map(|waveform| waveform.coordinates.iter().copied())
            })
            .collect::<Vec<_>>();
        coordinates.sort_unstable_by(f64::total_cmp);
        coordinates.dedup_by(|left, right| left.total_cmp(right).is_eq());
        let values = coordinates
            .iter()
            .map(|coordinate| {
                selected
                    .iter()
                    .filter_map(|(index, weight)| {
                        self.waveform(*index)
                            .map(|waveform| weight * sample_waveform(waveform, *coordinate))
                    })
                    .sum()
            })
            .collect();
        let reference_time = selected
            .iter()
            .filter_map(|(index, weight)| {
                self.reference_times
                    .get(*index)
                    .map(|reference| weight * reference)
            })
            .sum();
        Some(SampledWaveform {
            reference_time,
            coordinates,
            values,
        })
    }
}
