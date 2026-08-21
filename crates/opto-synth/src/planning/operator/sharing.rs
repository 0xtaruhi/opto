// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod activation;
mod rewrite;

use activation::{Activation, UseIndex};
use opto_ir::word;
use std::collections::BTreeMap;
use std::ops::Range;

/// One SSA suffix that implements a replacement for existing operations.
pub(crate) struct OperationRewrite {
    pub(crate) created: Range<usize>,
    pub(crate) replaced: Box<[word::OpId]>,
    pub(crate) replacement: word::ValueId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::planning::operator::sharing) enum ArithmeticKind {
    Add,
    Subtract,
}

impl ArithmeticKind {
    pub(in crate::planning::operator::sharing) const fn binary(self) -> word::BinaryOp {
        match self {
            Self::Add => word::BinaryOp::Add,
            Self::Subtract => word::BinaryOp::Sub,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    kind: ArithmeticKind,
    width: u32,
    signed: bool,
    state: u8,
}

#[derive(Clone)]
pub(in crate::planning::operator::sharing) struct ShareCandidate {
    pub(in crate::planning::operator::sharing) operation: word::OpId,
    pub(in crate::planning::operator::sharing) result: word::ValueId,
    pub(in crate::planning::operator::sharing) inputs: [word::ValueId; 2],
    pub(in crate::planning::operator::sharing) kind: ArithmeticKind,
    pub(in crate::planning::operator::sharing) activation: Activation,
}

pub(crate) fn share_muxed_arithmetic(
    module: &mut word::WordModule,
) -> Result<Box<[OperationRewrite]>, crate::SynthError> {
    let uses = UseIndex::build(module)?;
    let mut buckets = BTreeMap::<SourceKey, Vec<ShareCandidate>>::new();
    for index in 0..module.operations().len() {
        let operation_id = word::OpId::from_index(index).map_err(crate::SynthError::Word)?;
        let operation = module.operation(operation_id).ok_or_else(|| {
            crate::SynthError::invariant(format!("unknown arithmetic operation {operation_id:?}"))
        })?;
        let word::OpKind::Binary { op, left, right } = operation.kind else {
            continue;
        };
        let kind = match op {
            word::BinaryOp::Add => ArithmeticKind::Add,
            word::BinaryOp::Sub => ArithmeticKind::Subtract,
            _ => continue,
        };
        let result = module.value(operation.result).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "arithmetic operation {operation_id:?} has no result"
            ))
        })?;
        let Some(activation) = uses.activation(operation.result)? else {
            continue;
        };
        let key = SourceKey {
            kind,
            width: result.ty.width(),
            signed: result.ty.is_signed(),
            state: result.ty.state() as u8,
        };
        buckets.entry(key).or_default().push(ShareCandidate {
            operation: operation_id,
            result: operation.result,
            inputs: [left, right],
            kind,
            activation,
        });
    }

    let mut groups = Vec::new();
    for candidates in buckets.values_mut() {
        candidates.sort_by_key(|candidate| candidate.operation);
        groups.extend(exclusive_groups(module, candidates)?);
    }
    rewrite::materialize_groups(module, groups)
}

fn exclusive_groups(
    module: &word::WordModule,
    candidates: &[ShareCandidate],
) -> Result<Vec<Vec<ShareCandidate>>, crate::SynthError> {
    let mut consumed = vec![false; candidates.len()];
    let mut groups = Vec::new();
    for seed in 0..candidates.len() {
        if consumed[seed] {
            continue;
        }
        let mut group = vec![candidates[seed].clone()];
        for (index, candidate) in candidates.iter().enumerate().skip(seed + 1) {
            if consumed[index] {
                continue;
            }
            if group
                .iter()
                .all(|member| member.activation.is_exclusive(&candidate.activation))
            {
                group.push(candidate.clone());
            }
        }
        if group.len() < 2 || guard_depends_on_group(module, &group)? {
            continue;
        }
        for (index, candidate) in candidates.iter().enumerate() {
            if group
                .iter()
                .any(|member| member.operation == candidate.operation)
            {
                consumed[index] = true;
            }
        }
        groups.push(group);
    }
    Ok(groups)
}

fn guard_depends_on_group(
    module: &word::WordModule,
    group: &[ShareCandidate],
) -> Result<bool, crate::SynthError> {
    for candidate in group {
        for condition in candidate.activation.conditions() {
            for member in group {
                if value_depends_on(module, condition, member.result)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn value_depends_on(
    module: &word::WordModule,
    value: word::ValueId,
    target: word::ValueId,
) -> Result<bool, crate::SynthError> {
    let mut pending = vec![value];
    let mut visited = vec![false; module.values().len()];
    while let Some(value) = pending.pop() {
        if value == target {
            return Ok(true);
        }
        let slot = visited.get_mut(value.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "sharing guard references unknown value {value:?}"
            ))
        })?;
        if std::mem::replace(slot, true) {
            continue;
        }
        let value = module
            .value(value)
            .ok_or_else(|| crate::SynthError::invariant("sharing guard value disappeared"))?;
        let word::ValueKind::Operation(operation) = value.kind else {
            continue;
        };
        let operation = module.operation(operation).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "sharing guard references unknown operation {operation:?}"
            ))
        })?;
        pending.extend(crate::word::operation_inputs(&operation.kind));
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word::{LValue, LogicStateKind, PortDirection, SourceSpan, WordType};

    #[test]
    fn shares_source_identical_operations_under_exclusive_guards() {
        let mut module = word::WordModule::new("top");
        let data_ty = WordType::new(4, false, LogicStateKind::FourState).unwrap();
        let bit_ty = WordType::new(1, false, LogicStateKind::FourState).unwrap();
        let data_ports = ["a", "b", "c", "d"].map(|name| {
            module
                .add_port(name, PortDirection::Input, data_ty, SourceSpan::default())
                .unwrap()
        });
        let select = module
            .add_port(
                "select",
                PortDirection::Input,
                bit_ty,
                SourceSpan::default(),
            )
            .unwrap();
        let data = data_ports.map(|port| {
            module
                .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
                .unwrap()
        });
        let select = module
            .read_signal(module.port(select).unwrap().signal, SourceSpan::default())
            .unwrap();
        let source = SourceSpan::located("/rtl/top.sv", Some(12), None, "binary expression");
        let first = module
            .binary(word::BinaryOp::Add, data[0], data[1], source.clone())
            .unwrap();
        let second = module
            .binary(word::BinaryOp::Add, data[2], data[3], source)
            .unwrap();
        let original = module
            .mux(select, first, second, SourceSpan::default())
            .unwrap();
        let output = module
            .add_port("y", PortDirection::Output, data_ty, SourceSpan::default())
            .unwrap();
        module
            .connect(
                LValue::signal(module.port(output).unwrap().signal),
                original,
                SourceSpan::default(),
            )
            .unwrap();

        assert_eq!(share_muxed_arithmetic(&mut module).unwrap().len(), 1);
        let plan = crate::planning::operator::ArchitectureDecisions::for_private_region(
            &module,
            &[],
            crate::boolean::bitblast::implementation_providers().into(),
        )
        .unwrap();
        assert!(
            plan.operators()
                .iter()
                .any(|operator| { matches!(operator.kind(), crate::OperatorKind::Add) })
        );
        let mut lowered = module.clone();
        let mut provenance =
            crate::artifact::provenance::ProvenanceBuilder::new(&lowered, &plan).unwrap();
        crate::boolean::bitblast::bitblast_module_with_plan(&mut lowered, &plan, &mut provenance)
            .unwrap();
        let shared = module.connects()[0].value;
        let reference = module
            .mux(select, first, second, SourceSpan::default())
            .unwrap();
        let implementation = (0..4)
            .map(|bit| {
                module
                    .extract(shared, bit, 1, SourceSpan::default())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        opto_formal::prove_value_bits(&module, reference, &implementation)
            .unwrap()
            .require_proved()
            .unwrap();
    }
}
