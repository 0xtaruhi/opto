// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use hashbrown::{HashMap, HashSet};
use opto_ir::{
    BitVal, ConstBits,
    logic::{Lit, LogicBuilder},
    word,
};

mod arithmetic;

const MAX_SYMBOLIC_NODES: usize = 262_144;

#[derive(Debug)]
pub(super) enum SymbolicError {
    Unsupported,
    Synthesis(crate::SynthError),
}

impl From<crate::SynthError> for SymbolicError {
    fn from(error: crate::SynthError) -> Self {
        Self::Synthesis(error)
    }
}

type SymbolicResult<T> = Result<T, SymbolicError>;

pub(super) struct WordLogicEncoder<'module> {
    module: &'module word::WordModule,
    signal_drivers: Option<&'module crate::word::signal_driver::SignalDriverIndex>,
    builder: LogicBuilder,
    values: Vec<Option<Vec<Lit>>>,
    external_signals: HashMap<(word::SignalId, u32), Lit>,
    external_order: Vec<(word::SignalId, u32)>,
    bound_signals: HashMap<word::SignalId, Vec<Lit>>,
}

impl<'module> WordLogicEncoder<'module> {
    pub(super) fn new(module: &'module word::WordModule) -> Self {
        Self {
            module,
            signal_drivers: None,
            builder: LogicBuilder::new(),
            values: vec![None; module.values().len()],
            external_signals: HashMap::new(),
            external_order: Vec::new(),
            bound_signals: HashMap::new(),
        }
    }

    pub(super) fn with_signal_drivers(
        module: &'module word::WordModule,
        signal_drivers: &'module crate::word::signal_driver::SignalDriverIndex,
    ) -> Self {
        Self {
            signal_drivers: Some(signal_drivers),
            ..Self::new(module)
        }
    }

    #[cfg(test)]
    pub(super) fn begin_unbound(&mut self) {
        self.values.fill(None);
        self.bound_signals.clear();
    }

    pub(super) fn begin_state(
        &mut self,
        signal: word::SignalId,
        state: &ConstBits,
    ) -> SymbolicResult<()> {
        self.values.fill(None);
        self.bound_signals.clear();
        let width = self.signal_width(signal)?;
        let width = u32::try_from(width)
            .map_err(|_| crate::SynthError::capacity("symbolic FSM state width"))?;
        if state.width() != width {
            return Err(crate::SynthError::invariant(format!(
                "symbolic FSM state width mismatch: signal={width}, state={}",
                state.width()
            ))
            .into());
        }
        let bits = (0..width)
            .map(|bit| match state.bit_lsb(bit) {
                Some(BitVal::Zero) => Ok(Lit::FALSE),
                Some(BitVal::One) => Ok(Lit::TRUE),
                Some(BitVal::X | BitVal::Z) => Err(SymbolicError::Unsupported),
                None => {
                    Err(crate::SynthError::invariant("symbolic FSM state is missing a bit").into())
                }
            })
            .collect::<SymbolicResult<Vec<_>>>()?;
        self.bound_signals.insert(signal, bits);
        Ok(())
    }

    pub(super) fn values(&mut self, roots: &[word::ValueId]) -> SymbolicResult<Vec<Lit>> {
        let mut bits = Vec::new();
        for &root in roots {
            bits.extend(self.value(root)?);
        }
        Ok(bits)
    }

    pub(super) fn register_next(
        &mut self,
        register: &word::RegisterOp,
        state: word::SignalId,
    ) -> SymbolicResult<Vec<Lit>> {
        let held = self.signal_bits(state)?;
        let mut next = self.value(register.d)?;
        if held.len() != next.len() {
            return Err(crate::SynthError::invariant(format!(
                "symbolic register update width mismatch: state={}, data={}",
                held.len(),
                next.len()
            ))
            .into());
        }
        if let Some(enable) = register.enable {
            let active = self.control(enable.value, enable.active_high)?;
            next = next
                .into_iter()
                .zip(&held)
                .map(|(next, &held)| self.select(active, next, held))
                .collect::<SymbolicResult<Vec<_>>>()?;
        }
        for reset in register.resets.iter().rev() {
            let active = self.control(reset.value, reset.active_high)?;
            let reset_value = self.value(reset.reset_value)?;
            if reset_value.len() != next.len() {
                return Err(crate::SynthError::invariant(format!(
                    "symbolic register reset width mismatch: state={}, reset={}",
                    next.len(),
                    reset_value.len()
                ))
                .into());
            }
            next = reset_value
                .into_iter()
                .zip(next)
                .map(|(reset, next)| self.select(active, reset, next))
                .collect::<SymbolicResult<Vec<_>>>()?;
        }
        Ok(next)
    }

    pub(super) fn equals_constant(
        &mut self,
        value: &[Lit],
        constant: &ConstBits,
    ) -> SymbolicResult<Lit> {
        if constant.width() as usize != value.len() {
            return Err(crate::SynthError::invariant(format!(
                "symbolic comparison width mismatch: value={}, constant={}",
                value.len(),
                constant.width()
            ))
            .into());
        }
        let mut equal = Lit::TRUE;
        for (bit, &value) in value.iter().enumerate() {
            let bit = u32::try_from(bit).map_err(|_| {
                crate::SynthError::capacity("symbolic comparison bit exceeds 32-bit capacity")
            })?;
            let expected = match constant.bit_lsb(bit) {
                Some(BitVal::Zero) => false,
                Some(BitVal::One) => true,
                Some(BitVal::X | BitVal::Z) => return Err(SymbolicError::Unsupported),
                None => {
                    return Err(crate::SynthError::invariant(
                        "symbolic comparison constant is missing a bit",
                    )
                    .into());
                }
            };
            let matches = if expected { value } else { value.inverted() };
            equal = self.and(equal, matches)?;
        }
        Ok(equal)
    }

    pub(super) fn partition(
        &mut self,
        value: &[Lit],
        states: &[ConstBits],
        classes: &[usize],
    ) -> SymbolicResult<Vec<Lit>> {
        if states.is_empty() || states.len() != classes.len() {
            return Err(crate::SynthError::invariant(
                "symbolic FSM partition requires one class per non-empty state set",
            )
            .into());
        }
        let class_count = classes.iter().copied().max().map_or(0, |class| class + 1);
        let width = (usize::BITS - class_count.saturating_sub(1).leading_zeros()).max(1) as usize;
        let mut bits = (0..width)
            .map(|bit| Self::constant_bit(classes[0], bit))
            .collect::<Vec<_>>();
        for (state, &class) in states.iter().zip(classes) {
            let selected = self.equals_constant(value, state)?;
            for (bit, output) in bits.iter_mut().enumerate() {
                *output = self.select(selected, Self::constant_bit(class, bit), *output)?;
            }
        }
        Ok(bits)
    }

    fn constant_bit(value: usize, bit: usize) -> Lit {
        if value & (1usize << bit) == 0 {
            Lit::FALSE
        } else {
            Lit::TRUE
        }
    }

    fn value(&mut self, root: word::ValueId) -> SymbolicResult<Vec<Lit>> {
        if let Some(bits) = self.values.get(root.index()).and_then(Clone::clone) {
            return Ok(bits);
        }
        let mut active = HashSet::new();
        let mut pending = vec![(root, false)];
        while let Some((value_id, exiting)) = pending.pop() {
            if self
                .values
                .get(value_id.index())
                .is_some_and(Option::is_some)
            {
                continue;
            }
            if exiting {
                if !active.remove(&value_id) {
                    continue;
                }
                let bits = self.encode_value(value_id)?;
                let slot = self.values.get_mut(value_id.index()).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "symbolic encoder has no cache slot for value {value_id:?}"
                    ))
                })?;
                *slot = Some(bits);
                continue;
            }
            let value = self.module.value(value_id).ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "symbolic encoder references unknown value {value_id:?}"
                ))
            })?;
            match value.kind {
                word::ValueKind::Signal(reference)
                    if self.reference_drivers(reference).is_some() =>
                {
                    if !active.insert(value_id) {
                        return Err(crate::SynthError::invariant(
                            "symbolic encoder found a combinational signal cycle",
                        )
                        .into());
                    }
                    pending.push((value_id, true));
                    pending.extend(
                        self.reference_drivers(reference)
                            .expect("signal drivers remain available")
                            .into_iter()
                            .map(|driver| (driver, false)),
                    );
                }
                word::ValueKind::Signal(_) | word::ValueKind::Constant(_) => {
                    let bits = self.encode_value(value_id)?;
                    let slot = self.values.get_mut(value_id.index()).ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "symbolic encoder has no cache slot for value {value_id:?}"
                        ))
                    })?;
                    *slot = Some(bits);
                }
                word::ValueKind::Operation(operation_id) => {
                    let operation = self.module.operation(operation_id).ok_or_else(|| {
                        crate::SynthError::invariant(format!(
                            "symbolic encoder references unknown operation {operation_id:?}"
                        ))
                    })?;
                    if matches!(
                        operation.kind,
                        word::OpKind::Register(_) | word::OpKind::Latch(_)
                    ) {
                        return Err(SymbolicError::Unsupported);
                    }
                    if !active.insert(value_id) {
                        return Err(crate::SynthError::invariant(
                            "symbolic encoder found a combinational cycle",
                        )
                        .into());
                    }
                    pending.push((value_id, true));
                    pending.extend(
                        crate::word::operation_inputs(&operation.kind)
                            .into_iter()
                            .rev()
                            .map(|input| (input, false)),
                    );
                }
            }
        }
        self.values
            .get(root.index())
            .and_then(Clone::clone)
            .ok_or_else(|| {
                crate::SynthError::invariant("symbolic encoder did not produce its root").into()
            })
    }

    fn encode_value(&mut self, id: word::ValueId) -> SymbolicResult<Vec<Lit>> {
        let value = self.module.value(id).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "symbolic encoder references unknown value {id:?}"
            ))
        })?;
        let ty = value.ty;
        let kind = value.kind.clone();
        match kind {
            word::ValueKind::Signal(reference) => {
                if let Some(driver_bits) = self.reference_bits(reference) {
                    let mut literals = Vec::with_capacity(driver_bits.len());
                    for (driver, bit) in driver_bits {
                        let cached = self.cached(driver)?;
                        let literal = cached.get(bit as usize).copied().ok_or_else(|| {
                            crate::SynthError::invariant(
                                "symbolic signal reference exceeds its driver width",
                            )
                        })?;
                        literals.push(literal);
                    }
                    Ok(literals)
                } else {
                    (0..reference.width())
                        .map(|offset| {
                            let bit = reference.lsb.checked_add(offset).ok_or_else(|| {
                                crate::SynthError::capacity(
                                    "symbolic signal reference exceeds 32-bit capacity",
                                )
                            })?;
                            self.signal(reference.signal, bit)
                        })
                        .collect()
                }
            }
            word::ValueKind::Constant(constant) => (0..ty.width())
                .map(|bit| match constant.bit_lsb(bit) {
                    Some(BitVal::Zero) => Ok(Lit::FALSE),
                    Some(BitVal::One) => Ok(Lit::TRUE),
                    Some(BitVal::X | BitVal::Z) => Err(SymbolicError::Unsupported),
                    None => Err(crate::SynthError::invariant(format!(
                        "symbolic constant has no bit {bit}"
                    ))
                    .into()),
                })
                .collect(),
            word::ValueKind::Operation(operation_id) => {
                let operation = self.module.operation(operation_id).ok_or_else(|| {
                    crate::SynthError::invariant(format!(
                        "symbolic encoder references unknown operation {operation_id:?}"
                    ))
                })?;
                self.operation(&operation.kind.clone(), ty)
            }
        }
    }

    fn operation(&mut self, kind: &word::OpKind, ty: word::WordType) -> SymbolicResult<Vec<Lit>> {
        match kind {
            word::OpKind::Unary { op, arg } => self.unary(*op, *arg),
            word::OpKind::Binary { op, left, right } => self.binary(*op, *left, *right, ty),
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => self.mux(*cond, *then_value, *else_value),
            word::OpKind::Concat { parts } => {
                let mut result = Vec::new();
                for &part in parts.iter().rev() {
                    result.extend(self.cached(part)?);
                }
                Ok(result)
            }
            word::OpKind::Extract { value, lsb, width } => {
                let bits = self.cached(*value)?;
                let start = *lsb as usize;
                let end = start.checked_add(width.get() as usize).ok_or_else(|| {
                    crate::SynthError::capacity(
                        "symbolic extract range exceeds addressable capacity",
                    )
                })?;
                bits.get(start..end).map(<[Lit]>::to_vec).ok_or_else(|| {
                    crate::SynthError::invariant("symbolic extract exceeds source width").into()
                })
            }
            word::OpKind::DynamicExtract {
                value,
                offset,
                width,
            } => self.dynamic_extract(*value, *offset, width.get()),
            word::OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => self.dynamic_insert(*value, *offset, *replacement),
            word::OpKind::Cast {
                kind,
                value,
                target,
            } => self.cast(*kind, *value, *target),
            word::OpKind::Register(_) | word::OpKind::Latch(_) => Err(SymbolicError::Unsupported),
        }
    }

    fn unary(&mut self, op: word::UnaryOp, arg: word::ValueId) -> SymbolicResult<Vec<Lit>> {
        let bits = self.cached(arg)?;
        match op {
            word::UnaryOp::BitNot => Ok(bits.into_iter().map(Lit::inverted).collect()),
            word::UnaryOp::LogicalNot => Ok(vec![self.reduce_or(&bits)?.inverted()]),
            word::UnaryOp::ReductionAnd => Ok(vec![self.reduce_and(&bits)?]),
            word::UnaryOp::ReductionOr => Ok(vec![self.reduce_or(&bits)?]),
            word::UnaryOp::ReductionXor => Ok(vec![self.reduce_xor(&bits)?]),
        }
    }

    fn binary(
        &mut self,
        op: word::BinaryOp,
        left: word::ValueId,
        right: word::ValueId,
        ty: word::WordType,
    ) -> SymbolicResult<Vec<Lit>> {
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let left = self.cached(left)?;
        let right = self.cached(right)?;
        match op {
            word::BinaryOp::Add | word::BinaryOp::Sub => self.add_sub(
                &left,
                left_ty,
                &right,
                right_ty,
                ty,
                op == word::BinaryOp::Sub,
            ),
            word::BinaryOp::BitAnd | word::BinaryOp::BitOr | word::BinaryOp::BitXor => {
                let left = Self::resize(&left, left_ty, ty)?;
                let right = Self::resize(&right, right_ty, ty)?;
                left.into_iter()
                    .zip(right)
                    .map(|(left, right)| match op {
                        word::BinaryOp::BitAnd => self.and(left, right),
                        word::BinaryOp::BitOr => self.or(left, right),
                        word::BinaryOp::BitXor => self.xor(left, right),
                        _ => unreachable!(),
                    })
                    .collect()
            }
            word::BinaryOp::LogicalAnd | word::BinaryOp::LogicalOr => {
                let left = self.reduce_or(&left)?;
                let right = self.reduce_or(&right)?;
                Ok(vec![if op == word::BinaryOp::LogicalAnd {
                    self.and(left, right)?
                } else {
                    self.or(left, right)?
                }])
            }
            word::BinaryOp::Eq | word::BinaryOp::Ne => {
                let width = left_ty.width().max(right_ty.width());
                let compare_ty = word::WordType::new(
                    width,
                    left_ty.is_signed() && right_ty.is_signed(),
                    ty.state(),
                )
                .map_err(crate::SynthError::from)?;
                let left = Self::resize(&left, left_ty, compare_ty)?;
                let right = Self::resize(&right, right_ty, compare_ty)?;
                let differences = left
                    .into_iter()
                    .zip(right)
                    .map(|(left, right)| self.xor(left, right))
                    .collect::<SymbolicResult<Vec<_>>>()?;
                let different = self.reduce_or(&differences)?;
                Ok(vec![if op == word::BinaryOp::Ne {
                    different
                } else {
                    different.inverted()
                }])
            }
            word::BinaryOp::Mul => self.multiply(&left, left_ty, &right, right_ty, ty),
            word::BinaryOp::Div | word::BinaryOp::Mod => self.divide(
                &left,
                left_ty,
                &right,
                right_ty,
                ty,
                op == word::BinaryOp::Mod,
            ),
            word::BinaryOp::Lt | word::BinaryOp::Le | word::BinaryOp::Gt | word::BinaryOp::Ge => {
                self.compare(op, &left, left_ty, &right, right_ty, ty)
            }
            word::BinaryOp::Shl | word::BinaryOp::Shr | word::BinaryOp::Ashr => {
                self.shift(op, &left, left_ty, &right, ty)
            }
        }
    }

    fn mux(
        &mut self,
        cond: word::ValueId,
        then_value: word::ValueId,
        else_value: word::ValueId,
    ) -> SymbolicResult<Vec<Lit>> {
        let cond = self.cached(cond)?;
        let [cond]: [Lit; 1] = cond.try_into().map_err(|cond: Vec<_>| {
            crate::SynthError::invariant(format!(
                "symbolic mux condition has {} bits, expected one",
                cond.len()
            ))
        })?;
        let then_value = self.cached(then_value)?;
        let else_value = self.cached(else_value)?;
        if then_value.len() != else_value.len() {
            return Err(crate::SynthError::invariant(
                "symbolic mux branches have different widths",
            )
            .into());
        }
        then_value
            .into_iter()
            .zip(else_value)
            .map(|(then_value, else_value)| self.select(cond, then_value, else_value))
            .collect()
    }

    fn cast(
        &mut self,
        kind: word::CastKind,
        value: word::ValueId,
        target: word::WordType,
    ) -> SymbolicResult<Vec<Lit>> {
        let mut bits = self.cached(value)?;
        let width = target.width() as usize;
        match kind {
            word::CastKind::Truncate => bits.truncate(width),
            word::CastKind::ZeroExtend | word::CastKind::SignExtend => {
                let extension = if kind == word::CastKind::SignExtend {
                    *bits.last().ok_or_else(|| {
                        crate::SynthError::invariant("symbolic cast cannot extend an empty value")
                    })?
                } else {
                    Lit::FALSE
                };
                bits.resize(width, extension);
            }
        }
        Ok(bits)
    }

    fn dynamic_extract(
        &mut self,
        value: word::ValueId,
        offset: word::ValueId,
        width: u32,
    ) -> SymbolicResult<Vec<Lit>> {
        let source_width = self
            .module
            .value(value)
            .map(|value| value.ty.width() as usize)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "symbolic extract references unknown value {value:?}"
                ))
            })?;
        let result_width = width as usize;
        let available_offsets = source_width.checked_sub(result_width).ok_or_else(|| {
            crate::SynthError::invariant("symbolic dynamic extract exceeds source width")
        })?;
        let max_offset = word::unsigned_value_range(self.module, offset)
            .map(word::UnsignedValueRange::maximum)
            .ok_or(SymbolicError::Unsupported)?;
        let value = self.cached(value)?;
        let offset = self.cached(offset)?;
        let selection_max = max_offset.min(available_offsets as u128);
        let in_range = if max_offset > available_offsets as u128 {
            Some(self.unsigned_at_most_constant(&offset, available_offsets)?)
        } else {
            None
        };
        (0..result_width)
            .map(|result_bit| {
                let mut candidates = value[result_bit..].to_vec();
                for (stage, &control) in offset.iter().enumerate() {
                    let stage = u32::try_from(stage).map_err(|_| {
                        crate::SynthError::capacity("symbolic dynamic extract stage count")
                    })?;
                    let distance = 1usize.checked_shl(stage).ok_or_else(|| {
                        crate::SynthError::capacity(
                            "symbolic dynamic extract distance exceeds capacity",
                        )
                    })?;
                    if distance as u128 > selection_max {
                        continue;
                    }
                    for index in 0..candidates.len() - distance {
                        candidates[index] =
                            self.select(control, candidates[index + distance], candidates[index])?;
                    }
                }
                let selected = candidates.first().copied().ok_or_else(|| {
                    crate::SynthError::invariant("symbolic dynamic extract has no source candidate")
                })?;
                in_range.map_or(Ok(selected), |valid| {
                    self.select(valid, selected, Lit::FALSE)
                })
            })
            .collect()
    }

    fn unsigned_at_most_constant(&mut self, value: &[Lit], maximum: usize) -> SymbolicResult<Lit> {
        let mut equal = Lit::TRUE;
        let mut greater = Lit::FALSE;
        for (index, &bit) in value.iter().enumerate().rev() {
            let maximum_bit = index < usize::BITS as usize && maximum & (1usize << index) != 0;
            if !maximum_bit {
                let greater_here = self.and(equal, bit)?;
                greater = self.or(greater, greater_here)?;
            }
            equal = self.and(equal, if maximum_bit { bit } else { bit.inverted() })?;
        }
        Ok(greater.inverted())
    }

    fn dynamic_insert(
        &mut self,
        value: word::ValueId,
        offset: word::ValueId,
        replacement: word::ValueId,
    ) -> SymbolicResult<Vec<Lit>> {
        let value = self.cached(value)?;
        let offset = self.cached(offset)?;
        let replacement = self.cached(replacement)?;
        if replacement.len() > value.len() {
            return Err(crate::SynthError::invariant(
                "symbolic dynamic insert replacement exceeds source width",
            )
            .into());
        }
        let mut shifted = vec![Lit::FALSE; value.len()];
        shifted[..replacement.len()].copy_from_slice(&replacement);
        let mut mask = vec![Lit::FALSE; value.len()];
        mask[..replacement.len()].fill(Lit::TRUE);
        for (stage, control) in offset.into_iter().enumerate() {
            let stage = u32::try_from(stage)
                .map_err(|_| crate::SynthError::capacity("symbolic dynamic insert stage count"))?;
            let distance = 1usize.checked_shl(stage).ok_or_else(|| {
                crate::SynthError::capacity("symbolic dynamic insert distance exceeds capacity")
            })?;
            for index in (0..value.len()).rev() {
                let shifted_value = index
                    .checked_sub(distance)
                    .map_or(Lit::FALSE, |source| shifted[source]);
                let shifted_mask = index
                    .checked_sub(distance)
                    .map_or(Lit::FALSE, |source| mask[source]);
                shifted[index] = self.select(control, shifted_value, shifted[index])?;
                mask[index] = self.select(control, shifted_mask, mask[index])?;
            }
        }
        value
            .into_iter()
            .zip(shifted)
            .zip(mask)
            .map(|((original, replacement), select)| self.select(select, replacement, original))
            .collect()
    }

    fn control(&mut self, value: word::ValueId, active_high: bool) -> SymbolicResult<Lit> {
        let bits = self.value(value)?;
        let [value]: [Lit; 1] = bits.try_into().map_err(|bits: Vec<_>| {
            crate::SynthError::invariant(format!(
                "symbolic register control has {} bits, expected one",
                bits.len()
            ))
        })?;
        Ok(if active_high { value } else { value.inverted() })
    }

    fn signal_width(&self, signal: word::SignalId) -> SymbolicResult<usize> {
        self.module
            .signal(signal)
            .map(|signal| signal.ty.width() as usize)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "symbolic encoder references unknown signal {signal:?}"
                ))
                .into()
            })
    }

    fn reference_drivers(&self, reference: word::SignalRef) -> Option<Vec<word::ValueId>> {
        if self.bound_signals.contains_key(&reference.signal) {
            return None;
        }
        self.signal_drivers?.reference_drivers(reference)
    }

    fn reference_bits(&self, reference: word::SignalRef) -> Option<Vec<(word::ValueId, u32)>> {
        if self.bound_signals.contains_key(&reference.signal) {
            return None;
        }
        self.signal_drivers?.resolve_reference(reference)
    }

    fn signal_bits(&mut self, signal: word::SignalId) -> SymbolicResult<Vec<Lit>> {
        let width = self.signal_width(signal)?;
        (0..width)
            .map(|bit| {
                let bit = u32::try_from(bit).map_err(|_| {
                    crate::SynthError::capacity("symbolic signal bit exceeds 32-bit capacity")
                })?;
                self.signal(signal, bit)
            })
            .collect()
    }

    fn signal(&mut self, signal: word::SignalId, bit: u32) -> SymbolicResult<Lit> {
        if let Some(bits) = self.bound_signals.get(&signal) {
            return bits.get(bit as usize).copied().ok_or_else(|| {
                crate::SynthError::invariant(format!("symbolic signal {signal:?} has no bit {bit}"))
                    .into()
            });
        }
        if let Some(&literal) = self.external_signals.get(&(signal, bit)) {
            return Ok(literal);
        }
        let origin = u32::try_from(self.external_signals.len()).map_err(|_| {
            crate::SynthError::capacity("symbolic input count exceeds 32-bit capacity")
        })?;
        let literal = self
            .builder
            .input(origin)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        self.check_budget()?;
        self.external_signals.insert((signal, bit), literal);
        self.external_order.push((signal, bit));
        Ok(literal)
    }

    fn cached(&self, value: word::ValueId) -> SymbolicResult<Vec<Lit>> {
        self.values
            .get(value.index())
            .and_then(Clone::clone)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "symbolic operand {value:?} was not encoded before its user"
                ))
                .into()
            })
    }

    fn value_type(&self, value: word::ValueId) -> SymbolicResult<word::WordType> {
        self.module
            .value(value)
            .map(|value| value.ty)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "symbolic encoder references unknown value {value:?}"
                ))
                .into()
            })
    }

    fn resize(
        bits: &[Lit],
        source: word::WordType,
        target: word::WordType,
    ) -> SymbolicResult<Vec<Lit>> {
        let width = target.width() as usize;
        let mut result = bits[..bits.len().min(width)].to_vec();
        if result.len() < width {
            let extension = if target.is_signed() && source.is_signed() {
                *bits.last().ok_or_else(|| {
                    crate::SynthError::invariant(
                        "symbolic resize cannot sign-extend an empty value",
                    )
                })?
            } else {
                Lit::FALSE
            };
            result.resize(width, extension);
        }
        Ok(result)
    }

    fn and(&mut self, left: Lit, right: Lit) -> SymbolicResult<Lit> {
        let result = self
            .builder
            .and(left, right, 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        self.check_budget()?;
        Ok(result)
    }

    fn or(&mut self, left: Lit, right: Lit) -> SymbolicResult<Lit> {
        let result = self
            .builder
            .or(left, right, 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        self.check_budget()?;
        Ok(result)
    }

    fn xor(&mut self, left: Lit, right: Lit) -> SymbolicResult<Lit> {
        let result = self
            .builder
            .xor(left, right, 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        self.check_budget()?;
        Ok(result)
    }

    fn select(&mut self, condition: Lit, when_true: Lit, when_false: Lit) -> SymbolicResult<Lit> {
        let result = self
            .builder
            .mux(condition, when_true, when_false, 0)
            .map_err(|error| crate::SynthError::capacity(error.to_string()))?;
        self.check_budget()?;
        Ok(result)
    }

    fn reduce_and(&mut self, values: &[Lit]) -> SymbolicResult<Lit> {
        let mut result = Lit::TRUE;
        for &value in values {
            result = self.and(result, value)?;
        }
        Ok(result)
    }

    fn reduce_or(&mut self, values: &[Lit]) -> SymbolicResult<Lit> {
        let mut result = Lit::FALSE;
        for &value in values {
            result = self.or(result, value)?;
        }
        Ok(result)
    }

    fn reduce_xor(&mut self, values: &[Lit]) -> SymbolicResult<Lit> {
        let mut result = Lit::FALSE;
        for &value in values {
            result = self.xor(result, value)?;
        }
        Ok(result)
    }

    fn check_budget(&self) -> SymbolicResult<()> {
        if self.builder.node_count() > MAX_SYMBOLIC_NODES {
            Err(SymbolicError::Unsupported)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    pub(super) fn into_logic(self) -> (opto_ir::logic::LogicNetwork, Vec<(word::SignalId, u32)>) {
        (self.builder.freeze(), self.external_order)
    }
}

#[cfg(test)]
mod tests;
