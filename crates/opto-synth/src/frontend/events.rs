// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::state::Assignment;
use opto_ir::{BitVal, proc, word};
use std::collections::HashSet;

type BooleanControl = (word::SignalRef, bool);
type OwnedEvent = (proc::EventId, proc::SensitivityEvent);
type PartitionedEvents = (OwnedEvent, Vec<OwnedEvent>);

pub(super) fn dual_edge_clock<'a>(
    module: &word::WordModule,
    events: impl IntoIterator<Item = &'a proc::SensitivityEvent>,
) -> Option<word::ValueId> {
    let mut events = events.into_iter();
    let first = *events.next()?;
    let second = *events.next()?;
    if events.next().is_some()
        || !same_value(module, first.value, second.value)
        || first.edge == second.edge
    {
        return None;
    }
    Some(first.value)
}

pub(super) fn resolve_flop_events(
    module: &mut word::WordModule,
    events: &[OwnedEvent],
    assignments: &mut [Assignment],
) -> Result<OwnedEvent, crate::SynthError> {
    if let [clock] = events {
        return Ok(*clock);
    }
    let first_resets = assignments
        .iter()
        .find_map(|assignment| {
            (!assignment.resets.is_empty()).then_some(assignment.resets.as_slice())
        })
        .ok_or_else(|| {
            crate::SynthError::invalid(
                "multi-edge always_ff has no recognizable asynchronous reset",
            )
        })?;
    let (clock, async_events) = partition_clock_and_async_controls(module, events, first_resets)?;
    let mut expected_controls = async_events
        .iter()
        .map(|(_, event)| event_control(module, event))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            crate::SynthError::unsupported(
                "iff-qualified or computed asynchronous sensitivity events cannot be represented by the register reset model",
            )
        })?;
    expected_controls.sort_unstable();
    expected_controls.dedup();
    let canonical_control = match async_events.as_slice() {
        [(_, event)] => Some((event.value, event.edge == word::Edge::Pos)),
        _ => None,
    };
    for assignment in assignments {
        if assignment.resets.is_empty() {
            if async_events
                .iter()
                .all(|(event_id, _)| assignment.holds_on(*event_id))
            {
                continue;
            }
            return Err(crate::SynthError::unsupported(format!(
                "multi-edge always_ff target '{}' can update during a non-clock sensitivity event",
                assignment.target_name(module)
            )));
        }
        let Some(async_count) =
            matching_async_prefix(module, &assignment.resets, &expected_controls)?
        else {
            return Err(crate::SynthError::invalid(
                "asynchronous always_ff uses inconsistent reset conditions",
            ));
        };
        for (index, reset) in assignment.resets.iter_mut().enumerate() {
            reset.kind = if index < async_count {
                word::ResetKind::Async
            } else {
                word::ResetKind::Sync
            };
        }
        if async_count == 1
            && let Some((value, active_high)) = canonical_control
        {
            assignment.resets[0].value = value;
            assignment.resets[0].active_high = active_high;
        }
    }
    Ok(clock)
}

fn partition_clock_and_async_controls(
    module: &word::WordModule,
    events: &[OwnedEvent],
    resets: &[word::Reset],
) -> Result<PartitionedEvents, crate::SynthError> {
    let mut matches = Vec::new();
    for (clock_index, &clock) in events.iter().enumerate() {
        let async_events = events
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != clock_index)
            .map(|(_, event)| *event)
            .collect::<Vec<_>>();
        let Some(mut controls) = async_events
            .iter()
            .map(|(_, event)| event_control(module, event))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        controls.sort_unstable();
        controls.dedup();
        if matching_async_prefix(module, resets, &controls)?.is_some() {
            matches.push((clock, async_events));
        }
    }
    match <[_; 1]>::try_from(matches) {
        Ok([single]) => Ok(single),
        Err(matches) if matches.is_empty() => {
            let reset_controls = async_reset_controls(module, resets)?
                .into_iter()
                .map(|control| format_control(module, control))
                .collect::<Vec<_>>()
                .join(", ");
            let sensitivity_events = events
                .iter()
                .map(|(_, event)| format_event(module, event))
                .collect::<Vec<_>>()
                .join(", ");
            Err(crate::SynthError::invalid(format!(
                "asynchronous reset controls [{reset_controls}] do not match sensitivity events [{sensitivity_events}]"
            )))
        }
        Err(_) => Err(crate::SynthError::invalid(
            "multi-edge always_ff has an ambiguous clock event",
        )),
    }
}

fn format_control(module: &word::WordModule, (reference, active_high): BooleanControl) -> String {
    let name = module
        .signal(reference.signal)
        .and_then(|signal| signal.name)
        .map_or("<unnamed>", |name| module.name_str(name));
    let edge = if active_high { "posedge" } else { "negedge" };
    if module
        .signal(reference.signal)
        .is_some_and(|signal| signal.ty.width() == 1)
    {
        format!("{edge} {name}")
    } else {
        format!("{edge} {name}[{}]", reference.lsb)
    }
}

fn format_event(module: &word::WordModule, event: &proc::SensitivityEvent) -> String {
    event_control(module, event).map_or_else(
        || format!("{:?} expression {:?}", event.edge, event.value),
        |control| format_control(module, control),
    )
}

fn matching_async_prefix(
    module: &word::WordModule,
    resets: &[word::Reset],
    expected: &[BooleanControl],
) -> Result<Option<usize>, crate::SynthError> {
    for count in 1..=resets.len() {
        if async_reset_controls(module, &resets[..count])? == expected {
            return Ok(Some(count));
        }
    }
    Ok(None)
}

pub(super) fn async_reset_controls(
    module: &word::WordModule,
    resets: &[word::Reset],
) -> Result<Vec<BooleanControl>, crate::SynthError> {
    let mut controls = Vec::new();
    for reset in resets {
        collect_async_reset_controls(module, reset.value, reset.active_high, &mut controls)?;
    }
    controls.sort_unstable();
    controls.dedup();
    Ok(controls)
}

fn collect_async_reset_controls(
    module: &word::WordModule,
    value: word::ValueId,
    asserted_when_high: bool,
    controls: &mut Vec<BooleanControl>,
) -> Result<(), crate::SynthError> {
    if asserted_when_high
        && let Some(operation) = module.value(value).and_then(|value| match value.kind {
            word::ValueKind::Operation(operation) => module.operation(operation),
            word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
        })
        && let word::OpKind::Binary {
            op: word::BinaryOp::LogicalOr | word::BinaryOp::BitOr,
            left,
            right,
        } = operation.kind
    {
        collect_async_reset_controls(module, left, true, controls)?;
        collect_async_reset_controls(module, right, true, controls)?;
        return Ok(());
    }
    let control = normalize_boolean_value(module, value, asserted_when_high).ok_or_else(|| {
        crate::SynthError::invalid(
            "asynchronous reset condition is not an OR of scalar event signals",
        )
    })?;
    controls.push(control);
    Ok(())
}

pub(super) fn normalize_boolean_value(
    module: &word::WordModule,
    value: word::ValueId,
    asserted_when_high: bool,
) -> Option<BooleanControl> {
    let value = module.value(value)?;
    if value.ty.width() != 1 {
        return None;
    }
    match &value.kind {
        word::ValueKind::Signal(reference) if reference.width() == 1 => {
            Some((*reference, asserted_when_high))
        }
        word::ValueKind::Operation(operation) => {
            let operation = module.operation(*operation)?;
            match &operation.kind {
                word::OpKind::Unary {
                    op: word::UnaryOp::LogicalNot | word::UnaryOp::BitNot,
                    arg,
                } => normalize_boolean_value(module, *arg, !asserted_when_high),
                word::OpKind::Cast { value, .. } => {
                    normalize_boolean_value(module, *value, asserted_when_high)
                }
                word::OpKind::Binary { op, left, right }
                    if matches!(op, word::BinaryOp::Eq | word::BinaryOp::Ne) =>
                {
                    normalize_comparison(module, *op, *left, *right, asserted_when_high)
                }
                _ => None,
            }
        }
        word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => None,
    }
}

fn normalize_comparison(
    module: &word::WordModule,
    op: word::BinaryOp,
    left: word::ValueId,
    right: word::ValueId,
    asserted_when_high: bool,
) -> Option<BooleanControl> {
    let (value, constant) = scalar_constant(module, right)
        .map(|constant| (left, constant))
        .or_else(|| scalar_constant(module, left).map(|constant| (right, constant)))?;
    let equality_asserts_high = if op == word::BinaryOp::Eq {
        constant
    } else {
        !constant
    };
    normalize_boolean_value(
        module,
        value,
        if asserted_when_high {
            equality_asserts_high
        } else {
            !equality_asserts_high
        },
    )
}

fn event_control(
    module: &word::WordModule,
    event: &proc::SensitivityEvent,
) -> Option<BooleanControl> {
    if event.iff.is_some() {
        return None;
    }
    normalize_boolean_value(module, event.value, event.edge == word::Edge::Pos)
}

#[allow(
    clippy::too_many_lines,
    reason = "event-clock equality compares the complete supported combinational Word shape without allocating canonical clones"
)]
pub(super) fn same_value(
    module: &word::WordModule,
    left: word::ValueId,
    right: word::ValueId,
) -> bool {
    let mut pending = vec![(left, right)];
    let mut visited = HashSet::new();
    while let Some((left, right)) = pending.pop() {
        if left == right || !visited.insert((left, right)) {
            continue;
        }
        let Some((left, right)) = module.value(left).zip(module.value(right)) else {
            return false;
        };
        if left.ty != right.ty {
            return false;
        }
        match (&left.kind, &right.kind) {
            (word::ValueKind::Signal(left), word::ValueKind::Signal(right)) if left == right => {}
            (word::ValueKind::Constant(left), word::ValueKind::Constant(right))
                if left == right => {}
            (word::ValueKind::Operation(left), word::ValueKind::Operation(right)) => {
                let Some((left, right)) = module.operation(*left).zip(module.operation(*right))
                else {
                    return false;
                };
                match (&left.kind, &right.kind) {
                    (
                        word::OpKind::Unary {
                            op: left_op,
                            arg: left_arg,
                        },
                        word::OpKind::Unary {
                            op: right_op,
                            arg: right_arg,
                        },
                    ) if left_op == right_op => pending.push((*left_arg, *right_arg)),
                    (
                        word::OpKind::Binary {
                            op: left_op,
                            left: left_left,
                            right: left_right,
                        },
                        word::OpKind::Binary {
                            op: right_op,
                            left: right_left,
                            right: right_right,
                        },
                    ) if left_op == right_op => {
                        pending.push((*left_left, *right_left));
                        pending.push((*left_right, *right_right));
                    }
                    (
                        word::OpKind::Mux {
                            cond: left_cond,
                            then_value: left_then,
                            else_value: left_else,
                        },
                        word::OpKind::Mux {
                            cond: right_cond,
                            then_value: right_then,
                            else_value: right_else,
                        },
                    ) => {
                        pending.push((*left_cond, *right_cond));
                        pending.push((*left_then, *right_then));
                        pending.push((*left_else, *right_else));
                    }
                    (
                        word::OpKind::Concat { parts: left },
                        word::OpKind::Concat { parts: right },
                    ) if left.len() == right.len() => {
                        pending.extend(left.iter().copied().zip(right.iter().copied()));
                    }
                    (
                        word::OpKind::Extract {
                            value: left_value,
                            lsb: left_lsb,
                            width: left_width,
                        },
                        word::OpKind::Extract {
                            value: right_value,
                            lsb: right_lsb,
                            width: right_width,
                        },
                    ) if left_lsb == right_lsb && left_width == right_width => {
                        pending.push((*left_value, *right_value));
                    }
                    (
                        word::OpKind::DynamicExtract {
                            value: left_value,
                            offset: left_offset,
                            width: left_width,
                        },
                        word::OpKind::DynamicExtract {
                            value: right_value,
                            offset: right_offset,
                            width: right_width,
                        },
                    ) if left_width == right_width => {
                        pending.push((*left_value, *right_value));
                        pending.push((*left_offset, *right_offset));
                    }
                    (
                        word::OpKind::Cast {
                            kind: left_kind,
                            value: left_value,
                            target: left_target,
                        },
                        word::OpKind::Cast {
                            kind: right_kind,
                            value: right_value,
                            target: right_target,
                        },
                    ) if left_kind == right_kind && left_target == right_target => {
                        pending.push((*left_value, *right_value));
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

fn scalar_constant(module: &word::WordModule, value: word::ValueId) -> Option<bool> {
    let value = module.value(value)?;
    let word::ValueKind::Constant(bits) = &value.kind else {
        return None;
    };
    match bits.bit_lsb(0)? {
        BitVal::Zero => Some(false),
        BitVal::One => Some(true),
        BitVal::X | BitVal::Z => None,
    }
}
