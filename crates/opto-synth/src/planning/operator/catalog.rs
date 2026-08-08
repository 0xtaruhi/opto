// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::demand::ObservableBits;
use super::{OperatorId, OperatorKind, SemanticOperator};
use crate::planning::architecture::{ArithmeticTerm, DynamicExtractOperator};
use crate::planning::provider::{ImplementationProviderId, ProviderRecipeId, StructuralEstimate};
use opto_ir::word;
use serde::{Deserialize, Serialize};

const MISSING_OPERATOR_ID: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
/// Dense identifier for one implementation candidate in an operator catalog.
pub struct ImplementationCandidateId(u32);

impl ImplementationCandidateId {
    /// Return the zero-based candidate index.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }

    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One provider recipe eligible to implement a semantic operator.
pub struct ImplementationCandidate {
    id: ImplementationCandidateId,
    operator: OperatorId,
    provider: ImplementationProviderId,
    recipe: ProviderRecipeId,
}

impl ImplementationCandidate {
    /// Return this candidate's dense identifier.
    #[must_use]
    pub fn id(self) -> ImplementationCandidateId {
        self.id
    }

    /// Return the semantic operator this candidate implements.
    #[must_use]
    pub fn operator(self) -> OperatorId {
        self.operator
    }

    pub(crate) fn provider(self) -> ImplementationProviderId {
        self.provider
    }

    pub(crate) fn recipe(self) -> ProviderRecipeId {
        self.recipe
    }
}

#[derive(Debug)]
pub(super) struct OperatorCatalog {
    providers: Box<[&'static dyn crate::planning::provider::ImplementationProvider]>,
    operators: Box<[SemanticOperator]>,
    operator_by_source_operation: Box<[u32]>,
    candidates: opto_core::PackedRows<ImplementationCandidate>,
    source_operations: opto_core::PackedRows<word::OpId>,
    arithmetic_terms: opto_core::PackedRows<ArithmeticTerm>,
}

impl OperatorCatalog {
    pub(super) fn regional_shell(operation_count: usize) -> Self {
        Self {
            providers: Box::new([]),
            operators: Box::new([]),
            operator_by_source_operation: vec![MISSING_OPERATOR_ID; operation_count]
                .into_boxed_slice(),
            candidates: opto_core::PackedRows::try_from_rows(Vec::new()).unwrap(),
            source_operations: opto_core::PackedRows::try_from_rows(Vec::new()).unwrap(),
            arithmetic_terms: opto_core::PackedRows::try_from_rows(Vec::new()).unwrap(),
        }
    }

    pub(super) fn for_module(
        module: &word::WordModule,
        observable: &ObservableBits,
        fuse_arithmetic: bool,
        providers: Box<[&'static dyn crate::planning::provider::ImplementationProvider]>,
    ) -> Result<Self, crate::SynthError> {
        let mut operators = Vec::new();
        let mut source_operations = Vec::new();
        let mut arithmetic_terms = Vec::new();
        let mut operator_by_source_operation = vec![MISSING_OPERATOR_ID; module.operations().len()];
        let regions = if fuse_arithmetic {
            arithmetic_regions(module)?
        } else {
            AdditiveRegions::default()
        };
        let AdditiveRegions {
            mut by_root,
            absorbed,
        } = regions;
        let mut unsigned_values = word::UnsignedValueAnalysis::new(module);
        let mut known_bits = word::KnownBitsAnalysis::new(module);
        for (index, _) in module.operations().iter().enumerate() {
            let raw_id = u32::try_from(operators.len())
                .map_err(|_| crate::SynthError::capacity("operator ID exceeds 32-bit capacity"))?;
            if raw_id == MISSING_OPERATOR_ID {
                return Err(crate::SynthError::invariant("exhausted operator ID space"));
            }
            let id = OperatorId::from_raw(raw_id);
            if absorbed.contains(&index) {
                continue;
            }
            let region = by_root.remove(&index);
            let operator = if let Some(region) = region.as_ref() {
                semantic_sum_operator(module, observable, &mut known_bits, index, id, region)?
            } else {
                semantic_operator(
                    module,
                    observable,
                    &mut unsigned_values,
                    &mut known_bits,
                    index,
                    id,
                )?
            };
            let Some(operator) = operator else {
                continue;
            };
            let mut operator_sources = Vec::new();
            if let Some(region) = region.as_ref() {
                for &source_index in &region.operations {
                    let source_operation = word::OpId::from_index(source_index).map_err(|_| {
                        crate::SynthError::capacity(
                            "arithmetic-region operation ID exceeds 32-bit capacity",
                        )
                    })?;
                    operator_sources.push(source_operation);
                    operator_by_source_operation[source_index] = id.raw();
                }
            }
            if region.is_none() {
                operator_sources.push(operator.source_operation());
            }
            source_operations.push(operator_sources);
            arithmetic_terms.push(region.map(|region| region.terms).unwrap_or_default());
            operators.push(operator);
            operator_by_source_operation[index] = id.raw();
        }

        let mut candidates = Vec::with_capacity(operators.len());
        let mut candidate_count = 0usize;
        for operator in &operators {
            let mut operator_candidates = Vec::new();
            for (index, &provider) in providers.iter().enumerate() {
                let provider_id =
                    ImplementationProviderId::from_raw(u8::try_from(index).map_err(|_| {
                        crate::SynthError::capacity(
                            "implementation provider table exceeds 8-bit capacity",
                        )
                    })?);
                let provider_start = operator_candidates.len();
                let mut error = None;
                provider.enumerate_recipes(*operator, &mut |recipe| {
                    if error.is_some() {
                        return;
                    }
                    if provider.recipe_name(recipe).is_none() {
                        error = Some(crate::SynthError::invalid(format!(
                            "resource '{}' enumerated unknown recipe {}",
                            provider.resource_name(),
                            recipe.raw()
                        )));
                        return;
                    }
                    if operator_candidates[provider_start..]
                        .iter()
                        .any(|candidate: &ImplementationCandidate| candidate.recipe == recipe)
                    {
                        error = Some(crate::SynthError::invalid(format!(
                            "resource '{}' enumerated recipe {} more than once",
                            provider.resource_name(),
                            recipe.raw()
                        )));
                        return;
                    }
                    let Some(candidate_index) =
                        candidate_count.checked_add(operator_candidates.len())
                    else {
                        error = Some(crate::SynthError::capacity(
                            "implementation candidate table exceeds addressable capacity",
                        ));
                        return;
                    };
                    let Ok(raw_id) = u32::try_from(candidate_index) else {
                        error = Some(crate::SynthError::capacity(
                            "implementation candidate ID exceeds 32-bit capacity",
                        ));
                        return;
                    };
                    if raw_id == u32::MAX {
                        error = Some(crate::SynthError::capacity(
                            "exhausted implementation candidate ID space",
                        ));
                        return;
                    }
                    operator_candidates.push(ImplementationCandidate {
                        id: ImplementationCandidateId(raw_id),
                        operator: operator.id(),
                        provider: provider_id,
                        recipe,
                    });
                });
                if let Some(error) = error {
                    return Err(error);
                }
            }
            candidate_count = candidate_count
                .checked_add(operator_candidates.len())
                .ok_or_else(|| {
                    crate::SynthError::capacity(
                        "implementation candidate table exceeds addressable capacity",
                    )
                })?;
            candidates.push(operator_candidates);
        }

        let candidates = opto_core::PackedRows::try_from_rows(candidates).map_err(|_| {
            crate::SynthError::capacity(
                "implementation candidates exceed 32-bit packed-row capacity",
            )
        })?;
        let source_operations =
            opto_core::PackedRows::try_from_rows(source_operations).map_err(|_| {
                crate::SynthError::capacity("source operations exceed 32-bit packed-row capacity")
            })?;
        let arithmetic_terms =
            opto_core::PackedRows::try_from_rows(arithmetic_terms).map_err(|_| {
                crate::SynthError::capacity("arithmetic terms exceed 32-bit packed-row capacity")
            })?;
        Ok(Self {
            providers,
            operators: operators.into_boxed_slice(),
            operator_by_source_operation: operator_by_source_operation.into_boxed_slice(),
            candidates,
            source_operations,
            arithmetic_terms,
        })
    }

    pub(super) fn operators(&self) -> &[SemanticOperator] {
        &self.operators
    }

    pub(super) fn operator(&self, id: OperatorId) -> Option<SemanticOperator> {
        self.operators.get(id.raw() as usize).copied()
    }

    pub(super) fn source_operations(&self, id: OperatorId) -> &[word::OpId] {
        self.source_operations.get(id.raw() as usize).unwrap_or(&[])
    }

    pub(super) fn arithmetic_terms(&self, id: OperatorId) -> &[ArithmeticTerm] {
        self.arithmetic_terms.get(id.raw() as usize).unwrap_or(&[])
    }

    pub(super) fn candidates(&self, operator: OperatorId) -> &[ImplementationCandidate] {
        self.candidates.get(operator.raw() as usize).unwrap_or(&[])
    }

    pub(super) fn candidate(
        &self,
        id: ImplementationCandidateId,
    ) -> Option<ImplementationCandidate> {
        self.candidates.values().get(id.raw() as usize).copied()
    }

    pub(super) fn candidate_recipe_name(&self, id: ImplementationCandidateId) -> Option<&str> {
        let candidate = self.candidate(id)?;
        self.provider(candidate)?.recipe_name(candidate.recipe())
    }

    pub(super) fn candidate_implementation_name(
        &self,
        id: ImplementationCandidateId,
    ) -> Option<&str> {
        let candidate = self.candidate(id)?;
        self.provider(candidate)?
            .implementation_name(candidate.recipe())
    }

    pub(super) fn candidate_module_name(&self, id: ImplementationCandidateId) -> Option<&str> {
        let candidate = self.candidate(id)?;
        let operator = self.operator(candidate.operator())?;
        self.provider(candidate)?.module_name(operator)
    }

    pub(super) fn candidate_operation_mnemonic(
        &self,
        id: ImplementationCandidateId,
    ) -> Option<&str> {
        let candidate = self.candidate(id)?;
        let operator = self.operator(candidate.operator())?;
        self.provider(candidate)?.operation_mnemonic(operator)
    }

    pub(super) fn provider(
        &self,
        candidate: ImplementationCandidate,
    ) -> Option<&(dyn crate::planning::provider::ImplementationProvider + 'static)> {
        self.providers.get(candidate.provider().index()).copied()
    }

    pub(super) fn candidate_estimate(
        &self,
        candidate: ImplementationCandidate,
    ) -> Result<StructuralEstimate, crate::SynthError> {
        let operator = self.operator(candidate.operator()).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "candidate {} references unknown operator {}",
                candidate.id().raw(),
                candidate.operator().raw()
            ))
        })?;
        self.provider(candidate)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "candidate {} references an unknown implementation provider",
                    candidate.id().raw()
                ))
            })?
            .structural_estimate(candidate.recipe(), operator)
    }

    pub(super) fn operator_for_source_operation(
        &self,
        operation: word::OpId,
    ) -> Option<OperatorId> {
        self.operator_by_source_operation
            .get(operation.index())
            .copied()
            .filter(|&operator| operator != MISSING_OPERATOR_ID)
            .map(OperatorId::from_raw)
    }
}

struct AdditiveRegion {
    terms: Vec<ArithmeticTerm>,
    operations: Vec<usize>,
}

#[derive(Default)]
struct AdditiveRegions {
    by_root: hashbrown::HashMap<usize, AdditiveRegion>,
    absorbed: hashbrown::HashSet<usize>,
}

fn arithmetic_regions(module: &word::WordModule) -> Result<AdditiveRegions, crate::SynthError> {
    let uses = crate::word::uses::value_use_counts(module)?;
    additive_regions(module, &uses)
}

fn additive_regions(
    module: &word::WordModule,
    uses: &[u32],
) -> Result<AdditiveRegions, crate::SynthError> {
    let mut regions = AdditiveRegions::default();
    let mut consumer = vec![None; module.values().len()];
    for (parent, operation) in module.operations().iter().enumerate() {
        for input in crate::word::operation_inputs(&operation.kind) {
            if uses[input.index()] == 1 {
                consumer[input.index()] = Some(parent);
            }
        }
    }
    let mut absorb_into = vec![None; module.operations().len()];
    for (child, operation) in module.operations().iter().enumerate() {
        if !is_additive(&operation.kind) {
            continue;
        }
        let Some(parent) = consumer[operation.result.index()] else {
            continue;
        };
        let Some(parent_operation) = module.operations().get(parent) else {
            continue;
        };
        if !is_additive(&parent_operation.kind)
            || value_type(module, operation.result)?.width()
                != value_type(module, parent_operation.result)?.width()
        {
            continue;
        }
        absorb_into[child] = Some(parent);
    }

    for (root, operation) in module.operations().iter().enumerate() {
        if !is_additive(&operation.kind) || absorb_into[root].is_some() {
            continue;
        }
        let region = collect_additive_region(module, root, &absorb_into, uses)?;
        if region.terms.len() < 3 && !region.terms.iter().any(|term| term.is_product()) {
            continue;
        }
        regions.absorbed.extend(
            region
                .operations
                .iter()
                .copied()
                .filter(|&index| index != root),
        );
        regions.by_root.insert(root, region);
    }
    Ok(regions)
}

fn collect_additive_region(
    module: &word::WordModule,
    root: usize,
    absorb_into: &[Option<usize>],
    uses: &[u32],
) -> Result<AdditiveRegion, crate::SynthError> {
    let operation = module.operations().get(root).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown arithmetic-region root {root}"))
    })?;
    let mut pending = Vec::new();
    push_additive_operands(&mut pending, root, &operation.kind, false)?;
    let mut terms = Vec::new();
    let mut operations = vec![root];
    while let Some((value, negative, parent)) = pending.pop() {
        let child = module.value(value).and_then(|value| match value.kind {
            word::ValueKind::Operation(operation) => Some(operation.index()),
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
        });
        if let Some(child) = child
            && absorb_into.get(child).copied().flatten() == Some(parent)
        {
            let operation = module.operations().get(child).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "arithmetic region references unknown operation {child}"
                ))
            })?;
            operations.push(child);
            push_additive_operands(&mut pending, child, &operation.kind, negative)?;
            continue;
        }
        let parent_ty = module
            .operations()
            .get(parent)
            .map(|operation| value_type(module, operation.result))
            .transpose()?
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "arithmetic term references unknown parent operation {parent}"
                ))
            })?;
        let value_ty = value_type(module, value)?;
        let ty = word::WordType::new(
            value_ty.width(),
            value_ty.is_signed() && parent_ty.is_signed(),
            value_ty.state(),
        )
        .map_err(crate::SynthError::from)?;
        let product = child
            .filter(|_| uses[value.index()] == 1)
            .and_then(|index| {
                module
                    .operations()
                    .get(index)
                    .map(|operation| (index, operation))
            })
            .filter(|(_, operation)| {
                matches!(
                    operation.kind,
                    word::OpKind::Binary {
                        op: word::BinaryOp::Mul,
                        ..
                    }
                ) && value_ty.width() == parent_ty.width()
            });
        if let Some((index, operation)) = product {
            let word::OpKind::Binary { left, right, .. } = operation.kind else {
                unreachable!("filtered multiplication changed kind")
            };
            let (left, left_ty) = normalize_multiply_input(module, left)?;
            let (right, right_ty) = normalize_multiply_input(module, right)?;
            let inputs = [left, right];
            operations.push(index);
            terms.push(ArithmeticTerm::Product {
                inputs,
                input_types: [left_ty, right_ty],
                ty,
                negative,
                constant_input: inputs
                    .iter()
                    .position(|&input| is_defined_constant(module, input))
                    .and_then(|index| u8::try_from(index).ok()),
            });
        } else {
            terms.push(ArithmeticTerm::Value {
                value,
                ty,
                negative,
            });
        }
    }
    operations.sort_unstable();
    Ok(AdditiveRegion { terms, operations })
}

fn push_additive_operands(
    pending: &mut Vec<(word::ValueId, bool, usize)>,
    operation: usize,
    kind: &word::OpKind,
    negative: bool,
) -> Result<(), crate::SynthError> {
    let word::OpKind::Binary { op, left, right } = *kind else {
        return Err(crate::SynthError::invariant(
            "arithmetic region contains a non-binary operation",
        ));
    };
    let right_negative = match op {
        word::BinaryOp::Add => negative,
        word::BinaryOp::Sub => !negative,
        _ => {
            return Err(crate::SynthError::invariant(
                "arithmetic region contains a non-additive operation",
            ));
        }
    };
    pending.push((right, right_negative, operation));
    pending.push((left, negative, operation));
    Ok(())
}

fn is_additive(kind: &word::OpKind) -> bool {
    matches!(
        kind,
        word::OpKind::Binary {
            op: word::BinaryOp::Add | word::BinaryOp::Sub,
            ..
        }
    )
}

fn semantic_operator(
    module: &word::WordModule,
    observable: &ObservableBits,
    unsigned_values: &mut word::UnsignedValueAnalysis,
    known_bits: &mut word::KnownBitsAnalysis,
    index: usize,
    id: OperatorId,
) -> Result<Option<SemanticOperator>, crate::SynthError> {
    let operation = module.operations().get(index).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown source operation index {index}"))
    })?;
    if let word::OpKind::DynamicExtract {
        value,
        offset,
        width,
    } = operation.kind
    {
        let result = module.value(operation.result).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "dynamic extract operation {index} has unknown result {:?}",
                operation.result
            ))
        })?;
        let implementation_width =
            implementation_width(module, observable, known_bits, operation.result);
        if implementation_width == 0 {
            return Ok(None);
        }
        let value_ty = value_type(module, value)?;
        let offset_ty = value_type(module, offset)?;
        let available_offsets = value_ty.width().checked_sub(width.get()).ok_or_else(|| {
            crate::SynthError::invariant("dynamic extract width exceeds its input")
        })?;
        let maximum_offset = unsigned_values
            .range(module, offset)
            .map(word::UnsignedValueRange::maximum)
            .ok_or_else(|| {
                crate::SynthError::invariant("cannot prove dynamic extract offset bounds")
            })?;
        let alignment = unsigned_values.alignment(module, offset);
        let source_operation = word::OpId::from_index(index).map_err(|_| {
            crate::SynthError::capacity("semantic operation ID exceeds 32-bit capacity")
        })?;
        return Ok(Some(SemanticOperator {
            id,
            kind: OperatorKind::DynamicExtract,
            source_operation,
            inputs: [value, offset],
            input_types: [value_ty, offset_ty],
            result: operation.result,
            constant_input: None,
            term_count: 0,
            negative_term_count: 0,
            product_term_count: 0,
            variable_product_term_count: 0,
            semantic_width: result.ty.width(),
            implementation_width,
            signed: result.ty.is_signed(),
            dynamic_extract: Some(DynamicExtractOperator::new(
                maximum_offset,
                available_offsets,
                alignment,
                offset_ty.width(),
            )),
        }));
    }
    let word::OpKind::Binary { op, left, right } = operation.kind else {
        return Ok(None);
    };
    let mut inputs = [left, right];
    let kind = match op {
        word::BinaryOp::Add if is_constant_one(module, left) => {
            inputs = [right, left];
            OperatorKind::Increment
        }
        word::BinaryOp::Add if is_constant_one(module, right) => OperatorKind::Increment,
        word::BinaryOp::Sub if is_constant_one(module, right) => OperatorKind::Decrement,
        word::BinaryOp::Add => OperatorKind::Add,
        word::BinaryOp::Sub => OperatorKind::Subtract,
        word::BinaryOp::Mul => OperatorKind::Multiply,
        word::BinaryOp::Div => OperatorKind::Divide,
        word::BinaryOp::Mod => OperatorKind::Modulo,
        _ => return Ok(None),
    };
    let mut input_types = inputs.map(|input| value_type(module, input));
    if kind == OperatorKind::Multiply {
        let normalized = inputs.map(|input| normalize_multiply_input(module, input));
        let [left, right] = normalized;
        let (left, left_type) = left?;
        let (right, right_type) = right?;
        inputs = [left, right];
        input_types = [Ok(left_type), Ok(right_type)];
    }
    let source_operation = word::OpId::from_index(index).map_err(|_| {
        crate::SynthError::capacity("semantic operation ID exceeds 32-bit capacity")
    })?;
    let result = module.value(operation.result).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "semantic operation {source_operation:?} has unknown result {:?}",
            operation.result
        ))
    })?;
    let [left_type, right_type] = input_types;
    let implementation_width =
        implementation_width(module, observable, known_bits, operation.result);
    if implementation_width == 0 {
        return Ok(None);
    }
    Ok(Some(SemanticOperator {
        id,
        kind,
        source_operation,
        inputs,
        input_types: [left_type?, right_type?],
        result: operation.result,
        constant_input: inputs
            .iter()
            .position(|&input| is_defined_constant(module, input))
            .and_then(|index| u8::try_from(index).ok()),
        term_count: 0,
        negative_term_count: 0,
        product_term_count: 0,
        variable_product_term_count: 0,
        semantic_width: result.ty.width(),
        implementation_width,
        signed: result.ty.is_signed(),
        dynamic_extract: None,
    }))
}

fn semantic_sum_operator(
    module: &word::WordModule,
    observable: &ObservableBits,
    known_bits: &mut word::KnownBitsAnalysis,
    index: usize,
    id: OperatorId,
    region: &AdditiveRegion,
) -> Result<Option<SemanticOperator>, crate::SynthError> {
    let operation = module.operations().get(index).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown arithmetic-region root {index}"))
    })?;
    let result = module.value(operation.result).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "arithmetic-region root {index} has unknown result {:?}",
            operation.result
        ))
    })?;
    let implementation_width =
        implementation_width(module, observable, known_bits, operation.result);
    if implementation_width == 0 {
        return Ok(None);
    }
    let mut boundary_inputs = region
        .terms
        .iter()
        .copied()
        .flat_map(ArithmeticTerm::inputs);
    let Some(first) = boundary_inputs.next() else {
        return Err(crate::SynthError::invariant(
            "arithmetic region has no boundary inputs",
        ));
    };
    let second = boundary_inputs.next().unwrap_or(first);
    let term_count = u32::try_from(region.terms.len()).map_err(|_| {
        crate::SynthError::capacity("arithmetic term count exceeds 32-bit capacity")
    })?;
    let negative_term_count = u32::try_from(
        region
            .terms
            .iter()
            .filter(|term| term.is_negative())
            .count(),
    )
    .map_err(|_| {
        crate::SynthError::capacity("negative arithmetic term count exceeds 32-bit capacity")
    })?;
    let product_term_count = u32::try_from(
        region.terms.iter().filter(|term| term.is_product()).count(),
    )
    .map_err(|_| crate::SynthError::capacity("product term count exceeds 32-bit capacity"))?;
    let variable_product_term_count = u32::try_from(
        region
            .terms
            .iter()
            .filter(|term| term.has_variable_product())
            .count(),
    )
    .map_err(|_| {
        crate::SynthError::capacity("variable product term count exceeds 32-bit capacity")
    })?;
    let source_operation = word::OpId::from_index(index).map_err(|_| {
        crate::SynthError::capacity("semantic operation ID exceeds 32-bit capacity")
    })?;
    Ok(Some(SemanticOperator {
        id,
        kind: OperatorKind::Sum,
        source_operation,
        inputs: [first, second],
        input_types: [value_type(module, first)?, value_type(module, second)?],
        result: operation.result,
        constant_input: None,
        term_count,
        negative_term_count,
        product_term_count,
        variable_product_term_count,
        semantic_width: result.ty.width(),
        implementation_width,
        signed: result.ty.is_signed(),
        dynamic_extract: None,
    }))
}

fn implementation_width(
    module: &word::WordModule,
    observable: &ObservableBits,
    known_bits: &mut word::KnownBitsAnalysis,
    result: word::ValueId,
) -> u32 {
    let demand = observable.required_prefix(result);
    known_bits.active_width(module, result, demand)
}

fn normalize_multiply_input(
    module: &word::WordModule,
    input: word::ValueId,
) -> Result<(word::ValueId, word::WordType), crate::SynthError> {
    let ty = value_type(module, input)?;
    let word::ValueKind::Operation(operation) = module
        .value(input)
        .ok_or_else(|| {
            crate::SynthError::invariant(format!("multiply has unknown input {input:?}"))
        })?
        .kind
    else {
        return Ok((input, ty));
    };
    let operation = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "multiply input references unknown operation {operation:?}"
        ))
    })?;
    let word::OpKind::Cast {
        kind,
        value,
        target,
    } = operation.kind
    else {
        return Ok((input, ty));
    };
    let source_ty = value_type(module, value)?;
    if target.width() <= source_ty.width() {
        return Ok((input, ty));
    }
    let signed = match kind {
        word::CastKind::SignExtend => true,
        word::CastKind::ZeroExtend => false,
        word::CastKind::Truncate => return Ok((input, ty)),
    };
    let normalized = word::WordType::new(source_ty.width(), signed, source_ty.state())
        .map_err(crate::SynthError::from)?;
    Ok((value, normalized))
}

fn value_type(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<word::WordType, crate::SynthError> {
    module.value(value).map(|value| value.ty).ok_or_else(|| {
        crate::SynthError::invariant(format!("semantic operation has unknown input {value:?}"))
    })
}

fn is_constant_one(module: &word::WordModule, value: word::ValueId) -> bool {
    let Some(word::Value {
        kind: word::ValueKind::Constant(bits),
        ..
    }) = module.value(value)
    else {
        return false;
    };
    bits.bit_lsb(0) == Some(opto_ir::BitVal::One)
        && (1..bits.width()).all(|index| bits.bit_lsb(index) == Some(opto_ir::BitVal::Zero))
}

fn is_defined_constant(module: &word::WordModule, value: word::ValueId) -> bool {
    let Some(value) = module.value(value) else {
        return false;
    };
    match value.kind {
        word::ValueKind::Constant(ref bits) => bits
            .as_slice()
            .iter()
            .all(|bit| matches!(bit, opto_ir::BitVal::Zero | opto_ir::BitVal::One)),
        word::ValueKind::Operation(operation) => module.operation(operation).is_some_and(|op| {
            matches!(op.kind, word::OpKind::Cast { value, .. } if is_defined_constant(module, value))
        }),
        word::ValueKind::Signal(_) => false,
    }
}
