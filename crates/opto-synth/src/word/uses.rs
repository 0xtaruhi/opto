// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

pub(crate) use opto_ir::word::netlist_observability;

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
