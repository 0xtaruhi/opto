// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Inversion boundary between synthesis policy and the power-analysis owner.

/// Immutable power service supplied by the session coordinator.
///
/// Synthesis owns candidate selection, while `opto-power` owns activity
/// propagation and Liberty power evaluation. This interface lets one candidate
/// transaction request a measurement without introducing an engine-to-engine
/// crate dependency.
pub trait SynthesisPowerEvaluator: Send + Sync {
    /// Returns dynamic power for one scenario and edited timing topology.
    ///
    /// `None` means that the scenario lacks complete reliable activity or
    /// characterized power units. Implementations must not invent activity.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when the supplied timing, activity, or
    /// power generations are inconsistent or power evaluation fails.
    fn dynamic_power_watts(
        &self,
        runtime: &opto_runtime::ExecutionContext,
        scenario: &opto_timing::Scenario,
        model: &opto_timing::TimingModel,
        electrical: &opto_timing::TimingElectricalSnapshot,
    ) -> Result<Option<f64>, String>;
}

#[derive(Debug, Default)]
/// Explicit evaluator used by standalone callers that do not own a power
/// engine. Dynamic power remains absent from their objective.
pub struct NoPowerEvaluation;

impl SynthesisPowerEvaluator for NoPowerEvaluation {
    fn dynamic_power_watts(
        &self,
        _runtime: &opto_runtime::ExecutionContext,
        _scenario: &opto_timing::Scenario,
        _model: &opto_timing::TimingModel,
        _electrical: &opto_timing::TimingElectricalSnapshot,
    ) -> Result<Option<f64>, String> {
        Ok(None)
    }
}
