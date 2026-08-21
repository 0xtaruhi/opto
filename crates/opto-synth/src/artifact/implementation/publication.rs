// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Provenance translation across the final mapped publication boundary.

use super::{CellId, ImplementationDb, Ordering, OriginSetId};
use opto_ir::mapped::MappedCellRemap;

impl ImplementationDb {
    /// Translates stable optimization-time cell slots to the one dense
    /// publication generation.
    ///
    /// The origin and fragment arenas are content-addressed and therefore survive
    /// unchanged; only their per-cell rows and the operator-to-cell reverse
    /// index cross the generation boundary.
    pub(crate) fn remap_cells_for_publication(
        &mut self,
        remap: &MappedCellRemap,
    ) -> Result<(), crate::SynthError> {
        self.require_generation(remap.source_generation())?;
        if remap.source_generation() == remap.target_generation() {
            return Err(crate::SynthError::invariant(
                "publication cell translation does not cross mapped generations",
            ));
        }
        if !self.committed_fragment_impact.is_empty() {
            return Err(crate::SynthError::invariant(
                "cannot repack implementation fragments with unconsumed mapped edits",
            ));
        }
        if remap.old_cell_slot_count() != self.cell_origins.len()
            || self.cell_origins.len() != self.cell_fragments.len()
        {
            return Err(crate::SynthError::invariant(
                "publication cell translation does not match implementation slot indexes",
            ));
        }

        let mut cell_origins = vec![OriginSetId::EMPTY; remap.cell_count()];
        let mut cell_fragments = vec![None; remap.cell_count()];
        let mut occupied = vec![false; remap.cell_count()];
        for (index, (&origin, &fragment)) in self
            .cell_origins
            .iter()
            .zip(&self.cell_fragments)
            .enumerate()
        {
            let old = CellId::from_index(index).map_err(crate::SynthError::Mapped)?;
            let Some(new) = remap.cell(old) else {
                if fragment.is_some() || origin != OriginSetId::EMPTY {
                    return Err(crate::SynthError::invariant(
                        "publication translation discarded a live implementation cell",
                    ));
                }
                continue;
            };
            let Some(fragment) = fragment else {
                return Err(crate::SynthError::invariant(
                    "publication translation retained a removed implementation cell",
                ));
            };
            let Some(mark) = occupied.get_mut(new.index()) else {
                return Err(crate::SynthError::invariant(
                    "publication translation produced an out-of-range cell ID",
                ));
            };
            if std::mem::replace(mark, true) {
                return Err(crate::SynthError::invariant(
                    "publication translation produced duplicate cell IDs",
                ));
            }
            cell_origins[new.index()] = origin;
            cell_fragments[new.index()] = Some(fragment);
        }
        if occupied.iter().any(|&used| !used) {
            return Err(crate::SynthError::invariant(
                "publication translation does not cover its dense cell domain",
            ));
        }

        let region_cells = self
            .regions
            .iter()
            .map(|region| {
                let mut cells = region
                    .mapped_cells
                    .iter()
                    .map(|&cell| {
                        remap.cell(cell).ok_or_else(|| {
                            crate::SynthError::invariant(format!(
                                "implementation region {} references a removed publication cell",
                                region.id.raw()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let original_len = cells.len();
                cells.sort_unstable();
                cells.dedup();
                if cells.len() != original_len {
                    return Err(crate::SynthError::invariant(format!(
                        "implementation region {} contains duplicate publication cells",
                        region.id.raw()
                    )));
                }
                Ok(cells)
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let fragment_cells = self
            .fragment_cells
            .iter()
            .enumerate()
            .map(|(fragment, cells)| {
                let mut cells = cells
                    .iter()
                    .map(|&cell| {
                        remap.cell(cell).ok_or_else(|| {
                            crate::SynthError::invariant(format!(
                                "fragment {fragment} references a removed publication cell"
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                cells.sort_unstable();
                cells.dedup();
                Ok(cells)
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;

        self.cell_origins = cell_origins;
        self.cell_fragments = cell_fragments;
        for (region, cells) in self.regions.iter_mut().zip(region_cells) {
            region.mapped_cells = cells;
        }
        self.fragment_cells = fragment_cells;
        self.mapped_generation
            .store(remap.target_generation().get().get(), Ordering::Release);
        Ok(())
    }
}
