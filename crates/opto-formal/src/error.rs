// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use thiserror::Error;

#[derive(Debug, Error)]
/// Failure to construct or solve a formal-equivalence problem.
pub enum FormalError {
    /// The supplied IR violates a precondition required by the proof model.
    #[error("{0}")]
    InvalidModel(String),
    /// The model contains an operation not supported by the encoder.
    #[error("{0}")]
    Unsupported(String),
    /// The proof representation exceeds an addressable capacity.
    #[error("{0}")]
    Capacity(String),
    /// The SAT solver failed while processing a specific proof context.
    #[error("{context} SAT solver failed: {source}")]
    Solver {
        /// A static description of the proof operation.
        context: &'static str,
        /// The underlying SAT solver failure.
        #[source]
        source: varisat::solver::SolverError,
    },
    /// The word IR could not be queried or validated.
    #[error("equivalence proof: {0}")]
    Word(#[source] opto_ir::word::WordError),
}

impl FormalError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidModel(message.into())
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::Unsupported(message.into())
    }

    pub(crate) fn capacity(message: impl Into<String>) -> Self {
        Self::Capacity(message.into())
    }
}
