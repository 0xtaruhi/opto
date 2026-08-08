// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::state::Assignment;
use opto_ir::{BitVal, proc, word};

pub(super) fn resolve_flop_events(
    module: &mut word::WordModule,
    events: &[proc::SensitivityEvent],
    assignments: &mut [Assignment],
    source: &word::SourceSpan,
) -> Result<proc::SensitivityEvent, crate::SynthError> {
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
    let expected_controls = async_events
        .iter()
        .map(|event| (event.signal, event.edge == word::Edge::Pos))
        .collect::<Vec<_>>();
    let canonical_control = match async_events.as_slice() {
        [event] => Some((
            module
                .read_signal(event.signal, source.clone())
                .map_err(crate::SynthError::from)?,
            event.edge == word::Edge::Pos,
        )),
        _ => None,
    };
    for assignment in assignments {
        if assignment.resets.is_empty() {
            if async_events.iter().all(|&event| assignment.holds_on(event)) {
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
    events: &[proc::SensitivityEvent],
    resets: &[word::Reset],
) -> Result<(proc::SensitivityEvent, Vec<proc::SensitivityEvent>), crate::SynthError> {
    let mut matches = Vec::new();
    for (clock_index, &clock) in events.iter().enumerate() {
        let async_events = events
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != clock_index)
            .map(|(_, event)| *event)
            .collect::<Vec<_>>();
        let mut controls = async_events
            .iter()
            .map(|event| (event.signal, event.edge == word::Edge::Pos))
            .collect::<Vec<_>>();
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
                .map(|event| format_control(module, (event.signal, event.edge == word::Edge::Pos)))
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

fn format_control(
    module: &word::WordModule,
    (signal, active_high): (word::SignalId, bool),
) -> String {
    let name = module
        .signal(signal)
        .and_then(|signal| signal.name)
        .map_or("<unnamed>", |name| module.name_str(name));
    let edge = if active_high { "posedge" } else { "negedge" };
    format!("{edge} {name}")
}

fn matching_async_prefix(
    module: &word::WordModule,
    resets: &[word::Reset],
    expected: &[(word::SignalId, bool)],
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
) -> Result<Vec<(word::SignalId, bool)>, crate::SynthError> {
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
    controls: &mut Vec<(word::SignalId, bool)>,
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
) -> Option<(word::SignalId, bool)> {
    let value = module.value(value)?;
    if value.ty.width() != 1 {
        return None;
    }
    match &value.kind {
        word::ValueKind::Signal(reference)
            if reference.lsb == 0
                && reference.width() == 1
                && module.signal(reference.signal)?.ty.width() == 1 =>
        {
            Some((reference.signal, asserted_when_high))
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
) -> Option<(word::SignalId, bool)> {
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
