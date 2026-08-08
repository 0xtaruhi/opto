// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Initial regional-plan restoration and selective target covering.

use super::{RegionalIr, RegionalMapper, RegionalMappingSeed, RegionalPlans, SynthesisProgress};

impl RegionalMapper<'_> {
    /// Establishes the plans and bindings that the first epoch commits.
    pub(super) fn seed_plans(
        &self,
        _ir: &RegionalIr<'_>,
        state: &mut RegionalPlans,
        seed: &RegionalMappingSeed,
        _observer: &mut dyn FnMut(SynthesisProgress),
    ) -> Result<(), crate::SynthError> {
        let rows = self.regions.regions().len();
        let RegionalMappingSeed::Private { plans, bindings } = seed;
        if plans.len() != rows || bindings.len() != rows {
            return Err(crate::SynthError::invariant(
                "private regional topology does not align with the region graph",
            ));
        }
        state.plans = plans.to_vec();
        state.bindings = bindings.to_vec();
        Ok(())
    }
}
