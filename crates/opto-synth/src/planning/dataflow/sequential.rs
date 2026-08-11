// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::DataflowChanges;
use hashbrown::HashMap;
use opto_ir::word;
use opto_runtime::ExecutionContext;
use std::collections::BTreeSet;

pub(crate) fn lower_inductive_state_constants(
    module: &mut word::WordModule,
    facts: &word::InductiveStateConstants,
    ownership: &mut crate::regional::StructuralOwnershipProvenance,
) -> Result<usize, crate::SynthError> {
    let initial_operations = module.operations().len();
    let candidates = (0..initial_operations)
        .filter_map(|index| {
            let operation_id = word::OpId::from_index(index).ok()?;
            let operation = module.operation(operation_id)?;
            let width = module.value(operation.result)?.ty.width();
            matches!(
                operation.kind,
                word::OpKind::Register(_) | word::OpKind::Latch(_)
            )
            .then(|| {
                let constants = (0..width)
                    .map(|bit| facts.bit(operation.result, bit))
                    .collect::<Box<[_]>>();
                constants
                    .iter()
                    .any(|bit| *bit != word::KnownBit::Unknown)
                    .then(|| (operation_id, operation.clone(), constants))
            })
            .flatten()
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(0);
    }

    let mut aliases = Vec::with_capacity(candidates.len());
    let mut lowered_bits = 0usize;
    for (source_operation, operation, constants) in candidates {
        let start = ownership.start(module)?;
        let result_ty = module
            .value(operation.result)
            .ok_or_else(|| crate::SynthError::invariant("state result disappeared"))?
            .ty;
        let mut bits = Vec::with_capacity(constants.len());
        for (index, constant) in constants.iter().copied().enumerate() {
            let index =
                u32::try_from(index).map_err(|_| crate::SynthError::capacity("state bit index"))?;
            let bit = match constant {
                word::KnownBit::Zero | word::KnownBit::One => {
                    lowered_bits += 1;
                    module
                        .constant(
                            opto_ir::ConstBits::from_bits(vec![match constant {
                                word::KnownBit::Zero => opto_ir::BitVal::Zero,
                                word::KnownBit::One => opto_ir::BitVal::One,
                                word::KnownBit::Unknown => unreachable!(),
                            }])
                            .map_err(crate::SynthError::from)?,
                            word::WordType::new(1, false, result_ty.state())
                                .map_err(crate::SynthError::from)?,
                            operation.source.clone(),
                        )
                        .map_err(crate::SynthError::from)?
                }
                word::KnownBit::Unknown => match &operation.kind {
                    word::OpKind::Register(register) => {
                        let d = scalar_state_input(module, register.d, index, &operation.source)?;
                        let resets = register
                            .resets
                            .iter()
                            .map(|reset| {
                                Ok(word::Reset {
                                    reset_value: scalar_state_input(
                                        module,
                                        reset.reset_value,
                                        index,
                                        &operation.source,
                                    )?,
                                    ..*reset
                                })
                            })
                            .collect::<Result<Vec<_>, crate::SynthError>>()?;
                        module
                            .register(
                                word::RegisterOp {
                                    d,
                                    resets,
                                    ..register.clone()
                                },
                                operation.source.clone(),
                            )
                            .map_err(crate::SynthError::from)?
                    }
                    word::OpKind::Latch(latch) => {
                        let d = scalar_state_input(module, latch.d, index, &operation.source)?;
                        let resets = latch
                            .resets
                            .iter()
                            .map(|reset| {
                                Ok(word::Reset {
                                    reset_value: scalar_state_input(
                                        module,
                                        reset.reset_value,
                                        index,
                                        &operation.source,
                                    )?,
                                    ..*reset
                                })
                            })
                            .collect::<Result<Vec<_>, crate::SynthError>>()?;
                        module
                            .latch(
                                word::LatchOp {
                                    d,
                                    resets,
                                    ..latch.clone()
                                },
                                operation.source.clone(),
                            )
                            .map_err(crate::SynthError::from)?
                    }
                    _ => unreachable!("candidate is sequential"),
                },
            };
            bits.push(bit);
        }
        bits.reverse();
        let replacement = if let [bit] = bits.as_slice() {
            *bit
        } else {
            module
                .concat(bits, operation.source.clone())
                .map_err(crate::SynthError::from)?
        };
        aliases.push((operation.result, replacement));
        ownership.claim_since(module, start, &[source_operation])?;
    }

    let mut replacements = (0..module.values().len())
        .map(|index| word::ValueId::from_index(index).map_err(crate::SynthError::Word))
        .collect::<Result<Vec<_>, _>>()?;
    for (source, replacement) in aliases {
        replacements[source.index()] = replacement;
    }
    module
        .rewrite_value_uses(&replacements)
        .map_err(crate::SynthError::from)?;
    Ok(lowered_bits)
}

fn scalar_state_input(
    module: &mut word::WordModule,
    value: word::ValueId,
    index: u32,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    if module
        .value(value)
        .is_some_and(|stored| stored.ty.width() == 1 && index == 0)
    {
        return Ok(value);
    }
    module
        .extract(value, index, 1, source.clone())
        .map_err(crate::SynthError::from)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SequentialKey {
    Register {
        region: crate::RegionRowId,
        d: word::ValueId,
        clock: word::ValueId,
        edge: word::Edge,
        enable: Option<word::Enable>,
        resets: Box<[word::Reset]>,
    },
    Latch {
        region: crate::RegionRowId,
        d: word::ValueId,
        enable: word::Enable,
        resets: Box<[word::Reset]>,
    },
}

#[cfg(test)]
pub(crate) fn share_equivalent_sequential_values_in_regions(
    module: &mut word::WordModule,
    runtime: &ExecutionContext,
    operation_regions: &[Option<crate::RegionRowId>],
) -> Result<DataflowChanges, crate::SynthError> {
    if operation_regions.len() != module.operations().len() {
        return Err(crate::SynthError::invariant(
            "regional sequential sharing has incomplete operation ownership",
        ));
    }
    share_equivalent_sequential_values_by(module, runtime, operation_regions, |value| value)
}

pub(crate) fn share_equivalent_sequential_values_by(
    module: &mut word::WordModule,
    runtime: &ExecutionContext,
    operation_regions: &[Option<crate::RegionRowId>],
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
    let mut sequential_targets = HashMap::<word::ValueId, word::LValue>::new();
    let mut multiple_targets = BTreeSet::new();
    for connect in module.connects() {
        if sequential_targets
            .insert(connect.value, connect.target.clone())
            .is_some()
        {
            multiple_targets.insert(connect.value);
        }
    }
    for value in multiple_targets {
        sequential_targets.remove(&value);
    }
    let mut representatives = HashMap::<SequentialKey, word::ValueId>::new();
    let mut aliases = Vec::new();
    for (index, operation) in module.operations().iter().enumerate() {
        let Some(region) = operation_regions[index] else {
            continue;
        };
        let Some(target) = sequential_targets.get(&operation.result) else {
            continue;
        };
        if module.signal_is_preserved(target.signal) {
            continue;
        }
        let key = match &operation.kind {
            word::OpKind::Register(register) => SequentialKey::Register {
                region,
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
                region,
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
            let target = sequential_targets.get(&representative).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "shared sequential value {representative:?} has no signal target"
                ))
            })?;
            connect.value = read_static_target(module, target, &connect.source)?;
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

fn read_static_target(
    module: &mut word::WordModule,
    target: &word::LValue,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    if target.dynamic.is_some() {
        return Err(crate::SynthError::invariant(
            "shared sequential value has a dynamic signal target",
        ));
    }
    match target.range {
        Some(range) => module
            .read_signal_slice(target.signal, range.lsb, range.width(), source.clone())
            .map_err(Into::into),
        None => module
            .read_signal(target.signal, source.clone())
            .map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_region(module: &word::WordModule) -> Vec<Option<crate::RegionRowId>> {
        vec![Some(crate::RegionRowId::from_index(0).unwrap()); module.operations().len()]
    }

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

        let mut split = module.clone();
        let split_regions = vec![
            Some(crate::RegionRowId::from_index(0).unwrap()),
            Some(crate::RegionRowId::from_index(1).unwrap()),
        ];
        assert!(
            !share_equivalent_sequential_values_in_regions(
                &mut split,
                crate::test_runtime(),
                &split_regions,
            )
            .unwrap()
            .has_equivalences()
        );

        let regions = one_region(&module);
        let changes = share_equivalent_sequential_values_in_regions(
            &mut module,
            crate::test_runtime(),
            &regions,
        )
        .unwrap();

        assert!(changes.has_equivalences());
        assert_eq!(changes.representatives()[q1.index()], q0);
        assert_eq!(module.connects()[0].value, q0);
        assert!(matches!(
            module.value(module.connects()[1].value).unwrap().kind,
            word::ValueKind::Signal(reference) if reference.signal == q0_signal
        ));
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

        let regions = one_region(&module);
        let changes = share_equivalent_sequential_values_in_regions(
            &mut module,
            crate::test_runtime(),
            &regions,
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

        let regions = one_region(&module);
        let changes = share_equivalent_sequential_values_in_regions(
            &mut module,
            crate::test_runtime(),
            &regions,
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

        let regions = one_region(&module);
        let canonical =
            super::super::optimize_owned_combinational_dataflow(&mut module, &regions).unwrap();
        let changes = share_equivalent_sequential_values_by(
            &mut module,
            crate::test_runtime(),
            &regions,
            |value| canonical.representatives()[value.index()],
        )
        .unwrap();

        assert!(changes.has_equivalences());
        assert_eq!(
            changes.representatives()[registers[1].index()],
            registers[0]
        );
    }

    #[test]
    fn lowers_only_inductively_constant_bits_of_vector_state() {
        let mut module = word::WordModule::new("state_constants");
        let source = word::SourceSpan::default();
        let bit = word::WordType::bits(1).unwrap();
        let pair = word::WordType::bits(2).unwrap();
        let clock = module
            .add_port("clock", word::PortDirection::Input, bit, source.clone())
            .unwrap();
        let reset = module
            .add_port("reset", word::PortDirection::Input, bit, source.clone())
            .unwrap();
        let input = module
            .add_port("input", word::PortDirection::Input, bit, source.clone())
            .unwrap();
        let output = module
            .add_port("output", word::PortDirection::Output, pair, source.clone())
            .unwrap();
        let state_signal = module.add_wire("state", pair, source.clone()).unwrap();
        let clock = module
            .read_signal(module.port(clock).unwrap().signal, source.clone())
            .unwrap();
        let reset = module
            .read_signal(module.port(reset).unwrap().signal, source.clone())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, source.clone())
            .unwrap();
        let state = module.read_signal(state_signal, source.clone()).unwrap();
        let low = module.extract(state, 0, 1, source.clone()).unwrap();
        let held_zero = module
            .binary(word::BinaryOp::BitAnd, low, input, source.clone())
            .unwrap();
        let data = module
            .concat(vec![input, held_zero], source.clone())
            .unwrap();
        let zero = module
            .constant(
                opto_ir::ConstBits::from_bin_str("00").unwrap(),
                pair,
                source.clone(),
            )
            .unwrap();
        let register = module
            .register(
                word::RegisterOp {
                    name: None,
                    d: data,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: vec![word::Reset {
                        kind: word::ResetKind::Async,
                        value: reset,
                        active_high: true,
                        reset_value: zero,
                    }],
                },
                source.clone(),
            )
            .unwrap();
        module
            .connect(word::LValue::signal(state_signal), register, source.clone())
            .unwrap();
        let visible = module.read_signal(state_signal, source.clone()).unwrap();
        module
            .connect(
                word::LValue::signal(module.port(output).unwrap().signal),
                visible,
                source,
            )
            .unwrap();

        let facts = word::inductive_state_constants(&module);
        assert_eq!(facts.bit(register, 0), word::KnownBit::Zero);
        assert_eq!(facts.bit(register, 1), word::KnownBit::Unknown);
        let mut ownership = crate::regional::StructuralOwnershipProvenance::from_owners_for_test(
            &module,
            one_region(&module),
        )
        .unwrap();
        assert_eq!(
            lower_inductive_state_constants(&mut module, &facts, &mut ownership).unwrap(),
            1
        );
        let remap = module.compact_observable_netlist().unwrap();
        ownership.apply_netlist_remap(&remap).unwrap();

        assert_eq!(
            module
                .operations()
                .iter()
                .filter(|operation| matches!(operation.kind, word::OpKind::Register(_)))
                .count(),
            1
        );
    }
}
