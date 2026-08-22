// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! SAT miters between word-level values and bit-level implementations.
//!
//! Cut values become shared unconstrained SAT variables. This keeps the miter
//! proportional to the compared cones while making an UNSAT result valid for
//! every possible cut assignment.

use super::logic::{LogicEncoding, encode_logic_network};
use super::{FormalError, ProofOutcome, ProofReport};
use opto_ir::BitVal;
use opto_ir::word;
use std::collections::{BTreeMap, BTreeSet};

mod arithmetic;
use varisat::{ExtendFormula, Lit, Solver};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Exact finite transition relation under the encoded signal-cut model.
pub struct FiniteTransitionRelation {
    successors: Box<[Box<[usize]>]>,
    report: ProofReport,
}

impl FiniteTransitionRelation {
    /// Returns target-state ordinals reachable in one transition.
    #[must_use]
    pub fn successors(&self, state: usize) -> Option<&[usize]> {
        self.successors.get(state).map(Box::as_ref)
    }

    /// Returns SAT encoding size metrics shared by all transition queries.
    #[must_use]
    pub fn report(&self) -> ProofReport {
        self.report
    }
}

/// Enumerates feasible transitions between a supplied finite set of states.
///
/// `state` and `next_state` must have the same width as every entry in
/// `states`. Other signal boundaries remain unconstrained, so an edge exists
/// exactly when some boundary assignment can produce the target state.
///
/// # Errors
///
/// Returns an error for an empty state set, width mismatch, unresolved X/Z
/// states, malformed IR, unsupported operations, representation overflow, or
/// SAT solver failure.
pub fn enumerate_finite_transitions(
    module: &word::WordModule,
    state: word::ValueId,
    next_state: word::ValueId,
    states: &[opto_ir::ConstBits],
) -> Result<FiniteTransitionRelation, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    let state_bits = encoder.value(state)?;
    let next_bits = encoder.value(next_state)?;
    enumerate_finite_transition_bits(encoder, &state_bits, &next_bits, states)
}

/// Enumerates feasible transitions while treating a complete signal as state.
///
/// This variant avoids requiring a pre-existing whole-signal read in Word IR;
/// every other signal remains an unconstrained environment or state boundary.
///
/// # Errors
///
/// Returns the same errors as [`enumerate_finite_transitions`].
pub fn enumerate_finite_signal_transitions(
    module: &word::WordModule,
    state: word::SignalId,
    next_state: word::ValueId,
    states: &[opto_ir::ConstBits],
) -> Result<FiniteTransitionRelation, FormalError> {
    let state_width = module
        .signal(state)
        .ok_or_else(|| {
            FormalError::invalid(format!(
                "finite transition relation references unknown signal {state:?}"
            ))
        })?
        .ty
        .width();
    let mut encoder = CnfEncoder::new(module);
    let state_bits = (0..state_width)
        .map(|bit| encoder.signal(state, bit))
        .collect::<Vec<_>>();
    let next_bits = encoder.value(next_state)?;
    enumerate_finite_transition_bits(encoder, &state_bits, &next_bits, states)
}

/// Enumerates feasible transitions using complete register update semantics.
///
/// # Errors
///
/// Returns the same errors as [`enumerate_finite_transitions`].
pub fn enumerate_finite_signal_register_transitions(
    module: &word::WordModule,
    state: word::SignalId,
    register: &word::RegisterOp,
    states: &[opto_ir::ConstBits],
) -> Result<FiniteTransitionRelation, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    let state_bits = encoder.signal_bits(state)?;
    let next_bits = encoder.register_next(register, state)?;
    enumerate_finite_transition_bits(encoder, &state_bits, &next_bits, states)
}

fn enumerate_finite_transition_bits(
    mut encoder: CnfEncoder<'_>,
    state_bits: &[Lit],
    next_bits: &[Lit],
    states: &[opto_ir::ConstBits],
) -> Result<FiniteTransitionRelation, FormalError> {
    if states.is_empty() {
        return Err(FormalError::invalid(
            "finite transition relation requires at least one state",
        ));
    }
    if state_bits.len() != next_bits.len() {
        return Err(FormalError::invalid(format!(
            "finite transition width mismatch: state={}, next={}",
            state_bits.len(),
            next_bits.len()
        )));
    }
    let assignments = states
        .iter()
        .map(|state| constant_assignment(state, state_bits.len()))
        .collect::<Result<Vec<_>, FormalError>>()?;
    let mut successors = Vec::with_capacity(states.len());
    let mut assumptions = Vec::with_capacity(state_bits.len().saturating_mul(2));
    for source in &assignments {
        let mut targets = Vec::new();
        for (target, target_assignment) in assignments.iter().enumerate() {
            assumptions.clear();
            assumptions.extend(
                state_bits
                    .iter()
                    .zip(source)
                    .map(|(&literal, &value)| if value { literal } else { !literal }),
            );
            assumptions.extend(
                next_bits
                    .iter()
                    .zip(target_assignment)
                    .map(|(&literal, &value)| if value { literal } else { !literal }),
            );
            encoder.solver.assume(&assumptions);
            if encoder
                .solver
                .solve()
                .map_err(|source| FormalError::Solver {
                    context: "finite transition relation",
                    source,
                })?
            {
                targets.push(target);
            }
        }
        successors.push(targets.into_boxed_slice());
    }
    Ok(FiniteTransitionRelation {
        successors: successors.into_boxed_slice(),
        report: ProofReport {
            encoded_values: encoder.encoded_values,
            clauses: encoder.clauses,
        },
    })
}

fn constant_assignment(
    constant: &opto_ir::ConstBits,
    width: usize,
) -> Result<Vec<bool>, FormalError> {
    if constant.as_slice().len() != width {
        return Err(FormalError::invalid(format!(
            "constant width mismatch: expected {width}, got {}",
            constant.width()
        )));
    }
    constant
        .as_slice()
        .iter()
        .rev()
        .map(|bit| match bit {
            BitVal::Zero => Ok(false),
            BitVal::One => Ok(true),
            BitVal::X | BitVal::Z => Err(FormalError::unsupported(
                "constant assignment does not accept X/Z bits",
            )),
        })
        .collect()
}

/// Proves that a word value equals an ordered vector of scalar bit values.
///
/// # Errors
///
/// Returns an error for width mismatch, malformed IR, unsupported operations,
/// representation overflow, or SAT solver failure.
pub fn prove_value_bits(
    module: &word::WordModule,
    reference: word::ValueId,
    implementation: &[word::ValueId],
) -> Result<ProofOutcome, FormalError> {
    prove_value_bits_at_cut(module, reference, implementation, &[])
}

/// Prove equivalence with each cut operand encoded as unconstrained fresh
/// variables shared between its word-level value and its bit decomposition,
/// keeping the fanin cone above the cut out of the miter. Constant bits keep
/// their constant encoding. A free cut makes the proof strictly stronger —
/// equivalence must hold for every operand combination, not only those the
/// surrounding logic can drive — so an UNSAT result remains a valid proof,
/// while the miter size stays proportional to the two compared networks
/// instead of the design.
///
/// # Errors
///
/// Returns an error for width mismatch, an invalid cut relation, malformed or
/// unsupported Word IR, representation overflow, or SAT solver failure.
pub fn prove_value_bits_at_cut(
    module: &word::WordModule,
    reference: word::ValueId,
    implementation: &[word::ValueId],
    cuts: &[(word::ValueId, Vec<word::ValueId>)],
) -> Result<ProofOutcome, FormalError> {
    prove_value_bits_with_cut(module, reference, implementation, cuts, false)
}

/// Prove that an implementation matches the least-significant prefix of a
/// wider reference value. The omitted high bits are outside the observable
/// cone established by synthesis demand analysis.
///
/// # Errors
///
/// Returns an error for an implementation wider than the reference, an invalid
/// cut relation, malformed or unsupported Word IR, capacity overflow, or SAT
/// solver failure.
pub fn prove_value_prefix_at_cut(
    module: &word::WordModule,
    reference: word::ValueId,
    implementation: &[word::ValueId],
    cuts: &[(word::ValueId, Vec<word::ValueId>)],
) -> Result<ProofOutcome, FormalError> {
    prove_value_bits_with_cut(module, reference, implementation, cuts, true)
}

/// Prove a word-level reference against a detached Boolean implementation.
/// Each scalar boundary value is encoded once and shared with the logic
/// network input whose origin is its ordinal in `boundary`. The detached
/// implementation can therefore be checked before it is committed to the
/// Word IR.
///
/// # Errors
///
/// Returns an error unless every boundary value is scalar and all boundary,
/// output, cut, and logic-network references are valid and width-compatible;
/// unsupported IR, capacity overflow, and SAT solver failure are also reported.
pub fn prove_value_against_logic_at_cut(
    module: &word::WordModule,
    reference: word::ValueId,
    implementation: &opto_ir::logic::LogicNetwork,
    implementation_outputs: &[opto_ir::logic::Lit],
    boundary: &[word::ValueId],
) -> Result<ProofOutcome, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    let mut boundary_literals = Vec::with_capacity(boundary.len());
    for &value in boundary {
        encoder.cut_value(value, &[value])?;
        let bits = encoder.value(value)?;
        let [literal]: [Lit; 1] = bits.try_into().map_err(|bits: Vec<_>| {
            FormalError::invalid(format!(
                "logic boundary value {value:?} has {} bits, expected one",
                bits.len()
            ))
        })?;
        boundary_literals.push(literal);
    }

    let reference_bits = encoder.value(reference)?;
    let (implementation_bits, implementation_nodes) = {
        let mut logic = WordLogicEncoder {
            encoder: &mut encoder,
            boundary: &boundary_literals,
            constant_false: None,
        };
        encode_logic_network(&mut logic, implementation, implementation_outputs)?
    };
    if reference_bits.len() != implementation_bits.len() {
        return Err(FormalError::invalid(format!(
            "equivalence proof width mismatch: reference={}, implementation={}",
            reference_bits.len(),
            implementation_bits.len()
        )));
    }
    let differences = reference_bits
        .into_iter()
        .zip(implementation_bits)
        .map(|(reference, implementation)| encoder.xor(reference, implementation))
        .collect::<Vec<_>>();
    encoder.solver.add_clause(&differences);
    let satisfiable = encoder
        .solver
        .solve()
        .map_err(|source| FormalError::Solver {
            context: "word/logic equivalence",
            source,
        })?;
    if satisfiable {
        return Ok(ProofOutcome::disproved(
            "equivalence proof failed: SAT miter found a counterexample",
        ));
    }
    Ok(ProofOutcome::proved(ProofReport {
        encoded_values: encoder.encoded_values + implementation_nodes,
        clauses: encoder.clauses,
    }))
}

/// Proves equality of two word values under word-value equality assumptions.
///
/// This is the induction-step primitive for sequential rewrites. Each
/// assumption constrains two same-width combinational values to be equal
/// before the miter asks whether `reference` and `implementation` can differ.
///
/// # Errors
///
/// Returns an error for width mismatch, malformed IR, unsupported operations,
/// representation overflow, or SAT solver failure.
pub fn prove_value_equivalence_under_assumptions(
    module: &word::WordModule,
    reference: word::ValueId,
    implementation: word::ValueId,
    assumptions: &[(word::ValueId, word::ValueId)],
) -> Result<ProofOutcome, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    for &(left, right) in assumptions {
        let left = encoder.value(left)?;
        let right = encoder.value(right)?;
        if left.len() != right.len() {
            return Err(FormalError::invalid(format!(
                "equivalence assumption width mismatch: left={}, right={}",
                left.len(),
                right.len()
            )));
        }
        for (left, right) in left.into_iter().zip(right) {
            encoder.clause(&[!left, right]);
            encoder.clause(&[left, !right]);
        }
    }

    let reference = encoder.value(reference)?;
    let implementation = encoder.value(implementation)?;
    if reference.len() != implementation.len() {
        return Err(FormalError::invalid(format!(
            "equivalence proof width mismatch: reference={}, implementation={}",
            reference.len(),
            implementation.len()
        )));
    }
    let differences = reference
        .into_iter()
        .zip(implementation)
        .map(|(reference, implementation)| encoder.xor(reference, implementation))
        .collect::<Vec<_>>();
    encoder.clause(&differences);
    let satisfiable = encoder
        .solver
        .solve()
        .map_err(|source| FormalError::Solver {
            context: "conditional word equivalence",
            source,
        })?;
    if satisfiable {
        return Ok(ProofOutcome::disproved(
            "conditional equivalence proof failed: SAT miter found a counterexample",
        ));
    }
    Ok(ProofOutcome::proved(ProofReport {
        encoded_values: encoder.encoded_values,
        clauses: encoder.clauses,
    }))
}

/// Proves equality of two private Word fragments at an explicit shared cut.
///
/// Every pair in `shared_inputs` is bound to the same fresh SAT bits in both
/// modules. The output vectors are then flattened in order and compared in one
/// miter. Dense value IDs remain local to their module and are never treated as
/// cross-module identity.
///
/// # Errors
///
/// Returns an error for a cut or output width mismatch, malformed or
/// unsupported Word IR, representation overflow, or SAT solver failure.
pub fn prove_module_values_equivalent_at_cut(
    reference: &word::WordModule,
    reference_outputs: &[word::ValueId],
    implementation: &word::WordModule,
    implementation_outputs: &[word::ValueId],
    shared_inputs: &[(word::ValueId, word::ValueId)],
) -> Result<ProofOutcome, FormalError> {
    let mut reference_encoder = CnfEncoder::new(reference);
    let mut implementation_cuts = Vec::with_capacity(shared_inputs.len());
    for &(reference_input, implementation_input) in shared_inputs {
        let reference_width = reference_encoder.value_width(reference_input)?;
        let implementation_width = implementation
            .value(implementation_input)
            .ok_or_else(|| {
                FormalError::invalid(format!(
                    "module equivalence references unknown implementation value {implementation_input:?}"
                ))
            })?
            .ty
            .width() as usize;
        if reference_width != implementation_width {
            return Err(FormalError::invalid(format!(
                "module equivalence cut width mismatch: reference={reference_width}, implementation={implementation_width}"
            )));
        }
        let literals = (0..reference_width)
            .map(|_| reference_encoder.solver.new_lit())
            .collect::<Vec<_>>();
        reference_encoder.bind_value(reference_input, &literals)?;
        implementation_cuts.push((implementation_input, literals));
    }
    let reference_bits = reference_outputs
        .iter()
        .map(|&value| reference_encoder.value(value))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let (solver, reference_values, clauses) = reference_encoder.into_solver();
    let mut implementation_encoder = CnfEncoder::with_solver(implementation, solver, clauses);
    for (value, literals) in implementation_cuts {
        implementation_encoder.bind_value(value, &literals)?;
    }
    let implementation_bits = implementation_outputs
        .iter()
        .map(|&value| implementation_encoder.value(value))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if reference_bits.len() != implementation_bits.len() {
        return Err(FormalError::invalid(format!(
            "module equivalence output width mismatch: reference={}, implementation={}",
            reference_bits.len(),
            implementation_bits.len()
        )));
    }
    let differences = reference_bits
        .into_iter()
        .zip(implementation_bits)
        .map(|(reference, implementation)| implementation_encoder.xor(reference, implementation))
        .collect::<Vec<_>>();
    implementation_encoder.clause(&differences);
    let outcome = finish_proof(implementation_encoder, "module boundary equivalence")?;
    Ok(match outcome {
        ProofOutcome::Proved(report) => ProofOutcome::Proved(ProofReport {
            encoded_values: report.encoded_values + reference_values,
            clauses: report.clauses,
        }),
        ProofOutcome::Disproved(counterexample) => ProofOutcome::Disproved(counterexample),
    })
}

/// Proves relational equality of two complete register updates.
///
/// Unassigned signals are shared between the two evaluations.
///
/// # Errors
///
/// Returns an error for duplicate assignments, width mismatch, unresolved
/// X/Z assignments, malformed IR, unsupported operations, representation
/// overflow, or SAT solver failure.
pub fn prove_register_equivalence_between_signal_assignments(
    module: &word::WordModule,
    reference: &word::RegisterOp,
    reference_state: word::SignalId,
    reference_assignments: &[(word::SignalId, opto_ir::ConstBits)],
    implementation: &word::RegisterOp,
    implementation_state: word::SignalId,
    implementation_assignments: &[(word::SignalId, opto_ir::ConstBits)],
) -> Result<ProofOutcome, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    encoder.bind_signals(reference_assignments)?;
    let reference = encoder.register_next(reference, reference_state)?;
    encoder.prepare_second_evaluation(reference_assignments, implementation_assignments);
    encoder.bind_signals(implementation_assignments)?;
    let implementation = encoder.register_next(implementation, implementation_state)?;
    prove_literal_equivalence(
        encoder,
        reference,
        implementation,
        "relational register equivalence",
    )
}

/// Proves that observations are identical for two values of one state signal.
///
/// # Errors
///
/// Returns an error for state width mismatch, unresolved X/Z states, malformed
/// IR, unsupported operations, representation overflow, or SAT solver failure.
pub fn prove_values_equivalent_between_signal_states(
    module: &word::WordModule,
    observations: &[word::ValueId],
    state: word::SignalId,
    left: &opto_ir::ConstBits,
    right: &opto_ir::ConstBits,
) -> Result<ProofOutcome, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    let left_assignment = [(state, left.clone())];
    let right_assignment = [(state, right.clone())];
    encoder.bind_signal(state, left)?;
    let left_bits = observations
        .iter()
        .map(|&value| encoder.value(value))
        .collect::<Result<Vec<_>, _>>()?;
    encoder.prepare_second_evaluation(&left_assignment, &right_assignment);
    encoder.bind_signal(state, right)?;
    let right_bits = observations
        .iter()
        .map(|&value| encoder.value(value))
        .collect::<Result<Vec<_>, _>>()?;
    prove_literal_equivalence(
        encoder,
        left_bits.into_iter().flatten().collect(),
        right_bits.into_iter().flatten().collect(),
        "FSM observation equivalence",
    )
}

fn prove_partition_equivalence(
    mut encoder: CnfEncoder<'_>,
    left: PartitionBits,
    right: PartitionBits,
) -> Result<ProofOutcome, FormalError> {
    if left.bits.len() != right.bits.len() {
        return Err(FormalError::invalid(
            "FSM partition encodings have different widths",
        ));
    }
    let mut differences = left
        .bits
        .into_iter()
        .zip(right.bits)
        .map(|(left, right)| encoder.xor(left, right))
        .collect::<Vec<_>>();
    differences.extend([!left.valid, !right.valid]);
    encoder.clause(&differences);
    finish_proof(encoder, "FSM partition refinement")
}

/// Proves that two complete register updates enter the same partition.
///
/// A transition outside `states` disproves the relation.
///
/// # Errors
///
/// Returns an error for malformed partitions, state width mismatch, unresolved
/// X/Z states, malformed IR, unsupported operations, representation overflow,
/// or SAT solver failure.
pub fn prove_partitioned_register_successor_equivalence(
    module: &word::WordModule,
    register: &word::RegisterOp,
    state: word::SignalId,
    left: &opto_ir::ConstBits,
    right: &opto_ir::ConstBits,
    states: &[opto_ir::ConstBits],
    classes: &[usize],
) -> Result<ProofOutcome, FormalError> {
    if states.is_empty() || states.len() != classes.len() {
        return Err(FormalError::invalid(
            "FSM partition requires one class for every non-empty state",
        ));
    }
    let mut encoder = CnfEncoder::new(module);
    encoder.bind_signal(state, left)?;
    let left_next = encoder.register_next(register, state)?;
    let left_partition = encoder.partition(&left_next, states, classes)?;
    let left_assignment = [(state, left.clone())];
    let right_assignment = [(state, right.clone())];
    encoder.prepare_second_evaluation(&left_assignment, &right_assignment);
    encoder.bind_signal(state, right)?;
    let right_next = encoder.register_next(register, state)?;
    let right_partition = encoder.partition(&right_next, states, classes)?;
    prove_partition_equivalence(encoder, left_partition, right_partition)
}

/// Proves that a word value has one fully resolved constant value.
///
/// # Errors
///
/// Returns an error for width mismatch, malformed IR, unsupported operations,
/// representation overflow, or SAT solver failure.
pub fn prove_value_constant(
    module: &word::WordModule,
    value: word::ValueId,
    constant: &opto_ir::ConstBits,
) -> Result<ProofOutcome, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    let value = encoder.value(value)?;
    let constant = constant_assignment(constant, value.len())?;
    let differences = value
        .into_iter()
        .zip(constant)
        .map(|(value, constant)| if constant { !value } else { value })
        .collect::<Vec<_>>();
    encoder.clause(&differences);
    let satisfiable = encoder
        .solver
        .solve()
        .map_err(|source| FormalError::Solver {
            context: "word constant proof",
            source,
        })?;
    if satisfiable {
        return Ok(ProofOutcome::disproved(
            "constant proof failed: SAT miter found a counterexample",
        ));
    }
    Ok(ProofOutcome::proved(ProofReport {
        encoded_values: encoder.encoded_values,
        clauses: encoder.clauses,
    }))
}

fn prove_literal_equivalence(
    mut encoder: CnfEncoder<'_>,
    reference: Vec<Lit>,
    implementation: Vec<Lit>,
    context: &'static str,
) -> Result<ProofOutcome, FormalError> {
    if reference.len() != implementation.len() {
        return Err(FormalError::invalid(format!(
            "equivalence proof width mismatch: reference={}, implementation={}",
            reference.len(),
            implementation.len()
        )));
    }
    let differences = reference
        .into_iter()
        .zip(implementation)
        .map(|(reference, implementation)| encoder.xor(reference, implementation))
        .collect::<Vec<_>>();
    encoder.clause(&differences);
    finish_proof(encoder, context)
}

fn finish_proof(
    mut encoder: CnfEncoder<'_>,
    context: &'static str,
) -> Result<ProofOutcome, FormalError> {
    let satisfiable = encoder
        .solver
        .solve()
        .map_err(|source| FormalError::Solver { context, source })?;
    if satisfiable {
        return Ok(ProofOutcome::disproved(
            "equivalence proof failed: SAT miter found a counterexample",
        ));
    }
    Ok(ProofOutcome::proved(ProofReport {
        encoded_values: encoder.encoded_values,
        clauses: encoder.clauses,
    }))
}

struct WordLogicEncoder<'a, 'model> {
    encoder: &'a mut CnfEncoder<'model>,
    boundary: &'a [Lit],
    constant_false: Option<Lit>,
}

impl LogicEncoding for WordLogicEncoder<'_, '_> {
    fn constant_false(&mut self) -> Lit {
        *self
            .constant_false
            .get_or_insert_with(|| self.encoder.constant(false))
    }

    fn input(&mut self, origin: u32) -> Result<Lit, FormalError> {
        let input = usize::try_from(origin)
            .map_err(|_| FormalError::capacity("logic input origin exceeds host capacity"))?;
        self.boundary.get(input).copied().ok_or_else(|| {
            FormalError::invalid(format!(
                "logic input origin {origin} is outside its proof boundary"
            ))
        })
    }

    fn and(&mut self, left: Lit, right: Lit) -> Lit {
        self.encoder.and(left, right)
    }

    fn xor(&mut self, left: Lit, right: Lit) -> Lit {
        self.encoder.xor(left, right)
    }

    fn mux(&mut self, select: Lit, then_value: Lit, else_value: Lit) -> Lit {
        self.encoder.select(select, then_value, else_value)
    }
}

fn prove_value_bits_with_cut(
    module: &word::WordModule,
    reference: word::ValueId,
    implementation: &[word::ValueId],
    cuts: &[(word::ValueId, Vec<word::ValueId>)],
    prefix: bool,
) -> Result<ProofOutcome, FormalError> {
    let mut encoder = CnfEncoder::new(module);
    for (value, bits) in cuts {
        encoder.cut_value(*value, bits)?;
    }
    let mut reference_bits = encoder.value(reference)?;
    let widths_match = if prefix {
        implementation.len() <= reference_bits.len()
    } else {
        implementation.len() == reference_bits.len()
    };
    if !widths_match {
        return Err(FormalError::invalid(format!(
            "equivalence proof width mismatch: reference={}, implementation={}",
            reference_bits.len(),
            implementation.len()
        )));
    }
    reference_bits.truncate(implementation.len());
    let implementation_bits = implementation
        .iter()
        .map(|&value| {
            let bits = encoder.value(value)?;
            let [bit]: [Lit; 1] = bits.try_into().map_err(|bits: Vec<_>| {
                FormalError::invalid(format!(
                    "equivalence proof expected a scalar implementation value, got {} bits",
                    bits.len()
                ))
            })?;
            Ok(bit)
        })
        .collect::<Result<Vec<_>, FormalError>>()?;
    let differences = reference_bits
        .into_iter()
        .zip(implementation_bits)
        .map(|(reference, implementation)| encoder.xor(reference, implementation))
        .collect::<Vec<_>>();
    encoder.solver.add_clause(&differences);
    let satisfiable = encoder
        .solver
        .solve()
        .map_err(|source| FormalError::Solver {
            context: "equivalence",
            source,
        })?;
    if satisfiable {
        return Ok(ProofOutcome::disproved(
            "equivalence proof failed: SAT miter found a counterexample",
        ));
    }
    Ok(ProofOutcome::proved(ProofReport {
        encoded_values: encoder.encoded_values,
        clauses: encoder.clauses,
    }))
}
struct CnfEncoder<'model> {
    module: &'model word::WordModule,
    solver: Solver<'static>,
    values: Vec<Option<Vec<Lit>>>,
    signals: BTreeMap<(word::SignalId, u32), Lit>,
    encoded_values: usize,
    clauses: usize,
}

struct PartitionBits {
    bits: Vec<Lit>,
    valid: Lit,
}

impl<'model> CnfEncoder<'model> {
    fn new(module: &'model word::WordModule) -> Self {
        Self {
            module,
            solver: Solver::new(),
            values: vec![None; module.values().len()],
            signals: BTreeMap::new(),
            encoded_values: 0,
            clauses: 0,
        }
    }

    fn with_solver(
        module: &'model word::WordModule,
        solver: Solver<'static>,
        clauses: usize,
    ) -> Self {
        Self {
            module,
            solver,
            values: vec![None; module.values().len()],
            signals: BTreeMap::new(),
            encoded_values: 0,
            clauses,
        }
    }

    fn into_solver(self) -> (Solver<'static>, usize, usize) {
        (self.solver, self.encoded_values, self.clauses)
    }

    fn value_width(&self, value: word::ValueId) -> Result<usize, FormalError> {
        self.module
            .value(value)
            .map(|value| value.ty.width() as usize)
            .ok_or_else(|| {
                FormalError::invalid(format!(
                    "module equivalence references unknown reference value {value:?}"
                ))
            })
    }

    fn bind_value(&mut self, value: word::ValueId, literals: &[Lit]) -> Result<(), FormalError> {
        let width = self.value_width(value)?;
        if width != literals.len() {
            return Err(FormalError::invalid(format!(
                "module equivalence binding width mismatch: value={width}, binding={}",
                literals.len()
            )));
        }
        let slot = self.values.get_mut(value.index()).ok_or_else(|| {
            FormalError::invalid(format!(
                "module equivalence has no cache slot for value {value:?}"
            ))
        })?;
        match slot {
            Some(existing) if existing == literals => Ok(()),
            Some(_) => Err(FormalError::invalid(format!(
                "module equivalence value {value:?} was bound more than once"
            ))),
            None => {
                *slot = Some(literals.to_vec());
                self.encoded_values += 1;
                Ok(())
            }
        }
    }

    fn bind_signals(
        &mut self,
        assignments: &[(word::SignalId, opto_ir::ConstBits)],
    ) -> Result<(), FormalError> {
        let mut assigned = BTreeSet::new();
        for (signal, value) in assignments {
            if !assigned.insert(*signal) {
                return Err(FormalError::invalid(format!(
                    "relational proof assigns signal {signal:?} more than once"
                )));
            }
            self.bind_signal(*signal, value)?;
        }
        Ok(())
    }

    fn bind_signal(
        &mut self,
        signal: word::SignalId,
        value: &opto_ir::ConstBits,
    ) -> Result<(), FormalError> {
        let width = self.signal_width(signal)?;
        let assignment = constant_assignment(value, width)?;
        self.signals.retain(|(bound, _), _| *bound != signal);
        for (bit, value) in assignment.into_iter().enumerate() {
            let bit = u32::try_from(bit)
                .map_err(|_| FormalError::capacity("relational proof signal index overflow"))?;
            let literal = self.constant(value);
            self.signals.insert((signal, bit), literal);
        }
        Ok(())
    }

    fn signal_width(&self, signal: word::SignalId) -> Result<usize, FormalError> {
        let width = self
            .module
            .signal(signal)
            .ok_or_else(|| {
                FormalError::invalid(format!(
                    "relational proof references unknown signal {signal:?}"
                ))
            })?
            .ty
            .width();
        usize::try_from(width)
            .map_err(|_| FormalError::capacity("relational proof signal width overflow"))
    }

    fn signal_bits(&mut self, signal: word::SignalId) -> Result<Vec<Lit>, FormalError> {
        let width = self.signal_width(signal)?;
        (0..width)
            .map(|bit| {
                let bit = u32::try_from(bit)
                    .map_err(|_| FormalError::capacity("relational proof signal index overflow"))?;
                Ok(self.signal(signal, bit))
            })
            .collect()
    }

    fn register_next(
        &mut self,
        register: &word::RegisterOp,
        state: word::SignalId,
    ) -> Result<Vec<Lit>, FormalError> {
        let held = self.signal_bits(state)?;
        let mut next = self.value(register.d)?;
        if held.len() != next.len() {
            return Err(FormalError::invalid(format!(
                "register update width mismatch: state={}, data={}",
                held.len(),
                next.len()
            )));
        }
        if let Some(enable) = register.enable {
            let active = self.control(enable.value, enable.active_high)?;
            next = next
                .into_iter()
                .zip(&held)
                .map(|(next, &held)| self.select(active, next, held))
                .collect();
        }
        for reset in register.resets.iter().rev() {
            let active = self.control(reset.value, reset.active_high)?;
            let reset_value = self.value(reset.reset_value)?;
            if reset_value.len() != next.len() {
                return Err(FormalError::invalid(format!(
                    "register reset width mismatch: state={}, reset={}",
                    next.len(),
                    reset_value.len()
                )));
            }
            next = reset_value
                .into_iter()
                .zip(next)
                .map(|(reset, next)| self.select(active, reset, next))
                .collect();
        }
        Ok(next)
    }

    fn control(&mut self, value: word::ValueId, active_high: bool) -> Result<Lit, FormalError> {
        let bits = self.value(value)?;
        let [value]: [Lit; 1] = bits.try_into().map_err(|bits: Vec<_>| {
            FormalError::invalid(format!(
                "register control has {} bits, expected one",
                bits.len()
            ))
        })?;
        Ok(if active_high { value } else { !value })
    }

    fn prepare_second_evaluation(
        &mut self,
        left: &[(word::SignalId, opto_ir::ConstBits)],
        right: &[(word::SignalId, opto_ir::ConstBits)],
    ) {
        let assigned = left
            .iter()
            .chain(right)
            .map(|(signal, _)| *signal)
            .collect::<BTreeSet<_>>();
        self.signals
            .retain(|(signal, _), _| !assigned.contains(signal));
        self.values.fill(None);
    }

    fn partition(
        &mut self,
        value: &[Lit],
        states: &[opto_ir::ConstBits],
        classes: &[usize],
    ) -> Result<PartitionBits, FormalError> {
        if states.len() != classes.len() {
            return Err(FormalError::invalid(
                "FSM partition state and class counts differ",
            ));
        }
        let class_count = classes
            .iter()
            .copied()
            .max()
            .and_then(|maximum| maximum.checked_add(1))
            .ok_or_else(|| FormalError::invalid("FSM partition is empty or too large"))?;
        let class_ids = classes.iter().copied().collect::<BTreeSet<_>>();
        if class_ids.len() != class_count {
            return Err(FormalError::invalid(
                "FSM partition class IDs must be dense",
            ));
        }
        for (index, state) in states.iter().enumerate() {
            if states[..index].contains(state) {
                return Err(FormalError::invalid(
                    "FSM partition contains a duplicate state",
                ));
            }
        }
        let width = (usize::BITS - class_count.saturating_sub(1).leading_zeros()).max(1);
        let width = usize::try_from(width)
            .map_err(|_| FormalError::capacity("FSM partition code width overflow"))?;
        let mut bits = (0..width)
            .map(|bit| self.constant(((classes[0] >> bit) & 1) != 0))
            .collect::<Vec<_>>();
        let mut matches = Vec::with_capacity(states.len());
        for (state, &class) in states.iter().zip(classes) {
            let assignment = constant_assignment(state, value.len())?;
            let equal = value
                .iter()
                .copied()
                .zip(assignment)
                .map(|(literal, expected)| if expected { literal } else { !literal })
                .reduce(|left, right| self.and(left, right))
                .unwrap_or_else(|| self.constant(true));
            matches.push(equal);
            for (bit, output) in bits.iter_mut().enumerate() {
                let class_bit = self.constant(((class >> bit) & 1) != 0);
                *output = self.select(equal, class_bit, *output);
            }
        }
        Ok(PartitionBits {
            bits,
            valid: self.reduce_or(&matches),
        })
    }

    /// Seed `value` as the concatenation of `bits`, where every non-constant
    /// bit becomes a fresh unconstrained variable. Both the word-level
    /// reference and the bit-level implementation then share the same operand
    /// variables, and encoding stops at the cut instead of walking the cone.
    fn cut_value(
        &mut self,
        value: word::ValueId,
        bits: &[word::ValueId],
    ) -> Result<(), FormalError> {
        let width = self
            .module
            .value(value)
            .ok_or_else(|| {
                FormalError::invalid(format!(
                    "equivalence proof references unknown value {value:?}"
                ))
            })?
            .ty
            .width();
        if width as usize != bits.len() {
            return Err(FormalError::invalid(format!(
                "equivalence proof cut width mismatch: value has {width} bits, cut supplies {}",
                bits.len()
            )));
        }
        let mut word_bits = Vec::with_capacity(bits.len());
        for &bit in bits {
            let already_encoded = self.values.get(bit.index()).is_some_and(Option::is_some);
            let constant = matches!(
                self.module.value(bit).map(|value| &value.kind),
                Some(word::ValueKind::Constant(_))
            );
            let lits = if already_encoded || constant {
                self.value(bit)?
            } else {
                let lit = self.solver.new_lit();
                let slot = self.values.get_mut(bit.index()).ok_or_else(|| {
                    FormalError::invalid(format!(
                        "equivalence proof has no cache slot for value {bit:?}"
                    ))
                })?;
                *slot = Some(vec![lit]);
                self.encoded_values += 1;
                vec![lit]
            };
            let [lit] = lits[..] else {
                return Err(FormalError::invalid(format!(
                    "equivalence proof cut bit {bit:?} is not a scalar value"
                )));
            };
            word_bits.push(lit);
        }
        let slot = self.values.get_mut(value.index()).ok_or_else(|| {
            FormalError::invalid(format!(
                "equivalence proof has no cache slot for value {value:?}"
            ))
        })?;
        match slot {
            Some(existing) if *existing == word_bits => {}
            Some(_) => {
                return Err(FormalError::invalid(format!(
                    "equivalence proof cut value {value:?} was already encoded differently"
                )));
            }
            None => {
                *slot = Some(word_bits);
                self.encoded_values += 1;
            }
        }
        Ok(())
    }
}

mod encoding;
