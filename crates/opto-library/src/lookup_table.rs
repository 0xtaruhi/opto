// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Compact one- and two-dimensional Liberty lookup tables.
//!
//! Imported tables intern identical axes. Evaluation uses linear extrapolation
//! outside axis bounds and bilinear interpolation for two-dimensional data.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// Scalar, one-dimensional, or two-dimensional characterization table.
pub struct LookupTable {
    pub(crate) index_1: Arc<[f64]>,
    pub(crate) index_2: Arc<[f64]>,
    pub(crate) values: Arc<[f64]>,
}

impl LookupTable {
    /// Constructs a table from owned axes and row-major values.
    ///
    /// This low-level constructor does not validate axis ordering or dimensions;
    /// the Liberty importer performs that validation before construction.
    #[must_use]
    pub fn new(index_1: Vec<f64>, index_2: Vec<f64>, values: Vec<f64>) -> Self {
        Self {
            index_1: index_1.into(),
            index_2: index_2.into(),
            values: values.into(),
        }
    }

    pub(crate) fn from_shared(
        index_1: Arc<[f64]>,
        index_2: Arc<[f64]>,
        values: Arc<[f64]>,
    ) -> Self {
        Self {
            index_1,
            index_2,
            values,
        }
    }

    #[must_use]
    /// Constructs a table containing one scalar value.
    pub fn scalar(value: f64) -> Self {
        Self {
            index_1: Arc::from([]),
            index_2: Arc::from([]),
            values: Arc::from([value]),
        }
    }

    /// Returns the first table value, when present.
    #[must_use]
    pub fn default_value(&self) -> Option<f64> {
        self.values.first().copied()
    }

    #[must_use]
    /// Returns the largest characterized output-load coordinate.
    ///
    /// Delay and transition tables use the second axis for total output net
    /// capacitance. Scalar and input-transition-only tables have no explicit
    /// output-load domain.
    pub fn maximum_output_load(&self) -> Option<f64> {
        self.index_2.last().copied()
    }

    /// Interpolates a value for transition and load coordinates.
    ///
    /// Missing coordinates select the first point on the corresponding axis.
    #[must_use]
    pub fn value_at(&self, input_transition: Option<f64>, output_load: Option<f64>) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }

        match (&self.index_1[..], &self.index_2[..]) {
            ([], []) => self.default_value(),
            (index_1, []) => interpolate_1d(index_1, &self.values, input_transition),
            ([], index_2) => interpolate_1d(index_2, &self.values, output_load),
            (index_1, index_2) => interpolate_2d(
                index_1,
                index_2,
                &self.values,
                input_transition,
                output_load,
            ),
        }
    }

    pub(crate) fn pointwise_max(&self, other: &Self) -> Option<Self> {
        if !axis_equal(&self.index_1, &other.index_1)
            || !axis_equal(&self.index_2, &other.index_2)
            || self.values.len() != other.values.len()
        {
            return None;
        }
        let values = self
            .values
            .iter()
            .zip(other.values.iter())
            .map(|(&left, &right)| left.max(right))
            .collect::<Vec<_>>();
        Some(Self::from_shared(
            Arc::clone(&self.index_1),
            Arc::clone(&self.index_2),
            values.into(),
        ))
    }
}

#[derive(Debug, Default)]
pub(crate) struct LookupTableBuilder {
    axes_by_hash: HashMap<u64, Vec<Arc<[f64]>>>,
}

impl LookupTableBuilder {
    pub(crate) fn build(
        &mut self,
        index_1: &[f64],
        index_2: &[f64],
        values: &[f64],
    ) -> LookupTable {
        LookupTable {
            index_1: self.intern_axis(index_1),
            index_2: self.intern_axis(index_2),
            values: Arc::from(values),
        }
    }

    pub(crate) fn intern_axis(&mut self, axis: &[f64]) -> Arc<[f64]> {
        let hash = axis_hash(axis);
        let bucket = self.axes_by_hash.entry(hash).or_default();
        if let Some(existing) = bucket.iter().find(|existing| axis_equal(existing, axis)) {
            return Arc::clone(existing);
        }
        let interned = Arc::<[f64]>::from(axis);
        bucket.push(Arc::clone(&interned));
        interned
    }
}

fn axis_hash(axis: &[f64]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    axis.len().hash(&mut hasher);
    for value in axis {
        value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

fn axis_equal(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.to_bits() == right.to_bits())
}

/// Returns the maximum of two optional values, treating absence as no sample.
#[must_use]
pub fn max_optional_f64(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn interpolate_1d(axis: &[f64], values: &[f64], target: Option<f64>) -> Option<f64> {
    if axis.is_empty() || values.is_empty() {
        return values.first().copied();
    }
    let target = target.unwrap_or(axis[0]);
    let (lo, hi, ratio) = interpolation_bracket(axis, target);
    let lo_value = *values.get(lo)?;
    let hi_value = *values.get(hi).unwrap_or(&lo_value);
    Some(lerp(lo_value, hi_value, ratio))
}

fn interpolate_2d(
    index_1: &[f64],
    index_2: &[f64],
    values: &[f64],
    input_transition: Option<f64>,
    output_load: Option<f64>,
) -> Option<f64> {
    let width = index_2.len();
    if width == 0 || index_1.is_empty() || values.len() < index_1.len() * width {
        return values.first().copied();
    }

    let x = input_transition.unwrap_or(index_1[0]);
    let y = output_load.unwrap_or(index_2[0]);
    let (x_lo, x_hi, x_ratio) = interpolation_bracket(index_1, x);
    let (y_lo, y_hi, y_ratio) = interpolation_bracket(index_2, y);
    let v00 = *values.get(x_lo * width + y_lo)?;
    let v01 = *values.get(x_lo * width + y_hi)?;
    let v10 = *values.get(x_hi * width + y_lo)?;
    let v11 = *values.get(x_hi * width + y_hi)?;
    let low = lerp(v00, v01, y_ratio);
    let high = lerp(v10, v11, y_ratio);
    Some(lerp(low, high, x_ratio))
}

pub(crate) fn interpolation_bracket(axis: &[f64], target: f64) -> (usize, usize, f64) {
    if axis.len() <= 1 {
        return (0, 0, 0.0);
    }
    if target <= axis[0] {
        return bracket_ratio(axis, 0, 1, target);
    }
    for index in 1..axis.len() {
        if target <= axis[index] {
            return bracket_ratio(axis, index - 1, index, target);
        }
    }
    let last = axis.len() - 1;
    bracket_ratio(axis, last - 1, last, target)
}

fn bracket_ratio(axis: &[f64], lower: usize, upper: usize, target: f64) -> (usize, usize, f64) {
    let denominator = axis[upper] - axis[lower];
    let ratio = if denominator == 0.0 {
        0.0
    } else {
        (target - axis[lower]) / denominator
    };
    (lower, upper, ratio)
}

pub(crate) fn lerp(left: f64, right: f64, ratio: f64) -> f64 {
    left + (right - left) * ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_two_dimensional_tables() {
        let table = LookupTable::new(vec![0.0, 1.0], vec![0.0, 10.0], vec![1.0, 3.0, 5.0, 7.0]);

        assert_eq!(table.default_value(), Some(1.0));
        assert_eq!(table.maximum_output_load(), Some(10.0));
        assert_eq!(table.value_at(Some(0.5), Some(5.0)), Some(4.0));
        assert_eq!(table.value_at(Some(2.0), Some(20.0)), Some(13.0));
        assert_eq!(table.value_at(Some(-1.0), Some(-10.0)), Some(-5.0));
    }

    #[test]
    fn import_builder_interns_identical_axes() {
        let mut builder = LookupTableBuilder::default();
        let first = builder.build(&[0.0, 1.0], &[2.0, 3.0], &[1.0, 2.0, 3.0, 4.0]);
        let second = builder.build(&[0.0, 1.0], &[2.0, 3.0], &[5.0, 6.0, 7.0, 8.0]);

        assert!(Arc::ptr_eq(&first.index_1, &second.index_1));
        assert!(Arc::ptr_eq(&first.index_2, &second.index_2));
        assert!(!Arc::ptr_eq(&first.values, &second.values));
    }
}
