// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::TimingInstance;

/// Compact direct lookup from a stable `TimingInstanceId` to its linked cell.
/// `Option<LibraryCellId>` is four bytes because `LibraryCellId` uses a
/// non-zero representation, so dense mapped designs need one word per
/// instance and no object-level map allocations.
#[derive(Debug)]
pub(in crate::analysis::topology) struct InstanceCellArena {
    slots: opto_core::PagedCowVec<Option<LibraryCellId>>,
}

impl InstanceCellArena {
    pub(in crate::analysis::topology) fn try_from_entries(
        slot_count: usize,
        entries: impl IntoIterator<Item = Result<(TimingInstanceId, LibraryCellId), crate::TimingError>>,
    ) -> Result<Self, crate::TimingError> {
        if slot_count > u32::MAX as usize {
            return Err(instance_cell_capacity());
        }
        let mut slots = opto_core::PagedCowVec::new(None);
        slots
            .try_resize(slot_count)
            .map_err(|_| instance_cell_capacity())?;
        for entry in entries {
            let (instance, cell) = entry?;
            let index = instance.raw() as usize;
            if index >= slots.len() {
                return Err(crate::TimingAnalysisError::InconsistentTopology.into());
            }
            if slots
                .try_set(index, Some(cell))
                .map_err(|_| instance_cell_capacity())?
                .flatten()
                .is_some()
            {
                return Err(
                    crate::TimingModelError::DuplicateInstanceId { id: instance.raw() }.into(),
                );
            }
        }
        Ok(Self { slots })
    }

    pub(in crate::analysis::topology) fn fork_shared(&self) -> Self {
        Self {
            slots: self.slots.fork_shared(),
        }
    }

    pub(in crate::analysis::topology) fn owned_memory_bytes(&self) -> usize {
        self.slots.owned_memory_bytes()
    }

    pub(in crate::analysis::topology) fn shared_pages(
        &self,
    ) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.slots.shared_pages()
    }

    pub(in crate::analysis::topology) fn get(
        &self,
        instance: TimingInstanceId,
    ) -> Option<LibraryCellId> {
        self.slots.get(instance.raw() as usize).copied().flatten()
    }

    pub(in crate::analysis::topology) fn insert(
        &mut self,
        instance: TimingInstanceId,
        cell: LibraryCellId,
    ) -> Result<Option<LibraryCellId>, crate::TimingError> {
        let index = instance.raw() as usize;
        self.slots
            .try_set(index, Some(cell))
            .map(Option::flatten)
            .map_err(|_| instance_cell_capacity())
    }

    pub(in crate::analysis::topology) fn remove(
        &mut self,
        instance: TimingInstanceId,
    ) -> Result<Option<LibraryCellId>, crate::TimingError> {
        let index = instance.raw() as usize;
        if index >= self.slots.len() {
            return Ok(None);
        }
        self.slots
            .try_set(index, None)
            .map(Option::flatten)
            .map_err(|_| instance_cell_capacity())
    }

    pub(in crate::analysis::topology) fn truncate(&mut self, len: usize) {
        self.slots.truncate(len);
    }

    pub(in crate::analysis::topology) fn trim(&mut self) {
        while self.slots.get(self.slots.len().saturating_sub(1)) == Some(&None) {
            self.slots.truncate(self.slots.len() - 1);
        }
    }

    pub(in crate::analysis::topology) fn len(&self) -> usize {
        self.slots.len()
    }
}

impl Default for InstanceCellArena {
    fn default() -> Self {
        Self {
            slots: opto_core::PagedCowVec::new(None),
        }
    }
}

fn instance_cell_capacity() -> crate::TimingError {
    crate::TimingModelError::Capacity {
        resource: "instance-cell arena",
    }
    .into()
}

/// Name-sorted typed IDs into the immutable timing-library arena.
///
/// The index duplicates neither cell records nor names and is built once with
/// the graph, so incremental region commits do not rescan the whole library.
#[derive(Debug, Default, Clone)]
pub(in crate::analysis::topology) struct LibraryCellIndex {
    by_name: Box<[LibraryCellId]>,
}

impl LibraryCellIndex {
    pub(in crate::analysis::topology) fn owned_memory_bytes(&self) -> usize {
        opto_core::resident::slice_bytes::<LibraryCellId>(self.by_name.len())
    }

    pub(in crate::analysis::topology) fn build(
        library: &TimingLibrary,
    ) -> Result<Self, crate::TimingError> {
        let mut by_name = (0..library.cells.len())
            .map(LibraryCellId::from_index)
            .collect::<Result<Vec<_>, _>>()?;
        by_name.sort_unstable_by(|left, right| {
            Self::cell(library, *left)
                .name()
                .cmp(Self::cell(library, *right).name())
        });
        Ok(Self {
            by_name: by_name.into_boxed_slice(),
        })
    }

    pub(in crate::analysis::topology) fn resolve(
        &self,
        library: &TimingLibrary,
        instance: &TimingInstance,
    ) -> Result<LibraryCellId, crate::TimingError> {
        self.resolve_name(library, &instance.name, &instance.cell)
    }

    pub(in crate::analysis::topology) fn resolve_name(
        &self,
        library: &TimingLibrary,
        instance: &str,
        cell: &str,
    ) -> Result<LibraryCellId, crate::TimingError> {
        let position = self
            .by_name
            .binary_search_by(|id| Self::cell(library, *id).name().cmp(cell))
            .map_err(|_| crate::TimingModelError::UnknownCell {
                instance: instance.to_string(),
                cell: cell.to_string(),
            })?;
        let id = self.by_name[position];
        let duplicate_before = position
            .checked_sub(1)
            .and_then(|position| self.by_name.get(position))
            .is_some_and(|other| Self::cell(library, *other).name() == cell);
        let duplicate_after = self
            .by_name
            .get(position + 1)
            .is_some_and(|other| Self::cell(library, *other).name() == cell);
        if duplicate_before || duplicate_after {
            return Err(crate::TimingModelError::AmbiguousCell {
                instance: instance.to_string(),
                cell: cell.to_string(),
            }
            .into());
        }
        Ok(id)
    }

    pub(in crate::analysis::topology) fn cell(
        library: &TimingLibrary,
        id: LibraryCellId,
    ) -> TargetCellRef<'_> {
        library
            .cells
            .get(id.index())
            .expect("library-cell IDs originate from the current immutable timing library")
    }
}
