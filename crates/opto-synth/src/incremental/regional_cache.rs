// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::regional::{
    BoundaryRepairArtifactRecord, RegionContextKey, RegionCoverPlanRecord,
    RegionalSharedAllocations,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegionalCacheRecord {
    context: RegionContextKey,
    memory_implementations: Arc<[u8]>,
    plan: Option<RegionCoverPlanRecord>,
    boundary_repairs: Arc<[BoundaryRepairArtifactRecord]>,
}

impl RegionalCacheRecord {
    pub(crate) fn new(context: RegionContextKey, memory_implementations: &[u8]) -> Self {
        Self {
            context,
            memory_implementations: memory_implementations.into(),
            plan: None,
            boundary_repairs: Arc::from([]),
        }
    }

    pub(crate) fn set_plan(&mut self, plan: RegionCoverPlanRecord) {
        self.plan = Some(plan);
    }

    pub(crate) fn clear_plan(&mut self) {
        self.plan = None;
    }

    pub(crate) fn clear_boundary_repairs(&mut self) {
        self.boundary_repairs = Arc::from([]);
    }

    pub(crate) fn set_boundary_repairs(
        &mut self,
        mut repairs: Vec<BoundaryRepairArtifactRecord>,
    ) -> Result<(), crate::SynthError> {
        repairs.sort_unstable_by_key(BoundaryRepairArtifactRecord::semantic_identity);
        if repairs
            .iter()
            .any(|repair| repair.driver_context() != self.context)
            || repairs.windows(2).any(|pair| {
                pair[0].semantic_identity() >= pair[1].semantic_identity()
                    || (pair[0].driver(), pair[0].sink()) == (pair[1].driver(), pair[1].sink())
            })
        {
            return Err(crate::SynthError::invariant(
                "regional cache boundary repairs do not belong to unique driver edges",
            ));
        }
        self.boundary_repairs = repairs.into();
        Ok(())
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

    pub(crate) const fn plan(&self) -> Option<&RegionCoverPlanRecord> {
        self.plan.as_ref()
    }

    pub(crate) fn boundary_repairs(&self) -> &[BoundaryRepairArtifactRecord] {
        &self.boundary_repairs
    }

    pub(crate) fn with_context(&self, context: RegionContextKey) -> Self {
        Self {
            context,
            memory_implementations: self.memory_implementations.clone(),
            plan: None,
            boundary_repairs: Arc::from([]),
        }
    }

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
        for repair in self.boundary_repairs.iter() {
            repair.validate()?;
            if repair.driver_context() != self.context {
                return Err(crate::SynthError::invariant(
                    "regional cache boundary repair belongs to another driver context",
                ));
            }
        }
        if self.boundary_repairs.windows(2).any(|pair| {
            pair[0].semantic_identity() >= pair[1].semantic_identity()
                || (pair[0].driver(), pair[0].sink()) == (pair[1].driver(), pair[1].sink())
        }) {
            return Err(crate::SynthError::invariant(
                "regional cache boundary repairs are not strictly edge ordered",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_all(records: &[Self]) -> Result<(), crate::SynthError> {
        let mut previous = None;
        // Each record binds every repair to its own context and requires repair
        // identities to be strictly ordered. Contexts are strictly unique here,
        // so one repair cannot occur in two different records.
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
            .saturating_add(shared.charge(&self.boundary_repairs, || {
                self.boundary_repairs
                    .iter()
                    .map(BoundaryRepairArtifactRecord::owned_memory_bytes)
                    .fold(0usize, usize::saturating_add)
            }))
    }
}
