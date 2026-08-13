// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BitBackend, BitBlaster, BitSpan, BitVal, ImplementationRequest, ScalarBit, word};
use crate::OperatorKind;
use crate::planning::provider::{ImplementationProvider, ProviderRecipeId, StructuralEstimate};

const ONE_HOT: ProviderRecipeId = ProviderRecipeId::from_raw(0);
const BARREL: ProviderRecipeId = ProviderRecipeId::from_raw(1);

#[derive(Debug)]
struct DynamicExtractProvider;

impl ImplementationProvider for DynamicExtractProvider {
    fn resource_name(&self) -> &'static str {
        "dynamic-extract"
    }

    fn enumerate_recipes(
        &self,
        operator: crate::SemanticOperator,
        emit: &mut dyn FnMut(ProviderRecipeId),
    ) {
        if operator.kind() != OperatorKind::DynamicExtract {
            return;
        }
        if operator
            .dynamic_extract()
            .is_some_and(|extract| extract.supports_one_hot(operator.width()))
        {
            // Recipe order is the default selection order.
            emit(ONE_HOT);
        }
        emit(BARREL);
    }

    fn recipe_name(&self, recipe: ProviderRecipeId) -> Option<&str> {
        match recipe {
            ONE_HOT => Some("shared-one-hot"),
            BARREL => Some("mux-barrel"),
            _ => None,
        }
    }

    fn module_name(&self, operator: crate::SemanticOperator) -> Option<&str> {
        (operator.kind() == OperatorKind::DynamicExtract).then_some("DW_part_select")
    }

    fn operation_mnemonic(&self, operator: crate::SemanticOperator) -> Option<&str> {
        (operator.kind() == OperatorKind::DynamicExtract).then_some("part_select")
    }

    fn implementation_name(&self, recipe: ProviderRecipeId) -> Option<&str> {
        match recipe {
            ONE_HOT => Some("shared-one-hot"),
            BARREL => Some("mux-barrel"),
            _ => None,
        }
    }

    fn structural_estimate(
        &self,
        recipe: ProviderRecipeId,
        operator: crate::SemanticOperator,
    ) -> Result<StructuralEstimate, crate::SynthError> {
        let extract = operator.dynamic_extract().ok_or_else(|| {
            crate::SynthError::invariant("dynamic-extract recipe has no extract metadata")
        })?;
        let width = u64::from(operator.width());
        let estimate = match recipe {
            BARREL => {
                let stages = extract.barrel_stages();
                StructuralEstimate {
                    logic_depth: stages,
                    // A MUX requires a select inversion and two product terms
                    // in primitive-gate libraries. Target mapping replaces
                    // this estimate whenever both candidates survive pruning.
                    logic_units: width
                        .checked_mul(u64::from(stages))
                        .and_then(|units| units.checked_mul(4))
                        .ok_or_else(|| {
                            crate::SynthError::invariant("dynamic-extract barrel estimate overflow")
                        })?,
                    wiring_units: width
                        .checked_mul(u64::from(stages))
                        .and_then(|units| units.checked_mul(3))
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "dynamic-extract barrel wiring estimate overflow",
                            )
                        })?,
                }
            }
            ONE_HOT => {
                let taps = u64::from(extract.tap_count());
                let select_bits = extract.tap_count().next_power_of_two().ilog2();
                let decode_depth = select_bits.max(1).next_power_of_two().ilog2();
                let reduction_depth = extract.tap_count().next_power_of_two().ilog2();
                let decode_units = taps
                    .checked_mul(u64::from(select_bits.saturating_sub(1)))
                    .and_then(|units| units.checked_add(u64::from(select_bits)))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "dynamic-extract one-hot decode estimate overflow",
                        )
                    })?;
                let data_units = width
                    .checked_mul(taps.saturating_mul(2).saturating_sub(1))
                    .ok_or_else(|| {
                        crate::SynthError::invariant(
                            "dynamic-extract one-hot data estimate overflow",
                        )
                    })?;
                StructuralEstimate {
                    logic_depth: decode_depth
                        .checked_add(1)
                        .and_then(|depth| depth.checked_add(reduction_depth))
                        .ok_or_else(|| {
                            crate::SynthError::invariant(
                                "dynamic-extract one-hot depth estimate overflow",
                            )
                        })?,
                    logic_units: decode_units.checked_add(data_units).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "dynamic-extract one-hot logic estimate overflow",
                        )
                    })?,
                    wiring_units: width.checked_mul(taps).ok_or_else(|| {
                        crate::SynthError::invariant(
                            "dynamic-extract one-hot wiring estimate overflow",
                        )
                    })?,
                }
            }
            _ => {
                return Err(crate::SynthError::invariant(format!(
                    "resource '{}' has no recipe {}",
                    self.resource_name(),
                    recipe.raw()
                )));
            }
        };
        Ok(estimate)
    }
}

impl DynamicExtractProvider {
    fn lower<B: BitBackend>(
        &self,
        recipe: ProviderRecipeId,
        blaster: &mut BitBlaster<'_, B>,
        request: ImplementationRequest<'_>,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        match recipe {
            ONE_HOT | BARREL => blaster.lower_dynamic_extract_architecture(
                request.operator,
                recipe == ONE_HOT,
                request.source,
            ),
            _ => Err(crate::SynthError::invariant(format!(
                "resource '{}' has no recipe {}",
                self.resource_name(),
                recipe.raw()
            ))),
        }
    }
}

pub(super) fn implementation_provider() -> &'static dyn ImplementationProvider {
    &DynamicExtractProvider
}

pub(super) fn lower_implementation<B: BitBackend>(
    recipe: ProviderRecipeId,
    blaster: &mut BitBlaster<'_, B>,
    request: ImplementationRequest<'_>,
) -> Result<Vec<ScalarBit>, crate::SynthError> {
    DynamicExtractProvider.lower(recipe, blaster, request)
}

impl<B: BitBackend> BitBlaster<'_, B> {
    pub(super) fn dynamic_extract_bits(
        &mut self,
        operation: word::OpId,
        result_ty: word::WordType,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let source_operation = self.source_operation(operation)?.ok_or_else(|| {
            crate::SynthError::invariant(
                "region-local generated dynamic extract has no architecture decision",
            )
        })?;
        if self
            .operator_for_source_operation(source_operation)
            .is_none()
        {
            if self.plan.is_operation_elided(source_operation) {
                let placeholder = self.constant(BitVal::Zero, result_ty.state(), source)?;
                return Ok(vec![placeholder; result_ty.width() as usize]);
            }
            return Err(crate::SynthError::invariant(format!(
                "dynamic extract {source_operation:?} has no implementation decision"
            )));
        }
        let operator = self
            .operator_for_source_operation(source_operation)
            .ok_or_else(|| {
                crate::SynthError::invariant(format!(
                    "dynamic extract {source_operation:?} has no implementation decision"
                ))
            })?;
        let source_semantic = self.plan.operator(operator).ok_or_else(|| {
            crate::SynthError::invariant(format!(
                "dynamic extract {source_operation:?} references an unknown semantic operator"
            ))
        })?;
        let candidate = self
            .plan
            .selected_candidate(operator)
            .ok_or_else(|| crate::SynthError::invariant("operator has no candidate"))?;
        let semantic = self.local_semantic_operator(source_semantic, operation)?;
        if semantic.kind() != OperatorKind::DynamicExtract {
            return Err(crate::SynthError::invariant(format!(
                "dynamic extract {operation:?} resolved to {:?}",
                semantic.kind()
            )));
        }
        let implementation_ty =
            word::WordType::new(semantic.width(), result_ty.is_signed(), result_ty.state())
                .map_err(crate::SynthError::from)?;
        let previous = self.active_operator.replace(operator);
        let result = super::lower_implementation(
            candidate.provider(),
            candidate.recipe(),
            self,
            ImplementationRequest {
                operator: semantic,
                result_type: implementation_ty,
                source,
            },
        );
        self.active_operator = previous;
        let mut result = result?;
        if result.len() != semantic.width() as usize {
            return Err(crate::SynthError::invariant(format!(
                "dynamic-extract implementation produced {} bits for width {}",
                result.len(),
                semantic.width()
            )));
        }
        let placeholder = self.constant(BitVal::Zero, result_ty.state(), source)?;
        result.resize(semantic.semantic_width() as usize, placeholder);
        Ok(result)
    }

    pub(super) fn lower_dynamic_extract_architecture(
        &mut self,
        operator: crate::SemanticOperator,
        one_hot: bool,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let [value, offset] = operator.inputs();
        let width = operator.width();
        let extract = operator.dynamic_extract().ok_or_else(|| {
            crate::SynthError::invariant("dynamic extract has no architecture metadata")
        })?;
        // Region lowering may turn the selector into a boundary input, where
        // local range analysis cannot recover constraints proved on the source
        // dataflow graph. The architecture decision retains those immutable
        // source semantics and is therefore authoritative during lowering.
        let max_offset = extract.maximum_offset();
        let alignment = extract.alignment();
        let offset_state = self.value_type(offset)?.state();
        let value = self.value(value)?;
        let offset = self.value(offset)?;
        let available_offsets = value
            .len()
            .checked_sub(width)
            .ok_or_else(|| crate::SynthError::invariant("dynamic extract exceeds source width"))?;
        let selection_max = extract.selection_max();
        let needs_range_guard = max_offset > u128::from(available_offsets);

        if one_hot {
            let taps = self
                .dynamic_extract_taps(offset, selection_max, alignment)
                .ok_or_else(|| {
                    crate::SynthError::invariant(
                        "one-hot dynamic extract cannot enumerate its selected taps",
                    )
                })?;
            let result =
                self.one_hot_dynamic_extract_bits(value, offset, &taps, alignment, width, source)?;
            return self.zero_out_of_range_extract(
                result,
                offset,
                available_offsets,
                offset_state,
                needs_range_guard,
                source,
            );
        }

        // The barrel only ever exposes candidates[0], so walk the stages
        // backward to bound how many positions each stage must maintain;
        // everything past that prefix is dead and never emitted. Constant
        // offset bits collapse their stage to a plain reindex.
        let mut stages = Vec::new();
        for stage in 0..offset.len() {
            let shift = 1usize.checked_shl(stage).ok_or_else(|| {
                crate::SynthError::invariant("dynamic extract stage distance overflow")
            })?;
            if shift as u128 > selection_max {
                continue;
            }
            stages.push((stage, shift, self.offset_bit_constant(offset, stage)));
        }
        let mut needed = vec![0usize; stages.len()];
        let mut prefix = 1usize;
        for (index, &(_, shift, constant)) in stages.iter().enumerate().rev() {
            needed[index] = prefix;
            if constant != Some(false) {
                prefix = prefix.saturating_add(shift);
            }
        }

        let mut result = Vec::with_capacity(width as usize);
        for result_bit in 0..width {
            let mut candidates = (result_bit..value.len())
                .take(prefix)
                .map(|index| self.bit(value, index))
                .collect::<Vec<_>>();
            for (&(stage, shift, constant), &needed_len) in stages.iter().zip(&needed) {
                let limit = needed_len.min(candidates.len().saturating_sub(shift));
                match constant {
                    Some(false) => {}
                    Some(true) => {
                        for index in 0..limit {
                            candidates[index] = candidates[index + shift];
                        }
                    }
                    None => {
                        let control = self.bit(offset, stage);
                        for index in 0..limit {
                            candidates[index] = self.emit_mux(
                                control,
                                candidates[index + shift],
                                candidates[index],
                                source,
                            )?;
                        }
                    }
                }
            }
            result.push(candidates.first().copied().ok_or_else(|| {
                crate::SynthError::invariant("dynamic extract has no source candidate")
            })?);
        }
        self.zero_out_of_range_extract(
            result,
            offset,
            available_offsets,
            offset_state,
            needs_range_guard,
            source,
        )
    }

    fn zero_out_of_range_extract(
        &mut self,
        result: Vec<ScalarBit>,
        offset: BitSpan,
        maximum: u32,
        state: word::LogicStateKind,
        required: bool,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        if !required {
            return Ok(result);
        }
        let offset_bits = (0..offset.len())
            .map(|index| self.bit(offset, index))
            .collect::<Vec<_>>();
        let mut maximum_bits = Vec::with_capacity(offset.len() as usize);
        for index in 0..offset.len() {
            let set = index < u32::BITS && ((maximum >> index) & 1) != 0;
            maximum_bits.push(self.constant(
                if set { BitVal::One } else { BitVal::Zero },
                state,
                source,
            )?);
        }
        let out_of_range = self.unsigned_less(&maximum_bits, &offset_bits, source)?;
        let in_range = self.emit_unary(word::UnaryOp::BitNot, out_of_range, source)?;
        result
            .into_iter()
            .map(|bit| {
                let zero = self.zero_for_scalar(bit, source)?;
                self.emit_mux(in_range, bit, zero, source)
            })
            .collect()
    }

    pub(super) fn offset_bit_constant(&self, offset: BitSpan, index: u32) -> Option<bool> {
        let bit = self.bit(offset, index);
        self.scalar_constant(bit)
    }

    /// Enumerate the offsets a bounded dynamic extract can select. The
    /// proven offset alignment strides the enumeration (scaled indices are
    /// multiples of the element width even before their low bits fold to
    /// constants), and constant offset bits prune the survivors further.
    pub(super) fn dynamic_extract_taps(
        &self,
        offset: BitSpan,
        max_offset: u128,
        alignment: u32,
    ) -> Option<Vec<u128>> {
        if max_offset >= crate::planning::architecture::ONE_HOT_EXTRACT_MAX_OFFSET {
            return None;
        }
        let stride = 1u128.checked_shl(alignment.min(127))?;
        let constants = (0..offset.len())
            .map(|index| self.offset_bit_constant(offset, index))
            .collect::<Vec<_>>();
        let mut taps = Vec::new();
        let mut tap = 0u128;
        'taps: while tap <= max_offset {
            for (index, constant) in constants.iter().enumerate() {
                let expected = index < 128 && (tap >> index) & 1 == 1;
                if constant.is_some_and(|bit| bit != expected) {
                    tap += stride;
                    continue 'taps;
                }
            }
            taps.push(tap);
            tap += stride;
        }
        Some(taps)
    }

    /// Lower a bounded dynamic extract as a shared one-hot offset decode
    /// feeding an AND-OR data reduction instead of a mux tree.
    pub(super) fn one_hot_dynamic_extract_bits(
        &mut self,
        value: BitSpan,
        offset: BitSpan,
        taps: &[u128],
        alignment: u32,
        width: u32,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let tap_bit = |tap: u128, result_bit: u32| -> Result<u32, crate::SynthError> {
            u32::try_from(tap)
                .ok()
                .and_then(|tap| tap.checked_add(result_bit))
                .ok_or_else(|| crate::SynthError::invariant("dynamic extract tap index overflow"))
        };
        if let [tap] = taps {
            return (0..width)
                .map(|result_bit| Ok(self.bit(value, tap_bit(*tap, result_bit)?)))
                .collect();
        }
        let selects = self.shared_one_hot_selects(offset, taps, alignment, source)?;
        (0..width)
            .map(|result_bit| {
                let mut terms = Vec::with_capacity(taps.len());
                for (select, &tap) in selects.iter().zip(taps) {
                    let data = self.bit(value, tap_bit(tap, result_bit)?);
                    terms.push(self.emit_binary(word::BinaryOp::BitAnd, *select, data, source)?);
                }
                self.reduce_balanced(terms, word::BinaryOp::BitOr, source)
            })
            .collect()
    }

    /// Build the offset minterms as a shared binary decoder. Grouping taps by
    /// their most-significant undecided bits lets sibling minterms reuse every
    /// common prefix instead of rebuilding a full AND tree for each tap.
    fn shared_one_hot_selects(
        &mut self,
        offset: BitSpan,
        taps: &[u128],
        alignment: u32,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let indices = (alignment.min(offset.len())..offset.len())
            .rev()
            .filter(|&index| self.offset_bit_constant(offset, index).is_none())
            .collect::<Vec<_>>();
        if indices.is_empty() {
            return Err(crate::SynthError::invariant(
                "one-hot dynamic extract taps are not distinguishable",
            ));
        }

        let mut groups = vec![(taps.iter().copied().enumerate().collect::<Vec<_>>(), None)];
        for index in indices {
            let bit = self.bit(offset, index);
            let inverse = if groups.iter().any(|(taps, _)| {
                taps.iter()
                    .any(|&(_, tap)| index >= 128 || (tap >> index) & 1 == 0)
            }) {
                let inverse = self.emit_unary(word::UnaryOp::BitNot, bit, source)?;
                Some(inverse)
            } else {
                None
            };
            let mut next = Vec::with_capacity(groups.len().saturating_mul(2));
            for (group, prefix) in groups {
                for expected in [false, true] {
                    let child = group
                        .iter()
                        .copied()
                        .filter(|&(_, tap)| {
                            let set = index < 128 && (tap >> index) & 1 == 1;
                            set == expected
                        })
                        .collect::<Vec<_>>();
                    if child.is_empty() {
                        continue;
                    }
                    let literal = if expected {
                        bit
                    } else {
                        inverse.ok_or_else(|| {
                            crate::SynthError::invariant(
                                "one-hot decoder is missing an inverted selector literal",
                            )
                        })?
                    };
                    let select = if let Some(prefix) = prefix {
                        self.emit_binary(word::BinaryOp::BitAnd, prefix, literal, source)?
                    } else {
                        literal
                    };
                    next.push((child, Some(select)));
                }
            }
            groups = next;
        }

        let mut selects = vec![None; taps.len()];
        for (group, select) in groups {
            let [(position, _)] = group.as_slice() else {
                return Err(crate::SynthError::invariant(
                    "one-hot dynamic extract taps are not distinguishable",
                ));
            };
            selects[*position] = select;
        }
        selects
            .into_iter()
            .map(|select| {
                select.ok_or_else(|| {
                    crate::SynthError::invariant("one-hot dynamic extract lost a decoded tap")
                })
            })
            .collect()
    }

    fn reduce_balanced(
        &mut self,
        mut values: Vec<ScalarBit>,
        op: word::BinaryOp,
        source: &word::SourceSpan,
    ) -> Result<ScalarBit, crate::SynthError> {
        while values.len() > 1 {
            let mut next = Vec::with_capacity(values.len().div_ceil(2));
            for pair in values.chunks(2) {
                next.push(match *pair {
                    [left, right] => self.emit_binary(op, left, right, source)?,
                    [single] => single,
                    _ => unreachable!("chunks(2) yields one or two items"),
                });
            }
            values = next;
        }
        values
            .into_iter()
            .next()
            .ok_or_else(|| crate::SynthError::invariant("cannot reduce an empty bit vector"))
    }

    pub(super) fn dynamic_insert_bits(
        &mut self,
        value: word::ValueId,
        offset: word::ValueId,
        replacement: word::ValueId,
        source: &word::SourceSpan,
    ) -> Result<Vec<ScalarBit>, crate::SynthError> {
        let original = self.value(value)?;
        let offset = self.value(offset)?;
        let replacement = self.value(replacement)?;
        if replacement.len() > original.len() {
            return Err(crate::SynthError::invariant(
                "dynamic insert replacement exceeds source width",
            ));
        }
        let state = self.value_type(value)?.state();
        let zero = self.constant(BitVal::Zero, state, source)?;
        let one = self.constant(BitVal::One, state, source)?;
        let mut shifted = vec![zero; original.len() as usize];
        let mut mask = vec![zero; original.len() as usize];
        for index in 0..replacement.len() {
            shifted[index as usize] = self.bit(replacement, index);
            mask[index as usize] = one;
        }
        for stage in 0..offset.len() {
            let distance = 1usize.checked_shl(stage).ok_or_else(|| {
                crate::SynthError::invariant("dynamic insert stage distance overflow")
            })?;
            let control = self.bit(offset, stage);
            for index in (0..shifted.len()).rev() {
                let shifted_value = index
                    .checked_sub(distance)
                    .map_or(zero, |source| shifted[source]);
                let shifted_mask = index
                    .checked_sub(distance)
                    .map_or(zero, |source| mask[source]);
                shifted[index] = self.emit_mux(control, shifted_value, shifted[index], source)?;
                mask[index] = self.emit_mux(control, shifted_mask, mask[index], source)?;
            }
        }
        (0..original.len())
            .map(|index| {
                self.emit_mux(
                    mask[index as usize],
                    shifted[index as usize],
                    self.bit(original, index),
                    source,
                )
            })
            .collect()
    }
}
