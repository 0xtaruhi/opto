// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Literal {
    pub(super) condition: word::ValueId,
    pub(super) positive: bool,
}

#[derive(Clone)]
pub(super) struct Activation {
    literals: Box<[Literal]>,
}

impl Activation {
    fn guarded(mut literals: Vec<Literal>) -> Option<Self> {
        literals.sort_by_key(|literal| literal.condition);
        let mut normalized = Vec::<Literal>::with_capacity(literals.len());
        for literal in literals {
            match normalized.last() {
                Some(previous) if previous.condition == literal.condition => {
                    if previous.positive != literal.positive {
                        return None;
                    }
                }
                _ => normalized.push(literal),
            }
        }
        (!normalized.is_empty()).then(|| Self {
            literals: normalized.into_boxed_slice(),
        })
    }

    pub(super) fn is_exclusive(&self, other: &Self) -> bool {
        let mut left = 0;
        let mut right = 0;
        while left < self.literals.len() && right < other.literals.len() {
            match self.literals[left]
                .condition
                .cmp(&other.literals[right].condition)
            {
                std::cmp::Ordering::Less => left += 1,
                std::cmp::Ordering::Greater => right += 1,
                std::cmp::Ordering::Equal => {
                    if self.literals[left].positive != other.literals[right].positive {
                        return true;
                    }
                    left += 1;
                    right += 1;
                }
            }
        }
        false
    }

    pub(super) fn literals(&self) -> &[Literal] {
        &self.literals
    }

    pub(super) fn conditions(&self) -> impl Iterator<Item = word::ValueId> + '_ {
        self.literals.iter().map(|literal| literal.condition)
    }
}

#[derive(Debug, Clone, Copy)]
enum Use {
    Root,
    Next(word::ValueId),
    Guarded {
        next: word::ValueId,
        literal: Literal,
    },
}

pub(super) struct UseIndex {
    rows: opto_core::PackedRows<Use>,
}

impl UseIndex {
    pub(super) fn build(module: &word::WordModule) -> Result<Self, crate::SynthError> {
        let observability = crate::word::uses::netlist_observability(module)?;
        let mut entries = Vec::<(word::ValueId, Use)>::new();
        for operation in module.operations() {
            append_operation_uses(
                &operation.kind,
                operation.result,
                observability.observes_value(operation.result)?,
                &mut entries,
            );
        }

        let mut signal_reads = vec![Vec::new(); module.signals().len()];
        for (index, value) in module.values().iter().enumerate() {
            if let word::ValueKind::Signal(reference) = value.kind {
                signal_reads[reference.signal.index()]
                    .push(word::ValueId::from_index(index).map_err(crate::SynthError::Word)?);
            }
        }
        for (index, connect) in module.connects().iter().enumerate() {
            if observability.observes_root_connect(index)? {
                entries.push((connect.value, Use::Root));
                entries.extend(
                    connect
                        .target
                        .dynamic
                        .map(|dynamic| (dynamic.offset, Use::Root)),
                );
            } else {
                entries.extend(
                    signal_reads[connect.target.signal.index()]
                        .iter()
                        .copied()
                        .map(|read| (connect.value, Use::Next(read))),
                );
            }
        }
        entries.extend(
            observability
                .non_connect_root_values()
                .iter()
                .copied()
                .map(|value| (value, Use::Root)),
        );
        entries.sort_by_key(|(value, _)| *value);

        Ok(Self {
            rows: opto_core::PackedRows::try_from_entries(
                module.values().len(),
                entries
                    .into_iter()
                    .map(|(value, usage)| (value.index(), usage)),
            )
            .map_err(|error| crate::SynthError::invariant(error.to_string()))?,
        })
    }

    pub(super) fn activation(
        &self,
        start: word::ValueId,
    ) -> Result<Option<Activation>, crate::SynthError> {
        let value_count = self.rows.row_count();
        let mut visited = vec![false; value_count];
        let mut literals = Vec::new();
        let mut current = start;

        loop {
            let visited = visited.get_mut(current.index()).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "activation references unknown value {current:?}"
                ))
            })?;
            if std::mem::replace(visited, true) {
                return Ok(None);
            }
            let [usage] = self.uses(current)? else {
                return Ok(None);
            };
            match *usage {
                Use::Root => return Ok(Activation::guarded(literals)),
                Use::Next(next) => current = next,
                Use::Guarded { next, literal } => {
                    literals.push(literal);
                    current = next;
                }
            }
        }
    }

    fn uses(&self, value: word::ValueId) -> Result<&[Use], crate::SynthError> {
        self.rows.get(value.index()).ok_or_else(|| {
            crate::SynthError::invariant(format!("use table references unknown value {value:?}"))
        })
    }
}

fn append_operation_uses(
    kind: &word::OpKind,
    result: word::ValueId,
    result_is_observable: bool,
    entries: &mut Vec<(word::ValueId, Use)>,
) {
    match kind {
        word::OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            entries.push((*cond, Use::Next(result)));
            entries.push((
                *then_value,
                Use::Guarded {
                    next: result,
                    literal: Literal {
                        condition: *cond,
                        positive: true,
                    },
                },
            ));
            entries.push((
                *else_value,
                Use::Guarded {
                    next: result,
                    literal: Literal {
                        condition: *cond,
                        positive: false,
                    },
                },
            ));
        }
        word::OpKind::Register(_) | word::OpKind::Latch(_) if result_is_observable => {
            entries.extend(
                crate::word::operation_inputs(kind)
                    .into_iter()
                    .map(|input| (input, Use::Root)),
            );
        }
        word::OpKind::Register(_) | word::OpKind::Latch(_) => {}
        _ => entries.extend(
            crate::word::operation_inputs(kind)
                .into_iter()
                .map(|input| (input, Use::Next(result))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_ir::word::{LogicStateKind, PortDirection, SourceSpan, WordType};

    fn input(module: &mut word::WordModule, name: &str, ty: WordType) -> word::ValueId {
        let port = module
            .add_port(name, PortDirection::Input, ty, SourceSpan::default())
            .unwrap();
        module
            .read_signal(module.port(port).unwrap().signal, SourceSpan::default())
            .unwrap()
    }

    #[test]
    fn rejects_reconvergent_activation_without_enumerating_paths() {
        let mut module = word::WordModule::new("top");
        let data_ty = WordType::new(8, false, LogicStateKind::FourState).unwrap();
        let bit_ty = WordType::new(1, false, LogicStateKind::FourState).unwrap();
        let left = input(&mut module, "left", data_ty);
        let right = input(&mut module, "right", data_ty);
        let arithmetic = module
            .binary(word::BinaryOp::Add, left, right, SourceSpan::default())
            .unwrap();
        let mut current = arithmetic;
        for index in 0..64 {
            let select = input(&mut module, &format!("select_{index}"), bit_ty);
            current = module
                .mux(select, current, current, SourceSpan::default())
                .unwrap();
        }

        let uses = UseIndex::build(&module).unwrap();
        assert!(uses.activation(arithmetic).unwrap().is_none());
    }

    #[test]
    fn register_input_is_an_activation_boundary() {
        let mut module = word::WordModule::new("top");
        let data_ty = WordType::new(8, false, LogicStateKind::FourState).unwrap();
        let bit_ty = WordType::new(1, false, LogicStateKind::FourState).unwrap();
        let left = input(&mut module, "left", data_ty);
        let right = input(&mut module, "right", data_ty);
        let clock = input(&mut module, "clock", bit_ty);
        let arithmetic = module
            .binary(word::BinaryOp::Add, left, right, SourceSpan::default())
            .unwrap();
        module
            .register(
                word::RegisterOp {
                    name: None,
                    d: arithmetic,
                    clock,
                    edge: word::Edge::Pos,
                    enable: None,
                    resets: Vec::new(),
                },
                SourceSpan::default(),
            )
            .unwrap();

        let uses = UseIndex::build(&module).unwrap();
        assert!(uses.activation(arithmetic).unwrap().is_none());
    }
}
