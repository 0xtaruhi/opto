// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional implementation-provenance updates for mapped region edits.

use super::{
    BoundaryEdge, BoundaryEdgeId, ImplementationDb, MappedCellOwnership, MappedOwnerId,
    MappedOwnerImpact, OriginSetId, RegionOwnerId,
};
use crate::{OperatorId, RegionAnchorId};
use opto_ir::mapped::{AppliedRegionDelta, CellId, MappedGenerationId, MappedNetlist, TempCellId};
use std::collections::{BTreeMap, BTreeSet};

impl ImplementationDb {
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
                ownership: self.prepare_ownership(&lineage.ownership)?,
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

        let mut owner_impact = MappedOwnerImpact::default();
        // A pin reconnection is part of the boundary artifact's exact
        // footprint, not a replacement of the sink region. Only a removed or
        // payload-changed existing cell invalidates its own region plan.
        for (cell, previous) in applied.previous_live_cells() {
            if mapped.cell(cell) != Some(previous) {
                owner_impact.record(cell, self.cell_ownership(cell)?);
            }
        }
        for addition in &additions {
            match addition.ownership {
                PreparedOwnership::Global => {
                    owner_impact.record(addition.cell, MappedCellOwnership::Global);
                }
                PreparedOwnership::Region(region) => {
                    owner_impact.record(addition.cell, MappedCellOwnership::Region(region));
                }
                PreparedOwnership::Boundary(edge) => {
                    owner_impact.record_boundary(edge.driver, edge.sink);
                }
            }
        }

        Ok(PreparedImplementationEdit {
            generation: mapped.generation_id(),
            removals,
            additions,
            owner_impact,
        })
    }

    pub(crate) fn commit_region_edit(
        &mut self,
        edit: PreparedImplementationEdit,
    ) -> Result<(), crate::SynthError> {
        self.require_generation(edit.generation)?;
        let PreparedImplementationEdit {
            generation: _,
            removals,
            additions,
            owner_impact,
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
            .ok_or_else(|| {
                crate::SynthError::invariant("implementation origin-set count overflow")
            })?;
        if final_set_count > u32::MAX as usize {
            return Err(crate::SynthError::invariant(
                "implementation origin-set ID exceeds capacity",
            ));
        }
        let additional_operators = pending_origins.iter().try_fold(0usize, |total, set| {
            total.checked_add(set.len()).ok_or_else(|| {
                crate::SynthError::invariant("implementation origin operator count overflow")
            })
        })?;
        let final_operator_count = self
            .origin_operators
            .len()
            .checked_add(additional_operators)
            .ok_or_else(|| {
                crate::SynthError::invariant("implementation origin operator count overflow")
            })?;
        if final_operator_count > u32::MAX as usize {
            return Err(crate::SynthError::invariant(
                "implementation origin operator table exceeds capacity",
            ));
        }
        let mut pending_owners = additions
            .iter()
            .filter_map(|addition| match &addition.ownership {
                PreparedOwnership::Region(owner) => Some(*owner),
                PreparedOwnership::Global | PreparedOwnership::Boundary(_) => None,
            })
            .filter(|owners| !self.region_owner_ids.contains_key(owners))
            .collect::<Vec<_>>();
        pending_owners.sort_unstable();
        pending_owners.dedup();
        let final_owner_count = self
            .region_owners
            .len()
            .checked_add(pending_owners.len())
            .ok_or_else(|| crate::SynthError::invariant("mapped region-owner count overflow"))?;
        if final_owner_count >= MappedOwnerId::BOUNDARY_TAG as usize {
            return Err(crate::SynthError::capacity("mapped region-owner count"));
        }
        let mut pending_edges = additions
            .iter()
            .filter_map(|addition| match addition.ownership {
                PreparedOwnership::Boundary(edge) => Some(edge),
                PreparedOwnership::Global | PreparedOwnership::Region(_) => None,
            })
            .filter(|edge| !self.boundary_edge_ids.contains_key(edge))
            .collect::<Vec<_>>();
        pending_edges.sort_unstable();
        pending_edges.dedup();
        let final_edge_count = self
            .boundary_edges
            .len()
            .checked_add(pending_edges.len())
            .ok_or_else(|| crate::SynthError::invariant("mapped boundary-edge count overflow"))?;
        if final_edge_count >= MappedOwnerId::BOUNDARY_TAG as usize {
            return Err(crate::SynthError::capacity("mapped boundary-edge count"));
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
        for owner in pending_owners {
            let id = RegionOwnerId(
                u32::try_from(self.region_owners.len() + 1)
                    .map_err(|_| crate::SynthError::capacity("mapped region owner count"))?,
            );
            self.region_owners.push(owner);
            self.region_owner_ids.insert(owner, id);
        }
        for edge in pending_edges {
            let id = BoundaryEdgeId(
                u32::try_from(self.boundary_edges.len())
                    .map_err(|_| crate::SynthError::capacity("mapped boundary-edge count"))?,
            );
            self.boundary_edges.push(edge);
            self.boundary_edge_cells.push(Vec::new());
            self.boundary_edge_ids.insert(edge, id);
        }

        let assignments = additions
            .iter()
            .map(|addition| {
                let origin = self
                    .origin_id(&addition.operators)
                    .expect("implementation origin was preinterned");
                let owner = match &addition.ownership {
                    PreparedOwnership::Global => MappedOwnerId::region(RegionOwnerId::GLOBAL)?,
                    PreparedOwnership::Region(owner) => MappedOwnerId::region(
                        *self
                            .region_owner_ids
                            .get(owner)
                            .expect("mapped region owner was preinterned"),
                    )?,
                    PreparedOwnership::Boundary(edge) => MappedOwnerId::boundary(
                        *self
                            .boundary_edge_ids
                            .get(edge)
                            .expect("mapped boundary edge was preinterned"),
                    )?,
                };
                Ok((addition.cell, origin, owner))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let required_slots = assignments
            .iter()
            .map(|(cell, _, _)| cell.index() + 1)
            .max()
            .unwrap_or(self.cell_origins.len());
        self.cell_origins.resize(required_slots, OriginSetId::EMPTY);
        self.cell_owners.resize(required_slots, None);
        let mut touched_edges = BTreeSet::new();
        for &cell in &removals {
            if let Some(edge) = self
                .owner_for_cell(cell)
                .and_then(MappedOwnerId::boundary_id)
            {
                self.boundary_edge_cells[edge.0 as usize].retain(|&owned| owned != cell);
                touched_edges.insert(edge);
            }
            self.cell_origins[cell.index()] = OriginSetId::EMPTY;
            self.cell_owners[cell.index()] = None;
        }
        for &(cell, origin, owner) in &assignments {
            self.cell_origins[cell.index()] = origin;
            self.cell_owners[cell.index()] = Some(owner);
            if let Some(edge) = owner.boundary_id() {
                self.boundary_edge_cells[edge.0 as usize].push(cell);
                touched_edges.insert(edge);
            }
        }
        for edge in touched_edges {
            let cells = &mut self.boundary_edge_cells[edge.0 as usize];
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
        self.committed_owner_impact.merge(owner_impact);
        Ok(())
    }

    fn prepare_ownership(
        &self,
        lineage: &OwnershipLineage,
    ) -> Result<PreparedOwnership, crate::SynthError> {
        match lineage {
            OwnershipLineage::Inherited(sources) => self.inherited_ownership(sources),
            OwnershipLineage::Boundary { drivers, sink } => {
                let mut driver = None;
                for &source in drivers {
                    let endpoint = self.ownership_endpoint(source)?;
                    if let Some(endpoint) = endpoint
                        && driver.replace(endpoint).is_some_and(|old| old != endpoint)
                    {
                        return Err(crate::SynthError::invariant(
                            "boundary artifact has more than one driver-region endpoint",
                        ));
                    }
                }
                let driver = driver.ok_or_else(|| {
                    crate::SynthError::invariant("boundary artifact has no driver-region endpoint")
                })?;
                let sink = self.ownership_endpoint(*sink)?.ok_or_else(|| {
                    crate::SynthError::invariant("boundary artifact has no sink-region endpoint")
                })?;
                if driver == sink {
                    return Err(crate::SynthError::invariant(
                        "boundary artifact endpoints belong to the same region",
                    ));
                }
                Ok(PreparedOwnership::Boundary(BoundaryEdge { driver, sink }))
            }
        }
    }

    fn inherited_ownership(
        &self,
        sources: &[CellId],
    ) -> Result<PreparedOwnership, crate::SynthError> {
        if sources.is_empty() {
            return Err(crate::SynthError::invariant(
                "ownership lineage requires at least one mapped source cell",
            ));
        }
        let mut owner = None;
        for &source in sources {
            let candidate = match self.cell_ownership(source)? {
                MappedCellOwnership::Region(region) => PreparedOwnership::Region(region),
                MappedCellOwnership::Boundary { driver, sink, .. } => {
                    PreparedOwnership::Boundary(BoundaryEdge { driver, sink })
                }
                MappedCellOwnership::Global => PreparedOwnership::Global,
                MappedCellOwnership::Removed | MappedCellOwnership::Unknown => {
                    return Err(crate::SynthError::invariant(format!(
                        "ownership lineage references non-live mapped cell {source:?}"
                    )));
                }
            };
            if owner
                .replace(candidate.clone())
                .is_some_and(|old| old != candidate)
            {
                return Err(crate::SynthError::invariant(
                    "one mapped artifact cannot inherit more than one owner atom",
                ));
            }
        }
        owner.ok_or_else(|| crate::SynthError::invariant("ownership lineage is empty"))
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
        ownership_sources: impl IntoIterator<Item = CellId>,
    ) -> Result<(), crate::SynthError> {
        self.insert(
            added,
            semantic_sources,
            OwnershipLineage::Inherited(sorted_cells(ownership_sources)),
        )
    }

    pub(crate) fn record_boundary_cell(
        &mut self,
        added: TempCellId,
        semantic_sources: impl IntoIterator<Item = CellId>,
        driver_sources: impl IntoIterator<Item = CellId>,
        sink: CellId,
    ) -> Result<(), crate::SynthError> {
        self.insert(
            added,
            semantic_sources,
            OwnershipLineage::Boundary {
                drivers: sorted_cells(driver_sources),
                sink,
            },
        )
    }

    fn insert(
        &mut self,
        added: TempCellId,
        semantic_sources: impl IntoIterator<Item = CellId>,
        ownership: OwnershipLineage,
    ) -> Result<(), crate::SynthError> {
        let lineage = AddedCellLineage {
            semantic_sources: sorted_cells(semantic_sources),
            ownership,
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
    ownership: OwnershipLineage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnershipLineage {
    Inherited(Box<[CellId]>),
    Boundary {
        drivers: Box<[CellId]>,
        sink: CellId,
    },
}

#[derive(Debug)]
pub(crate) struct PreparedImplementationEdit {
    generation: MappedGenerationId,
    removals: Vec<CellId>,
    additions: Vec<PreparedImplementationAddition>,
    owner_impact: MappedOwnerImpact,
}

#[derive(Debug)]
struct PreparedImplementationAddition {
    cell: CellId,
    operators: Box<[OperatorId]>,
    ownership: PreparedOwnership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreparedOwnership {
    Global,
    Region(RegionAnchorId),
    Boundary(BoundaryEdge),
}
