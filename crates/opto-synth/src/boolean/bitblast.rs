// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod arithmetic;
mod compressor;
mod division;
mod dynamic;
mod io;
mod multiplier;
mod operations;
mod sequential;
mod support;

use crate::OperatorId;
use crate::artifact::provenance::ProvenanceBuilder;
use crate::planning::operator::ArchitectureDecisions;
use crate::planning::provider::{
    ImplementationProvider, ImplementationProviderId, ProviderRecipeId,
};
use opto_ir::word;
use opto_ir::{BitVal, ConstBits};
use smallvec::SmallVec;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::num::NonZeroU32;

pub(crate) fn validate_synthesizable_constants(
    module: &word::WordModule,
) -> Result<(), crate::SynthError> {
    for value in module.values() {
        let word::ValueKind::Constant(bits) = &value.kind else {
            continue;
        };
        if bits.as_slice().contains(&BitVal::Z) {
            return Err(crate::SynthError::invalid(format!(
                "tri-state constant in design '{}' at {:?} is not supported",
                module.name(),
                value.source
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn bitblast_module_with_plan(
    module: &mut word::WordModule,
    plan: &ArchitectureDecisions,
    provenance: &mut ProvenanceBuilder,
) -> Result<(), crate::SynthError> {
    bitblast_module_with_regions(
        module,
        plan,
        provenance,
        &[],
        &[],
        GlobalBitblastScope::Complete,
    )
    .map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalBitblastScope {
    Complete,
    RegionalShell,
}

#[derive(Debug)]
pub(crate) struct LoweredRegionOwnership {
    owners: Vec<Option<crate::RegionRowId>>,
    lowered_values: Vec<Option<Box<[word::ValueId]>>>,
}

impl LoweredRegionOwnership {
    fn new(value_count: usize) -> Self {
        Self {
            owners: vec![None; value_count],
            lowered_values: vec![None; value_count],
        }
    }

    fn set(
        &mut self,
        value: word::ValueId,
        owner: crate::RegionRowId,
    ) -> Result<(), crate::SynthError> {
        if self.owners.len() <= value.index() {
            self.owners.resize(value.index() + 1, None);
        }
        let slot = &mut self.owners[value.index()];
        if slot.is_some_and(|current| current != owner) {
            return Err(crate::SynthError::invariant(
                "lowered value crosses synthesis-region ownership",
            ));
        }
        *slot = Some(owner);
        Ok(())
    }

    fn set_batch(
        &mut self,
        values: &[word::ValueId],
        owner: crate::RegionRowId,
    ) -> Result<(), crate::SynthError> {
        if let Some(last) = values.last() {
            let required = last
                .index()
                .checked_add(1)
                .ok_or_else(|| crate::SynthError::capacity("lowered ownership value batch"))?;
            if self.owners.len() < required {
                self.owners.resize(required, None);
            }
        }
        for &value in values {
            let slot = &mut self.owners[value.index()];
            if slot.is_some_and(|current| current != owner) {
                return Err(crate::SynthError::invariant(
                    "lowered value batch crosses synthesis-region ownership",
                ));
            }
            *slot = Some(owner);
        }
        Ok(())
    }

    fn claim(&mut self, value: word::ValueId, owner: crate::RegionRowId) {
        if self.owners.len() <= value.index() {
            self.owners.resize(value.index() + 1, None);
        }
        self.owners[value.index()].get_or_insert(owner);
    }

    pub(crate) fn owner(&self, value: word::ValueId) -> Option<crate::RegionRowId> {
        self.owners.get(value.index()).copied().flatten()
    }

    pub(crate) fn lowered_bits(&self, value: word::ValueId) -> Option<&[word::ValueId]> {
        self.lowered_values
            .get(value.index())
            .and_then(Option::as_deref)
    }

    pub(crate) fn infer_unowned(
        &mut self,
        module: &word::WordModule,
    ) -> Result<(), crate::SynthError> {
        self.owners.resize(module.values().len(), None);
        let mut consumers = vec![Vec::new(); module.values().len()];
        for operation in module.operations() {
            for input in crate::word::operation_inputs(&operation.kind) {
                consumers[input.index()].push(operation.result);
            }
        }
        loop {
            let mut inferred = Vec::new();
            for operation in module.operations() {
                if self.owner(operation.result).is_some() {
                    continue;
                }
                let mut adjacent = crate::word::operation_inputs(&operation.kind)
                    .into_iter()
                    .filter_map(|input| self.owner(input))
                    .chain(
                        consumers[operation.result.index()]
                            .iter()
                            .filter_map(|&consumer| self.owner(consumer)),
                    );
                let Some(owner) = adjacent.next() else {
                    continue;
                };
                if adjacent.all(|candidate| candidate == owner) {
                    inferred.push((operation.result, owner));
                }
            }
            if inferred.is_empty() {
                break;
            }
            for (value, owner) in inferred {
                self.set(value, owner)?;
            }
        }
        Ok(())
    }

    fn capture_lowered_values(
        &mut self,
        cache: &[Option<BitSpan>],
        arena: &[word::ValueId],
    ) -> Result<(), crate::SynthError> {
        self.lowered_values.resize(cache.len(), None);
        for (index, span) in cache.iter().copied().enumerate() {
            let Some(span) = span else { continue };
            let start = span.start as usize;
            let end = start
                .checked_add(span.len() as usize)
                .ok_or_else(|| crate::SynthError::invariant("lowered bit span overflows"))?;
            let bits = arena.get(start..end).ok_or_else(|| {
                crate::SynthError::invariant("lowered bit span is outside its local arena")
            })?;
            self.lowered_values[index] = Some(bits.into());
        }
        Ok(())
    }
}

pub(crate) fn bitblast_module_with_regions(
    module: &mut word::WordModule,
    plan: &ArchitectureDecisions,
    provenance: &mut ProvenanceBuilder,
    operation_regions: &[Option<crate::RegionRowId>],
    required_values: &[word::ValueId],
    scope: GlobalBitblastScope,
) -> Result<LoweredRegionOwnership, crate::SynthError> {
    if !module.memories().is_empty()
        || !module.memory_read_ports().is_empty()
        || !module.memory_write_ports().is_empty()
    {
        return Err(crate::SynthError::invariant(
            "logic lowering received unmaterialized memory resources",
        ));
    }
    let connects = module.take_connects();
    let instance_connections = crate::word::instances::snapshot(module);
    if !operation_regions.is_empty() && operation_regions.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "source operation ownership does not cover the lowering module",
        ));
    }
    let mut blaster = BitBlaster::new(
        module,
        BitBlasterRequest {
            plan,
            operator_lookup: OperatorLookup::Decisions,
            provenance,
            operation_regions,
            boundary_inputs: &[],
            source_operations: None,
            source_values: None,
            runtime: None,
            global_scope: scope,
        },
    );
    for connect in connects {
        blaster.lower_connect(&connect)?;
    }
    for (instance_index, port, value, source) in instance_connections {
        blaster.lower_instance_connection(instance_index, port, value, source)?;
    }
    for &value in required_values {
        blaster.value(value)?;
    }
    blaster
        .lowered_owners
        .capture_lowered_values(&blaster.cache, &blaster.arena)?;
    Ok(blaster.lowered_owners)
}

pub(crate) struct LocalRegionBitblastRequest<'a> {
    pub(crate) plan: &'a ArchitectureDecisions,
    pub(crate) operators: &'a crate::DurableOperatorArena,
    pub(crate) provenance: &'a mut ProvenanceBuilder,
    pub(crate) owner: crate::RegionRowId,
    pub(crate) boundary_inputs: &'a [word::ValueId],
    pub(crate) roots: &'a [word::ValueId],
    pub(crate) runtime: &'a opto_runtime::ExecutionContext,
}

pub(crate) fn bitblast_local_region_values(
    module: &mut word::WordModule,
    request: LocalRegionBitblastRequest<'_>,
) -> Result<LoweredRegionOwnership, crate::SynthError> {
    let LocalRegionBitblastRequest {
        plan,
        operators,
        provenance,
        owner,
        boundary_inputs,
        roots,
        runtime,
    } = request;
    operators.validate_decisions(plan, module.operations().len())?;
    let connects = module.take_connects();
    let instance_connections = crate::word::instances::snapshot(module);
    let operation_regions = vec![Some(owner); module.operations().len()];
    let mut blaster = BitBlaster::new(
        module,
        BitBlasterRequest {
            plan,
            operator_lookup: OperatorLookup::Durable(operators),
            provenance,
            operation_regions: &operation_regions,
            boundary_inputs,
            source_operations: None,
            source_values: None,
            runtime: Some(runtime),
            global_scope: GlobalBitblastScope::Complete,
        },
    );
    for connect in connects {
        blaster.lower_connect(&connect)?;
    }
    for (instance_index, port, value, source) in instance_connections {
        blaster.lower_instance_connection(instance_index, port, value, source)?;
    }
    for &root in roots {
        blaster.value(root)?;
    }
    blaster
        .lowered_owners
        .capture_lowered_values(&blaster.cache, &blaster.arena)?;
    Ok(blaster.lowered_owners)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BitSpan {
    start: u32,
    len: NonZeroU32,
}

type BitColumn = SmallVec<[word::ValueId; 4]>;
type BitColumns = Vec<BitColumn>;

impl BitSpan {
    fn len(self) -> u32 {
        self.len.get()
    }
}

pub(crate) struct BitBlaster<'a> {
    module: &'a mut word::WordModule,
    plan: &'a ArchitectureDecisions,
    operator_lookup: OperatorLookup<'a>,
    provenance: &'a mut ProvenanceBuilder,
    active_operator: Option<OperatorId>,
    active_region: Option<crate::RegionRowId>,
    operation_regions: &'a [Option<crate::RegionRowId>],
    boundary_inputs: BTreeSet<word::ValueId>,
    lowered_owners: LoweredRegionOwnership,
    arena: Vec<word::ValueId>,
    cache: Vec<Option<BitSpan>>,
    constants: [Option<word::ValueId>; 8],
    source_operations: Option<&'a [Option<word::OpId>]>,
    source_values: Option<&'a BTreeMap<word::ValueId, word::ValueId>>,
    runtime: Option<&'a opto_runtime::ExecutionContext>,
    global_scope: GlobalBitblastScope,
}

struct BitBlasterRequest<'a> {
    plan: &'a ArchitectureDecisions,
    operator_lookup: OperatorLookup<'a>,
    provenance: &'a mut ProvenanceBuilder,
    operation_regions: &'a [Option<crate::RegionRowId>],
    boundary_inputs: &'a [word::ValueId],
    source_operations: Option<&'a [Option<word::OpId>]>,
    source_values: Option<&'a BTreeMap<word::ValueId, word::ValueId>>,
    runtime: Option<&'a opto_runtime::ExecutionContext>,
    global_scope: GlobalBitblastScope,
}

#[derive(Clone, Copy)]
enum OperatorLookup<'a> {
    Decisions,
    Durable(&'a crate::DurableOperatorArena),
}

impl<'a> BitBlaster<'a> {
    fn new(module: &'a mut word::WordModule, request: BitBlasterRequest<'a>) -> Self {
        let BitBlasterRequest {
            plan,
            operator_lookup,
            provenance,
            operation_regions,
            boundary_inputs,
            source_operations,
            source_values,
            runtime,
            global_scope,
        } = request;
        let value_count = module.values().len();
        Self {
            module,
            plan,
            operator_lookup,
            provenance,
            active_operator: None,
            active_region: None,
            operation_regions,
            boundary_inputs: boundary_inputs.iter().copied().collect(),
            lowered_owners: LoweredRegionOwnership::new(value_count),
            arena: Vec::new(),
            cache: Vec::new(),
            constants: [None; 8],
            source_operations,
            source_values,
            runtime,
            global_scope,
        }
    }

    pub(super) fn source_operation(
        &self,
        local: word::OpId,
    ) -> Result<Option<word::OpId>, crate::SynthError> {
        match self.source_operations {
            Some(sources) => sources.get(local.index()).copied().ok_or_else(|| {
                crate::SynthError::invariant(
                    "region-local operation has no immutable source-operation identity",
                )
            }),
            None => Ok(Some(local)),
        }
    }

    pub(super) fn operator_for_source_operation(
        &self,
        operation: word::OpId,
    ) -> Option<OperatorId> {
        match self.operator_lookup {
            OperatorLookup::Decisions => self.plan.operator_for_source_operation(operation),
            OperatorLookup::Durable(operators) => operators.operator_for_local_operation(operation),
        }
    }

    pub(super) fn local_source_value(
        &self,
        source: word::ValueId,
    ) -> Result<word::ValueId, crate::SynthError> {
        match self.source_values {
            Some(values) => values.get(&source).copied().ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "regional provider input {source:?} is absent from its local dependency cone"
                ))
            }),
            None => Ok(source),
        }
    }

    pub(super) fn local_semantic_operator(
        &self,
        mut operator: crate::SemanticOperator,
        local_operation: word::OpId,
    ) -> Result<crate::SemanticOperator, crate::SynthError> {
        operator.source_operation = local_operation;
        operator.inputs = operator
            .inputs
            .map(|value| self.local_source_value(value))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .expect("semantic operator has exactly two inputs");
        operator.result = self.local_source_value(operator.result)?;
        Ok(operator)
    }
}

pub(crate) fn implementation_providers() -> [&'static dyn ImplementationProvider; 4] {
    [
        arithmetic::implementation_provider(),
        dynamic::implementation_provider(),
        multiplier::implementation_provider(),
        division::implementation_provider(),
    ]
}

fn constant_index(bit: BitVal, state: word::LogicStateKind) -> usize {
    let bit = match bit {
        BitVal::Zero => 0,
        BitVal::One => 1,
        BitVal::X => 2,
        BitVal::Z => 3,
    };
    state as usize * 4 + bit
}

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub(super) struct ImplementationRequest<'a> {
    pub(super) operator: crate::SemanticOperator,
    pub(super) result_type: word::WordType,
    pub(super) source: &'a word::SourceSpan,
}

fn lower_implementation(
    provider: ImplementationProviderId,
    recipe: ProviderRecipeId,
    blaster: &mut BitBlaster<'_>,
    request: ImplementationRequest<'_>,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    match provider.index() {
        0 => arithmetic::lower_implementation(recipe, blaster, request),
        1 => dynamic::lower_implementation(recipe, blaster, request),
        2 => multiplier::lower_implementation(recipe, blaster, request),
        3 => division::lower_implementation(recipe, blaster, request),
        _ => Err(crate::SynthError::invariant(
            "implementation candidate references an unknown provider",
        )),
    }
}
