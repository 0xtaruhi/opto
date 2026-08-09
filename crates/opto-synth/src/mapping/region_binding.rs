// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::planning::regional::{RegionalMemoryValueBinding, RegionalMemoryValueKind};
use opto_ir::word;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RegionPlanValueBinding {
    SourceBit {
        value: word::ValueId,
        bit: u32,
    },
    MemoryOperationBit {
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
    pub(crate) outputs: Arc<[Arc<[RegionPlanValueBinding]>]>,
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
            .chain(self.outputs.iter().flat_map(|bindings| bindings.iter()))
            .filter_map(|binding| match *binding {
                RegionPlanValueBinding::SourceBit { value, .. } => Some(value),
                RegionPlanValueBinding::MemoryOperationBit { .. }
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
        for bindings in Arc::make_mut(&mut self.outputs) {
            for binding in Arc::make_mut(bindings) {
                visit(binding)?;
            }
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
                RegionPlanValueBinding::MemoryOperationBit {
                    memory,
                    ordinal,
                    bit,
                } => {
                    let operation = memories.operation(memory, ordinal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional plan memory operation failed shell reconstruction",
                        )
                    })?;
                    let value = module
                        .operation(operation)
                        .map(|operation| operation.result)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "regional plan memory operation references an unknown operation",
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
        let materialize = |binding: &mut RegionPlanValueBinding| -> Result<(), crate::SynthError> {
            let (value, bit) = match *binding {
                RegionPlanValueBinding::SourceBit { value, bit } => (value, bit),
                RegionPlanValueBinding::MemoryOperationBit {
                    memory,
                    ordinal,
                    bit,
                } => {
                    let operation = memories.operation(memory, ordinal).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "regional plan memory operation failed global reconstruction",
                        )
                    })?;
                    let value = module
                        .operation(operation)
                        .map(|operation| operation.result)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "regional plan memory operation references an unknown operation",
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
            let lowered = if bit == 0
                && stored.ty.width() == 1
                && matches!(
                    stored.kind,
                    word::ValueKind::Signal(_) | word::ValueKind::Constant(_)
                ) {
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
        self.for_each_binding_mut(materialize)
    }

    pub(crate) fn lowered_values(&self) -> impl Iterator<Item = word::ValueId> + '_ {
        self.inputs
            .iter()
            .chain(self.outputs.iter().flat_map(|bindings| bindings.iter()))
            .filter_map(|binding| match *binding {
                RegionPlanValueBinding::Lowered(value) => Some(value),
                RegionPlanValueBinding::SourceBit { .. }
                | RegionPlanValueBinding::MemoryOperationBit { .. }
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
            .flat_map(|bindings| bindings.iter().copied())
            .map(|binding| resolve_plan_value(binding, ownership))
            .collect()
    }

    pub(crate) fn resolve_output_groups(
        &self,
        ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
    ) -> Result<Vec<Vec<word::ValueId>>, crate::SynthError> {
        self.outputs
            .iter()
            .map(|bindings| {
                let mut values = bindings
                    .iter()
                    .copied()
                    .map(|binding| resolve_plan_value(binding, ownership))
                    .collect::<Result<Vec<_>, _>>()?;
                values.sort_unstable();
                values.dedup();
                Ok(values)
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CandidateBindingInputs<'a> {
    pub(crate) source_module: &'a word::WordModule,
    pub(crate) local_module: &'a word::WordModule,
    pub(crate) source_to_local: &'a std::collections::BTreeMap<word::ValueId, word::ValueId>,
    pub(crate) boundary_bindings: &'a [(word::ValueId, word::ValueId)],
    pub(crate) memory_values: &'a [RegionalMemoryValueBinding],
    pub(crate) operation_sources: &'a [Option<word::OpId>],
    pub(crate) root_bindings: &'a [(word::ValueId, word::SignalId)],
    pub(crate) ownership: &'a crate::boolean::bitblast::LoweredRegionOwnership,
}

fn bind_root_outputs(
    source_module: &word::WordModule,
    local_module: &word::WordModule,
    root_bindings: &[(word::ValueId, word::SignalId)],
    local_to_sources: &mut std::collections::BTreeMap<word::ValueId, Vec<RegionPlanValueBinding>>,
) -> Result<(), crate::SynthError> {
    for &(source, signal) in root_bindings {
        let width = source_module
            .value(source)
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "regional root binding references an unknown source value",
                )
            })?
            .ty
            .width();
        for connect in local_module
            .connects()
            .iter()
            .filter(|connect| connect.target.signal == signal)
        {
            let bit = match connect.target.range {
                Some(range) if range.msb == range.lsb => range.lsb,
                None if width == 1 => 0,
                Some(_) | None => {
                    return Err(crate::SynthError::invariant(
                        "regional root output was not scalarized before binding",
                    ));
                }
            };
            if bit >= width {
                return Err(crate::SynthError::invariant(
                    "regional root output bit exceeds its source value",
                ));
            }
            local_to_sources
                .entry(connect.value)
                .or_default()
                .push(RegionPlanValueBinding::SourceBit { value: source, bit });
        }
    }
    Ok(())
}

pub(crate) fn build_candidate_binding<'a>(
    inputs: CandidateBindingInputs<'_>,
    subject_inputs: &[word::ValueId],
    output_values: impl IntoIterator<Item = &'a [word::ValueId]>,
) -> Result<RegionPlanBinding, crate::SynthError> {
    let CandidateBindingInputs {
        source_module,
        local_module,
        source_to_local,
        boundary_bindings,
        memory_values,
        operation_sources,
        root_bindings,
        ownership,
    } = inputs;
    let output_values = output_values.into_iter().collect::<Vec<_>>();
    let mut local_to_sources =
        std::collections::BTreeMap::<word::ValueId, Vec<RegionPlanValueBinding>>::new();
    for (source, local) in source_to_local
        .iter()
        .map(|(&source, &local)| (source, local))
        .chain(boundary_bindings.iter().copied())
    {
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
    for memory_value in memory_values {
        let Some(bits) = ownership.lowered_bits(memory_value.local) else {
            continue;
        };
        for (bit, &lowered) in bits.iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| crate::SynthError::capacity("regional memory bit index"))?;
            let binding = match memory_value.kind {
                RegionalMemoryValueKind::Operation => RegionPlanValueBinding::MemoryOperationBit {
                    memory: memory_value.source_memory,
                    ordinal: memory_value.ordinal,
                    bit,
                },
                RegionalMemoryValueKind::State => RegionPlanValueBinding::MemoryStateBit {
                    memory: memory_value.source_memory,
                    ordinal: memory_value.ordinal,
                    bit,
                },
            };
            local_to_sources.entry(lowered).or_default().push(binding);
        }
    }
    bind_root_outputs(
        source_module,
        local_module,
        root_bindings,
        &mut local_to_sources,
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
                local_to_sources.entry(lowered).or_insert_with(|| {
                    vec![RegionPlanValueBinding::SequentialInputBit {
                        operation: source_operation,
                        role,
                        bit,
                    }]
                });
            }
        }
    }
    for operation in local_module.operations() {
        let Some(source) = scalar_alias_input(local_module, operation) else {
            continue;
        };
        let Some(bindings) = local_to_sources.get(&source).cloned() else {
            continue;
        };
        local_to_sources.entry(operation.result).or_insert(bindings);
    }
    for bindings in local_to_sources.values_mut() {
        bindings.sort_unstable_by_key(|binding| match *binding {
            RegionPlanValueBinding::SourceBit { value, bit } => {
                let kind = match source_module.value(value).map(|value| &value.kind) {
                    Some(word::ValueKind::Signal(_)) => 0,
                    Some(word::ValueKind::Constant(_)) => 1,
                    Some(word::ValueKind::Operation(_)) | None => 2,
                };
                (kind, value.raw(), bit, 0)
            }
            RegionPlanValueBinding::MemoryOperationBit {
                memory,
                ordinal,
                bit,
            } => (3, memory.raw(), ordinal, bit),
            RegionPlanValueBinding::MemoryStateBit {
                memory,
                ordinal,
                bit,
            } => (4, memory.raw(), ordinal, bit),
            RegionPlanValueBinding::SequentialInputBit {
                operation,
                role,
                bit,
            } => (5, operation.raw(), sequential_role_key(role), bit),
            RegionPlanValueBinding::Lowered(value) => (6, value.raw(), 0, 0),
        });
        bindings.dedup();
    }
    let signal_bindings = local_to_sources
        .iter()
        .filter_map(|(&value, bindings)| {
            let word::ValueKind::Signal(reference) = local_module.value(value)?.kind else {
                return None;
            };
            Some((
                (reference.signal, reference.lsb, reference.width()),
                bindings.clone(),
            ))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for (index, value) in local_module.values().iter().enumerate() {
        let word::ValueKind::Signal(reference) = value.kind else {
            continue;
        };
        let Some(bindings) =
            signal_bindings.get(&(reference.signal, reference.lsb, reference.width()))
        else {
            continue;
        };
        let local = word::ValueId::from_index(index).map_err(crate::SynthError::from)?;
        local_to_sources
            .entry(local)
            .or_insert_with(|| bindings.clone());
    }
    let signal_drivers = crate::word::signal_driver::SignalDriverIndex::new(local_module)?;
    let values_to_bind = subject_inputs
        .iter()
        .copied()
        .chain(
            output_values
                .iter()
                .flat_map(|values| values.iter().copied()),
        )
        .collect::<std::collections::BTreeSet<_>>();
    for value in values_to_bind {
        resolve_immutable_binding_alias(
            value,
            local_module,
            &signal_drivers,
            &mut local_to_sources,
            &mut std::collections::BTreeSet::new(),
        )?;
    }
    let locate = |value: word::ValueId| {
        local_to_sources
            .get(&value)
            .and_then(|bindings| bindings.first())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "region-local cover input value {value:?} ({:?}) has no immutable source-bit binding",
                    local_module.value(value).map(|stored| &stored.kind)
                ))
            })
    };
    let locate_all = |value: word::ValueId| {
        local_to_sources
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
    let outputs = output_values
        .into_iter()
        .map(|values| {
            let mut bindings = values
                .iter()
                .copied()
                .map(locate_all)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            bindings.sort_unstable();
            bindings.dedup();
            Ok(Arc::from(bindings))
        })
        .collect::<Result<Vec<_>, crate::SynthError>>()?
        .into();
    Ok(RegionPlanBinding { inputs, outputs })
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
    let source =
        match stored.kind {
            word::ValueKind::Operation(operation) => module
                .operation(operation)
                .and_then(|operation| scalar_alias_input(module, operation)),
            word::ValueKind::Signal(reference) => signal_drivers
                .resolve_reference(reference)
                .and_then(|resolved| match resolved.as_slice() {
                    [(driver, 0)]
                        if module
                            .value(*driver)
                            .is_some_and(|driver| driver.ty.width() == 1) =>
                    {
                        Some(*driver)
                    }
                    _ => None,
                }),
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

fn scalar_alias_input(
    module: &word::WordModule,
    operation: &word::Operation,
) -> Option<word::ValueId> {
    let result = module.value(operation.result)?;
    if result.ty.width() != 1 {
        return None;
    }
    match &operation.kind {
        word::OpKind::Cast { value, .. }
            if module
                .value(*value)
                .is_some_and(|value| value.ty.width() == 1) =>
        {
            Some(*value)
        }
        word::OpKind::Extract { value, lsb, .. }
            if *lsb == 0
                && module
                    .value(*value)
                    .is_some_and(|value| value.ty.width() == 1) =>
        {
            Some(*value)
        }
        word::OpKind::Concat { parts } if parts.len() == 1 => Some(parts[0]),
        word::OpKind::Unary { .. }
        | word::OpKind::Binary { .. }
        | word::OpKind::Mux { .. }
        | word::OpKind::Register(_)
        | word::OpKind::Latch(_)
        | word::OpKind::Extract { .. }
        | word::OpKind::DynamicExtract { .. }
        | word::OpKind::DynamicInsert { .. }
        | word::OpKind::Cast { .. }
        | word::OpKind::Concat { .. } => None,
    }
}

fn resolve_plan_value(
    binding: RegionPlanValueBinding,
    _ownership: &crate::boolean::bitblast::LoweredRegionOwnership,
) -> Result<word::ValueId, crate::SynthError> {
    match binding {
        RegionPlanValueBinding::Lowered(value) => Ok(value),
        RegionPlanValueBinding::SourceBit { .. }
        | RegionPlanValueBinding::MemoryOperationBit { .. }
        | RegionPlanValueBinding::MemoryStateBit { .. }
        | RegionPlanValueBinding::SequentialInputBit { .. } => Err(crate::SynthError::invariant(
            "regional plan binding was not materialized against the selected global lowering",
        )),
    }
}

#[cfg(test)]
mod tests;
