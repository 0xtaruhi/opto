// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::DataflowChanges;
use hashbrown::HashMap;
use opto_ir::word;
use opto_runtime::ExecutionContext;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SequentialKey {
    Register {
        d: word::ValueId,
        clock: word::ValueId,
        edge: word::Edge,
        enable: Option<word::Enable>,
        resets: Box<[word::Reset]>,
    },
    Latch {
        d: word::ValueId,
        enable: word::Enable,
        resets: Box<[word::Reset]>,
    },
}

pub(crate) fn share_equivalent_sequential_values_by(
    module: &mut word::WordModule,
    operations: &[word::OpId],
    runtime: &ExecutionContext,
    canonical_value: impl Fn(word::ValueId) -> word::ValueId,
) -> Result<DataflowChanges, crate::SynthError> {
    let mut canonical_signals = HashMap::new();
    for (index, value) in module.values().iter().enumerate() {
        let word::ValueKind::Signal(reference) = value.kind else {
            continue;
        };
        canonical_signals
            .entry(reference)
            .or_insert(word::ValueId::from_index(index).map_err(crate::SynthError::Word)?);
    }
    let canonical_value = |value: word::ValueId| {
        let value = canonical_value(value);
        match module.value(value).map(|value| &value.kind) {
            Some(word::ValueKind::Signal(reference)) => canonical_signals[reference],
            Some(word::ValueKind::Operation(_) | word::ValueKind::Constant(_)) | None => value,
        }
    };
    let mut representatives = HashMap::<SequentialKey, word::ValueId>::new();
    let mut aliases = Vec::new();
    for &operation_id in operations {
        let operation = module.operation(operation_id).ok_or_else(|| {
            crate::SynthError::invariant("explicit sequential sharing candidate is not live")
        })?;
        let key = match &operation.kind {
            word::OpKind::Register(register) => SequentialKey::Register {
                d: canonical_value(register.d),
                clock: canonical_value(register.clock),
                edge: register.edge,
                enable: register.enable.map(|enable| word::Enable {
                    value: canonical_value(enable.value),
                    ..enable
                }),
                resets: register
                    .resets
                    .iter()
                    .map(|reset| word::Reset {
                        value: canonical_value(reset.value),
                        reset_value: canonical_value(reset.reset_value),
                        ..*reset
                    })
                    .collect(),
            },
            word::OpKind::Latch(latch) => SequentialKey::Latch {
                d: canonical_value(latch.d),
                enable: word::Enable {
                    value: canonical_value(latch.enable.value),
                    ..latch.enable
                },
                resets: latch
                    .resets
                    .iter()
                    .map(|reset| word::Reset {
                        value: canonical_value(reset.value),
                        reset_value: canonical_value(reset.reset_value),
                        ..*reset
                    })
                    .collect(),
            },
            _ => continue,
        };
        if let Some(&representative) = representatives.get(&key) {
            aliases.push((operation.result, representative));
        } else {
            representatives.insert(key, operation.result);
        }
    }
    if aliases.is_empty() {
        return DataflowChanges::identity(module.values().len());
    }

    let mut replacements = vec![None; module.values().len()];
    for &(from, to) in &aliases {
        replacements[from.index()] = Some(to);
    }
    let operation_count = module.operations().len();
    let rewritten_operations = runtime.analyze_indexed(operation_count, |index| {
        let kind = &module.operations()[index].kind;
        if !crate::word::operation_inputs(kind)
            .into_iter()
            .any(|value| replacements.get(value.index()).is_some_and(Option::is_some))
        {
            return Ok::<_, crate::SynthError>(None);
        }
        let mut kind = kind.clone();
        super::rewrite_operation_inputs(&mut kind, |value| {
            Ok(replacements
                .get(value.index())
                .copied()
                .flatten()
                .unwrap_or(value))
        })?;
        Ok(Some(kind))
    })?;
    for (index, rewritten) in rewritten_operations.into_iter().enumerate() {
        let Some(kind) = rewritten else { continue };
        let operation = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        module
            .operation_mut(operation)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!("unknown operation {operation:?}"))
            })?
            .kind = kind;
    }

    let connects = module.take_connects();
    for mut connect in connects {
        if let Some(representative) = replacements.get(connect.value.index()).copied().flatten() {
            connect.value = representative;
        }
        if let Some(dynamic) = &mut connect.target.dynamic {
            dynamic.offset = replacements
                .get(dynamic.offset.index())
                .copied()
                .flatten()
                .unwrap_or(dynamic.offset);
        }
        module
            .connect(connect.target, connect.value, connect.source)
            .map_err(crate::SynthError::from)?;
    }

    let instance_connections = module
        .instances()
        .iter()
        .enumerate()
        .flat_map(|(instance, body)| {
            body.connections
                .iter()
                .map(move |connection| (instance, connection.port, connection.value))
        })
        .collect::<Vec<_>>();
    for (instance, port, value) in instance_connections {
        let value = replacements
            .get(value.index())
            .copied()
            .flatten()
            .unwrap_or(value);
        let instance = word::InstId::from_index(instance).map_err(crate::SynthError::Word)?;
        let port = module.name_str(port).to_string();
        module
            .set_instance_connection_value(instance, &port, value)
            .map_err(crate::SynthError::from)?;
    }

    DataflowChanges::from_aliases(module.values().len(), &aliases)
}

pub(crate) fn shareable_sequential_operations(
    module: &word::WordModule,
) -> Result<Box<[word::OpId]>, crate::SynthError> {
    let mut targets = HashMap::<word::ValueId, word::SignalId>::new();
    let mut ambiguous = BTreeSet::new();
    for connect in module.connects() {
        if targets
            .insert(connect.value, connect.target.signal)
            .is_some()
        {
            ambiguous.insert(connect.value);
        }
    }
    module
        .operations()
        .iter()
        .enumerate()
        .filter_map(|(index, operation)| {
            matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            )
            .then_some((index, operation))
        })
        .filter(|(_, operation)| {
            !ambiguous.contains(&operation.result)
                && targets
                    .get(&operation.result)
                    .is_some_and(|&signal| !module.signal_is_preserved(signal))
        })
        .map(|(index, _)| word::OpId::from_index(index).map_err(crate::SynthError::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_registers_with_identical_data_and_controls() {
        let mut module = word::WordModule::new("top");
        let ty = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::default();
        let d_signal = module.add_wire("d", ty, source.clone()).unwrap();
        let clock_signal = module.add_wire("clock", ty, source.clone()).unwrap();
        let q0_signal = module.add_wire("q0", ty, source.clone()).unwrap();
        let q1_signal = module.add_wire("q1", ty, source.clone()).unwrap();
        let d = module.read_signal(d_signal, source.clone()).unwrap();
        let clock = module.read_signal(clock_signal, source.clone()).unwrap();
        let q0_name = module.intern_name("q0_reg").unwrap();
        let q0 = module
            .register(
                word::RegisterOp {
                    name: Some(q0_name),
                    d,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                source.clone(),
            )
            .unwrap();
        let q1_name = module.intern_name("q1_reg").unwrap();
        let q1_clock = module.read_signal(clock_signal, source.clone()).unwrap();
        let q1 = module
            .register(
                word::RegisterOp {
                    name: Some(q1_name),
                    d,
                    clock: q1_clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                source.clone(),
            )
            .unwrap();
        module
            .connect(word::LValue::signal(q0_signal), q0, source.clone())
            .unwrap();
        module
            .connect(word::LValue::signal(q1_signal), q1, source)
            .unwrap();

        let candidates = shareable_sequential_operations(&module).unwrap();
        let changes = share_equivalent_sequential_values_by(
            &mut module,
            &candidates,
            crate::test_runtime(),
            |value| value,
        )
        .unwrap();

        assert!(changes.has_equivalences());
        assert_eq!(changes.representatives()[q1.index()], q0);
        assert_eq!(module.connects()[0].value, q0);
        assert_eq!(module.connects()[1].value, q0);
        module.compact_netlist().unwrap();
        assert_eq!(
            module
                .operations()
                .iter()
                .filter(|operation| matches!(operation.kind, word::OpKind::Register(_)))
                .count(),
            1
        );
    }

    #[test]
    fn does_not_share_async_registers() {
        let mut module = word::WordModule::new("top");
        let ty = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::default();
        let d_signal = module.add_wire("d", ty, source.clone()).unwrap();
        let clock_signal = module.add_wire("clock", ty, source.clone()).unwrap();
        let d = module.read_signal(d_signal, source.clone()).unwrap();
        let clock = module.read_signal(clock_signal, source.clone()).unwrap();
        for index in 0..2 {
            let signal = module
                .add_wire(format!("q{index}"), ty, source.clone())
                .unwrap();
            let value = module
                .register(
                    word::RegisterOp {
                        name: None,
                        d,
                        clock,
                        edge: word::Edge::Pos,
                        enable: None,
                        resets: Vec::new(),
                    },
                    source.clone(),
                )
                .unwrap();
            module
                .connect(word::LValue::signal(signal), value, source.clone())
                .unwrap();
            if index == 1 {
                module
                    .set_synthesis_directive(
                        word::AnnotationTarget::Signal(signal),
                        word::SynthesisDirectiveKind::AsyncRegister,
                        true,
                        source.clone(),
                    )
                    .unwrap();
            }
        }

        let candidates = shareable_sequential_operations(&module).unwrap();
        let changes = share_equivalent_sequential_values_by(
            &mut module,
            &candidates,
            crate::test_runtime(),
            |value| value,
        )
        .unwrap();

        assert!(!changes.has_equivalences());
        assert_eq!(
            module
                .operations()
                .iter()
                .filter(|operation| matches!(operation.kind, word::OpKind::Register(_)))
                .count(),
            2
        );
    }

    #[test]
    fn keeps_registers_with_different_enables() {
        let mut module = word::WordModule::new("top");
        let ty = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::default();
        let signals = ["d", "clock", "enable0", "enable1"].map(|name| {
            let signal = module.add_wire(name, ty, source.clone()).unwrap();
            module.read_signal(signal, source.clone()).unwrap()
        });
        for (index, enable) in signals[2..].iter().enumerate() {
            let name = module.intern_name(format!("q{index}_reg")).unwrap();
            module
                .register(
                    word::RegisterOp {
                        name: Some(name),
                        d: signals[0],
                        clock: signals[1],
                        edge: word::Edge::Pos,
                        enable: Some(word::Enable {
                            value: *enable,
                            active_high: true,
                        }),
                        resets: Vec::new(),
                    },
                    source.clone(),
                )
                .unwrap();
        }

        let candidates = (0..module.operations().len())
            .map(|index| word::OpId::from_index(index).unwrap())
            .collect::<Vec<_>>();
        let changes = share_equivalent_sequential_values_by(
            &mut module,
            &candidates,
            crate::test_runtime(),
            |value| value,
        )
        .unwrap();
        assert!(!changes.has_equivalences());
    }

    #[test]
    fn shares_registers_by_boolean_graph_equivalence() {
        let mut module = word::WordModule::new("top");
        let ty = word::WordType::bits(1).unwrap();
        let source = word::SourceSpan::default();
        let d_signal = module.add_wire("d", ty, source.clone()).unwrap();
        let clock_signal = module.add_wire("clock", ty, source.clone()).unwrap();
        let d = module.read_signal(d_signal, source.clone()).unwrap();
        let clock = module.read_signal(clock_signal, source.clone()).unwrap();
        let d0 = module
            .unary(word::UnaryOp::BitNot, d, source.clone())
            .unwrap();
        let d1 = module
            .unary(word::UnaryOp::BitNot, d, source.clone())
            .unwrap();
        let registers = [d0, d1].map(|data| {
            module
                .register(
                    word::RegisterOp {
                        name: None,
                        d: data,
                        clock,
                        edge: word::Edge::Pos,
                        enable: None,
                        resets: Vec::new(),
                    },
                    source.clone(),
                )
                .unwrap()
        });
        for (index, register) in registers.iter().enumerate() {
            let signal = module
                .add_wire(format!("q{index}"), ty, source.clone())
                .unwrap();
            module
                .connect(word::LValue::signal(signal), *register, source.clone())
                .unwrap();
        }

        let canonical = super::super::canonicalize_combinational_dataflow(&mut module).unwrap();
        let candidates = shareable_sequential_operations(&module).unwrap();
        let changes = share_equivalent_sequential_values_by(
            &mut module,
            &candidates,
            crate::test_runtime(),
            |value| canonical.representatives()[value.index()],
        )
        .unwrap();

        assert!(changes.has_equivalences());
        assert_eq!(
            changes.representatives()[registers[1].index()],
            registers[0]
        );
    }
}
