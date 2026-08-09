// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

pub(super) fn is_state(kind: &word::OpKind) -> bool {
    matches!(kind, word::OpKind::Register(_) | word::OpKind::Latch(_))
}

pub(super) fn operation_work(module: &word::WordModule, operation: &word::Operation) -> u64 {
    let width = module
        .value(operation.result)
        .map_or(1, |value| u64::from(value.ty.width()))
        .max(1);
    match operation.kind {
        word::OpKind::Binary {
            op: word::BinaryOp::Mul,
            ..
        } => width.saturating_mul(width),
        word::OpKind::Binary {
            op: word::BinaryOp::Div | word::BinaryOp::Mod,
            ..
        } => width.saturating_mul(width).saturating_mul(4),
        word::OpKind::Binary { .. } | word::OpKind::Mux { .. } => width.saturating_mul(2),
        word::OpKind::Register(_) | word::OpKind::Latch(_) => width.saturating_mul(4),
        _ => width,
    }
}

pub(super) fn memory_work(memory: &word::Memory, port_count: usize) -> u64 {
    let stored_bits =
        u64::from(memory.element_type.width()).saturating_mul(u64::from(memory.depth.get()));
    memory_port_expansion_work(stored_bits, port_count)
}

fn memory_port_expansion_work(stored_bits: u64, port_count: usize) -> u64 {
    let port_count = u64::try_from(port_count).unwrap_or(u64::MAX);
    // Register-bank lowering emits one word-level selection per logical port.
    // A scalar mux contributes two product terms and their combining node in
    // the Boolean primitive network, in addition to the stored state bits.
    stored_bits.saturating_add(stored_bits.saturating_mul(port_count).saturating_mul(3))
}

pub(super) fn memory_read_inputs(port: &word::MemoryReadPort) -> Vec<word::ValueId> {
    let mut values = vec![port.address];
    if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = port.timing {
        values.push(clock.value);
        if let Some(enable) = enable {
            values.push(enable.value);
        }
    }
    values
}

pub(super) fn memory_write_inputs(port: &word::MemoryWritePort) -> Vec<word::ValueId> {
    let mut values = vec![port.address, port.data, port.clock.value];
    if let Some(enable) = port.enable {
        values.push(enable.value);
    }
    if let Some(mask) = port.mask {
        values.push(mask.value);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::memory_port_expansion_work;

    #[test]
    fn memory_work_accounts_for_every_primitive_port_mux() {
        assert_eq!(memory_port_expansion_work(65_536, 17), 3_407_872);
    }
}
