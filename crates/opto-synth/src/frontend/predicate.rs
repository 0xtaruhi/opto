// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Canonical Boolean predicates used while normalizing procedural control.
//!
//! Source values that represent the same signal polarity share one AXM literal.
//! Cofactors and later Word materialization traverse iteratively, preserve
//! complement edges, and rebuild through canonical constructors so a control
//! predicate never depends on CFG path duplication or recursion depth.

use super::{BitVal, ConstBits, word};
use opto_ir::logic::{Lit, LogicBuilder, NodeKind};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
/// Canonical control condition with explicit constant cases.
pub(super) enum Predicate {
    Never,
    Always,
    Value { literal: Lit },
}

impl Predicate {
    fn from_literal(literal: Lit) -> Self {
        match literal {
            Lit::FALSE => Self::Never,
            Lit::TRUE => Self::Always,
            literal => Self::Value { literal },
        }
    }

    fn literal(self) -> Lit {
        match self {
            Self::Never => Lit::FALSE,
            Self::Always => Lit::TRUE,
            Self::Value { literal } => literal,
        }
    }
}

impl PartialEq for Predicate {
    fn eq(&self, other: &Self) -> bool {
        self.literal() == other.literal()
    }
}

impl Eq for Predicate {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MaterializedPredicate {
    Never,
    Always,
    Value(word::ValueId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Atom {
    Signal(word::SignalId),
    Value(word::ValueId),
}

#[derive(Debug, Clone, Copy)]
struct Variable {
    literal: Lit,
    source_active_high: bool,
}

/// Shared canonical graph and sparse Word-value correspondence for one lowering run.
pub(super) struct PredicateArena {
    builder: LogicBuilder,
    variables: HashMap<Atom, Variable>,
    input_sources: Vec<Option<word::ValueId>>,
    materialized: Vec<[Option<word::ValueId>; 2]>,
    represented: HashMap<word::ValueId, Lit>,
}

/// Cached cofactor of the arena under one literal assignment.
pub(super) struct PredicateRestriction {
    condition: Lit,
    condition_value: bool,
    restricted: Vec<[Option<Lit>; 2]>,
}

impl PredicateArena {
    pub(super) fn new() -> Self {
        Self {
            builder: LogicBuilder::new(),
            variables: HashMap::new(),
            input_sources: vec![None],
            materialized: vec![[None; 2]],
            represented: HashMap::new(),
        }
    }

    pub(super) fn value(
        &mut self,
        module: &word::WordModule,
        value: word::ValueId,
    ) -> Result<Predicate, crate::SynthError> {
        if let Some(literal) = self.represented.get(&value).copied() {
            return Ok(Predicate::from_literal(literal));
        }
        if let Some(stored) = module.value(value)
            && let word::ValueKind::Constant(bits) = &stored.kind
            && bits.width() == 1
        {
            return Ok(match bits.as_slice()[0] {
                BitVal::Zero => Predicate::Never,
                BitVal::One => Predicate::Always,
                BitVal::X | BitVal::Z => self.variable(Atom::Value(value), value, true)?,
            });
        }
        if let Some((signal, active_high)) =
            super::events::normalize_boolean_value(module, value, true)
        {
            return self.variable(Atom::Signal(signal), value, active_high);
        }
        let boolean_binary = module.value(value).and_then(|stored| {
            (stored.ty.width() == 1).then_some(())?;
            let word::ValueKind::Operation(operation) = stored.kind else {
                return None;
            };
            let operation = module.operation(operation)?;
            let word::OpKind::Binary { op, left, right } = operation.kind else {
                return None;
            };
            matches!(
                op,
                word::BinaryOp::LogicalAnd
                    | word::BinaryOp::BitAnd
                    | word::BinaryOp::LogicalOr
                    | word::BinaryOp::BitOr
                    | word::BinaryOp::BitXor
            )
            .then_some((op, left, right))
        });
        if let Some((op, left, right)) = boolean_binary
            && let (Some(left), Some(right)) = (
                self.simple_boolean_value(module, left)?,
                self.simple_boolean_value(module, right)?,
            )
        {
            let predicate = match op {
                word::BinaryOp::LogicalAnd | word::BinaryOp::BitAnd => self.and(left, right)?,
                word::BinaryOp::LogicalOr | word::BinaryOp::BitOr => self.or(left, right)?,
                word::BinaryOp::BitXor => self.xor(left, right)?,
                _ => unreachable!("filtered Boolean binary operation"),
            };
            self.remember_word(value, predicate.literal());
            return Ok(predicate);
        }
        self.variable(Atom::Value(value), value, true)
    }

    fn simple_boolean_value(
        &mut self,
        module: &word::WordModule,
        value: word::ValueId,
    ) -> Result<Option<Predicate>, crate::SynthError> {
        if let Some(stored) = module.value(value)
            && let word::ValueKind::Constant(bits) = &stored.kind
            && bits.width() == 1
        {
            return Ok(Some(match bits.as_slice()[0] {
                BitVal::Zero => Predicate::Never,
                BitVal::One => Predicate::Always,
                BitVal::X | BitVal::Z => self.variable(Atom::Value(value), value, true)?,
            }));
        }
        let Some((signal, active_high)) =
            super::events::normalize_boolean_value(module, value, true)
        else {
            return Ok(None);
        };
        self.variable(Atom::Signal(signal), value, active_high)
            .map(Some)
    }

    fn variable(
        &mut self,
        atom: Atom,
        source: word::ValueId,
        active_high: bool,
    ) -> Result<Predicate, crate::SynthError> {
        if let Some(variable) = self.variables.get(&atom).copied() {
            let literal = if active_high == variable.source_active_high {
                variable.literal
            } else {
                variable.literal.inverted()
            };
            self.remember_word(source, literal);
            return Ok(Predicate::from_literal(literal));
        }
        let literal = self
            .builder
            .input(source.raw())
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        self.input_sources.resize(self.builder.node_count(), None);
        self.input_sources[literal.node().index()] = Some(source);
        self.variables.insert(
            atom,
            Variable {
                literal,
                source_active_high: active_high,
            },
        );
        self.remember_word(source, literal);
        Ok(Predicate::from_literal(literal))
    }

    pub(super) fn not(predicate: Predicate) -> Predicate {
        Predicate::from_literal(predicate.literal().inverted())
    }

    pub(super) fn and(
        &mut self,
        left: Predicate,
        right: Predicate,
    ) -> Result<Predicate, crate::SynthError> {
        if left == Predicate::Never || right == Predicate::Never {
            return Ok(Predicate::Never);
        }
        if left == Predicate::Always {
            return Ok(right);
        }
        if right == Predicate::Always {
            return Ok(left);
        }
        let literal = self
            .builder
            .and(left.literal(), right.literal(), 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        Ok(Predicate::from_literal(literal))
    }

    pub(super) fn or(
        &mut self,
        left: Predicate,
        right: Predicate,
    ) -> Result<Predicate, crate::SynthError> {
        if left == Predicate::Always || right == Predicate::Always {
            return Ok(Predicate::Always);
        }
        if left == Predicate::Never {
            return Ok(right);
        }
        if right == Predicate::Never {
            return Ok(left);
        }
        let literal = self
            .builder
            .or(left.literal(), right.literal(), 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        Ok(Predicate::from_literal(literal))
    }

    fn xor(&mut self, left: Predicate, right: Predicate) -> Result<Predicate, crate::SynthError> {
        let literal = self
            .builder
            .xor(left.literal(), right.literal(), 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        Ok(Predicate::from_literal(literal))
    }

    pub(super) fn restriction(
        &self,
        condition: Predicate,
        condition_value: bool,
    ) -> Result<PredicateRestriction, crate::SynthError> {
        let Predicate::Value { literal: condition } = condition else {
            return Err(crate::SynthError::invariant(
                "a process predicate restriction requires a nonconstant condition",
            ));
        };
        Ok(PredicateRestriction {
            condition,
            condition_value,
            restricted: vec![[None; 2]; self.builder.node_count()],
        })
    }

    /// Computes a Boolean cofactor without round-tripping through Word IR.
    ///
    /// `condition` may itself be a canonical subgraph. Exact occurrences of
    /// that subgraph are replaced with the requested phase, while all other
    /// nodes are rebuilt through the canonicalizing builder.
    pub(super) fn restrict(
        &mut self,
        predicate: Predicate,
        restriction: &mut PredicateRestriction,
    ) -> Result<Predicate, crate::SynthError> {
        let Predicate::Value { literal: root } = predicate else {
            return Ok(predicate);
        };
        if let Some(result) = restriction.get(root) {
            return Ok(Predicate::from_literal(result));
        }
        let mut pending = vec![(root, false)];
        while let Some((literal, expanded)) = pending.pop() {
            if restriction.get(literal).is_some() {
                continue;
            }
            if literal == restriction.condition {
                restriction.set(literal, LogicBuilder::constant(restriction.condition_value));
                continue;
            }
            if literal == restriction.condition.inverted() {
                restriction.set(
                    literal,
                    LogicBuilder::constant(!restriction.condition_value),
                );
                continue;
            }
            if literal.node() == opto_ir::logic::NodeId::CONSTANT {
                restriction.set(literal, literal);
                continue;
            }
            if literal.is_inverted() {
                let positive = literal.positive();
                if !expanded {
                    pending.push((literal, true));
                    pending.push((positive, false));
                    continue;
                }
                let result = restriction.get(positive).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "positive process predicate cofactor was not evaluated",
                    )
                })?;
                restriction.set(literal, result.inverted());
                continue;
            }
            let node = literal.node();
            let kind = self.builder.kind(node).ok_or_else(|| {
                crate::SynthError::invariant("canonical process predicate node disappeared")
            })?;
            if !expanded {
                match kind {
                    NodeKind::Constant | NodeKind::Input => {}
                    NodeKind::And | NodeKind::Xor => {
                        pending.push((literal, true));
                        pending.push((self.fanin(node, 1)?, false));
                        pending.push((self.fanin(node, 0)?, false));
                        continue;
                    }
                    NodeKind::Mux => {
                        pending.push((literal, true));
                        pending.push((self.fanin(node, 2)?, false));
                        pending.push((self.fanin(node, 1)?, false));
                        pending.push((self.fanin(node, 0)?, false));
                        continue;
                    }
                }
            }
            let result = match kind {
                NodeKind::Constant => Lit::FALSE,
                NodeKind::Input => literal,
                NodeKind::And => {
                    let left = self.restricted_fanin(restriction, node, 0)?;
                    let right = self.restricted_fanin(restriction, node, 1)?;
                    self.builder
                        .and(left, right, 0)
                        .map_err(|error| crate::SynthError::capacity(error.to_string()))?
                }
                NodeKind::Xor => {
                    let left = self.restricted_fanin(restriction, node, 0)?;
                    let right = self.restricted_fanin(restriction, node, 1)?;
                    self.builder
                        .xor(left, right, 0)
                        .map_err(|error| crate::SynthError::capacity(error.to_string()))?
                }
                NodeKind::Mux => {
                    let select = self.restricted_fanin(restriction, node, 0)?;
                    let then_value = self.restricted_fanin(restriction, node, 1)?;
                    let else_value = self.restricted_fanin(restriction, node, 2)?;
                    self.builder
                        .mux(select, then_value, else_value, 0)
                        .map_err(|error| crate::SynthError::capacity(error.to_string()))?
                }
            };
            restriction.set(literal, result);
        }
        let result = restriction.get(root).ok_or_else(|| {
            crate::SynthError::invariant("process predicate cofactor root was not evaluated")
        })?;
        Ok(if result == root {
            predicate
        } else {
            Predicate::from_literal(result)
        })
    }

    /// Materializes one predicate into Word IR while preserving shared subgraphs.
    pub(super) fn materialize(
        &mut self,
        module: &mut word::WordModule,
        predicate: Predicate,
        source: &word::SourceSpan,
    ) -> Result<MaterializedPredicate, crate::SynthError> {
        Ok(match predicate {
            Predicate::Never => MaterializedPredicate::Never,
            Predicate::Always => MaterializedPredicate::Always,
            Predicate::Value { literal } => {
                MaterializedPredicate::Value(self.materialize_literal(module, literal, source)?)
            }
        })
    }

    fn materialize_literal(
        &mut self,
        module: &mut word::WordModule,
        root: Lit,
        source: &word::SourceSpan,
    ) -> Result<word::ValueId, crate::SynthError> {
        self.materialized
            .resize(self.builder.node_count(), [None; 2]);
        let mut pending = vec![(root, false)];
        while let Some((literal, expanded)) = pending.pop() {
            if self.cached(literal).is_some() {
                continue;
            }
            if let Some((left, right)) = self.demorgan_or(literal) {
                if !expanded {
                    pending.push((literal, true));
                    pending.push((right, false));
                    pending.push((left, false));
                    continue;
                }
                let value = module
                    .binary(
                        word::BinaryOp::LogicalOr,
                        self.cached(left).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "canonical disjunction left input was not materialized",
                            )
                        })?,
                        self.cached(right).ok_or_else(|| {
                            crate::SynthError::invariant(
                                "canonical disjunction right input was not materialized",
                            )
                        })?,
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?;
                self.set_cached(literal, value);
                continue;
            }
            if literal.is_inverted() {
                let positive = literal.positive();
                if !expanded {
                    pending.push((literal, true));
                    pending.push((positive, false));
                    continue;
                }
                let value = self.cached(positive).ok_or_else(|| {
                    crate::SynthError::invariant(
                        "canonical process predicate input was not materialized",
                    )
                })?;
                let inverted = module
                    .unary(word::UnaryOp::LogicalNot, value, source.clone())
                    .map_err(crate::SynthError::from)?;
                self.set_cached(literal, inverted);
                continue;
            }
            let node = literal.node();
            let kind = self.builder.kind(node).ok_or_else(|| {
                crate::SynthError::invariant("canonical process predicate node disappeared")
            })?;
            if !expanded {
                match kind {
                    NodeKind::Constant | NodeKind::Input => {}
                    NodeKind::And | NodeKind::Xor => {
                        pending.push((literal, true));
                        pending.push((self.fanin(node, 1)?, false));
                        pending.push((self.fanin(node, 0)?, false));
                        continue;
                    }
                    NodeKind::Mux => {
                        pending.push((literal, true));
                        pending.push((self.fanin(node, 2)?, false));
                        pending.push((self.fanin(node, 1)?, false));
                        pending.push((self.fanin(node, 0)?, false));
                        continue;
                    }
                }
            }
            let value = match kind {
                NodeKind::Constant => module
                    .constant(
                        ConstBits::from_bits(vec![BitVal::Zero])
                            .map_err(crate::SynthError::from)?,
                        word::WordType::bits(1).map_err(crate::SynthError::from)?,
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?,
                NodeKind::Input => self
                    .input_sources
                    .get(node.index())
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "canonical process predicate input has no Word source",
                        )
                    })?,
                NodeKind::And => module
                    .binary(
                        word::BinaryOp::LogicalAnd,
                        self.fanin_value(node, 0)?,
                        self.fanin_value(node, 1)?,
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?,
                NodeKind::Xor => module
                    .binary(
                        word::BinaryOp::BitXor,
                        self.fanin_value(node, 0)?,
                        self.fanin_value(node, 1)?,
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?,
                NodeKind::Mux => module
                    .mux(
                        self.fanin_value(node, 0)?,
                        self.fanin_value(node, 1)?,
                        self.fanin_value(node, 2)?,
                        source.clone(),
                    )
                    .map_err(crate::SynthError::from)?,
            };
            self.set_cached(literal, value);
        }
        self.cached(root).ok_or_else(|| {
            crate::SynthError::invariant("canonical process predicate root was not materialized")
        })
    }

    fn fanin_value(
        &self,
        node: opto_ir::logic::NodeId,
        index: usize,
    ) -> Result<word::ValueId, crate::SynthError> {
        let fanin = self.builder.fanin(node, index).ok_or_else(|| {
            crate::SynthError::invariant("canonical process predicate fanin disappeared")
        })?;
        self.cached(fanin).ok_or_else(|| {
            crate::SynthError::invariant("canonical process predicate fanin was not materialized")
        })
    }

    fn fanin(&self, node: opto_ir::logic::NodeId, index: usize) -> Result<Lit, crate::SynthError> {
        self.builder.fanin(node, index).ok_or_else(|| {
            crate::SynthError::invariant("canonical process predicate fanin disappeared")
        })
    }

    fn restricted_fanin(
        &self,
        restriction: &PredicateRestriction,
        node: opto_ir::logic::NodeId,
        index: usize,
    ) -> Result<Lit, crate::SynthError> {
        let fanin = self.fanin(node, index)?;
        restriction.get(fanin).ok_or_else(|| {
            crate::SynthError::invariant("process predicate cofactor fanin was not evaluated")
        })
    }

    fn demorgan_or(&self, literal: Lit) -> Option<(Lit, Lit)> {
        if !literal.is_inverted() || self.builder.kind(literal.node()) != Some(NodeKind::And) {
            return None;
        }
        let left = self.builder.fanin(literal.node(), 0)?;
        let right = self.builder.fanin(literal.node(), 1)?;
        (left.is_inverted() && right.is_inverted()).then_some((left.positive(), right.positive()))
    }

    fn cached(&self, literal: Lit) -> Option<word::ValueId> {
        self.materialized.get(literal.node().index())?[usize::from(literal.is_inverted())]
    }

    fn set_cached(&mut self, literal: Lit, value: word::ValueId) {
        if literal.node().index() >= self.materialized.len() {
            self.materialized
                .resize(literal.node().index() + 1, [None; 2]);
        }
        self.materialized[literal.node().index()][usize::from(literal.is_inverted())] = Some(value);
        self.remember_representation(value, literal);
    }

    fn remember_word(&mut self, value: word::ValueId, literal: Lit) {
        if self.cached(literal).is_none() {
            self.set_cached(literal, value);
        } else {
            self.remember_representation(value, literal);
        }
    }

    fn remember_representation(&mut self, value: word::ValueId, literal: Lit) {
        self.represented.insert(value, literal);
    }
}

impl PredicateRestriction {
    fn get(&self, literal: Lit) -> Option<Lit> {
        self.restricted
            .get(literal.node().index())?
            .get(usize::from(literal.is_inverted()))
            .copied()
            .flatten()
    }

    fn set(&mut self, literal: Lit, result: Lit) {
        if literal.node().index() >= self.restricted.len() {
            self.restricted
                .resize(literal.node().index() + 1, [None; 2]);
        }
        self.restricted[literal.node().index()][usize::from(literal.is_inverted())] = Some(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn represented_values_are_sparse_in_the_global_value_space() {
        let mut arena = PredicateArena::new();
        let value = word::ValueId::from_index(1_000_000).unwrap();

        arena.remember_representation(value, Lit::TRUE);

        assert_eq!(arena.represented.len(), 1);
        assert_eq!(arena.represented.get(&value), Some(&Lit::TRUE));
    }
}
