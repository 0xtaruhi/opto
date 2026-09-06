// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBackend, BitBlaster, BitColumn, BitColumns, ScalarBit};
use opto_ir::word;

pub(in crate::boolean::bitblast) type BitRow = Vec<Option<ScalarBit>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::boolean::bitblast) enum CompressionSchedule {
    Serial,
    Balanced,
    Wallace,
    Dadda,
}

pub(in crate::boolean::bitblast) struct BitMatrix {
    width: usize,
    rows: Vec<BitRow>,
    correction: Vec<bool>,
}

impl BitMatrix {
    pub(in crate::boolean::bitblast) fn new(width: usize) -> Self {
        Self {
            width,
            rows: Vec::new(),
            correction: vec![false; width],
        }
    }

    pub(in crate::boolean::bitblast) fn width(&self) -> usize {
        self.width
    }

    /// Absorb an early one-bit row when that removes the whole compression
    /// layer. Every remaining nonconstant operand bit must arrive no earlier
    /// than carry-in; otherwise preserve the ordinary compression schedule.
    /// Unknown Word-shell levels cannot establish this structural precondition.
    pub(in crate::boolean::bitblast) fn take_carry_input(
        &mut self,
        is_zero: impl Fn(ScalarBit) -> bool,
        level: impl Fn(ScalarBit) -> Option<u32>,
    ) -> Option<ScalarBit> {
        if self.rows.len() != 3 || self.correction.iter().any(|bit| *bit) {
            return None;
        }
        let index = self.rows.iter().enumerate().position(|(index, row)| {
            let Some(carry) = row.first().copied().flatten().filter(|&bit| !is_zero(bit)) else {
                return false;
            };
            let Some(carry_level) = level(carry) else {
                return false;
            };
            row.iter().skip(1).all(|bit| bit.is_none_or(&is_zero))
                && self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|&(other, _)| other != index)
                    .flat_map(|(_, row)| row.iter().flatten().copied())
                    .all(|bit| is_zero(bit) || level(bit).is_some_and(|level| level >= carry_level))
        })?;
        self.rows.remove(index)[0]
    }

    pub(in crate::boolean::bitblast) fn push_row(&mut self, row: BitRow) {
        debug_assert_eq!(row.len(), self.width);
        if row.iter().any(Option::is_some) {
            self.rows.push(row);
        }
    }

    pub(in crate::boolean::bitblast) fn add_correction_power(
        &mut self,
        column: usize,
        negative: bool,
    ) {
        if column >= self.width {
            return;
        }
        let mut carry = true;
        for bit in &mut self.correction[column..] {
            if !carry {
                break;
            }
            let previous = *bit;
            *bit ^= true;
            carry = if negative { !previous } else { previous };
        }
    }

    fn take_rows_with_correction(&mut self, one: ScalarBit) -> Vec<BitRow> {
        if self.correction.iter().any(|bit| *bit) {
            self.rows.push(
                self.correction
                    .iter()
                    .map(|&bit| bit.then_some(one))
                    .collect(),
            );
        }
        std::mem::take(&mut self.rows)
    }
}

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(in crate::boolean::bitblast) fn append_matrix(
        &mut self,
        target: &mut BitMatrix,
        mut source_matrix: BitMatrix,
        negative: bool,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        if target.width != source_matrix.width {
            return Err(crate::SynthError::invariant(
                "cannot merge arithmetic matrices with different widths",
            ));
        }
        if !negative {
            target.rows.append(&mut source_matrix.rows);
            for (column, bit) in source_matrix.correction.into_iter().enumerate() {
                if bit {
                    target.add_correction_power(column, false);
                }
            }
            return Ok(());
        }
        for row in source_matrix.rows {
            let mut negated = Vec::with_capacity(target.width);
            for (column, bit) in row.into_iter().enumerate() {
                if let Some(bit) = bit {
                    negated.push(Some(self.emit_unary(word::UnaryOp::BitNot, bit, source)?));
                    target.add_correction_power(column, true);
                } else {
                    negated.push(None);
                }
            }
            target.push_row(negated);
        }
        for (column, bit) in source_matrix.correction.into_iter().enumerate() {
            if bit {
                target.add_correction_power(column, true);
            }
        }
        Ok(())
    }

    pub(in crate::boolean::bitblast) fn append_signed_row(
        &mut self,
        matrix: &mut BitMatrix,
        row: BitRow,
        negative: bool,
        source: &word::SourceSpan,
    ) -> Result<(), crate::SynthError> {
        let mut single = BitMatrix::new(matrix.width());
        single.push_row(row);
        self.append_matrix(matrix, single, negative, source)
    }

    pub(in crate::boolean::bitblast) fn reduce_matrix(
        &mut self,
        mut matrix: BitMatrix,
        schedule: CompressionSchedule,
        zero: ScalarBit,
        one: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(Vec<ScalarBit>, Vec<ScalarBit>), crate::SynthError> {
        let rows = matrix.take_rows_with_correction(one);
        let rows = match schedule {
            CompressionSchedule::Serial => self.reduce_rows_serial(rows, matrix.width, source)?,
            CompressionSchedule::Balanced => {
                self.reduce_rows_balanced(rows, matrix.width, source)?
            }
            CompressionSchedule::Wallace => {
                self.reduce_columns_wallace(rows_to_columns(rows, matrix.width), source)?
            }
            CompressionSchedule::Dadda => {
                self.reduce_columns_dadda(rows_to_columns(rows, matrix.width), source)?
            }
        };
        rows_to_vectors(rows, matrix.width, zero)
    }

    fn reduce_rows_serial(
        &mut self,
        mut rows: Vec<BitRow>,
        width: usize,
        source: &word::SourceSpan,
    ) -> Result<Vec<BitRow>, crate::SynthError> {
        if rows.len() <= 2 {
            return Ok(rows);
        }
        let remainder = rows.split_off(2);
        for row in remainder {
            let [left, right]: [BitRow; 2] = rows.try_into().map_err(|rows: Vec<_>| {
                crate::SynthError::invariant(format!(
                    "serial compressor retained {} accumulator rows",
                    rows.len()
                ))
            })?;
            rows = self.compress_row_triplet(&left, &right, &row, width, source)?;
        }
        Ok(rows)
    }

    fn reduce_rows_balanced(
        &mut self,
        mut rows: Vec<BitRow>,
        width: usize,
        source: &word::SourceSpan,
    ) -> Result<Vec<BitRow>, crate::SynthError> {
        while rows.len() > 2 {
            let mut next = Vec::with_capacity(rows.len().div_ceil(3) * 2);
            let mut chunks = rows.into_iter();
            while let Some(left) = chunks.next() {
                let Some(right) = chunks.next() else {
                    next.push(left);
                    break;
                };
                let Some(third) = chunks.next() else {
                    next.push(left);
                    next.push(right);
                    break;
                };
                next.extend(self.compress_row_triplet(&left, &right, &third, width, source)?);
            }
            rows = next;
        }
        Ok(rows)
    }

    fn compress_row_triplet(
        &mut self,
        left: &[Option<ScalarBit>],
        right: &[Option<ScalarBit>],
        third: &[Option<ScalarBit>],
        width: usize,
        source: &word::SourceSpan,
    ) -> Result<Vec<BitRow>, crate::SynthError> {
        let mut sum = vec![None; width];
        let mut carry = vec![None; width];
        for column in 0..width {
            let bits = [left[column], right[column], third[column]];
            let mut bits = bits.into_iter().flatten();
            let Some(first) = bits.next() else {
                continue;
            };
            let Some(second) = bits.next() else {
                sum[column] = Some(first);
                continue;
            };
            let (result, carry_out) = if let Some(third) = bits.next() {
                self.full_adder(first, second, third, source)?
            } else {
                self.half_adder(first, second, source)?
            };
            sum[column] = Some(result);
            if column + 1 < width {
                carry[column + 1] = Some(carry_out);
            }
        }
        Ok(vec![sum, carry])
    }

    fn reduce_columns_wallace(
        &mut self,
        mut columns: BitColumns,
        source: &word::SourceSpan,
    ) -> Result<Vec<BitRow>, crate::SynthError> {
        while columns.iter().any(|column| column.len() > 2) {
            let mut next = vec![BitColumn::new(); columns.len()];
            for (index, column) in columns.into_iter().enumerate() {
                let (chunks, remainder) = column.as_chunks::<3>();
                for chunk in chunks {
                    let (sum, carry) = self.full_adder(chunk[0], chunk[1], chunk[2], source)?;
                    next[index].push(sum);
                    if index + 1 < next.len() {
                        next[index + 1].push(carry);
                    }
                }
                next[index].extend_from_slice(remainder);
            }
            columns = next;
        }
        columns_to_rows(columns)
    }

    fn reduce_columns_dadda(
        &mut self,
        mut columns: BitColumns,
        source: &word::SourceSpan,
    ) -> Result<Vec<BitRow>, crate::SynthError> {
        let maximum = columns.iter().map(BitColumn::len).max().unwrap_or(0);
        let mut targets = vec![2usize];
        while *targets.last().expect("Dadda target list is nonempty") < maximum {
            let previous = *targets.last().expect("Dadda target list is nonempty");
            targets.push(previous.saturating_mul(3) / 2);
        }
        for &target in targets.iter().rev().skip(1) {
            loop {
                let mut next = vec![BitColumn::new(); columns.len()];
                let mut made_progress = false;
                for (index, column) in columns.into_iter().enumerate() {
                    let incoming = next[index].len();
                    let excess = column.len().saturating_add(incoming).saturating_sub(target);
                    let full_adders = (excess / 2).min(column.len() / 3);
                    let remaining_bits = column.len() - full_adders * 3;
                    let half_adders = excess
                        .saturating_sub(full_adders * 2)
                        .min(remaining_bits / 2);
                    made_progress |= full_adders + half_adders != 0;
                    let mut bits = column.into_iter();
                    for _ in 0..full_adders {
                        let inputs = [
                            bits.next().expect("Dadda full adder has three inputs"),
                            bits.next().expect("Dadda full adder has three inputs"),
                            bits.next().expect("Dadda full adder has three inputs"),
                        ];
                        let (sum, carry) =
                            self.full_adder(inputs[0], inputs[1], inputs[2], source)?;
                        next[index].push(sum);
                        if index + 1 < next.len() {
                            next[index + 1].push(carry);
                        }
                    }
                    for _ in 0..half_adders {
                        let left = bits.next().expect("Dadda half adder has two inputs");
                        let right = bits.next().expect("Dadda half adder has two inputs");
                        let (sum, carry) = self.half_adder(left, right, source)?;
                        next[index].push(sum);
                        if index + 1 < next.len() {
                            next[index + 1].push(carry);
                        }
                    }
                    next[index].extend(bits);
                }
                let complete = next.iter().all(|column| column.len() <= target);
                columns = next;
                if complete {
                    break;
                }
                if !made_progress {
                    return Err(crate::SynthError::invariant(
                        "Dadda reduction made no progress",
                    ));
                }
            }
        }
        if columns.iter().any(|column| column.len() > 2) {
            return Err(crate::SynthError::invariant(
                "Dadda reduction left more than two rows",
            ));
        }
        columns_to_rows(columns)
    }

    pub(in crate::boolean::bitblast) fn full_adder(
        &mut self,
        left: ScalarBit,
        right: ScalarBit,
        carry_in: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, ScalarBit), crate::SynthError> {
        let propagate = self.emit_binary(word::BinaryOp::BitXor, left, right, source)?;
        let sum = self.emit_binary(word::BinaryOp::BitXor, propagate, carry_in, source)?;
        let generate = self.emit_binary(word::BinaryOp::BitAnd, left, right, source)?;
        let propagated = self.emit_binary(word::BinaryOp::BitAnd, propagate, carry_in, source)?;
        let carry = self.emit_binary(word::BinaryOp::BitOr, generate, propagated, source)?;
        Ok((sum, carry))
    }

    fn half_adder(
        &mut self,
        left: ScalarBit,
        right: ScalarBit,
        source: &word::SourceSpan,
    ) -> Result<(ScalarBit, ScalarBit), crate::SynthError> {
        Ok((
            self.emit_binary(word::BinaryOp::BitXor, left, right, source)?,
            self.emit_binary(word::BinaryOp::BitAnd, left, right, source)?,
        ))
    }
}

fn rows_to_columns(rows: Vec<BitRow>, width: usize) -> BitColumns {
    let mut columns = vec![BitColumn::new(); width];
    for row in rows {
        for (column, bit) in columns.iter_mut().zip(row) {
            if let Some(bit) = bit {
                column.push(bit);
            }
        }
    }
    columns
}

fn columns_to_rows(columns: BitColumns) -> Result<Vec<BitRow>, crate::SynthError> {
    let width = columns.len();
    let mut rows = vec![vec![None; width], vec![None; width]];
    for (index, column) in columns.into_iter().enumerate() {
        if column.len() > 2 {
            return Err(crate::SynthError::invariant(
                "column reduction left more than two rows",
            ));
        }
        for (row, bit) in column.into_iter().enumerate() {
            rows[row][index] = Some(bit);
        }
    }
    Ok(rows)
}

fn rows_to_vectors(
    mut rows: Vec<BitRow>,
    width: usize,
    zero: ScalarBit,
) -> Result<(Vec<ScalarBit>, Vec<ScalarBit>), crate::SynthError> {
    if rows.len() > 2 {
        return Err(crate::SynthError::invariant(
            "arithmetic compression left more than two rows",
        ));
    }
    rows.resize_with(2, || vec![None; width]);
    let [left, right]: [BitRow; 2] = rows.try_into().map_err(|_| {
        crate::SynthError::invariant("arithmetic compression did not produce two rows")
    })?;
    Ok((
        left.into_iter().map(|bit| bit.unwrap_or(zero)).collect(),
        right.into_iter().map(|bit| bit.unwrap_or(zero)).collect(),
    ))
}
