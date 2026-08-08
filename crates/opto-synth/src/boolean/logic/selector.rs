// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::pipeline::{TransformAnalyses, TransformProduct};
use super::rewrite::remap_literal;
use hashbrown::HashMap;
use smallvec::SmallVec;

const MAX_SELECTS: usize = 5;
const MAX_INTERIOR: usize = 31;
const AND_WEIGHT: i64 = 2;
const MUX_WEIGHT: i64 = 6;
type Selects = SmallVec<[LogicNodeId; MAX_SELECTS]>;
type SelectKey = SmallVec<[u32; MAX_SELECTS]>;
type Fanins = SmallVec<[LogicNodeId; 3]>;

pub(crate) fn restructure(network: &LogicGraph, roots: &[LogicNodeId]) -> TransformProduct {
    let node_count = network.node_count();
    let references = network.reference_counts(roots);

    let mut claimed = vec![false; node_count];
    let mut trees = Vec::new();
    for index in (0..node_count).rev() {
        let node = LogicNodeId::from_index(index);
        if claimed[index]
            || references[index] == 0
            || !matches!(network.node(node), LogicNode::Mux { .. })
        {
            continue;
        }
        let Some(tree) = detect_tree(network, &references, node) else {
            continue;
        };
        for interior in &tree.interior {
            claimed[interior.index()] = true;
        }
        trees.push(tree);
    }

    let mut groups = HashMap::<SelectKey, Vec<usize>>::new();
    for (index, tree) in trees.iter().enumerate() {
        let key = tree
            .selects
            .iter()
            .map(|select| {
                u32::try_from(select.index())
                    .expect("logic node index is bounded by compact graph storage")
            })
            .collect::<SelectKey>();
        groups.entry(key).or_default().push(index);
    }

    let mut decisions = HashMap::<usize, SelectorPlan>::new();
    for members in groups.values() {
        let group_len = i64::try_from(members.len())
            .expect("selector group size is bounded by the logic graph");
        let mut decode_cases = 0u32;
        let mut indicator_sets = hashbrown::HashSet::<u32>::new();
        for &member in members {
            let tree = &trees[member];
            for &(_, mask) in &tree.leaves {
                decode_cases |= mask;
                indicator_sets.insert(mask);
            }
        }
        let selects = i64::try_from(trees[members[0]].selects.len())
            .expect("selector width is bounded by the logic graph");
        let decode_cost = i64::from(decode_cases.count_ones()) * (selects - 1).max(0) * AND_WEIGHT;
        let indicator_cost = indicator_sets
            .iter()
            .map(|mask| (i64::from(mask.count_ones()) - 1).max(0) * AND_WEIGHT)
            .sum::<i64>();
        let shared = (decode_cost + indicator_cost + group_len - 1) / group_len;
        for &member in members {
            let tree = &trees[member];
            let old = MUX_WEIGHT
                * i64::try_from(tree.interior.len())
                    .expect("selector tree size is bounded by the logic graph");
            let picked = i64::try_from(tree.leaves.len())
                .expect("selector leaf count is bounded by the logic graph");
            let new = picked * AND_WEIGHT + (picked - 1).max(0) * AND_WEIGHT + shared;
            if old > new {
                decisions.insert(
                    tree.root.index(),
                    SelectorPlan {
                        selects: tree.selects.clone(),
                        leaves: tree.leaves.clone(),
                    },
                );
            }
        }
    }

    materialize(network, &decisions, roots)
}

struct Tree {
    root: LogicNodeId,
    selects: Selects,
    interior: Vec<LogicNodeId>,
    leaves: Vec<(LogicNodeId, u32)>,
}

struct SelectorPlan {
    selects: Selects,
    leaves: Vec<(LogicNodeId, u32)>,
}

fn detect_tree(network: &LogicGraph, references: &[u32], root: LogicNodeId) -> Option<Tree> {
    let mut selects = Selects::new();
    let mut interior = Vec::new();
    let mut stack = vec![root.positive()];
    while let Some(node) = stack.pop() {
        let LogicNode::Mux { cond, .. } = network.node(node) else {
            return None;
        };
        let select = cond.positive();
        if !selects.contains(&select) {
            if selects.len() == MAX_SELECTS {
                continue;
            }
            selects.push(select);
        }
        interior.push(node);
        if interior.len() > MAX_INTERIOR {
            return None;
        }
        let LogicNode::Mux {
            then_value,
            else_value,
            ..
        } = network.node(node)
        else {
            unreachable!()
        };
        for child in [then_value, else_value] {
            if child.is_inverted() {
                continue;
            }
            let child = child.positive();
            if references[child.index()] != 1 {
                continue;
            }
            let LogicNode::Mux { cond, .. } = network.node(child) else {
                continue;
            };
            if selects.contains(&cond.positive()) || selects.len() < MAX_SELECTS {
                stack.push(child);
            }
        }
    }
    if interior.len() < 2 {
        return None;
    }
    selects.sort();
    let interior_set = interior
        .iter()
        .map(|node| node.index())
        .collect::<hashbrown::HashSet<_>>();
    if selects
        .iter()
        .any(|select| interior_set.contains(&select.index()))
    {
        return None;
    }
    let mut leaves = Vec::<(LogicNodeId, u32)>::new();
    for case in 0..1u32 << selects.len() {
        let mut current = root.positive();
        let leaf = loop {
            let LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } = network.node(current)
            else {
                break current;
            };
            if !interior_set.contains(&current.index()) {
                break current;
            }
            let position = selects
                .iter()
                .position(|select| *select == cond.positive())
                .expect("interior selects belong to the tree select set");
            let taken = ((case >> position) & 1 == 1) != cond.is_inverted();
            let child = if taken { then_value } else { else_value };
            if child.is_inverted() || !interior_set.contains(&child.index()) {
                break child;
            }
            current = child.positive();
        };
        match leaves.iter_mut().find(|(existing, _)| *existing == leaf) {
            Some((_, mask)) => *mask |= 1 << case,
            None => leaves.push((leaf, 1 << case)),
        }
    }
    Some(Tree {
        root: root.positive(),
        selects,
        interior,
        leaves,
    })
}

fn materialize(
    network: &LogicGraph,
    decisions: &HashMap<usize, SelectorPlan>,
    roots: &[LogicNodeId],
) -> TransformProduct {
    let mut rebuilt = LogicGraph::new();
    let mut remap: Vec<Option<LogicNodeId>> = vec![None; network.node_count()];
    let mut stack = Vec::new();
    for root in roots {
        stack.push((root.positive(), false));
        while let Some((node, expanded)) = stack.pop() {
            if remap[node.index()].is_some() {
                continue;
            }
            let stored = network.node(node);
            if !expanded {
                stack.push((node, true));
                if let Some(plan) = decisions.get(&node.index()) {
                    for leaf in plan.leaves.iter().rev() {
                        stack.push((leaf.0.positive(), false));
                    }
                    for select in plan.selects.iter().rev() {
                        stack.push((select.positive(), false));
                    }
                } else {
                    let mut fanins = stored.fanins().collect::<Fanins>();
                    fanins.reverse();
                    for fanin in fanins {
                        stack.push((fanin.positive(), false));
                    }
                }
                continue;
            }
            let mapped = match stored {
                LogicNode::Const(value) => {
                    debug_assert!(!value);
                    LogicGraph::constant(false)
                }
                LogicNode::Var(origin) => rebuilt
                    .variable(origin as usize)
                    .expect("rebuilt selector network stays within input capacity"),
                LogicNode::And(..) | LogicNode::Xor(..) | LogicNode::Mux { .. } => {
                    if let Some(plan) = decisions.get(&node.index()) {
                        build_selector(&mut rebuilt, plan, &remap)
                    } else {
                        let fanins = stored
                            .fanins()
                            .map(|fanin| {
                                remap_literal(&remap, fanin)
                                    .expect("copied fanins are materialized first")
                            })
                            .collect::<Fanins>();
                        match stored {
                            LogicNode::And(..) => rebuilt.and(fanins[0], fanins[1]),
                            LogicNode::Xor(..) => rebuilt.xor(fanins[0], fanins[1]),
                            LogicNode::Mux { .. } => rebuilt.mux(fanins[0], fanins[1], fanins[2]),
                            _ => unreachable!(),
                        }
                    }
                }
            };
            remap[node.index()] = Some(mapped);
        }
    }
    TransformProduct {
        network: rebuilt,
        remap: remap.into_boxed_slice(),
        analyses: TransformAnalyses::default(),
    }
}

fn build_selector(
    network: &mut LogicGraph,
    plan: &SelectorPlan,
    remap: &[Option<LogicNodeId>],
) -> LogicNodeId {
    let selects = plan
        .selects
        .iter()
        .map(|select| {
            remap_literal(remap, *select).expect("selector selects are materialized first")
        })
        .collect::<Selects>();
    let mut selected = LogicGraph::constant(false);
    for (leaf, mask) in &plan.leaves {
        let leaf = remap_literal(remap, *leaf).expect("selector leaves are materialized first");
        let mut indicator = LogicGraph::constant(false);
        for case in 0..1u32 << selects.len() {
            if mask & (1 << case) == 0 {
                continue;
            }
            let mut term = LogicGraph::constant(true);
            for (position, &select) in selects.iter().enumerate() {
                let literal = if (case >> position) & 1 == 1 {
                    select
                } else {
                    select.inverted()
                };
                term = network.and(term, literal);
            }
            indicator = network.or(indicator, term);
        }
        let value = network.and(indicator, leaf);
        selected = network.or(selected, value);
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boolean::logic::rewrite::optimize_network;

    fn gate_weight(network: &LogicGraph) -> u64 {
        (0..network.node_count())
            .map(|index| match network.node(LogicNodeId::from_index(index)) {
                LogicNode::Const(_) | LogicNode::Var(_) => 0,
                LogicNode::And(..) => 2u64,
                LogicNode::Xor(..) => 5,
                LogicNode::Mux { .. } => 6,
            })
            .sum()
    }

    #[test]
    fn collapses_wide_selection_with_repeated_data() {
        let mut network = LogicGraph::new();
        let s0 = network.variable(0).unwrap();
        let s1 = network.variable(1).unwrap();
        let s2 = network.variable(2).unwrap();
        let x = network.variable(3).unwrap();
        let y = network.variable(4).unwrap();
        let level0 = (0..4)
            .map(|case| if case == 3 { x } else { y })
            .collect::<Vec<_>>();
        let level1 = [
            network.mux(s0, level0[1], level0[0]),
            network.mux(s0, level0[3], level0[2]),
        ];
        let level2 = network.mux(s1, level1[1], level1[0]);
        let root = network.mux(s2, level2, y);
        network.freeze();
        let expected = network.truth_table(root, 5);
        let before = gate_weight(&network);

        let outcome = optimize_network(
            &network,
            &[root],
            &[None],
            crate::SynthesisDiagnostics::default(),
            crate::test_runtime(),
        )
        .unwrap();

        let mapped = remap_literal(&outcome.remap, root).unwrap();
        assert_eq!(outcome.network.truth_table(mapped, 5), expected);
        assert!(
            gate_weight(&outcome.network) < before,
            "one-hot restructuring shrinks the repeated-data selector, got {} vs {}",
            gate_weight(&outcome.network),
            before
        );
    }

    #[test]
    fn shares_decode_terms_across_bus_bits() {
        let mut network = LogicGraph::new();
        let s0 = network.variable(0).unwrap();
        let s1 = network.variable(1).unwrap();
        let mut roots = Vec::new();
        for bit in 0..2 {
            let a = network.variable(2 + bit * 2).unwrap();
            let b = network.variable(3 + bit * 2).unwrap();
            let low = network.mux(s0, a, b);
            let high = network.mux(s0, b, b);
            let root = network.mux(s1, high, low);
            roots.push(root);
        }
        network.freeze();
        let expected = roots
            .iter()
            .map(|&root| network.truth_table(root, 6))
            .collect::<Vec<_>>();

        let outcome = optimize_network(
            &network,
            &roots,
            &vec![None; roots.len()],
            crate::SynthesisDiagnostics::default(),
            crate::test_runtime(),
        )
        .unwrap();

        for (root, expected) in roots.iter().zip(expected) {
            let mapped = remap_literal(&outcome.remap, *root).unwrap();
            assert_eq!(outcome.network.truth_table(mapped, 6), expected);
        }
    }
}
