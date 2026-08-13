// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod arithmetic;
mod backend;
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

use backend::{AxmBackend, BitBackend, ScalarBit, WordBackend};

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
    pub(crate) fn new(value_count: usize) -> Self {
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
        let signal_drivers = crate::word::signal_driver::SignalDriverIndex::new(module)?;
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
            for (index, value) in module.values().iter().enumerate() {
                let value_id = word::ValueId::from_index(index).map_err(crate::SynthError::from)?;
                if self.owner(value_id).is_some() {
                    continue;
                }
                let word::ValueKind::Signal(reference) = value.kind else {
                    continue;
                };
                let Some(drivers) = signal_drivers.resolve_reference(reference) else {
                    continue;
                };
                let mut owners = drivers.into_iter().map(|(driver, _)| self.owner(driver));
                let Some(Some(owner)) = owners.next() else {
                    continue;
                };
                if owners.all(|candidate| candidate == Some(owner)) {
                    inferred.push((value_id, owner));
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
        arena: &[ScalarBit],
        backend: &impl BitBackend,
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
            self.lowered_values[index] = Some(
                bits.iter()
                    .map(|&bit| {
                        backend.word_value(bit).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "non-Word backend cannot publish scalar Word ownership",
                            )
                        })
                    })
                    .collect::<Result<Box<[_]>, _>>()?,
            );
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
    let frozen_semantics = freeze_regional_semantics(module, operation_regions, scope)?;
    let mut blaster = BitBlaster::<WordBackend>::new(
        module,
        BitBlasterRequest {
            plan,
            operator_lookup: OperatorLookup::Decisions,
            provenance,
            operation_regions,
            boundary_inputs: &[],
            source_operations: None,
            source_values: None,
            global_scope: scope,
            frozen_semantics,
        },
    )?;
    for connect in connects {
        blaster.lower_connect(&connect)?;
    }
    for (instance_index, port, value, source) in instance_connections {
        blaster.lower_instance_connection(instance_index, port, value, source)?;
    }
    for &value in required_values {
        blaster.value(value)?;
    }
    blaster.lowered_owners.capture_lowered_values(
        &blaster.cache,
        &blaster.arena,
        &blaster.backend,
    )?;
    Ok(blaster.lowered_owners)
}

pub(crate) struct LocalRegionBooleanLowering {
    pub(crate) ownership: LoweredRegionOwnership,
    pub(crate) subject: crate::boolean::logic::CanonicalRegionLogic,
}

pub(crate) struct LocalRegionBooleanRequest<'a> {
    pub(crate) plan: &'a ArchitectureDecisions,
    pub(crate) operators: &'a crate::DurableOperatorArena,
    pub(crate) provenance: &'a mut ProvenanceBuilder,
    pub(crate) owner: crate::RegionRowId,
    pub(crate) boundary_inputs: &'a [word::ValueId],
    pub(crate) roots: &'a [word::ValueId],
    pub(crate) binding_values: &'a [word::ValueId],
}

pub(crate) fn lower_local_region_boolean(
    module: &mut word::WordModule,
    request: LocalRegionBooleanRequest<'_>,
) -> Result<LocalRegionBooleanLowering, crate::SynthError> {
    let LocalRegionBooleanRequest {
        plan,
        operators,
        provenance,
        owner,
        boundary_inputs,
        roots,
        binding_values,
    } = request;
    operators.validate_decisions(plan, module.operations().len())?;
    let operation_regions = vec![Some(owner); module.operations().len()];
    let mut blaster = BitBlaster::<AxmBackend>::new(
        module,
        BitBlasterRequest {
            plan,
            operator_lookup: OperatorLookup::Durable(operators),
            provenance,
            operation_regions: &operation_regions,
            boundary_inputs,
            source_operations: None,
            source_values: None,
            global_scope: GlobalBitblastScope::Complete,
            frozen_semantics: FrozenSubstrateSemantics::default(),
        },
    )?;
    for &root in roots {
        blaster.value(root)?;
    }
    for &value in binding_values {
        blaster.value(value)?;
    }
    let mut bindings = roots
        .iter()
        .chain(binding_values)
        .copied()
        .collect::<Vec<_>>();
    bindings.sort_unstable();
    bindings.dedup();
    let mut value_nodes = Vec::new();
    for original in bindings {
        let span = blaster
            .cache
            .get(original.index())
            .copied()
            .flatten()
            .ok_or_else(|| crate::SynthError::invariant("regional AXM binding was not lowered"))?;
        let mut lowered = Vec::with_capacity(span.len() as usize);
        for bit_index in 0..span.len() {
            let bit = blaster.bit(span, bit_index);
            let ScalarBit::Logic(node) = bit else {
                return Err(crate::SynthError::invariant(
                    "regional AXM binding contains a scalar Word value",
                ));
            };
            let handle = if node.index() == 0 {
                blaster.binding_constant(original, node.is_inverted())?
            } else {
                match blaster.backend.binding_value(bit) {
                    Some(value)
                        if binding_represents_original_bit(
                            blaster.module,
                            original,
                            bit_index,
                            value,
                        ) =>
                    {
                        value
                    }
                    Some(_) | None => blaster.binding_projection(original, bit_index)?,
                }
            };
            blaster.lowered_owners.set(handle, owner)?;
            lowered.push(handle);
            value_nodes.push((handle, node));
        }
        if blaster.lowered_owners.lowered_values.len() <= original.index() {
            blaster
                .lowered_owners
                .lowered_values
                .resize(original.index() + 1, None);
        }
        blaster.lowered_owners.lowered_values[original.index()] = Some(lowered.into_boxed_slice());
    }
    value_nodes.sort_by_key(|&(value, _)| value);
    value_nodes.dedup_by_key(|(value, _)| *value);
    let (network, inputs) = std::mem::take(&mut blaster.backend).finish();
    Ok(LocalRegionBooleanLowering {
        ownership: blaster.lowered_owners,
        subject: crate::boolean::logic::CanonicalRegionLogic {
            network,
            value_nodes: value_nodes.into_boxed_slice(),
            inputs,
        },
    })
}

fn binding_represents_original_bit(
    module: &word::WordModule,
    original: word::ValueId,
    bit: u32,
    representative: word::ValueId,
) -> bool {
    let Some(original_value) = module.value(original) else {
        return false;
    };
    if original_value.ty.width() == 1 {
        return bit == 0 && representative == original;
    }
    let Some(representative_value) = module.value(representative) else {
        return false;
    };
    match (&original_value.kind, &representative_value.kind) {
        (word::ValueKind::Signal(original), word::ValueKind::Signal(representative)) => {
            representative.width() == 1
                && representative.signal == original.signal
                && original.lsb.checked_add(bit) == Some(representative.lsb)
        }
        (_, word::ValueKind::Operation(operation)) => {
            module.operation(*operation).is_some_and(|operation| {
                matches!(
                    operation.kind,
                    word::OpKind::Extract {
                        value,
                        lsb,
                        width,
                    } if value == original && lsb == bit && width.get() == 1
                )
            })
        }
        (word::ValueKind::Constant(_) | word::ValueKind::Operation(_), _)
        | (word::ValueKind::Signal(_), word::ValueKind::Constant(_)) => false,
    }
}

type FrozenBitConstants = BTreeMap<word::ValueId, Box<[Option<BitVal>]>>;

#[derive(Default)]
struct FrozenSubstrateSemantics {
    aliases: BTreeMap<word::ValueId, word::ValueId>,
    constants: FrozenBitConstants,
}

fn freeze_regional_semantics(
    module: &word::WordModule,
    operation_regions: &[Option<crate::RegionRowId>],
    scope: GlobalBitblastScope,
) -> Result<FrozenSubstrateSemantics, crate::SynthError> {
    if scope != GlobalBitblastScope::RegionalShell {
        return Ok(FrozenSubstrateSemantics::default());
    }
    let semantics = crate::mapping::FullDomainRootSemantics::new(module)?;
    let mut facts = word::KnownBitsAnalysis::new(module);
    let mut aliases = BTreeMap::new();
    let mut constants = BTreeMap::new();
    for (index, operation) in module.operations().iter().enumerate() {
        if operation_regions.get(index).copied().flatten().is_none()
            || matches!(
                operation.kind,
                word::OpKind::Concat { .. }
                    | word::OpKind::Extract { .. }
                    | word::OpKind::Cast { .. }
                    | word::OpKind::Register(_)
                    | word::OpKind::Latch(_)
            )
        {
            continue;
        }
        let canonical = semantics.canonical_root(operation.result)?;
        if canonical != operation.result {
            aliases.insert(operation.result, canonical);
            continue;
        }
        let width = module
            .value(operation.result)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional constant proof references an unknown operation result",
                )
            })?
            .ty
            .width();
        let bits = (0..width)
            .map(|bit| match facts.bit(module, operation.result, bit) {
                word::KnownBit::Zero => Some(BitVal::Zero),
                word::KnownBit::One => Some(BitVal::One),
                word::KnownBit::Unknown => None,
            })
            .collect::<Box<[_]>>();
        if bits.iter().any(Option::is_some) {
            constants.insert(operation.result, bits);
        }
    }
    Ok(FrozenSubstrateSemantics { aliases, constants })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BitSpan {
    start: u32,
    len: NonZeroU32,
}

type BitColumn = SmallVec<[ScalarBit; 4]>;
type BitColumns = Vec<BitColumn>;

impl BitSpan {
    fn len(self) -> u32 {
        self.len.get()
    }
}

pub(super) struct BitBlaster<'a, B: BitBackend = WordBackend> {
    module: &'a mut word::WordModule,
    plan: &'a ArchitectureDecisions,
    operator_lookup: OperatorLookup<'a>,
    provenance: &'a mut ProvenanceBuilder,
    active_operator: Option<OperatorId>,
    active_region: Option<crate::RegionRowId>,
    operation_regions: &'a [Option<crate::RegionRowId>],
    boundary_inputs: BTreeSet<word::ValueId>,
    signal_drivers: crate::word::signal_driver::SignalDriverIndex,
    active_values: BTreeSet<word::ValueId>,
    lowered_owners: LoweredRegionOwnership,
    arena: Vec<ScalarBit>,
    cache: Vec<Option<BitSpan>>,
    constants: [Option<ScalarBit>; 8],
    source_operations: Option<&'a [Option<word::OpId>]>,
    source_values: Option<&'a BTreeMap<word::ValueId, word::ValueId>>,
    global_scope: GlobalBitblastScope,
    frozen_semantics: FrozenSubstrateSemantics,
    backend: B,
}

struct BitBlasterRequest<'a> {
    plan: &'a ArchitectureDecisions,
    operator_lookup: OperatorLookup<'a>,
    provenance: &'a mut ProvenanceBuilder,
    operation_regions: &'a [Option<crate::RegionRowId>],
    boundary_inputs: &'a [word::ValueId],
    source_operations: Option<&'a [Option<word::OpId>]>,
    source_values: Option<&'a BTreeMap<word::ValueId, word::ValueId>>,
    global_scope: GlobalBitblastScope,
    frozen_semantics: FrozenSubstrateSemantics,
}

#[derive(Clone, Copy)]
enum OperatorLookup<'a> {
    Decisions,
    Durable(&'a crate::DurableOperatorArena),
}

impl<'a, B: BitBackend> BitBlaster<'a, B> {
    fn new(
        module: &'a mut word::WordModule,
        request: BitBlasterRequest<'a>,
    ) -> Result<Self, crate::SynthError> {
        let BitBlasterRequest {
            plan,
            operator_lookup,
            provenance,
            operation_regions,
            boundary_inputs,
            source_operations,
            source_values,
            global_scope,
            frozen_semantics,
        } = request;
        let value_count = module.values().len();
        let signal_drivers = crate::word::signal_driver::SignalDriverIndex::new(module)?;
        Ok(Self {
            module,
            plan,
            operator_lookup,
            provenance,
            active_operator: None,
            active_region: None,
            operation_regions,
            boundary_inputs: boundary_inputs.iter().copied().collect(),
            signal_drivers,
            active_values: BTreeSet::new(),
            lowered_owners: LoweredRegionOwnership::new(value_count),
            arena: Vec::new(),
            cache: Vec::new(),
            constants: [None; 8],
            source_operations,
            source_values,
            global_scope,
            frozen_semantics,
            backend: B::default(),
        })
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

fn lower_implementation<B: BitBackend>(
    provider: ImplementationProviderId,
    recipe: ProviderRecipeId,
    blaster: &mut BitBlaster<'_, B>,
    request: ImplementationRequest<'_>,
) -> Result<Vec<ScalarBit>, crate::SynthError> {
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
