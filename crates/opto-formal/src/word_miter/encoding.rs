// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitVal, CnfEncoder, ExtendFormula, FormalError, Lit, word};

impl CnfEncoder<'_> {
    pub(super) fn value(&mut self, id: word::ValueId) -> Result<Vec<Lit>, FormalError> {
        if let Some(bits) = self.values.get(id.index()).and_then(Clone::clone) {
            return Ok(bits);
        }
        let value = self.module.value(id).ok_or_else(|| {
            FormalError::invalid(format!("equivalence proof references unknown value {id:?}"))
        })?;
        let bits = match &value.kind {
            word::ValueKind::Signal(reference) => (0..reference.width())
                .map(|offset| {
                    let bit = reference.lsb.checked_add(offset).ok_or_else(|| {
                        FormalError::capacity("equivalence proof signal index overflow")
                    })?;
                    Ok(self.signal(reference.signal, bit))
                })
                .collect::<Result<Vec<_>, FormalError>>()?,
            word::ValueKind::Constant(constant) => (0..value.ty.width())
                .map(|bit| {
                    let value = constant.bit_lsb(bit).ok_or_else(|| {
                        FormalError::invalid(format!("equivalence proof constant has no bit {bit}"))
                    })?;
                    match value {
                        BitVal::Zero => Ok(self.constant(false)),
                        BitVal::One => Ok(self.constant(true)),
                        BitVal::X | BitVal::Z => Err(FormalError::unsupported(
                            "equivalence proof does not accept unresolved X/Z constants",
                        )),
                    }
                })
                .collect::<Result<Vec<_>, FormalError>>()?,
            word::ValueKind::Operation(operation) => {
                let operation = self.module.operation(*operation).ok_or_else(|| {
                    FormalError::invalid(format!(
                        "equivalence proof references unknown operation {operation:?}"
                    ))
                })?;
                self.operation(&operation.kind, value.ty)?
            }
        };
        let slot = self.values.get_mut(id.index()).ok_or_else(|| {
            FormalError::invalid(format!(
                "equivalence proof has no cache slot for value {id:?}"
            ))
        })?;
        *slot = Some(bits.clone());
        self.encoded_values += 1;
        Ok(bits)
    }

    fn operation(
        &mut self,
        kind: &word::OpKind,
        ty: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
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
                    result.extend(self.value(part)?);
                }
                Ok(result)
            }
            word::OpKind::Extract { value, lsb, width } => {
                let bits = self.value(*value)?;
                let start = usize::try_from(*lsb).map_err(|_| {
                    FormalError::capacity("equivalence proof extract index overflow")
                })?;
                let width = usize::try_from(width.get()).map_err(|_| {
                    FormalError::capacity("equivalence proof extract width overflow")
                })?;
                let end = start.checked_add(width).ok_or_else(|| {
                    FormalError::capacity("equivalence proof extract range overflow")
                })?;
                bits.get(start..end).map(<[Lit]>::to_vec).ok_or_else(|| {
                    FormalError::invalid("equivalence proof extract exceeds source width")
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
            word::OpKind::Register(_) | word::OpKind::Latch(_) => Err(FormalError::unsupported(
                "equivalence proof cannot encode a sequential operation",
            )),
        }
    }

    fn unary(&mut self, op: word::UnaryOp, arg: word::ValueId) -> Result<Vec<Lit>, FormalError> {
        let bits = self.value(arg)?;
        match op {
            word::UnaryOp::BitNot => Ok(bits.into_iter().map(|bit| !bit).collect()),
            word::UnaryOp::LogicalNot => Ok(vec![!self.reduce_or(&bits)]),
            word::UnaryOp::ReductionAnd => Ok(vec![self.reduce_and(&bits)]),
            word::UnaryOp::ReductionOr => Ok(vec![self.reduce_or(&bits)]),
            word::UnaryOp::ReductionXor => Ok(vec![self.reduce_xor(&bits)]),
        }
    }

    fn binary(
        &mut self,
        op: word::BinaryOp,
        left: word::ValueId,
        right: word::ValueId,
        ty: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
        let left_ty = self.value_type(left)?;
        let right_ty = self.value_type(right)?;
        let left = self.value(left)?;
        let right = self.value(right)?;
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
                let left = self.resize(&left, left_ty, ty)?;
                let right = self.resize(&right, right_ty, ty)?;
                Ok(left
                    .into_iter()
                    .zip(right)
                    .map(|(left, right)| match op {
                        word::BinaryOp::BitAnd => self.and(left, right),
                        word::BinaryOp::BitOr => self.or(left, right),
                        word::BinaryOp::BitXor => self.xor(left, right),
                        _ => unreachable!(),
                    })
                    .collect())
            }
            word::BinaryOp::LogicalAnd | word::BinaryOp::LogicalOr => {
                let left = self.reduce_or(&left);
                let right = self.reduce_or(&right);
                Ok(vec![if op == word::BinaryOp::LogicalAnd {
                    self.and(left, right)
                } else {
                    self.or(left, right)
                }])
            }
            word::BinaryOp::Eq | word::BinaryOp::Ne => {
                let width = left_ty.width().max(right_ty.width());
                let compare_ty = word::WordType::new(
                    width,
                    left_ty.is_signed() && right_ty.is_signed(),
                    ty.state(),
                )
                .map_err(FormalError::Word)?;
                let left = self.resize(&left, left_ty, compare_ty)?;
                let right = self.resize(&right, right_ty, compare_ty)?;
                let differences = left
                    .into_iter()
                    .zip(right)
                    .map(|(left, right)| self.xor(left, right))
                    .collect::<Vec<_>>();
                let different = self.reduce_or(&differences);
                Ok(vec![if op == word::BinaryOp::Ne {
                    different
                } else {
                    !different
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

    fn shift(
        &mut self,
        op: word::BinaryOp,
        value: &[Lit],
        value_ty: word::WordType,
        amount: &[Lit],
        result_ty: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
        let mut current = self.resize(value, value_ty, result_ty)?;
        let zero = self.constant(false);
        let width = result_ty.width();
        let relevant_stages = if width <= 1 {
            0
        } else {
            u32::BITS - (width - 1).leading_zeros()
        };
        let stages =
            amount
                .len()
                .min(usize::try_from(relevant_stages).map_err(|_| {
                    FormalError::capacity("equivalence proof shift stage overflow")
                })?);
        for (stage, &control) in amount.iter().take(stages).enumerate() {
            let shift =
                1usize
                    .checked_shl(stage.try_into().map_err(|_| {
                        FormalError::capacity("equivalence proof shift stage overflow")
                    })?)
                    .ok_or_else(|| {
                        FormalError::capacity("equivalence proof shift distance overflow")
                    })?;
            let right_fill = if op == word::BinaryOp::Ashr && result_ty.is_signed() {
                current[current.len() - 1]
            } else {
                zero
            };
            let mut next = Vec::with_capacity(current.len());
            for index in 0..current.len() {
                let shifted = match op {
                    word::BinaryOp::Shl if index >= shift => current[index - shift],
                    word::BinaryOp::Shr | word::BinaryOp::Ashr if index + shift < current.len() => {
                        current[index + shift]
                    }
                    word::BinaryOp::Shl => zero,
                    word::BinaryOp::Shr | word::BinaryOp::Ashr => right_fill,
                    _ => {
                        return Err(FormalError::invalid(format!(
                            "equivalence proof received non-shift operation {op:?}"
                        )));
                    }
                };
                next.push(self.select(control, shifted, current[index]));
            }
            current = next;
        }
        if amount.len() > stages {
            let overflow = self.reduce_or(&amount[stages..]);
            let overflow_fill = if op == word::BinaryOp::Ashr && result_ty.is_signed() {
                current[current.len() - 1]
            } else {
                zero
            };
            for bit in &mut current {
                *bit = self.select(overflow, overflow_fill, *bit);
            }
        }
        Ok(current)
    }

    pub(super) fn add_bits(
        &mut self,
        left: Vec<Lit>,
        right: Vec<Lit>,
        mut carry: Lit,
    ) -> Result<Vec<Lit>, FormalError> {
        if left.len() != right.len() || left.is_empty() {
            return Err(FormalError::invalid(
                "equivalence proof adder inputs must have equal non-zero widths",
            ));
        }
        let mut result = Vec::with_capacity(left.len());
        let width = left.len();
        for (index, (left, right)) in left.into_iter().zip(right).enumerate() {
            let partial = self.xor(left, right);
            result.push(self.xor(partial, carry));
            if index + 1 != width {
                let generate = self.and(left, right);
                let propagated = self.and(partial, carry);
                carry = self.or(generate, propagated);
            }
        }
        Ok(result)
    }

    fn mux(
        &mut self,
        cond: word::ValueId,
        then_value: word::ValueId,
        else_value: word::ValueId,
    ) -> Result<Vec<Lit>, FormalError> {
        let cond = self.value(cond)?;
        let [cond]: [Lit; 1] = cond.try_into().map_err(|cond: Vec<_>| {
            FormalError::invalid(format!(
                "equivalence proof mux condition has {} bits",
                cond.len()
            ))
        })?;
        let then_value = self.value(then_value)?;
        let else_value = self.value(else_value)?;
        if then_value.len() != else_value.len() {
            return Err(FormalError::invalid(
                "equivalence proof mux branches have different widths",
            ));
        }
        Ok(then_value
            .into_iter()
            .zip(else_value)
            .map(|(then_value, else_value)| self.select(cond, then_value, else_value))
            .collect())
    }

    fn cast(
        &mut self,
        kind: word::CastKind,
        value: word::ValueId,
        target: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
        let mut bits = self.value(value)?;
        let target_width = usize::try_from(target.width())
            .map_err(|_| FormalError::capacity("equivalence proof target width overflow"))?;
        match kind {
            word::CastKind::Truncate => bits.truncate(target_width),
            word::CastKind::ZeroExtend | word::CastKind::SignExtend => {
                let extension = if kind == word::CastKind::SignExtend {
                    *bits.last().ok_or_else(|| {
                        FormalError::invalid("equivalence proof cannot extend an empty value")
                    })?
                } else {
                    self.constant(false)
                };
                bits.resize(target_width, extension);
            }
        }
        Ok(bits)
    }

    fn dynamic_extract(
        &mut self,
        value: word::ValueId,
        offset: word::ValueId,
        width: u32,
    ) -> Result<Vec<Lit>, FormalError> {
        let source_width = self
            .module
            .value(value)
            .map(|value| value.ty.width())
            .ok_or_else(|| {
                FormalError::invalid(format!(
                    "equivalence proof references unknown value {value:?}"
                ))
            })?;
        let source_width = usize::try_from(source_width).map_err(|_| {
            FormalError::capacity("equivalence proof dynamic extract source width overflow")
        })?;
        let result_width = usize::try_from(width).map_err(|_| {
            FormalError::capacity("equivalence proof dynamic extract width overflow")
        })?;
        let available_offsets = source_width.checked_sub(result_width).ok_or_else(|| {
            FormalError::invalid("equivalence proof dynamic extract exceeds source width")
        })?;
        let max_offset = word::unsigned_value_range(self.module, offset)
            .map(word::UnsignedValueRange::maximum)
            .ok_or_else(|| {
                FormalError::unsupported(
                    "equivalence proof cannot prove dynamic extract offset bounds",
                )
            })?;
        let value = self.value(value)?;
        let offset = self.value(offset)?;
        let selection_max = max_offset.min(available_offsets as u128);
        let in_range = if max_offset > available_offsets as u128 {
            Some(self.unsigned_at_most_constant(&offset, available_offsets))
        } else {
            None
        };
        let zero = self.constant(false);
        (0..result_width)
            .map(|result_bit| {
                let mut candidates = value[result_bit..].to_vec();
                for (stage, &control) in offset.iter().enumerate() {
                    let shift = 1usize
                        .checked_shl(stage.try_into().map_err(|_| {
                            FormalError::capacity(
                                "equivalence proof dynamic extract stage overflow",
                            )
                        })?)
                        .ok_or_else(|| {
                            FormalError::capacity(
                                "equivalence proof dynamic extract distance overflow",
                            )
                        })?;
                    if shift as u128 > selection_max {
                        continue;
                    }
                    for index in 0..candidates.len() - shift {
                        candidates[index] =
                            self.select(control, candidates[index + shift], candidates[index]);
                    }
                }
                let selected = candidates.first().copied().ok_or_else(|| {
                    FormalError::invalid(
                        "equivalence proof dynamic extract has no source candidate",
                    )
                })?;
                Ok(in_range.map_or(selected, |valid| self.select(valid, selected, zero)))
            })
            .collect()
    }

    fn unsigned_at_most_constant(&mut self, value: &[Lit], maximum: usize) -> Lit {
        let mut equal = self.constant(true);
        let mut greater = self.constant(false);
        for (index, &bit) in value.iter().enumerate().rev() {
            let maximum_bit = index < usize::BITS as usize && ((maximum >> index) & 1) != 0;
            if !maximum_bit {
                let greater_here = self.and(equal, bit);
                greater = self.or(greater, greater_here);
            }
            equal = self.and(equal, if maximum_bit { bit } else { !bit });
        }
        !greater
    }

    fn dynamic_insert(
        &mut self,
        value: word::ValueId,
        offset: word::ValueId,
        replacement: word::ValueId,
    ) -> Result<Vec<Lit>, FormalError> {
        let value = self.value(value)?;
        let offset = self.value(offset)?;
        let replacement = self.value(replacement)?;
        if replacement.len() > value.len() {
            return Err(FormalError::invalid(
                "equivalence proof dynamic insert replacement exceeds source width",
            ));
        }
        let zero = self.constant(false);
        let one = self.constant(true);
        let mut shifted = vec![zero; value.len()];
        shifted[..replacement.len()].copy_from_slice(&replacement);
        let mut mask = vec![zero; value.len()];
        mask[..replacement.len()].fill(one);
        for (stage, control) in offset.into_iter().enumerate() {
            let distance = 1usize
                .checked_shl(stage.try_into().map_err(|_| {
                    FormalError::capacity("equivalence proof dynamic insert stage overflow")
                })?)
                .ok_or_else(|| {
                    FormalError::capacity("equivalence proof dynamic insert distance overflow")
                })?;
            for index in (0..value.len()).rev() {
                let shifted_value = index
                    .checked_sub(distance)
                    .map_or(zero, |source| shifted[source]);
                let shifted_mask = index
                    .checked_sub(distance)
                    .map_or(zero, |source| mask[source]);
                shifted[index] = self.select(control, shifted_value, shifted[index]);
                mask[index] = self.select(control, shifted_mask, mask[index]);
            }
        }
        Ok(value
            .into_iter()
            .zip(shifted)
            .zip(mask)
            .map(|((original, replacement), select)| self.select(select, replacement, original))
            .collect())
    }

    fn value_type(&self, value: word::ValueId) -> Result<word::WordType, FormalError> {
        self.module
            .value(value)
            .map(|value| value.ty)
            .ok_or_else(|| {
                FormalError::invalid(format!(
                    "equivalence proof references unknown value {value:?}"
                ))
            })
    }

    pub(super) fn resize(
        &mut self,
        bits: &[Lit],
        source: word::WordType,
        target: word::WordType,
    ) -> Result<Vec<Lit>, FormalError> {
        let width = usize::try_from(target.width())
            .map_err(|_| FormalError::capacity("equivalence proof width overflow"))?;
        let mut result = bits[..bits.len().min(width)].to_vec();
        if result.len() < width {
            let extension = if target.is_signed() && source.is_signed() {
                *bits.last().ok_or_else(|| {
                    FormalError::invalid("equivalence proof cannot sign-extend an empty value")
                })?
            } else {
                self.constant(false)
            };
            result.resize(width, extension);
        }
        Ok(result)
    }

    pub(super) fn signal(&mut self, signal: word::SignalId, bit: u32) -> Lit {
        if let Some(&literal) = self.signals.get(&(signal, bit)) {
            return literal;
        }
        let literal = self.solver.new_var().positive();
        self.signals.insert((signal, bit), literal);
        literal
    }

    pub(super) fn constant(&mut self, value: bool) -> Lit {
        let literal = self.solver.new_var().positive();
        self.clause(&[if value { literal } else { !literal }]);
        literal
    }

    pub(super) fn and(&mut self, left: Lit, right: Lit) -> Lit {
        let output = self.solver.new_var().positive();
        self.clause(&[!left, !right, output]);
        self.clause(&[left, !output]);
        self.clause(&[right, !output]);
        output
    }

    pub(super) fn or(&mut self, left: Lit, right: Lit) -> Lit {
        let output = self.solver.new_var().positive();
        self.clause(&[left, right, !output]);
        self.clause(&[!left, output]);
        self.clause(&[!right, output]);
        output
    }

    pub(super) fn xor(&mut self, left: Lit, right: Lit) -> Lit {
        let output = self.solver.new_var().positive();
        self.clause(&[!left, !right, !output]);
        self.clause(&[left, right, !output]);
        self.clause(&[left, !right, output]);
        self.clause(&[!left, right, output]);
        output
    }

    pub(super) fn select(&mut self, cond: Lit, then_value: Lit, else_value: Lit) -> Lit {
        let output = self.solver.new_var().positive();
        self.clause(&[!cond, !then_value, output]);
        self.clause(&[!cond, then_value, !output]);
        self.clause(&[cond, !else_value, output]);
        self.clause(&[cond, else_value, !output]);
        output
    }

    fn reduce_and(&mut self, values: &[Lit]) -> Lit {
        values
            .iter()
            .copied()
            .reduce(|left, right| self.and(left, right))
            .unwrap_or_else(|| self.constant(true))
    }

    pub(super) fn reduce_or(&mut self, values: &[Lit]) -> Lit {
        values
            .iter()
            .copied()
            .reduce(|left, right| self.or(left, right))
            .unwrap_or_else(|| self.constant(false))
    }

    fn reduce_xor(&mut self, values: &[Lit]) -> Lit {
        values
            .iter()
            .copied()
            .reduce(|left, right| self.xor(left, right))
            .unwrap_or_else(|| self.constant(false))
    }

    pub(super) fn clause(&mut self, literals: &[Lit]) {
        self.solver.add_clause(literals);
        self.clauses += 1;
    }
}
