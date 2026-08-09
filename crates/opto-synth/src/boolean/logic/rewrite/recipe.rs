// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    AND_WEIGHT, ApprovedDivisors, CENSUS_DIVISOR_STORAGE, CutDatabase, DivisorRef, EXTRACTION_CAP,
    EXTRACTION_MARGIN, EXTRACTION_MIN_USES, ExecutionContext, HashMap, KCut, LogicGraph,
    LogicNodeId, MAX_CENSUS_DIVISORS, MAX_PLAN_NODES, MIN_DIVISOR_CUT_LEAVES, Plan,
    REWRITE_CUTS_PER_NODE, Synthesizer, TruthTable, WINDOW_CUT_LEAVES, XOR_WEIGHT, expand_truth,
    full_truth_mask,
};

pub(super) struct Decision {
    pub(super) cut: KCut,
    pub(super) divisors: Box<[DivisorRef]>,
    pub(super) plan: PlanRecipe,
}

pub(super) struct CandidateDecision {
    pub(super) cut: KCut,
    pub(super) divisors: Box<[DivisorRef]>,
    pub(super) plan: PlanRecipe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecipeNode {
    Constant(bool),
    Literal {
        var: u8,
        inverted: bool,
    },
    And(u16, u16),
    Or(u16, u16),
    Xor(u16, u16),
    Mux {
        select: u8,
        then_plan: u16,
        else_plan: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlanRecipe(pub(super) Box<[RecipeNode]>);

impl PlanRecipe {
    pub(super) fn from_plan(plan: &Plan) -> Option<Self> {
        fn encode(plan: &Plan, nodes: &mut Vec<RecipeNode>) -> Option<u16> {
            let node = match plan {
                Plan::Constant(value) => RecipeNode::Constant(*value),
                Plan::Literal { var, inverted } => RecipeNode::Literal {
                    var: *var,
                    inverted: *inverted,
                },
                Plan::And(left, right) => {
                    RecipeNode::And(encode(left, nodes)?, encode(right, nodes)?)
                }
                Plan::Or(left, right) => {
                    RecipeNode::Or(encode(left, nodes)?, encode(right, nodes)?)
                }
                Plan::Xor(left, right) => {
                    RecipeNode::Xor(encode(left, nodes)?, encode(right, nodes)?)
                }
                Plan::Mux {
                    select,
                    then_plan,
                    else_plan,
                } => RecipeNode::Mux {
                    select: *select,
                    then_plan: encode(then_plan, nodes)?,
                    else_plan: encode(else_plan, nodes)?,
                },
            };
            if nodes.len() == MAX_PLAN_NODES {
                return None;
            }
            let id = u16::try_from(nodes.len()).ok()?;
            nodes.push(node);
            Some(id)
        }

        let mut nodes = Vec::with_capacity(MAX_PLAN_NODES);
        encode(plan, &mut nodes)?;
        Some(Self(nodes.into_boxed_slice()))
    }

    pub(super) fn proves(&self, truth: TruthTable, care: u64, divisors: &[u64]) -> bool {
        let assignments = 1usize << truth.input_count;
        let mask = full_truth_mask(assignments);
        let literal = |var: u8| {
            let var = usize::from(var);
            if var < truth.input_count {
                Some((0..assignments).fold(0, |bits, assignment| {
                    bits | (((assignment >> var) & 1) as u64) << assignment
                }))
            } else {
                divisors.get(var - truth.input_count).copied()
            }
        };
        let mut values = [0_u64; MAX_PLAN_NODES];
        let mut value_count = 0usize;
        for &node in &self.0 {
            let prior = &values[..value_count];
            let value = match node {
                RecipeNode::Constant(value) => mask * u64::from(value),
                RecipeNode::Literal { var, inverted } => {
                    let Some(value) = literal(var) else {
                        return false;
                    };
                    if inverted { value ^ mask } else { value }
                }
                RecipeNode::And(left, right) => {
                    match (prior.get(usize::from(left)), prior.get(usize::from(right))) {
                        (Some(left), Some(right)) => left & right,
                        _ => return false,
                    }
                }
                RecipeNode::Or(left, right) => {
                    match (prior.get(usize::from(left)), prior.get(usize::from(right))) {
                        (Some(left), Some(right)) => left | right,
                        _ => return false,
                    }
                }
                RecipeNode::Xor(left, right) => {
                    match (prior.get(usize::from(left)), prior.get(usize::from(right))) {
                        (Some(left), Some(right)) => left ^ right,
                        _ => return false,
                    }
                }
                RecipeNode::Mux {
                    select,
                    then_plan,
                    else_plan,
                } => {
                    let Some(select) = literal(select) else {
                        return false;
                    };
                    match (
                        prior.get(usize::from(then_plan)),
                        prior.get(usize::from(else_plan)),
                    ) {
                        (Some(then_plan), Some(else_plan)) => {
                            (select & then_plan) | (!select & else_plan & mask)
                        }
                        _ => return false,
                    }
                }
            };
            values[value_count] = value;
            value_count += 1;
        }
        value_count != 0 && (values[value_count - 1] ^ truth.bits) & care & mask == 0
    }
}

pub(super) const PAIR_TRUTHS: [u8; 5] = [0b1000, 0b0100, 0b0010, 0b0001, 0b0110];

pub(super) fn remap_pair_truth(truth: u8, left: LogicNodeId, right: LogicNodeId) -> u8 {
    let swapped = left.positive() > right.positive();
    let mut remapped = 0u8;
    for assignment in 0..4usize {
        let (new_left, new_right) = if swapped {
            ((assignment >> 1) & 1, assignment & 1)
        } else {
            (assignment & 1, (assignment >> 1) & 1)
        };
        let old_left = new_left ^ usize::from(left.is_inverted());
        let old_right = new_right ^ usize::from(right.is_inverted());
        let old_assignment = old_left | (old_right << 1);
        remapped |= ((truth >> old_assignment) & 1) << assignment;
    }
    remapped
}

pub(super) fn build_pair_function(
    network: &mut LogicGraph,
    left: LogicNodeId,
    right: LogicNodeId,
    truth: u8,
) -> LogicNodeId {
    match truth {
        0b1000 => network.and(left, right),
        0b0010 => network.and(left, right.inverted()),
        0b0100 => network.and(left.inverted(), right),
        0b0001 => network.and(left.inverted(), right.inverted()),
        0b0110 => network.xor(left, right),
        0b1001 => network.xor(left, right).inverted(),
        _ => unreachable!("extraction truths cover exactly the single-gate pair functions"),
    }
}

fn pair_weight(truth: u8) -> u32 {
    if matches!(truth, 0b0110 | 0b1001) {
        XOR_WEIGHT
    } else {
        AND_WEIGHT
    }
}

pub(super) fn census_divisors(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    runtime: &ExecutionContext,
) -> Result<ApprovedDivisors, crate::SynthError> {
    let node_count = network.node_count();
    let references = network.reference_counts(roots);
    let cuts = CutDatabase::build_with_cut_cap_parallel(
        network,
        WINDOW_CUT_LEAVES,
        REWRITE_CUTS_PER_NODE,
        runtime,
    )?;
    let shards = runtime.fold_indexed(
        node_count,
        || (Synthesizer::fresh(), HashMap::new()),
        |(synthesizer, counters), index| {
            census_node(network, &cuts, &references, synthesizer, index, counters);
            Ok::<_, crate::SynthError>(())
        },
    )?;
    let mut counters = HashMap::new();
    for (_, shard) in shards {
        for (key, (score, uses)) in shard {
            let entry = counters.entry(key).or_insert((0i64, 0usize));
            entry.0 += score;
            entry.1 += uses;
        }
    }
    let mut ranked = counters
        .into_iter()
        .filter(|&(key, (score, uses))| {
            uses >= EXTRACTION_MIN_USES && score > i64::from(pair_weight(key.2)) * EXTRACTION_MARGIN
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|&(key, (score, _))| (std::cmp::Reverse(score), key));
    ranked.truncate(EXTRACTION_CAP);
    let mut approved = ApprovedDivisors::default();
    for (key, _) in ranked {
        let id = u32::try_from(approved.definitions.len())
            .expect("approved divisor count is capped before materialization");
        approved.definitions.push((
            LogicNodeId::from_index(key.0 as usize),
            LogicNodeId::from_index(key.1 as usize),
            key.2,
        ));
        approved
            .by_pair
            .entry((key.0, key.1))
            .or_default()
            .push((id, key.2));
    }
    Ok(approved)
}

fn census_node(
    network: &LogicGraph,
    cuts: &CutDatabase,
    references: &[u32],
    synthesizer: &mut Synthesizer,
    index: usize,
    counters: &mut HashMap<(u32, u32, u8), (i64, usize)>,
) {
    let node = LogicNodeId::from_index(index);
    if !network.node(node).is_gate() || references[index] == 0 {
        return;
    }
    for cut in cuts.cuts(node).iter().copied() {
        if cut.contains(node) || cut.len() < MIN_DIVISOR_CUT_LEAVES {
            continue;
        }
        let leaves = cut.leaves();
        let mut hypotheticals = smallvec::SmallVec::<[u64; CENSUS_DIVISOR_STORAGE]>::new();
        let mut keys = smallvec::SmallVec::<[(u32, u32, u8); CENSUS_DIVISOR_STORAGE]>::new();
        for first in 0..leaves.len() {
            for second in first + 1..leaves.len() {
                for &truth in &PAIR_TRUTHS {
                    let expanded = expand_truth(u64::from(truth), &[first, second], leaves.len());
                    hypotheticals.push(expanded);
                    keys.push((
                        u32::try_from(leaves[first].index())
                            .expect("logic node index is bounded by compact graph storage"),
                        u32::try_from(leaves[second].index())
                            .expect("logic node index is bounded by compact graph storage"),
                        truth,
                    ));
                }
            }
        }
        debug_assert!(hypotheticals.len() <= MAX_CENSUS_DIVISORS);
        let truth = network.truth_table_for_cut(node, cut);
        let (plain_cost, _) = synthesizer.plan(truth);
        let (cost, plan) = synthesizer.divisor_plan(truth, u64::MAX, &hypotheticals, 1);
        if cost >= plain_cost {
            continue;
        }
        let improvement = i64::from(plain_cost - cost);
        let mut used = used_divisor_mask(&plan, cut.len());
        while used != 0 {
            let hypothetical = used.trailing_zeros() as usize;
            used &= used - 1;
            let entry = counters.entry(keys[hypothetical]).or_insert((0i64, 0usize));
            entry.0 += improvement;
            entry.1 += 1;
        }
    }
}

fn used_divisor_mask(plan: &Plan, input_count: usize) -> u128 {
    match plan {
        Plan::Constant(_) => 0,
        Plan::Literal { var, .. } => {
            let var = usize::from(*var);
            if var < input_count {
                0
            } else {
                let index = var - input_count;
                debug_assert!(index < u128::BITS as usize);
                1 << index
            }
        }
        Plan::And(left, right) | Plan::Or(left, right) | Plan::Xor(left, right) => {
            used_divisor_mask(left, input_count) | used_divisor_mask(right, input_count)
        }
        Plan::Mux {
            then_plan,
            else_plan,
            ..
        } => used_divisor_mask(then_plan, input_count) | used_divisor_mask(else_plan, input_count),
    }
}
