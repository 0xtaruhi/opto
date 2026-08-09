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
}

impl StructuralOwnershipProvenance {
    #[cfg(test)]
    pub(crate) fn global(module: &word::WordModule) -> Self {
        Self {
            owners: vec![None; module.operations().len()],
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
        Ok(Self {
            owners: graph.operation_owner_rows().to_vec(),
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

    pub(crate) fn len(&self) -> usize {
        self.owners.len()
    }

    pub(crate) fn verify_frozen(
        &self,
        module: &word::WordModule,
        graph: &super::SynthesisRegionGraph,
    ) -> Result<(), crate::SynthError> {
        if self.owners.len() != module.operations().len()
            || graph.operation_owner_rows().len() != self.owners.len()
        {
            return Err(crate::SynthError::invariant(
                "frozen region graph does not cover structural ownership",
            ));
        }
        let mut resolved = std::collections::BTreeMap::new();
        for index in 0..self.owners.len() {
            let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
            let Some(owner) = self.owner(operation) else {
                continue;
            };
            let Some(frozen) = graph.operation_owner_rows()[index] else {
                continue;
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
}
