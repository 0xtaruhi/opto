// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    Candidate, CandidateContext, CandidateIndex, CandidateRange, CellBinding, CellCost,
    CombinationalCellCatalog, CoverTiming, CutDatabase, CutTruthDatabase, ExactChoice,
    ExecutionContext, FlowChoice, HashMap, InverterCell, Joint, KCut, LibraryCover,
    LibraryCoverBinding, LibraryCoverCell, LibraryCoverSource, LogicGraph, LogicNode, LogicNodeId,
    MappingCost, SlotChoice, TruthTable, enumerate_joints, full_truth_mask, inverter_truth,
    node_candidates, observability_cares, opposite, slot, slot_node, tighten_required_arrival,
    window_cares,
};
use smallvec::SmallVec;

mod demand;
mod implementation;
mod selection;

use demand::CoverDemand;
pub(super) use implementation::CoverEndpoints;

type LiteralDependencies = SmallVec<[usize; 8]>;

#[derive(Default)]
struct ExactViability {
    active: bool,
    candidates: Box<[bool]>,
    inverter: bool,
    joints: Box<[bool]>,
}

pub(crate) struct CoverPlanner<'a> {
    network: &'a LogicGraph,
    cuts: &'a CutDatabase,
    catalog: &'a CombinationalCellCatalog,
    inverter: Option<InverterCell>,
    candidates: CandidateIndex,
    joints: Vec<Joint>,
    slot_joints: opto_core::PackedRows<u32>,
    joints_by_node: opto_core::PackedRows<u32>,
    base_slots: usize,
    choices: Vec<Option<SlotChoice>>,
    flows: Vec<MappingCost>,
    required_arrivals: Vec<f64>,
    endpoint_loads: Vec<f64>,
    load_estimates: Vec<f64>,
    input_transitions: Vec<f64>,
    input_arrivals: Vec<f64>,
    loads_ready: bool,
    reference_estimates: Vec<f64>,
    demand: CoverDemand,
    live_nodes: Box<[bool]>,
    /// Reused frontier buffers for [`CoverPlanner::change_choices_references`].
    ///
    /// Exact recovery calls it twice per candidate per slot, so allocating its
    /// two frontiers per call dominated the pass.
    reference_scratch: ReferenceScratch,
    trial_scratch: TrialScratch,
}

/// Frontier buffers owned across reference-count updates.
#[derive(Default)]
struct ReferenceScratch {
    seeded_roots: Vec<usize>,
    next: Vec<usize>,
}

/// Visited marks owned across trial evaluations, stamped by an epoch so that
/// starting a trial costs nothing.
#[derive(Default)]
struct TrialScratch {
    epoch: u32,
    marks: Vec<u32>,
    frontier: Vec<usize>,
    next: Vec<usize>,
}

impl TrialScratch {
    fn begin(&mut self, slots: usize) {
        self.marks.resize(slots, 0);
        self.frontier.clear();
        self.next.clear();
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
    }

    /// Marks a slot, reporting whether this trial had not reached it yet.
    fn mark(&mut self, slot: usize) -> bool {
        std::mem::replace(&mut self.marks[slot], self.epoch) != self.epoch
    }
}

const MAX_OBSERVABILITY_CONSUMERS: usize = 8;

pub(crate) fn analyze_node_cares(
    network: &LogicGraph,
    cuts: &CutDatabase,
    index: usize,
    consumers: &[u32],
    is_output: bool,
    exact_only: &[bool],
) -> (Option<Box<[u64]>>, bool) {
    let node = LogicNodeId::from_index(index);
    if !network.node(node).is_cover_node() {
        return (None, true);
    }
    let mut cares = window_cares(network, cuts, node);
    if !is_output
        && !consumers.is_empty()
        && consumers.len() <= MAX_OBSERVABILITY_CONSUMERS
        && consumers
            .iter()
            .all(|&consumer| exact_only[consumer as usize])
    {
        let mut observed_union = vec![0u64; cuts.cuts(node).len()];
        let mut complete = true;
        for &consumer in consumers {
            let Some(observability) = observability_cares(network, cuts, index, consumer as usize)
            else {
                complete = false;
                break;
            };
            for (observed, consumer_observed) in observed_union.iter_mut().zip(observability.iter())
            {
                *observed |= consumer_observed;
            }
        }
        if complete {
            let merged = cares.get_or_insert_with(|| {
                std::iter::repeat_n(u64::MAX, cuts.cuts(node).len()).collect()
            });
            for (care, observed) in merged.iter_mut().zip(observed_union) {
                *care &= observed;
            }
        }
    }
    let exact = cares.as_ref().is_none_or(|cares| {
        cuts.cuts(node)
            .iter()
            .zip(cares.iter())
            .all(|(cut, &care)| {
                let assignments = 1usize << cut.len();
                let full = full_truth_mask(assignments);
                care & full == full
            })
    });
    (cares, exact)
}
