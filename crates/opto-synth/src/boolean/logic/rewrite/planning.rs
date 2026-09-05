// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    AND_WEIGHT, ApprovedDivisors, CandidateDecision, CutDatabase, DIVISOR_CAP, Decision,
    DivisorFunctions, DivisorRef, Divisors, KCut, LogicGraph, LogicNodeId, MAX_PLAN_NODES,
    MFFC_NODE_BUDGET, MUX_WEIGHT, MffcScratch, Plan, PlanInputs, PlanRecipe, RecipeNode,
    RewriteRecipeCache, RewriteRecipeKey, SupportIndex, Synthesizer, TimingBudget,
    WINDOW_CUT_LEAVES, XOR_WEIGHT, node_weight, window_cares,
};
use opto_ir::logic::{Lit, LogicProbe, StructuralIndex};

#[derive(Clone, Copy)]
pub(super) struct DecisionAnalysis<'a> {
    pub(super) network: &'a LogicGraph,
    pub(super) cuts: &'a CutDatabase,
    pub(super) support_index: &'a SupportIndex,
    pub(super) virtuals: &'a ApprovedDivisors,
    pub(super) references: &'a [u32],
    pub(super) timing: Option<&'a TimingBudget>,
    pub(super) active: Option<&'a [bool]>,
    pub(super) recipe_cache: &'a RewriteRecipeCache,
    pub(super) incremental_metrics: &'a crate::incremental::IncrementalRunMetrics,
    pub(super) check_incremental: bool,
    pub(super) structural: &'a StructuralIndex,
}

pub(super) fn decide_node(
    analysis: &DecisionAnalysis<'_>,
    mffc: &mut MffcScratch,
    synthesizer: &mut Synthesizer,
    index: usize,
) -> Result<Option<Decision>, crate::SynthError> {
    let DecisionAnalysis {
        network,
        cuts,
        support_index,
        virtuals,
        references,
        timing,
        active,
        recipe_cache,
        incremental_metrics,
        check_incremental,
        structural,
    } = *analysis;
    if active.is_some_and(|active| !active[index]) {
        return Ok(None);
    }
    let node = LogicNodeId::from_index(index);
    if !network.node(node).is_gate() || references[index] == 0 {
        return Ok(None);
    }
    let cares = window_cares(network, cuts, node);
    let level = |node| {
        timing.map_or_else(
            || network.level(node),
            |timing| timing.current(network, node),
        )
    };
    let mut best_score = None;
    let mut best = None;
    for (cut_index, cut) in cuts.cuts(node).iter().copied().enumerate() {
        if cut.contains(node) {
            continue;
        }
        let Some((available, dying)) = mffc_weight(network, references, node, cut, mffc) else {
            continue;
        };
        let truth = support_index.truth(node, cut_index);
        let care = cares.as_ref().map_or(u64::MAX, |cares| cares[cut_index]);
        let mut levels = [0; WINDOW_CUT_LEAVES + DIVISOR_CAP];
        for (slot, &leaf) in cut.leaves().iter().enumerate() {
            levels[slot] = level(leaf);
        }
        // Offer both bounded decompositions under one feasibility/area
        // ordering. Depth minimization alone would spend slack even when the
        // area recipe already meets the propagated requirement.
        let objectives = if timing.and_then(|budget| budget.required(node)).is_some() {
            2
        } else {
            1
        };
        for timing_directed in [false, true].into_iter().take(objectives) {
            let plain_key = if timing_directed {
                RewriteRecipeKey::timing(truth, &levels[..cut.len()])
            } else {
                RewriteRecipeKey::area(truth)
            };
            let plain = if let Some(cached) = recipe_cache.lookup(plain_key, incremental_metrics)? {
                if check_incremental {
                    let (cost, plan) = if timing_directed {
                        synthesizer.timing_plan(truth, &levels[..cut.len()])
                    } else {
                        synthesizer.plan(truth)
                    };
                    let cold = PlanRecipe::from_plan(&plan).map(|recipe| (cost, recipe));
                    if cold.as_ref() != Some(&cached) {
                        return Err(crate::SynthError::invariant(
                            "cached Boolean plain recipe differs from cold synthesis",
                        ));
                    }
                }
                Some(cached)
            } else {
                let (cost, plan) = if timing_directed {
                    synthesizer.timing_plan(truth, &levels[..cut.len()])
                } else {
                    synthesizer.plan(truth)
                };
                let recipe = PlanRecipe::from_plan(&plan);
                if let Some(recipe) = &recipe {
                    recipe_cache.insert(plain_key, cost, recipe)?;
                }
                recipe.map(|recipe| (cost, recipe))
            };
            if let Some((_, recipe)) = plain
                && recipe.proves(truth, u64::MAX, &[])
                && let Some(score) = proposal_score(
                    timing,
                    node,
                    RegionCost::removed(network, timing, node, available, dying.len()),
                    RegionCost::replacement_recipe(
                        added_cost(
                            &recipe,
                            cut.leaves(),
                            dying,
                            &mut LogicProbe::new(network.storage_network(), structural),
                        ),
                        &recipe,
                        &levels[..cut.len()],
                    ),
                )
                && best_score.is_none_or(|best| score > best)
            {
                best_score = Some(score);
                best = Some(CandidateDecision {
                    cut,
                    divisors: Box::default(),
                    plan: recipe,
                });
            }
        }
        let divisors = collect_divisors(support_index, virtuals, index, cut, dying);
        let assignments = 1usize << cut.len();
        let full = full_truth_mask(assignments);
        if divisors.is_empty() && care & full == full {
            continue;
        }
        let functions = divisors
            .iter()
            .map(|&(_, function)| function)
            .collect::<DivisorFunctions>();
        let divisor_key = RewriteRecipeKey::divisor(truth, care, &functions);
        let cached_divisor = recipe_cache.lookup(divisor_key, incremental_metrics)?;
        for (slot, &(divisor, _)) in divisors.iter().enumerate() {
            levels[cut.len() + slot] = match divisor {
                DivisorRef::Node(divisor) => level(divisor),
                DivisorRef::Virtual(id) => {
                    let (left, right, _) = virtuals.definitions[id as usize];
                    level(left).max(level(right)).saturating_add(1)
                }
            };
        }
        let divisor = if let Some(cached) = cached_divisor {
            if check_incremental {
                let (divisor_cost, divisor_plan) =
                    synthesizer.divisor_plan(truth, care, &functions, 0);
                let cold =
                    PlanRecipe::from_plan(&divisor_plan).map(|recipe| (divisor_cost, recipe));
                if cold.as_ref() != Some(&cached) {
                    return Err(crate::SynthError::invariant(
                        "cached Boolean divisor recipe differs from cold synthesis",
                    ));
                }
            }
            Some(cached)
        } else {
            let (divisor_cost, divisor_plan) = synthesizer.divisor_plan(truth, care, &functions, 0);
            let recipe = PlanRecipe::from_plan(&divisor_plan);
            if let Some(recipe) = &recipe {
                recipe_cache.insert(divisor_key, divisor_cost, recipe)?;
            }
            recipe.map(|recipe| (divisor_cost, recipe))
        };
        if let Some((cost, recipe)) = divisor
            && recipe.proves(truth, care, &functions)
            && let Some(score) = proposal_score(
                timing,
                node,
                RegionCost::removed(network, timing, node, available, dying.len()),
                RegionCost::replacement_recipe(
                    divisor_leaves(cut, &divisors).map_or(
                        (recipe_ops(&recipe), cost),
                        |leaves: PlanInputs| {
                            added_cost(
                                &recipe,
                                leaves.as_slice(),
                                dying,
                                &mut LogicProbe::new(network.storage_network(), structural),
                            )
                        },
                    ),
                    &recipe,
                    &levels[..cut.len() + divisors.len()],
                ),
            )
            && best_score.is_none_or(|best| score > best)
        {
            best_score = Some(score);
            best = Some(CandidateDecision {
                cut,
                divisors: divisors.iter().map(|&(divisor, _)| divisor).collect(),
                plan: recipe,
            });
        }
    }
    let Some(best) = best else {
        return Ok(None);
    };
    Ok(Some(Decision {
        cut: best.cut,
        divisors: best.divisors,
        plan: best.plan,
    }))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ProposalScore {
    critical_depth: i64,
    area: i64,
    gates: i64,
}

#[derive(Clone, Copy)]
pub(super) struct RegionCost {
    weight: u32,
    gates: u32,
    depth: u32,
}

/// Collects the recipe inputs of a divisor proposal as network literals.
///
/// A virtual divisor has no node yet, so its plan cannot be priced against the
/// network and the caller falls back to charging the whole recipe.
fn divisor_leaves(cut: KCut, divisors: &Divisors) -> Option<PlanInputs> {
    let mut leaves = cut.leaves().iter().copied().collect::<PlanInputs>();
    for &(divisor, _) in divisors {
        match divisor {
            DivisorRef::Node(node) => leaves.push(node),
            DivisorRef::Virtual(_) => return None,
        }
    }
    Some(leaves)
}

/// Counts the recipe operations that structural hashing would have to create,
/// and the weight they would add, given a network that already contains some of
/// them. Resolution runs through [`opto_ir::logic::LogicProbe`], so the answer
/// is the one materialization will produce rather than an estimate.
///
/// `dying` is the replaced region: those nodes are removed by the very rewrite
/// being priced, so a plan cannot reuse them. Counting them as present would
/// credit their removal and their reuse at the same time, which scores a
/// rewrite that keeps the region alive as if it had deleted it.
pub(super) fn added_cost(
    recipe: &PlanRecipe,
    leaves: &[LogicNodeId],
    dying: &[u32],
    probe: &mut LogicProbe<'_>,
) -> (u32, u32) {
    let mut values: Vec<Option<Lit>> = Vec::with_capacity(recipe.0.len());
    let (mut gates, mut weight) = (0, 0);
    for node in &recipe.0 {
        let resolved = match *node {
            RecipeNode::Constant(value) => Some(Ok(LogicGraph::constant(value).lit())),
            RecipeNode::Literal { var, inverted } => {
                let leaf = leaves[usize::from(var)];
                Some(Ok(if inverted { leaf.inverted() } else { leaf }.lit()))
            }
            RecipeNode::And(left, right) => values[usize::from(left)]
                .zip(values[usize::from(right)])
                .map(|(left, right)| probe.and(left, right)),
            RecipeNode::Or(left, right) => values[usize::from(left)]
                .zip(values[usize::from(right)])
                .map(|(left, right)| probe.or(left, right)),
            RecipeNode::Xor(left, right) => values[usize::from(left)]
                .zip(values[usize::from(right)])
                .map(|(left, right)| probe.xor(left, right)),
            RecipeNode::Mux {
                select,
                then_plan,
                else_plan,
            } => values[usize::from(then_plan)]
                .zip(values[usize::from(else_plan)])
                .map(|(then_value, else_value)| {
                    probe.mux(leaves[usize::from(select)].lit(), then_value, else_value)
                }),
        };
        let value = match resolved {
            Some(Ok(value))
                if u32::try_from(value.node().index())
                    .is_ok_and(|index| !dying.contains(&index)) =>
            {
                Some(value)
            }
            // Absent, dying, or built on one of those: materialization creates it.
            Some(_) | None => None,
        };
        if value.is_none() {
            gates += 1;
            weight += match node {
                RecipeNode::Constant(_) | RecipeNode::Literal { .. } => 0,
                RecipeNode::And(..) | RecipeNode::Or(..) => AND_WEIGHT,
                RecipeNode::Xor(..) => XOR_WEIGHT,
                RecipeNode::Mux { .. } => MUX_WEIGHT,
            };
        }
        values.push(value);
    }
    (gates, weight)
}

impl RegionCost {
    fn removed(
        network: &LogicGraph,
        timing: Option<&TimingBudget>,
        node: LogicNodeId,
        weight: u32,
        gates: usize,
    ) -> Self {
        Self {
            weight,
            gates: gates
                .try_into()
                .expect("MFFC region budget fits the compact gate count"),
            depth: timing.map_or_else(
                || network.level(node),
                |timing| timing.current(network, node),
            ),
        }
    }

    /// Prices a replacement by the nodes materialization would actually add.
    ///
    /// `new` is the gate count and weight of the recipe operations the network
    /// does not already contain; structural hashing gives the rest away, so
    /// charging for them would reject rewrites that cost nothing.
    fn replacement_recipe(new: (u32, u32), plan: &PlanRecipe, inputs: &[u32]) -> Self {
        let (gates, weight) = new;
        Self {
            weight,
            gates,
            depth: recipe_level(plan, inputs),
        }
    }
}

fn recipe_level(recipe: &PlanRecipe, inputs: &[u32]) -> u32 {
    let mut levels = [0; MAX_PLAN_NODES];
    for (index, node) in recipe.0.iter().copied().enumerate() {
        levels[index] = match node {
            RecipeNode::Constant(_) => 0,
            RecipeNode::Literal { var, .. } => inputs[usize::from(var)],
            RecipeNode::And(left, right)
            | RecipeNode::Or(left, right)
            | RecipeNode::Xor(left, right) => levels[usize::from(left)]
                .max(levels[usize::from(right)])
                .saturating_add(1),
            RecipeNode::Mux {
                select,
                then_plan,
                else_plan,
            } => inputs[usize::from(select)]
                .max(levels[usize::from(then_plan)])
                .max(levels[usize::from(else_plan)])
                .saturating_add(1),
        };
    }
    recipe.0.len().checked_sub(1).map_or(0, |last| levels[last])
}

fn recipe_ops(recipe: &PlanRecipe) -> u32 {
    u32::try_from(
        recipe
            .0
            .iter()
            .filter(|node| {
                matches!(
                    node,
                    RecipeNode::And(..)
                        | RecipeNode::Or(..)
                        | RecipeNode::Xor(..)
                        | RecipeNode::Mux { .. }
                )
            })
            .count(),
    )
    .expect("rewrite recipe length is compact")
}

pub(super) fn proposal_score(
    timing: Option<&TimingBudget>,
    node: LogicNodeId,
    removed: RegionCost,
    replacement: RegionCost,
) -> Option<ProposalScore> {
    let area_gain = i64::from(removed.weight) - i64::from(replacement.weight);
    let gate_gain = i64::from(removed.gates) - i64::from(replacement.gates);
    let critical_depth_gain =
        if let Some(required) = timing.and_then(|timing| timing.required(node)) {
            let removed_violation = removed.depth.saturating_sub(required);
            let replacement_violation = replacement.depth.saturating_sub(required);
            if replacement_violation > removed_violation {
                return None;
            }
            i64::from(removed_violation) - i64::from(replacement_violation)
        } else {
            0
        };
    if timing.is_some_and(|timing| timing.violation(node) > 0)
        && critical_depth_gain > 0
        && (area_gain < 0 || gate_gain < 0)
    {
        return None;
    }
    let score = ProposalScore {
        critical_depth: critical_depth_gain,
        area: area_gain,
        gates: gate_gain,
    };
    (critical_depth_gain > 0 || (area_gain, gate_gain) > (0, 0)).then_some(score)
}

pub(super) fn plan_level(plan: &Plan, inputs: &[u32]) -> u32 {
    match plan {
        Plan::Constant(_) => 0,
        Plan::Literal { var, .. } => inputs[usize::from(*var)],
        Plan::And(left, right) | Plan::Or(left, right) | Plan::Xor(left, right) => {
            plan_level(left, inputs)
                .max(plan_level(right, inputs))
                .saturating_add(1)
        }
        Plan::Mux {
            select,
            then_plan,
            else_plan,
        } => inputs[usize::from(*select)]
            .max(plan_level(then_plan, inputs))
            .max(plan_level(else_plan, inputs))
            .saturating_add(1),
    }
}

pub(super) fn full_truth_mask(assignments: usize) -> u64 {
    if assignments == u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << assignments) - 1
    }
}

pub(super) fn collect_divisors(
    support_index: &SupportIndex,
    virtuals: &ApprovedDivisors,
    target: usize,
    cut: KCut,
    dying: &[u32],
) -> Divisors {
    let leaves = cut.leaves();
    let mut divisors = Divisors::new();
    // Per-leaf fingerprints are computed once and combined per subset, so the
    // negative filter costs one XOR chain and one bit test instead of building
    // and hashing a full support key.
    let mut leaf_indices = [0u32; WINDOW_CUT_LEAVES];
    let mut leaf_fingerprints = [0u64; WINDOW_CUT_LEAVES];
    for (slot, leaf) in leaves.iter().enumerate() {
        let index = u32::try_from(leaf.index())
            .expect("logic node index is bounded by compact graph storage");
        leaf_indices[slot] = index;
        leaf_fingerprints[slot] = super::support::leaf_fingerprint(index);
    }
    let pairs_wanted = !virtuals.is_empty();
    for subset in 1u32..1 << leaves.len() {
        if subset.count_ones() < 2 {
            continue;
        }
        let mut fingerprint = 0u64;
        let mut key = [0u32; WINDOW_CUT_LEAVES];
        let mut position_count = 0;
        let mut remaining = subset;
        while remaining != 0 {
            let position = remaining.trailing_zeros() as usize;
            remaining &= remaining - 1;
            key[position_count] = leaf_indices[position];
            fingerprint ^= leaf_fingerprints[position];
            position_count += 1;
        }
        if pairs_wanted && position_count == 2 {
            let pair = (key[0], key[1]);
            if let Some(entries) = virtuals.by_pair.get(&pair) {
                for &(id, truth) in entries {
                    let expanded = expand_truth_for_subset(u64::from(truth), subset, leaves.len());
                    if !insert_divisor(&mut divisors, DivisorRef::Virtual(id), expanded) {
                        return divisors;
                    }
                }
            }
        }
        if !support_index.may_contain(fingerprint) {
            continue;
        }
        for &(divisor, function) in support_index.entries(&key[..position_count]) {
            if divisor as usize >= target || dying.contains(&divisor) {
                continue;
            }
            let expanded = expand_truth_for_subset(function, subset, leaves.len());
            if !insert_divisor(
                &mut divisors,
                DivisorRef::Node(LogicNodeId::from_index(divisor as usize)),
                expanded,
            ) {
                return divisors;
            }
        }
    }
    divisors
}

/// Adds one distinct divisor function, reporting whether collection may continue.
///
/// Duplicate functions are rejected by scanning the retained divisors instead of
/// by a hash set: the cap is sixteen, so the scan is a handful of register
/// comparisons, while a set costs one allocation per call and this runs once per
/// cut of every node of every pass.
pub(super) fn insert_divisor(divisors: &mut Divisors, divisor: DivisorRef, function: u64) -> bool {
    if divisors.len() == DIVISOR_CAP {
        return false;
    }
    if !divisors.iter().any(|&(_, seen)| seen == function) {
        divisors.push((divisor, function));
    }
    divisors.len() < DIVISOR_CAP
}

static TRUTH_PROJECTIONS: [[[u8; 64]; 64]; WINDOW_CUT_LEAVES + 1] = truth_projections();

#[allow(
    clippy::large_stack_arrays,
    clippy::cast_possible_truncation,
    reason = "the const projection table is bounded to an eight-input truth window"
)]
const fn truth_projections() -> [[[u8; 64]; 64]; WINDOW_CUT_LEAVES + 1] {
    let mut projections = [[[0u8; 64]; 64]; WINDOW_CUT_LEAVES + 1];
    let mut var_count = 0;
    while var_count <= WINDOW_CUT_LEAVES {
        let mut subset = 0;
        while subset < 1usize << var_count {
            let mut assignment = 0;
            while assignment < 1usize << var_count {
                let mut projected = 0u8;
                let mut source_bit = 0;
                let mut position = 0;
                while position < var_count {
                    if subset & (1 << position) != 0 {
                        projected |= (((assignment >> position) & 1) << source_bit) as u8;
                        source_bit += 1;
                    }
                    position += 1;
                }
                projections[var_count][subset][assignment] = projected;
                assignment += 1;
            }
            subset += 1;
        }
        var_count += 1;
    }
    projections
}

pub(super) fn expand_truth(bits: u64, positions: &[usize], var_count: usize) -> u64 {
    let mut subset = 0u32;
    for &position in positions {
        subset |= 1 << position;
    }
    expand_truth_for_subset(bits, subset, var_count)
}

pub(super) fn expand_truth_for_subset(bits: u64, subset: u32, var_count: usize) -> u64 {
    let mut expanded = 0u64;
    let projection = &TRUTH_PROJECTIONS[var_count][subset as usize];
    for (assignment, &projected) in projection.iter().enumerate().take(1usize << var_count) {
        expanded |= ((bits >> projected) & 1) << assignment;
    }
    expanded
}

pub(super) fn mffc_weight<'scratch>(
    network: &LogicGraph,
    references: &[u32],
    root: LogicNodeId,
    cut: KCut,
    scratch: &'scratch mut MffcScratch,
) -> Option<(u32, &'scratch [u32])> {
    scratch.begin(root);
    let mut weight = node_weight(network.node(root));
    while let Some(current) = scratch.stack.pop() {
        for fanin in network.node(current).fanins() {
            let fanin = fanin.positive();
            if cut.contains(fanin) {
                continue;
            }
            let node = network.node(fanin);
            if !node.is_gate() {
                continue;
            }
            if scratch.decrement(fanin, references)? {
                if scratch.dying.len() == MFFC_NODE_BUDGET {
                    return None;
                }
                weight += node_weight(node);
                scratch.dying.push(
                    u32::try_from(fanin.index())
                        .expect("logic node index is bounded by compact graph storage"),
                );
                scratch.stack.push(fanin);
            }
        }
    }
    Some((weight, &scratch.dying))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_can_reduce_a_multi_stage_timing_deficit() {
        let node = LogicNodeId::from_index(1);
        let timing = TimingBudget {
            arrivals: Box::new([0, 5]),
            required: Box::new([u32::MAX, 1]),
        };
        let score = proposal_score(
            Some(&timing),
            node,
            RegionCost {
                weight: 4,
                gates: 1,
                depth: 5,
            },
            RegionCost {
                weight: 4,
                gates: 1,
                depth: 3,
            },
        )
        .expect("a rewrite that reduces violation is useful before full closure");

        assert_eq!(score.critical_depth, 2);
    }

    #[test]
    fn timing_progress_does_not_expand_a_local_region() {
        let node = LogicNodeId::from_index(1);
        let timing = TimingBudget {
            arrivals: Box::new([0, 5]),
            required: Box::new([u32::MAX, 1]),
        };

        assert!(
            proposal_score(
                Some(&timing),
                node,
                RegionCost {
                    weight: 4,
                    gates: 1,
                    depth: 5,
                },
                RegionCost {
                    weight: 6,
                    gates: 2,
                    depth: 3,
                },
            )
            .is_none()
        );
    }

    #[test]
    fn feasible_rewrite_recovers_area_within_the_actual_budget() {
        let mut network = LogicGraph::new();
        let left = network.variable(0).unwrap();
        let right = network.variable(1).unwrap();
        let node = network.and(left, right);
        network.freeze();
        let mut arrivals = vec![0; network.node_count()];
        arrivals[node.index()] = 2;
        let mut required = vec![u32::MAX; network.node_count()];
        required[node.index()] = 5;
        let timing = TimingBudget {
            arrivals: arrivals.into_boxed_slice(),
            required: required.into_boxed_slice(),
        };
        let removed = RegionCost::removed(&network, Some(&timing), node, 4, 2);

        assert_eq!(removed.depth, 2);
        assert!(
            proposal_score(
                Some(&timing),
                node,
                removed,
                RegionCost {
                    weight: 2,
                    gates: 1,
                    depth: 3,
                },
            )
            .is_some()
        );
        assert!(
            proposal_score(
                Some(&timing),
                node,
                removed,
                RegionCost {
                    weight: 2,
                    gates: 1,
                    depth: 6
                },
            )
            .is_none()
        );
        assert!(
            proposal_score(
                Some(&timing),
                node,
                removed,
                RegionCost {
                    weight: 5,
                    gates: 2,
                    depth: 1
                },
            )
            .is_none()
        );
    }
}
