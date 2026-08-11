// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Frozen structural owner atoms consumed by the final region partition.

use opto_ir::word;

/// Durable ownership of operations rewritten during structural preparation.
///
/// Each initial operation captures its frozen region atom. A transform may
/// publish generated operations only from exact sources with one common atom.
/// The final partition consumes these atoms directly, so ownership survives
/// source-operation replacement without source-span or connectivity guesses.
#[derive(Debug, Clone)]
pub(crate) struct StructuralOwnershipProvenance {
    owners: Vec<Option<super::RegionRowId>>,
    owner_anchors: Vec<[u8; 32]>,
}

impl StructuralOwnershipProvenance {
    #[cfg(test)]
    pub(crate) fn global(module: &word::WordModule) -> Self {
        Self {
            owners: vec![None; module.operations().len()],
            owner_anchors: Vec::new(),
        }
    }

    pub(crate) fn new(
        module: &word::WordModule,
        graph: &super::SynthesisRegionGraph,
    ) -> Result<Self, crate::SynthError> {
        if graph.operation_owner_rows().len() != module.operations().len() {
            return Err(crate::SynthError::invariant(
                "initial structural ownership does not cover the operation arena",
            ));
        }
        let owners = graph.operation_owner_rows().to_vec();
        let owner_anchors = graph
            .regions()
            .iter()
            .copied()
            .map(super::SynthesisRegion::partition_anchor)
            .collect();
        Ok(Self {
            owners,
            owner_anchors,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_owners_for_test(
        module: &word::WordModule,
        owners: Vec<Option<super::RegionRowId>>,
    ) -> Result<Self, crate::SynthError> {
        if owners.len() != module.operations().len() {
            return Err(crate::SynthError::invariant(
                "test structural ownership does not cover the operation arena",
            ));
        }
        let owner_count = owners
            .iter()
            .flatten()
            .map(|owner| owner.index().saturating_add(1))
            .max()
            .unwrap_or(0);
        let owner_anchors = (0..owner_count)
            .map(|index| {
                let mut anchor = [0; 32];
                anchor[..8].copy_from_slice(&(index as u64).to_le_bytes());
                anchor
            })
            .collect();
        Ok(Self {
            owners,
            owner_anchors,
        })
    }

    pub(crate) fn start(&self, module: &word::WordModule) -> Result<usize, crate::SynthError> {
        if self.owners.len() != module.operations().len() {
            return Err(crate::SynthError::invariant(
                "structural ownership is not synchronized with the operation arena",
            ));
        }
        Ok(self.owners.len())
    }

    pub(crate) fn claim_since(
        &mut self,
        module: &word::WordModule,
        start: usize,
        sources: &[word::OpId],
    ) -> Result<(), crate::SynthError> {
        self.claim_range(module, start, module.operations().len(), sources)
    }

    pub(crate) fn claim_range(
        &mut self,
        module: &word::WordModule,
        start: usize,
        end: usize,
        sources: &[word::OpId],
    ) -> Result<(), crate::SynthError> {
        if self.owners.len() != start || start > end || end > module.operations().len() {
            return Err(crate::SynthError::invariant(
                "generated ownership lost operation-arena synchronization",
            ));
        }
        if start == end {
            return Ok(());
        }
        let mut source_owners = sources
            .iter()
            .map(|source| self.owners.get(source.index()).copied())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "generated ownership references an unknown source operation",
                )
            })?;
        source_owners.sort_unstable();
        source_owners.dedup();
        let [owner] = source_owners.as_slice() else {
            return Err(crate::SynthError::invariant(
                "generated operation does not have one exact structural owner",
            ));
        };
        self.owners.resize(end, *owner);
        Ok(())
    }

    pub(crate) fn owner(&self, operation: word::OpId) -> Option<super::RegionRowId> {
        self.owners.get(operation.index()).copied().flatten()
    }

    pub(crate) fn owners(&self) -> &[Option<super::RegionRowId>] {
        &self.owners
    }

    pub(crate) fn anchor(&self, operation: word::OpId) -> Option<[u8; 32]> {
        self.owner(operation)
            .and_then(|owner| self.owner_anchors.get(owner.index()).copied())
    }

    pub(crate) fn len(&self) -> usize {
        self.owners.len()
    }

    pub(crate) fn verify_frozen(
        &self,
        module: &word::WordModule,
        graph: &super::SynthesisRegionGraph,
    ) -> Result<(), crate::SynthError> {
        if self
            .owners
            .iter()
            .flatten()
            .any(|owner| owner.index() >= self.owner_anchors.len())
        {
            return Err(crate::SynthError::invariant(
                "structural owner anchor table does not cover ownership",
            ));
        }
        let reachable = super::region_graph::partition::synthesis_reachable_operations(module)?;
        verify_relation(&self.owners, graph.operation_owner_rows(), &reachable)
    }
}

fn verify_relation(
    structural: &[Option<super::RegionRowId>],
    frozen: &[Option<super::RegionRowId>],
    reachable: &[bool],
) -> Result<(), crate::SynthError> {
    if structural.len() != frozen.len() || structural.len() != reachable.len() {
        return Err(crate::SynthError::invariant(
            "frozen region graph does not cover structural ownership",
        ));
    }
    let mut resolved = std::collections::BTreeMap::new();
    for (index, ((owner, frozen), reachable)) in structural
        .iter()
        .copied()
        .zip(frozen.iter().copied())
        .zip(reachable.iter().copied())
        .enumerate()
    {
        if frozen.is_some() != reachable {
            return Err(crate::SynthError::invariant(
                "frozen region ownership does not match the Word root closure",
            ));
        }
        let Some(frozen) = frozen else {
            continue;
        };
        let Some(owner) = owner else {
            return Err(crate::SynthError::invariant(format!(
                "live operation {index} lost structural ownership provenance"
            )));
        };
        if resolved
            .insert(owner, frozen)
            .is_some_and(|prior| prior != frozen)
        {
            return Err(crate::SynthError::invariant(
                "final partition split one frozen structural owner",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize) -> super::super::RegionRowId {
        super::super::RegionRowId::from_index(index).unwrap()
    }

    #[test]
    fn rejects_live_operation_without_structural_owner() {
        let result = verify_relation(&[None], &[Some(row(0))], &[true]);
        assert!(result.is_err());
    }

    #[test]
    fn accepts_owned_operation_removed_from_the_root_closure() {
        verify_relation(&[Some(row(0))], &[None], &[false]).unwrap();
    }

    #[test]
    fn rejects_structural_owner_split() {
        let result = verify_relation(
            &[Some(row(0)), Some(row(0))],
            &[Some(row(0)), Some(row(1))],
            &[true, true],
        );
        assert!(result.is_err());
    }

    #[test]
    fn accepts_explicit_final_partition_merge() {
        verify_relation(
            &[Some(row(0)), Some(row(1))],
            &[Some(row(0)), Some(row(0))],
            &[true, true],
        )
        .unwrap();
    }

    #[test]
    fn frozen_verification_rejects_an_owner_without_an_anchor() {
        let mut module = word::WordModule::new("missing_anchor");
        let span = word::SourceSpan::stable("test");
        let ty = word::WordType::bits(1).unwrap();
        let input = module
            .add_port("d", word::PortDirection::Input, ty, span.clone())
            .unwrap();
        let value = module
            .read_signal(module.port(input).unwrap().signal, span.clone())
            .unwrap();
        let value = module
            .unary(word::UnaryOp::BitNot, value, span.clone())
            .unwrap();
        let output = module
            .add_port("q", word::PortDirection::Output, ty, span.clone())
            .unwrap();
        module
            .connect(
                word::LValue::signal(module.port(output).unwrap().signal),
                value,
                span,
            )
            .unwrap();
        let graph = super::super::region_graph::partition::build(
            &module,
            super::super::region_graph::RegionPartitionPolicy::default(),
        )
        .unwrap();
        let mut provenance = StructuralOwnershipProvenance::new(&module, &graph).unwrap();
        provenance.owner_anchors.clear();

        let error = provenance.verify_frozen(&module, &graph).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("structural owner anchor table does not cover ownership")
        );
    }
}
