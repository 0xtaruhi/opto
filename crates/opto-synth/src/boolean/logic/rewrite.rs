// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::TruthTable;
use super::cuts::{CutDatabase, IncrementalCutInputs, KCut};
use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::pipeline::{TransformProduct, TransformState};
use hashbrown::HashMap;
use opto_runtime::ExecutionContext;
use std::collections::VecDeque;
use std::sync::Arc;

mod cache;
mod decision;
mod materialize;
mod planning;
mod recipe;
mod support;

pub(crate) use cache::RewriteRecipeCache;
use cache::RewriteRecipeKey;
pub(in crate::boolean::logic) use decision::{Plan, Synthesizer};
use planning::{DecisionAnalysis, decide_node, expand_truth, full_truth_mask, plan_level};
use recipe::{
    CandidateDecision, Decision, PlanRecipe, RecipeNode, build_pair_function, census_divisors,
    remap_pair_truth,
};
pub(crate) use support::{CoverageCheck, projected_cuts, projected_leaves, window_cares};
use support::{REWRITE_CUTS_PER_NODE, SupportIndex, build_support_index};

const WINDOW_CUT_LEAVES: usize = 6;
const DIVISOR_CAP: usize = 16;
const DIVISOR_DEPTH: usize = 2;
const MIN_DIVISOR_CUT_LEAVES: usize = 3;
const MAX_CENSUS_DIVISORS: usize =
    WINDOW_CUT_LEAVES * (WINDOW_CUT_LEAVES - 1) / 2 * recipe::PAIR_TRUTHS.len();
// SmallVec supports this inline array size without its optional const-generics
// feature, and the six-input census needs at most MAX_CENSUS_DIVISORS (75).
const CENSUS_DIVISOR_STORAGE: usize = 96;
const MAX_PASSES: usize = 6;
const MFFC_NODE_BUDGET: usize = 4_096;
const MFFC_TABLE_CAPACITY: usize = MFFC_NODE_BUDGET * 2;
const MAX_PLAN_NODES: usize = (1 << (WINDOW_CUT_LEAVES + DIVISOR_DEPTH + 1)) - 1;
const AND_WEIGHT: u32 = 2;
const XOR_WEIGHT: u32 = 5;
const MUX_WEIGHT: u32 = 6;

#[derive(Clone, Copy, Default)]
struct ReferenceDelta {
    node: u32,
    decrements: u32,
}

/// Fixed-budget sparse overlay for one removable-cone traversal. The dense
/// reference census stays immutable and is shared by every worker.
struct MffcScratch {
    deltas: Box<[ReferenceDelta]>,
    touched: Vec<usize>,
    stack: Vec<LogicNodeId>,
    dying: Vec<u32>,
}

impl MffcScratch {
    fn new() -> Self {
        debug_assert!(MFFC_TABLE_CAPACITY.is_power_of_two());
        Self {
            deltas: vec![ReferenceDelta::default(); MFFC_TABLE_CAPACITY].into_boxed_slice(),
            touched: Vec::with_capacity(MFFC_NODE_BUDGET),
            stack: Vec::with_capacity(MFFC_NODE_BUDGET),
            dying: Vec::with_capacity(MFFC_NODE_BUDGET),
        }
    }

    fn begin(&mut self, root: LogicNodeId) {
        for slot in self.touched.drain(..) {
            self.deltas[slot] = ReferenceDelta::default();
        }
        self.stack.clear();
        self.dying.clear();
        self.stack.push(root.positive());
        self.dying.push(
            u32::try_from(root.index())
                .expect("logic node index is bounded by compact graph storage"),
        );
    }

    fn decrement(&mut self, node: LogicNodeId, references: &[u32]) -> Option<bool> {
        let key = u32::try_from(node.index()).ok()?.checked_add(1)?;
        let mask = MFFC_TABLE_CAPACITY - 1;
        let mut slot = (key.wrapping_mul(0x9e37_79b9) as usize) & mask;
        loop {
            let delta = &mut self.deltas[slot];
            if delta.node == key {
                delta.decrements = delta.decrements.checked_add(1)?;
                if delta.decrements > references[node.index()] {
                    return None;
                }
                return Some(delta.decrements == references[node.index()]);
            }
            if delta.node == 0 {
                if self.touched.len() == MFFC_NODE_BUDGET {
                    return None;
                }
                *delta = ReferenceDelta {
                    node: key,
                    decrements: 1,
                };
                self.touched.push(slot);
                return Some(references[node.index()] == 1);
            }
            slot = (slot + 1) & mask;
        }
    }
}

struct TimingBudget {
    required: Box<[u32]>,
}

impl TimingBudget {
    fn for_roots(
        network: &LogicGraph,
        roots: &[LogicNodeId],
        requirements: &[Option<f64>],
    ) -> Option<Self> {
        let mut required = vec![u32::MAX; network.node_count()];
        let mut constrained = false;
        for (&root, requirement) in roots.iter().zip(requirements) {
            if requirement.is_none() {
                continue;
            }
            constrained = true;
            required[root.index()] = required[root.index()].min(network.level(root));
        }
        if !constrained {
            return None;
        }
        for index in (0..network.node_count()).rev() {
            let limit = required[index];
            if limit == u32::MAX {
                continue;
            }
            for fanin in network.node(LogicNodeId::from_index(index)).fanins() {
                required[fanin.index()] = required[fanin.index()].min(limit.saturating_sub(1));
            }
        }
        Some(Self {
            required: required.into_boxed_slice(),
        })
    }

    fn limit(&self, network: &LogicGraph, node: LogicNodeId) -> u32 {
        match self.required[node.index()] {
            u32::MAX => network.level(node),
            required => required,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RewriteIncremental<'a> {
    recipe_cache: &'a RewriteRecipeCache,
    metrics: &'a crate::incremental::IncrementalRunMetrics,
}

impl<'a> RewriteIncremental<'a> {
    pub(crate) const fn new(
        recipe_cache: &'a RewriteRecipeCache,
        metrics: &'a crate::incremental::IncrementalRunMetrics,
    ) -> Self {
        Self {
            recipe_cache,
            metrics,
        }
    }
}

pub(crate) struct CutReuse {
    cuts: CutDatabase,
    old_to_new: Box<[Option<LogicNodeId>]>,
    new_to_old: Box<[Option<u32>]>,
    old_predecessors: Box<[[Option<u32>; 3]]>,
    references: Box<[u32]>,
}

#[cfg(test)]
pub(crate) fn optimize_network(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<TransformProduct, crate::SynthError> {
    super::pipeline::optimize_with(
        network,
        roots,
        requirements,
        diagnostics,
        runtime,
        None,
        super::pipeline::OptimizationPolicy::Baseline,
    )
}

#[cfg(test)]
pub(crate) fn optimize_network_cached(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: RewriteIncremental<'_>,
) -> Result<TransformProduct, crate::SynthError> {
    super::pipeline::optimize_with(
        network,
        roots,
        requirements,
        diagnostics,
        runtime,
        Some(incremental),
        super::pipeline::OptimizationPolicy::Baseline,
    )
}

pub(super) fn resynthesize(
    state: &mut TransformState,
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: RewriteIncremental<'_>,
) -> Result<(), crate::SynthError> {
    normalize(state, requirements, diagnostics, runtime, incremental)?;
    let census_started = std::time::Instant::now();
    let mut approved = census_divisors(&state.network, &state.roots, runtime)?;
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(diagnostics),
        "logic.rewrite.census",
        "wall={:?}",
        census_started.elapsed()
    );
    if !approved.is_empty() {
        let next = rewrite_pass(
            &state.network,
            &state.roots,
            RewritePass {
                virtuals: &approved,
                reuse: state.analyses.rewrite.as_ref(),
                incremental_decisions: false,
                requirements,
                diagnostics,
                runtime,
                incremental,
            },
        )?;
        approved = approved.remapped(&next.remap);
        state.apply(next)?;
        converge(
            state,
            &mut approved,
            RewriteEnvironment {
                requirements,
                diagnostics,
                runtime,
                incremental,
            },
        )?;
    }
    Ok(())
}

pub(super) fn normalize(
    state: &mut TransformState,
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: RewriteIncremental<'_>,
) -> Result<(), crate::SynthError> {
    let mut empty = ApprovedDivisors::default();
    let first = rewrite_pass(
        &state.network,
        &state.roots,
        RewritePass {
            virtuals: &empty,
            reuse: state.analyses.rewrite.as_ref(),
            incremental_decisions: false,
            requirements,
            diagnostics,
            runtime,
            incremental,
        },
    )?;
    state.apply(first)?;
    converge(
        state,
        &mut empty,
        RewriteEnvironment {
            requirements,
            diagnostics,
            runtime,
            incremental,
        },
    )?;
    Ok(())
}

fn converge(
    state: &mut TransformState,
    virtuals: &mut ApprovedDivisors,
    environment: RewriteEnvironment<'_>,
) -> Result<(), crate::SynthError> {
    let RewriteEnvironment {
        requirements,
        diagnostics,
        runtime,
        incremental,
    } = environment;
    let mut cost = network_score(&state.network, &state.roots, requirements);
    for _ in 1..MAX_PASSES {
        let next = rewrite_pass(
            &state.network,
            &state.roots,
            RewritePass {
                virtuals,
                reuse: state.analyses.rewrite.as_ref(),
                incremental_decisions: true,
                requirements,
                diagnostics,
                runtime,
                incremental,
            },
        )?;
        let next_roots = super::pipeline::map_roots(&next.remap, &state.roots)?;
        let next_cost = network_score(&next.network, &next_roots, requirements);
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(diagnostics),
            "logic.rewrite.score",
            "depth={}->{} total_depth={}->{} weight={}->{} gates={}->{}",
            cost.depth,
            next_cost.depth,
            cost.total_depth,
            next_cost.total_depth,
            cost.weight,
            next_cost.weight,
            cost.gates,
            next_cost.gates
        );
        if next_cost >= cost {
            break;
        }
        cost = next_cost;
        *virtuals = virtuals.remapped(&next.remap);
        state.apply(next)?;
    }
    Ok(())
}

pub(crate) fn remap_literal(
    remap: &[Option<LogicNodeId>],
    literal: LogicNodeId,
) -> Option<LogicNodeId> {
    remap[literal.index()].map(|mapped| {
        if literal.is_inverted() {
            mapped.inverted()
        } else {
            mapped
        }
    })
}

fn network_size(network: &LogicGraph) -> (u64, usize) {
    let mut weight = 0u64;
    let mut gates = 0usize;
    for index in 0..network.node_count() {
        let node = network.node(LogicNodeId::from_index(index));
        weight += u64::from(node_weight(node));
        gates += usize::from(node.is_gate());
    }
    (weight, gates)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NetworkScore {
    depth: u32,
    total_depth: u64,
    weight: u64,
    gates: usize,
}

fn network_score(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
) -> NetworkScore {
    let (weight, gates) = network_size(network);
    let (depth, total_depth) = roots
        .iter()
        .zip(requirements)
        .filter_map(|(&root, requirement)| requirement.map(|_| network.level(root)))
        .fold((0, 0), |(maximum, total), depth| {
            (maximum.max(depth), total + u64::from(depth))
        });
    NetworkScore {
        depth,
        total_depth,
        weight,
        gates,
    }
}

pub(super) fn timing_profile(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
) -> (u32, u64) {
    let score = network_score(network, roots, requirements);
    (score.depth, score.total_depth)
}

fn node_weight(node: LogicNode) -> u32 {
    match node {
        LogicNode::Const(_) | LogicNode::Var(_) => 0,
        LogicNode::And(..) => AND_WEIGHT,
        LogicNode::Xor(..) => XOR_WEIGHT,
        LogicNode::Mux { .. } => MUX_WEIGHT,
    }
}

#[derive(Clone, Copy)]
struct RewritePass<'a> {
    virtuals: &'a ApprovedDivisors,
    reuse: Option<&'a CutReuse>,
    incremental_decisions: bool,
    requirements: &'a [Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &'a ExecutionContext,
    incremental: RewriteIncremental<'a>,
}

#[derive(Clone, Copy)]
struct RewriteEnvironment<'a> {
    requirements: &'a [Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &'a ExecutionContext,
    incremental: RewriteIncremental<'a>,
}

fn rewrite_pass(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    pass: RewritePass<'_>,
) -> Result<TransformProduct, crate::SynthError> {
    let RewritePass {
        virtuals,
        reuse,
        incremental_decisions,
        requirements,
        diagnostics,
        runtime,
        incremental,
    } = pass;
    let timing = diagnostics.timing;
    let started = std::time::Instant::now();
    let node_count = network.node_count();
    let references = network.reference_counts(roots);
    let timing_budget = TimingBudget::for_roots(network, roots, requirements);

    let (cuts, active) = match reuse {
        Some(reuse) => {
            let (incremental, reused) = CutDatabase::build_incremental(
                network,
                WINDOW_CUT_LEAVES,
                REWRITE_CUTS_PER_NODE,
                IncrementalCutInputs {
                    previous: &reuse.cuts,
                    old_to_new: &reuse.old_to_new,
                    new_to_old: &reuse.new_to_old,
                    old_predecessors: &reuse.old_predecessors,
                    check_incremental: diagnostics.check_incremental,
                },
                runtime,
            )?;
            if diagnostics.check_incremental {
                let full = CutDatabase::build_with_cut_cap_parallel(
                    network,
                    WINDOW_CUT_LEAVES,
                    REWRITE_CUTS_PER_NODE,
                    runtime,
                )?;
                incremental.assert_same(&full);
            }
            crate::api::diagnostics::trace!(
                crate::api::diagnostics::SynthTrace::new(timing),
                "logic.rewrite.cut_reuse",
                "reused={} of {node_count}",
                reused.iter().filter(|&&reused| reused).count()
            );
            let active = (incremental_decisions && timing_budget.is_none())
                .then(|| incremental_decision_active(network, &references, reuse, &reused));
            (incremental, active)
        }
        None => (
            CutDatabase::build_with_cut_cap_parallel(
                network,
                WINDOW_CUT_LEAVES,
                REWRITE_CUTS_PER_NODE,
                runtime,
            )?,
            None,
        ),
    };
    let trace = crate::api::diagnostics::SynthTrace::new(timing);
    crate::api::diagnostics::trace!(
        trace,
        "logic.rewrite.cuts",
        "nodes={node_count} wall={:?}",
        started.elapsed()
    );
    if let Some(active) = &active {
        crate::api::diagnostics::trace!(
            trace,
            "logic.rewrite.active",
            "active={} of {node_count}",
            active.iter().filter(|&&active| active).count()
        );
    }
    let index_started = std::time::Instant::now();
    let support_index = build_support_index(network, &cuts, &references, runtime)?;

    crate::api::diagnostics::trace!(
        trace,
        "logic.rewrite.index",
        "wall={:?}",
        index_started.elapsed()
    );
    let decide_started = std::time::Instant::now();
    let structural = opto_ir::logic::StructuralIndex::of(network.storage_network());
    let analysis = DecisionAnalysis {
        network,
        cuts: &cuts,
        support_index: &support_index,
        virtuals,
        references: &references,
        timing: timing_budget.as_ref(),
        active: active.as_deref(),
        recipe_cache: incremental.recipe_cache,
        incremental_metrics: incremental.metrics,
        check_incremental: diagnostics.check_incremental,
        structural: &structural,
    };
    let decisions = runtime.analyze_indexed_with(
        node_count,
        || (MffcScratch::new(), Synthesizer::fresh()),
        |(mffc, synthesizer), index| decide_node(&analysis, mffc, synthesizer, index),
    )?;

    crate::api::diagnostics::trace!(
        trace,
        "logic.rewrite.decide",
        "wall={:?} applied={} of {}",
        decide_started.elapsed(),
        decisions
            .iter()
            .filter(|decision| decision.is_some())
            .count(),
        node_count
    );
    let materialize_started = std::time::Instant::now();
    let mut outcome = materialize::materialize(network, &decisions, virtuals, roots);
    let old_to_new = outcome.remap.clone();
    let mut owners = vec![None; outcome.network.node_count()];
    let mut collisions = vec![false; owners.len()];
    for (old, mapped) in old_to_new.iter().enumerate() {
        let Some(mapped) = mapped else {
            continue;
        };
        if decisions[old].is_some() {
            continue;
        }
        if !materialize::is_exact_copy(network, &outcome.network, old, &old_to_new) {
            continue;
        }
        let new = mapped.index();
        if owners[new].is_some() {
            owners[new] = None;
            collisions[new] = true;
        } else if !collisions[new] {
            owners[new] =
                Some(u32::try_from(old).expect("logic graph is bounded by compact node storage"));
        }
    }
    drop(decisions);
    let old_predecessors = owners
        .iter()
        .map(|owner| {
            let mut predecessors = [None; 3];
            if let Some(old) = owner {
                for (slot, fanin) in network
                    .node(LogicNodeId::from_index(*old as usize))
                    .fanins()
                    .enumerate()
                {
                    predecessors[slot] = Some(
                        u32::try_from(fanin.index())
                            .expect("logic node index is bounded by compact graph storage"),
                    );
                }
            }
            predecessors
        })
        .collect();
    outcome.analyses.rewrite = Some(CutReuse {
        cuts,
        old_to_new,
        new_to_old: owners.into_boxed_slice(),
        old_predecessors,
        references: references.into_boxed_slice(),
    });
    crate::api::diagnostics::trace!(
        trace,
        "logic.rewrite.materialize",
        "wall={:?}",
        materialize_started.elapsed()
    );
    Ok(outcome)
}

fn incremental_decision_active(
    network: &LogicGraph,
    references: &[u32],
    reuse: &CutReuse,
    reused: &[bool],
) -> Box<[bool]> {
    assert_eq!(network.node_count(), references.len());
    assert_eq!(network.node_count(), reused.len());
    let reference_changed = reuse
        .new_to_old
        .iter()
        .enumerate()
        .map(|(index, old)| {
            old.is_none_or(|old| {
                reuse
                    .references
                    .get(old as usize)
                    .is_none_or(|&old| old != references[index])
            })
        })
        .collect::<Box<[_]>>();
    (0..network.node_count())
        .map(|index| {
            !reused[index]
                || reference_changed[index]
                || network
                    .node(LogicNodeId::from_index(index))
                    .fanins()
                    .any(|fanin| reference_changed[fanin.index()])
        })
        .collect()
}

const EXTRACTION_MIN_USES: usize = 3;
const EXTRACTION_MARGIN: i64 = 2;
const EXTRACTION_CAP: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DivisorRef {
    Node(LogicNodeId),
    Virtual(u32),
}

type Divisors = smallvec::SmallVec<[(DivisorRef, u64); DIVISOR_CAP]>;
type DivisorFunctions = smallvec::SmallVec<[u64; DIVISOR_CAP]>;
type PlanInputs = smallvec::SmallVec<[LogicNodeId; WINDOW_CUT_LEAVES + DIVISOR_CAP]>;

#[derive(Default)]
struct ApprovedDivisors {
    definitions: Vec<(LogicNodeId, LogicNodeId, u8)>,
    by_pair: HashMap<(u32, u32), Vec<(u32, u8)>>,
}

impl ApprovedDivisors {
    fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    fn remapped(&self, remap: &[Option<LogicNodeId>]) -> Self {
        let mut remapped = Self::default();
        for &(left, right, truth) in &self.definitions {
            let (Some(left), Some(right)) =
                (remap_literal(remap, left), remap_literal(remap, right))
            else {
                continue;
            };
            let truth = remap_pair_truth(truth, left, right);
            let (left, right) = if left.positive() <= right.positive() {
                (left.positive(), right.positive())
            } else {
                (right.positive(), left.positive())
            };
            let id = u32::try_from(remapped.definitions.len())
                .expect("approved divisor count is capped before remapping");
            remapped.definitions.push((left, right, truth));
            remapped
                .by_pair
                .entry((
                    u32::try_from(left.index())
                        .expect("logic node index is bounded by compact graph storage"),
                    u32::try_from(right.index())
                        .expect("logic node index is bounded by compact graph storage"),
                ))
                .or_default()
                .push((id, truth));
        }
        remapped
    }
}

#[cfg(test)]
mod tests;
