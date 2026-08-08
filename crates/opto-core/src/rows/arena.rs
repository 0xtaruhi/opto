// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{PackedRows, PackedRowsBuilder, PackedRowsError};
use std::ops::Index;
use std::sync::Arc;

const ROWS_PER_PAGE: usize = 4096;

/// Variable-length rows retained as fixed-size immutable CSR pages.
#[derive(Debug)]
pub struct RowArena<T> {
    pages: Vec<RowPage<T>>,
    row_count: u32,
}

#[derive(Debug)]
enum RowPage<T> {
    Packed(Arc<PackedRows<T>>),
    Dirty(DirtyPage<T>),
}

#[derive(Debug)]
struct DirtyPage<T> {
    base: Option<Arc<PackedRows<T>>>,
    rows: u16,
    overrides: Vec<(u16, Vec<T>)>,
}

/// Streaming constructor that seals each 4096-row page as it fills.
#[derive(Debug)]
pub struct RowArenaBuilder<T> {
    pages: Vec<RowPage<T>>,
    current: Option<PackedRowsBuilder<T>>,
    row_count: u32,
}

impl<T> RowArenaBuilder<T> {
    /// Reserve a paged arena without retaining one allocation per source row.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if `row_capacity` exceeds the
    /// 32-bit row count or the page table cannot be reserved.
    pub fn try_with_capacity(row_capacity: usize) -> Result<Self, PackedRowsError> {
        let row_count = u32::try_from(row_capacity).map_err(|_| PackedRowsError::Capacity)?;
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(row_capacity.div_ceil(ROWS_PER_PAGE))
            .map_err(|_| PackedRowsError::Capacity)?;
        Ok(Self {
            pages,
            current: (row_count != 0)
                .then(|| PackedRowsBuilder::try_with_capacity(ROWS_PER_PAGE, 0))
                .transpose()?,
            row_count: 0,
        })
    }

    /// Append one row to the current page.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if the arena already contains
    /// `u32::MAX` rows, a new page cannot be allocated, or the packed value
    /// offsets exceed their 32-bit representation.
    ///
    /// # Panics
    ///
    /// Panics only if page initialization succeeds without installing the
    /// current builder, which would violate this builder's internal state machine.
    pub fn try_push_row<I>(&mut self, row: I) -> Result<(), PackedRowsError>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator,
    {
        if self.row_count == u32::MAX {
            return Err(PackedRowsError::Capacity);
        }
        let row = row.into_iter();
        if self
            .current
            .as_ref()
            .is_some_and(|page| page.row_count() == ROWS_PER_PAGE)
        {
            self.pages
                .try_reserve(1)
                .map_err(|_| PackedRowsError::Capacity)?;
            let next = PackedRowsBuilder::try_with_capacity(ROWS_PER_PAGE, 0)?;
            self.finish_page();
            self.current = Some(next);
        }
        if self.current.is_none() {
            self.pages
                .try_reserve(1)
                .map_err(|_| PackedRowsError::Capacity)?;
            self.current = Some(PackedRowsBuilder::try_with_capacity(ROWS_PER_PAGE, 0)?);
        }
        self.current
            .as_mut()
            .expect("current row page was initialized")
            .try_push_row(row)?;
        self.row_count += 1;
        Ok(())
    }

    /// Freeze all completed pages without copying their row values.
    #[must_use]
    pub fn finish(mut self) -> RowArena<T> {
        self.finish_page();
        RowArena {
            pages: self.pages,
            row_count: self.row_count,
        }
    }

    fn finish_page(&mut self) {
        if let Some(page) = self.current.take()
            && page.row_count() != 0
        {
            self.pages.push(RowPage::Packed(Arc::new(page.finish())));
        }
    }
}

impl<T> RowArena<T> {
    /// Pack independently allocated source rows directly into immutable pages.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if row/value counts exceed the
    /// compact representation or a page allocation fails.
    pub fn try_from_rows(rows: Vec<Vec<T>>) -> Result<Self, PackedRowsError> {
        let mut builder = RowArenaBuilder::try_with_capacity(rows.len())?;
        for row in rows {
            builder.try_push_row(row)?;
        }
        Ok(builder.finish())
    }

    /// Create a fallible arena of empty rows.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if `row_count` exceeds `u32::MAX`
    /// or the page table cannot be reserved.
    pub fn try_empty(row_count: usize) -> Result<Self, PackedRowsError> {
        let mut arena = Self {
            pages: Vec::new(),
            row_count: 0,
        };
        arena.resize_empty(row_count)?;
        Ok(arena)
    }

    /// Transactionally repack only pages touched since the previous commit.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if replacement storage cannot be
    /// reserved or a dirty page's packed offsets exceed 32 bits. On error, no
    /// page replacement is committed.
    pub fn compact(&mut self) -> Result<(), PackedRowsError>
    where
        T: Clone,
    {
        let dirty_count = self
            .pages
            .iter()
            .filter(|page| matches!(page, RowPage::Dirty(_)))
            .count();
        if dirty_count == 0 {
            return Ok(());
        }
        let mut replacements = Vec::new();
        replacements
            .try_reserve_exact(dirty_count)
            .map_err(|_| PackedRowsError::Capacity)?;
        for (index, page) in self.pages.iter().enumerate() {
            let RowPage::Dirty(page) = page else {
                continue;
            };
            replacements.push((index, Arc::new(page.pack()?)));
        }
        for (index, page) in replacements {
            self.pages[index] = RowPage::Packed(page);
        }
        Ok(())
    }

    /// Fork a committed arena by sharing every immutable page.
    #[must_use]
    pub fn fork_shared(&self) -> Option<Self> {
        self.pages
            .iter()
            .map(|page| match page {
                RowPage::Packed(page) => Some(RowPage::Packed(Arc::clone(page))),
                RowPage::Dirty(_) => None,
            })
            .collect::<Option<Vec<_>>>()
            .map(|pages| Self {
                pages,
                row_count: self.row_count,
            })
    }

    /// Enumerate immutable page allocations for exact cross-fork deduplication.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn shared_pages(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.pages.iter().filter_map(|page| {
            let page = match page {
                RowPage::Packed(page) => Some(page),
                RowPage::Dirty(page) => page.base.as_ref(),
            }?;
            Some((Arc::as_ptr(page) as usize, page.owned_memory_bytes()))
        })
    }

    /// Logical bytes of all immutable pages retained by this arena.
    #[must_use]
    pub fn shared_memory_bytes(&self) -> usize {
        self.shared_pages()
            .map(|(_, bytes)| bytes)
            .fold(0usize, usize::saturating_add)
    }

    /// Iterate over rows in stable row order.
    #[must_use = "iterators are lazy and do nothing unless consumed"]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[T]> {
        (0..self.len()).map(|row| &self[row])
    }

    /// Return a row, or `None` when its index is outside the arena.
    #[must_use]
    pub fn get(&self, row: usize) -> Option<&[T]> {
        (row < self.len()).then(|| self.pages[row / ROWS_PER_PAGE].row(row % ROWS_PER_PAGE))
    }

    /// Append a value, materializing only its row in the touched page.
    pub fn push(&mut self, row: usize, value: T)
    where
        T: Clone,
    {
        self.row_mut(row).push(value);
    }

    /// Retain values, materializing only the touched row.
    pub fn retain(&mut self, row: usize, predicate: impl FnMut(&T) -> bool)
    where
        T: Clone,
    {
        self.row_mut(row).retain(predicate);
    }

    /// Replace one row without copying its former values.
    pub fn replace(&mut self, row: usize, values: Vec<T>) {
        let (page, local) = self.dirty_row(row);
        match page.overrides.binary_search_by_key(&local, |&(row, _)| row) {
            Ok(index) => page.overrides[index].1 = values,
            Err(index) => page.overrides.insert(index, (local, values)),
        }
    }

    /// Borrow one row mutably, cloning no other row in its page.
    pub fn row_mut(&mut self, row: usize) -> &mut Vec<T>
    where
        T: Clone,
    {
        let (page, local) = self.dirty_row(row);
        let index = match page.overrides.binary_search_by_key(&local, |&(row, _)| row) {
            Ok(index) => index,
            Err(index) => {
                let values = page
                    .base
                    .as_ref()
                    .and_then(|base| base.get(local as usize))
                    .map_or_else(Vec::new, <[T]>::to_vec);
                page.overrides.insert(index, (local, values));
                index
            }
        };
        &mut page.overrides[index].1
    }

    /// Append one empty row, returning capacity failures explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if the 32-bit row count is
    /// exhausted or the page table cannot grow.
    pub fn push_empty(&mut self) -> Result<(), PackedRowsError> {
        self.resize_empty(self.len().checked_add(1).ok_or(PackedRowsError::Capacity)?)
    }

    /// Reserve the dense page table for a future row count without changing
    /// the logical arena, allowing multi-arena edits to preflight allocation.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if `row_count` exceeds `u32::MAX`
    /// or the page table reservation fails.
    pub fn try_reserve_rows(&mut self, row_count: usize) -> Result<(), PackedRowsError> {
        u32::try_from(row_count).map_err(|_| PackedRowsError::Capacity)?;
        let target_pages = row_count.div_ceil(ROWS_PER_PAGE);
        self.pages
            .try_reserve(target_pages.saturating_sub(self.pages.len()))
            .map_err(|_| PackedRowsError::Capacity)
    }

    /// Grow with implicit empty rows.
    ///
    /// # Errors
    ///
    /// Returns [`PackedRowsError::Capacity`] if the requested row count or page
    /// table cannot be represented. Existing rows remain unchanged on error.
    ///
    /// # Panics
    ///
    /// Panics if `row_count` is less than the current length; shrinking must use
    /// [`Self::truncate_empty`] so removed rows are checked for emptiness.
    pub fn resize_empty(&mut self, row_count: usize) -> Result<(), PackedRowsError> {
        assert!(row_count >= self.len(), "resize_empty cannot remove rows");
        if row_count == self.len() {
            return Ok(());
        }
        self.try_reserve_rows(row_count)?;
        let row_count = u32::try_from(row_count).map_err(|_| PackedRowsError::Capacity)?;
        let target_pages = (row_count as usize).div_ceil(ROWS_PER_PAGE);
        let existing_pages = self.pages.len();
        if let Some(last) = self.pages.last_mut()
            && !(self.row_count as usize).is_multiple_of(ROWS_PER_PAGE)
        {
            let rows =
                (row_count as usize - (existing_pages - 1) * ROWS_PER_PAGE).min(ROWS_PER_PAGE);
            let rows = u16::try_from(rows).expect("one row page contains at most 4096 rows");
            last.make_dirty(rows).rows = rows;
        }
        while self.pages.len() < target_pages {
            let first = self.pages.len() * ROWS_PER_PAGE;
            let rows = (row_count as usize - first).min(ROWS_PER_PAGE);
            self.pages.push(RowPage::Dirty(DirtyPage {
                base: None,
                rows: u16::try_from(rows).expect("one row page contains at most 4096 rows"),
                overrides: Vec::new(),
            }));
        }
        self.row_count = row_count;
        Ok(())
    }

    /// Remove the final row, which must be empty.
    ///
    /// # Panics
    ///
    /// Panics if the arena is empty or its final row contains a value.
    pub fn pop_empty(&mut self) {
        let row_count = self.len().checked_sub(1).expect("row arena is not empty");
        self.truncate_empty(row_count);
    }

    /// Truncate trailing rows after verifying that every removed row is empty.
    ///
    /// # Panics
    ///
    /// Panics if `row_count` exceeds the current length or any removed row is
    /// nonempty.
    pub fn truncate_empty(&mut self, row_count: usize) {
        assert!(
            row_count <= self.len() && (row_count..self.len()).all(|row| self[row].is_empty()),
            "only empty trailing rows can be truncated"
        );
        if row_count == self.len() {
            return;
        }
        let target_pages = row_count.div_ceil(ROWS_PER_PAGE);
        self.pages.truncate(target_pages);
        let tail_rows = row_count % ROWS_PER_PAGE;
        if tail_rows != 0
            && let Some(last) = self.pages.last_mut()
        {
            let rows = tail_rows;
            let rows = u16::try_from(rows).expect("one row page contains at most 4096 rows");
            let dirty = last.make_dirty(rows);
            dirty.rows = rows;
        }
        self.row_count = u32::try_from(row_count).expect("truncation cannot exceed stored length");
    }

    /// Number of rows in the arena.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.row_count as usize
    }

    /// Whether the arena contains no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// Deterministic logical resident size, including page-table edit state.
    #[must_use]
    pub fn owned_memory_bytes(&self) -> usize {
        self.pages.iter().fold(
            crate::resident::slice_bytes::<RowPage<T>>(self.pages.capacity()),
            |bytes, page| bytes.saturating_add(page.owned_memory_bytes()),
        )
    }

    fn dirty_row(&mut self, row: usize) -> (&mut DirtyPage<T>, u16) {
        assert!(row < self.len(), "row arena index is in bounds");
        let local = u16::try_from(row % ROWS_PER_PAGE).expect("page-local row fits in u16");
        let rows = self.pages[row / ROWS_PER_PAGE].visible_rows();
        (self.pages[row / ROWS_PER_PAGE].make_dirty(rows), local)
    }
}

impl<T> RowPage<T> {
    fn visible_rows(&self) -> u16 {
        match self {
            Self::Packed(page) => {
                u16::try_from(page.row_count()).expect("one row page contains at most 4096 rows")
            }
            Self::Dirty(page) => page.rows,
        }
    }

    fn make_dirty(&mut self, rows: u16) -> &mut DirtyPage<T> {
        if let Self::Packed(page) = self {
            *self = Self::Dirty(DirtyPage {
                base: Some(Arc::clone(page)),
                rows,
                overrides: Vec::new(),
            });
        }
        let Self::Dirty(page) = self else {
            unreachable!("packed page was converted to dirty storage")
        };
        page
    }

    fn row(&self, row: usize) -> &[T] {
        match self {
            Self::Packed(page) => page.row(row),
            Self::Dirty(page) => page.row(row),
        }
    }

    fn owned_memory_bytes(&self) -> usize {
        match self {
            Self::Packed(page) => page.owned_memory_bytes(),
            Self::Dirty(page) => page.owned_memory_bytes(),
        }
    }
}

impl<T> DirtyPage<T> {
    fn row(&self, row: usize) -> &[T] {
        assert!(row < self.rows as usize, "row page index is in bounds");
        self.overrides
            .binary_search_by_key(
                &u16::try_from(row).expect("validated page-local row fits in u16"),
                |&(row, _)| row,
            )
            .ok()
            .map(|index| self.overrides[index].1.as_slice())
            .or_else(|| self.base.as_ref().and_then(|base| base.get(row)))
            .unwrap_or_default()
    }

    fn value_count(&self) -> Result<usize, PackedRowsError> {
        let base_rows = self
            .base
            .as_ref()
            .map_or(0, |base| base.row_count().min(self.rows as usize));
        let base_count = base_rows
            .checked_sub(1)
            .and_then(|row| self.base.as_ref()?.row_range(row))
            .map_or(0, |range| range.end);
        let (removed, added) = self
            .overrides
            .iter()
            .filter(|(row, _)| *row < self.rows)
            .try_fold((0usize, 0usize), |(removed, added), (row, values)| {
                let old = self
                    .base
                    .as_ref()
                    .filter(|_| (*row as usize) < base_rows)
                    .and_then(|base| base.get(*row as usize))
                    .map_or(0, <[T]>::len);
                Ok::<_, PackedRowsError>((
                    removed.checked_add(old).ok_or(PackedRowsError::Capacity)?,
                    added
                        .checked_add(values.len())
                        .ok_or(PackedRowsError::Capacity)?,
                ))
            })?;
        base_count
            .checked_sub(removed)
            .and_then(|count| count.checked_add(added))
            .filter(|&count| u32::try_from(count).is_ok())
            .ok_or(PackedRowsError::Capacity)
    }

    fn pack(&self) -> Result<PackedRows<T>, PackedRowsError>
    where
        T: Clone,
    {
        let mut builder =
            PackedRowsBuilder::try_with_capacity(self.rows as usize, self.value_count()?)?;
        for row in 0..self.rows as usize {
            builder.try_push_row(self.row(row).iter().cloned())?;
        }
        Ok(builder.finish())
    }

    fn owned_memory_bytes(&self) -> usize {
        self.base
            .as_ref()
            .map_or(0, |base| base.owned_memory_bytes())
            .saturating_add(crate::resident::slice_bytes::<(u16, Vec<T>)>(
                self.overrides.capacity(),
            ))
            .saturating_add(self.overrides.iter().fold(0usize, |bytes, (_, row)| {
                bytes.saturating_add(crate::resident::slice_bytes::<T>(row.capacity()))
            }))
    }
}

impl<T> Index<usize> for RowArena<T> {
    type Output = [T];

    fn index(&self, row: usize) -> &Self::Output {
        self.get(row).expect("row arena index is in bounds")
    }
}
