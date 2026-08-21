// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::planning::regional::{RegionalMemoryLogicBinding, RegionalMemoryStateBinding};
use opto_ir::word;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RegionPlanValueBinding {
    SourceBit {
        value: word::ValueId,
        bit: u32,
    },
    MemoryLogicBit {
        memory: word::MemoryId,
        ordinal: u32,
        bit: u32,
    },
    MemoryStateBit {
        memory: word::MemoryId,
        ordinal: u32,
        bit: u32,
    },
    SequentialPinBit(SequentialPinKey),
    Lowered(word::ValueId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SequentialPinRole {
    Data,
    Clock,
    Enable,
    ResetControl(u32),
    ResetValue(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SequentialPinKey {
    pub(crate) state: opto_ir::design::CellId,
    pub(crate) role: SequentialPinRole,
    pub(crate) bit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegionPlanBinding {
    pub(crate) inputs: Arc<[RegionPlanValueBinding]>,
    pub(crate) outputs: Arc<[RegionPlanValueBinding]>,
}

impl RegionPlanBinding {
    pub(crate) fn empty() -> Self {
        Self {
            inputs: Arc::from([]),
            outputs: Arc::from([]),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }

    pub(crate) fn source_values(&self) -> impl Iterator<Item = word::ValueId> + '_ {
        self.inputs
            .iter()
            .chain(self.outputs.iter())
            .filter_map(|binding| match *binding {
                RegionPlanValueBinding::SourceBit { value, .. } => Some(value),
                RegionPlanValueBinding::MemoryLogicBit { .. }
                | RegionPlanValueBinding::MemoryStateBit { .. }
                | RegionPlanValueBinding::SequentialPinBit(_)
                | RegionPlanValueBinding::Lowered(_) => None,
            })
    }

    pub(crate) fn sequential_pins(&self) -> impl Iterator<Item = SequentialPinKey> + '_ {
        self.outputs.iter().filter_map(|binding| match *binding {
            RegionPlanValueBinding::SequentialPinBit(pin) => Some(pin),
            _ => None,
        })
    }

    fn for_each_binding_mut(
        &mut self,
        mut visit: impl FnMut(&mut RegionPlanValueBinding) -> Result<(), crate::SynthError>,
    ) -> Result<(), crate::SynthError> {
        for binding in Arc::make_mut(&mut self.inputs) {
            visit(binding)?;
        }
        for binding in Arc::make_mut(&mut self.outputs) {
            visit(binding)?;
        }
        Ok(())
    }

    pub(crate) fn resolve_memory_sources(
        &mut self,
        module: &word::WordModule,
        memories: &crate::planning::memory::MemoryLoweringBinding,
    ) -> Result<(), crate::SynthError> {
        let resolve = |binding: &mut RegionPlanValueBinding| -> Result<(), crate::SynthError> {
            let (value, bit) = match *binding {
                RegionPlanValueBinding::MemoryLogicBit {
                    memory,
                    ordinal,
                    bit,
                } => {
                    let operation = memories.operation(memory, ordinal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "region-owned memory logic failed shell reconstruction",
                        )
                    })?;
                    let value = module
                        .operation(operation)
                        .map(|operation| operation.result)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "region-owned memory logic references an unknown operation",
                            )
                        })?;
                    (value, bit)
                }
                RegionPlanValueBinding::MemoryStateBit {
                    memory,
                    ordinal,
                    bit,
                } => (
                    memories.state_value(memory, ordinal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional plan memory state failed shell reconstruction",
                        )
                    })?,
                    bit,
                ),
                RegionPlanValueBinding::SourceBit { .. }
                | RegionPlanValueBinding::SequentialPinBit(_)
                | RegionPlanValueBinding::Lowered(_) => {
                    return Ok(());
                }
            };
            *binding = RegionPlanValueBinding::SourceBit { value, bit };
            Ok(())
        };
        self.for_each_binding_mut(resolve)
    }

    pub(crate) fn materialize_source_bits(
        &mut self,
        module: &word::WordModule,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
        memories: &crate::planning::memory::MemoryLoweringBinding,
    ) -> Result<(), crate::SynthError> {
        let endpoint_bits = module
            .values()
            .iter()
            .enumerate()
            .filter_map(|(index, stored)| {
                let word::ValueKind::Signal(reference) = stored.kind else {
                    return None;
                };
                (reference.width() == 1).then(|| {
                    word::ValueId::from_index(index)
                        .map(|value| ((reference.signal, reference.lsb), value))
                        .map_err(crate::SynthError::from)
                })
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
        let materialize = |binding: &mut RegionPlanValueBinding,
                           preserve_endpoint: bool|
         -> Result<(), crate::SynthError> {
            let (value, bit) = match *binding {
                RegionPlanValueBinding::SourceBit { value, bit } => (value, bit),
                RegionPlanValueBinding::MemoryLogicBit {
                    memory,
                    ordinal,
                    bit,
                } => {
                    let operation = memories.operation(memory, ordinal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "region-owned memory logic failed global reconstruction",
                        )
                    })?;
                    let value = module
                        .operation(operation)
                        .map(|operation| operation.result)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "region-owned memory logic references an unknown operation",
                            )
                        })?;
                    (value, bit)
                }
                RegionPlanValueBinding::MemoryStateBit {
                    memory,
                    ordinal,
                    bit,
                } => (
                    memories.state_value(memory, ordinal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional plan memory state failed global reconstruction",
                        )
                    })?,
                    bit,
                ),
                RegionPlanValueBinding::SequentialPinBit(_) if preserve_endpoint => return Ok(()),
                RegionPlanValueBinding::SequentialPinBit(_) => {
                    return Err(crate::SynthError::invariant(
                        "regional sequential binding was not resolved before scalar lowering",
                    ));
                }
                RegionPlanValueBinding::Lowered(_) => return Ok(()),
            };
            let stored = module.value(value).ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional plan binding references an unknown source value",
                )
            })?;
            let endpoint = match stored.kind {
                word::ValueKind::Signal(reference) if preserve_endpoint => reference
                    .lsb
                    .checked_add(bit)
                    .filter(|_| bit < reference.width())
                    .and_then(|lsb| endpoint_bits.get(&(reference.signal, lsb)).copied()),
                word::ValueKind::Signal(_)
                | word::ValueKind::Constant(_)
                | word::ValueKind::Operation(_) => None,
            };
            let lowered = if let Some(endpoint) = endpoint {
                endpoint
            } else if bit == 0
                && stored.ty.width() == 1
                && matches!(
                    stored.kind,
                    word::ValueKind::Signal(_) | word::ValueKind::Constant(_)
                )
            {
                value
            } else {
                region_binding
                    .lowered_bits(value)
                    .and_then(|bits| bits.get(bit as usize))
                    .copied()
                    .ok_or_else(|| {
                        let operation = match stored.kind {
                            word::ValueKind::Operation(operation) => {
                                module.operation(operation).map(|operation| &operation.kind)
                            }
                            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
                        };
                        crate::SynthError::invariant(format!(
                            "regional plan source bit {value:?}[{bit}] ({:?}, operation {operation:?}) is absent from scalar lowering",
                            stored.kind,
                        ))
                    })?
            };
            *binding = RegionPlanValueBinding::Lowered(lowered);
            Ok(())
        };
        for binding in Arc::make_mut(&mut self.inputs) {
            materialize(binding, false)?;
        }
        for binding in Arc::make_mut(&mut self.outputs) {
            materialize(binding, true)?;
        }
        Ok(())
    }

    pub(crate) fn lowered_values(&self) -> impl Iterator<Item = word::ValueId> + '_ {
        self.inputs
            .iter()
            .chain(self.outputs.iter())
            .filter_map(|binding| match *binding {
                RegionPlanValueBinding::Lowered(value) => Some(value),
                RegionPlanValueBinding::SourceBit { .. }
                | RegionPlanValueBinding::MemoryLogicBit { .. }
                | RegionPlanValueBinding::MemoryStateBit { .. }
                | RegionPlanValueBinding::SequentialPinBit(_) => None,
            })
    }

    pub(crate) fn resolve_inputs(
        &self,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        self.inputs
            .iter()
            .copied()
            .map(|binding| resolve_plan_value(binding, region_binding))
            .collect()
    }

    pub(crate) fn resolve_outputs(
        &self,
        region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        self.outputs
            .iter()
            .copied()
            .filter_map(|binding| match binding {
                RegionPlanValueBinding::SequentialPinBit(_) => None,
                _ => Some(resolve_plan_value(binding, region_binding)),
            })
            .collect()
    }
}

pub(crate) struct CandidateBinding {
    pub(crate) binding: RegionPlanBinding,
    pub(crate) output_widths: Box<[usize]>,
}

/// Borrowed identity domain used to bind one private cover to the frozen design.
///
/// Only sources present in this view may cross the regional publication
/// boundary; keeping them together centralizes that authority without owning a
/// second copy of the plan or its topology.
#[derive(Clone, Copy)]
pub(crate) struct CandidateBindingDomain<'a> {
    pub(crate) source_module: &'a word::WordModule,
    pub(crate) local_module: &'a word::WordModule,
    pub(crate) source_to_local: &'a std::collections::BTreeMap<word::ValueId, word::ValueId>,
    pub(crate) boundary_bindings: &'a [(word::ValueId, word::ValueId)],
    pub(crate) owned_memory_logic: &'a [RegionalMemoryLogicBinding],
    pub(crate) memory_states: &'a [RegionalMemoryStateBinding],
    pub(crate) operation_sources: &'a crate::planning::regional::LocalOperationProvenance,
    pub(crate) source_cells: &'a std::collections::BTreeMap<word::OpId, opto_ir::design::CellId>,
    pub(crate) root_bindings: &'a [(word::ValueId, word::SignalId)],
    pub(crate) region_binding: &'a crate::boolean::bitblast::LoweredRegionBinding,
}

type BindingMap = std::collections::BTreeMap<word::ValueId, Vec<RegionPlanValueBinding>>;

fn bind_owned_memory_logic_bit(
    outputs: &mut BindingMap,
    lowered: word::ValueId,
    binding: RegionPlanValueBinding,
) {
    // State results have a distinct MemoryStateBit identity and never enter
    // this collection. Every memory-logic row is therefore a cover output.
    outputs.entry(lowered).or_default().push(binding);
}

fn bind_root_outputs(
    source_module: &word::WordModule,
    local_module: &word::WordModule,
    source_to_local: &std::collections::BTreeMap<word::ValueId, word::ValueId>,
    root_bindings: &[(word::ValueId, word::SignalId)],
    region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
    local_to_sources: &mut std::collections::BTreeMap<word::ValueId, Vec<RegionPlanValueBinding>>,
) -> Result<(), crate::SynthError> {
    let semantics = super::roots::FullDomainRootSemantics::new(local_module)?;
    for &(source, _) in root_bindings {
        let width = source_module
            .value(source)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional root binding references an unknown source value",
                )
            })?
            .ty
            .width();
        let local = source_to_local.get(&source).copied().ok_or_else(|| {
            crate::SynthError::invariant(
                "regional root binding is absent from its frozen private cone",
            )
        })?;
        let bits = match region_binding.lowered_bits(local) {
            Some(bits) => bits,
            None if width == 1 => std::slice::from_ref(&local),
            None => {
                return Err(crate::SynthError::invariant(
                    "regional root binding is absent from scalar lowering",
                ));
            }
        };
        if bits.len() != width as usize {
            return Err(crate::SynthError::invariant(
                "regional root binding width differs from scalar lowering",
            ));
        }
        for (bit, &target) in bits.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("regional root bit index"))?;
            let bindings = local_to_sources
                .entry(semantics.canonical_root(target)?)
                .or_default();
            // A frozen root endpoint supersedes private-shell producer handles;
            // publishing both would drive the producer back into itself.
            bindings.retain(|binding| matches!(binding, RegionPlanValueBinding::SourceBit { .. }));
            bindings.push(RegionPlanValueBinding::SourceBit { value: source, bit });
        }
    }
    Ok(())
}

pub(crate) fn build_candidate_binding<'a>(
    domain: CandidateBindingDomain<'_>,
    subject_inputs: &[word::ValueId],
    output_values: impl IntoIterator<Item = &'a [word::ValueId]>,
) -> Result<CandidateBinding, crate::SynthError> {
    let CandidateBindingDomain {
        source_module,
        local_module,
        source_to_local,
        boundary_bindings,
        owned_memory_logic,
        memory_states,
        operation_sources,
        source_cells,
        root_bindings,
        region_binding,
    } = domain;
    let output_values = output_values.into_iter().collect::<Vec<_>>();
    let mut local_to_sources = BindingMap::new();
    // Input identities come only from the region graph's frozen boundary
    // contract. The complete source-to-local provenance map also contains
    // owned operations, observations, and publication roots, none of which are
    // immutable inputs. Output identities are similarly limited to explicit
    // owned roots and sequential endpoints.
    let mut local_to_outputs = BindingMap::new();
    for &(source, local) in boundary_bindings {
        let bits = match region_binding.lowered_bits(local) {
            Some(bits) => bits,
            None if local_module
                .value(local)
                .is_some_and(|value| value.ty.width() == 1) =>
            {
                std::slice::from_ref(&local)
            }
            None => continue,
        };
        for (bit, &lowered) in bits.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("regional source bit index"))?;
            local_to_sources
                .entry(lowered)
                .or_default()
                .push(RegionPlanValueBinding::SourceBit { value: source, bit });
        }
    }
    for memory_state in memory_states {
        let Some(bits) = region_binding.lowered_bits(memory_state.local) else {
            continue;
        };
        for (bit, &lowered) in bits.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("regional memory bit index"))?;
            let binding = RegionPlanValueBinding::MemoryStateBit {
                memory: memory_state.source_memory,
                ordinal: memory_state.ordinal,
                bit,
            };
            local_to_sources.entry(lowered).or_default().push(binding);
        }
    }
    for memory_logic in owned_memory_logic {
        let Some(bits) = region_binding.lowered_bits(memory_logic.local) else {
            continue;
        };
        for (bit, &lowered) in bits.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("region-owned memory logic bit index"))?;
            let binding = RegionPlanValueBinding::MemoryLogicBit {
                memory: memory_logic.source_memory,
                ordinal: memory_logic.ordinal,
                bit,
            };
            bind_owned_memory_logic_bit(&mut local_to_outputs, lowered, binding);
        }
    }
    canonicalize_bindings(local_module, &mut local_to_sources)?;
    canonicalize_bindings(local_module, &mut local_to_outputs)?;
    bind_root_outputs(
        source_module,
        local_module,
        source_to_local,
        root_bindings,
        region_binding,
        &mut local_to_outputs,
    )?;
    for (index, operation) in local_module.operations().iter().enumerate() {
        let local_operation = word::OpId::from_index(index).map_err(crate::SynthError::from)?;
        let Some([source_operation]) = operation_sources.sources(local_operation) else {
            continue;
        };
        let state = source_cells.get(source_operation).copied().ok_or_else(|| {
            crate::SynthError::invariant(
                "regional state provenance has no stable logical cell binding",
            )
        })?;
        let source_kind = &source_module
            .operation(*source_operation)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional state provenance references an unknown source operation",
                )
            })?
            .kind;
        for (role, value) in sequential_inputs(&operation.kind)? {
            let Some(bits) = region_binding.lowered_bits(value) else {
                continue;
            };
            let source = sequential_input(source_kind, role);
            let source_bits = source
                .and_then(|source| source_to_local.get(&source))
                .and_then(|local| region_binding.lowered_bits(*local));
            for (bit, &lowered) in bits.iter().enumerate() {
                let bit = u32::try_from(bit)
                    .map_err(|_| crate::SynthError::capacity("regional sequential bit index"))?;
                if source_bits.and_then(|bits| bits.get(bit as usize)).copied() == Some(lowered) {
                    local_to_sources.entry(lowered).or_insert_with(|| {
                        vec![RegionPlanValueBinding::SourceBit {
                            value: source.expect("matching source bits require a source value"),
                            bit,
                        }]
                    });
                }
                let binding =
                    RegionPlanValueBinding::SequentialPinBit(SequentialPinKey { state, role, bit });
                local_to_outputs
                    .entry(lowered)
                    .or_insert_with(|| vec![binding]);
            }
        }
    }
    for operation in local_module.operations() {
        let Some(source) = super::roots::scalar_projection_input(local_module, operation) else {
            continue;
        };
        let Some(bindings) = local_to_sources.get(&source).cloned() else {
            if let Some(bindings) = local_to_outputs.get(&source).cloned() {
                local_to_outputs.entry(operation.result).or_insert(bindings);
            }
            continue;
        };
        local_to_sources.entry(operation.result).or_insert(bindings);
        if let Some(bindings) = local_to_outputs.get(&source).cloned() {
            local_to_outputs.entry(operation.result).or_insert(bindings);
        }
    }
    for bindings in local_to_sources
        .values_mut()
        .chain(local_to_outputs.values_mut())
    {
        bindings.sort_unstable();
        bindings.dedup();
    }
    complete_binding_aliases(
        local_module,
        subject_inputs,
        &output_values,
        &mut local_to_sources,
        &mut local_to_outputs,
    )?;
    let locate = |value: word::ValueId| {
        local_to_sources
            .get(&value)
            .and_then(|bindings| bindings.first())
            .copied()
            .ok_or_else(|| {
                let operation = local_module.value(value).and_then(|stored| match stored.kind {
                    word::ValueKind::Operation(operation) => {
                        local_module.operation(operation).map(|operation| &operation.kind)
                    }
                    word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
                });
                crate::SynthError::invariant(format!(
                    "region-local cover input value {value:?} ({:?}, operation {operation:?}) has no immutable source-bit binding",
                    local_module.value(value).map(|stored| &stored.kind),
                ))
            })
    };
    let locate_all = |value: word::ValueId| {
        local_to_outputs
            .get(&value)
            .cloned()
            .map(Vec::into_boxed_slice)
            .ok_or_else(|| {
                let operation = local_module.value(value).and_then(|stored| match stored.kind {
                    word::ValueKind::Operation(operation) => {
                        local_module.operation(operation).map(|operation| &operation.kind)
                    }
                    word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
                });
                crate::SynthError::invariant(format!(
                    "region-local cover output value {value:?} ({:?}, operation {operation:?}) has no immutable source-bit binding",
                    local_module.value(value).map(|stored| &stored.kind),
                ))
            })
    };
    let inputs = subject_inputs
        .iter()
        .copied()
        .map(locate)
        .collect::<Result<Vec<_>, _>>()?
        .into();
    let mut output_widths = Vec::with_capacity(output_values.len());
    let outputs = output_values
        .into_iter()
        .map(|values| {
            let [value] = values else {
                return Err(crate::SynthError::invariant(
                    "regional publication obligation is not one global root",
                ));
            };
            let bindings = locate_all(*value)?;
            if bindings.is_empty() {
                return Err(crate::SynthError::invariant(
                    "regional publication obligation has no source binding",
                ));
            }
            output_widths.push(bindings.len());
            Ok(bindings)
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .into();
    Ok(CandidateBinding {
        binding: RegionPlanBinding { inputs, outputs },
        output_widths: output_widths.into_boxed_slice(),
    })
}

fn canonicalize_bindings(
    module: &word::WordModule,
    bindings: &mut BindingMap,
) -> Result<(), crate::SynthError> {
    let semantics = super::roots::FullDomainRootSemantics::new(module)?;
    let mut canonical = BindingMap::new();
    for (value, values) in std::mem::take(bindings) {
        canonical
            .entry(semantics.canonical_root(value)?)
            .or_default()
            .extend(values);
    }
    *bindings = canonical;
    Ok(())
}

fn complete_binding_aliases(
    module: &word::WordModule,
    subject_inputs: &[word::ValueId],
    output_values: &[&[word::ValueId]],
    sources: &mut BindingMap,
    outputs: &mut BindingMap,
) -> Result<(), crate::SynthError> {
    let signal_bindings = |bindings: &BindingMap| {
        bindings
            .iter()
            .filter_map(|(&value, bindings)| {
                let word::ValueKind::Signal(reference) = module.value(value)?.kind else {
                    return None;
                };
                Some((
                    (reference.signal, reference.lsb, reference.width()),
                    bindings.clone(),
                ))
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let source_signals = signal_bindings(sources);
    let output_signals = signal_bindings(outputs);
    for (index, value) in module.values().iter().enumerate() {
        let word::ValueKind::Signal(reference) = value.kind else {
            continue;
        };
        let local = word::ValueId::from_index(index).map_err(crate::SynthError::from)?;
        let key = (reference.signal, reference.lsb, reference.width());
        if let Some(bindings) = source_signals.get(&key) {
            sources.entry(local).or_insert_with(|| bindings.clone());
        }
        if let Some(bindings) = output_signals.get(&key) {
            outputs.entry(local).or_insert_with(|| bindings.clone());
        }
    }
    let drivers = crate::word::signal_driver::SignalDriverIndex::new(module)?;
    for &value in subject_inputs {
        resolve_immutable_binding_alias(
            value,
            module,
            &drivers,
            sources,
            &mut std::collections::BTreeSet::new(),
        )?;
        resolve_immutable_binding_alias(
            value,
            module,
            &drivers,
            outputs,
            &mut std::collections::BTreeSet::new(),
        )?;
    }
    for &value in output_values.iter().flat_map(|values| values.iter()) {
        resolve_immutable_binding_alias(
            value,
            module,
            &drivers,
            outputs,
            &mut std::collections::BTreeSet::new(),
        )?;
    }
    Ok(())
}

fn resolve_immutable_binding_alias(
    value: word::ValueId,
    module: &word::WordModule,
    signal_drivers: &crate::word::signal_driver::SignalDriverIndex,
    bindings: &mut std::collections::BTreeMap<word::ValueId, Vec<RegionPlanValueBinding>>,
    active: &mut std::collections::BTreeSet<word::ValueId>,
) -> Result<Option<Vec<RegionPlanValueBinding>>, crate::SynthError> {
    if let Some(resolved) = bindings.get(&value) {
        return Ok(Some(resolved.clone()));
    }
    if !active.insert(value) {
        return Err(crate::SynthError::invariant(
            "region-local immutable binding aliases contain a cycle",
        ));
    }
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant("region-local binding references an unknown value")
    })?;
    let source = match stored.kind {
        word::ValueKind::Operation(operation) => module
            .operation(operation)
            .and_then(|operation| super::roots::scalar_projection_input(module, operation)),
        word::ValueKind::Signal(reference) => signal_drivers.scalar_driver(module, reference),
        word::ValueKind::Constant(_) => None,
    };
    let resolved = source
        .map(|source| {
            resolve_immutable_binding_alias(source, module, signal_drivers, bindings, active)
        })
        .transpose()?
        .flatten();
    active.remove(&value);
    if let Some(resolved) = &resolved {
        bindings.insert(value, resolved.clone());
    }
    Ok(resolved)
}

fn sequential_inputs(
    operation: &word::OpKind,
) -> Result<Vec<(SequentialPinRole, word::ValueId)>, crate::SynthError> {
    let mut inputs = Vec::new();
    let resets = match operation {
        word::OpKind::Register(register) => {
            inputs.push((SequentialPinRole::Data, register.d));
            inputs.push((SequentialPinRole::Clock, register.clock));
            if let Some(enable) = register.enable {
                inputs.push((SequentialPinRole::Enable, enable.value));
            }
            &register.resets
        }
        word::OpKind::Latch(latch) => {
            inputs.push((SequentialPinRole::Data, latch.d));
            inputs.push((SequentialPinRole::Enable, latch.enable.value));
            &latch.resets
        }
        _ => return Ok(inputs),
    };
    for (index, reset) in resets.iter().enumerate() {
        let index = u32::try_from(index)
            .map_err(|_| crate::SynthError::capacity("regional sequential reset index"))?;
        inputs.push((SequentialPinRole::ResetControl(index), reset.value));
        inputs.push((SequentialPinRole::ResetValue(index), reset.reset_value));
    }
    Ok(inputs)
}

pub(crate) fn sequential_input(
    operation: &word::OpKind,
    role: SequentialPinRole,
) -> Option<word::ValueId> {
    match (operation, role) {
        (word::OpKind::Register(register), SequentialPinRole::Data) => Some(register.d),
        (word::OpKind::Register(register), SequentialPinRole::Clock) => Some(register.clock),
        (word::OpKind::Register(register), SequentialPinRole::Enable) => {
            register.enable.map(|enable| enable.value)
        }
        (word::OpKind::Latch(latch), SequentialPinRole::Data) => Some(latch.d),
        (word::OpKind::Latch(latch), SequentialPinRole::Enable) => Some(latch.enable.value),
        (word::OpKind::Register(register), SequentialPinRole::ResetControl(index)) => {
            register.resets.get(index as usize).map(|reset| reset.value)
        }
        (word::OpKind::Register(register), SequentialPinRole::ResetValue(index)) => register
            .resets
            .get(index as usize)
            .map(|reset| reset.reset_value),
        (word::OpKind::Latch(latch), SequentialPinRole::ResetControl(index)) => {
            latch.resets.get(index as usize).map(|reset| reset.value)
        }
        (word::OpKind::Latch(latch), SequentialPinRole::ResetValue(index)) => latch
            .resets
            .get(index as usize)
            .map(|reset| reset.reset_value),
        (word::OpKind::Latch(_), SequentialPinRole::Clock)
        | (
            word::OpKind::Unary { .. }
            | word::OpKind::Binary { .. }
            | word::OpKind::Mux { .. }
            | word::OpKind::TriState { .. }
            | word::OpKind::Extract { .. }
            | word::OpKind::DynamicExtract { .. }
            | word::OpKind::DynamicInsert { .. }
            | word::OpKind::Concat { .. }
            | word::OpKind::Cast { .. },
            _,
        ) => None,
    }
}

fn resolve_plan_value(
    binding: RegionPlanValueBinding,
    _region_binding: &crate::boolean::bitblast::LoweredRegionBinding,
) -> Result<word::ValueId, crate::SynthError> {
    match binding {
        RegionPlanValueBinding::Lowered(value) => Ok(value),
        RegionPlanValueBinding::SourceBit { .. }
        | RegionPlanValueBinding::MemoryLogicBit { .. }
        | RegionPlanValueBinding::MemoryStateBit { .. }
        | RegionPlanValueBinding::SequentialPinBit(_) => Err(crate::SynthError::invariant(
            "regional plan binding was not materialized against the selected global lowering",
        )),
    }
}

#[cfg(test)]
mod tests;
