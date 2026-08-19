// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Whole-module combinational dependency validation at the Word IR boundary.

use opto_ir::word;
use smallvec::SmallVec;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ValueSlice {
    value: word::ValueId,
    lsb: u32,
    width: u32,
}

impl ValueSlice {
    fn full(module: &word::WordModule, value: word::ValueId) -> Result<Self, crate::SynthError> {
        let width = module
            .value(value)
            .map(|stored| stored.ty.width())
            .ok_or_else(|| {
                crate::SynthError::invariant("combinational walk reached an unknown Word value")
            })?;
        Ok(Self {
            value,
            lsb: 0,
            width,
        })
    }

    fn end(self) -> Result<u32, crate::SynthError> {
        self.lsb
            .checked_add(self.width)
            .ok_or_else(|| crate::SynthError::invariant("combinational value slice overflow"))
    }
}

struct WalkFrame {
    selection: ValueSlice,
    dependencies: SmallVec<[ValueSlice; 4]>,
    next: usize,
}

/// Rejects combinational feedback immediately after procedural normalization.
///
/// Operations are topologically stored, but signal reads follow connect
/// drivers and can therefore close a cycle. Registers and latches terminate a
/// dependency walk. The iterative three-color traversal is linear in the
/// reachable Word graph and reports the exact value path with source spans.
pub(crate) fn validate_combinational_acyclic(
    module: &word::WordModule,
) -> Result<(), crate::SynthError> {
    let drivers = crate::word::signal_driver::SignalDriverIndex::new(module)?;
    let mut known_bits = word::KnownBitsAnalysis::new(module);
    let mut unsigned_values = word::UnsignedValueAnalysis::new(module);
    let mut state = HashMap::<ValueSlice, u8>::new();
    let mut stack = Vec::<WalkFrame>::new();
    for index in 0..module.values().len() {
        let value = word::ValueId::from_index(index).map_err(crate::SynthError::Word)?;
        let root = ValueSlice::full(module, value)?;
        if state.get(&root).copied().unwrap_or(0) != 0 {
            continue;
        }
        push_frame(
            module,
            &drivers,
            &mut known_bits,
            &mut unsigned_values,
            root,
            &mut state,
            &mut stack,
        )?;
        while let Some(frame) = stack.last_mut() {
            let Some(&dependency) = frame.dependencies.get(frame.next) else {
                let completed = stack.pop().ok_or_else(|| {
                    crate::SynthError::invariant("combinational walk lost its active frame")
                })?;
                state.insert(completed.selection, 2);
                continue;
            };
            frame.next += 1;
            let dependency_state = state.get(&dependency).copied().unwrap_or(0);
            match dependency_state {
                0 => push_frame(
                    module,
                    &drivers,
                    &mut known_bits,
                    &mut unsigned_values,
                    dependency,
                    &mut state,
                    &mut stack,
                )?,
                1 => {
                    let start = stack
                        .iter()
                        .position(|frame| frame.selection == dependency)
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "active combinational value is absent from the dependency stack",
                            )
                        })?;
                    let mut cycle = stack[start..]
                        .iter()
                        .map(|frame| frame.selection.value)
                        .collect::<Vec<_>>();
                    if let Some(operation) = cycle.iter().position(|&value| {
                        module.value(value).is_some_and(|stored| {
                            matches!(stored.kind, word::ValueKind::Operation(_))
                        })
                    }) {
                        cycle.rotate_left(operation);
                    }
                    let nodes = cycle
                        .iter()
                        .copied()
                        .map(|value| cycle_node(module, value))
                        .collect();
                    let debug_values = cycle
                        .iter()
                        .copied()
                        .chain(cycle.first().copied())
                        .collect();
                    return Err(crate::CombinationalCycle::after_normalization(
                        module.name(),
                        nodes,
                        debug_values,
                    )
                    .into());
                }
                2 => {}
                _ => {
                    return Err(crate::SynthError::invariant(
                        "combinational walk state is outside its three-color domain",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn push_frame(
    module: &word::WordModule,
    drivers: &crate::word::signal_driver::SignalDriverIndex,
    known_bits: &mut word::KnownBitsAnalysis,
    unsigned_values: &mut word::UnsignedValueAnalysis,
    selection: ValueSlice,
    state: &mut HashMap<ValueSlice, u8>,
    stack: &mut Vec<WalkFrame>,
) -> Result<(), crate::SynthError> {
    state.insert(selection, 1);
    stack.push(WalkFrame {
        selection,
        dependencies: dependencies(module, drivers, known_bits, unsigned_values, selection)?,
        next: 0,
    });
    Ok(())
}

fn dependencies(
    module: &word::WordModule,
    drivers: &crate::word::signal_driver::SignalDriverIndex,
    known_bits: &mut word::KnownBitsAnalysis,
    unsigned_values: &mut word::UnsignedValueAnalysis,
    selection: ValueSlice,
) -> Result<SmallVec<[ValueSlice; 4]>, crate::SynthError> {
    let stored = module.value(selection.value).ok_or_else(|| {
        crate::SynthError::invariant("combinational walk reached an unknown Word value")
    })?;
    if selection.width == 0 || selection.end()? > stored.ty.width() {
        return Err(crate::SynthError::invariant(
            "combinational walk reached an invalid value slice",
        ));
    }
    if (0..selection.width).all(|offset| {
        known_bits.bit(module, selection.value, selection.lsb + offset) != word::KnownBit::Unknown
    }) {
        return Ok(SmallVec::new());
    }
    Ok(match stored.kind {
        word::ValueKind::Constant(_) => SmallVec::new(),
        word::ValueKind::Signal(reference) => signal_dependencies(drivers, reference, selection)?,
        word::ValueKind::Operation(operation) => {
            let operation = module.operation(operation).ok_or_else(|| {
                crate::SynthError::invariant("combinational walk reached an unknown Word operation")
            })?;
            if matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            ) {
                SmallVec::new()
            } else {
                operation_dependencies(
                    module,
                    known_bits,
                    unsigned_values,
                    &operation.kind,
                    selection,
                )?
            }
        }
    })
}

fn signal_dependencies(
    drivers: &crate::word::signal_driver::SignalDriverIndex,
    reference: word::SignalRef,
    selection: ValueSlice,
) -> Result<SmallVec<[ValueSlice; 4]>, crate::SynthError> {
    let Some(bits) = drivers.resolve_reference(reference) else {
        return Ok(SmallVec::new());
    };
    let start = usize::try_from(selection.lsb)
        .map_err(|_| crate::SynthError::capacity("signal selection index"))?;
    let end = usize::try_from(selection.end()?)
        .map_err(|_| crate::SynthError::capacity("signal selection end"))?;
    let selected = bits.get(start..end).ok_or_else(|| {
        crate::SynthError::invariant("signal driver selection exceeds the resolved reference")
    })?;
    let mut dependencies = SmallVec::<[ValueSlice; 4]>::new();
    for &(value, bit) in selected {
        if let Some(previous) = dependencies.last_mut()
            && previous.value == value
            && previous.lsb.checked_add(previous.width) == Some(bit)
        {
            previous.width += 1;
        } else {
            dependencies.push(ValueSlice {
                value,
                lsb: bit,
                width: 1,
            });
        }
    }
    Ok(dependencies)
}

fn operation_dependencies(
    module: &word::WordModule,
    known_bits: &mut word::KnownBitsAnalysis,
    unsigned_values: &mut word::UnsignedValueAnalysis,
    operation: &word::OpKind,
    selection: ValueSlice,
) -> Result<SmallVec<[ValueSlice; 4]>, crate::SynthError> {
    let mut dependencies = SmallVec::new();
    match operation {
        word::OpKind::Unary {
            op: word::UnaryOp::BitNot,
            arg,
        } => push_slice(
            module,
            &mut dependencies,
            *arg,
            selection.lsb,
            selection.width,
        )?,
        word::OpKind::Unary { arg, .. } => push_full(module, &mut dependencies, *arg)?,
        word::OpKind::Binary {
            op: word::BinaryOp::BitAnd | word::BinaryOp::BitOr | word::BinaryOp::BitXor,
            left,
            right,
        } => {
            push_extended_slice(module, &mut dependencies, *left, selection)?;
            push_extended_slice(module, &mut dependencies, *right, selection)?;
        }
        word::OpKind::Binary { left, right, .. } => {
            push_full(module, &mut dependencies, *left)?;
            push_full(module, &mut dependencies, *right)?;
        }
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => match known_bits.bit(module, *cond, 0) {
            word::KnownBit::Zero => push_slice(
                module,
                &mut dependencies,
                *else_value,
                selection.lsb,
                selection.width,
            )?,
            word::KnownBit::One => push_slice(
                module,
                &mut dependencies,
                *then_value,
                selection.lsb,
                selection.width,
            )?,
            word::KnownBit::Unknown => {
                push_full(module, &mut dependencies, *cond)?;
                push_slice(
                    module,
                    &mut dependencies,
                    *then_value,
                    selection.lsb,
                    selection.width,
                )?;
                push_slice(
                    module,
                    &mut dependencies,
                    *else_value,
                    selection.lsb,
                    selection.width,
                )?;
            }
        },
        word::OpKind::TriState { data, enable } => {
            push_slice(
                module,
                &mut dependencies,
                *data,
                selection.lsb,
                selection.width,
            )?;
            push_full(module, &mut dependencies, enable.value)?;
        }
        word::OpKind::Concat { parts } => {
            let mut part_lsb = 0u32;
            for &part in parts.iter().rev() {
                let width = value_width(module, part)?;
                let part_end = part_lsb.checked_add(width).ok_or_else(|| {
                    crate::SynthError::invariant("concatenation dependency width overflow")
                })?;
                let overlap_lsb = selection.lsb.max(part_lsb);
                let overlap_end = selection.end()?.min(part_end);
                if overlap_lsb < overlap_end {
                    push_slice(
                        module,
                        &mut dependencies,
                        part,
                        overlap_lsb - part_lsb,
                        overlap_end - overlap_lsb,
                    )?;
                }
                part_lsb = part_end;
            }
        }
        word::OpKind::Extract { value, lsb, .. } => push_slice(
            module,
            &mut dependencies,
            *value,
            lsb.checked_add(selection.lsb).ok_or_else(|| {
                crate::SynthError::invariant("extract dependency offset overflow")
            })?,
            selection.width,
        )?,
        word::OpKind::Cast { kind, value, .. } => {
            push_cast_slice(module, &mut dependencies, *kind, *value, selection)?;
        }
        word::OpKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            if let Some(offset) = crate::word::known_u32(module, known_bits, *offset) {
                let input_width = value_width(module, *value)?;
                if offset
                    .checked_add(width.get())
                    .is_some_and(|end| end <= input_width)
                {
                    push_slice(
                        module,
                        &mut dependencies,
                        *value,
                        offset.checked_add(selection.lsb).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "dynamic extract dependency offset overflow",
                            )
                        })?,
                        selection.width,
                    )?;
                }
            } else if let Some(crate::word::ScaledDynamicOffset {
                scale,
                maximum_selector,
                ..
            }) = crate::word::scaled_dynamic_offset(module, known_bits, *offset)
            {
                push_full(module, &mut dependencies, *offset)?;
                let input_width = value_width(module, *value)?;
                let available_offsets = input_width.checked_sub(width.get()).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "dynamic extract dependency width exceeds its input",
                    )
                })?;
                let mut selector = 0u128;
                while selector <= maximum_selector {
                    let candidate = selector.checked_mul(scale).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "scaled dynamic extract dependency offset overflow",
                        )
                    })?;
                    if candidate > u128::from(available_offsets) {
                        break;
                    }
                    let candidate = u32::try_from(candidate).map_err(|_| {
                        crate::SynthError::capacity("scaled dynamic extract dependency offset")
                    })?;
                    push_slice(
                        module,
                        &mut dependencies,
                        *value,
                        candidate.checked_add(selection.lsb).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "scaled dynamic extract selected bit overflow",
                            )
                        })?,
                        selection.width,
                    )?;
                    selector += 1;
                }
            } else if let Some(range) = unsigned_values.range(module, *offset) {
                push_full(module, &mut dependencies, *offset)?;
                let input_width = value_width(module, *value)?;
                let available_offsets = input_width.checked_sub(width.get()).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "dynamic extract dependency width exceeds its input",
                    )
                })?;
                let first = range.minimum();
                let last = range.maximum().min(u128::from(available_offsets));
                if first <= last {
                    let lsb = first
                        .checked_add(u128::from(selection.lsb))
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| {
                            crate::SynthError::capacity("dynamic extract dependency offset")
                        })?;
                    let end = last
                        .checked_add(u128::from(selection.end()?))
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| {
                            crate::SynthError::capacity("dynamic extract dependency end")
                        })?;
                    push_slice(module, &mut dependencies, *value, lsb, end - lsb)?;
                }
            } else {
                push_full(module, &mut dependencies, *value)?;
                push_full(module, &mut dependencies, *offset)?;
            }
        }
        word::OpKind::DynamicInsert {
            value,
            offset,
            replacement,
        } => {
            push_full(module, &mut dependencies, *value)?;
            push_full(module, &mut dependencies, *offset)?;
            push_full(module, &mut dependencies, *replacement)?;
        }
        word::OpKind::Register(_) | word::OpKind::Latch(_) => {}
    }
    Ok(dependencies)
}

fn push_extended_slice(
    module: &word::WordModule,
    dependencies: &mut SmallVec<[ValueSlice; 4]>,
    value: word::ValueId,
    selection: ValueSlice,
) -> Result<(), crate::SynthError> {
    let width = value_width(module, value)?;
    let low_end = selection.end()?.min(width);
    if selection.lsb < low_end {
        push_slice(
            module,
            dependencies,
            value,
            selection.lsb,
            low_end - selection.lsb,
        )?;
    }
    if selection.end()? > width
        && module
            .value(value)
            .is_some_and(|stored| stored.ty.is_signed())
    {
        push_slice(module, dependencies, value, width - 1, 1)?;
    }
    Ok(())
}

fn push_cast_slice(
    module: &word::WordModule,
    dependencies: &mut SmallVec<[ValueSlice; 4]>,
    kind: word::CastKind,
    value: word::ValueId,
    selection: ValueSlice,
) -> Result<(), crate::SynthError> {
    let width = value_width(module, value)?;
    let low_end = selection.end()?.min(width);
    if selection.lsb < low_end {
        push_slice(
            module,
            dependencies,
            value,
            selection.lsb,
            low_end - selection.lsb,
        )?;
    }
    if kind == word::CastKind::SignExtend && selection.end()? > width {
        push_slice(module, dependencies, value, width - 1, 1)?;
    }
    Ok(())
}

fn push_full(
    module: &word::WordModule,
    dependencies: &mut SmallVec<[ValueSlice; 4]>,
    value: word::ValueId,
) -> Result<(), crate::SynthError> {
    dependencies.push(ValueSlice::full(module, value)?);
    Ok(())
}

fn push_slice(
    module: &word::WordModule,
    dependencies: &mut SmallVec<[ValueSlice; 4]>,
    value: word::ValueId,
    lsb: u32,
    width: u32,
) -> Result<(), crate::SynthError> {
    let value_width = value_width(module, value)?;
    let end = lsb
        .checked_add(width)
        .ok_or_else(|| crate::SynthError::invariant("dependency slice overflow"))?;
    if width == 0 || end > value_width {
        return Err(crate::SynthError::invariant(
            "operation dependency exceeds its input value",
        ));
    }
    let dependency = ValueSlice { value, lsb, width };
    if !dependencies.contains(&dependency) {
        dependencies.push(dependency);
    }
    Ok(())
}

fn value_width(module: &word::WordModule, value: word::ValueId) -> Result<u32, crate::SynthError> {
    module
        .value(value)
        .map(|stored| stored.ty.width())
        .ok_or_else(|| {
            crate::SynthError::invariant("operation dependency references an unknown Word value")
        })
}

pub(crate) fn cycle_node(
    module: &word::WordModule,
    value: word::ValueId,
) -> crate::CombinationalCycleNode {
    let Some(stored) = module.value(value) else {
        return crate::CombinationalCycleNode::new(
            "an unknown generated value",
            word::SourceSpan::default(),
        );
    };
    match stored.kind {
        word::ValueKind::Operation(operation) => module.operation(operation).map_or_else(
            || {
                crate::CombinationalCycleNode::new(
                    "an unknown generated operation",
                    stored.source.clone(),
                )
            },
            |operation| {
                crate::CombinationalCycleNode::new(
                    operation_description(&operation.kind),
                    operation.source.clone(),
                )
            },
        ),
        word::ValueKind::Signal(reference) => {
            let description = module.signal(reference.signal).map_or_else(
                || "an unknown signal".to_string(),
                |signal| {
                    let name = signal
                        .name
                        .and_then(|name| module.resolve_name(name))
                        .unwrap_or("<generated>");
                    if reference.lsb == 0 && reference.width() == signal.ty.width() {
                        format!("signal '{name}'")
                    } else if reference.width() == 1 {
                        format!("signal bit '{name}[{}]'", reference.lsb)
                    } else {
                        let msb = reference.lsb + reference.width() - 1;
                        format!("signal slice '{name}[{msb}:{}]'", reference.lsb)
                    }
                },
            );
            crate::CombinationalCycleNode::new(description, stored.source.clone())
        }
        word::ValueKind::Constant(ref bits) => crate::CombinationalCycleNode::new(
            format!("constant value {bits:?}"),
            stored.source.clone(),
        ),
    }
}

fn operation_description(operation: &word::OpKind) -> &'static str {
    match operation {
        word::OpKind::Unary { op, .. } => match op {
            word::UnaryOp::LogicalNot => "a logical NOT expression",
            word::UnaryOp::BitNot => "a bitwise NOT expression",
            word::UnaryOp::ReductionAnd => "an AND reduction",
            word::UnaryOp::ReductionOr => "an OR reduction",
            word::UnaryOp::ReductionXor => "an XOR reduction",
        },
        word::OpKind::Binary { op, .. } => match op {
            word::BinaryOp::Add => "an addition expression",
            word::BinaryOp::Sub => "a subtraction expression",
            word::BinaryOp::Mul => "a multiplication expression",
            word::BinaryOp::Div => "a division expression",
            word::BinaryOp::Mod => "a remainder expression",
            word::BinaryOp::BitAnd => "a bitwise AND expression",
            word::BinaryOp::BitOr => "a bitwise OR expression",
            word::BinaryOp::BitXor => "a bitwise XOR expression",
            word::BinaryOp::LogicalAnd => "a logical AND expression",
            word::BinaryOp::LogicalOr => "a logical OR expression",
            word::BinaryOp::Eq => "an equality comparison",
            word::BinaryOp::Ne => "an inequality comparison",
            word::BinaryOp::Lt => "a less-than comparison",
            word::BinaryOp::Le => "a less-than-or-equal comparison",
            word::BinaryOp::Gt => "a greater-than comparison",
            word::BinaryOp::Ge => "a greater-than-or-equal comparison",
            word::BinaryOp::Shl => "a left-shift expression",
            word::BinaryOp::Shr => "a logical right-shift expression",
            word::BinaryOp::Ashr => "an arithmetic right-shift expression",
        },
        word::OpKind::Mux { .. } => "a conditional selection",
        word::OpKind::TriState { .. } => "a tri-state driver",
        word::OpKind::Concat { .. } => "a concatenation",
        word::OpKind::Extract { .. } => "a static bit selection",
        word::OpKind::DynamicExtract { .. } => "a dynamic bit selection",
        word::OpKind::DynamicInsert { .. } => "a dynamic bit assignment",
        word::OpKind::Cast { .. } => "a width conversion",
        word::OpKind::Register(_) => "a register",
        word::OpKind::Latch(_) => "a latch",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_signal_driver_cycle_before_region_construction() {
        let mut module = word::WordModule::new("feedback");
        let bit = word::WordType::bits(1).unwrap();
        let left = module
            .add_wire("left", bit, word::SourceSpan::default())
            .unwrap();
        let right = module
            .add_wire("right", bit, word::SourceSpan::default())
            .unwrap();
        let left_value = module
            .read_signal(left, word::SourceSpan::default())
            .unwrap();
        let right_value = module
            .read_signal(right, word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(left),
                right_value,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(right),
                left_value,
                word::SourceSpan::default(),
            )
            .unwrap();

        let error = validate_combinational_acyclic(&module).unwrap_err();
        let crate::SynthError::CombinationalCycle(cycle) = error else {
            panic!("unexpected error kind");
        };
        assert_eq!(cycle.region(), None);
        assert_eq!(cycle.debug_values(), &[left_value, right_value, left_value]);
        assert_eq!(
            cycle.path_description(),
            "signal 'left' -> signal 'right' -> signal 'left'"
        );
    }

    #[test]
    fn sequential_state_terminates_a_feedback_walk() {
        let mut module = word::WordModule::new("state");
        let bit = word::WordType::bits(1).unwrap();
        let clock_port = module
            .add_port(
                "clock",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let q = module
            .add_wire("q", bit, word::SourceSpan::default())
            .unwrap();
        let clock = module
            .read_signal(
                module.port(clock_port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let feedback = module.read_signal(q, word::SourceSpan::default()).unwrap();
        let state = module
            .register(
                word::RegisterOp {
                    name: None,
                    d: feedback,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(word::LValue::signal(q), state, word::SourceSpan::default())
            .unwrap();

        validate_combinational_acyclic(&module).unwrap();
    }

    #[test]
    fn controlling_constant_terminates_a_false_feedback_walk() {
        let mut module = word::WordModule::new("constant_feedback");
        let bit = word::WordType::bits(1).unwrap();
        let signal = module
            .add_wire("feedback", bit, word::SourceSpan::default())
            .unwrap();
        let feedback = module
            .read_signal(signal, word::SourceSpan::default())
            .unwrap();
        let disabled = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let value = module
            .binary(
                word::BinaryOp::BitAnd,
                disabled,
                feedback,
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(
                word::LValue::signal(signal),
                value,
                word::SourceSpan::default(),
            )
            .unwrap();

        validate_combinational_acyclic(&module).unwrap();
    }

    #[test]
    fn disjoint_packed_fields_do_not_form_a_feedback_cycle() {
        let mut module = word::WordModule::new("packed_fields");
        let bit = word::WordType::bits(1).unwrap();
        let pair = word::WordType::bits(2).unwrap();
        let address_port = module
            .add_port(
                "address",
                word::PortDirection::Input,
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let request = module
            .add_wire("request", pair, word::SourceSpan::default())
            .unwrap();
        let old_address = module
            .read_signal_slice(request, 0, 1, word::SourceSpan::default())
            .unwrap();
        let flag = module
            .unary(
                word::UnaryOp::LogicalNot,
                old_address,
                word::SourceSpan::default(),
            )
            .unwrap();
        let new_address = module
            .read_signal(
                module.port(address_port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let assembled = module
            .concat(vec![flag, new_address], word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(request),
                assembled,
                word::SourceSpan::default(),
            )
            .unwrap();

        validate_combinational_acyclic(&module).unwrap();
    }

    #[test]
    fn scaled_dynamic_record_selection_keeps_fields_disjoint() {
        let mut module = word::WordModule::new("dynamic_packed_fields");
        let selector_ty = word::WordType::bits(2).unwrap();
        let offset_ty = word::WordType::bits(3).unwrap();
        let record_pair = word::WordType::bits(4).unwrap();
        let selector_port = module
            .add_port(
                "selector",
                word::PortDirection::Input,
                selector_ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let data_port = module
            .add_port(
                "data",
                word::PortDirection::Input,
                word::WordType::bits(3).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        let records = module
            .add_wire("records", record_pair, word::SourceSpan::default())
            .unwrap();
        let selector = module
            .read_signal(
                module.port(selector_port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        let selector = module
            .cast(
                word::CastKind::ZeroExtend,
                selector,
                offset_ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let scale = module
            .constant(
                opto_ir::ConstBits::from_bin_str("010").unwrap(),
                offset_ty,
                word::SourceSpan::default(),
            )
            .unwrap();
        let offset = module
            .binary(
                word::BinaryOp::Mul,
                selector,
                scale,
                word::SourceSpan::default(),
            )
            .unwrap();
        let records_value = module
            .read_signal(records, word::SourceSpan::default())
            .unwrap();
        let selected = module
            .dynamic_extract(records_value, offset, 2, word::SourceSpan::default())
            .unwrap();
        let selected_low = module
            .extract(selected, 0, 1, word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(records).with_range(word::BitRange { msb: 1, lsb: 1 }),
                selected_low,
                word::SourceSpan::default(),
            )
            .unwrap();
        let data = module
            .read_signal(
                module.port(data_port).unwrap().signal,
                word::SourceSpan::default(),
            )
            .unwrap();
        for (source_bit, target_bit) in [(0, 0), (1, 2), (2, 3)] {
            let value = module
                .extract(data, source_bit, 1, word::SourceSpan::default())
                .unwrap();
            module
                .connect(
                    word::LValue::signal(records).with_range(word::BitRange {
                        msb: target_bit,
                        lsb: target_bit,
                    }),
                    value,
                    word::SourceSpan::default(),
                )
                .unwrap();
        }

        validate_combinational_acyclic(&module).unwrap();
    }

    #[test]
    fn feedback_within_one_packed_field_is_rejected() {
        let mut module = word::WordModule::new("packed_field_feedback");
        let bit = word::WordType::bits(1).unwrap();
        let pair = word::WordType::bits(2).unwrap();
        let request = module
            .add_wire("request", pair, word::SourceSpan::default())
            .unwrap();
        let feedback = module
            .read_signal_slice(request, 0, 1, word::SourceSpan::default())
            .unwrap();
        let high = module
            .constant(
                opto_ir::ConstBits::from_bin_str("0").unwrap(),
                bit,
                word::SourceSpan::default(),
            )
            .unwrap();
        let assembled = module
            .concat(vec![high, feedback], word::SourceSpan::default())
            .unwrap();
        module
            .connect(
                word::LValue::signal(request),
                assembled,
                word::SourceSpan::default(),
            )
            .unwrap();

        assert!(matches!(
            validate_combinational_acyclic(&module),
            Err(crate::SynthError::CombinationalCycle(_))
        ));
    }
}
