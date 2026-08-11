// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Durable semantic operators retained through target mapping.

use super::ArchitectureDecisions;
use crate::planning::architecture::ArithmeticTerm;
use crate::{OperationAnchorId, OperatorId, OperatorKind};
use opto_ir::word;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MISSING_OPERATOR: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Content-derived identity of one complete operator semantic signature.
pub struct OperatorSignatureId([u8; 32]);

impl OperatorSignatureId {
    /// Return the canonical signature digest.
    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// One typed term in a fused additive operator.
pub enum OperatorTermShape {
    /// A direct additive input.
    Value {
        /// Type used by the fused implementation.
        ty: word::WordType,
        /// Whether this term is subtracted.
        negative: bool,
    },
    /// A product feeding the additive reduction.
    Product {
        /// Types of the two multiplicands.
        input_types: [word::WordType; 2],
        /// Type of the product term.
        ty: word::WordType,
        /// Whether this product is subtracted.
        negative: bool,
        /// Constant multiplicand position when one input is constant.
        constant_input: Option<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Complete bounded-selector semantics of a dynamic extract.
pub struct DynamicExtractShape {
    /// Largest value allowed by the selector type analysis.
    pub maximum_offset: u128,
    /// Largest selection that can address the extracted source.
    pub selection_max: u128,
    /// Proven count of low zero selector bits.
    pub alignment: u32,
    /// Width of the selector operand.
    pub offset_width: u32,
    /// Number of reachable aligned taps.
    pub tap_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Kind-specific semantics that types and widths alone cannot express.
pub enum OperatorShape {
    /// A binary or unary arithmetic operator with no additional shape data.
    Arithmetic,
    /// Fused additive and product terms in deterministic operand order.
    Sum(Box<[OperatorTermShape]>),
    /// Bounded dynamic extraction parameters.
    DynamicExtract(DynamicExtractShape),
    /// Integer division or remainder conventions.
    Division,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Complete, versioned semantics shared by equivalent operator occurrences.
pub struct OperatorSignature {
    id: OperatorSignatureId,
    kind: OperatorKind,
    input_types: Box<[word::WordType]>,
    result_type: word::WordType,
    implementation_width: u32,
    shape: OperatorShape,
}

impl OperatorSignature {
    /// Return the content-derived signature identity.
    #[must_use]
    pub const fn id(&self) -> OperatorSignatureId {
        self.id
    }

    /// Return the semantic operator kind.
    #[must_use]
    pub const fn kind(&self) -> OperatorKind {
        self.kind
    }

    /// Return the exact operand types in binding order.
    #[must_use]
    pub fn input_types(&self) -> &[word::WordType] {
        &self.input_types
    }

    /// Return the language-level result type.
    #[must_use]
    pub const fn result_type(&self) -> word::WordType {
        self.result_type
    }

    fn validate_checkpoint(&self) -> Result<(), crate::SynthError> {
        if self.id != signature_id(self)? || self.implementation_width == 0 {
            return Err(crate::SynthError::invariant(
                "operator manifest contains an invalid semantic signature",
            ));
        }
        let valid_shape = match (self.kind, &self.shape) {
            (OperatorKind::Sum, OperatorShape::Sum(terms)) => {
                let input_types = terms
                    .iter()
                    .flat_map(|term| match term {
                        OperatorTermShape::Value { ty, .. } => [Some(*ty), None],
                        OperatorTermShape::Product { input_types, .. } => input_types.map(Some),
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                !terms.is_empty() && input_types.as_slice() == self.input_types.as_ref()
            }
            (OperatorKind::DynamicExtract, OperatorShape::DynamicExtract(dynamic)) => {
                self.input_types.len() == 2 && valid_dynamic_extract(dynamic, self)
            }
            (OperatorKind::Divide | OperatorKind::Modulo, OperatorShape::Division) => {
                self.input_types.len() == 2
            }
            (
                OperatorKind::Add
                | OperatorKind::Subtract
                | OperatorKind::Increment
                | OperatorKind::Decrement
                | OperatorKind::Multiply,
                OperatorShape::Arithmetic,
            ) => self.input_types.len() == 2,
            _ => false,
        };
        if !valid_shape {
            return Err(crate::SynthError::invariant(
                "operator manifest signature kind and shape disagree",
            ));
        }
        Ok(())
    }
}

fn valid_dynamic_extract(shape: &DynamicExtractShape, signature: &OperatorSignature) -> bool {
    let [value, offset] = signature.input_types.as_ref() else {
        return false;
    };
    if offset.is_signed()
        || offset.width() != shape.offset_width
        || signature.result_type.width() > value.width()
        || shape.selection_max > shape.maximum_offset
        || shape.selection_max > u128::from(value.width() - signature.result_type.width())
        || shape.alignment > shape.offset_width
        || (shape.offset_width < u128::BITS && shape.maximum_offset >= 1u128 << shape.offset_width)
    {
        return false;
    }
    let stride = 1u128
        .checked_shl(shape.alignment.min(127))
        .unwrap_or(u128::MAX);
    shape.tap_count
        == shape
            .selection_max
            .checked_div(stride)
            .and_then(|count| count.checked_add(1))
            .and_then(|count| u32::try_from(count).ok())
            .unwrap_or(u32::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One durable operator occurrence with separate semantics and provenance.
pub struct PreservedOperatorInstance {
    operator: OperatorId,
    anchor: OperationAnchorId,
    signature: OperatorSignatureId,
    operands: Box<[word::ValueId]>,
    result: word::ValueId,
    source_operations: Box<[word::OpId]>,
}

impl PreservedOperatorInstance {
    /// Return the region-local dense operator ID used by architecture decisions.
    #[must_use]
    pub const fn operator(&self) -> OperatorId {
        self.operator
    }

    /// Return the stable source occurrence identity.
    #[must_use]
    pub const fn anchor(&self) -> OperationAnchorId {
        self.anchor
    }

    /// Return the shared semantic signature identity.
    #[must_use]
    pub const fn signature(&self) -> OperatorSignatureId {
        self.signature
    }

    /// Return region-local operands in the signature's binding order.
    #[must_use]
    pub fn operands(&self) -> &[word::ValueId] {
        &self.operands
    }

    /// Return the region-local result value.
    #[must_use]
    pub const fn result(&self) -> word::ValueId {
        self.result
    }

    /// Return immutable source operations fused into this occurrence.
    #[must_use]
    pub fn source_operations(&self) -> &[word::OpId] {
        &self.source_operations
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Region-owned operator records that outlive one architecture-decision view.
pub struct DurableOperatorArena {
    signatures: Box<[OperatorSignature]>,
    instances: Box<[PreservedOperatorInstance]>,
    operator_by_local_operation: Box<[u32]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Checkpoint-stable operator semantics and occurrence identities.
pub struct OperatorManifest {
    signatures: Box<[OperatorSignature]>,
    instances: Box<[OperatorManifestInstance]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One operator occurrence retained by the synthesis database.
pub struct OperatorManifestInstance {
    anchor: OperationAnchorId,
    signature: OperatorSignatureId,
    source_operations: Box<[word::OpId]>,
}

impl OperatorManifest {
    pub(crate) fn capture<'a>(
        arenas: impl ExactSizeIterator<Item = &'a DurableOperatorArena> + Clone,
    ) -> Result<Self, crate::SynthError> {
        let mut signatures = BTreeMap::new();
        for signature in arenas.clone().flat_map(DurableOperatorArena::signatures) {
            match signatures.entry(signature.id()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(signature.clone());
                }
                std::collections::btree_map::Entry::Occupied(entry) if entry.get() != signature => {
                    return Err(crate::SynthError::invariant(
                        "operator manifest signature digest collision",
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
        let expected = arenas
            .clone()
            .map(|arena| arena.instances().len())
            .sum::<usize>();
        let instances = arenas
            .flat_map(DurableOperatorArena::instances)
            .map(|instance| OperatorManifestInstance {
                anchor: instance.anchor(),
                signature: instance.signature(),
                source_operations: instance.source_operations().into(),
            })
            .collect::<Vec<_>>();
        if instances.len() != expected {
            return Err(crate::SynthError::invariant(
                "operator manifest occurrences do not align with durable arenas",
            ));
        }
        Ok(Self {
            signatures: signatures.into_values().collect(),
            instances: instances.into_boxed_slice(),
        })
    }

    pub(crate) fn validate_checkpoint(&self) -> Result<(), crate::SynthError> {
        for signature in &self.signatures {
            signature.validate_checkpoint()?;
        }
        let signatures = self
            .signatures
            .iter()
            .map(OperatorSignature::id)
            .collect::<std::collections::BTreeSet<_>>();
        if signatures.len() != self.signatures.len()
            || !self
                .signatures
                .windows(2)
                .all(|pair| pair[0].id() < pair[1].id())
            || self
                .instances
                .iter()
                .any(|instance| !signatures.contains(&instance.signature))
        {
            return Err(crate::SynthError::invariant(
                "operator manifest references an invalid semantic signature",
            ));
        }
        let used_signatures = self
            .instances
            .iter()
            .map(OperatorManifestInstance::signature)
            .collect::<std::collections::BTreeSet<_>>();
        let anchors = self
            .instances
            .iter()
            .map(OperatorManifestInstance::anchor)
            .collect::<std::collections::BTreeSet<_>>();
        let mut source_operations = std::collections::BTreeSet::new();
        if used_signatures != signatures
            || anchors.len() != self.instances.len()
            || self.instances.iter().any(|instance| {
                instance.source_operations.is_empty()
                    || !instance
                        .source_operations
                        .windows(2)
                        .all(|pair| pair[0] < pair[1])
                    || instance
                        .source_operations
                        .iter()
                        .any(|&operation| !source_operations.insert(operation))
            })
        {
            return Err(crate::SynthError::invariant(
                "operator manifest has invalid occurrence identities",
            ));
        }
        Ok(())
    }

    pub(crate) fn owned_memory_bytes(&self) -> usize {
        let signature_bytes = self
            .signatures
            .len()
            .saturating_mul(size_of::<OperatorSignature>())
            + self
                .signatures
                .iter()
                .map(|signature| {
                    signature
                        .input_types
                        .len()
                        .saturating_mul(size_of::<word::WordType>())
                        .saturating_add(match &signature.shape {
                            OperatorShape::Sum(terms) => {
                                terms.len().saturating_mul(size_of::<OperatorTermShape>())
                            }
                            OperatorShape::Arithmetic
                            | OperatorShape::DynamicExtract(_)
                            | OperatorShape::Division => 0,
                        })
                })
                .sum::<usize>();
        signature_bytes
            .saturating_add(
                self.instances
                    .len()
                    .saturating_mul(size_of::<OperatorManifestInstance>()),
            )
            .saturating_add(
                self.instances
                    .iter()
                    .map(|instance| {
                        instance
                            .source_operations
                            .len()
                            .saturating_mul(size_of::<word::OpId>())
                    })
                    .sum(),
            )
    }

    pub(crate) fn serialized_size(&self) -> Result<usize, crate::SynthError> {
        opto_archive::serialized_size(self).map_err(|error| {
            crate::SynthError::invariant(format!("operator manifest encoding: {error}"))
        })
    }

    /// Return complete semantic signatures in canonical digest order.
    #[must_use]
    pub fn signatures(&self) -> &[OperatorSignature] {
        &self.signatures
    }

    /// Return operator occurrences in deterministic regional order.
    #[must_use]
    pub fn instances(&self) -> &[OperatorManifestInstance] {
        &self.instances
    }

    /// Look up a retained signature by content identity.
    #[must_use]
    pub fn signature(&self, id: OperatorSignatureId) -> Option<&OperatorSignature> {
        self.signatures
            .binary_search_by_key(&id, OperatorSignature::id)
            .ok()
            .map(|index| &self.signatures[index])
    }
}

impl OperatorManifestInstance {
    /// Return the stable source occurrence identity.
    #[must_use]
    pub const fn anchor(&self) -> OperationAnchorId {
        self.anchor
    }

    /// Return the complete semantic-signature identity.
    #[must_use]
    pub const fn signature(&self) -> OperatorSignatureId {
        self.signature
    }

    /// Return source operations represented by this occurrence.
    #[must_use]
    pub fn source_operations(&self) -> &[word::OpId] {
        &self.source_operations
    }
}

impl DurableOperatorArena {
    pub(crate) fn capture(
        module: &word::WordModule,
        decisions: &ArchitectureDecisions,
        source_operations: &[Box<[word::OpId]>],
        mut operation_anchor: impl FnMut(word::OpId) -> Result<OperationAnchorId, crate::SynthError>,
    ) -> Result<Self, crate::SynthError> {
        let mut signatures = BTreeMap::new();
        let mut instances = Vec::with_capacity(decisions.operators().len());
        for (semantic, source_operations) in decisions.operators().iter().zip(source_operations) {
            let operands = decisions.operator_inputs(*semantic).collect::<Vec<_>>();
            let input_types = operands
                .iter()
                .map(|&value| {
                    module.value(value).map(|value| value.ty).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "durable operator references an unknown operand",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let result_type = module
                .value(semantic.result())
                .map(|value| value.ty)
                .ok_or_else(|| {
                    crate::SynthError::invariant("durable operator references an unknown result")
                })?;
            let shape = operator_shape(decisions, *semantic)?;
            let mut signature = OperatorSignature {
                id: OperatorSignatureId([0; 32]),
                kind: semantic.kind(),
                input_types: input_types.into_boxed_slice(),
                result_type,
                implementation_width: semantic.width(),
                shape,
            };
            signature.id = signature_id(&signature)?;
            match signatures.entry(signature.id) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(signature.clone());
                }
                std::collections::btree_map::Entry::Occupied(entry)
                    if entry.get() != &signature =>
                {
                    return Err(crate::SynthError::invariant(
                        "operator signature digest collision",
                    ));
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
            let primary_source = *source_operations.first().ok_or_else(|| {
                crate::SynthError::invariant("durable operator has no source occurrence")
            })?;
            instances.push(PreservedOperatorInstance {
                operator: semantic.id(),
                anchor: operation_anchor(primary_source)?,
                signature: signature.id,
                operands: operands.into_boxed_slice(),
                result: semantic.result(),
                source_operations: source_operations.clone(),
            });
        }
        let operator_by_local_operation = (0..module.operations().len())
            .map(|index| {
                let operation = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
                Ok(decisions
                    .operator_for_source_operation(operation)
                    .map_or(MISSING_OPERATOR, OperatorId::raw))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        Ok(Self {
            signatures: signatures.into_values().collect(),
            instances: instances.into_boxed_slice(),
            operator_by_local_operation: operator_by_local_operation.into_boxed_slice(),
        })
    }

    pub(crate) fn validate_decisions(
        &self,
        decisions: &ArchitectureDecisions,
        operation_count: usize,
    ) -> Result<(), crate::SynthError> {
        if self.instances.len() != decisions.operators().len() {
            return Err(crate::SynthError::invariant(
                "durable operator arena does not align with architecture decisions",
            ));
        }
        if self.operator_by_local_operation.len() != operation_count {
            return Err(crate::SynthError::invariant(
                "durable operator lookup does not cover its local module",
            ));
        }
        for (index, &operator) in self.operator_by_local_operation.iter().enumerate() {
            let operation = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
            if decisions
                .operator_for_source_operation(operation)
                .map_or(MISSING_OPERATOR, OperatorId::raw)
                != operator
            {
                return Err(crate::SynthError::invariant(
                    "durable operator lookup disagrees with architecture decisions",
                ));
            }
        }
        for (instance, semantic) in self.instances.iter().zip(decisions.operators()) {
            let signature = self.signature(instance.signature).ok_or_else(|| {
                crate::SynthError::invariant("durable operator has no semantic signature")
            })?;
            if instance.operator != semantic.id()
                || instance.result != semantic.result()
                || signature.kind != semantic.kind()
                || instance.operands.as_ref()
                    != decisions.operator_inputs(*semantic).collect::<Vec<_>>()
            {
                return Err(crate::SynthError::invariant(
                    "durable operator record disagrees with its selected architecture",
                ));
            }
        }
        Ok(())
    }

    /// Return signatures in canonical digest order.
    #[must_use]
    pub fn signatures(&self) -> &[OperatorSignature] {
        &self.signatures
    }

    /// Return occurrences in deterministic recognition order.
    #[must_use]
    pub fn instances(&self) -> &[PreservedOperatorInstance] {
        &self.instances
    }

    /// Look up one complete signature by content identity.
    #[must_use]
    pub fn signature(&self, id: OperatorSignatureId) -> Option<&OperatorSignature> {
        self.signatures
            .binary_search_by_key(&id, OperatorSignature::id)
            .ok()
            .map(|index| &self.signatures[index])
    }

    pub(crate) fn operator_for_local_operation(&self, operation: word::OpId) -> Option<OperatorId> {
        self.operator_by_local_operation
            .get(operation.index())
            .copied()
            .filter(|&operator| operator != MISSING_OPERATOR)
            .map(OperatorId::from_raw)
    }
}

fn operator_shape(
    decisions: &ArchitectureDecisions,
    semantic: crate::SemanticOperator,
) -> Result<OperatorShape, crate::SynthError> {
    Ok(match semantic.kind() {
        OperatorKind::Sum => OperatorShape::Sum(
            decisions
                .arithmetic_terms(semantic.id())
                .iter()
                .map(|term| match *term {
                    ArithmeticTerm::Value { ty, negative, .. } => {
                        OperatorTermShape::Value { ty, negative }
                    }
                    ArithmeticTerm::Product {
                        input_types,
                        ty,
                        negative,
                        constant_input,
                        ..
                    } => OperatorTermShape::Product {
                        input_types,
                        ty,
                        negative,
                        constant_input,
                    },
                })
                .collect(),
        ),
        OperatorKind::DynamicExtract => {
            let dynamic = semantic.dynamic_extract.ok_or_else(|| {
                crate::SynthError::invariant("dynamic-extract operator has no semantic shape")
            })?;
            OperatorShape::DynamicExtract(DynamicExtractShape {
                maximum_offset: dynamic.maximum_offset(),
                selection_max: dynamic.selection_max(),
                alignment: dynamic.alignment(),
                offset_width: dynamic.offset_width(),
                tap_count: dynamic.tap_count(),
            })
        }
        OperatorKind::Divide | OperatorKind::Modulo => OperatorShape::Division,
        OperatorKind::Add
        | OperatorKind::Subtract
        | OperatorKind::Increment
        | OperatorKind::Decrement
        | OperatorKind::Multiply => OperatorShape::Arithmetic,
    })
}

fn signature_id(signature: &OperatorSignature) -> Result<OperatorSignatureId, crate::SynthError> {
    let mut semantic = signature.clone();
    semantic.id = OperatorSignatureId([0; 32]);
    let encoded = opto_archive::to_bytes(&semantic)
        .map_err(|error| crate::SynthError::invariant(format!("operator signature: {error}")))?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"opto/operator-signature/v2\0");
    digest.update(&encoded);
    Ok(OperatorSignatureId(*digest.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_validation_recomputes_signature_identity_and_schema() {
        let ty = word::WordType::bits(8).unwrap();
        let mut signature = OperatorSignature {
            id: OperatorSignatureId([0; 32]),
            kind: OperatorKind::Multiply,
            input_types: Box::new([ty, ty]),
            result_type: ty,
            implementation_width: 8,
            shape: OperatorShape::Arithmetic,
        };
        signature.id = signature_id(&signature).unwrap();
        signature.validate_checkpoint().unwrap();

        signature.implementation_width = 0;
        assert!(signature.validate_checkpoint().is_err());
        signature.implementation_width = 8;
        signature.shape = OperatorShape::Division;
        signature.id = signature_id(&signature).unwrap();
        assert!(signature.validate_checkpoint().is_err());
    }
}
