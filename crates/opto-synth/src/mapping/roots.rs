// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::word;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MappingRoot {
    pub(crate) value: word::ValueId,
    pub(crate) required_time: Option<f64>,
    pub(crate) output_load: Option<f64>,
}

pub(crate) fn requires_combinational_cover(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let stored = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant(format!("unknown mapping root {value:?}")))?;
    if stored.ty.width() != 1 {
        return Err(crate::SynthError::invariant(format!(
            "non-scalar mapping root {value:?} reached regional Boolean mapping"
        )));
    }
    let word::ValueKind::Operation(operation) = stored.kind else {
        return Ok(true);
    };
    let operation = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown mapping-root operation {operation:?}"))
    })?;
    Ok(!matches!(
        operation.kind,
        word::OpKind::Register(_) | word::OpKind::Latch(_)
    ))
}

pub(crate) fn mapping_roots(
    module: &word::WordModule,
    timing: &opto_timing::TimingContext,
    port_bindings: &opto_timing::PortBindings,
) -> Result<Vec<MappingRoot>, crate::SynthError> {
    let mut roots = Vec::new();
    let global_required = timing.minimum_synthesis_delay();
    for connect in module.connects() {
        let endpoint = timing_port_for_signal(module, connect.target.signal, port_bindings);
        let endpoint_required = endpoint
            .and_then(|port| timing.minimum_max_delay_to(opto_timing::TimingEndpoint::Port(port)))
            .or(global_required);
        let output_load = endpoint.and_then(|port| timing.load_on(port));
        let value = module.value(connect.value).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown RTL value {:?}", connect.value))
        })?;
        let word::ValueKind::Operation(operation_id) = value.kind else {
            roots.push(MappingRoot {
                value: connect.value,
                required_time: endpoint_required,
                output_load,
            });
            continue;
        };
        let operation = module.operation(operation_id).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown RTL operation {operation_id:?}"))
        })?;
        if let word::OpKind::Register(register) = &operation.kind {
            let required_time = timing_port_for_value(module, register.clock, port_bindings)
                .and_then(|port| timing.minimum_clock_period_on(port))
                .or(global_required);
            roots.push(MappingRoot {
                value: register.d,
                required_time,
                output_load: None,
            });
            roots.push(timed_root(register.clock, global_required));
            if let Some(enable) = register.enable {
                roots.push(timed_root(enable.value, global_required));
            }
            for reset in &register.resets {
                roots.push(timed_root(reset.value, global_required));
                roots.push(timed_root(reset.reset_value, global_required));
            }
        } else if let word::OpKind::Latch(latch) = &operation.kind {
            roots.push(MappingRoot {
                value: latch.d,
                required_time: global_required,
                output_load: None,
            });
            roots.push(timed_root(latch.enable.value, global_required));
            for reset in &latch.resets {
                roots.push(timed_root(reset.value, global_required));
                roots.push(timed_root(reset.reset_value, global_required));
            }
        } else {
            roots.push(MappingRoot {
                value: connect.value,
                required_time: endpoint_required,
                output_load,
            });
        }
    }
    for connection in module
        .instances()
        .iter()
        .flat_map(|instance| &instance.connections)
    {
        roots.extend(
            scalar_value_parts(module, connection.value)?
                .into_iter()
                .map(unconstrained_root),
        );
    }
    Ok(merge_by_value(roots))
}

/// Folds every sink constraint on a value into the single root that names it.
///
/// One value routinely reaches several sinks — a clock read by every register,
/// or the shared next-state cone of two equivalent FSM states. Each sink states
/// its own requirement, but the value is one mapping slot, so it must satisfy
/// the tightest required time and drive the total external load.
/// Discovery order is preserved: it is the order the cover visits roots in, so
/// merging must not silently reorder the mapping problem.
pub(crate) fn merge_by_value(roots: Vec<MappingRoot>) -> Vec<MappingRoot> {
    let mut slots = BTreeMap::<word::ValueId, usize>::new();
    let mut merged = Vec::with_capacity(roots.len());
    for root in roots {
        match slots.entry(root.value) {
            Entry::Vacant(slot) => {
                slot.insert(merged.len());
                merged.push(root);
            }
            Entry::Occupied(slot) => {
                let current: &mut MappingRoot = &mut merged[*slot.get()];
                current.required_time = match (current.required_time, root.required_time) {
                    (Some(current), Some(other)) => Some(current.min(other)),
                    (current, other) => current.or(other),
                };
                current.output_load = match (current.output_load, root.output_load) {
                    (Some(current), Some(other)) => Some(current + other),
                    (current, other) => current.or(other),
                };
            }
        }
    }
    merged
}

fn unconstrained_root(value: word::ValueId) -> MappingRoot {
    timed_root(value, None)
}

fn timed_root(value: word::ValueId, required_time: Option<f64>) -> MappingRoot {
    MappingRoot {
        value,
        required_time,
        output_load: None,
    }
}

fn timing_port_for_value(
    module: &word::WordModule,
    value: word::ValueId,
    port_bindings: &opto_timing::PortBindings,
) -> Option<opto_timing::PortId> {
    let word::ValueKind::Signal(reference) = module.value(value)?.kind else {
        return None;
    };
    timing_port_for_signal(module, reference.signal, port_bindings)
}

fn timing_port_for_signal(
    module: &word::WordModule,
    signal: word::SignalId,
    port_bindings: &opto_timing::PortBindings,
) -> Option<opto_timing::PortId> {
    let word::SignalKind::Port(port) = module.signal(signal)?.kind else {
        return None;
    };
    port_bindings.get(port.index())
}

pub(crate) fn scalar_value_parts(
    module: &word::WordModule,
    value: word::ValueId,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    let stored = module.value(value).ok_or_else(|| {
        crate::SynthError::invariant(format!("unknown instance connection value {value:?}"))
    })?;
    if stored.ty.width() == 1 {
        return Ok(vec![value]);
    }
    let word::ValueKind::Operation(operation) = stored.kind else {
        return Err(crate::SynthError::invariant(
            "vector instance connection was not bitblasted",
        ));
    };
    let operation = module.operation(operation).ok_or_else(|| {
        crate::SynthError::invariant(format!(
            "unknown instance connection operation {operation:?}"
        ))
    })?;
    let word::OpKind::Concat { parts } = &operation.kind else {
        return Err(crate::SynthError::invariant(
            "vector instance connection is not a concatenation of scalar bits",
        ));
    };
    let mut bits = Vec::with_capacity(parts.len());
    for &part in parts.iter().rev() {
        bits.extend(scalar_value_parts(module, part)?);
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word::{LValue, PortDirection, SourceSpan, WordModule, WordType};

    #[test]
    fn output_roots_keep_endpoint_specific_budget_and_load() {
        let mut module = WordModule::new("top");
        let input = module
            .add_port(
                "a",
                PortDirection::Input,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let output = module
            .add_port(
                "y",
                PortDirection::Output,
                WordType::bits(1).unwrap(),
                SourceSpan::default(),
            )
            .unwrap();
        let value = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                value,
                SourceSpan::default(),
            )
            .unwrap();

        let input_port = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(2).unwrap());
        let output_port = opto_timing::PortId::from_uid(opto_core::ObjectUid::from_raw(3).unwrap());
        let mut timing = opto_timing::TimingContext::new();
        timing
            .set_max_delay(
                0.8,
                Vec::new(),
                vec![opto_timing::TimingEndpoint::Port(output_port)],
            )
            .unwrap();
        timing.set_load(0.02, &[output_port]).unwrap();
        let port_bindings = opto_timing::PortBindings::new([input_port, output_port]);

        let roots = mapping_roots(&module, &timing, &port_bindings).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].value, value);
        assert_eq!(roots[0].required_time, Some(0.8));
        assert_eq!(roots[0].output_load, Some(0.02));
    }
}
