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
pub(crate) struct LoweredRegionBinding {
    regions: Vec<Option<crate::RegionRowId>>,
    lowered_values: Vec<Option<Box<[word::ValueId]>>>,
}

impl LoweredRegionBinding {
    pub(crate) fn new(value_count: usize) -> Self {
        Self {
            regions: vec![None; value_count],
            lowered_values: vec![None; value_count],
        }
    }

    fn bind(
        &mut self,
        value: word::ValueId,
        region: crate::RegionRowId,
    ) -> Result<(), crate::SynthError> {
        if self.regions.len() <= value.index() {
            self.regions.resize(value.index() + 1, None);
        }
        let slot = &mut self.regions[value.index()];
        if slot.is_some_and(|current| current != region) {
            return Err(crate::SynthError::invariant(
                "lowered value is bound to conflicting synthesis regions",
            ));
        }
        *slot = Some(region);
        Ok(())
    }

    pub(crate) fn lowered_bits(&self, value: word::ValueId) -> Option<&[word::ValueId]> {
        self.lowered_values
            .get(value.index())
            .and_then(Option::as_deref)
    }

    #[cfg(test)]
    pub(crate) fn bind_identity_for_test(&mut self, value: word::ValueId) {
        self.lowered_values[value.index()] = Some(Box::new([value]));
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
                                "non-Word backend cannot publish a scalar Word binding",
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
    regional_publication: &[RegionalPublicationBit],
    scope: GlobalBitblastScope,
) -> Result<LoweredRegionBinding, crate::SynthError> {
    if !module.memories().is_empty()
        || !module.memory_read_ports().is_empty()
        || !module.memory_write_ports().is_empty()
    {
        return Err(crate::SynthError::invariant(
            "logic lowering received unmaterialized memory resources",
        ));
    }
    if !operation_regions.is_empty() && operation_regions.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "source operation region binding does not cover the lowering module",
        ));
    }
    let publication_contract =
        freeze_publication_contract(module, operation_regions, regional_publication, scope)?;
    let observability =
        crate::word::uses::netlist_observability_with_values(module, required_values)?;
    let lowering_order = observable_operation_results(module, plan, &observability)?;
    let connects = module.take_connects();
    let instance_connections = crate::word::instances::snapshot(module);
    let mut blaster = BitBlaster::<WordBackend>::new(
        module,
        BitBlasterRequest {
            plan,
            provenance,
            operation_regions,
            boundary_inputs: &[],
            source_operations: None,
            source_values: None,
            global_scope: scope,
            publication_contract,
        },
    )?;
    // Word operation order is the canonical SSA topology. Materializing the
    // live prefix in that order keeps dependency lookup cache-only for ordinary
    // operation edges instead of making call-stack depth depend on RTL depth.
    for value in lowering_order {
        blaster.value(value)?;
    }
    for (index, connect) in connects.into_iter().enumerate() {
        if !observability.observes_connect(index)? {
            continue;
        }
        match blaster.classify_connect(&connect)? {
            io::ConnectLowering::Boolean => blaster.lower_boolean_connect(&connect)?,
            io::ConnectLowering::PhysicalTriState(connect) => {
                blaster.lower_physical_tri_state_connect(connect)?;
            }
        }
    }
    for (instance_index, port, value, source) in instance_connections {
        blaster.lower_instance_connection(instance_index, port, value, source)?;
    }
    for &value in required_values {
        blaster.value(value)?;
    }
    blaster.lowered_regions.capture_lowered_values(
        &blaster.cache,
        &blaster.arena,
        &blaster.backend,
    )?;
    Ok(blaster.lowered_regions)
}

pub(crate) struct LocalRegionBooleanLowering {
    pub(crate) binding: LoweredRegionBinding,
    pub(crate) subject: crate::boolean::logic::CanonicalRegionLogic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RegionalPublicationBit {
    pub(crate) target: word::ValueId,
    pub(crate) bit: u32,
    /// The region containing the source bit producer.
    pub(crate) producer: crate::RegionRowId,
}

pub(crate) struct LocalRegionBooleanRequest<'a> {
    pub(crate) plan: &'a ArchitectureDecisions,
    pub(crate) operators: &'a crate::DurableOperatorArena,
    pub(crate) provenance: &'a mut ProvenanceBuilder,
    pub(crate) region: crate::RegionRowId,
    pub(crate) boundary_inputs: &'a [word::ValueId],
    pub(crate) roots: &'a [word::ValueId],
    /// Values whose scalar identities must remain addressable after lowering.
    /// This includes region-owned logic as well as portable boundary handles;
    /// cross-boundary authority is defined separately by the binding contract.
    pub(crate) tracked_values: &'a [word::ValueId],
}

pub(crate) fn lower_private_word_values(
    module: &mut word::WordModule,
    plan: &ArchitectureDecisions,
    provenance: &mut ProvenanceBuilder,
    region: crate::RegionRowId,
    values: &[word::ValueId],
) -> Result<LoweredRegionBinding, crate::SynthError> {
    let operation_regions = vec![Some(region); module.operations().len()];
    let mut blaster = BitBlaster::<WordBackend>::new(
        module,
        BitBlasterRequest {
            plan,
            provenance,
            operation_regions: &operation_regions,
            boundary_inputs: &[],
            source_operations: None,
            source_values: None,
            global_scope: GlobalBitblastScope::Complete,
            publication_contract: FrozenPublicationContract::default(),
        },
    )?;
    for &value in values {
        blaster.value(value)?;
    }
    blaster.lowered_regions.capture_lowered_values(
        &blaster.cache,
        &blaster.arena,
        &blaster.backend,
    )?;
    Ok(blaster.lowered_regions)
}

pub(crate) fn lower_local_region_boolean(
    module: &mut word::WordModule,
    request: LocalRegionBooleanRequest<'_>,
) -> Result<LocalRegionBooleanLowering, crate::SynthError> {
    let LocalRegionBooleanRequest {
        plan,
        operators,
        provenance,
        region,
        boundary_inputs,
        roots,
        tracked_values,
    } = request;
    operators.validate_decisions(plan)?;
    let operation_regions = vec![Some(region); module.operations().len()];
    let mut observed_values = roots.to_vec();
    observed_values.extend_from_slice(tracked_values);
    let observability =
        crate::word::uses::netlist_observability_with_values(module, &observed_values)?;
    let lowering_order = observable_operation_results(module, plan, &observability)?;
    let mut blaster = BitBlaster::<AxmBackend>::new(
        module,
        BitBlasterRequest {
            plan,
            provenance,
            operation_regions: &operation_regions,
            boundary_inputs,
            source_operations: None,
            source_values: None,
            global_scope: GlobalBitblastScope::Complete,
            publication_contract: FrozenPublicationContract::default(),
        },
    )?;
    for value in lowering_order {
        blaster.value(value)?;
    }
    for &root in roots {
        blaster.value(root)?;
    }
    for &value in tracked_values {
        blaster.value(value)?;
    }
    let mut bindings = roots
        .iter()
        .chain(tracked_values)
        .copied()
        .collect::<Vec<_>>();
    bindings.sort_unstable();
    bindings.dedup();
    let mut value_nodes = Vec::new();
    let mut input_bindings = Vec::new();
    let mut dont_care_values = Vec::new();
    for original in bindings {
        let binds_input = boundary_inputs.binary_search(&original).is_ok()
            || blaster.module.value(original).is_some_and(|stored| {
                matches!(
                    stored.kind,
                    word::ValueKind::Operation(operation)
                        if blaster.module.operation(operation).is_some_and(|operation| matches!(
                            operation.kind,
                            word::OpKind::Register(_) | word::OpKind::Latch(_)
                        ))
                )
            });
        let span = blaster
            .cache
            .get(original.index())
            .copied()
            .flatten()
            .ok_or_else(|| crate::SynthError::invariant("regional AXM binding was not lowered"))?;
        let mut lowered = Vec::with_capacity(span.len() as usize);
        for bit_index in 0..span.len() {
            let bit = blaster.bit(span, bit_index);
            let (handle, node) = match bit {
                ScalarBit::Logic(node) => {
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
                    (handle, Some(node))
                }
                ScalarBit::DontCare(_) => {
                    // Preserve don't-care propagation through the Boolean
                    // cone, then choose a deterministic constant only at the
                    // physical publication boundary. Publishing the constant
                    // node lets the mapper create a real output driver without
                    // introducing a logic-cell cost.
                    let handle = blaster.binding_constant(original, false)?;
                    dont_care_values.push(handle);
                    (
                        handle,
                        Some(crate::boolean::logic::network::LogicGraph::constant(false)),
                    )
                }
                ScalarBit::Word(_) => {
                    return Err(crate::SynthError::invariant(
                        "regional AXM binding contains a scalar Word value",
                    ));
                }
            };
            blaster.lowered_regions.bind(handle, region)?;
            lowered.push(handle);
            if let Some(node) = node {
                value_nodes.push((handle, node));
                if binds_input && node.index() != 0 {
                    input_bindings.push((handle, node));
                }
            }
        }
        if blaster.lowered_regions.lowered_values.len() <= original.index() {
            blaster
                .lowered_regions
                .lowered_values
                .resize(original.index() + 1, None);
        }
        blaster.lowered_regions.lowered_values[original.index()] = Some(lowered.into_boxed_slice());
    }
    value_nodes.sort_by_key(|&(value, _)| value);
    value_nodes.dedup_by_key(|(value, _)| *value);
    dont_care_values.sort_unstable();
    dont_care_values.dedup();
    input_bindings.sort_unstable();
    input_bindings.dedup();
    blaster.backend.bind_input_identities(&input_bindings)?;
    let (network, inputs) = std::mem::take(&mut blaster.backend).finish();
    Ok(LocalRegionBooleanLowering {
        binding: blaster.lowered_regions,
        subject: crate::boolean::logic::CanonicalRegionLogic {
            network,
            value_nodes: value_nodes.into_boxed_slice(),
            dont_care_values: dont_care_values.into_boxed_slice(),
            inputs,
        },
    })
}

fn observable_operation_results(
    module: &word::WordModule,
    plan: &ArchitectureDecisions,
    observability: &crate::word::uses::NetlistObservability,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    let mut order = Vec::new();
    for (index, operation) in module.operations().iter().enumerate() {
        let id = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        let is_operator_root = plan
            .operator_for_source_operation(id)
            .and_then(|operator| plan.operator(operator))
            .is_none_or(|operator| operator.result() == operation.result);
        if is_operator_root
            && !matches!(operation.kind, word::OpKind::TriState { .. })
            && observability.observes_value(operation.result)?
        {
            order.push(operation.result);
        }
    }
    Ok(order)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrozenPublicationBit {
    RegionArtifact,
    SubstrateConstant(BitVal),
}

/// Immutable per-bit binding at the regional publication boundary.
///
/// This contract is captured from the complete Word design before lowering
/// mutates connectivity. Regional shell endpoints and the mapped substrate
/// consume it directly; neither may rediscover membership from a partial view.
#[derive(Default)]
struct FrozenPublicationContract {
    bits: BTreeMap<word::ValueId, Box<[FrozenPublicationBit]>>,
}

fn freeze_publication_contract(
    module: &word::WordModule,
    operation_regions: &[Option<crate::RegionRowId>],
    regional_publication: &[RegionalPublicationBit],
    scope: GlobalBitblastScope,
) -> Result<FrozenPublicationContract, crate::SynthError> {
    if scope != GlobalBitblastScope::RegionalShell {
        return Ok(FrozenPublicationContract::default());
    }
    let mut facts = word::KnownBitsAnalysis::new(module);
    let mut bits = BTreeMap::new();
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
        let width = module
            .value(operation.result)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional constant proof references an unknown operation result",
                )
            })?
            .ty
            .width();
        let publication = (0..width)
            .map(|bit| match facts.bit(module, operation.result, bit) {
                word::KnownBit::Zero => FrozenPublicationBit::SubstrateConstant(BitVal::Zero),
                word::KnownBit::One => FrozenPublicationBit::SubstrateConstant(BitVal::One),
                word::KnownBit::Unknown => FrozenPublicationBit::RegionArtifact,
            })
            .collect::<Box<[_]>>();
        bits.insert(operation.result, publication);
    }
    for publication in regional_publication {
        let stored = module.value(publication.target).ok_or_else(|| {
            crate::SynthError::invariant(
                "regional publication contract references an unknown target",
            )
        })?;
        if publication.bit >= stored.ty.width() {
            return Err(crate::SynthError::invariant(
                "regional publication contract bit exceeds its target",
            ));
        }
        let operation = match stored.kind {
            word::ValueKind::Operation(operation) => operation,
            // Only operation results enter the shell publication contract;
            // root analysis never emits a signal or constant target.
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => continue,
        };
        let region = operation_regions
            .get(operation.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                crate::SynthError::invariant("regional publication target has no region binding")
            })?;
        if region != publication.producer {
            return Err(crate::SynthError::invariant(format!(
                "regional publication {:?}[{}] names producer {:?}, but the source operation belongs to {:?}",
                publication.target, publication.bit, publication.producer, region,
            )));
        }
        let contract = bits.get(&publication.target).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "regional publication contract target {:?}[{}] from producer {:?} has no frozen shell binding ({:?})",
                publication.target,
                publication.bit,
                publication.producer,
                module.operation(operation).map(|operation| &operation.kind),
            ))
        })?;
        let frozen = contract
            .get(publication.bit as usize)
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional publication bit exceeds its frozen shell contract",
                )
            })?;
        if frozen != FrozenPublicationBit::RegionArtifact {
            return Err(crate::SynthError::invariant(format!(
                "regional producer {:?} claims full-domain constant publication {:?}[{}]",
                publication.producer, publication.target, publication.bit,
            )));
        }
    }
    Ok(FrozenPublicationContract { bits })
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
    provenance: &'a mut ProvenanceBuilder,
    active_operator: Option<OperatorId>,
    active_region: Option<crate::RegionRowId>,
    operation_regions: &'a [Option<crate::RegionRowId>],
    boundary_inputs: BTreeSet<word::ValueId>,
    signal_drivers: crate::word::signal_driver::SignalDriverIndex,
    known_bits: word::KnownBitsAnalysis,
    active_values: BTreeSet<word::ValueId>,
    lowered_regions: LoweredRegionBinding,
    arena: Vec<ScalarBit>,
    cache: Vec<Option<BitSpan>>,
    constants: [Option<ScalarBit>; 8],
    source_operations: Option<&'a [Option<word::OpId>]>,
    source_values: Option<&'a BTreeMap<word::ValueId, word::ValueId>>,
    global_scope: GlobalBitblastScope,
    publication_contract: FrozenPublicationContract,
    backend: B,
}

struct BitBlasterRequest<'a> {
    plan: &'a ArchitectureDecisions,
    provenance: &'a mut ProvenanceBuilder,
    operation_regions: &'a [Option<crate::RegionRowId>],
    boundary_inputs: &'a [word::ValueId],
    source_operations: Option<&'a [Option<word::OpId>]>,
    source_values: Option<&'a BTreeMap<word::ValueId, word::ValueId>>,
    global_scope: GlobalBitblastScope,
    publication_contract: FrozenPublicationContract,
}

impl<'a, B: BitBackend> BitBlaster<'a, B> {
    fn new(
        module: &'a mut word::WordModule,
        request: BitBlasterRequest<'a>,
    ) -> Result<Self, crate::SynthError> {
        let BitBlasterRequest {
            plan,
            provenance,
            operation_regions,
            boundary_inputs,
            source_operations,
            source_values,
            global_scope,
            publication_contract,
        } = request;
        let value_count = module.values().len();
        let signal_drivers = crate::word::signal_driver::SignalDriverIndex::new(module)?;
        let known_bits = word::KnownBitsAnalysis::new(module);
        Ok(Self {
            module,
            plan,
            provenance,
            active_operator: None,
            active_region: None,
            operation_regions,
            boundary_inputs: boundary_inputs.iter().copied().collect(),
            signal_drivers,
            known_bits,
            active_values: BTreeSet::new(),
            lowered_regions: LoweredRegionBinding::new(value_count),
            arena: Vec::new(),
            cache: Vec::new(),
            constants: [None; 8],
            source_operations,
            source_values,
            global_scope,
            publication_contract,
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
        self.plan.operator_for_source_operation(operation)
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
