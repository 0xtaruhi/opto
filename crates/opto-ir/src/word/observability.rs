// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Externally observable closure of a word-level netlist.

use super::{SignalId, ValueId, ValueKind, WordError, WordModule};
use opto_core::PackedRows;

/// Signals and structural connections that can affect a module boundary.
///
/// State is traversed when its output is observed; merely declaring a register
/// or latch does not make that state observable. This distinction permits an
/// equivalent-state transform to discard a superseded state copy without
/// weakening the data, clock, enable, or reset cone of every live state bit.
#[derive(Debug)]
pub struct NetlistObservability {
    signals: Box<[bool]>,
    connects: Box<[bool]>,
    values: Box<[bool]>,
}

impl NetlistObservability {
    /// Returns whether `signal` can affect an externally observable sink.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `signal` is outside the module's signal arena.
    pub fn observes_signal(&self, signal: SignalId) -> Result<bool, WordError> {
        self.signals
            .get(signal.index())
            .copied()
            .ok_or_else(|| WordError::new("observability query references an unknown signal"))
    }

    /// Returns whether structural connection `index` can affect an observable sink.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `index` is outside the module's connection list.
    pub fn observes_connect(&self, index: usize) -> Result<bool, WordError> {
        self.connects
            .get(index)
            .copied()
            .ok_or_else(|| WordError::new("observability query references an unknown connection"))
    }

    /// Returns whether `value` can affect an observable sink.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `value` is outside the module's value arena.
    pub fn observes_value(&self, value: ValueId) -> Result<bool, WordError> {
        self.values
            .get(value.index())
            .copied()
            .ok_or_else(|| WordError::new("observability query references an unknown value"))
    }
}

/// Computes the externally observable signal and connection closure.
///
/// Output/inout ports, preserved signals, child-instance bindings, and memory
/// controls seed the walk. Reading a signal exposes its drivers; reaching a
/// state result then exposes that state's data and controls through the normal
/// operation-input relation. The walk is iterative and linear in the stored
/// graph and connection count.
///
/// # Errors
///
/// Returns [`WordError`] if a structural reference is outside its owning arena
/// or the packed connection index exceeds its compact capacity.
pub fn netlist_observability(module: &WordModule) -> Result<NetlistObservability, WordError> {
    let mut signal_offsets = Vec::with_capacity(module.signals().len() + 1);
    signal_offsets.push(0usize);
    for signal in module.signals() {
        let end = signal_offsets
            .last()
            .copied()
            .unwrap_or_default()
            .checked_add(signal.ty.width() as usize)
            .ok_or_else(|| WordError::new("observability signal-bit capacity overflow"))?;
        signal_offsets.push(end);
    }
    let bit_count = signal_offsets.last().copied().unwrap_or_default();
    let mut static_entries = Vec::new();
    let mut dynamic_entries = Vec::new();
    for (index, connect) in module.connects().iter().enumerate() {
        let signal = module.signal(connect.target.signal).ok_or_else(|| {
            WordError::new("observability connection references an unknown signal")
        })?;
        if connect.target.dynamic.is_some() {
            dynamic_entries.push((connect.target.signal.index(), index));
            continue;
        }
        let (lsb, width) = connect
            .target
            .range
            .map_or((0, signal.ty.width()), |range| {
                (range.lsb.min(range.msb), range.width())
            });
        let end = lsb
            .checked_add(width)
            .ok_or_else(|| WordError::new("observability connection range overflow"))?;
        if end > signal.ty.width() {
            return Err(WordError::new(
                "observability connection range exceeds its signal",
            ));
        }
        let offset = signal_offsets[connect.target.signal.index()];
        static_entries.extend((lsb..end).map(|bit| (offset + bit as usize, index)));
    }
    let connects_by_bit = PackedRows::try_from_entries(bit_count, static_entries)
        .map_err(|error| WordError::new(error.to_string()))?;
    let dynamic_connects_by_signal =
        PackedRows::try_from_entries(module.signals().len(), dynamic_entries)
            .map_err(|error| WordError::new(error.to_string()))?;
    let mut observed_connects = vec![false; module.connects().len()];
    let mut observed_signals = vec![false; module.signals().len()];
    let mut observed_signal_bits = vec![false; bit_count];
    let mut reachable_values = vec![false; module.values().len()];
    let mut pending_signals = module
        .ports()
        .iter()
        .filter(|port| {
            matches!(
                port.direction,
                super::PortDirection::Output | super::PortDirection::Inout
            )
        })
        .map(|port| {
            let signal = &module.signals()[port.signal.index()];
            (port.signal, 0u32, signal.ty.width())
        })
        .chain(module.preserved_signals().map(|signal| {
            let width = module.signals()[signal.index()].ty.width();
            (signal, 0u32, width)
        }))
        .collect::<Vec<_>>();
    let mut pending_values = module
        .instances()
        .iter()
        .flat_map(|instance| &instance.connections)
        .map(|connection| connection.value)
        .chain(memory_roots(module))
        .collect::<Vec<_>>();

    loop {
        while let Some((signal, lsb, width)) = pending_signals.pop() {
            let stored = module.signal(signal).ok_or_else(|| {
                WordError::new(format!(
                    "observability root references unknown signal {signal:?}"
                ))
            })?;
            let end = lsb
                .checked_add(width)
                .ok_or_else(|| WordError::new("observability signal range overflow"))?;
            if end > stored.ty.width() {
                return Err(WordError::new(
                    "observability signal range exceeds its signal",
                ));
            }
            observed_signals[signal.index()] = true;
            let signal_offset = signal_offsets[signal.index()];
            for bit in lsb..end {
                let bit_index = signal_offset + bit as usize;
                if std::mem::replace(&mut observed_signal_bits[bit_index], true) {
                    continue;
                }
                for &connect_index in connects_by_bit.row(bit_index) {
                    if !std::mem::replace(&mut observed_connects[connect_index], true) {
                        let connect = &module.connects()[connect_index];
                        pending_values.push(connect.value);
                    }
                }
                for &connect_index in dynamic_connects_by_signal.row(signal.index()) {
                    if !std::mem::replace(&mut observed_connects[connect_index], true) {
                        let connect = &module.connects()[connect_index];
                        pending_values.push(connect.value);
                        pending_values.extend(connect.target.dynamic.map(|dynamic| dynamic.offset));
                    }
                }
            }
        }

        let Some(value_id) = pending_values.pop() else {
            break;
        };
        let reachable = reachable_values.get_mut(value_id.index()).ok_or_else(|| {
            WordError::new(format!(
                "observability root references unknown value {value_id:?}"
            ))
        })?;
        if std::mem::replace(reachable, true) {
            continue;
        }
        let value = module
            .value(value_id)
            .ok_or_else(|| WordError::new(format!("observability walk lost value {value_id:?}")))?;
        match value.kind {
            ValueKind::Signal(reference) => {
                pending_signals.push((reference.signal, reference.lsb, reference.width()));
            }
            ValueKind::Operation(operation) => {
                let operation = module.operation(operation).ok_or_else(|| {
                    WordError::new(format!("observability walk lost operation {operation:?}"))
                })?;
                operation
                    .kind
                    .for_each_input(|input| pending_values.push(input));
            }
            ValueKind::Constant(_) => {}
        }
    }

    Ok(NetlistObservability {
        signals: observed_signals.into_boxed_slice(),
        connects: observed_connects.into_boxed_slice(),
        values: reachable_values.into_boxed_slice(),
    })
}

fn memory_roots(module: &WordModule) -> impl Iterator<Item = ValueId> + '_ {
    module
        .memory_read_ports()
        .iter()
        .flat_map(|port| {
            let (clock, enable) = match port.timing {
                super::MemoryReadTiming::Asynchronous => (None, None),
                super::MemoryReadTiming::Synchronous { clock, enable, .. } => {
                    (Some(clock.value), enable.map(|enable| enable.value))
                }
            };
            [Some(port.address), clock, enable].into_iter().flatten()
        })
        .chain(module.memory_write_ports().iter().flat_map(|port| {
            [
                Some(port.address),
                Some(port.data),
                Some(port.clock.value),
                port.enable.map(|enable| enable.value),
                port.mask.map(|mask| mask.value),
            ]
            .into_iter()
            .flatten()
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word::{Edge, LValue, PortDirection, RegisterOp, SourceSpan, WordType};

    #[test]
    fn state_is_observed_only_through_a_live_consumer() {
        let span = SourceSpan::default();
        let bit = WordType::bits(1).unwrap();
        let mut module = WordModule::new("state_liveness");
        let clock = module
            .add_port("clock", PortDirection::Input, bit, span.clone())
            .unwrap();
        let data = module
            .add_port("data", PortDirection::Input, bit, span.clone())
            .unwrap();
        let output = module
            .add_port("output", PortDirection::Output, bit, span.clone())
            .unwrap();
        let clock = module
            .read_signal(module.port(clock).unwrap().signal, span.clone())
            .unwrap();
        let data = module
            .read_signal(module.port(data).unwrap().signal, span.clone())
            .unwrap();
        let mut states = Vec::new();
        for name in ["live", "dead"] {
            let signal = module.add_wire(name, bit, span.clone()).unwrap();
            let state = module
                .register(
                    RegisterOp {
                        name: None,
                        d: data,
                        clock,
                        edge: Edge::Pos,
                        enable: None,
                        resets: Vec::new(),
                    },
                    span.clone(),
                )
                .unwrap();
            module
                .connect(LValue::signal(signal), state, span.clone())
                .unwrap();
            states.push(signal);
        }
        let live = module.read_signal(states[0], span.clone()).unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                live,
                span,
            )
            .unwrap();

        let observed = netlist_observability(&module).unwrap();
        assert!(observed.observes_signal(states[0]).unwrap());
        assert!(!observed.observes_signal(states[1]).unwrap());
    }
}
