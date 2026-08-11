// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

pub(crate) struct NetlistObservability {
    observed_signals: Box<[bool]>,
    reachable_values: Box<[bool]>,
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
}

/// Computes the externally observable signal/connection closure.
///
/// Ports, preserved signals, child-instance bindings, memory controls, and
/// state-holding operations seed the walk. Reading a signal makes its driver
/// observable, and the driver's operands can expose further signal reads.
/// The packed signal-to-connect index keeps the closure linear in netlist size.
pub(crate) fn netlist_observability(
    module: &word::WordModule,
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
    let mut live_connects = vec![false; module.connects().len()];
    let mut observed_signals = vec![false; module.signals().len()];
    let mut reachable_values = vec![false; module.values().len()];
    let mut pending_signals = module
        .ports()
        .iter()
        .map(|port| port.signal)
        .chain(module.preserved_signals())
        .collect::<Vec<_>>();
    let mut pending_values = module
        .instances()
        .iter()
        .flat_map(|instance| &instance.connections)
        .map(|connection| connection.value)
        .chain(memory_roots(module))
        .collect::<Vec<_>>();

    for (index, connect) in module.connects().iter().enumerate() {
        if value_is_storage(module, connect.value) {
            live_connects[index] = true;
            pending_values.push(connect.value);
            pending_values.extend(connect.target.dynamic.map(|dynamic| dynamic.offset));
        }
    }

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

    Ok(NetlistObservability {
        observed_signals: observed_signals.into_boxed_slice(),
        reachable_values: reachable_values.into_boxed_slice(),
    })
}

fn value_is_storage(module: &word::WordModule, value: word::ValueId) -> bool {
    matches!(
        module.value(value).map(|value| &value.kind),
        Some(word::ValueKind::Operation(operation))
            if module.operation(*operation).is_some_and(|operation| {
                matches!(
                    operation.kind,
                    word::OpKind::Register(_) | word::OpKind::Latch(_)
                )
            })
    )
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
        .chain(structural_roots(module))
}

/// Values observed directly by module connections or child instances. Cone
/// reachability analyses start here; dynamic lvalue offsets and memory-port
/// operands are structural roots just like connection data.
pub(crate) fn structural_roots(
    module: &word::WordModule,
) -> impl Iterator<Item = word::ValueId> + '_ {
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
        .chain(memory_roots(module))
}

pub(crate) fn memory_roots(module: &word::WordModule) -> impl Iterator<Item = word::ValueId> + '_ {
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
    use opto_ir::word::{LValue, PortDirection, SourceSpan, WordModule, WordType};
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
}
