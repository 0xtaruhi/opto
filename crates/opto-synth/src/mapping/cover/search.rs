// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::super::library::{
    CellBinding, CellBindingId, CombinationalCellCatalog, JointCellBinding,
};
use crate::boolean::logic::cuts::{CutDatabase, CutTruthDatabase, KCut};
use crate::boolean::logic::network::{LogicGraph, LogicNode, LogicNodeId};
use crate::boolean::logic::{TruthTable, inverter_truth, window_cares};
use crate::planning::mapping_policy::{CellCost, MappingCost};
use hashbrown::HashMap;
use opto_runtime::ExecutionContext;

const DONT_CARE_FILL_CAP: u32 = 4;
const RECOVERY_ROUND_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LibraryCoverSource {
    Constant(bool),
    Input(usize),
    Cell(usize),
    CellSecond(usize),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum LibraryCoverBinding {
    Single(CellBinding),
    Joint(JointCellBinding),
}

#[derive(Debug)]
pub(crate) struct LibraryCoverCell {
    pub(crate) second_node: Option<LogicNodeId>,
    pub(crate) binding: LibraryCoverBinding,
    pub(crate) binding_identity: Box<[u8]>,
    pub(crate) truth: TruthTable,
    pub(crate) second_truth: Option<TruthTable>,
    pub(crate) sources: Box<[LibraryCoverSource]>,
}

#[derive(Debug)]
pub(crate) struct LibraryCover {
    pub(crate) cells: Box<[LibraryCoverCell]>,
    pub(crate) outputs: Box<[LibraryCoverSource]>,
    pub(crate) total_area: f64,
    pub(crate) output_costs: Box<[MappingCost]>,
}

#[derive(Clone, Copy)]
pub(crate) struct CoverTiming<'a> {
    pub(crate) required_times: &'a [Option<f64>],
    pub(crate) output_loads: &'a [Option<f64>],
    pub(crate) input_transitions: &'a [Option<f64>],
    pub(crate) input_arrivals: &'a [Option<f64>],
}

#[cfg(test)]
pub(crate) fn cover_logic_network(
    network: &LogicGraph,
    cuts: &CutDatabase,
    outputs: &[LogicNodeId],
    catalog: &CombinationalCellCatalog,
    timing_constraints: CoverTiming<'_>,
    runtime: &ExecutionContext,
) -> Result<Option<LibraryCover>, crate::SynthError> {
    let truths = CutTruthDatabase::build_parallel(network, cuts, runtime)?;
    cover_logic_network_with_truths(
        network,
        cuts,
        &truths,
        outputs,
        catalog,
        timing_constraints,
        runtime,
    )
}

pub(crate) fn cover_logic_network_with_truths(
    network: &LogicGraph,
    cuts: &CutDatabase,
    truths: &CutTruthDatabase,
    outputs: &[LogicNodeId],
    catalog: &CombinationalCellCatalog,
    timing_constraints: CoverTiming<'_>,
    runtime: &ExecutionContext,
) -> Result<Option<LibraryCover>, crate::SynthError> {
    cover_logic_network_with_recovery(CoverProblem {
        network,
        cuts,
        truths,
        outputs,
        catalog,
        timing: timing_constraints,
        runtime,
    })
}

#[derive(Clone, Copy)]
struct CoverProblem<'a> {
    network: &'a LogicGraph,
    cuts: &'a CutDatabase,
    truths: &'a CutTruthDatabase,
    outputs: &'a [LogicNodeId],
    catalog: &'a CombinationalCellCatalog,
    timing: CoverTiming<'a>,
    runtime: &'a ExecutionContext,
}

fn cover_logic_network_with_recovery(
    problem: CoverProblem<'_>,
) -> Result<Option<LibraryCover>, crate::SynthError> {
    let CoverProblem {
        network,
        cuts,
        truths,
        outputs,
        catalog,
        timing: timing_constraints,
        runtime,
    } = problem;
    let CoverTiming {
        required_times,
        output_loads,
        ..
    } = timing_constraints;
    if outputs.is_empty() {
        return Ok(None);
    }
    if !matches!(
        network.node(LogicNodeId::from_index(0)),
        LogicNode::Const(false)
    ) {
        return Ok(None);
    }
    let diagnostics = catalog.diagnostics();
    let timing = diagnostics.timing;
    let planner_started = std::time::Instant::now();
    if outputs.len() != required_times.len() || outputs.len() != output_loads.len() {
        return Err(crate::SynthError::invariant(
            "mapping endpoint constraints do not align with cover outputs",
        ));
    }
    let mut planner = CoverPlanner::new(
        network,
        cuts,
        truths,
        catalog,
        planner::CoverEndpoints {
            outputs,
            timing: timing_constraints,
        },
        runtime,
    )?;
    let trace = crate::api::diagnostics::SynthTrace::new(timing);
    crate::api::diagnostics::trace!(
        trace,
        "cover.init",
        "nodes={} wall={:?}",
        network.node_count(),
        planner_started.elapsed()
    );
    let passes_started = std::time::Instant::now();
    let output_slots = outputs.iter().copied().map(slot).collect::<Vec<_>>();
    planner.flow_pass(runtime)?;
    if !planner.select(&output_slots)? {
        return Ok(None);
    }
    planner.update_reference_estimates();
    planner.update_required_arrivals(&output_slots, required_times)?;
    planner.update_load_estimates()?;
    planner.flow_pass(runtime)?;
    if !planner.select(&output_slots)? {
        return Ok(None);
    }
    planner.update_required_arrivals(&output_slots, required_times)?;
    planner.joint_pass()?;
    if !planner.select(&output_slots)? {
        return Err(crate::SynthError::invariant(
            "joint recovery produced an incomplete cover",
        ));
    }
    {
        for recovery_iteration in 1..=RECOVERY_ROUND_LIMIT {
            let before = planner.selected_area();
            let exact_started = std::time::Instant::now();
            let exact_changes = planner.exact_pass(runtime)?;
            let exact_elapsed = exact_started.elapsed();
            if !planner.select(&output_slots)? {
                return Err(crate::SynthError::invariant(
                    "exact recovery produced an incomplete cover",
                ));
            }
            let joint_started = std::time::Instant::now();
            let joint_changes = planner.joint_pass()?;
            let joint_elapsed = joint_started.elapsed();
            if !planner.select(&output_slots)? {
                return Err(crate::SynthError::invariant(
                    "joint recovery produced an incomplete cover",
                ));
            }
            let after = planner.selected_area();
            crate::api::diagnostics::trace!(
                trace,
                "cover.recovery",
                "iteration={recovery_iteration} area={before:.3}->{after:.3} \
                 exact={exact_elapsed:?}/{exact_changes} \
                 joint={joint_elapsed:?}/{joint_changes}"
            );
            match recovery_converged(recovery_iteration, exact_changes, joint_changes) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    crate::api::diagnostics::trace!(
                        trace,
                        "cover.recovery_limit",
                        "rounds={RECOVERY_ROUND_LIMIT}"
                    );
                    return Err(error);
                }
            }
        }
    }
    crate::api::diagnostics::trace!(trace, "cover.passes", "wall={:?}", passes_started.elapsed());
    if !planner.select(&output_slots)? {
        return Ok(None);
    }
    let cover = Some(planner.flatten(&output_slots)?);
    let joint_trace = crate::api::diagnostics::SynthTrace::new(diagnostics.joint_cells);
    if joint_trace.is_enabled() {
        let selected = cover.as_ref().map_or(0, |cover| {
            cover
                .cells
                .iter()
                .filter(|cell| cell.second_node.is_some())
                .count()
        });
        crate::api::diagnostics::trace!(
            joint_trace,
            "cover.joints",
            "nodes={} enumerated={} selected={}",
            network.node_count(),
            planner.joint_count(),
            selected
        );
    }
    Ok(cover)
}

fn recovery_converged(
    iteration: usize,
    exact_changes: usize,
    joint_changes: usize,
) -> Result<bool, crate::SynthError> {
    if exact_changes == 0 && joint_changes == 0 {
        return Ok(true);
    }
    if iteration == RECOVERY_ROUND_LIMIT {
        return Err(crate::SynthError::invariant(format!(
            "cover recovery did not converge within {RECOVERY_ROUND_LIMIT} rounds"
        )));
    }
    Ok(false)
}

fn slot(node: LogicNodeId) -> usize {
    node.index() * 2 + usize::from(node.is_inverted())
}

fn slot_node(slot: usize) -> LogicNodeId {
    let node = LogicNodeId::from_index(slot / 2);
    if slot & 1 == 1 { node.inverted() } else { node }
}

fn opposite(slot: usize) -> usize {
    slot ^ 1
}

fn full_truth_mask(assignments: usize) -> u64 {
    if assignments == u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << assignments) - 1
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Candidate {
    truth_bits: u64,
    binding: CellBindingId,
    truth_input_count: u8,
    cut: u8,
    inversions: u8,
    inverted_extra: u8,
}

struct CandidateIndex {
    arenas: Box<[Vec<Candidate>]>,
    ranges: Box<[CandidateRange]>,
}

#[derive(Clone, Copy)]
struct CandidateRange {
    arena: u32,
    start: u32,
    len: u32,
}

impl std::ops::Index<usize> for CandidateIndex {
    type Output = [Candidate];

    fn index(&self, slot: usize) -> &Self::Output {
        let range = self.ranges[slot];
        let arena = &self.arenas[range.arena as usize];
        let start = range.start as usize;
        &arena[start..start + range.len as usize]
    }
}

impl Candidate {
    const NO_INVERTED_EXTRA: u8 = u8::MAX;

    fn new(
        truth: TruthTable,
        truth_input_count: u8,
        binding: CellBindingId,
        cut: u8,
        inversions: u8,
        inverted_extra: Option<u8>,
    ) -> Self {
        Self {
            truth_bits: truth.bits,
            binding,
            truth_input_count,
            cut,
            inversions,
            inverted_extra: inverted_extra.unwrap_or(Self::NO_INVERTED_EXTRA),
        }
    }

    fn truth(self) -> TruthTable {
        TruthTable {
            input_count: usize::from(self.truth_input_count),
            bits: self.truth_bits,
        }
    }

    fn cell_binding(self, catalog: &CombinationalCellCatalog) -> CellBinding {
        catalog.binding(self.binding)
    }

    fn nominal_cost(self, catalog: &CombinationalCellCatalog) -> CellCost {
        catalog.cost_for_binding_id(self.binding)
    }

    fn leaf_slot(&self, input: usize, leaf: LogicNodeId) -> usize {
        leaf.index() * 2 + ((self.inversions >> input) & 1) as usize
    }

    fn extra_slot(&self, cut: KCut) -> Option<usize> {
        (self.inverted_extra != Self::NO_INVERTED_EXTRA).then(|| {
            let input = self.inverted_extra;
            let leaf = cut.leaves()[input as usize];
            leaf.index() * 2 + (1 ^ ((self.inversions >> input) & 1)) as usize
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotChoice {
    Constant(bool),
    Boundary(usize),
    Cell(u32),
    Inverter,
    JointOutput(u32),
    JointCell(u32),
}

#[derive(Debug, Clone, Copy)]
struct InverterCell {
    binding: CellBinding,
    cost: CellCost,
}

#[derive(Debug, Clone)]
struct Joint {
    cut: KCut,
    inversions: u8,
    binding: JointCellBinding,
    cost: CellCost,
    slots: [usize; 2],
    truths: [TruthTable; 2],
}

impl Joint {
    fn leaf_slots(&self) -> impl Iterator<Item = usize> + '_ {
        self.cut
            .leaves()
            .iter()
            .copied()
            .enumerate()
            .map(|(input, leaf)| leaf.index() * 2 + ((self.inversions >> input) & 1) as usize)
    }
}

mod planner;
use planner::CoverPlanner;

fn tighten_required_arrival(
    required_arrivals: &mut [f64],
    pending: &mut Vec<usize>,
    target: usize,
    candidate: f64,
) {
    if candidate < required_arrivals[target] {
        required_arrivals[target] = candidate;
        pending.push(target);
    }
}

mod candidates;
mod normalization;
use candidates::{CandidateContext, enumerate_joints, node_candidates, observability_cares};
#[derive(Debug, Clone, Copy)]
struct FlowChoice {
    choice: SlotChoice,
    cost: MappingCost,
    truth: TruthTable,
    order: (u8, u8, u32),
}

#[derive(Debug, Clone, Copy)]
struct ExactChoice {
    choice: SlotChoice,
    area: f64,
    arrival: f64,
    truth: TruthTable,
    order: (u8, u8, u32),
}

impl ExactChoice {
    fn prefers_over(&self, current: &Self, timing_driven: bool) -> bool {
        crate::planning::mapping_policy::compare_area_arrival_objective(
            timing_driven,
            self.area,
            self.arrival,
            current.area,
            current.arrival,
        )
        .then_with(|| self.truth.cmp(&current.truth))
        .then_with(|| self.order.cmp(&current.order))
        .is_lt()
    }
}

fn joint_replacement_is_preferred(
    timing_driven: bool,
    restores_timing: bool,
    candidate_area: f64,
    candidate_arrival: f64,
    current_area: f64,
    current_arrival: f64,
) -> bool {
    restores_timing
        || crate::planning::mapping_policy::compare_area_arrival_objective(
            timing_driven,
            candidate_area,
            candidate_arrival,
            current_area,
            current_arrival,
        )
        .is_lt()
}

#[cfg(test)]
mod tests;
