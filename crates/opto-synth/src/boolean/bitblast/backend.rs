// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use opto_ir::word;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarBit {
    Word(word::ValueId),
    Logic(crate::boolean::logic::network::LogicNodeId),
    DontCare(word::ValueId),
}

/// Scalar storage selected by the shared bit-lowering algorithms.
///
/// The regional implementation will store canonical AXM literals while the
/// global sequential shell continues to materialize scalar Word values.
pub(crate) trait BitBackend: Default + Send + Sync {
    fn import_word(&mut self, module: &word::WordModule, value: word::ValueId) -> ScalarBit;

    fn word_value(&self, bit: ScalarBit) -> Option<word::ValueId>;

    fn bit_type(
        &self,
        module: &word::WordModule,
        bit: ScalarBit,
    ) -> Result<word::WordType, crate::SynthError>;

    fn constant(&self, module: &word::WordModule, bit: ScalarBit) -> Option<bool>;

    /// Returns the retained AXM level when this backend owns a logic graph.
    /// Word shell lowering has no structural timing graph.
    fn structural_level(&self, bit: ScalarBit) -> Option<u32>;

    fn preserves_native_word_operations(&self) -> bool;

    fn follows_signal_drivers(&self) -> bool;

    fn treats_state_as_input(&self) -> bool;

    fn emit_unary(
        &mut self,
        module: &mut word::WordModule,
        op: word::UnaryOp,
        arg: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError>;

    fn emit_binary(
        &mut self,
        module: &mut word::WordModule,
        op: word::BinaryOp,
        left: ScalarBit,
        right: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError>;

    fn emit_mux(
        &mut self,
        module: &mut word::WordModule,
        cond: ScalarBit,
        then_value: ScalarBit,
        else_value: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError>;
}

#[derive(Default)]
pub(crate) struct WordBackend;

impl BitBackend for WordBackend {
    fn structural_level(&self, _bit: ScalarBit) -> Option<u32> {
        None
    }

    fn import_word(&mut self, _module: &word::WordModule, value: word::ValueId) -> ScalarBit {
        ScalarBit::Word(value)
    }

    fn word_value(&self, bit: ScalarBit) -> Option<word::ValueId> {
        let ScalarBit::Word(value) = bit else {
            return None;
        };
        Some(value)
    }

    fn bit_type(
        &self,
        module: &word::WordModule,
        bit: ScalarBit,
    ) -> Result<word::WordType, crate::SynthError> {
        let value = self.word_value(bit).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        module
            .value(value)
            .map(|value| value.ty)
            .ok_or_else(|| crate::SynthError::invariant("unknown scalar Word backend value"))
    }

    fn constant(&self, module: &word::WordModule, bit: ScalarBit) -> Option<bool> {
        let stored = module.value(self.word_value(bit)?)?;
        if stored.ty.width() != 1 {
            return None;
        }
        let word::ValueKind::Constant(bits) = &stored.kind else {
            return None;
        };
        match bits.bit_lsb(0)? {
            opto_ir::BitVal::Zero => Some(false),
            opto_ir::BitVal::One => Some(true),
            opto_ir::BitVal::X | opto_ir::BitVal::Z => None,
        }
    }

    fn preserves_native_word_operations(&self) -> bool {
        true
    }

    fn follows_signal_drivers(&self) -> bool {
        false
    }

    fn treats_state_as_input(&self) -> bool {
        false
    }

    fn emit_unary(
        &mut self,
        module: &mut word::WordModule,
        op: word::UnaryOp,
        arg: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError> {
        let arg = self.word_value(arg).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        let value = module
            .unary(op, arg, source.clone())
            .map_err(crate::SynthError::from)?;
        Ok((ScalarBit::Word(value), Some(value)))
    }

    fn emit_binary(
        &mut self,
        module: &mut word::WordModule,
        op: word::BinaryOp,
        left: ScalarBit,
        right: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError> {
        let left = self.word_value(left).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        let right = self.word_value(right).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        let value = module
            .binary(op, left, right, source.clone())
            .map_err(crate::SynthError::from)?;
        Ok((ScalarBit::Word(value), Some(value)))
    }

    fn emit_mux(
        &mut self,
        module: &mut word::WordModule,
        cond: ScalarBit,
        then_value: ScalarBit,
        else_value: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError> {
        let cond = self.word_value(cond).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        let then_value = self.word_value(then_value).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        let else_value = self.word_value(else_value).ok_or_else(|| {
            crate::SynthError::invariant("Word backend received a canonical AXM literal")
        })?;
        let value = module
            .mux(cond, then_value, else_value, source.clone())
            .map_err(crate::SynthError::from)?;
        Ok((ScalarBit::Word(value), Some(value)))
    }
}

pub(crate) struct AxmBackend {
    graph: crate::boolean::logic::network::LogicGraph,
    values: BTreeMap<word::ValueId, ScalarBit>,
    signals: BTreeMap<(word::SignalId, u32), ScalarBit>,
    representatives: BTreeMap<crate::boolean::logic::network::LogicNodeId, word::ValueId>,
    inputs: Vec<word::ValueId>,
}

impl Default for AxmBackend {
    fn default() -> Self {
        Self {
            graph: crate::boolean::logic::network::LogicGraph::new(),
            values: BTreeMap::new(),
            signals: BTreeMap::new(),
            representatives: BTreeMap::new(),
            inputs: Vec::new(),
        }
    }
}

impl AxmBackend {
    pub(crate) fn finish(
        mut self,
    ) -> (
        crate::boolean::logic::network::LogicGraph,
        Box<[word::ValueId]>,
    ) {
        self.graph.freeze();
        (self.graph, self.inputs.into_boxed_slice())
    }

    fn literal(
        bit: ScalarBit,
    ) -> Result<crate::boolean::logic::network::LogicNodeId, crate::SynthError> {
        let ScalarBit::Logic(literal) = bit else {
            return Err(crate::SynthError::invariant(
                "AXM backend received a scalar Word value",
            ));
        };
        Ok(literal)
    }

    pub(crate) fn binding_value(&self, bit: ScalarBit) -> Option<word::ValueId> {
        match bit {
            ScalarBit::Logic(literal) => self.representatives.get(&literal).copied(),
            ScalarBit::DontCare(value) => Some(value),
            ScalarBit::Word(_) => None,
        }
    }

    fn input(&mut self, value: word::ValueId, signal: Option<word::SignalRef>) -> ScalarBit {
        if let Some(bit) = self.values.get(&value).copied() {
            return bit;
        }
        if let Some(reference) = signal
            && let Some(bit) = self
                .signals
                .get(&(reference.signal, reference.lsb))
                .copied()
        {
            self.values.insert(value, bit);
            return bit;
        }
        let bit = ScalarBit::Logic(
            self.graph
                .variable(self.inputs.len())
                .expect("AXM input count fits compact graph storage"),
        );
        self.inputs.push(value);
        self.values.insert(value, bit);
        let ScalarBit::Logic(literal) = bit else {
            unreachable!("AXM input is always a logic literal")
        };
        self.representatives.insert(literal, value);
        if let Some(reference) = signal {
            self.signals.insert((reference.signal, reference.lsb), bit);
        }
        bit
    }
}

impl BitBackend for AxmBackend {
    fn structural_level(&self, bit: ScalarBit) -> Option<u32> {
        let ScalarBit::Logic(node) = bit else {
            return None;
        };
        Some(self.graph.construction_level(node))
    }

    fn import_word(&mut self, module: &word::WordModule, value: word::ValueId) -> ScalarBit {
        let Some(stored) = module.value(value) else {
            return self.input(value, None);
        };
        if let word::ValueKind::Constant(bits) = &stored.kind
            && stored.ty.width() == 1
            && let Some(bit) = bits.bit_lsb(0)
        {
            return match bit {
                opto_ir::BitVal::Zero | opto_ir::BitVal::One => {
                    ScalarBit::Logic(crate::boolean::logic::network::LogicGraph::constant(
                        matches!(bit, opto_ir::BitVal::One),
                    ))
                }
                opto_ir::BitVal::X => ScalarBit::DontCare(value),
                opto_ir::BitVal::Z => self.input(value, None),
            };
        }
        let signal = match stored.kind {
            word::ValueKind::Signal(reference) if reference.width() == 1 => Some(reference),
            _ => None,
        };
        self.input(value, signal)
    }

    fn word_value(&self, _bit: ScalarBit) -> Option<word::ValueId> {
        None
    }

    fn bit_type(
        &self,
        _module: &word::WordModule,
        _bit: ScalarBit,
    ) -> Result<word::WordType, crate::SynthError> {
        word::WordType::new(1, false, word::LogicStateKind::TwoState)
            .map_err(crate::SynthError::from)
    }

    fn constant(&self, _module: &word::WordModule, bit: ScalarBit) -> Option<bool> {
        let ScalarBit::Logic(literal) = bit else {
            return None;
        };
        (literal.index() == 0).then(|| literal.is_inverted())
    }

    fn preserves_native_word_operations(&self) -> bool {
        false
    }

    fn follows_signal_drivers(&self) -> bool {
        true
    }

    fn treats_state_as_input(&self) -> bool {
        true
    }

    fn emit_unary(
        &mut self,
        _module: &mut word::WordModule,
        op: word::UnaryOp,
        arg: ScalarBit,
        _source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError> {
        if matches!(arg, ScalarBit::DontCare(_)) {
            return Ok((arg, None));
        }
        let arg = Self::literal(arg)?;
        let value = match op {
            word::UnaryOp::BitNot | word::UnaryOp::LogicalNot => {
                crate::boolean::logic::network::LogicGraph::not(arg)
            }
            word::UnaryOp::ReductionAnd
            | word::UnaryOp::ReductionOr
            | word::UnaryOp::ReductionXor => arg,
        };
        Ok((ScalarBit::Logic(value), None))
    }

    fn emit_binary(
        &mut self,
        _module: &mut word::WordModule,
        op: word::BinaryOp,
        left: ScalarBit,
        right: ScalarBit,
        _source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError> {
        match (left, right) {
            (ScalarBit::DontCare(_), ScalarBit::DontCare(_)) => return Ok((left, None)),
            (ScalarBit::DontCare(_), _) => return Ok((right, None)),
            (_, ScalarBit::DontCare(_)) => return Ok((left, None)),
            _ => {}
        }
        let left = Self::literal(left)?;
        let right = Self::literal(right)?;
        let value = match op {
            word::BinaryOp::BitAnd | word::BinaryOp::LogicalAnd => self.graph.and(left, right),
            word::BinaryOp::BitOr | word::BinaryOp::LogicalOr => self.graph.or(left, right),
            word::BinaryOp::BitXor | word::BinaryOp::Ne => self.graph.xor(left, right),
            word::BinaryOp::Eq => self.graph.xor(left, right).inverted(),
            _ => {
                return Err(crate::SynthError::invariant(format!(
                    "non-Boolean operation {op:?} reached AXM scalar emission"
                )));
            }
        };
        Ok((ScalarBit::Logic(value), None))
    }

    fn emit_mux(
        &mut self,
        _module: &mut word::WordModule,
        cond: ScalarBit,
        then_value: ScalarBit,
        else_value: ScalarBit,
        _source: &word::SourceSpan,
    ) -> Result<(ScalarBit, Option<word::ValueId>), crate::SynthError> {
        if matches!(cond, ScalarBit::DontCare(_)) {
            return Ok((else_value, None));
        }
        match (then_value, else_value) {
            (ScalarBit::DontCare(_), ScalarBit::DontCare(_)) => {
                return Ok((then_value, None));
            }
            (ScalarBit::DontCare(_), _) => return Ok((else_value, None)),
            (_, ScalarBit::DontCare(_)) => return Ok((then_value, None)),
            _ => {}
        }
        let value = self.graph.mux(
            Self::literal(cond)?,
            Self::literal(then_value)?,
            Self::literal(else_value)?,
        );
        Ok((ScalarBit::Logic(value), None))
    }
}
