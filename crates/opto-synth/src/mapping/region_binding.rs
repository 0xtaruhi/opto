// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::planning::regional::{
    RegionalMemoryLogicBinding, RegionalMemoryLogicKind, RegionalMemoryStateBinding,
};
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
    SequentialInputBit {
        operation: word::OpId,
        role: SequentialInputRole,
        bit: u32,
    },
    Lowered(word::ValueId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SequentialInputRole {
    Data,
    Clock,
    Enable,
    ResetControl(u32),
    ResetValue(u32),
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
                | RegionPlanValueBinding::SequentialInputBit { .. }
                | RegionPlanValueBinding::Lowered(_) => None,
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
        memories: &crate::planning::memory::MemoryLoweringOwnership,
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
                | RegionPlanValueBinding::SequentialInputBit { .. }
                | RegionPlanValueBinding::Lowered(_) => {
                    return Ok(());
                }
            };
            *binding = RegionPlanValueBinding::SourceBit { value, bit };
            Ok(())
        };
        self.for_each_binding_mut(resolve)
    }

    pub(crate) fn resolve_sequential_sources(
        &mut self,
        module: &word::WordModule,
    ) -> Result<(), crate::SynthError> {
        let resolve = |binding: &mut RegionPlanValueBinding| -> Result<(), crate::SynthError> {
            let RegionPlanValueBinding::SequentialInputBit {
                operation,
                role,
                bit,
            } = *binding
            else {
                return Ok(());
            };
            let operation = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional sequential binding references an unknown source operation",
                )
            })?;
            let value = sequential_input(&operation.kind, role).ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional sequential binding no longer matches its source endpoint",
                )
            })?;
            let width = module
                .value(value)
                .map(|value| value.ty.width())
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "regional sequential binding references an unknown source value",
                    )
                })?;
            if bit >= width {
                return Err(crate::SynthError::invariant(
                    "regional sequential binding bit exceeds its source endpoint",
                ));
            }
            *binding = RegionPlanValueBinding::SourceBit { value, bit };
            Ok(())
        };
        self.for_each_binding_mut(resolve)
    }

    pub(crate) fn materialize_source_bits(
        &mut self,
        module: &word::WordModule,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
        memories: &crate::planning::memory::MemoryLoweringOwnership,
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
                RegionPlanValueBinding::SequentialInputBit { .. } => {
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
                ownership
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
                | RegionPlanValueBinding::SequentialInputBit { .. } => None,
            })
    }

    pub(crate) fn resolve_inputs(
        &self,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        self.inputs
            .iter()
            .copied()
            .map(|binding| resolve_plan_value(binding, ownership))
            .collect()
    }

    pub(crate) fn resolve_outputs(
        &self,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        self.outputs
            .iter()
            .copied()
            .map(|binding| resolve_plan_value(binding, ownership))
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
    pub(crate) operation_sources: &'a [Option<word::OpId>],
    pub(crate) root_bindings: &'a [(word::ValueId, word::SignalId)],
    pub(crate) ownership: &'a crate::boolean::bitblast::LoweredRegionOwnership,
}

type BindingMap = std::collections::BTreeMap<word::ValueId, Vec<RegionPlanValueBinding>>;

fn bind_owned_memory_logic_bit(
    sources: &mut BindingMap,
    outputs: &mut BindingMap,
    kind: RegionalMemoryLogicKind,
    lowered: word::ValueId,
    binding: RegionPlanValueBinding,
) {
    match kind {
        RegionalMemoryLogicKind::Combinational => {
            // Region-owned combinational lowering is an output identity only.
            // Its complete fan-in must remain covered.
            outputs.entry(lowered).or_default().push(binding);
        }
        RegionalMemoryLogicKind::SequentialState => {
            // A sequential result is published by the state artifact and
            // enters, but is never implemented by, the cover.
            sources.entry(lowered).or_default().push(binding);
        }
    }
}

fn bind_root_outputs(
    source_module: &word::WordModule,
    local_module: &word::WordModule,
    source_to_local: &std::collections::BTreeMap<word::ValueId, word::ValueId>,
    root_bindings: &[(word::ValueId, word::SignalId)],
    ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
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
        let bits = match ownership.lowered_bits(local) {
            Some(bits) => bits,
            None if width == 1 => std::slice::from_ref(&local),
            None => {
                return Err(crate::SynthError::invariant(
                    "regional root binding is absent from scalar ownership",
                ));
            }
        };
        if bits.len() != width as usize {
            return Err(crate::SynthError::invariant(
                "regional root binding width differs from scalar ownership",
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
        root_bindings,
        ownership,
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
        let bits = match ownership.lowered_bits(local) {
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
        let Some(bits) = ownership.lowered_bits(memory_state.local) else {
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
        let Some(bits) = ownership.lowered_bits(memory_logic.local) else {
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
            bind_owned_memory_logic_bit(
                &mut local_to_sources,
                &mut local_to_outputs,
                memory_logic.kind,
                lowered,
                binding,
            );
        }
    }
    canonicalize_bindings(local_module, &mut local_to_sources)?;
    canonicalize_bindings(local_module, &mut local_to_outputs)?;
    bind_root_outputs(
        source_module,
        local_module,
        source_to_local,
        root_bindings,
        ownership,
        &mut local_to_outputs,
    )?;
    for (index, operation) in local_module.operations().iter().enumerate() {
        let Some(source_operation) = operation_sources.get(index).copied().flatten() else {
            continue;
        };
        for (role, value) in sequential_inputs(&operation.kind)? {
            let Some(bits) = ownership.lowered_bits(value) else {
                continue;
            };
            for (bit, &lowered) in bits.iter().enumerate() {
                let bit = u32::try_from(bit)
                    .map_err(|_| crate::SynthError::capacity("regional sequential bit index"))?;
                let binding = RegionPlanValueBinding::SequentialInputBit {
                    operation: source_operation,
                    role,
                    bit,
                };
                local_to_sources
                    .entry(lowered)
                    .or_insert_with(|| vec![binding]);
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
        bindings.sort_unstable_by_key(|binding| match *binding {
            RegionPlanValueBinding::MemoryLogicBit {
                memory,
                ordinal,
                bit,
            } => (0, memory.raw(), ordinal, bit),
            RegionPlanValueBinding::MemoryStateBit {
                memory,
                ordinal,
                bit,
            } => (1, memory.raw(), ordinal, bit),
            RegionPlanValueBinding::SequentialInputBit {
                operation,
                role,
                bit,
            } => (2, operation.raw(), sequential_role_key(role), bit),
            RegionPlanValueBinding::SourceBit { value, bit } => {
                let kind = match source_module.value(value).map(|value| &value.kind) {
                    Some(word::ValueKind::Signal(_)) => 3,
                    Some(word::ValueKind::Constant(_)) => 4,
                    Some(word::ValueKind::Operation(_)) | None => 5,
                };
                (kind + 3, value.raw(), bit, 0)
            }
            RegionPlanValueBinding::Lowered(value) => (9, value.raw(), 0, 0),
        });
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
                    "regional publication obligation has no owner binding",
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
) -> Result<Vec<(SequentialInputRole, word::ValueId)>, crate::SynthError> {
    let mut inputs = Vec::new();
    let resets = match operation {
        word::OpKind::Register(register) => {
            inputs.push((SequentialInputRole::Data, register.d));
            inputs.push((SequentialInputRole::Clock, register.clock));
            if let Some(enable) = register.enable {
                inputs.push((SequentialInputRole::Enable, enable.value));
            }
            &register.resets
        }
        word::OpKind::Latch(latch) => {
            inputs.push((SequentialInputRole::Data, latch.d));
            inputs.push((SequentialInputRole::Enable, latch.enable.value));
            &latch.resets
        }
        _ => return Ok(inputs),
    };
    for (index, reset) in resets.iter().enumerate() {
        let index = u32::try_from(index)
            .map_err(|_| crate::SynthError::capacity("regional sequential reset index"))?;
        inputs.push((SequentialInputRole::ResetControl(index), reset.value));
        inputs.push((SequentialInputRole::ResetValue(index), reset.reset_value));
    }
    Ok(inputs)
}

fn sequential_input(operation: &word::OpKind, role: SequentialInputRole) -> Option<word::ValueId> {
    match (operation, role) {
        (word::OpKind::Register(register), SequentialInputRole::Data) => Some(register.d),
        (word::OpKind::Register(register), SequentialInputRole::Clock) => Some(register.clock),
        (word::OpKind::Register(register), SequentialInputRole::Enable) => {
            register.enable.map(|enable| enable.value)
        }
        (word::OpKind::Latch(latch), SequentialInputRole::Data) => Some(latch.d),
        (word::OpKind::Latch(latch), SequentialInputRole::Enable) => Some(latch.enable.value),
        (word::OpKind::Register(register), SequentialInputRole::ResetControl(index)) => {
            register.resets.get(index as usize).map(|reset| reset.value)
        }
        (word::OpKind::Register(register), SequentialInputRole::ResetValue(index)) => register
            .resets
            .get(index as usize)
            .map(|reset| reset.reset_value),
        (word::OpKind::Latch(latch), SequentialInputRole::ResetControl(index)) => {
            latch.resets.get(index as usize).map(|reset| reset.value)
        }
        (word::OpKind::Latch(latch), SequentialInputRole::ResetValue(index)) => latch
            .resets
            .get(index as usize)
            .map(|reset| reset.reset_value),
        (word::OpKind::Latch(_), SequentialInputRole::Clock)
        | (
            word::OpKind::Unary { .. }
            | word::OpKind::Binary { .. }
            | word::OpKind::Mux { .. }
            | word::OpKind::Extract { .. }
            | word::OpKind::DynamicExtract { .. }
            | word::OpKind::DynamicInsert { .. }
            | word::OpKind::Concat { .. }
            | word::OpKind::Cast { .. },
            _,
        ) => None,
    }
}

fn sequential_role_key(role: SequentialInputRole) -> u32 {
    match role {
        SequentialInputRole::Data => 0,
        SequentialInputRole::Clock => 1,
        SequentialInputRole::Enable => 2,
        SequentialInputRole::ResetControl(index) => index.saturating_mul(2).saturating_add(3),
        SequentialInputRole::ResetValue(index) => index.saturating_mul(2).saturating_add(4),
    }
}

fn resolve_plan_value(
    binding: RegionPlanValueBinding,
    _ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
) -> Result<word::ValueId, crate::SynthError> {
    match binding {
        RegionPlanValueBinding::Lowered(value) => Ok(value),
        RegionPlanValueBinding::SourceBit { .. }
        | RegionPlanValueBinding::MemoryLogicBit { .. }
        | RegionPlanValueBinding::MemoryStateBit { .. }
        | RegionPlanValueBinding::SequentialInputBit { .. } => Err(crate::SynthError::invariant(
            "regional plan binding was not materialized against the selected global lowering",
        )),
    }
}

#[cfg(test)]
mod tests;
