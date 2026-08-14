// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

#[derive(Debug)]
pub(crate) struct GeneratedNames {
    next_instance: u32,
    next_wire: u32,
}

impl GeneratedNames {
    pub(crate) fn new(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        Ok(Self {
            next_instance: next_generated_index(
                module
                    .instances()
                    .iter()
                    .map(|instance| module.name_str(instance.name)),
                "U",
            )?,
            next_wire: next_generated_index(
                module
                    .signals()
                    .iter()
                    .filter_map(|signal| signal.name.map(|name| module.name_str(name))),
                "n",
            )?,
        })
    }

    pub(crate) fn instance(&mut self) -> Result<String, crate::SynthError> {
        let name = take_generated_name(&mut self.next_instance, "U")?;
        Ok(name)
    }

    pub(crate) fn preferred_instance(
        module: &word::WordModule,
        stem: &str,
        suffix: &str,
    ) -> Result<String, crate::SynthError> {
        let base = format!("{stem}{suffix}");
        if module.instance_id(&base).is_none() {
            return Ok(base);
        }
        for index in 1..=u32::MAX {
            let name = format!("{base}_{index}");
            if module.instance_id(&name).is_none() {
                return Ok(name);
            }
        }
        Err(crate::SynthError::invariant(format!(
            "exhausted generated instance names for '{base}'"
        )))
    }

    fn wire(&mut self) -> Result<String, crate::SynthError> {
        take_generated_name(&mut self.next_wire, "n")
    }
}

pub(crate) fn add_generated_wire_value(
    generated_names: &mut GeneratedNames,
    module: &mut word::WordModule,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let name = generated_names.wire()?;
    let signal = module
        .add_wire(
            name,
            word::WordType::bits(1).map_err(crate::SynthError::from)?,
            source.clone(),
        )
        .map_err(crate::SynthError::from)?;
    module
        .read_signal(signal, source.clone())
        .map_err(crate::SynthError::from)
}

pub(crate) fn live_operation_mask(
    module: &word::WordModule,
    observed_values: &[word::ValueId],
) -> Result<Vec<bool>, crate::SynthError> {
    Ok(liveness_masks(module, observed_values)?.1)
}

fn liveness_masks(
    module: &word::WordModule,
    observed_values: &[word::ValueId],
) -> Result<(Vec<bool>, Vec<bool>), crate::SynthError> {
    let mut live_values = vec![false; module.values().len()];
    let mut live_operations = vec![false; module.operations().len()];
    let mut pending = module
        .connects()
        .iter()
        .map(|connect| connect.value)
        .chain(
            module
                .connects()
                .iter()
                .filter_map(|connect| connect.target.dynamic.map(|range| range.offset)),
        )
        .chain(
            module
                .instances()
                .iter()
                .flat_map(|instance| &instance.connections)
                .map(|connection| connection.value),
        )
        .chain(observed_values.iter().copied())
        .collect::<Vec<_>>();
    for read in module.memory_read_ports() {
        pending.push(read.address);
        if let word::MemoryReadTiming::Synchronous { clock, enable, .. } = read.timing {
            pending.push(clock.value);
            if let Some(enable) = enable {
                pending.push(enable.value);
            }
        }
    }
    for write in module.memory_write_ports() {
        pending.extend([write.address, write.data, write.clock.value]);
        if let Some(enable) = write.enable {
            pending.push(enable.value);
        }
        if let Some(mask) = write.mask {
            pending.push(mask.value);
        }
    }
    while let Some(value) = pending.pop() {
        let visited = live_values.get_mut(value.index()).ok_or_else(|| {
            crate::SynthError::invariant("mapped-netlist liveness reached an unknown value")
        })?;
        if std::mem::replace(visited, true) {
            continue;
        }
        let Some(word::ValueKind::Operation(operation)) =
            module.value(value).map(|value| &value.kind)
        else {
            continue;
        };
        let operation = *operation;
        let live = live_operations.get_mut(operation.index()).ok_or_else(|| {
            crate::SynthError::invariant("mapped-netlist liveness reached an unknown operation")
        })?;
        if std::mem::replace(live, true) {
            continue;
        }
        let operation = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant("mapped-netlist liveness lost an operation")
        })?;
        pending.extend(crate::word::operation_inputs(&operation.kind));
    }
    Ok((live_values, live_operations))
}

fn next_generated_index<'a>(
    names: impl IntoIterator<Item = &'a str>,
    prefix: &str,
) -> Result<u32, crate::SynthError> {
    let max_index = names
        .into_iter()
        .filter_map(|name| name.strip_prefix(prefix)?.parse::<u32>().ok())
        .max();
    match max_index {
        Some(index) => index.checked_add(1).ok_or_else(|| {
            crate::SynthError::invariant(format!("exhausted generated '{prefix}' names"))
        }),
        None => Ok(1),
    }
}

fn take_generated_name(next: &mut u32, prefix: &str) -> Result<String, crate::SynthError> {
    let index = *next;
    *next = index.checked_add(1).ok_or_else(|| {
        crate::SynthError::invariant(format!("exhausted generated '{prefix}' names"))
    })?;
    Ok(format!("{prefix}{index}"))
}

/// Reads a register's own output through a dedicated wire.
///
/// A register's target signal can be assigned at several program points, so a
/// read of it denotes whichever assignment last ran. This wire has exactly one
/// driver, so reading it always denotes the register's output and nothing else.
pub(crate) fn add_generated_boundary_value(
    generated_names: &mut GeneratedNames,
    module: &mut word::WordModule,
    value: word::ValueId,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    let ty = module
        .value(value)
        .ok_or_else(|| crate::SynthError::invariant("boundary source value is unknown"))?
        .ty;
    let signal = module
        .add_wire(generated_names.wire()?, ty, source.clone())
        .map_err(crate::SynthError::from)?;
    module
        .connect(word::LValue::signal(signal), value, source.clone())
        .map_err(crate::SynthError::from)?;
    module
        .read_signal(signal, source.clone())
        .map_err(crate::SynthError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_names_are_allocated_monotonically_after_existing_names() {
        let mut module = word::WordModule::new("top");
        module
            .add_wire(
                "n7",
                word::WordType::bits(1).unwrap(),
                word::SourceSpan::default(),
            )
            .unwrap();
        module
            .add_instance(
                "U9",
                "existing_cell",
                Vec::new(),
                word::SourceSpan::default(),
            )
            .unwrap();

        let mut names = GeneratedNames::new(&module).unwrap();

        assert_eq!(names.wire().unwrap(), "n8");
        assert_eq!(names.wire().unwrap(), "n9");
        assert_eq!(names.instance().unwrap(), "U10");
        assert_eq!(names.instance().unwrap(), "U11");
    }

    #[test]
    fn liveness_keeps_observed_candidate_values_without_retaining_dead_logic() {
        let source = word::SourceSpan::default();
        let bit = word::WordType::bits(1).unwrap();
        let mut module = word::WordModule::new("top");
        let input = module
            .add_port("a", word::PortDirection::Input, bit, source.clone())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, source.clone())
            .unwrap();
        let observed = module
            .unary(word::UnaryOp::BitNot, input, source.clone())
            .unwrap();
        let dead = module
            .unary(word::UnaryOp::BitNot, observed, source)
            .unwrap();

        let live = live_operation_mask(&module, &[observed]).unwrap();
        let word::ValueKind::Operation(observed_operation) = module.value(observed).unwrap().kind
        else {
            unreachable!();
        };
        let word::ValueKind::Operation(dead_operation) = module.value(dead).unwrap().kind else {
            unreachable!();
        };

        assert!(live[observed_operation.index()]);
        assert!(!live[dead_operation.index()]);
    }
}

