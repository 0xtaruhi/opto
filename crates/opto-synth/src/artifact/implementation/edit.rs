// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional provenance and fragment-containment updates for mapped edits.

use super::{FragmentFootprint, FragmentImpact, ImplementationDb, MappedFragmentId, OriginSetId};
use crate::OperatorId;
use opto_ir::mapped::{AppliedRegionDelta, CellId, MappedGenerationId, MappedNetlist, TempCellId};
use std::collections::{BTreeMap, BTreeSet};

impl ImplementationDb {
    /// Validates one applied mapped delta and prepares its durable metadata.
    pub(crate) fn prepare_region_edit(
        &self,
        mapped: &MappedNetlist,
        applied: &AppliedRegionDelta,
        lineage: &ImplementationDelta,
    ) -> Result<PreparedImplementationEdit, crate::SynthError> {
        if applied.generation_id() != mapped.generation_id() {
            return Err(crate::SynthError::invariant(
                "implementation edit belongs to another mapped generation",
            ));
        }
        self.bind_or_validate_generation(mapped.generation_id())?;
        let added_cells = applied.added_cells().collect::<BTreeMap<_, _>>();
        if added_cells.len() != lineage.added_cells.len() {
            return Err(crate::SynthError::invariant(format!(
                "mapped region added {} cells but supplied {} provenance lineages",
                added_cells.len(),
                lineage.added_cells.len()
            )));
        }

        let mut additions = Vec::with_capacity(added_cells.len());
        for (&temporary, &cell) in &added_cells {
            let lineage = lineage.added_cells.get(&temporary).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "mapped cell {cell:?} has no explicit implementation lineage"
                ))
            })?;
            if !mapped.is_live_cell(cell) {
                return Err(crate::SynthError::invariant(format!(
                    "implementation lineage targets non-live mapped cell {cell:?}"
                )));
            }
            if matches!(
                lineage.fragment,
                FragmentFootprint::Boundary { driver, sink } if driver == sink
            ) {
                return Err(crate::SynthError::invariant(
                    "mapped boundary fragment has identical endpoints",
                ));
            }
            let mut operators = Vec::new();
            for &source in &lineage.semantic_sources {
                let source_operators = self.operators_for_cell(source).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "semantic lineage references unknown mapped cell {source:?}"
                    ))
                })?;
                operators.extend_from_slice(source_operators);
            }
            operators.sort_unstable();
            operators.dedup();
            additions.push(PreparedImplementationAddition {
                cell,
                operators: operators.into_boxed_slice(),
                fragment: lineage.fragment,
            });
        }
        if let Some(temporary) = lineage
            .added_cells
            .keys()
            .find(|temporary| !added_cells.contains_key(temporary))
        {
            return Err(crate::SynthError::invariant(format!(
                "implementation lineage references absent temporary cell {temporary:?}"
            )));
        }

        let mut removals = applied
            .affected_cells()
            .filter(|cell| !mapped.is_live_cell(*cell) && cell.index() < self.cell_origins.len())
            .collect::<Vec<_>>();
        removals.sort_unstable();
        removals.dedup();

        let mut fragment_impact = FragmentImpact::default();
        for (cell, previous) in applied.previous_live_cells() {
            if mapped.cell(cell) != Some(previous) {
                fragment_impact.record(cell, self.cell_fragment(cell).map(|row| row.1));
            }
        }
        for addition in &additions {
            fragment_impact.record(addition.cell, Some(addition.fragment));
        }

        Ok(PreparedImplementationEdit {
            generation: mapped.generation_id(),
            removals,
            additions,
            fragment_impact,
        })
    }

    /// Atomically publishes prepared provenance and fragment containment.
    pub(crate) fn commit_region_edit(
        &mut self,
        edit: PreparedImplementationEdit,
    ) -> Result<(), crate::SynthError> {
        self.require_generation(edit.generation)?;
        let PreparedImplementationEdit {
            generation: _,
            removals,
            additions,
            fragment_impact,
        } = edit;
        let mut affected_operators = removals
            .iter()
            .filter_map(|&cell| self.operators_for_cell(cell))
            .flatten()
            .copied()
            .chain(
                additions
                    .iter()
                    .flat_map(|addition| addition.operators.iter().copied()),
            )
            .collect::<Vec<_>>();
        affected_operators.sort_unstable();
        affected_operators.dedup();
        if let Some(operator) = affected_operators.iter().find(|operator| {
            self.regions
                .get(operator.raw() as usize)
                .is_none_or(|region| region.operator != **operator)
        }) {
            return Err(crate::SynthError::invariant(format!(
                "implementation edit references unknown operator {}",
                operator.raw()
            )));
        }

        let mut pending_origins = additions
            .iter()
            .map(|addition| addition.operators.clone())
            .filter(|operators| self.origin_id(operators).is_none())
            .collect::<Vec<_>>();
        pending_origins.sort_unstable();
        pending_origins.dedup();
        let final_set_count = self
            .origin_offsets
            .len()
            .saturating_sub(1)
            .checked_add(pending_origins.len())
            .ok_or_else(|| crate::SynthError::capacity("implementation origin-set count"))?;
        if final_set_count > u32::MAX as usize {
            return Err(crate::SynthError::capacity(
                "implementation origin-set count",
            ));
        }
        let additional_operators = pending_origins.iter().try_fold(0usize, |total, set| {
            total
                .checked_add(set.len())
                .ok_or_else(|| crate::SynthError::capacity("implementation origin operators"))
        })?;
        if self
            .origin_operators
            .len()
            .checked_add(additional_operators)
            .is_none_or(|count| count > u32::MAX as usize)
        {
            return Err(crate::SynthError::capacity(
                "implementation origin operators",
            ));
        }
        let mut pending_fragments = additions
            .iter()
            .map(|addition| addition.fragment)
            .filter(|fragment| !self.fragment_ids.contains_key(fragment))
            .collect::<Vec<_>>();
        pending_fragments.sort_unstable();
        pending_fragments.dedup();
        if self
            .fragments
            .len()
            .checked_add(pending_fragments.len())
            .is_none_or(|count| count > u32::MAX as usize)
        {
            return Err(crate::SynthError::capacity("mapped fragment count"));
        }

        for operators in pending_origins {
            let id = OriginSetId(
                u32::try_from(self.origin_offsets.len() - 1)
                    .map_err(|_| crate::SynthError::capacity("mapped origin-set count"))?,
            );
            self.origin_operators.extend_from_slice(&operators);
            self.origin_offsets.push(
                u32::try_from(self.origin_operators.len())
                    .map_err(|_| crate::SynthError::capacity("mapped origin operator count"))?,
            );
            self.insert_origin_id(id);
        }
        for fragment in pending_fragments {
            let id = MappedFragmentId::from_index(self.fragments.len())?;
            self.fragments.push(fragment);
            self.fragment_ids.insert(fragment, id);
            self.fragment_cells.push(Vec::new());
        }

        let assignments = additions
            .iter()
            .map(|addition| {
                (
                    addition.cell,
                    self.origin_id(&addition.operators)
                        .expect("implementation origin was preinterned"),
                    self.fragment_ids[&addition.fragment],
                )
            })
            .collect::<Vec<_>>();
        let required_slots = assignments
            .iter()
            .map(|(cell, _, _)| cell.index() + 1)
            .max()
            .unwrap_or(self.cell_origins.len());
        self.cell_origins.resize(required_slots, OriginSetId::EMPTY);
        self.cell_fragments.resize(required_slots, None);
        let mut touched_fragments = BTreeSet::new();
        for &cell in &removals {
            if let Some(fragment) = self.cell_fragments[cell.index()] {
                self.fragment_cells[fragment.raw() as usize].retain(|&stored| stored != cell);
                touched_fragments.insert(fragment);
            }
            self.cell_origins[cell.index()] = OriginSetId::EMPTY;
            self.cell_fragments[cell.index()] = None;
        }
        for &(cell, origin, fragment) in &assignments {
            self.cell_origins[cell.index()] = origin;
            self.cell_fragments[cell.index()] = Some(fragment);
            self.fragment_cells[fragment.raw() as usize].push(cell);
            touched_fragments.insert(fragment);
        }
        for fragment in touched_fragments {
            let cells = &mut self.fragment_cells[fragment.raw() as usize];
            cells.sort_unstable();
            cells.dedup();
        }

        let removed = removals.into_iter().collect::<BTreeSet<_>>();
        for operator in affected_operators {
            let region = self
                .regions
                .get_mut(operator.raw() as usize)
                .expect("implementation operator was validated before commit");
            region.mapped_cells.retain(|cell| !removed.contains(cell));
            for addition in &additions {
                if addition.operators.binary_search(&operator).is_ok() {
                    region.mapped_cells.push(addition.cell);
                }
            }
            region.mapped_cells.sort_unstable();
            region.mapped_cells.dedup();
        }
        self.committed_fragment_impact.merge(fragment_impact);
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ImplementationDelta {
    added_cells: BTreeMap<TempCellId, AddedCellLineage>,
}

impl PartialEq for ImplementationDelta {
    fn eq(&self, other: &Self) -> bool {
        self.added_cells.len() == other.added_cells.len()
            && self.added_cells.iter().zip(&other.added_cells).all(
                |((left_id, left_lineage), (right_id, right_lineage))| {
                    left_id.ordinal() == right_id.ordinal() && left_lineage == right_lineage
                },
            )
    }
}

impl Eq for ImplementationDelta {}

impl ImplementationDelta {
    pub(crate) fn record_added_cell(
        &mut self,
        added: TempCellId,
        semantic_sources: impl IntoIterator<Item = CellId>,
        fragment: FragmentFootprint,
    ) -> Result<(), crate::SynthError> {
        let lineage = AddedCellLineage {
            semantic_sources: sorted_cells(semantic_sources),
            fragment,
        };
        if self.added_cells.insert(added, lineage).is_some() {
            return Err(crate::SynthError::invariant(format!(
                "temporary mapped cell {added:?} has duplicate implementation lineage"
            )));
        }
        Ok(())
    }
}

fn sorted_cells(sources: impl IntoIterator<Item = CellId>) -> Box<[CellId]> {
    let mut sources = sources.into_iter().collect::<Vec<_>>();
    sources.sort_unstable();
    sources.dedup();
    sources.into_boxed_slice()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AddedCellLineage {
    semantic_sources: Box<[CellId]>,
    fragment: FragmentFootprint,
}

#[derive(Debug)]
pub(crate) struct PreparedImplementationEdit {
    generation: MappedGenerationId,
    removals: Vec<CellId>,
    additions: Vec<PreparedImplementationAddition>,
    fragment_impact: FragmentImpact,
}

#[derive(Debug)]
struct PreparedImplementationAddition {
    cell: CellId,
    operators: Box<[OperatorId]>,
    fragment: FragmentFootprint,
}
