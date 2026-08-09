// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Read-only validation of shared scalar bindings on a memory macro contract.

use opto_ir::word;
use opto_library::TargetMemory;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
enum Binding {
    ValueBit(word::ValueId, u32),
    SignalBit(word::SignalId, u32),
}

pub(super) fn bindings_are_consistent<'a>(
    module: &word::WordModule,
    reads: impl Iterator<Item = &'a word::MemoryReadPort>,
    writes: impl Iterator<Item = &'a word::MemoryWritePort>,
    target: &TargetMemory,
) -> bool {
    let mut bindings = BTreeMap::new();
    for (source, port) in reads.zip(&target.read_ports) {
        if !bind_value_bits(module, &mut bindings, &port.address_pins, source.address)
            || !bind_signal_bits(&mut bindings, &port.data_pins, source.data)
        {
            return false;
        }
        if let (word::MemoryReadTiming::Synchronous { clock, enable, .. }, Some(target_clock)) =
            (source.timing, port.clock.as_ref())
        {
            if !bind(
                module,
                &mut bindings,
                &target_clock.pin,
                Binding::ValueBit(clock.value, 0),
            ) {
                return false;
            }
            if let (Some(enable), Some(target_enable)) = (enable, port.enable.as_ref())
                && !bind(
                    module,
                    &mut bindings,
                    &target_enable.pin,
                    Binding::ValueBit(enable.value, 0),
                )
            {
                return false;
            }
        }
    }
    for (source, port) in writes.zip(&target.write_ports) {
        if !bind_value_bits(module, &mut bindings, &port.address_pins, source.address)
            || !bind_value_bits(module, &mut bindings, &port.data_pins, source.data)
            || !bind(
                module,
                &mut bindings,
                &port.clock.pin,
                Binding::ValueBit(source.clock.value, 0),
            )
        {
            return false;
        }
        if let (Some(enable), Some(target_enable)) = (source.enable, port.enable.as_ref())
            && !bind(
                module,
                &mut bindings,
                &target_enable.pin,
                Binding::ValueBit(enable.value, 0),
            )
        {
            return false;
        }
        if let (Some(mask), false) = (source.mask, port.mask_pins.is_empty())
            && !bind_value_bits(module, &mut bindings, &port.mask_pins, mask.value)
        {
            return false;
        }
    }
    true
}

fn bind_value_bits<'a>(
    module: &word::WordModule,
    bindings: &mut BTreeMap<&'a str, Binding>,
    pins: &'a [String],
    value: word::ValueId,
) -> bool {
    pins.iter().enumerate().all(|(bit, pin)| {
        let Ok(bit) = u32::try_from(bit) else {
            return false;
        };
        bind(module, bindings, pin, Binding::ValueBit(value, bit))
    })
}

fn bind_signal_bits<'a>(
    bindings: &mut BTreeMap<&'a str, Binding>,
    pins: &'a [String],
    signal: word::SignalId,
) -> bool {
    pins.iter().enumerate().all(|(bit, pin)| {
        let Ok(bit) = u32::try_from(bit) else {
            return false;
        };
        let binding = Binding::SignalBit(signal, bit);
        if let Some(previous) = bindings.get(pin.as_str()).copied() {
            bindings_match_without_module(previous, binding)
        } else {
            bindings.insert(pin, binding);
            true
        }
    })
}

fn bind<'a>(
    module: &word::WordModule,
    bindings: &mut BTreeMap<&'a str, Binding>,
    pin: &'a str,
    binding: Binding,
) -> bool {
    if let Some(previous) = bindings.get(pin).copied() {
        bindings_match(module, previous, binding)
    } else {
        bindings.insert(pin, binding);
        true
    }
}

fn bindings_match(module: &word::WordModule, left: Binding, right: Binding) -> bool {
    match (left, right) {
        (Binding::ValueBit(left, left_bit), Binding::ValueBit(right, right_bit)) => {
            left_bit == right_bit
                && (left == right
                    || module
                        .value(left)
                        .zip(module.value(right))
                        .is_some_and(|(left, right)| {
                            left.ty == right.ty && left.kind == right.kind
                        }))
        }
        _ => bindings_match_without_module(left, right),
    }
}

fn bindings_match_without_module(left: Binding, right: Binding) -> bool {
    matches!(
        (left, right),
        (Binding::SignalBit(left, left_bit), Binding::SignalBit(right, right_bit))
            if left == right && left_bit == right_bit
    )
}
