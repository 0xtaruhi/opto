// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Stable semantic value hashing independent of Word arena insertion order.

use opto_ir::word;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub(crate) fn value_keys(module: &word::WordModule) -> Result<Vec<[u8; 32]>, crate::SynthError> {
    let mut keys = vec![[0; 32]; module.values().len()];
    let mut resolved = vec![false; module.values().len()];
    let mut operation_values = Vec::new();
    for (index, value) in module.values().iter().enumerate() {
        if matches!(value.kind, word::ValueKind::Operation(_)) {
            operation_values.push(index);
            continue;
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/synthesis-region/value/v1\0");
        super::append_type_hash(&mut digest, value.ty);
        match &value.kind {
            word::ValueKind::Signal(reference) => {
                digest.update(&[0]);
                let signal = module.signal(reference.signal).ok_or_else(|| {
                    crate::SynthError::invariant("value references an unknown signal")
                })?;
                match signal.name {
                    Some(name) => super::append_hash_text(&mut digest, module.name_str(name)),
                    None => super::append_hash_text(&mut digest, ""),
                }
                digest.update(&[match signal.kind {
                    word::SignalKind::Wire => 0,
                    word::SignalKind::Register => 1,
                    word::SignalKind::ProcessLocal => 2,
                    word::SignalKind::Port(port) => {
                        let direction = module.port(port).map(|port| port.direction);
                        match direction {
                            Some(word::PortDirection::Input) => 3,
                            Some(word::PortDirection::Output) => 4,
                            Some(word::PortDirection::Inout) => 5,
                            None => 6,
                        }
                    }
                }]);
                digest.update(&reference.lsb.to_le_bytes());
                digest.update(&reference.width().to_le_bytes());
            }
            word::ValueKind::Constant(bits) => {
                digest.update(&[1]);
                digest.update(&bits.width().to_le_bytes());
                for bit in bits.as_slice() {
                    digest.update(&[match bit {
                        opto_ir::BitVal::Zero => 0,
                        opto_ir::BitVal::One => 1,
                        opto_ir::BitVal::X => 2,
                        opto_ir::BitVal::Z => 3,
                    }]);
                }
            }
            word::ValueKind::Operation(_) => unreachable!("operations were deferred"),
        }
        keys[index] = *digest.finalize().as_bytes();
        resolved[index] = true;
    }

    let mut dependency_counts = vec![0u32; module.values().len()];
    let mut consumer_counts = vec![0usize; module.values().len()];
    for &index in &operation_values {
        let operation = operation_for_value(module, index)?;
        for input in crate::word::operation_inputs(&operation.kind) {
            let input_resolved = resolved.get(input.index()).copied().ok_or_else(|| {
                crate::SynthError::invariant("operation references an unknown value")
            })?;
            if input_resolved {
                continue;
            }
            dependency_counts[index] = dependency_counts[index]
                .checked_add(1)
                .ok_or_else(|| crate::SynthError::capacity("semantic value dependencies"))?;
            consumer_counts[input.index()] = consumer_counts[input.index()]
                .checked_add(1)
                .ok_or_else(|| crate::SynthError::capacity("semantic value consumers"))?;
        }
    }
    let mut consumer_offsets = Vec::with_capacity(module.values().len().saturating_add(1));
    consumer_offsets.push(0usize);
    for count in consumer_counts {
        consumer_offsets.push(
            consumer_offsets
                .last()
                .copied()
                .unwrap_or(0)
                .checked_add(count)
                .ok_or_else(|| crate::SynthError::capacity("semantic value consumer CSR"))?,
        );
    }
    let mut consumers = vec![0usize; consumer_offsets.last().copied().unwrap_or(0)];
    let mut cursors = consumer_offsets[..module.values().len()].to_vec();
    for &index in &operation_values {
        let operation = operation_for_value(module, index)?;
        for input in crate::word::operation_inputs(&operation.kind) {
            if resolved[input.index()] {
                continue;
            }
            let cursor = &mut cursors[input.index()];
            consumers[*cursor] = index;
            *cursor += 1;
        }
    }

    let mut ready = operation_values
        .iter()
        .copied()
        .filter(|&index| dependency_counts[index] == 0)
        .map(Reverse)
        .collect::<BinaryHeap<_>>();
    let mut resolved_operations = 0usize;
    while let Some(Reverse(index)) = ready.pop() {
        let value = &module.values()[index];
        let operation = operation_for_value(module, index)?;
        let mut digest = blake3::Hasher::new();
        digest.update(b"opto/synthesis-region/value/v1\0");
        super::append_type_hash(&mut digest, value.ty);
        digest.update(&[2]);
        super::append_operation_hash(module, operation, &keys, &mut digest)?;
        keys[index] = *digest.finalize().as_bytes();
        resolved[index] = true;
        resolved_operations += 1;
        for &consumer in &consumers[consumer_offsets[index]..consumer_offsets[index + 1]] {
            dependency_counts[consumer] =
                dependency_counts[consumer].checked_sub(1).ok_or_else(|| {
                    crate::SynthError::invariant("semantic dependency count underflow")
                })?;
            if dependency_counts[consumer] == 0 {
                ready.push(Reverse(consumer));
            }
        }
    }
    if resolved_operations != operation_values.len() {
        return Err(crate::SynthError::invalid(
            "combinational operation cycle prevents stable region identity",
        ));
    }
    Ok(keys)
}

fn operation_for_value(
    module: &word::WordModule,
    value_index: usize,
) -> Result<&word::Operation, crate::SynthError> {
    let value = module
        .values()
        .get(value_index)
        .ok_or_else(|| crate::SynthError::invariant("semantic value disappeared"))?;
    let word::ValueKind::Operation(operation) = value.kind else {
        return Err(crate::SynthError::invariant(
            "deferred semantic value is not an operation",
        ));
    };
    module
        .operation(operation)
        .ok_or_else(|| crate::SynthError::invariant("value references an unknown operation"))
}
