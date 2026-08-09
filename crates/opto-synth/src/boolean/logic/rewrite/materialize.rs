// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    ApprovedDivisors, Decision, DivisorRef, LogicGraph, LogicNode, LogicNodeId, PlanInputs,
    PlanRecipe, RecipeNode, build_pair_function, remap_literal,
};
use crate::boolean::logic::pipeline::{TransformAnalyses, TransformProduct};

pub(super) fn materialize(
    network: &LogicGraph,
    decisions: &[Option<Decision>],
    virtuals: &ApprovedDivisors,
    roots: &[LogicNodeId],
) -> TransformProduct {
    let mut reachable = vec![false; network.node_count()];
    let mut stack = roots.iter().map(|root| root.positive()).collect::<Vec<_>>();
    while let Some(node) = stack.pop() {
        if std::mem::replace(&mut reachable[node.index()], true) {
            continue;
        }
        match decisions[node.index()].as_ref() {
            Some(decision) => {
                for divisor in &decision.divisors {
                    match *divisor {
                        DivisorRef::Node(divisor) => stack.push(divisor.positive()),
                        DivisorRef::Virtual(id) => {
                            let (left, right, _) = virtuals.definitions[id as usize];
                            stack.push(left.positive());
                            stack.push(right.positive());
                        }
                    }
                }
                stack.extend(decision.cut.leaves().iter().map(|leaf| leaf.positive()));
            }
            None => stack.extend(
                network
                    .node(node)
                    .fanins()
                    .map(super::super::network::LogicNodeId::positive),
            ),
        }
    }

    let mut rebuilt = LogicGraph::new();
    let mut remap: Vec<Option<LogicNodeId>> = vec![None; network.node_count()];
    let mut plan_values = Vec::new();
    for (index, &is_reachable) in reachable.iter().enumerate() {
        if !is_reachable {
            continue;
        }
        let node = LogicNodeId::from_index(index);
        let stored = network.node(node);
        let mapped = match stored {
            LogicNode::Const(value) => {
                debug_assert!(!value);
                LogicGraph::constant(false)
            }
            LogicNode::Var(origin) => rebuilt
                .variable(origin as usize)
                .expect("rebuilt logic network stays within input capacity"),
            LogicNode::And(..) | LogicNode::Xor(..) | LogicNode::Mux { .. } => {
                if let Some(decision) = decisions[index].as_ref() {
                    let mut leaves = decision
                        .cut
                        .leaves()
                        .iter()
                        .map(|leaf| {
                            remap_literal(&remap, *leaf)
                                .expect("rewrite cut leaves are materialized first")
                        })
                        .collect::<PlanInputs>();
                    for divisor in &decision.divisors {
                        let mapped = match *divisor {
                            DivisorRef::Node(node) => remap_literal(&remap, node)
                                .expect("rewrite divisors are materialized first"),
                            DivisorRef::Virtual(id) => {
                                let (left, right, truth) = virtuals.definitions[id as usize];
                                let left = remap_literal(&remap, left)
                                    .expect("virtual divisor leaves are materialized");
                                let right = remap_literal(&remap, right)
                                    .expect("virtual divisor leaves are materialized");
                                build_pair_function(&mut rebuilt, left, right, truth)
                            }
                        };
                        leaves.push(mapped);
                    }
                    build_plan(&mut rebuilt, &decision.plan, &leaves, &mut plan_values)
                } else {
                    let fanins = stored
                        .fanins()
                        .map(|fanin| {
                            remap_literal(&remap, fanin)
                                .expect("copied fanins are materialized first")
                        })
                        .collect::<smallvec::SmallVec<[LogicNodeId; 3]>>();
                    match stored {
                        LogicNode::And(..) => rebuilt.and(fanins[0], fanins[1]),
                        LogicNode::Xor(..) => rebuilt.xor(fanins[0], fanins[1]),
                        LogicNode::Mux { .. } => rebuilt.mux(fanins[0], fanins[1], fanins[2]),
                        _ => unreachable!(),
                    }
                }
            }
        };
        remap[index] = Some(mapped);
    }
    TransformProduct {
        network: rebuilt,
        remap: remap.into_boxed_slice(),
        analyses: TransformAnalyses::default(),
    }
}

pub(super) fn is_exact_copy(
    old_network: &LogicGraph,
    new_network: &LogicGraph,
    old: usize,
    remap: &[Option<LogicNodeId>],
) -> bool {
    let Some(mapped) = remap[old] else {
        return false;
    };
    if mapped.is_inverted() {
        return false;
    }
    let map = |literal| remap_literal(remap, literal);
    match (
        old_network.node(LogicNodeId::from_index(old)),
        new_network.node(mapped),
    ) {
        (LogicNode::Const(left), LogicNode::Const(right)) => left == right,
        (LogicNode::Var(left), LogicNode::Var(right)) => left == right,
        (LogicNode::And(left_a, left_b), LogicNode::And(right_a, right_b))
        | (LogicNode::Xor(left_a, left_b), LogicNode::Xor(right_a, right_b)) => {
            let (Some(left_a), Some(left_b)) = (map(left_a), map(left_b)) else {
                return false;
            };
            (left_a == right_a && left_b == right_b) || (left_a == right_b && left_b == right_a)
        }
        (
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            },
            LogicNode::Mux {
                cond: new_cond,
                then_value: new_then,
                else_value: new_else,
            },
        ) => {
            let (Some(cond), Some(then_value), Some(else_value)) =
                (map(cond), map(then_value), map(else_value))
            else {
                return false;
            };
            (cond == new_cond && then_value == new_then && else_value == new_else)
                || (cond.inverted() == new_cond && else_value == new_then && then_value == new_else)
        }
        _ => false,
    }
}

fn build_plan(
    network: &mut LogicGraph,
    plan: &PlanRecipe,
    leaves: &[LogicNodeId],
    values: &mut Vec<LogicNodeId>,
) -> LogicNodeId {
    values.clear();
    for &node in &plan.0 {
        let value = match node {
            RecipeNode::Constant(value) => LogicGraph::constant(value),
            RecipeNode::Literal { var, inverted } => {
                let leaf = leaves[usize::from(var)];
                if inverted { leaf.inverted() } else { leaf }
            }
            RecipeNode::And(left, right) => {
                network.and(values[usize::from(left)], values[usize::from(right)])
            }
            RecipeNode::Or(left, right) => {
                network.or(values[usize::from(left)], values[usize::from(right)])
            }
            RecipeNode::Xor(left, right) => {
                network.xor(values[usize::from(left)], values[usize::from(right)])
            }
            RecipeNode::Mux {
                select,
                then_plan,
                else_plan,
            } => network.mux(
                leaves[usize::from(select)],
                values[usize::from(then_plan)],
                values[usize::from(else_plan)],
            ),
        };
        values.push(value);
    }
    *values.last().expect("rewrite recipes are never empty")
}
