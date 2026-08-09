// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Human-readable evidence that an equivalence claim was disproved.
///
/// The current proof API records a diagnostic summary rather than a complete
/// signal assignment.
pub struct Counterexample {
    description: String,
}

impl Counterexample {
    pub(crate) fn new(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
        }
    }

    #[must_use]
    /// Returns the human-readable counterexample summary.
    pub fn description(&self) -> &str {
        &self.description
    }
}

impl fmt::Display for Counterexample {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.description)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// The semantic result of an equivalence proof.
pub enum ProofOutcome {
    /// The miter was unsatisfiable and the implementations are equivalent.
    Proved(super::ProofReport),
    /// The miter was satisfiable and contains a counterexample.
    Disproved(Counterexample),
}

impl ProofOutcome {
    #[must_use]
    /// Creates a successful proof outcome.
    pub fn proved(report: super::ProofReport) -> Self {
        Self::Proved(report)
    }

    #[must_use]
    /// Creates a disproved outcome from a diagnostic summary.
    pub fn disproved(description: impl Into<String>) -> Self {
        Self::Disproved(Counterexample::new(description))
    }

    /// Extracts the proof report, returning the counterexample on failure.
    ///
    /// # Errors
    ///
    /// Returns the preserved [`Counterexample`] when the proof outcome is
    /// disproved.
    pub fn require_proved(self) -> Result<super::ProofReport, Counterexample> {
        match self {
            Self::Proved(report) => Ok(report),
            Self::Disproved(counterexample) => Err(counterexample),
        }
    }
}
