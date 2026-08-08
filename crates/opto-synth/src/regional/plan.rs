// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{BoundaryContract, EarlyLate, FiniteValue, RegionContextKey, RiseFall};
use crate::{RegionAnchorId, RegionRevision};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::mem::{size_of, size_of_val};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Measured electrical behavior for one scenario/tag at a hard boundary.
pub struct BoundaryResponseRow {
    /// Analysis scenario in which the response was measured.
    pub scenario: opto_timing::ScenarioId,
    /// Interned timing-path semantics for the measured lane.
    pub timing_tag: super::TimingTagId,
    /// Measured arrival by corner and transition.
    pub arrival: EarlyLate<RiseFall<Option<FiniteValue>>>,
    /// Measured transition by corner and transition.
    pub transition: EarlyLate<RiseFall<Option<FiniteValue>>>,
    /// Measured input capacitance by corner.
    pub input_capacitance: EarlyLate<Option<FiniteValue>>,
    /// Measured switching activity when power analysis was available.
    pub activity: Option<opto_timing::ScenarioSwitchingActivity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Portable measured response keyed by the boundary's semantic identity.
pub struct BoundaryResponse {
    /// Content-derived identity of the boundary port, independent of dense IDs.
    pub port_semantic_key: [u8; 32],
    /// Sparse scenario/tag rows in deterministic order.
    pub rows: Box<[BoundaryResponseRow]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct RegionPlanBoundaryRecord {
    semantic_key: [u8; 32],
    generation: [u8; 32],
}

impl RegionPlanBoundaryRecord {
    fn from_contract(contract: &BoundaryContract) -> Self {
        Self {
            semantic_key: contract.port().semantic_key(),
            generation: contract.generation().bytes(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Ordered legality, closure, `QoR`, and stable tie-break values for one plan.
pub struct RegionPlanCost {
    /// Whether every timing and electrical constraint is satisfied.
    pub legal: bool,
    /// Largest dimensionless constraint violation; zero for a legal plan.
    pub worst_normalized_violation: FiniteValue,
    /// Worst slack across all analyzed scenarios and checks.
    pub minimum_slack: FiniteValue,
    /// Sum of negative timing slack across violating paths.
    pub total_negative_slack: FiniteValue,
    /// Total target-library cell area.
    pub area: FiniteValue,
    /// Leakage power when a power evaluator was supplied.
    pub leakage_power: Option<FiniteValue>,
    /// Dynamic power when a power evaluator was supplied.
    pub dynamic_power: Option<FiniteValue>,
    /// Number of mapped cells in the region-local implementation.
    pub cell_count: u32,
    /// Deterministic final tie-break key after all numeric objectives.
    pub stable_plan_key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// One exact target-cell identity retained by a selected regional plan.
///
/// This is the sorted census of cells encoded by the plan payload. Global
/// sequential and infrastructure artifacts are deliberately not duplicated in
/// every regional plan.
pub(crate) struct RegionImplementationCell {
    pub(crate) cell_name: Box<str>,
    pub(crate) pin_count: u32,
}

#[derive(Debug, Clone)]
/// Compact region-local solution retained after candidate builders are freed.
pub struct RegionCoverPlan {
    region: RegionAnchorId,
    revision: RegionRevision,
    context_key: RegionContextKey,
    cost: RegionPlanCost,
    local_net_count: u32,
    local_cell_count: u32,
    local_pin_count: u32,
    boundary_response: Arc<[BoundaryContract]>,
    measured_response: Arc<[BoundaryResponse]>,
    implementation_cells: Arc<[RegionImplementationCell]>,
    payload: Arc<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Stable semantic and context identity carried by one regional plan.
pub struct RegionPlanIdentity {
    /// Content-anchored identity of the synthesized region.
    pub region: RegionAnchorId,
    /// Region semantics excluding external timing context.
    pub revision: RegionRevision,
    /// Complete target, scenario, contract, and predecessor context.
    pub context_key: RegionContextKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Exact artifact-local topology counts encoded by one regional plan.
pub struct RegionPlanSize {
    /// Number of nets encoded exclusively inside the region payload.
    pub local_net_count: u32,
    /// Number of cells encoded exclusively inside the region payload.
    pub local_cell_count: u32,
    /// Number of cell pins encoded exclusively inside the region payload.
    pub local_pin_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Checkpoint-safe compact representation of one selected regional cover.
///
/// The record intentionally contains no revision-local Word or mapped IDs.
/// Its payload is interpreted only after the canonical region identity and
/// complete context key have both been reconstructed and validated.
pub(crate) struct RegionCoverPlanRecord {
    region: RegionAnchorId,
    revision: RegionRevision,
    context_key: RegionContextKey,
    cost: RegionPlanCost,
    local_net_count: u32,
    local_cell_count: u32,
    local_pin_count: u32,
    boundaries: Arc<[RegionPlanBoundaryRecord]>,
    measured_response: Arc<[BoundaryResponse]>,
    implementation_cells: Arc<[RegionImplementationCell]>,
    payload: Arc<[u8]>,
}

#[derive(Debug, Default)]
pub(crate) struct RegionalSharedAllocations(BTreeSet<usize>);

impl RegionalSharedAllocations {
    /// Charge one shared Arc allocation and any allocations nested in its elements.
    ///
    /// The two reference-count words and the slice payload share one allocation,
    /// so allocator metadata and slack must be applied to their combined size.
    pub(crate) fn charge<T>(
        &mut self,
        values: &Arc<[T]>,
        nested_bytes: impl FnOnce() -> usize,
    ) -> usize {
        if self.0.insert(Arc::as_ptr(values).cast::<T>() as usize) {
            let arc_bytes = size_of::<usize>()
                .saturating_mul(2)
                .saturating_add(size_of_val(values.as_ref()));
            opto_core::resident::allocation_bytes(arc_bytes).saturating_add(nested_bytes())
        } else {
            0
        }
    }
}

impl RegionCoverPlan {
    /// Seal a compact plan before optional measured responses are attached.
    #[must_use]
    pub fn new(
        identity: RegionPlanIdentity,
        cost: RegionPlanCost,
        size: RegionPlanSize,
        boundary_response: Vec<BoundaryContract>,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            region: identity.region,
            revision: identity.revision,
            context_key: identity.context_key,
            cost,
            local_net_count: size.local_net_count,
            local_cell_count: size.local_cell_count,
            local_pin_count: size.local_pin_count,
            boundary_response: boundary_response.into(),
            measured_response: Arc::from([]),
            implementation_cells: Arc::from([]),
            payload: payload.into(),
        }
    }

    pub(crate) fn with_measured_response(mut self, mut response: Vec<BoundaryResponse>) -> Self {
        response.sort_unstable_by_key(|response| response.port_semantic_key);
        self.measured_response = response.into();
        self
    }

    pub(crate) fn with_cost(mut self, cost: RegionPlanCost) -> Self {
        self.cost = cost;
        self
    }

    pub(crate) fn with_context_and_contracts(
        mut self,
        context: RegionContextKey,
        contracts: Vec<BoundaryContract>,
    ) -> Self {
        self.context_key = context;
        self.boundary_response = contracts.into();
        self
    }

    pub(crate) fn with_implementation_cells(
        mut self,
        cells: Vec<RegionImplementationCell>,
    ) -> Self {
        self.implementation_cells = cells.into();
        self
    }

    #[must_use]
    /// Return the content-anchored region identity.
    pub const fn region(&self) -> RegionAnchorId {
        self.region
    }

    #[must_use]
    /// Return the context-independent region semantics key.
    pub const fn revision(&self) -> RegionRevision {
        self.revision
    }

    #[must_use]
    /// Return the complete context key under which this plan is reusable.
    pub const fn context_key(&self) -> RegionContextKey {
        self.context_key
    }

    #[must_use]
    /// Return the ordered legality and optimization objectives.
    pub const fn cost(&self) -> RegionPlanCost {
        self.cost
    }

    #[must_use]
    /// Return the region-local net count encoded by the payload.
    pub const fn local_net_count(&self) -> u32 {
        self.local_net_count
    }

    #[must_use]
    /// Return the region-local cell count encoded by the payload.
    pub const fn local_cell_count(&self) -> u32 {
        self.local_cell_count
    }

    #[must_use]
    /// Return the region-local pin count encoded by the payload.
    pub const fn local_pin_count(&self) -> u32 {
        self.local_pin_count
    }

    #[must_use]
    /// Return the boundary contracts used while selecting the plan.
    pub fn boundary_response(&self) -> &[BoundaryContract] {
        &self.boundary_response
    }

    #[must_use]
    /// Return post-materialization electrical measurements by boundary port.
    pub fn measured_response(&self) -> &[BoundaryResponse] {
        &self.measured_response
    }

    pub(crate) fn implementation_cells(&self) -> &[RegionImplementationCell] {
        &self.implementation_cells
    }

    pub(crate) fn matches_materialized_topology(&self, current: &Self) -> bool {
        self.region == current.region
            && self.revision == current.revision
            && self.context_key == current.context_key
            && self.local_net_count == current.local_net_count
            && self.local_cell_count == current.local_cell_count
            && self.local_pin_count == current.local_pin_count
            && self.cost.area == current.cost.area
            && self.cost.leakage_power == current.cost.leakage_power
            && self.cost.dynamic_power == current.cost.dynamic_power
            && self.cost.cell_count == current.cost.cell_count
            && self.cost.stable_plan_key == current.cost.stable_plan_key
            && self.boundary_response == current.boundary_response
            && self.implementation_cells == current.implementation_cells
            && self.payload == current.payload
    }

    #[must_use]
    /// Return the opaque, versioned materialization payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub(crate) fn checkpoint_record(&self) -> RegionCoverPlanRecord {
        let mut boundaries = self
            .boundary_response
            .iter()
            .map(RegionPlanBoundaryRecord::from_contract)
            .collect::<Vec<_>>();
        boundaries.sort_unstable();
        RegionCoverPlanRecord {
            region: self.region,
            revision: self.revision,
            context_key: self.context_key,
            cost: self.cost,
            local_net_count: self.local_net_count,
            local_cell_count: self.local_cell_count,
            local_pin_count: self.local_pin_count,
            boundaries: boundaries.into(),
            measured_response: Arc::clone(&self.measured_response),
            implementation_cells: Arc::clone(&self.implementation_cells),
            payload: Arc::clone(&self.payload),
        }
    }
}

impl RegionCoverPlanRecord {
    pub(crate) const fn region(&self) -> RegionAnchorId {
        self.region
    }

    pub(crate) const fn context_key(&self) -> RegionContextKey {
        self.context_key
    }

    pub(crate) fn validate(&self, context: RegionContextKey) -> Result<(), crate::SynthError> {
        if self.context_key != context {
            return Err(crate::SynthError::invariant(
                "regional cover-plan record has a mismatched context key",
            ));
        }
        self.validate_boundary_responses()?;
        if self.cost.worst_normalized_violation.get() < 0.0
            || self.cost.total_negative_slack.get() < 0.0
            || self.cost.area.get() < 0.0
            || self
                .cost
                .leakage_power
                .is_some_and(|power| power.get() < 0.0)
            || self
                .cost
                .dynamic_power
                .is_some_and(|power| power.get() < 0.0)
        {
            return Err(crate::SynthError::invariant(
                "regional cover-plan record has a negative magnitude cost",
            ));
        }
        if self.implementation_cells.len() != self.cost.cell_count as usize {
            return Err(crate::SynthError::invariant(
                "regional cover-plan implementation cell count differs from its cost",
            ));
        }
        if self.local_cell_count != self.cost.cell_count {
            return Err(crate::SynthError::invariant(
                "regional cover-plan topology cell count differs from its cost",
            ));
        }
        if !self.implementation_cells.is_sorted() {
            return Err(crate::SynthError::invariant(
                "regional cover-plan implementation census is not canonical",
            ));
        }
        if self.local_cell_count != 0 && self.payload.is_empty() {
            return Err(crate::SynthError::invariant(
                "non-empty regional cover-plan record has no topology payload",
            ));
        }
        Ok(())
    }

    fn validate_boundary_responses(&self) -> Result<(), crate::SynthError> {
        if self
            .boundaries
            .windows(2)
            .any(|pair| pair[0].semantic_key >= pair[1].semantic_key)
        {
            return Err(crate::SynthError::invariant(
                "regional cover-plan boundaries are not strictly ordered by semantic key",
            ));
        }
        if self.measured_response.is_empty() {
            return Ok(());
        }
        if self.measured_response.len() != self.boundaries.len() {
            return Err(crate::SynthError::invariant(
                "regional cover-plan responses do not cover every boundary",
            ));
        }
        for (boundary, response) in self.boundaries.iter().zip(self.measured_response.iter()) {
            if response.port_semantic_key != boundary.semantic_key {
                return Err(crate::SynthError::invariant(
                    "regional cover-plan response does not match its boundary semantic key",
                ));
            }
            if response.rows.windows(2).any(|pair| {
                (pair[0].scenario, pair[0].timing_tag) >= (pair[1].scenario, pair[1].timing_tag)
            }) {
                return Err(crate::SynthError::invariant(
                    "regional cover-plan response rows are not strictly ordered by scenario and timing tag",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn restore(
        &self,
        region: crate::SynthesisRegion,
        context: RegionContextKey,
        contracts: &[BoundaryContract],
    ) -> Result<RegionCoverPlan, crate::SynthError> {
        self.validate(context)?;
        if self.region != region.id() || self.revision != region.revision() {
            return Err(crate::SynthError::invariant(
                "regional cover-plan identity failed reconstruction",
            ));
        }
        let mut current_contracts = contracts.iter().collect::<Vec<_>>();
        current_contracts.sort_unstable_by_key(|contract| contract.port().semantic_key());
        if !current_contracts
            .iter()
            .copied()
            .map(RegionPlanBoundaryRecord::from_contract)
            .eq(self.boundaries.iter().copied())
        {
            return Err(crate::SynthError::invariant(
                "regional cover-plan boundaries failed semantic reconstruction",
            ));
        }
        if !self.measured_response.is_empty() {
            for (contract, response) in current_contracts.iter().zip(self.measured_response.iter())
            {
                if response.rows.len() != contract.rows().len()
                    || !response
                        .rows
                        .iter()
                        .zip(contract.rows())
                        .all(|(measured, row)| {
                            (measured.scenario, measured.timing_tag)
                                == (row.scenario, row.timing_tag)
                        })
                {
                    return Err(crate::SynthError::invariant(
                        "regional cover-plan response rows do not match the reconstructed boundary contract",
                    ));
                }
            }
        }
        Ok(RegionCoverPlan {
            region: self.region,
            revision: self.revision,
            context_key: self.context_key,
            cost: self.cost,
            local_net_count: self.local_net_count,
            local_cell_count: self.local_cell_count,
            local_pin_count: self.local_pin_count,
            boundary_response: contracts.to_vec().into(),
            measured_response: Arc::clone(&self.measured_response),
            implementation_cells: Arc::clone(&self.implementation_cells),
            payload: Arc::clone(&self.payload),
        })
    }

    pub(crate) fn owned_memory_bytes(&self, shared: &mut RegionalSharedAllocations) -> usize {
        shared
            .charge(&self.boundaries, || 0)
            .saturating_add(shared.charge(&self.measured_response, || {
                self.measured_response
                    .iter()
                    .map(|response| {
                        opto_core::resident::slice_bytes::<BoundaryResponseRow>(response.rows.len())
                    })
                    .fold(0usize, usize::saturating_add)
            }))
            .saturating_add(shared.charge(&self.implementation_cells, || {
                self.implementation_cells
                    .iter()
                    .map(|cell| opto_core::resident::allocation_bytes(cell.cell_name.len()))
                    .fold(0usize, usize::saturating_add)
            }))
            .saturating_add(shared.charge(&self.payload, || 0))
    }
}
