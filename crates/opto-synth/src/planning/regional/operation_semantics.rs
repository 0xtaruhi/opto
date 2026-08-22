// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Semantic bindings for region-local operations.

use opto_ir::word;
use smallvec::SmallVec;

/// Stable semantic source of one private sequential operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LocalStateSource {
    Operation(word::OpId),
    Memory {
        memory: word::MemoryId,
        ordinal: u32,
    },
}

#[derive(Debug, Clone, Default)]
struct LocalOperationBinding {
    operations: SmallVec<[word::OpId; 1]>,
    states: SmallVec<[LocalStateSource; 1]>,
}

/// Dense semantic bindings for the private Word operation arena.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalOperationSemantics {
    rows: Vec<LocalOperationBinding>,
}

impl LocalOperationSemantics {
    fn set_row(
        &mut self,
        local: word::OpId,
        sources: impl IntoIterator<Item = word::OpId>,
        states: impl IntoIterator<Item = LocalStateSource>,
    ) -> Result<(), crate::SynthError> {
        let binding = LocalOperationBinding {
            operations: normalize(sources),
            states: normalize(states),
        };
        if local.index() == self.rows.len() {
            self.rows.push(binding);
            return Ok(());
        }
        let row = self.rows.get_mut(local.index()).ok_or_else(|| {
            crate::SynthError::invariant("local operation semantic rows are not dense")
        })?;
        *row = binding;
        Ok(())
    }

    /// Records one imported source operation and its state identity, if any.
    pub(crate) fn record_source(
        &mut self,
        local: word::OpId,
        source: word::OpId,
        state: bool,
    ) -> Result<(), crate::SynthError> {
        self.set_row(
            local,
            [source],
            state.then_some(LocalStateSource::Operation(source)),
        )
    }

    /// Records a generated combinational operation and its source operations.
    pub(crate) fn record_generated(
        &mut self,
        local: word::OpId,
        sources: impl IntoIterator<Item = word::OpId>,
    ) -> Result<(), crate::SynthError> {
        self.set_row(local, sources, [])
    }

    /// Binds a generated register-bank word directly to its source memory.
    pub(crate) fn record_memory_state(
        &mut self,
        local: word::OpId,
        memory: word::MemoryId,
        ordinal: u32,
    ) -> Result<(), crate::SynthError> {
        let operations = self
            .sources(local)
            .ok_or_else(|| crate::SynthError::invariant("local operation semantic row is absent"))?
            .to_vec();
        self.set_row(
            local,
            operations,
            [LocalStateSource::Memory { memory, ordinal }],
        )
    }

    /// Transfers the complete semantic row through a state replacement.
    pub(crate) fn replace_from(
        &mut self,
        local: word::OpId,
        replaced: word::OpId,
    ) -> Result<(), crate::SynthError> {
        let binding = self.rows.get(replaced.index()).cloned().ok_or_else(|| {
            crate::SynthError::invariant("replaced operation semantic row is absent")
        })?;
        let row = self.rows.get_mut(local.index()).ok_or_else(|| {
            crate::SynthError::invariant("replacement operation semantic row is absent")
        })?;
        *row = binding;
        Ok(())
    }

    /// Merges the complete semantic row after an equivalence-backed sharing rewrite.
    pub(crate) fn merge_from(
        &mut self,
        local: word::OpId,
        merged: word::OpId,
    ) -> Result<(), crate::SynthError> {
        let binding = self.rows.get(merged.index()).cloned().ok_or_else(|| {
            crate::SynthError::invariant("merged operation semantic row is absent")
        })?;
        let row = self.rows.get(local.index()).cloned().ok_or_else(|| {
            crate::SynthError::invariant("local operation semantic row is absent")
        })?;
        let mut operations = row.operations;
        operations.extend(binding.operations);
        let mut states = row.states;
        states.extend(binding.states);
        self.set_row(local, operations, states)
    }

    /// Extends a reused generated operation with newly discovered sources.
    pub(crate) fn extend_generated(
        &mut self,
        local: word::OpId,
        sources: impl IntoIterator<Item = word::OpId>,
    ) -> Result<(), crate::SynthError> {
        let row = self.rows.get(local.index()).cloned().ok_or_else(|| {
            crate::SynthError::invariant("local operation semantic row is absent")
        })?;
        let mut operations = row.operations;
        operations.extend(sources);
        self.set_row(local, operations, row.states)
    }

    /// Verifies that every operation constructed by the importer was recorded.
    pub(crate) fn seal_import(&self, module: &word::WordModule) -> Result<(), crate::SynthError> {
        if self.rows.len() != module.operations().len() {
            return Err(crate::SynthError::invariant(format!(
                "local operation provenance has {} rows for {} imported operations",
                self.rows.len(),
                module.operations().len()
            )));
        }
        Ok(())
    }

    /// Derives provenance for operations appended by a lower-level SSA builder.
    pub(crate) fn inherit_appended(
        &mut self,
        module: &word::WordModule,
    ) -> Result<(), crate::SynthError> {
        if self.rows.len() > module.operations().len() {
            return Err(crate::SynthError::invariant(
                "an SSA transform removed operations without a provenance remap",
            ));
        }
        while self.rows.len() < module.operations().len() {
            let local = word::OpId::from_index(self.rows.len()).map_err(crate::SynthError::from)?;
            let operation = module.operation(local).ok_or_else(|| {
                crate::SynthError::invariant(
                    "an appended local operation is absent from the dense SSA arena",
                )
            })?;
            let mut sources = Vec::new();
            for input in crate::word::operation_inputs(&operation.kind) {
                let Some(word::ValueKind::Operation(input)) =
                    module.value(input).map(|value| &value.kind)
                else {
                    continue;
                };
                if input.index() >= local.index() {
                    return Err(crate::SynthError::invariant(
                        "an appended SSA operation depends on a non-preceding operation",
                    ));
                }
                sources.extend(self.sources(*input).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "an appended SSA operation depends on missing provenance",
                    )
                })?);
            }
            self.record_generated(local, sources)?;
        }
        Ok(())
    }

    /// Attributes every helper in an SSA replacement to the operations it replaces.
    pub(crate) fn apply_rewrites(
        &mut self,
        module: &word::WordModule,
        rewrites: &[crate::planning::operator::OperationRewrite],
    ) -> Result<(), crate::SynthError> {
        let mut next = self.rows.len();
        for rewrite in rewrites {
            if rewrite.created.start != next
                || rewrite.created.start >= rewrite.created.end
                || rewrite.created.end > module.operations().len()
            {
                return Err(crate::SynthError::invariant(
                    "SSA replacement suffixes do not densely cover appended operations",
                ));
            }
            let mut sources = Vec::new();
            let mut states = Vec::new();
            for &operation in &rewrite.replaced {
                sources.extend(self.sources(operation).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "an SSA replacement references missing operation provenance",
                    )
                })?);
                states.extend(self.states(operation).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "an SSA replacement references missing state semantics",
                    )
                })?);
            }
            for index in rewrite.created.clone() {
                let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
                self.set_row(operation, sources.iter().copied(), states.iter().copied())?;
            }
            next = rewrite.created.end;
        }
        if next != module.operations().len() {
            return Err(crate::SynthError::invariant(
                "SSA replacements do not cover the complete appended operation suffix",
            ));
        }
        Ok(())
    }

    /// Applies the sole operation-ID remap committed by Word compaction.
    pub(crate) fn remap(&mut self, remap: &word::NetlistRemap) -> Result<(), crate::SynthError> {
        if remap.old_operation_count() != self.rows.len() {
            return Err(crate::SynthError::invariant(
                "operation provenance does not align with the compacted SSA arena",
            ));
        }
        let mut rows = vec![None; remap.operation_count()];
        for (index, row) in std::mem::take(&mut self.rows).into_iter().enumerate() {
            let old = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
            let Some(new) = remap.operation(old) else {
                continue;
            };
            if rows[new.index()].replace(row).is_some() {
                return Err(crate::SynthError::invariant(
                    "operation provenance remap is not one-to-one",
                ));
            }
        }
        self.rows = rows
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "operation provenance remap does not cover the compacted arena",
                )
            })?;
        Ok(())
    }

    /// Returns the normalized source-operation set for one local operation.
    pub(crate) fn sources(&self, local: word::OpId) -> Option<&[word::OpId]> {
        self.rows
            .get(local.index())
            .map(|row| row.operations.as_slice())
    }

    /// Returns the exact source states represented by one local operation.
    pub(crate) fn states(&self, local: word::OpId) -> Option<&[LocalStateSource]> {
        self.rows
            .get(local.index())
            .map(|row| row.states.as_slice())
    }

    /// Iterates over source sets in dense local operation order.
    #[cfg(test)]
    pub(crate) fn source_sets(&self) -> impl Iterator<Item = &[word::OpId]> {
        self.rows.iter().map(|row| row.operations.as_slice())
    }
}

fn normalize<T: Ord>(sources: impl IntoIterator<Item = T>) -> SmallVec<[T; 1]> {
    let mut sources = sources.into_iter().collect::<SmallVec<_>>();
    sources.sort_unstable();
    sources.dedup();
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_api_normalizes_record_replace_and_merge() {
        let first = word::OpId::from_index(0).unwrap();
        let second = word::OpId::from_index(1).unwrap();
        let local = word::OpId::from_index(0).unwrap();
        let mut provenance = LocalOperationSemantics::default();

        provenance
            .record_generated(local, [second, first, second])
            .unwrap();
        assert_eq!(provenance.sources(local).unwrap(), [first, second]);
        provenance.record_generated(local, [second]).unwrap();
        provenance.extend_generated(local, [first, second]).unwrap();
        assert_eq!(provenance.sources(local).unwrap(), [first, second]);
    }

    #[test]
    fn transform_inherits_new_operation_provenance_from_ssa_operands() {
        let mut module = word::WordModule::new("provenance");
        let bit = word::WordType::bits(1).unwrap();
        let input = module
            .add_port(
                "input",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let input = module
            .read_signal(
                module.port(input).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let original = module
            .unary(
                word::UnaryOp::LogicalNot,
                input,
                word::SourceSpan::default(),
            )
            .unwrap();
        let word::ValueKind::Operation(original_operation) = module.value(original).unwrap().kind
        else {
            panic!("unary builder did not create an operation");
        };
        let source = word::OpId::from_index(7).unwrap();
        let mut provenance = LocalOperationSemantics::default();
        provenance
            .record_generated(original_operation, [source])
            .unwrap();

        let generated = module
            .unary(
                word::UnaryOp::LogicalNot,
                original,
                word::SourceSpan::default(),
            )
            .unwrap();
        provenance.inherit_appended(&module).unwrap();
        let word::ValueKind::Operation(generated_operation) = module.value(generated).unwrap().kind
        else {
            panic!("unary builder did not create an operation");
        };
        assert_eq!(provenance.sources(generated_operation).unwrap(), [source]);
    }
}
