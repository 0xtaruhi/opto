// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Compile-time-typed source provenance for region-local operations.

use opto_ir::word;
use smallvec::SmallVec;

/// The sole mutable owner of local-operation to source-operation provenance.
///
/// Rows are dense in the local Word operation arena. Every mutation sorts and
/// deduplicates its source set, so import, generated-node sharing, optimization,
/// binding, and durable publication cannot maintain competing representations.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalOperationProvenance {
    rows: Vec<SmallVec<[word::OpId; 1]>>,
}

impl LocalOperationProvenance {
    /// Sets one dense provenance row, appending it when the operation is new.
    pub(crate) fn set(
        &mut self,
        local: word::OpId,
        sources: impl IntoIterator<Item = word::OpId>,
    ) -> Result<(), crate::SynthError> {
        let sources = normalize(sources);
        if local.index() == self.rows.len() {
            self.rows.push(sources);
            return Ok(());
        }
        let row = self.rows.get_mut(local.index()).ok_or_else(|| {
            crate::SynthError::invariant("local operation provenance rows are not dense")
        })?;
        *row = sources;
        Ok(())
    }

    /// Merges additional source operations into an existing local operation.
    pub(crate) fn merge(
        &mut self,
        local: word::OpId,
        sources: impl IntoIterator<Item = word::OpId>,
    ) -> Result<(), crate::SynthError> {
        let mut merged = self
            .sources(local)
            .ok_or_else(|| {
                crate::SynthError::invariant("local operation provenance row is absent")
            })?
            .to_vec();
        merged.extend(sources);
        self.set(local, merged)
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
            self.set(local, sources)?;
        }
        Ok(())
    }

    /// Attributes every helper in an SSA replacement to the operations it replaces.
    pub(crate) fn apply_rewrites(
        &mut self,
        module: &word::WordModule,
        rewrites: &[crate::planning::operator::OperationRewrite],
    ) -> Result<(), crate::SynthError> {
        self.inherit_appended(module)?;
        for rewrite in rewrites {
            let mut sources = Vec::new();
            for &operation in &rewrite.replaced {
                sources.extend(self.sources(operation).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "an SSA replacement references missing operation provenance",
                    )
                })?);
            }
            for index in rewrite.created.clone() {
                let operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
                self.merge(operation, sources.iter().copied())?;
            }
        }
        Ok(())
    }

    /// Returns the normalized source-operation set for one local operation.
    pub(crate) fn sources(&self, local: word::OpId) -> Option<&[word::OpId]> {
        self.rows.get(local.index()).map(SmallVec::as_slice)
    }

    /// Iterates over source sets in dense local operation order.
    #[cfg(test)]
    pub(crate) fn source_sets(&self) -> impl Iterator<Item = &[word::OpId]> {
        self.rows.iter().map(SmallVec::as_slice)
    }
}

fn normalize(sources: impl IntoIterator<Item = word::OpId>) -> SmallVec<[word::OpId; 1]> {
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
        let mut provenance = LocalOperationProvenance::default();

        provenance.set(local, [second, first, second]).unwrap();
        assert_eq!(provenance.sources(local).unwrap(), [first, second]);
        provenance.set(local, [second]).unwrap();
        provenance.merge(local, [first, second]).unwrap();
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
        let mut provenance = LocalOperationProvenance::default();
        provenance.set(original_operation, [source]).unwrap();

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
