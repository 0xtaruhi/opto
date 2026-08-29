// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::boolean::logic::{LogicSignature, MAX_MATCH_INPUTS, TruthTable};
use crate::mapping::{MappedCell, MappedInputConnection, MappedOutputConnection};
#[cfg(test)]
use crate::{BooleanFunction, TargetCell, TargetPin};
use crate::{
    BooleanFunctionRef, SynthesisOptions, TargetCellRef, TargetPinDirection, TargetPinRef,
};
use hashbrown::HashMap;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::sync::OnceLock;

use crate::planning::mapping_policy::{CellCost, compare_cell_cost};
use opto_library::normalized_cell_area;

#[derive(Debug)]
pub(crate) struct CombinationalCellCatalog {
    templates: Vec<CellTemplate>,
    bindings: Box<[CellBinding]>,
    binding_identities: Box<[Box<[u8]>]>,
    bindings_by_truth: HashMap<TruthTable, CellBindingRange>,
    joint_candidates: HashMap<(TruthTable, TruthTable), Vec<JointCellBinding>>,
    joint_input_counts: u8,
    diagnostics: crate::SynthesisDiagnostics,
}

#[derive(Debug, Clone, Copy)]
struct CellBindingRange {
    start: usize,
    len: usize,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CellBindingId(u32);

impl CellBindingId {
    fn try_from_index(index: usize) -> Result<Self, std::num::TryFromIntError> {
        u32::try_from(index).map(Self)
    }

    const fn index(self) -> usize {
        self.0 as usize
    }
}

impl Default for CombinationalCellCatalog {
    fn default() -> Self {
        Self {
            templates: Vec::new(),
            bindings: Box::new([]),
            binding_identities: Box::new([]),
            bindings_by_truth: HashMap::new(),
            joint_candidates: HashMap::new(),
            joint_input_counts: 0,
            diagnostics: crate::SynthesisDiagnostics::default(),
        }
    }
}

impl CombinationalCellCatalog {
    #[allow(
        clippy::cast_precision_loss,
        reason = "the output count is the denominator of an approximate representative cost"
    )]
    pub(crate) fn representative_cost(&self) -> Option<CellCost> {
        let mut count = 0u64;
        let mut total = CellCost {
            area: 0.0,
            delay: 0.0,
            transition: 0.0,
            input_capacitance: 0.0,
        };
        for template in &self.templates {
            for output in 0..template.outputs.len() {
                let cost = template.cost_for_output(output);
                if !has_finite_area_delay(cost) {
                    continue;
                }
                total.area += cost.area;
                total.delay += cost.delay;
                total.transition += cost.transition;
                total.input_capacitance += cost.input_capacitance;
                count = count.checked_add(1)?;
            }
        }
        (count != 0).then(|| CellCost {
            area: total.area / count as f64,
            delay: total.delay / count as f64,
            transition: total.transition / count as f64,
            input_capacitance: total.input_capacitance / count as f64,
        })
    }

    pub(crate) fn binding_cell_name(&self, binding: CellBinding) -> &str {
        &self.templates[binding.template].cell_name
    }

    pub(crate) fn binding_library_cell(
        &self,
        binding: CellBinding,
    ) -> Result<u32, crate::SynthError> {
        u32::try_from(self.templates[binding.template].cell_order)
            .map_err(|_| crate::SynthError::capacity("target cell index exceeds 32-bit capacity"))
    }

    pub(crate) fn joint_binding_cell_name(&self, binding: JointCellBinding) -> &str {
        &self.templates[binding.template].cell_name
    }

    pub(crate) fn joint_binding_library_cell(
        &self,
        binding: JointCellBinding,
    ) -> Result<u32, crate::SynthError> {
        u32::try_from(self.templates[binding.template].cell_order)
            .map_err(|_| crate::SynthError::capacity("target cell index exceeds 32-bit capacity"))
    }

    pub(crate) fn binding_identity(&self, binding: CellBinding) -> Vec<u8> {
        let template = &self.templates[binding.template];
        let mut bytes = Vec::new();
        append_identity_string(&mut bytes, &template.cell_name);
        append_identity_string(&mut bytes, &template.outputs[binding.output].pin);
        bytes.push(binding.inverted_input.unwrap_or(u8::MAX));
        bytes.extend_from_slice(&(template.input_pins.len() as u64).to_le_bytes());
        for (pin, name) in template.input_pins.iter().enumerate() {
            append_identity_string(&mut bytes, name);
            bytes.extend_from_slice(
                &(binding.pin_to_signature.signature_index(pin) as u64).to_le_bytes(),
            );
        }
        bytes
    }

    pub(crate) fn joint_binding_identity(&self, binding: JointCellBinding) -> Vec<u8> {
        let template = &self.templates[binding.template];
        let mut bytes = Vec::new();
        append_identity_string(&mut bytes, &template.cell_name);
        for &output in &binding.outputs {
            append_identity_string(&mut bytes, &template.outputs[output].pin);
        }
        bytes.extend_from_slice(&(template.input_pins.len() as u64).to_le_bytes());
        for (pin, name) in template.input_pins.iter().enumerate() {
            append_identity_string(&mut bytes, name);
            bytes.extend_from_slice(
                &(binding.pin_to_signature.signature_index(pin) as u64).to_le_bytes(),
            );
        }
        bytes
    }

    pub(crate) fn new(
        options: &SynthesisOptions,
        diagnostics: crate::SynthesisDiagnostics,
    ) -> Self {
        Self::from_cells(&options.target_cells, diagnostics)
    }

    pub(crate) fn from_cells(
        cells: &opto_library::TargetCellSet,
        diagnostics: crate::SynthesisDiagnostics,
    ) -> Self {
        let mut catalog = Self {
            diagnostics,
            ..Self::default()
        };
        let mut candidates_by_truth = HashMap::<TruthTable, Vec<CellBinding>>::new();
        let permutations = permutation_table();
        for (cell_order, cell) in cells.synthesis_cells() {
            catalog.index_cell(cell_order, cell, permutations, &mut candidates_by_truth);
        }
        let templates = &catalog.templates;
        for candidates in candidates_by_truth.values_mut() {
            candidates.sort_by(|left, right| compare_candidates(templates, left, right));
            retain_binding_pareto_frontier(templates, candidates);
        }
        let mut indexed_bindings = candidates_by_truth.into_iter().collect::<Vec<_>>();
        indexed_bindings.sort_unstable_by_key(|(truth, _)| *truth);
        let binding_count = indexed_bindings
            .iter()
            .map(|(_, candidates)| candidates.len())
            .sum();
        let mut bindings = Vec::with_capacity(binding_count);
        let mut bindings_by_truth = HashMap::with_capacity(indexed_bindings.len());
        for (truth, candidates) in indexed_bindings {
            let start = bindings.len();
            let len = candidates.len();
            bindings.extend(candidates);
            bindings_by_truth.insert(truth, CellBindingRange { start, len });
        }
        catalog.bindings = bindings.into_boxed_slice();
        catalog.binding_identities = catalog
            .bindings
            .iter()
            .map(|&binding| catalog.binding_identity(binding).into_boxed_slice())
            .collect();
        catalog.bindings_by_truth = bindings_by_truth;
        for candidates in catalog.joint_candidates.values_mut() {
            candidates.sort_by(|left, right| {
                templates[left.template]
                    .cell_name
                    .cmp(&templates[right.template].cell_name)
                    .then_with(|| left.outputs.cmp(&right.outputs))
                    .then_with(|| left.pin_to_signature.cmp(&right.pin_to_signature))
                    .then_with(|| left.template.cmp(&right.template))
            });
        }
        catalog
    }

    pub(crate) fn binding_for_identity(
        &self,
        truth: TruthTable,
        identity: &[u8],
    ) -> Option<CellBinding> {
        let range = self.bindings_by_truth.get(&truth).copied()?;
        self.bindings[range.start..range.start + range.len]
            .iter()
            .zip(&self.binding_identities[range.start..range.start + range.len])
            .find(|(_, candidate_identity)| candidate_identity.as_ref() == identity)
            .map(|(&binding, _)| binding)
    }

    pub(crate) fn joint_binding_for_identity(
        &self,
        truths: (TruthTable, TruthTable),
        identity: &[u8],
    ) -> Option<JointCellBinding> {
        self.joint_candidates
            .get(&truths)?
            .iter()
            .copied()
            .find(|&binding| self.joint_binding_identity(binding) == identity)
    }

    pub(crate) fn best_joint_binding(
        &self,
        first: TruthTable,
        second: TruthTable,
    ) -> Option<JointCellBinding> {
        self.joint_candidates
            .get(&(first, second))?
            .iter()
            .min_by(|left, right| {
                compare_cell_cost(self.joint_cost(**left), self.joint_cost(**right))
                    .then_with(|| left.cmp(right))
            })
            .copied()
    }

    pub(crate) fn has_joint_input_count(&self, input_count: usize) -> bool {
        input_count < u8::BITS as usize && self.joint_input_counts & (1u8 << input_count) != 0
    }

    pub(crate) fn joint_cost(&self, binding: JointCellBinding) -> CellCost {
        let template = &self.templates[binding.template];
        let first = template.cost_for_output(binding.outputs[0]);
        let second = template.cost_for_output(binding.outputs[1]);
        CellCost {
            area: template.area,
            delay: first.delay.max(second.delay),
            transition: first.transition.max(second.transition),
            input_capacitance: template.input_capacitance,
        }
    }

    pub(crate) fn joint_cell(
        &self,
        binding: JointCellBinding,
        signature: &LogicSignature,
        targets: [opto_ir::word::ValueId; 2],
    ) -> MappedCell {
        let template = &self.templates[binding.template];
        MappedCell {
            cell_name: template.cell_name.clone(),
            input_connections: template
                .input_pins
                .iter()
                .enumerate()
                .map(|(pin_index, pin)| MappedInputConnection {
                    pin: pin.clone(),
                    value: signature.inputs[binding.pin_to_signature.signature_index(pin_index)],
                })
                .collect(),
            output_connections: binding
                .outputs
                .iter()
                .zip(targets)
                .map(|(&output, value)| MappedOutputConnection {
                    pin: template.outputs[output].pin.clone(),
                    value,
                })
                .collect(),
        }
    }

    pub(crate) fn best_binding_for_truth(&self, truth: TruthTable) -> Option<CellBinding> {
        self.matching_bindings(truth)
            .iter()
            .filter(|candidate| candidate.inverted_input.is_none())
            .copied()
            .min_by(|left, right| {
                compare_cell_cost(self.cost_for_binding(*left), self.cost_for_binding(*right))
                    .then_with(|| left.cmp(right))
            })
    }

    pub(crate) fn matching_bindings(&self, truth: TruthTable) -> &[CellBinding] {
        let Some(range) = self.bindings_by_truth.get(&truth).copied() else {
            return &[];
        };
        &self.bindings[range.start..range.start + range.len]
    }

    pub(crate) fn binding(&self, id: CellBindingId) -> CellBinding {
        self.bindings[id.index()]
    }

    pub(crate) fn cost_for_binding_id(&self, id: CellBindingId) -> CellCost {
        self.cost_for_binding(self.binding(id))
    }

    pub(crate) fn visit_cover_bindings(
        &self,
        truth: TruthTable,
        mut visit: impl FnMut(CellBindingId, CellBinding),
    ) -> Result<(), std::num::TryFromIntError> {
        let Some(range) = self.bindings_by_truth.get(&truth).copied() else {
            return Ok(());
        };
        let bindings = &self.bindings[range.start..range.start + range.len];
        for (offset, &binding) in bindings.iter().enumerate() {
            visit(
                CellBindingId::try_from_index(range.start + offset)?,
                binding,
            );
        }
        Ok(())
    }

    pub(crate) const fn diagnostics(&self) -> crate::SynthesisDiagnostics {
        self.diagnostics
    }

    pub(crate) fn can_invert(&self) -> bool {
        self.matching_bindings(crate::boolean::logic::inverter_truth())
            .iter()
            .any(|binding| binding.inverted_input.is_none())
    }

    pub(crate) fn cost_for_binding(&self, binding: CellBinding) -> CellCost {
        self.templates[binding.template].cost_for_output(binding.output)
    }

    pub(crate) fn estimate_binding(
        &self,
        binding: CellBinding,
        signature_transitions: &[f64],
        output_load: f64,
    ) -> CellCost {
        self.templates[binding.template].estimate_output(
            binding.output,
            binding.pin_to_signature,
            signature_transitions,
            output_load,
        )
    }

    pub(crate) fn binding_input_loads(&self, binding: CellBinding) -> Vec<(usize, f64)> {
        self.templates[binding.template]
            .input_capacitances
            .iter()
            .copied()
            .enumerate()
            .map(|(pin, load)| (binding.pin_to_signature.signature_index(pin), load))
            .collect()
    }

    pub(crate) fn estimate_joint_binding(
        &self,
        binding: JointCellBinding,
        signature_transitions: &[f64],
        output_loads: [f64; 2],
    ) -> CellCost {
        let outputs = self.estimate_joint_outputs(binding, signature_transitions, output_loads);
        CellCost {
            area: outputs[0].area,
            delay: outputs[0].delay.max(outputs[1].delay),
            transition: outputs[0].transition.max(outputs[1].transition),
            input_capacitance: outputs[0].input_capacitance,
        }
    }

    pub(crate) fn estimate_joint_outputs(
        &self,
        binding: JointCellBinding,
        signature_transitions: &[f64],
        output_loads: [f64; 2],
    ) -> [CellCost; 2] {
        let template = &self.templates[binding.template];
        let first = template.estimate_output(
            binding.outputs[0],
            binding.pin_to_signature,
            signature_transitions,
            output_loads[0],
        );
        let second = template.estimate_output(
            binding.outputs[1],
            binding.pin_to_signature,
            signature_transitions,
            output_loads[1],
        );
        [first, second]
    }

    pub(crate) fn joint_input_loads(&self, binding: JointCellBinding) -> Vec<(usize, f64)> {
        self.templates[binding.template]
            .input_capacitances
            .iter()
            .copied()
            .enumerate()
            .map(|(pin, load)| (binding.pin_to_signature.signature_index(pin), load))
            .collect()
    }

    pub(crate) fn cell_for_binding(
        &self,
        binding: CellBinding,
        signature: &LogicSignature,
        target: opto_ir::word::ValueId,
    ) -> MappedCell {
        binding.materialize_mapped_cell(&self.templates[binding.template], signature, target)
    }

    pub(crate) fn binding_connection_count(&self, binding: CellBinding) -> usize {
        self.templates[binding.template]
            .input_pins
            .len()
            .saturating_add(1)
    }

    pub(crate) fn joint_binding_connection_count(&self, binding: JointCellBinding) -> usize {
        self.templates[binding.template]
            .input_pins
            .len()
            .saturating_add(2)
    }

    pub(crate) fn best_cost_for_signature(&self, signature: &LogicSignature) -> Option<CellCost> {
        self.best_binding_for_truth(signature.truth)
            .map(|candidate| self.templates[candidate.template].cost_for_output(candidate.output))
    }

    fn index_cell(
        &mut self,
        cell_order: usize,
        cell: TargetCellRef<'_>,
        permutations: &PermutationTable,
        candidates_by_truth: &mut HashMap<TruthTable, Vec<CellBinding>>,
    ) {
        if cell.sequential().next().is_some() {
            return;
        }
        let inputs = target_input_pins(cell);
        if inputs.len() > MAX_MATCH_INPUTS {
            return;
        }
        let outputs = cell
            .pins()
            .filter(|pin| pin.direction() == TargetPinDirection::Output)
            .filter(|pin| pin.three_state().is_none())
            .filter_map(|output| CellOutputTemplate::new(output, &inputs))
            .collect::<Vec<_>>();
        if outputs.is_empty() {
            return;
        }
        let template = self.templates.len();
        self.templates
            .push(CellTemplate::new(cell_order, cell, &inputs, outputs));
        for (output, cell_truth) in self.templates[template]
            .outputs
            .iter()
            .map(|output| output.truth)
            .enumerate()
        {
            let mut canonical = HashMap::<TruthTable, (bool, PinPermutation, Option<u8>)>::new();
            let mut record = |truth: TruthTable,
                              pin_to_signature: PinPermutation,
                              inverted_input: Option<u8>| {
                let entry = (inverted_input.is_some(), pin_to_signature, inverted_input);
                canonical
                    .entry(truth)
                    .and_modify(|current| *current = (*current).min(entry))
                    .or_insert(entry);
            };
            for &permutation in permutations.for_len(inputs.len()) {
                let truth = permute_truth_to_signature_order(cell_truth, &permutation);
                record(truth, permutation, None);
            }
            index_tied_projections(cell_truth, permutations, &mut record);
            for (truth, (_, pin_to_signature, inverted_input)) in canonical {
                candidates_by_truth
                    .entry(truth)
                    .or_default()
                    .push(CellBinding {
                        template,
                        output,
                        pin_to_signature,
                        inverted_input,
                    });
            }
        }
        self.index_joint_outputs(template, permutations);
    }

    fn index_joint_outputs(&mut self, template: usize, permutations: &PermutationTable) {
        let outputs = self.templates[template].outputs.len();
        if outputs < 2 {
            return;
        }
        let input_count = self.templates[template].input_pins.len();
        if input_count < u8::BITS as usize {
            self.joint_input_counts |= 1u8 << input_count;
        }
        for first in 0..outputs {
            for second in 0..outputs {
                if first == second {
                    continue;
                }
                let first_truth = self.templates[template].outputs[first].truth;
                let second_truth = self.templates[template].outputs[second].truth;
                let mut canonical = HashMap::<(TruthTable, TruthTable), PinPermutation>::new();
                for &permutation in permutations.for_len(input_count) {
                    let key = (
                        permute_truth_to_signature_order(first_truth, &permutation),
                        permute_truth_to_signature_order(second_truth, &permutation),
                    );
                    canonical
                        .entry(key)
                        .and_modify(|current| *current = (*current).min(permutation))
                        .or_insert(permutation);
                }
                for (key, pin_to_signature) in canonical {
                    self.joint_candidates
                        .entry(key)
                        .or_default()
                        .push(JointCellBinding {
                            template,
                            outputs: [first, second],
                            pin_to_signature,
                        });
                }
            }
        }
    }
}

fn append_identity_string(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
}

fn index_tied_projections(
    cell_truth: TruthTable,
    permutations: &PermutationTable,
    record: &mut impl FnMut(TruthTable, PinPermutation, Option<u8>),
) {
    let pin_count = cell_truth.input_count;
    if pin_count < 2 {
        return;
    }
    let signature_len = pin_count - 1;
    for tied in 0..pin_count {
        for tied_to in 0..pin_count {
            if tied_to == tied {
                continue;
            }
            for inverted in [false, true] {
                let projected = project_tied_truth(cell_truth, tied_to, tied, inverted);
                for &permutation in permutations.for_len(signature_len) {
                    let truth = permute_truth_to_signature_order(projected, &permutation);
                    let mut values = [0u8; MAX_MATCH_INPUTS];
                    let mut projected_position = 0;
                    for (pin, value) in values.iter_mut().enumerate().take(pin_count) {
                        if pin == tied {
                            continue;
                        }
                        *value = permutation.values[projected_position];
                        projected_position += 1;
                    }
                    let tied_to_signature = values[tied_to];
                    let inverted_input = if inverted {
                        values[tied] = u8::try_from(signature_len)
                            .expect("cell signature length fits a compact input index");
                        Some(tied_to_signature)
                    } else {
                        values[tied] = tied_to_signature;
                        None
                    };
                    record(
                        truth,
                        PinPermutation {
                            len: pin_count,
                            values,
                        },
                        inverted_input,
                    );
                }
            }
        }
    }
}

fn project_tied_truth(
    cell_truth: TruthTable,
    tied_to: usize,
    tied: usize,
    inverted: bool,
) -> TruthTable {
    let pin_count = cell_truth.input_count;
    let mut bits = 0u64;
    for assignment in 0..1usize << (pin_count - 1) {
        let low = assignment & ((1 << tied) - 1);
        let high = (assignment >> tied) << (tied + 1);
        let mut cell_assignment = low | high;
        let tie_value = ((cell_assignment >> tied_to) & 1 == 1) != inverted;
        if tie_value {
            cell_assignment |= 1 << tied;
        }
        if cell_truth.bit(cell_assignment) {
            bits |= 1u64 << assignment;
        }
    }
    TruthTable {
        input_count: pin_count - 1,
        bits,
    }
}

fn target_input_pins(cell: TargetCellRef<'_>) -> Vec<TargetPinRef<'_>> {
    cell.pins()
        .filter(|pin| pin.direction() == TargetPinDirection::Input)
        .collect()
}

pub(crate) fn function_truth_table(
    function: BooleanFunctionRef<'_>,
    inputs: &[TargetPinRef<'_>],
) -> Option<TruthTable> {
    let names = inputs.iter().map(|pin| pin.name()).collect::<Vec<_>>();
    Some(TruthTable {
        input_count: names.len(),
        bits: function.truth_table_bits(&names)?,
    })
}

pub(crate) fn permute_truth_to_signature_order(
    cell_truth: TruthTable,
    permutation: &PinPermutation,
) -> TruthTable {
    let mut bits = 0u64;
    for signature_assignment in 0..(1usize << cell_truth.input_count) {
        let mut cell_assignment = 0usize;
        for pin_index in 0..permutation.len() {
            let signature_index = permutation.signature_index(pin_index);
            let value = (signature_assignment >> signature_index) & 1;
            cell_assignment |= value << pin_index;
        }
        if cell_truth.bit(cell_assignment) {
            bits |= 1u64 << signature_assignment;
        }
    }
    TruthTable {
        input_count: cell_truth.input_count,
        bits,
    }
}

fn find_permutation(len: usize) -> Vec<PinPermutation> {
    let mut values = [0u8; MAX_MATCH_INPUTS];
    for (index, value) in values.iter_mut().take(len).enumerate() {
        *value = index
            .try_into()
            .expect("logic matcher input index fits compact permutation");
    }
    let mut permutations = Vec::new();
    find_permutation_inner(0, len, &mut values, &mut permutations);
    permutations
}

fn find_permutation_inner(
    index: usize,
    len: usize,
    values: &mut [u8; MAX_MATCH_INPUTS],
    permutations: &mut Vec<PinPermutation>,
) {
    if index == len {
        permutations.push(PinPermutation {
            len,
            values: *values,
        });
        return;
    }

    for candidate in index..len {
        values.swap(index, candidate);
        find_permutation_inner(index + 1, len, values, permutations);
        values.swap(index, candidate);
    }
}

/// Report whether a target cell is a combinational one-input buffer or inverter.
///
/// Cells with three-state behavior, multiple inputs, or no Boolean output
/// function are excluded even if their names suggest buffer behavior.
#[must_use]
pub fn target_cell_is_buffer_or_inverter(cell: TargetCellRef<'_>) -> bool {
    let inputs = target_input_pins(cell);
    if inputs.len() != 1 {
        return false;
    }
    let Some(output) = cell.pins().find(|pin| {
        pin.direction() == TargetPinDirection::Output
            && pin.function().is_some()
            && pin.three_state().is_none()
    }) else {
        return false;
    };
    let Some(function) = output.function() else {
        return false;
    };
    let input_truth = TruthTable {
        input_count: 1,
        bits: 0b10,
    };
    let inverter_truth = TruthTable {
        input_count: 1,
        bits: 0b01,
    };
    let Some(truth) = function_truth_table(function, &inputs) else {
        return false;
    };
    truth == input_truth || truth == inverter_truth
}

mod timing;
#[cfg(test)]
use timing::reference_fanout_load;
pub(crate) use timing::{CellBinding, JointCellBinding};
use timing::{CellOutputTemplate, CellTemplate};

fn has_finite_area_delay(cost: CellCost) -> bool {
    [cost.area, cost.delay]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
}

fn compare_candidates(
    templates: &[CellTemplate],
    left: &CellBinding,
    right: &CellBinding,
) -> Ordering {
    let left_template = &templates[left.template];
    let right_template = &templates[right.template];
    left_template
        .cell_name
        .cmp(&right_template.cell_name)
        .then_with(|| left_template.cell_order.cmp(&right_template.cell_order))
        .then_with(|| {
            left_template.outputs[left.output]
                .pin
                .cmp(&right_template.outputs[right.output].pin)
        })
        .then_with(|| left.inverted_input.cmp(&right.inverted_input))
        .then_with(|| left.pin_to_signature.cmp(&right.pin_to_signature))
}

fn retain_binding_pareto_frontier(templates: &[CellTemplate], candidates: &mut Vec<CellBinding>) {
    let dominated = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let candidate_cost = templates[candidate.template].cost_for_output(candidate.output);
            candidates.iter().enumerate().any(|(other_index, other)| {
                if candidate.inverted_input != other.inverted_input || other_index == index {
                    return false;
                }
                let other_cost = templates[other.template].cost_for_output(other.output);
                cell_cost_dominates(other_cost, candidate_cost, other_index < index)
            })
        })
        .collect::<Vec<_>>();
    let mut ordinal = 0usize;
    candidates.retain(|_| {
        let keep = !dominated[ordinal];
        ordinal += 1;
        keep
    });
}

fn cell_cost_dominates(left: CellCost, right: CellCost, left_precedes_right: bool) -> bool {
    left.area <= right.area
        && left.delay <= right.delay
        && left.transition <= right.transition
        && left.input_capacitance <= right.input_capacitance
        && (left_precedes_right
            || left.area < right.area
            || left.delay < right.delay
            || left.transition < right.transition
            || left.input_capacitance < right.input_capacitance)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct PinPermutation {
    len: usize,
    values: [u8; MAX_MATCH_INPUTS],
}

impl PinPermutation {
    fn len(self) -> usize {
        self.len
    }

    pub(crate) fn signature_index(self, pin_index: usize) -> usize {
        usize::from(self.values[pin_index])
    }
}

#[derive(Debug)]
pub(crate) struct PermutationTable {
    by_len: [Vec<PinPermutation>; MAX_MATCH_INPUTS + 1],
}

impl PermutationTable {
    pub(crate) fn new() -> Self {
        Self {
            by_len: std::array::from_fn(find_permutation),
        }
    }

    pub(crate) fn for_len(&self, len: usize) -> &[PinPermutation] {
        &self.by_len[len]
    }
}

fn permutation_table() -> &'static PermutationTable {
    static TABLE: OnceLock<PermutationTable> = OnceLock::new();
    TABLE.get_or_init(PermutationTable::new)
}

#[cfg(test)]
mod tests;
