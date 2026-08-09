// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::BitBlaster;
use super::ImplementationRequest;
use super::compressor::{BitMatrix, CompressionSchedule};
use crate::OperatorKind;
use crate::planning::architecture::ArithmeticTerm;
use crate::planning::provider::{ImplementationProvider, ProviderRecipeId, StructuralEstimate};
use opto_ir::BitVal;
use opto_ir::word;

const RADIX4_WALLACE: ProviderRecipeId = ProviderRecipeId::from_raw(0);
const ARRAY_WALLACE: ProviderRecipeId = ProviderRecipeId::from_raw(1);
const CONSTANT_CSD_WALLACE: ProviderRecipeId = ProviderRecipeId::from_raw(2);

#[derive(Debug)]
struct MultiplyProvider;

#[derive(Clone, Copy)]
struct BoothDigit {
    bits: [word::ValueId; 3],
    shift: u32,
    magnitude_width: u32,
    zero: word::ValueId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::boolean::bitblast) enum ProductEncoding {
    Radix4,
    Array,
}

impl ImplementationProvider for MultiplyProvider {
    fn resource_name(&self) -> &'static str {
        "multiply"
    }

    fn enumerate_recipes(
        &self,
        operator: crate::SemanticOperator,
        emit: &mut dyn FnMut(ProviderRecipeId),
    ) {
        if operator.kind() == OperatorKind::Multiply {
            if operator.constant_input().is_some() {
                emit(CONSTANT_CSD_WALLACE);
                return;
            }
            emit(RADIX4_WALLACE);
            emit(ARRAY_WALLACE);
        }
    }

    fn recipe_name(&self, recipe: ProviderRecipeId) -> Option<&str> {
        match recipe {
            RADIX4_WALLACE => Some("radix4-wallace"),
            ARRAY_WALLACE => Some("array-wallace"),
            CONSTANT_CSD_WALLACE => Some("constant-csd-wallace"),
            _ => None,
        }
    }

    fn module_name(&self, operator: crate::SemanticOperator) -> Option<&str> {
        (operator.kind() == OperatorKind::Multiply).then_some("DW02_mult")
    }

    fn operation_mnemonic(&self, operator: crate::SemanticOperator) -> Option<&str> {
        (operator.kind() == OperatorKind::Multiply).then_some("mult")
    }

    fn implementation_name(&self, recipe: ProviderRecipeId) -> Option<&str> {
        match recipe {
            RADIX4_WALLACE => Some("booth-radix4"),
            ARRAY_WALLACE => Some("array-baugh-wooley"),
            CONSTANT_CSD_WALLACE => Some("constant-csd"),
            _ => None,
        }
    }

    fn structural_estimate(
        &self,
        recipe: ProviderRecipeId,
        operator: crate::SemanticOperator,
    ) -> Result<StructuralEstimate, crate::SynthError> {
        if recipe == ARRAY_WALLACE {
            return array_structural_estimate(operator);
        }
        if recipe == CONSTANT_CSD_WALLACE {
            let width = u64::from(operator.width());
            return Ok(StructuralEstimate {
                logic_depth: operator.width().ilog2().saturating_mul(3).saturating_add(4),
                logic_units: width
                    .checked_mul(width.div_ceil(3).saturating_add(5))
                    .ok_or_else(|| {
                        crate::SynthError::invariant("constant multiplier estimate overflow")
                    })?,
                wiring_units: width.checked_mul(width.div_ceil(3)).ok_or_else(|| {
                    crate::SynthError::invariant("constant multiplier wiring estimate overflow")
                })?,
            });
        }
        if recipe != RADIX4_WALLACE {
            return Err(crate::SynthError::invariant(format!(
                "resource '{}' has no recipe {}",
                self.resource_name(),
                recipe.raw()
            )));
        }
        let width = u64::from(operator.width());
        let multiplier_type = operator
            .input_types()
            .into_iter()
            .min_by_key(|ty| (booth_group_count(*ty, operator.width()), ty.width()))
            .ok_or_else(|| crate::SynthError::invariant("multiply has no input types"))?;
        let multiplier_width = multiplier_type.width().min(operator.width());
        let multiplicand_type = operator
            .input_types()
            .into_iter()
            .max_by_key(|ty| (booth_group_count(*ty, operator.width()), ty.width()))
            .ok_or_else(|| crate::SynthError::invariant("multiply has no input types"))?;
        let magnitude = u64::from(multiplicand_type.width().min(operator.width()));
        let groups = u64::from(booth_group_count(multiplier_type, operator.width()));
        let mut partial_bits = 0u64;
        for group in 0..groups {
            let shift = group * 2;
            if shift >= width {
                break;
            }
            partial_bits += (width - shift).min(magnitude + 2) + 1;
        }
        let compressor_units = partial_bits
            .saturating_sub(width.saturating_mul(2))
            .checked_mul(5)
            .ok_or_else(|| {
                crate::SynthError::invariant("radix-4 multiplier compressor estimate overflow")
            })?;
        let selection_units = partial_bits.checked_mul(3).ok_or_else(|| {
            crate::SynthError::invariant("radix-4 multiplier selection estimate overflow")
        })?;
        Ok(StructuralEstimate {
            logic_depth: operator
                .width()
                .checked_mul(2)
                .and_then(|depth| depth.checked_add(multiplier_width.ilog2() * 2 + 4))
                .ok_or_else(|| {
                    crate::SynthError::invariant("radix-4 multiplier depth estimate overflow")
                })?,
            logic_units: selection_units
                .checked_add(compressor_units)
                .and_then(|units| units.checked_add(width.saturating_mul(5)))
                .ok_or_else(|| {
                    crate::SynthError::invariant("radix-4 multiplier logic estimate overflow")
                })?,
            wiring_units: partial_bits,
        })
    }
}

impl MultiplyProvider {
    fn lower(
        &self,
        recipe: ProviderRecipeId,
        blaster: &mut BitBlaster<'_>,
        request: ImplementationRequest<'_>,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        if recipe == ARRAY_WALLACE {
            let [left, right] = request.operator.inputs();
            return blaster.array_wallace_multiply_bits(
                left,
                right,
                request.operator.input_types(),
                request.result_type,
                request.source,
            );
        }
        if recipe == CONSTANT_CSD_WALLACE {
            let [left, right] = request.operator.inputs();
            return blaster.constant_wallace_multiply_bits(
                left,
                right,
                request.operator.input_types(),
                request.result_type,
                request.source,
            );
        }
        if recipe != RADIX4_WALLACE {
            return Err(crate::SynthError::invariant(format!(
                "resource '{}' has no recipe {}",
                self.resource_name(),
                recipe.raw()
            )));
        }
        let [left, right] = request.operator.inputs();
        blaster.radix4_wallace_multiply_bits(
            left,
            right,
            request.operator.input_types(),
            request.result_type,
            request.source,
        )
    }
}

pub(super) fn implementation_provider() -> &'static dyn ImplementationProvider {
    &MultiplyProvider
}

pub(super) fn lower_implementation(
    recipe: ProviderRecipeId,
    blaster: &mut BitBlaster<'_>,
    request: ImplementationRequest<'_>,
) -> Result<Vec<word::ValueId>, crate::SynthError> {
    MultiplyProvider.lower(recipe, blaster, request)
}

fn array_partial_bits(operator: crate::SemanticOperator) -> u64 {
    let width = operator.width();
    let [left_ty, right_ty] = operator.input_types();
    let left_width = left_ty.width().min(width);
    let right_width = right_ty.width().min(width);
    let mut bits = 0u64;
    for row in 0..right_width {
        bits += u64::from(left_width.min(width - row));
    }
    bits
}

fn array_structural_estimate(
    operator: crate::SemanticOperator,
) -> Result<StructuralEstimate, crate::SynthError> {
    let width = u64::from(operator.width());
    let partial_bits = array_partial_bits(operator);
    let compressor_units = partial_bits.saturating_sub(width.saturating_mul(2)) * 5;
    Ok(StructuralEstimate {
        logic_depth: operator
            .width()
            .checked_mul(2)
            .and_then(|depth| {
                let [left_ty, right_ty] = operator.input_types();
                let rows = right_ty.width().min(left_ty.width()).min(operator.width());
                depth.checked_add(rows.max(1).ilog2() * 3 + 4)
            })
            .ok_or_else(|| {
                crate::SynthError::invariant("array multiplier depth estimate overflow")
            })?,
        logic_units: partial_bits
            .checked_add(compressor_units)
            .and_then(|units| units.checked_add(width.saturating_mul(5)))
            .ok_or_else(|| {
                crate::SynthError::invariant("array multiplier logic estimate overflow")
            })?,
        wiring_units: partial_bits,
    })
}

impl BitBlaster<'_> {
    pub(in crate::boolean::bitblast) fn constant_multiply_vector(
        &mut self,
        multiplicand: &[word::ValueId],
        constant: &[bool],
        output_width: usize,
        addend: Option<&[word::ValueId]>,
        state: word::LogicStateKind,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        if multiplicand.is_empty() || output_width == 0 {
            return Err(crate::SynthError::invariant(
                "constant multiplier requires nonempty input and output",
            ));
        }
        let zero = self.constant(BitVal::Zero, state, source)?;
        let one = self.constant(BitVal::One, state, source)?;
        let mut matrix = BitMatrix::new(output_width);
        for (shift, negative) in nonadjacent_digits(constant) {
            if shift >= output_width {
                continue;
            }
            let mut row = vec![None; output_width];
            for (output, bit) in row.iter_mut().enumerate().skip(shift) {
                *bit = multiplicand.get(output - shift).copied();
            }
            self.append_signed_row(&mut matrix, row, negative, source)?;
        }
        if let Some(addend) = addend {
            let mut row = vec![None; output_width];
            for (slot, &bit) in row.iter_mut().zip(addend) {
                *slot = Some(bit);
            }
            matrix.push_row(row);
        }
        self.wallace_reduce(matrix, zero, one, source)
    }

    fn constant_product_matrix(
        &mut self,
        inputs: [word::ValueId; 2],
        input_types: [word::WordType; 2],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<BitMatrix, crate::SynthError> {
        let mut resized = Vec::with_capacity(2);
        for (value, ty) in inputs.into_iter().zip(input_types) {
            let span = self.value(value)?;
            resized.push(
                (0..result_ty.width())
                    .map(|index| self.resized_bit(span, ty, index, ty.is_signed(), source))
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        let constant_index = resized
            .iter()
            .position(|bits| bits.iter().all(|&bit| self.scalar_constant(bit).is_some()))
            .ok_or_else(|| {
                crate::SynthError::invariant(
                    "constant multiplier recipe has no defined constant operand",
                )
            })?;
        let constant = resized[constant_index]
            .iter()
            .map(|&bit| {
                self.scalar_constant(bit)
                    .expect("constant operand was checked")
            })
            .collect::<Vec<_>>();
        let multiplicand = &resized[1 - constant_index];
        self.constant_coefficient_matrix(
            multiplicand,
            &constant,
            result_ty.width() as usize,
            source,
        )
    }

    fn constant_coefficient_matrix(
        &mut self,
        multiplicand: &[word::ValueId],
        constant: &[bool],
        width: usize,
        source: &word::SourceSpan,
    ) -> Result<BitMatrix, crate::SynthError> {
        let mut matrix = BitMatrix::new(width);
        for (shift, negative) in nonadjacent_digits(constant) {
            if shift >= width {
                continue;
            }
            let mut row = vec![None; width];
            for (output, slot) in row.iter_mut().enumerate().skip(shift) {
                *slot = multiplicand.get(output - shift).copied();
            }
            self.append_signed_row(&mut matrix, row, negative, source)?;
        }
        Ok(matrix)
    }

    pub(in crate::boolean::bitblast) fn append_constant_coefficient_rows(
        &mut self,
        matrix: &mut BitMatrix,
        multiplicand: word::ValueId,
        multiplicand_ty: word::WordType,
        coefficient: &[bool],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let span = self.value(multiplicand)?;
        let bits = (0..result_ty.width())
            .map(|index| {
                self.resized_bit(
                    span,
                    multiplicand_ty,
                    index,
                    multiplicand_ty.is_signed(),
                    source,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let product = self.constant_coefficient_matrix(
            &bits,
            coefficient,
            result_ty.width() as usize,
            source,
        )?;
        self.append_matrix(matrix, product, false, source)
    }

    fn constant_wallace_multiply_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        input_types: [word::WordType; 2],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let matrix = self.constant_product_matrix([left, right], input_types, result_ty, source)?;
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;
        let one = self.constant(BitVal::One, result_ty.state(), source)?;
        self.wallace_reduce(matrix, zero, one, source)
    }

    fn radix4_wallace_multiply_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        input_types: [word::WordType; 2],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let matrix = self.radix4_product_matrix([left, right], input_types, result_ty, source)?;
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;
        let one = self.constant(BitVal::One, result_ty.state(), source)?;
        self.wallace_reduce(matrix, zero, one, source)
    }

    fn radix4_product_matrix(
        &mut self,
        inputs: [word::ValueId; 2],
        input_types: [word::WordType; 2],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<BitMatrix, crate::SynthError> {
        let [left, right] = inputs;
        let [left_ty, right_ty] = input_types;
        let (multiplicand, multiplicand_ty, multiplier, multiplier_ty) =
            if booth_group_count(left_ty, result_ty.width())
                >= booth_group_count(right_ty, result_ty.width())
            {
                (left, left_ty, right, right_ty)
            } else {
                (right, right_ty, left, left_ty)
            };
        let multiplicand_span = self.value(multiplicand)?;
        let multiplier_span = self.value(multiplier)?;
        let width = result_ty.width();
        let multiplier_width = multiplier_ty.width().min(width);
        let multiplier_signed = multiplier_ty.is_signed();
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;

        let multiplicand_bits = (0..width)
            .map(|index| {
                let bit = self.resized_bit(
                    multiplicand_span,
                    multiplicand_ty,
                    index,
                    multiplicand_ty.is_signed(),
                    source,
                )?;
                self.unsigned_bit(bit, source)
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let multiplier_bits = (0..multiplier_width)
            .map(|index| self.unsigned_bit(self.bit(multiplier_span, index), source))
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let group_count = booth_group_count(multiplier_ty, width);
        let mut matrix = BitMatrix::new(width as usize);
        let magnitude_width = multiplicand_ty.width().min(width);
        for group in 0..group_count {
            let shift = group
                .checked_mul(2)
                .ok_or_else(|| crate::SynthError::invariant("Booth group shift overflow"))?;
            if shift >= width {
                break;
            }
            let low = booth_multiplier_bit(
                &multiplier_bits,
                i64::from(shift) - 1,
                multiplier_signed,
                zero,
            );
            let middle =
                booth_multiplier_bit(&multiplier_bits, i64::from(shift), multiplier_signed, zero);
            let upper = booth_multiplier_bit(
                &multiplier_bits,
                i64::from(shift) + 1,
                multiplier_signed,
                zero,
            );
            self.append_booth_row(
                &mut matrix,
                &multiplicand_bits,
                BoothDigit {
                    bits: [upper, middle, low],
                    shift,
                    magnitude_width,
                    zero,
                },
                source,
            )?;
        }
        Ok(matrix)
    }

    fn array_wallace_multiply_bits(
        &mut self,
        left: word::ValueId,
        right: word::ValueId,
        input_types: [word::WordType; 2],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let matrix = self.array_product_matrix([left, right], input_types, result_ty, source)?;
        let zero = self.constant(BitVal::Zero, result_ty.state(), source)?;
        let one = self.constant(BitVal::One, result_ty.state(), source)?;
        self.wallace_reduce(matrix, zero, one, source)
    }

    fn array_product_matrix(
        &mut self,
        inputs: [word::ValueId; 2],
        input_types: [word::WordType; 2],
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<BitMatrix, crate::SynthError> {
        let [left, right] = inputs;
        let [left_ty, right_ty] = input_types;
        let width = result_ty.width();
        let left_span = self.value(left)?;
        let right_span = self.value(right)?;
        let left_width = left_ty.width().min(width);
        let right_width = right_ty.width().min(width);
        let left_bits = (0..left_width)
            .map(|index| self.unsigned_bit(self.bit(left_span, index), source))
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let right_bits = (0..right_width)
            .map(|index| self.unsigned_bit(self.bit(right_span, index), source))
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
        let left_sign = left_ty.is_signed() && left_ty.width() <= width;
        let right_sign = right_ty.is_signed() && right_ty.width() <= width;
        let mut matrix = BitMatrix::new(width as usize);
        for (j, &right_bit) in right_bits.iter().enumerate() {
            let mut row = vec![None; width as usize];
            for (i, &left_bit) in left_bits.iter().enumerate() {
                let column = i + j;
                if column >= width as usize {
                    continue;
                }
                let negative = (left_sign && i == left_bits.len() - 1)
                    != (right_sign && j == right_bits.len() - 1);
                let term = self.emit_binary(word::BinaryOp::BitAnd, left_bit, right_bit, source)?;
                if negative {
                    let inverted = self.emit_unary(word::UnaryOp::BitNot, term, source)?;
                    row[column] = Some(inverted);
                    matrix.add_correction_power(column, true);
                } else {
                    row[column] = Some(term);
                }
            }
            matrix.push_row(row);
        }
        Ok(matrix)
    }

    pub(in crate::boolean::bitblast) fn append_product_rows(
        &mut self,
        matrix: &mut BitMatrix,
        term: ArithmeticTerm,
        encoding: ProductEncoding,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let ArithmeticTerm::Product {
            inputs,
            input_types,
            negative,
            constant_input,
            ..
        } = term
        else {
            return Err(crate::SynthError::invariant(
                "product-row lowering received a scalar arithmetic term",
            ));
        };
        let product = if constant_input.is_some() {
            self.constant_product_matrix(inputs, input_types, result_ty, source)?
        } else {
            match encoding {
                ProductEncoding::Radix4 => {
                    self.radix4_product_matrix(inputs, input_types, result_ty, source)?
                }
                ProductEncoding::Array => {
                    self.array_product_matrix(inputs, input_types, result_ty, source)?
                }
            }
        };
        self.append_matrix(matrix, product, negative, source)
    }

    fn append_booth_row(
        &mut self,
        matrix: &mut BitMatrix,
        multiplicand: &[word::ValueId],
        digit: BoothDigit,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let BoothDigit {
            bits: [upper, middle, low],
            shift,
            magnitude_width,
            zero,
        } = digit;
        let one = self.emit_binary(word::BinaryOp::BitXor, middle, low, source)?;
        let not_one = self.emit_unary(word::UnaryOp::BitNot, one, source)?;
        let upper_middle = self.emit_binary(word::BinaryOp::BitXor, upper, middle, source)?;
        let two = self.emit_binary(word::BinaryOp::BitAnd, not_one, upper_middle, source)?;
        let width = u32::try_from(matrix.width()).map_err(|_| {
            crate::SynthError::capacity("multiplier output width exceeds 32-bit capacity")
        })?;
        let sign_column = shift.saturating_add(magnitude_width).saturating_add(1);
        let mut row = vec![None; matrix.width()];
        for output in shift..width.min(sign_column.saturating_add(1)) {
            let index = (output - shift) as usize;
            let doubled = index
                .checked_sub(1)
                .map_or(zero, |index| multiplicand[index]);
            let doubled = self.emit_binary(word::BinaryOp::BitAnd, two, doubled, source)?;
            let magnitude = self.emit_mux(one, multiplicand[index], doubled, source)?;
            let partial = self.emit_binary(word::BinaryOp::BitXor, magnitude, upper, source)?;
            if output == sign_column {
                let inverted = self.emit_unary(word::UnaryOp::BitNot, partial, source)?;
                row[output as usize] = Some(inverted);
                matrix.add_correction_power(output as usize, true);
            } else {
                row[output as usize] = Some(partial);
            }
        }
        matrix.push_row(row);
        if upper != zero {
            let mut sign = vec![None; matrix.width()];
            sign[shift as usize] = Some(upper);
            matrix.push_row(sign);
        }
        Ok(())
    }

    fn wallace_reduce(
        &mut self,
        matrix: BitMatrix,
        zero: word::ValueId,
        one: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<Vec<word::ValueId>, crate::SynthError> {
        let (left, right) =
            self.reduce_matrix(matrix, CompressionSchedule::Wallace, zero, one, source)?;
        self.add_vectors(&left, &right, zero, source)
    }
}

fn nonadjacent_digits(bits: &[bool]) -> Vec<(usize, bool)> {
    let mut digits = Vec::new();
    let mut carry = false;
    for index in 0..=bits.len() {
        let bit = bits.get(index).copied().unwrap_or(false);
        match (bit, carry) {
            (false, false) => {}
            (true, true) => carry = true,
            (false, true) => {
                digits.push((index, false));
                carry = false;
            }
            (true, false) => {
                if bits.get(index + 1).copied().unwrap_or(false) {
                    digits.push((index, true));
                    carry = true;
                } else {
                    digits.push((index, false));
                }
            }
        }
    }
    digits
}

fn booth_multiplier_bit(
    bits: &[word::ValueId],
    index: i64,
    signed: bool,
    zero: word::ValueId,
) -> word::ValueId {
    if index < 0 {
        return zero;
    }
    usize::try_from(index)
        .ok()
        .and_then(|index| bits.get(index))
        .copied()
        .unwrap_or_else(|| {
            if signed {
                bits.last().copied().unwrap_or(zero)
            } else {
                zero
            }
        })
}

fn booth_group_count(ty: word::WordType, result_width: u32) -> u32 {
    let width = ty.width().min(result_width);
    if ty.is_signed() {
        width.div_ceil(2)
    } else {
        width.saturating_add(1).div_ceil(2)
    }
}
