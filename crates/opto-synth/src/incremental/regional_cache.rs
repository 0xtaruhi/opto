// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::regional::{
    BoundaryContract, RegionContextKey, RegionCoverPlanRecord, RegionalSharedAllocations,
};
use crate::{RegionCoverPlan, SynthesisRegion};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Durable decision and optional plan for one exact regional context.
///
/// Memory decisions and topology payloads are shared immutable owners. Records
/// are valid for reuse only as a strictly context-sorted slice; a context may
/// never describe two different memory decisions.
pub(crate) struct RegionalCacheRecord {
    context: RegionContextKey,
    memory_implementations: Arc<[u8]>,
    plan: Option<RegionCoverPlanRecord>,
}

impl RegionalCacheRecord {
    pub(crate) fn new(context: RegionContextKey, memory_implementations: &[u8]) -> Self {
        Self {
            context,
            memory_implementations: memory_implementations.into(),
            plan: None,
        }
    }

    pub(crate) fn set_plan(&mut self, plan: &RegionCoverPlan) {
        self.plan = Some(plan.checkpoint_record());
    }

    pub(crate) fn clear_plan(&mut self) {
        self.plan = None;
    }

    pub(crate) fn plan_region(&self) -> Option<crate::RegionAnchorId> {
        self.plan.as_ref().map(RegionCoverPlanRecord::region)
    }

    pub(crate) const fn context(&self) -> RegionContextKey {
        self.context
    }

    pub(crate) fn memory_implementations(&self) -> &[u8] {
        &self.memory_implementations
    }

    /// Reconstructs the cached plan at the persistence boundary.
    ///
    /// The portable record remains private to the cache. Mapping code receives
    /// only a live plan whose region, revision, context, and contracts have all
    /// been validated against the current generation.
    pub(crate) fn restore_plan(
        &self,
        region: SynthesisRegion,
        contracts: &[BoundaryContract],
    ) -> Result<Option<RegionCoverPlan>, crate::SynthError> {
        self.plan
            .as_ref()
            .map(|plan| plan.restore(region, self.context, contracts))
            .transpose()
    }

    /// Reuses the immutable decision payload for a new measured context.
    ///
    /// The plan is cleared because its contracts and topology remain bound to
    /// the old context until independently reconstructed and validated.
    pub(crate) fn with_context(&self, context: RegionContextKey) -> Self {
        Self {
            context,
            memory_implementations: self.memory_implementations.clone(),
            plan: None,
        }
    }

    /// Rejects conflicting decisions assigned to the same context key.
    pub(crate) fn validate_same_decision(&self, other: &Self) -> Result<(), crate::SynthError> {
        if self.context != other.context
            || self.memory_implementations != other.memory_implementations
        {
            return Err(crate::SynthError::invariant(
                "one regional context describes different decisions",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), crate::SynthError> {
        if !self.memory_implementations.len().is_multiple_of(4) {
            return Err(crate::SynthError::invariant(
                "regional cache memory implementation payload is not 32-bit aligned",
            ));
        }
        if let Some(plan) = &self.plan {
            plan.validate(self.context)?;
        }
        Ok(())
    }

    /// Validates payloads and the canonical strict context order used by lookup.
    pub(crate) fn validate_all(records: &[Self]) -> Result<(), crate::SynthError> {
        let mut previous = None;
        for record in records {
            record.validate()?;
            if previous.is_some_and(|context| context >= record.context) {
                return Err(crate::SynthError::invariant(
                    "regional cache records are not strictly ordered by context",
                ));
            }
            previous = Some(record.context);
        }
        Ok(())
    }

    pub(crate) fn owned_memory_bytes(records: &[Self]) -> usize {
        let mut shared = RegionalSharedAllocations::default();
        records
            .iter()
            .map(|record| record.record_memory_bytes(&mut shared))
            .fold(0usize, usize::saturating_add)
    }

    fn record_memory_bytes(&self, shared: &mut RegionalSharedAllocations) -> usize {
        shared
            .charge(&self.memory_implementations, || 0)
            .saturating_add(
                self.plan
                    .as_ref()
                    .map_or(0, |plan| plan.owned_memory_bytes(shared)),
            )
    }
}
