// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::CapacityError;
use std::sync::Arc;

const VALUES_PER_PAGE: usize = 4096;

/// A stable-index dense vector whose values are shared one immutable page at a
/// time.
///
/// Forks clone only the dense page table. The first write to a page shared by
/// another fork clones at most 4096 values, while subsequent writes mutate the
/// now-exclusive page directly. There is no overlay to compact or commit.
#[derive(Debug, Clone)]
pub struct PagedCowVec<T> {
    pages: Vec<Arc<Vec<T>>>,
    len: u32,
    default: T,
}

impl<T> PagedCowVec<T> {
    /// Create an empty vector whose future slots use `default`.
    #[must_use]
    pub const fn new(default: T) -> Self {
        Self {
            pages: Vec::new(),
            len: 0,
            default,
        }
    }

    /// Build immutable pages from a complete dense value sequence.
    ///
    /// This is the seal-time counterpart of [`Self::try_set`]: callers that
    /// already own every value avoid growing and reopening the same tail page
    /// one slot at a time.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityError`] when the compact length or a page allocation
    /// cannot be represented.
    pub fn try_from_values(values: Vec<T>, default: T) -> Result<Self, CapacityError> {
        let len = u32::try_from(values.len()).map_err(|_| CapacityError)?;
        let page_count = values.len().div_ceil(VALUES_PER_PAGE);
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(page_count)
            .map_err(|_| CapacityError)?;
        let mut values = values.into_iter();
        for _ in 0..page_count {
            let mut page = Vec::new();
            page.try_reserve_exact(VALUES_PER_PAGE.min(values.len()))
                .map_err(|_| CapacityError)?;
            page.extend(values.by_ref().take(VALUES_PER_PAGE));
            pages.push(Arc::new(page));
        }
        Ok(Self {
            pages,
            len,
            default,
        })
    }

    /// Fork by sharing every immutable value page.
    #[must_use]
    pub fn fork_shared(&self) -> Self
    where
        T: Clone,
    {
        self.clone()
    }

    /// Return one value by stable zero-based position.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&T> {
        (index < self.len())
            .then(|| self.pages.get(index / VALUES_PER_PAGE))
            .flatten()?
            .get(index % VALUES_PER_PAGE)
    }

    /// Replace one value, growing intervening slots with the default.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityError`] if `index + 1` exceeds the compact 32-bit
    /// length or if reserving/cloning the affected pages fails.
    pub fn try_set(&mut self, index: usize, value: T) -> Result<Option<T>, CapacityError>
    where
        T: Clone,
    {
        let new_len = index.checked_add(1).ok_or(CapacityError)?;
        let old = self.get(index).cloned();
        if new_len > self.len() {
            self.try_resize(new_len)?;
        }
        self.try_unique_page(index / VALUES_PER_PAGE)?[index % VALUES_PER_PAGE] = value;
        Ok(old)
    }

    /// Resize the vector, filling newly visible values with the default.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityError`] if `len` exceeds the compact 32-bit length or
    /// if allocating page-table or value storage fails. The vector is unchanged
    /// until every required allocation succeeds.
    ///
    /// # Panics
    ///
    /// Panics only if an exclusively owned tail page becomes shared between
    /// its ownership check and resize; no user code runs in that interval.
    pub fn try_resize(&mut self, len: usize) -> Result<(), CapacityError>
    where
        T: Clone,
    {
        let len = u32::try_from(len).map_err(|_| CapacityError)?;
        if len <= self.len {
            self.truncate(len as usize);
            return Ok(());
        }

        let old_len = self.len as usize;
        let new_len = len as usize;
        let target_pages = new_len.div_ceil(VALUES_PER_PAGE);
        let added_pages = target_pages.saturating_sub(self.pages.len());
        self.pages
            .try_reserve(added_pages)
            .map_err(|_| CapacityError)?;

        let existing_tail =
            (!old_len.is_multiple_of(VALUES_PER_PAGE)).then_some(old_len / VALUES_PER_PAGE);
        let mut tail_replacement = None;
        if let Some(page_index) = existing_tail {
            let required = visible_page_len(new_len, page_index);
            if let Some(values) = Arc::get_mut(&mut self.pages[page_index]) {
                values
                    .try_reserve(required.saturating_sub(values.len()))
                    .map_err(|_| CapacityError)?;
            } else {
                let mut replacement = try_clone_page(&self.pages[page_index], required)?;
                replacement.resize(required.max(replacement.len()), self.default.clone());
                replacement[old_len % VALUES_PER_PAGE..required].fill(self.default.clone());
                tail_replacement = Some((page_index, Arc::new(replacement)));
            }
        }

        let mut new_pages = Vec::new();
        new_pages
            .try_reserve_exact(added_pages)
            .map_err(|_| CapacityError)?;
        for page_index in self.pages.len()..target_pages {
            let required = visible_page_len(new_len, page_index);
            let mut values = Vec::new();
            values
                .try_reserve_exact(required)
                .map_err(|_| CapacityError)?;
            values.resize(required, self.default.clone());
            new_pages.push(Arc::new(values));
        }

        if let Some((page_index, replacement)) = tail_replacement {
            self.pages[page_index] = replacement;
        } else if let Some(page_index) = existing_tail {
            let required = visible_page_len(new_len, page_index);
            let values = Arc::get_mut(&mut self.pages[page_index])
                .expect("reserved tail page remains exclusively owned");
            values.resize(required.max(values.len()), self.default.clone());
            values[old_len % VALUES_PER_PAGE..required].fill(self.default.clone());
        }
        self.pages.extend(new_pages);
        self.len = len;
        Ok(())
    }

    /// Truncate trailing positions without cloning an untouched tail page.
    pub fn truncate(&mut self, len: usize) {
        let len = self.len.min(u32::try_from(len).unwrap_or(u32::MAX));
        self.pages
            .truncate((len as usize).div_ceil(VALUES_PER_PAGE));
        self.len = len;
    }

    /// Enumerate immutable page allocations for cross-fork deduplication.
    pub fn shared_pages(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.pages.iter().map(|values| {
            (
                Arc::as_ptr(values) as usize,
                crate::resident::slice_bytes::<T>(values.capacity()),
            )
        })
    }

    /// Deterministic resident bytes, including the dense page table.
    #[must_use]
    pub fn owned_memory_bytes(&self) -> usize {
        self.pages.iter().fold(
            crate::resident::slice_bytes::<Arc<Vec<T>>>(self.pages.capacity()),
            |bytes, page| bytes.saturating_add(crate::resident::slice_bytes::<T>(page.capacity())),
        )
    }

    /// Number of visible values.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether no values are visible.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn try_unique_page(&mut self, page_index: usize) -> Result<&mut Vec<T>, CapacityError>
    where
        T: Clone,
    {
        if Arc::get_mut(&mut self.pages[page_index]).is_none() {
            self.pages[page_index] = Arc::new(try_clone_page(
                &self.pages[page_index],
                self.pages[page_index].len(),
            )?);
        }
        Ok(Arc::get_mut(&mut self.pages[page_index])
            .expect("copy-on-write page is exclusively owned"))
    }
}

impl<T: PartialEq> PartialEq for PagedCowVec<T> {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && (0..self.len()).all(|index| self.get(index) == other.get(index))
    }
}

impl<T: Eq> Eq for PagedCowVec<T> {}

fn visible_page_len(len: usize, page: usize) -> usize {
    len.saturating_sub(page * VALUES_PER_PAGE)
        .min(VALUES_PER_PAGE)
}

fn try_clone_page<T: Clone>(values: &[T], capacity: usize) -> Result<Vec<T>, CapacityError> {
    let mut replacement = Vec::new();
    replacement
        .try_reserve_exact(values.len().max(capacity))
        .map_err(|_| CapacityError)?;
    replacement.extend(values.iter().cloned());
    Ok(replacement)
}
