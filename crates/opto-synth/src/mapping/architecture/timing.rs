// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Bounded architecture feedback over the ordinary region-private AXM graph.
//!
//! Probes append only temporary Word binding handles. Every probe rolls them
//! back before the next recipe vector is constructed; only compact decisions
//! survive. Structural timing guides selection, while Liberty cover and the
//! enclosing mapped MMMC epochs remain the electrical authorities.

use super::{
    ArchitectureDecisions, BTreeMap, LocalRegionBooleanLowering, LocalRegionBooleanRequest,
    MappingRoot, ProvenanceBuilder, RegionArchitectureMaterializer, SynthError, SynthesisRegion,
    lower_local_region_boolean, mapping_roots, word,
};
use crate::boolean::logic::network::LogicNodeId;
use crate::boolean::logic::{CanonicalRegionLogic, StructuralTiming, TimingBudget};

const ARCHITECTURE_PROBES: usize = 4;

#[cfg(test)]
mod tests;

pub(super) struct PathSelection<'a> {
    pub(super) module: &'a mut word::WordModule,
    pub(super) decisions: &'a mut ArchitectureDecisions,
    pub(super) region: SynthesisRegion,
    pub(super) tracked_values: &'a [word::ValueId],
    pub(super) boundary_inputs: &'a [word::ValueId],
    pub(super) roots: &'a [(MappingRoot, word::ValueId)],
    pub(super) operation_sources: &'a crate::planning::regional::LocalOperationProvenance,
    pub(super) source_to_local: &'a BTreeMap<word::ValueId, word::ValueId>,
}

impl RegionArchitectureMaterializer<'_, '_> {
    pub(super) fn select_path_architecture(
        &self,
        selection: PathSelection<'_>,
    ) -> Result<(), SynthError> {
        let PathSelection {
            module,
            decisions,
            region,
            tracked_values,
            boundary_inputs,
            roots,
            operation_sources,
            source_to_local,
        } = selection;
        decisions.select_for_budgets(self.request.target_model, |_| None)?;
        if decisions.operators().is_empty() {
            return Ok(());
        }
        let sequential = super::super::sequential::SequentialTimingProjection::build(
            module,
            &self.request.mapping_context.sequential_catalog,
            &self.request.mapping_context.combinational_catalog,
        )?;
        let mut roots = roots.to_vec();
        roots.extend(
            mapping_roots(
                module,
                self.request.timing,
                &opto_timing::PortBindings::new([]),
                Some(&sequential),
            )?
            .into_iter()
            .map(|root| (root, root.value)),
        );
        let mut tracked = tracked_values.to_vec();
        tracked.extend(roots.iter().map(|(_, value)| *value));
        for &operator in decisions.operators() {
            tracked.push(operator.result());
            tracked.extend(decisions.operator_inputs(operator));
        }
        tracked.sort_unstable();
        tracked.dedup();
        let Some(stage_delay) = self
            .request
            .mapping_context
            .combinational_catalog
            .representative_cost()
            .map(|cost| cost.delay)
            .filter(|delay| delay.is_finite() && *delay > 0.0)
        else {
            return Ok(());
        };
        let mut best = decisions.clone();
        let mut best_quality = None;
        for round in 0..ARCHITECTURE_PROBES {
            let checkpoint = module.speculation_checkpoint();
            let probe = (|| {
                let (_, operators) = self.prepare_operators(
                    region,
                    module,
                    decisions,
                    operation_sources,
                    source_to_local,
                )?;
                let mut provenance = ProvenanceBuilder::for_regional_candidate(module);
                let lowering = lower_local_region_boolean(
                    module,
                    LocalRegionBooleanRequest {
                        plan: decisions,
                        operators: &operators,
                        provenance: &mut provenance,
                        owner: region.row(),
                        boundary_inputs,
                        roots: &roots.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
                        tracked_values: &tracked,
                    },
                )?;
                let mut slice = super::super::logic_partition::RegionLogicSlice::build_candidate(
                    region.id(),
                    [0; 32],
                    super::super::logic_partition::RegionLogicDomain {
                        module,
                        subject_inputs: &lowering.subject.inputs,
                        source_to_local,
                        ownership: &lowering.ownership,
                        contracts: self.request.contracts.contracts(region.row()),
                        roots: &roots,
                    },
                )?;
                slice.project_sequential_timing(&sequential);
                measure_paths(&lowering, &slice, decisions, stage_delay)
            })();
            module
                .rollback_speculation(checkpoint)
                .map_err(SynthError::from)?;
            let Some((quality, budgets)) = probe? else {
                break;
            };
            crate::api::diagnostics::trace!(
                crate::api::diagnostics::SynthTrace::timing(
                    self.request.mapping_context.config.diagnostics
                ),
                "architecture.path_probe",
                "region={} round={round} worst_violation={} total_violation={} gates={}",
                region.row().raw(),
                quality.0,
                quality.1,
                quality.2,
            );
            if best_quality.is_none_or(|current| quality < current) {
                best_quality = Some(quality);
                best = decisions.clone();
            }
            if !decisions.select_for_budgets(self.request.target_model, |operator| {
                budgets
                    .binary_search_by_key(&operator.id(), |&(id, _)| id)
                    .ok()
                    .and_then(|index| budgets[index].1)
            })? {
                break;
            }
        }
        *decisions = best;
        Ok(())
    }
}

type PathQuality = (u32, u64, usize);
type OperatorBudgets = Vec<(crate::OperatorId, Option<f64>)>;

/// Projects each operator's available time from its own inputs and outputs.
/// Sequential values are AXM inputs, not graph edges back into next-state
/// logic. Ordinary logic and reconvergent paths participate in both sweeps.
fn measure_paths(
    lowering: &LocalRegionBooleanLowering,
    slice: &super::super::logic_partition::RegionLogicSlice,
    decisions: &ArchitectureDecisions,
    stage_delay: f64,
) -> Result<Option<(PathQuality, OperatorBudgets)>, SynthError> {
    let subject = &lowering.subject;
    let roots = slice
        .roots()
        .iter()
        .filter_map(|root| node(subject, root.value).map(|node| (node, root.required_time)))
        .collect::<Vec<_>>();
    let root_nodes = roots.iter().map(|(node, _)| *node).collect::<Vec<_>>();
    let requirements = roots
        .iter()
        .map(|(_, required)| *required)
        .collect::<Vec<_>>();
    let arrivals = subject
        .inputs
        .iter()
        .map(|&input| slice.search_input_arrival(input))
        .collect::<Vec<_>>();
    let Some(timing) = TimingBudget::for_roots(
        &subject.network,
        &root_nodes,
        StructuralTiming::new(&requirements, &arrivals, Some(stage_delay)),
    )?
    else {
        return Ok(None);
    };
    let worst = root_nodes
        .iter()
        .map(|&root| timing.violation(root))
        .max()
        .unwrap_or(0);
    let total = root_nodes
        .iter()
        .map(|&root| u64::from(timing.violation(root)))
        .fold(0u64, u64::saturating_add);
    let live = subject.network.live_nodes(&root_nodes);
    let gates = live
        .iter()
        .enumerate()
        .filter(|(index, live)| {
            **live
                && subject
                    .network
                    .node(LogicNodeId::from_index(*index))
                    .is_gate()
        })
        .count();
    let mut budgets = Vec::with_capacity(decisions.operators().len());
    for &operator in decisions.operators() {
        // Preserve bit correspondence: the latest operand bit need not feed
        // the tightest result bit. Price a recipe's delay change against the
        // minimum slack of actual result-bit paths in the current graph.
        let margin = lowering
            .ownership
            .lowered_bits(operator.result())
            .unwrap_or_default()
            .iter()
            .filter_map(|&value| node(subject, value))
            .filter_map(|node| {
                timing
                    .required(node)
                    .map(|required| f64::from(required) - f64::from(timing.arrival(node)))
            })
            .min_by(f64::total_cmp);
        let selected = decisions.selected_candidate(operator.id()).ok_or_else(|| {
            SynthError::invariant("path projection has no selected operator recipe")
        })?;
        let depth = f64::from(decisions.candidate_estimate(selected)?.logic_depth);
        budgets.push((
            operator.id(),
            margin.map(|margin| (depth + margin) * stage_delay),
        ));
    }
    budgets.sort_unstable_by_key(|&(id, _)| id);
    Ok(Some(((worst, total, gates), budgets)))
}

fn node(subject: &CanonicalRegionLogic, value: word::ValueId) -> Option<LogicNodeId> {
    subject
        .value_nodes
        .binary_search_by_key(&value, |&(value, _)| value)
        .ok()
        .map(|index| subject.value_nodes[index].1)
}
