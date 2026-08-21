// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! SAT-backed equivalence checks for Opto intermediate representations.
//!
//! The public entry points compare logic networks or prove selected word-IR
//! values against mapped logic at an explicit cut. Proofs distinguish a
//! successful equivalence result from a concrete [`Counterexample`]; malformed
//! input and solver construction failures are reported as [`FormalError`].
//!
//! Cuts are part of the contract rather than an optimization detail. Signals on
//! the cut become shared symbolic inputs, so callers can prove a rewritten
//! region without encoding unrelated upstream logic.

mod error;
mod logic;
mod outcome;
mod word_miter;

pub use error::FormalError;
pub use logic::{
    BoundaryRefutation, prove_logic_literal_partitions, prove_logic_network_equivalence,
};
pub use outcome::{Counterexample, ProofOutcome};
pub use word_miter::{
    FiniteTransitionRelation, enumerate_finite_signal_register_transitions,
    enumerate_finite_signal_transitions, enumerate_finite_transitions,
    prove_module_values_equivalent_at_cut, prove_partitioned_register_successor_equivalence,
    prove_register_equivalence_between_signal_assignments, prove_value_against_logic_at_cut,
    prove_value_bits, prove_value_bits_at_cut, prove_value_constant,
    prove_value_equivalence_under_assumptions, prove_value_prefix_at_cut,
    prove_values_equivalent_between_signal_states,
};

/// Size metrics for the SAT instance constructed by a proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProofReport {
    /// Number of IR values assigned SAT literals.
    pub encoded_values: usize,
    /// Number of clauses emitted to the incremental solver.
    pub clauses: usize,
}

#[cfg(test)]
mod tests;
