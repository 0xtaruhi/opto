// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::Index;
use std::{error::Error, fmt};

mod arena;

pub use arena::{RowArena, RowArenaBuilder};

/// Failure while constructing compact row storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackedRowsError {
    /// An entry referred to a row outside the target arena.
    RowOutOfBounds {
        /// Rejected row index.
        row: usize,
        /// Number of rows in the target arena.
        row_count: usize,
    },
    /// The row count or one packed allocation's value count/offset exceeds
    /// 32-bit capacity.
    Capacity,
}

impl fmt::Display for PackedRowsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RowOutOfBounds { row, row_count } => write!(
                formatter,
                "row {row} is outside an arena containing {row_count} rows"
            ),
            Self::Capacity => formatter.write_str("packed rows exceed 32-bit capacity"),
        }
    }
}

impl Error for PackedRowsError {}

/// Immutable compressed-row storage backed by one offsets allocation and one
/// values allocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackedRows<T> {
    offsets: Box<[u32]>,
    values: Box<[T]>,
}

/// Incremental constructor for [`PackedRows`] that retains one offsets vector
/// and one values vector while earlier rows remain readable.
///
/// This is suitable for topological algorithms whose next row depends on
/// already-built rows. It avoids both an allocation per row and private
/// copies of the packed-row representation in downstream crates.
#[derive(Debug)]
pub struct PackedRowsBuilder<T> {
    offsets: Vec<u32>,
    values: Vec<T>,
}

impl<T> PackedRowsBuilder<T> {
    /// Create an empty builder and reserve the expected compact storage.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if either capacity exceeds the
    /// 32-bit CSR representation or either reservation fails.
    pub fn try_with_capacity(
        row_capacity: usize,
        value_capacity: usize,
    ) -> Result<Self, PackedRowsError> {
        if u32::try_from(row_capacity).is_err() || u32::try_from(value_capacity).is_err() {
            return Err(PackedRowsError::Capacity);
        }
        let offset_capacity = row_capacity
            .checked_add(1)
            .ok_or(PackedRowsError::Capacity)?;
        let mut offsets = Vec::new();
        offsets
            .try_reserve_exact(offset_capacity)
            .map_err(|_| PackedRowsError::Capacity)?;
        offsets.push(0);
        let mut values = Vec::new();
        values
            .try_reserve_exact(value_capacity)
            .map_err(|_| PackedRowsError::Capacity)?;
        Ok(Self { offsets, values })
    }

    /// Append one row without allocating an intermediate row object.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if the new row/value counts exceed
    /// 32 bits or either backing vector cannot reserve the required storage.
    pub fn try_push_row<I>(&mut self, row: I) -> Result<(), PackedRowsError>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator,
    {
        if u32::try_from(self.offsets.len()).is_err() {
            return Err(PackedRowsError::Capacity);
        }
        let row = row.into_iter();
        let row_len = row.len();
        let new_value_count = self
            .values
            .len()
            .checked_add(row_len)
            .filter(|&count| u32::try_from(count).is_ok())
            .ok_or(PackedRowsError::Capacity)?;
        self.values
            .try_reserve(row_len)
            .map_err(|_| PackedRowsError::Capacity)?;
        self.offsets
            .try_reserve(1)
            .map_err(|_| PackedRowsError::Capacity)?;
        self.values.extend(row);
        self.offsets.push(
            new_value_count
                .try_into()
                .map_err(|_| PackedRowsError::Capacity)?,
        );
        Ok(())
    }

    /// Return an already-built row.
    #[must_use]
    pub fn get(&self, row: usize) -> Option<&[T]> {
        let start = *self.offsets.get(row)? as usize;
        let end = *self.offsets.get(row + 1)? as usize;
        self.values.get(start..end)
    }

    /// Number of rows appended so far.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.offsets.len() - 1
    }

    /// Freeze the builder into immutable compact storage without copying.
    #[must_use]
    pub fn finish(self) -> PackedRows<T> {
        PackedRows {
            offsets: self.offsets.into_boxed_slice(),
            values: self.values.into_boxed_slice(),
        }
    }
}

impl<T> PackedRows<T> {
    /// Pack independently allocated rows into contiguous storage.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if row/value counts exceed 32 bits.
    pub fn try_from_rows(rows: Vec<Vec<T>>) -> Result<Self, PackedRowsError> {
        if u32::try_from(rows.len()).is_err() {
            return Err(PackedRowsError::Capacity);
        }
        let offset_count = rows.len().checked_add(1).ok_or(PackedRowsError::Capacity)?;
        let value_count = rows.iter().try_fold(0usize, |count, row| {
            count
                .checked_add(row.len())
                .filter(|&count| u32::try_from(count).is_ok())
                .ok_or(PackedRowsError::Capacity)
        })?;
        let mut offsets = Vec::with_capacity(offset_count);
        let mut values = Vec::with_capacity(value_count);
        offsets.push(0);
        for row in rows {
            values.extend(row);
            offsets.push(
                values
                    .len()
                    .try_into()
                    .map_err(|_| PackedRowsError::Capacity)?,
            );
        }
        Ok(Self {
            offsets: offsets.into_boxed_slice(),
            values: values.into_boxed_slice(),
        })
    }

    /// Pack rows supplied as iterators without first allocating one `Vec` per
    /// row.
    ///
    /// This is intended for algorithms that build temporary segmented storage
    /// in parallel and compact it once before retaining the result.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] once the iterator produces more
    /// rows or values than the 32-bit CSR offsets can represent.
    pub fn try_from_row_iter<R, I>(rows: R) -> Result<Self, PackedRowsError>
    where
        R: IntoIterator<Item = I>,
        I: IntoIterator<Item = T>,
    {
        let rows = rows.into_iter();
        let (minimum_rows, _) = rows.size_hint();
        if u32::try_from(minimum_rows).is_err() {
            return Err(PackedRowsError::Capacity);
        }
        let mut offsets = Vec::with_capacity(minimum_rows.saturating_add(1));
        let mut values = Vec::new();
        offsets.push(0);
        for row in rows {
            if u32::try_from(offsets.len()).is_err() {
                return Err(PackedRowsError::Capacity);
            }
            for value in row {
                if values
                    .len()
                    .checked_add(1)
                    .is_none_or(|count| u32::try_from(count).is_err())
                {
                    return Err(PackedRowsError::Capacity);
                }
                values.push(value);
            }
            offsets.push(
                values
                    .len()
                    .try_into()
                    .map_err(|_| PackedRowsError::Capacity)?,
            );
        }
        Ok(Self {
            offsets: offsets.into_boxed_slice(),
            values: values.into_boxed_slice(),
        })
    }

    /// Pack `(row, value)` entries in linear time while preserving their order
    /// within each row.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::RowOutOfBounds`] for an entry whose row is not
    /// below `row_count`, or [`PackedRowsError::Capacity`] when row/value counts
    /// exceed the 32-bit CSR representation.
    pub fn try_from_entries(
        row_count: usize,
        entries: impl IntoIterator<Item = (usize, T)>,
    ) -> Result<Self, PackedRowsError> {
        if u32::try_from(row_count).is_err() {
            return Err(PackedRowsError::Capacity);
        }
        let mut entries = entries.into_iter().collect::<Vec<_>>();
        if let Some(&(row, _)) = entries.iter().find(|(row, _)| *row >= row_count) {
            return Err(PackedRowsError::RowOutOfBounds { row, row_count });
        }
        if u32::try_from(entries.len()).is_err() {
            return Err(PackedRowsError::Capacity);
        }
        let offset_count = row_count.checked_add(1).ok_or(PackedRowsError::Capacity)?;
        let mut offsets = vec![0u32; offset_count];
        for &(row, _) in &entries {
            offsets[row + 1] = offsets[row + 1]
                .checked_add(1)
                .ok_or(PackedRowsError::Capacity)?;
        }
        for row in 1..offsets.len() {
            offsets[row] = offsets[row]
                .checked_add(offsets[row - 1])
                .ok_or(PackedRowsError::Capacity)?;
        }
        let mut cursors = offsets[..row_count].to_vec();
        let mut destinations = Vec::with_capacity(entries.len());
        for &(row, _) in &entries {
            let destination = cursors[row];
            destinations.push(destination);
            cursors[row] += 1;
        }
        for index in 0..entries.len() {
            while destinations[index] as usize != index {
                let destination = destinations[index] as usize;
                entries.swap(index, destination);
                destinations.swap(index, destination);
            }
        }
        Ok(Self {
            offsets: offsets.into_boxed_slice(),
            values: entries
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        })
    }

    /// Return a row, or `None` when its index is outside the arena.
    #[must_use]
    pub fn get(&self, row: usize) -> Option<&[T]> {
        let start = *self.offsets.get(row)? as usize;
        let end = *self.offsets.get(row + 1)? as usize;
        self.values.get(start..end)
    }

    /// Return a row and panic when its index is outside the arena.
    ///
    /// # Panics
    ///
    /// Panics if `row` is not below [`Self::row_count`].
    #[must_use]
    pub fn row(&self, row: usize) -> &[T] {
        self.get(row).expect("packed row index is in bounds")
    }

    /// Return the row-major value range backing `row`.
    ///
    /// Parallel per-value arrays index the same flat space, so this is the one
    /// place that owns the mapping from a row and an in-row offset to a flat
    /// position. Callers must not rebuild an offsets table of their own.
    #[must_use]
    pub fn row_range(&self, row: usize) -> Option<std::ops::Range<usize>> {
        let start = *self.offsets.get(row)? as usize;
        let end = *self.offsets.get(row + 1)? as usize;
        Some(start..end)
    }

    /// Iterate over rows without allocating row objects.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[T]> {
        (0..self.row_count()).map(|row| self.row(row))
    }

    /// Number of rows in the arena.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Number of values across all rows.
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    /// All values in row-major order.
    #[must_use]
    pub fn values(&self) -> &[T] {
        &self.values
    }

    /// Deterministic logical resident size of both packed allocations.
    #[must_use]
    pub fn owned_memory_bytes(&self) -> usize {
        crate::resident::slice_bytes::<u32>(self.offsets.len())
            .saturating_add(crate::resident::slice_bytes::<T>(self.values.len()))
    }
}

impl<T> Index<usize> for PackedRows<T> {
    type Output = [T];

    fn index(&self, row: usize) -> &Self::Output {
        self.row(row)
    }
}

impl<T: Serialize> Serialize for PackedRows<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.offsets.as_ref(), self.values.as_ref()).serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for PackedRows<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (offsets, values) = <(Box<[u32]>, Box<[T]>)>::deserialize(deserializer)?;
        if offsets.is_empty() || offsets[0] != 0 {
            return Err(serde::de::Error::custom(
                "packed rows require a zero leading offset",
            ));
        }
        if *offsets.last().unwrap_or(&0) as usize != values.len() {
            return Err(serde::de::Error::custom(
                "packed rows trailing offset must equal the value count",
            ));
        }
        if offsets.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(serde::de::Error::custom(
                "packed row offsets must be non-decreasing",
            ));
        }
        Ok(Self { offsets, values })
    }
}

#[cfg(test)]
mod tests {
    use super::{PackedRows, PackedRowsBuilder};

    #[test]
    fn scattered_entries_preserve_row_order() {
        let rows =
            PackedRows::try_from_entries(4, [(2, 'a'), (0, 'b'), (2, 'c'), (1, 'd'), (0, 'e')])
                .expect("entries fit");

        assert_eq!(rows[0], ['b', 'e']);
        assert_eq!(rows[1], ['d']);
        assert_eq!(rows[2], ['a', 'c']);
        assert!(rows[3].is_empty());
    }

    #[test]
    fn row_ranges_partition_the_flat_value_space() {
        let rows =
            PackedRows::try_from_rows(vec![vec!['a', 'b'], vec![], vec!['c']]).expect("rows fit");

        assert_eq!(rows.row_range(0), Some(0..2));
        assert_eq!(rows.row_range(1), Some(2..2));
        assert_eq!(rows.row_range(2), Some(2..3));
        assert_eq!(rows.row_range(3), None);
        assert_eq!(
            rows.row_range(2).map(|range| &rows.values()[range]),
            Some(&['c'][..])
        );
    }

    #[test]
    fn row_iter_packs_without_intermediate_row_vectors() {
        let source = [vec!['a', 'b'], vec![], vec!['c']];
        let rows =
            PackedRows::try_from_row_iter(source.iter().map(|row| row.as_slice().iter().copied()))
                .expect("rows fit");

        assert_eq!(rows[0], ['a', 'b']);
        assert!(rows[1].is_empty());
        assert_eq!(rows[2], ['c']);
    }

    #[test]
    fn incremental_builder_exposes_prior_rows_without_row_allocations() {
        let mut rows = PackedRowsBuilder::try_with_capacity(3, 4).expect("rows fit");
        rows.try_push_row(['a', 'b']).expect("first row fits");
        assert_eq!(rows.get(0), Some(&['a', 'b'][..]));
        rows.try_push_row(std::iter::empty())
            .expect("empty row fits");
        rows.try_push_row(['c', 'd']).expect("last row fits");

        let rows = rows.finish();
        assert_eq!(rows[0], ['a', 'b']);
        assert!(rows[1].is_empty());
        assert_eq!(rows[2], ['c', 'd']);
    }
}
