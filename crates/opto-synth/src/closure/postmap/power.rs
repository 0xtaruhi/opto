// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transactional multi-scenario power evaluation over edited timing topology.

use opto_timing::{AnalysisViewId, IncrementalTiming, ScenarioSet};

#[derive(Debug, Clone, Copy)]
pub(super) struct PowerProposal {
    dynamic_watts: Option<f64>,
}

impl PowerProposal {
    pub(super) const fn unmeasured() -> Self {
        Self {
            dynamic_watts: None,
        }
    }

    pub(super) const fn dynamic_watts(self) -> Option<f64> {
        self.dynamic_watts
    }
}

pub(super) struct MmmcPower {
    scenarios: ScenarioSet,
    view_ids: Box<[AnalysisViewId]>,
    power_owner_ids: Box<[Option<AnalysisViewId>]>,
    runtime: opto_runtime::ExecutionContext,
    evaluator: std::sync::Arc<dyn crate::SynthesisPowerEvaluator>,
    committed: PowerProposal,
}

impl MmmcPower {
    pub(super) fn new(
        timing: &crate::closure::mmmc::MmmcTiming,
        scenarios: &ScenarioSet,
        runtime: &opto_runtime::ExecutionContext,
        evaluator: std::sync::Arc<dyn crate::SynthesisPowerEvaluator>,
    ) -> Result<Self, crate::SynthError> {
        let mut owner = Self {
            scenarios: scenarios.clone(),
            view_ids: timing.view_ids().collect(),
            power_owner_ids: timing.power_owner_ids().collect(),
            runtime: runtime.clone(),
            evaluator,
            committed: PowerProposal {
                dynamic_watts: None,
            },
        };
        owner.committed = owner.evaluate(timing.owners())?;
        Ok(owner)
    }

    pub(super) const fn committed(&self) -> PowerProposal {
        self.committed
    }

    pub(super) fn evaluate(
        &self,
        timing: &[IncrementalTiming],
    ) -> Result<PowerProposal, crate::SynthError> {
        if self.power_owner_ids.len() != self.scenarios.scenarios().len()
            || self.view_ids.len() != timing.len()
        {
            return Err(crate::SynthError::invariant(
                "MMMC power scenarios and topology owners are misaligned",
            ));
        }
        let mut dynamic = Vec::with_capacity(self.power_owner_ids.len());
        for (&view, scenario) in self.power_owner_ids.iter().zip(self.scenarios.scenarios()) {
            let Some(view) = view else {
                return Ok(PowerProposal::unmeasured());
            };
            let owner = self.view_ids.binary_search(&view).map_err(|_| {
                crate::SynthError::invariant("MMMC power references an unavailable analysis view")
            })?;
            let timing = timing.get(owner).ok_or_else(|| {
                crate::SynthError::invariant("MMMC power view metadata and owners are misaligned")
            })?;
            let dynamic_watts = self
                .evaluator
                .dynamic_power_watts(&self.runtime, scenario, timing.model(), &|| {
                    timing
                        .electrical_snapshot()
                        .map_err(|error| error.to_string())
                })
                .map_err(crate::SynthError::Power)?;
            let dynamic_watts = crate::closure::validated_dynamic_power(
                dynamic_watts,
                "post-map power evaluation",
            )?;
            let Some(dynamic_watts) = dynamic_watts else {
                return Ok(PowerProposal {
                    dynamic_watts: None,
                });
            };
            dynamic.push(dynamic_watts);
        }
        Ok(PowerProposal {
            dynamic_watts: dynamic.into_iter().max_by(f64::total_cmp),
        })
    }

    pub(super) fn commit(&mut self, proposal: PowerProposal) {
        self.committed = proposal;
    }
}
