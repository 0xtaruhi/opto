// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

pub(crate) struct NetlistObservability {
    root_signals: Box<[bool]>,
    observed_signals: Box<[bool]>,
    reachable_values: Box<[bool]>,
    live_connects: Box<[bool]>,
    root_connects: Box<[bool]>,
    publication_connects: Box<[bool]>,
    observed_memories: Box<[bool]>,
    instance_root_values: Box<[word::ValueId]>,
    non_connect_root_values: Box<[word::ValueId]>,
    root_values: Box<[word::ValueId]>,
}

impl NetlistObservability {
    pub(crate) fn observes_signal(
        &self,
        signal: word::SignalId,
    ) -> Result<bool, crate::SynthError> {
        self.observed_signals
            .get(signal.index())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "observability query references a signal outside the Word arena",
                )
            })
    }

    pub(crate) fn observes_root_signal(
        &self,
        signal: word::SignalId,
    ) -> Result<bool, crate::SynthError> {
        self.root_signals
            .get(signal.index())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "observability query references a root signal outside the Word arena",
                )
            })
    }

    pub(crate) fn observes_value(&self, value: word::ValueId) -> Result<bool, crate::SynthError> {
        self.reachable_values
            .get(value.index())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "observability query references a value outside the Word arena",
                )
            })
    }

    pub(crate) fn observes_connect(&self, index: usize) -> Result<bool, crate::SynthError> {
        self.live_connects.get(index).copied().ok_or_else(|| {
            crate::SynthError::invariant(
                "observability query references a connection outside the Word arena",
            )
        })
    }

    pub(crate) fn observes_root_connect(&self, index: usize) -> Result<bool, crate::SynthError> {
        self.root_connects.get(index).copied().ok_or_else(|| {
            crate::SynthError::invariant(
                "observability query references a root connection outside the Word arena",
            )
        })
    }

    pub(crate) fn observes_publication_connect(
        &self,
        index: usize,
    ) -> Result<bool, crate::SynthError> {
        self.publication_connects
            .get(index)
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "observability query references a publication outside the connection arena",
                )
            })
    }

    pub(crate) fn observes_memory(
        &self,
        memory: word::MemoryId,
    ) -> Result<bool, crate::SynthError> {
        self.observed_memories
            .get(memory.index())
            .copied()
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "observability query references a memory outside the Word arena",
                )
            })
    }

    pub(crate) fn non_connect_root_values(&self) -> &[word::ValueId] {
        &self.non_connect_root_values
    }

    pub(crate) fn instance_root_values(&self) -> &[word::ValueId] {
        &self.instance_root_values
    }

    pub(crate) fn root_values(&self) -> &[word::ValueId] {
        &self.root_values
    }
}

/// Computes the externally observable signal/connection closure.
///
/// Ports, preserved signals, child-instance bindings, and explicit values seed
/// the walk. Reading a signal makes its driver observable, and the driver's
/// operands can expose further signal reads. Reading a memory output retains
/// that memory and all of its port controls. State and memories are retained
/// only when the closure reaches them; otherwise they are not implicit roots.
/// The packed signal-to-connect index keeps the closure linear in netlist size.
pub(crate) fn netlist_observability(
    module: &word::WordModule,
) -> Result<NetlistObservability, crate::SynthError> {
    netlist_observability_with_values(module, &[])
}

/// Computes netlist observability with additional explicitly observed values.
pub(crate) fn netlist_observability_with_values(
    module: &word::WordModule,
    observed_values: &[word::ValueId],
) -> Result<NetlistObservability, crate::SynthError> {
    let connects_by_signal = opto_core::PackedRows::try_from_entries(
        module.signals().len(),
        module
            .connects()
            .iter()
            .enumerate()
            .map(|(index, connect)| (connect.target.signal.index(), index)),
    )
    .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
    let memory_reads_by_signal = opto_core::PackedRows::try_from_entries(
        module.signals().len(),
        module
            .memory_read_ports()
            .iter()
            .enumerate()
            .map(|(index, read)| (read.data.index(), index)),
    )
    .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
    let memory_values = opto_core::PackedRows::try_from_entries(
        module.memories().len(),
        module
            .memory_read_ports()
            .iter()
            .flat_map(|read| memory_read_values(read).map(|value| (read.memory.index(), value)))
            .chain(module.memory_write_ports().iter().flat_map(|write| {
                memory_write_values(write).map(|value| (write.memory.index(), value))
            })),
    )
    .map_err(|error| crate::SynthError::invariant(error.to_string()))?;
    let mut live_connects = vec![false; module.connects().len()];
    let mut observed_signals = vec![false; module.signals().len()];
    let mut reachable_values = vec![false; module.values().len()];
    let mut observed_memories = vec![false; module.memories().len()];
    let observed_boundary_signals = module
        .ports()
        .iter()
        .map(|port| port.signal)
        .chain(module.preserved_signals())
        .collect::<Vec<_>>();
    let root_boundary_signals = module
        .ports()
        .iter()
        .filter(|port| {
            matches!(
                port.direction,
                word::PortDirection::Output | word::PortDirection::Inout
            )
        })
        .map(|port| port.signal)
        .chain(module.preserved_signals())
        .collect::<Vec<_>>();
    let mut root_signals = vec![false; module.signals().len()];
    for signal in &root_boundary_signals {
        *root_signals.get_mut(signal.index()).ok_or_else(|| {
            crate::SynthError::invariant("observability root references an unknown signal")
        })? = true;
    }
    let mut instance_root_values = module
        .instances()
        .iter()
        .flat_map(|instance| &instance.connections)
        .map(|connection| connection.value)
        .collect::<Vec<_>>();
    instance_root_values.sort_unstable();
    instance_root_values.dedup();
    let mut non_connect_root_values = instance_root_values
        .iter()
        .copied()
        .chain(observed_values.iter().copied())
        .collect::<Vec<_>>();
    non_connect_root_values.sort_unstable();
    non_connect_root_values.dedup();
    let mut root_values = non_connect_root_values.clone();
    let mut pending_signals = observed_boundary_signals;
    let mut pending_values = root_values.clone();

    loop {
        while let Some(signal) = pending_signals.pop() {
            let observed = observed_signals.get_mut(signal.index()).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "observability root references unknown signal {signal:?}"
                ))
            })?;
            if std::mem::replace(observed, true) {
                continue;
            }
            for &connect_index in connects_by_signal.row(signal.index()) {
                if std::mem::replace(&mut live_connects[connect_index], true) {
                    continue;
                }
                let connect = &module.connects()[connect_index];
                pending_values.push(connect.value);
                pending_values.extend(connect.target.dynamic.map(|dynamic| dynamic.offset));
            }
            for &read_index in memory_reads_by_signal.row(signal.index()) {
                let read = module.memory_read_ports().get(read_index).ok_or_else(|| {
                    crate::SynthError::invariant("observability lost a memory-read port")
                })?;
                let observed = observed_memories
                    .get_mut(read.memory.index())
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "observability memory read references an unknown memory",
                        )
                    })?;
                if std::mem::replace(observed, true) {
                    continue;
                }
                for &value in memory_values.row(read.memory.index()) {
                    non_connect_root_values.push(value);
                    root_values.push(value);
                    pending_values.push(value);
                }
            }
        }

        let Some(value_id) = pending_values.pop() else {
            break;
        };
        let reachable = reachable_values.get_mut(value_id.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "observability root references unknown value {value_id:?}"
            ))
        })?;
        if std::mem::replace(reachable, true) {
            continue;
        }
        let value = module.value(value_id).ok_or_else(|| {
            crate::SynthError::invariant(format!("observability walk lost value {value_id:?}"))
        })?;
        match value.kind {
            word::ValueKind::Signal(reference) => pending_signals.push(reference.signal),
            word::ValueKind::Operation(operation) => {
                let operation = module.operation(operation).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "observability walk lost operation {operation:?}"
                    ))
                })?;
                pending_values.extend(crate::word::operation_inputs(&operation.kind));
            }
            word::ValueKind::Constant(_) => {}
        }
    }

    let mut root_connects = vec![false; module.connects().len()];
    for signal in root_boundary_signals {
        for &connect_index in connects_by_signal.row(signal.index()) {
            if !live_connects[connect_index] {
                continue;
            }
            root_connects[connect_index] = true;
            let connect = &module.connects()[connect_index];
            root_values.push(connect.value);
            root_values.extend(connect.target.dynamic.map(|dynamic| dynamic.offset));
        }
    }
    let mut publication_connects = root_connects.clone();
    for (index, connect) in module.connects().iter().enumerate() {
        if !live_connects[index] || connect.target.dynamic.is_none() {
            continue;
        }
        publication_connects[index] = true;
        root_values.push(connect.value);
        root_values.extend(connect.target.dynamic.map(|dynamic| dynamic.offset));
    }
    non_connect_root_values.sort_unstable();
    non_connect_root_values.dedup();
    root_values.sort_unstable();
    root_values.dedup();

    Ok(NetlistObservability {
        root_signals: root_signals.into_boxed_slice(),
        observed_signals: observed_signals.into_boxed_slice(),
        reachable_values: reachable_values.into_boxed_slice(),
        live_connects: live_connects.into_boxed_slice(),
        root_connects: root_connects.into_boxed_slice(),
        publication_connects: publication_connects.into_boxed_slice(),
        observed_memories: observed_memories.into_boxed_slice(),
        instance_root_values: instance_root_values.into_boxed_slice(),
        non_connect_root_values: non_connect_root_values.into_boxed_slice(),
        root_values: root_values.into_boxed_slice(),
    })
}

/// Enumerates every direct structural use of a Word IR value. This is the
/// shared edge definition for fanout, removable-region, and mapper reference
/// accounting; adding a new structural operand kind must update only here.
pub(crate) fn direct_value_uses(
    module: &word::WordModule,
) -> impl Iterator<Item = word::ValueId> + '_ {
    module
        .operations()
        .iter()
        .flat_map(|operation| crate::word::operation_inputs(&operation.kind))
        .chain(structural_value_uses(module))
}

/// Values referenced directly by module connections, child instances, or
/// memory ports. These references contribute use counts but do not define
/// global liveness; [`netlist_observability`] owns that root closure.
fn structural_value_uses(module: &word::WordModule) -> impl Iterator<Item = word::ValueId> + '_ {
    module
        .connects()
        .iter()
        .flat_map(|connect| {
            std::iter::once(connect.value)
                .chain(connect.target.dynamic.map(|dynamic| dynamic.offset))
        })
        .chain(
            module
                .instances()
                .iter()
                .flat_map(|instance| &instance.connections)
                .map(|connection| connection.value),
        )
        .chain(memory_port_values(module))
}

fn memory_port_values(module: &word::WordModule) -> impl Iterator<Item = word::ValueId> + '_ {
    module
        .memory_read_ports()
        .iter()
        .flat_map(memory_read_values)
        .chain(
            module
                .memory_write_ports()
                .iter()
                .flat_map(memory_write_values),
        )
}

fn memory_read_values(port: &word::MemoryReadPort) -> impl Iterator<Item = word::ValueId> {
    let (clock, enable) = match port.timing {
        word::MemoryReadTiming::Asynchronous => (None, None),
        word::MemoryReadTiming::Synchronous { clock, enable, .. } => {
            (Some(clock.value), enable.map(|enable| enable.value))
        }
    };
    [Some(port.address), clock, enable].into_iter().flatten()
}

fn memory_write_values(port: &word::MemoryWritePort) -> impl Iterator<Item = word::ValueId> {
    [
        Some(port.address),
        Some(port.data),
        Some(port.clock.value),
        port.enable.map(|enable| enable.value),
        port.mask.map(|mask| mask.value),
    ]
    .into_iter()
    .flatten()
}

pub(crate) fn value_use_counts(module: &word::WordModule) -> Result<Box<[u32]>, crate::SynthError> {
    let mut uses = vec![0u32; module.values().len()];
    for value in direct_value_uses(module) {
        increment_use_count(&mut uses, value)?;
    }
    Ok(uses.into_boxed_slice())
}

pub(crate) fn increment_use_count(
    uses: &mut [u32],
    value: word::ValueId,
) -> Result<(), crate::SynthError> {
    let count = uses.get_mut(value.index()).ok_or_else(|| {
        crate::SynthError::invariant(format!("use references unknown value {value:?}"))
    })?;
    *count = count
        .checked_add(1)
        .ok_or_else(|| crate::SynthError::capacity("value use count exceeds 32-bit capacity"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word::{
        Edge, LValue, PortDirection, RegisterOp, SourceSpan, WordModule, WordType,
    };
    use std::num::NonZeroU32;

    #[test]
    fn counts_dynamic_connect_offsets_as_structural_uses() {
        let mut module = WordModule::new("uses");
        let data_port = module
            .add_port(
                "data",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let offset_port = module
            .add_port(
                "offset",
                PortDirection::Input,
                WordType::bits(2).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let target_port = module
            .add_port(
                "target",
                PortDirection::Output,
                WordType::bits(4).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let target = module.port(target_port).unwrap().signal;
        let data = module
            .read_signal(
                module.port(data_port).unwrap().signal,
                SourceSpan::default(),
            )
            .unwrap();
        let offset = module
            .read_signal(
                module.port(offset_port).unwrap().signal,
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                LValue::signal(target).with_dynamic_range(offset, NonZeroU32::new(1).unwrap()),
                data,
                SourceSpan::default(),
            )
            .unwrap();

        let uses = value_use_counts(&module).unwrap();
        assert_eq!(uses[data.index()], 1);
        assert_eq!(uses[offset.index()], 1);
    }

    fn module_with_state(observe_state: bool) -> (WordModule, word::SignalId, word::ValueId) {
        let mut module = WordModule::new("state_observability");
        let bit = WordType::bits(1).unwrap();
        let clock = module
            .add_port("clock", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let data = module
            .add_port("data", PortDirection::Input, bit, SourceSpan::default())
            .unwrap();
        let clock = module
            .read_signal(module.port(clock).unwrap().signal, SourceSpan::default())
            .unwrap();
        let data = module
            .read_signal(module.port(data).unwrap().signal, SourceSpan::default())
            .unwrap();
        let register = module
            .register(
                RegisterOp {
                    name: None,
                    d: data,
                    clock,
                    edge: Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                SourceSpan::default(),
            )
            .unwrap();
        let state = module
            .add_wire("state", bit, SourceSpan::default())
            .unwrap();
        module
            .connect(LValue::signal(state), register, SourceSpan::default())
            .unwrap();
        if observe_state {
            let output = module
                .add_port("q", PortDirection::Output, bit, SourceSpan::default())
                .unwrap();
            let state_value = module.read_signal(state, SourceSpan::default()).unwrap();
            module
                .connect(
                    LValue::signal(module.port(output).unwrap().signal),
                    state_value,
                    SourceSpan::default(),
                )
                .unwrap();
        }
        (module, state, register)
    }

    #[test]
    fn retains_state_only_when_reached_from_an_observable_root() {
        let (dead, dead_state, dead_register) = module_with_state(false);
        let dead_observability = netlist_observability(&dead).unwrap();
        assert!(!dead_observability.observes_signal(dead_state).unwrap());
        assert!(!dead_observability.observes_value(dead_register).unwrap());

        let (live, live_state, live_register) = module_with_state(true);
        let live_observability = netlist_observability(&live).unwrap();
        assert!(live_observability.observes_signal(live_state).unwrap());
        assert!(live_observability.observes_value(live_register).unwrap());
        assert!(live_observability.observes_connect(0).unwrap());
        assert!(!live_observability.observes_root_connect(0).unwrap());
        assert!(live_observability.observes_root_connect(1).unwrap());
        assert!(!live_observability.observes_root_signal(live_state).unwrap());
        let output = live
            .ports()
            .iter()
            .find(|port| live.name_str(port.name) == "q")
            .unwrap();
        assert!(
            live_observability
                .observes_root_signal(output.signal)
                .unwrap()
        );
    }

    #[test]
    fn explicit_observation_keeps_only_its_dependency_cone() {
        let source = SourceSpan::default();
        let bit = WordType::bits(1).unwrap();
        let mut module = WordModule::new("explicit_observation");
        let input = module
            .add_port("a", PortDirection::Input, bit, source.clone())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, source.clone())
            .unwrap();
        let observed = module
            .unary(word::UnaryOp::BitNot, input, source.clone())
            .unwrap();
        let dead = module
            .unary(word::UnaryOp::BitNot, observed, source)
            .unwrap();

        let observability = netlist_observability_with_values(&module, &[observed]).unwrap();

        assert!(observability.observes_value(observed).unwrap());
        assert!(!observability.observes_value(dead).unwrap());
    }

    #[test]
    fn retains_memory_controls_only_when_read_data_is_observed() {
        let source = SourceSpan::default();
        let bit = WordType::bits(1).unwrap();
        let mut module = WordModule::new("memory_observability");
        let address = module
            .add_port("address", PortDirection::Input, bit, source.clone())
            .unwrap();
        let address = module
            .read_signal(module.port(address).unwrap().signal, source.clone())
            .unwrap();
        let memory = module
            .add_memory("memory", bit, NonZeroU32::new(2).unwrap(), source.clone())
            .unwrap();
        let read_data = module.add_wire("read_data", bit, source.clone()).unwrap();
        module
            .add_memory_read_port(word::MemoryReadPort {
                memory,
                address,
                data: read_data,
                timing: word::MemoryReadTiming::Asynchronous,
                read_during_write: word::ReadDuringWrite::OldData,
                source: source.clone(),
            })
            .unwrap();

        let dead = netlist_observability(&module).unwrap();
        assert!(!dead.observes_memory(memory).unwrap());
        assert!(!dead.observes_value(address).unwrap());

        let output = module
            .add_port("q", PortDirection::Output, bit, source.clone())
            .unwrap();
        let read = module.read_signal(read_data, source.clone()).unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                read,
                source,
            )
            .unwrap();

        let live = netlist_observability(&module).unwrap();
        assert!(live.observes_memory(memory).unwrap());
        assert!(live.observes_value(address).unwrap());
        assert!(live.non_connect_root_values().contains(&address));
    }

    #[test]
    fn dynamic_internal_connect_is_a_publication_root() {
        let source = SourceSpan::default();
        let bit = WordType::bits(1).unwrap();
        let pair = WordType::bits(2).unwrap();
        let mut module = WordModule::new("dynamic_publication");
        let data = module
            .add_port("data", PortDirection::Input, bit, source.clone())
            .unwrap();
        let offset = module
            .add_port("offset", PortDirection::Input, bit, source.clone())
            .unwrap();
        let data = module
            .read_signal(module.port(data).unwrap().signal, source.clone())
            .unwrap();
        let offset = module
            .read_signal(module.port(offset).unwrap().signal, source.clone())
            .unwrap();
        let internal = module.add_wire("internal", pair, source.clone()).unwrap();
        module
            .connect(
                LValue::signal(internal).with_dynamic_range(offset, NonZeroU32::new(1).unwrap()),
                data,
                source.clone(),
            )
            .unwrap();
        let output = module
            .add_port("q", PortDirection::Output, pair, source.clone())
            .unwrap();
        let internal = module.read_signal(internal, source.clone()).unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                internal,
                source,
            )
            .unwrap();

        let observability = netlist_observability(&module).unwrap();
        assert!(observability.observes_connect(0).unwrap());
        assert!(!observability.observes_root_connect(0).unwrap());
        assert!(observability.observes_publication_connect(0).unwrap());
        assert!(observability.root_values().contains(&data));
        assert!(observability.root_values().contains(&offset));
    }
}
