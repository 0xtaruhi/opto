// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use hashbrown::HashSet;
use opto_ir::word;
use std::ops::Range;

#[derive(Clone)]
struct PriorityChain {
    connect: usize,
    nodes: Vec<word::ValueId>,
    default: PriorityDefault,
    entries: Vec<PriorityEntry>,
    source: word::SourceSpan,
}

#[derive(Clone, Copy)]
enum PriorityDefault {
    Value(word::ValueId),
    BooleanFalse(word::WordType),
}

#[derive(Clone, Copy)]
struct PriorityEntry {
    condition: word::ValueId,
    value: word::ValueId,
}

#[derive(Clone, Copy)]
struct PriorityNode {
    valid: word::ValueId,
    value: word::ValueId,
}

pub(super) struct GeneratedOperations<Scope> {
    pub(super) range: Range<usize>,
    pub(super) scope: Scope,
}

pub(super) struct RebalanceResult<Scope> {
    pub(super) changed: bool,
    pub(super) generated: Vec<GeneratedOperations<Scope>>,
}

#[cfg(test)]
pub(super) fn rebalance_constant_priority_muxes(
    module: &mut word::WordModule,
) -> Result<bool, crate::SynthError> {
    Ok(rebalance_constant_priority_muxes_by(module, |_| Some(()))?.changed)
}

pub(super) fn rebalance_constant_priority_muxes_by<Scope: Copy>(
    module: &mut word::WordModule,
    mut classify: impl FnMut(&[word::ValueId]) -> Option<Scope>,
) -> Result<RebalanceResult<Scope>, crate::SynthError> {
    let mut chains = module
        .connects()
        .iter()
        .enumerate()
        .filter_map(|(connect, sink)| trace_chain(module, connect, sink))
        .filter(|chain| chain.entries.len() >= 4)
        .filter_map(|chain| classify(&chain.nodes).map(|scope| (chain, scope)))
        .collect::<Vec<_>>();
    if chains.is_empty() {
        return Ok(RebalanceResult {
            changed: false,
            generated: Vec::new(),
        });
    }
    materialize_defaults(module, chains.iter_mut().map(|(chain, _)| chain))?;
    let condition_sequences = chains
        .iter()
        .map(|(chain, _)| {
            chain
                .entries
                .iter()
                .map(|entry| entry.condition)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (index, (chain, _)) in chains.iter_mut().enumerate() {
        let conditions = &condition_sequences[index];
        let Some(master) = condition_sequences
            .iter()
            .filter(|candidate| candidate.ends_with(conditions))
            .max_by_key(|candidate| candidate.len())
        else {
            continue;
        };
        let missing = master.len() - conditions.len();
        if missing != 0 {
            let default = chain.default.value()?;
            let mut aligned = Vec::with_capacity(master.len());
            aligned.extend(
                master[..missing]
                    .iter()
                    .copied()
                    .map(|condition| PriorityEntry {
                        condition,
                        value: default,
                    }),
            );
            aligned.append(&mut chain.entries);
            chain.entries = aligned;
        }
    }
    let mut replacements = Vec::with_capacity(chains.len());
    let mut generated = Vec::with_capacity(chains.len());
    for (chain, scope) in &chains {
        let start = module.operations().len();
        let replacement = build_chain(module, chain)?;
        let end = module.operations().len();
        generated.push(GeneratedOperations {
            range: start..end,
            scope: *scope,
        });
        replacements.push((chain.connect, replacement));
    }
    replacements.sort_unstable_by_key(|(connect, _)| *connect);

    let connects = module.take_connects();
    let mut replacement = replacements.into_iter().peekable();
    for (index, connect) in connects.into_iter().enumerate() {
        let value = if replacement
            .peek()
            .is_some_and(|(connect, _)| *connect == index)
        {
            replacement.next().expect("peeked replacement exists").1
        } else {
            connect.value
        };
        module
            .connect(connect.target, value, connect.source)
            .map_err(crate::SynthError::from)?;
    }
    Ok(RebalanceResult {
        changed: true,
        generated,
    })
}

impl PriorityDefault {
    fn value(self) -> Result<word::ValueId, crate::SynthError> {
        match self {
            Self::Value(value) => Ok(value),
            Self::BooleanFalse(_) => Err(crate::SynthError::invariant(
                "priority default was not materialized",
            )),
        }
    }
}

fn materialize_defaults<'a>(
    module: &mut word::WordModule,
    chains: impl IntoIterator<Item = &'a mut PriorityChain>,
) -> Result<(), crate::SynthError> {
    for chain in chains {
        let PriorityDefault::BooleanFalse(ty) = chain.default else {
            continue;
        };
        let value = false_constant(module, ty)?;
        chain.default = PriorityDefault::Value(value);
    }
    Ok(())
}

fn false_constant(
    module: &mut word::WordModule,
    ty: word::WordType,
) -> Result<word::ValueId, crate::SynthError> {
    if let Some(value) = module
        .values()
        .iter()
        .enumerate()
        .find_map(|(index, value)| {
            (value.ty == ty
                && matches!(
                    &value.kind,
                    word::ValueKind::Constant(bits)
                        if bits.bit_lsb(0) == Some(opto_ir::BitVal::Zero)
                ))
            .then(|| word::ValueId::from_index(index).ok())
            .flatten()
        })
    {
        return Ok(value);
    }
    module
        .constant(
            opto_ir::ConstBits::from_bits(vec![opto_ir::BitVal::Zero])
                .map_err(crate::SynthError::from)?,
            ty,
            word::SourceSpan::default(),
        )
        .map_err(crate::SynthError::from)
}

fn trace_chain(
    module: &word::WordModule,
    connect: usize,
    sink: &word::Connect,
) -> Option<PriorityChain> {
    let mut current = sink.value;
    let mut entries = Vec::new();
    let mut nodes = Vec::new();
    loop {
        let stored = module.value(current)?;
        match stored.kind {
            word::ValueKind::Constant(_) => {
                return finish_chain(
                    connect,
                    nodes,
                    PriorityDefault::Value(current),
                    entries,
                    sink.source.clone(),
                );
            }
            word::ValueKind::Operation(operation) => {
                let word::OpKind::Mux {
                    cond,
                    then_value,
                    else_value,
                } = module.operation(operation)?.kind
                else {
                    return finish_implicit_boolean_chain(
                        module,
                        connect,
                        nodes,
                        current,
                        entries,
                        sink.source.clone(),
                    );
                };
                nodes.push(current);
                let (condition, previous, update) =
                    if let Some(condition) = inverted_condition(module, cond) {
                        (condition, then_value, else_value)
                    } else {
                        (cond, else_value, then_value)
                    };
                if !matches!(module.value(update)?.kind, word::ValueKind::Constant(_)) {
                    return None;
                }
                entries.push(PriorityEntry {
                    condition,
                    value: update,
                });
                current = previous;
            }
            word::ValueKind::Signal(_) => {
                return finish_implicit_boolean_chain(
                    module,
                    connect,
                    nodes,
                    current,
                    entries,
                    sink.source.clone(),
                );
            }
        }
    }
}

fn finish_chain(
    connect: usize,
    nodes: Vec<word::ValueId>,
    default: PriorityDefault,
    mut entries: Vec<PriorityEntry>,
    source: word::SourceSpan,
) -> Option<PriorityChain> {
    entries.reverse();
    let mut conditions = HashSet::with_capacity(entries.len());
    let conditions_are_distinct = entries
        .iter()
        .all(|entry| conditions.insert(entry.condition));
    conditions_are_distinct.then_some(PriorityChain {
        connect,
        nodes,
        default,
        entries,
        source,
    })
}

fn finish_implicit_boolean_chain(
    module: &word::WordModule,
    connect: usize,
    nodes: Vec<word::ValueId>,
    implicit: word::ValueId,
    mut entries: Vec<PriorityEntry>,
    source: word::SourceSpan,
) -> Option<PriorityChain> {
    let ty = module.value(implicit)?.ty;
    if ty.width() != 1
        || entries.is_empty()
        || !entries
            .iter()
            .all(|entry| constant_boolean(module, entry.value) == Some(true))
    {
        return None;
    }
    let true_value = entries[0].value;
    entries.push(PriorityEntry {
        condition: implicit,
        value: true_value,
    });
    finish_chain(
        connect,
        nodes,
        PriorityDefault::BooleanFalse(ty),
        entries,
        source,
    )
}

fn constant_boolean(module: &word::WordModule, value: word::ValueId) -> Option<bool> {
    let word::ValueKind::Constant(bits) = &module.value(value)?.kind else {
        return None;
    };
    if bits.width() != 1 {
        return None;
    }
    match bits.bit_lsb(0)? {
        opto_ir::BitVal::Zero => Some(false),
        opto_ir::BitVal::One => Some(true),
        opto_ir::BitVal::X | opto_ir::BitVal::Z => None,
    }
}

fn inverted_condition(module: &word::WordModule, value: word::ValueId) -> Option<word::ValueId> {
    let word::ValueKind::Operation(operation) = module.value(value)?.kind else {
        return None;
    };
    match module.operation(operation)?.kind {
        word::OpKind::Unary {
            op: word::UnaryOp::LogicalNot | word::UnaryOp::BitNot,
            arg,
        } if module.value(arg)?.ty.width() == 1 => Some(arg),
        _ => None,
    }
}

fn build_chain(
    module: &mut word::WordModule,
    chain: &PriorityChain,
) -> Result<word::ValueId, crate::SynthError> {
    let entries = &chain.entries;
    let default = chain.default.value()?;
    let all_values_equal = entries
        .windows(2)
        .all(|pair| pair[0].value == pair[1].value);
    if all_values_equal {
        let valid = balanced_or(
            module,
            entries.iter().map(|entry| entry.condition).collect(),
            &chain.source,
        )?;
        return module
            .mux(valid, entries[0].value, default, chain.source.clone())
            .map_err(crate::SynthError::from);
    }

    let mut nodes = entries
        .iter()
        .map(|entry| PriorityNode {
            valid: entry.condition,
            value: entry.value,
        })
        .collect::<Vec<_>>();
    while nodes.len() > 1 {
        let mut next = Vec::with_capacity(nodes.len().div_ceil(2));
        for pair in nodes.chunks(2) {
            let node = match *pair {
                [low, high] => {
                    let valid = module
                        .binary(
                            word::BinaryOp::BitOr,
                            low.valid,
                            high.valid,
                            chain.source.clone(),
                        )
                        .map_err(crate::SynthError::from)?;
                    let value = module
                        .mux(high.valid, high.value, low.value, chain.source.clone())
                        .map_err(crate::SynthError::from)?;
                    PriorityNode { valid, value }
                }
                [single] => single,
                _ => unreachable!("chunks(2) yields one or two entries"),
            };
            next.push(node);
        }
        nodes = next;
    }
    let root = nodes
        .into_iter()
        .next()
        .ok_or_else(|| crate::SynthError::invariant("priority chain is empty"))?;
    if entries[0].value == default {
        Ok(root.value)
    } else {
        module
            .mux(root.valid, root.value, default, chain.source.clone())
            .map_err(crate::SynthError::from)
    }
}

fn balanced_or(
    module: &mut word::WordModule,
    mut values: Vec<word::ValueId>,
    source: &word::SourceSpan,
) -> Result<word::ValueId, crate::SynthError> {
    while values.len() > 1 {
        let mut next = Vec::with_capacity(values.len().div_ceil(2));
        for pair in values.chunks(2) {
            next.push(match *pair {
                [left, right] => module
                    .binary(word::BinaryOp::BitOr, left, right, source.clone())
                    .map_err(crate::SynthError::from)?,
                [single] => single,
                _ => unreachable!("chunks(2) yields one or two entries"),
            });
        }
        values = next;
    }
    values
        .into_iter()
        .next()
        .ok_or_else(|| crate::SynthError::invariant("priority reduction is empty"))
}
