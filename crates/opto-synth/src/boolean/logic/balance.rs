// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::pipeline::{TransformAnalyses, TransformProduct};
use super::rewrite::remap_literal;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub(crate) fn balance(network: &LogicGraph, roots: &[LogicNodeId]) -> TransformProduct {
    let node_count = network.node_count();
    let references = network.reference_counts(roots);

    let mut absorbed = vec![false; node_count];
    for index in 0..node_count {
        let id = LogicNodeId::from_index(index);
        match network.node(id) {
            LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                for fanin in [left, right] {
                    if chains_into(network, id, fanin, &references) {
                        absorbed[fanin.index()] = true;
                    }
                }
            }
            LogicNode::Mux { .. } => {
                if let Some((_, next)) = mux_chain_link(network, id, &references) {
                    absorbed[next.index()] = true;
                }
            }
            _ => {}
        }
    }

    let mut builder = Builder {
        graph: LogicGraph::new(),
        levels: vec![0],
    };
    let mut remap: Vec<Option<LogicNodeId>> = vec![None; node_count];
    let mut operands = Vec::new();
    let mut stack = Vec::new();
    for index in 0..node_count {
        if absorbed[index] {
            continue;
        }
        let id = LogicNodeId::from_index(index);
        let new = match network.node(id) {
            LogicNode::Const(value) => Some(builder.constant(value)),
            LogicNode::Var(origin) => builder.variable(origin as usize),
            LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                operands.clear();
                stack.push(left);
                stack.push(right);
                let mut translated = Some(());
                while let Some(fanin) = stack.pop() {
                    if chains_into(network, id, fanin, &references) {
                        let (LogicNode::And(a, b) | LogicNode::Xor(a, b)) = network.node(fanin)
                        else {
                            unreachable!("chain interiors are binary gates");
                        };
                        stack.push(a);
                        stack.push(b);
                    } else if let Some(operand) = remap_literal(&remap, fanin) {
                        operands.push(operand);
                    } else {
                        translated = None;
                    }
                }
                let gate = if matches!(network.node(id), LogicNode::Xor(..)) {
                    Gate::Xor
                } else {
                    Gate::And
                };
                translated.and_then(|()| build_tree(&mut builder, operands.drain(..), gate))
            }
            LogicNode::Mux { .. } => {
                let mut links = Vec::new();
                let mut current = id;
                let default = loop {
                    let LogicNode::Mux {
                        cond,
                        then_value,
                        else_value,
                    } = network.node(current)
                    else {
                        unreachable!("mux chains only traverse mux nodes");
                    };
                    if let Some((link, next)) = mux_chain_link(network, current, &references) {
                        links.push(link);
                        current = next;
                    } else {
                        links.push((cond, then_value));
                        break else_value;
                    }
                };
                let links = links
                    .into_iter()
                    .map(|(select, data)| {
                        Some((remap_literal(&remap, select)?, remap_literal(&remap, data)?))
                    })
                    .collect::<Option<Vec<_>>>();
                match (links, remap_literal(&remap, default)) {
                    (Some(links), Some(default)) => {
                        Some(build_priority(&mut builder, &links, default))
                    }
                    _ => None,
                }
            }
        };
        remap[index] = new;
    }

    let mut outcome = TransformProduct {
        network: builder.graph,
        remap: remap.into_boxed_slice(),
        analyses: TransformAnalyses::default(),
    };
    outcome.network.freeze();
    outcome
}

fn mux_chain_link(
    network: &LogicGraph,
    id: LogicNodeId,
    references: &[u32],
) -> Option<((LogicNodeId, LogicNodeId), LogicNodeId)> {
    let LogicNode::Mux {
        cond,
        then_value,
        else_value,
    } = network.node(id)
    else {
        return None;
    };
    let continues = |arm: LogicNodeId| {
        !arm.is_inverted()
            && references[arm.index()] == 1
            && matches!(network.node(arm), LogicNode::Mux { .. })
    };
    if continues(else_value) {
        Some(((cond, then_value), else_value))
    } else if continues(then_value) {
        Some(((cond.inverted(), else_value), then_value))
    } else {
        None
    }
}

fn build_priority(
    builder: &mut Builder,
    links: &[(LogicNodeId, LogicNodeId)],
    default: LogicNodeId,
) -> LogicNodeId {
    match links {
        [] => default,
        [(select, data)] => builder.mux(*select, *data, default),
        _ if links.len() > PRIORITY_SPLIT_CAP => {
            let (select, data) = links[0];
            let tail = build_priority(builder, &links[1..], default);
            builder.mux(select, data, tail)
        }
        _ => {
            let (peel, split) = priority_estimates(builder, links, builder.level(default));
            if split < peel {
                let middle = links.len().div_ceil(2);
                let guard = build_tree(
                    builder,
                    links[..middle].iter().map(|&(select, _)| select),
                    Gate::Or,
                )
                .expect("priority splits guard at least two selects");
                let top_default = links[middle - 1].1;
                let top = build_priority(builder, &links[..middle - 1], top_default);
                let bottom = build_priority(builder, &links[middle..], default);
                builder.mux(guard, top, bottom)
            } else {
                let (select, data) = links[0];
                let tail = build_priority(builder, &links[1..], default);
                builder.mux(select, data, tail)
            }
        }
    }
}

fn priority_estimates(
    builder: &Builder,
    links: &[(LogicNodeId, LogicNodeId)],
    default_level: u32,
) -> (u32, u32) {
    let (first_select, first_data) = links[0];
    let peel = builder
        .level(first_select)
        .max(builder.level(first_data))
        .max(priority_level_estimate(builder, &links[1..], default_level))
        + 1;
    let middle = links.len().div_ceil(2);
    let guard = tree_level_estimate(
        links[..middle]
            .iter()
            .map(|&(select, _)| builder.level(select)),
    );
    let top = priority_level_estimate(
        builder,
        &links[..middle - 1],
        builder.level(links[middle - 1].1),
    );
    let bottom = priority_level_estimate(builder, &links[middle..], default_level);
    let split = guard.max(top).max(bottom) + 1;
    (peel, split)
}

const PRIORITY_SPLIT_CAP: usize = 64;

fn priority_level_estimate(
    builder: &Builder,
    links: &[(LogicNodeId, LogicNodeId)],
    default_level: u32,
) -> u32 {
    match links {
        [] => default_level,
        [(select, data)] => {
            builder
                .level(*select)
                .max(builder.level(*data))
                .max(default_level)
                + 1
        }
        _ if links.len() > PRIORITY_SPLIT_CAP => {
            links
                .iter()
                .rev()
                .fold(default_level, |accumulated, &(select, data)| {
                    builder
                        .level(select)
                        .max(builder.level(data))
                        .max(accumulated)
                        + 1
                })
        }
        _ => {
            let (peel, split) = priority_estimates(builder, links, default_level);
            peel.min(split)
        }
    }
}

fn tree_level_estimate(levels: impl IntoIterator<Item = u32>) -> u32 {
    let mut heap: BinaryHeap<Reverse<u32>> = levels.into_iter().map(Reverse).collect();
    while heap.len() > 1 {
        let Reverse(left) = heap.pop().expect("tree estimate holds two operands");
        let Reverse(right) = heap.pop().expect("tree estimate holds two operands");
        heap.push(Reverse(left.max(right) + 1));
    }
    heap.pop().map_or(0, |Reverse(level)| level)
}

fn chains_into(
    network: &LogicGraph,
    consumer: LogicNodeId,
    fanin: LogicNodeId,
    references: &[u32],
) -> bool {
    if fanin.is_inverted() || references[fanin.index()] != 1 {
        return false;
    }
    matches!(
        (network.node(consumer), network.node(fanin)),
        (LogicNode::And(..), LogicNode::And(..)) | (LogicNode::Xor(..), LogicNode::Xor(..))
    )
}

#[derive(Clone, Copy)]
enum Gate {
    And,
    Xor,
    Or,
}

fn build_tree(
    builder: &mut Builder,
    operands: impl IntoIterator<Item = LogicNodeId>,
    gate: Gate,
) -> Option<LogicNodeId> {
    let mut store = Vec::new();
    let mut heap = BinaryHeap::new();
    for operand in operands {
        heap.push(Reverse((builder.level(operand), store.len())));
        store.push(operand);
    }
    while heap.len() > 1 {
        let Reverse((_, left)) = heap.pop().expect("heap holds at least two operands");
        let Reverse((_, right)) = heap.pop().expect("heap holds at least two operands");
        let combined = match gate {
            Gate::And => builder.and(store[left], store[right]),
            Gate::Xor => builder.xor(store[left], store[right]),
            Gate::Or => builder.or(store[left], store[right]),
        };
        heap.push(Reverse((builder.level(combined), store.len())));
        store.push(combined);
    }
    heap.pop().map(|Reverse((_, slot))| store[slot])
}

struct Builder {
    graph: LogicGraph,
    levels: Vec<u32>,
}

impl Builder {
    fn level(&self, literal: LogicNodeId) -> u32 {
        self.levels.get(literal.index()).copied().unwrap_or(0)
    }

    fn record(&mut self, literal: LogicNodeId, level: u32) -> LogicNodeId {
        while literal.index() >= self.levels.len() {
            self.levels.push(level);
        }
        literal
    }

    fn constant(&mut self, value: bool) -> LogicNodeId {
        let literal = LogicGraph::constant(value);
        self.record(literal, 0)
    }

    fn variable(&mut self, origin: usize) -> Option<LogicNodeId> {
        let literal = self.graph.variable(origin)?;
        Some(self.record(literal, 0))
    }

    fn and(&mut self, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
        let level = self.level(left).max(self.level(right)) + 1;
        let literal = self.graph.and(left, right);
        self.record(literal, level)
    }

    fn xor(&mut self, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
        let level = self.level(left).max(self.level(right)) + 1;
        let literal = self.graph.xor(left, right);
        self.record(literal, level)
    }

    fn or(&mut self, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
        let level = self.level(left).max(self.level(right)) + 1;
        let literal = self.graph.or(left, right);
        self.record(literal, level)
    }

    fn mux(
        &mut self,
        cond: LogicNodeId,
        then_value: LogicNodeId,
        else_value: LogicNodeId,
    ) -> LogicNodeId {
        let level = self
            .level(cond)
            .max(self.level(then_value))
            .max(self.level(else_value))
            + 1;
        let literal = self.graph.mux(cond, then_value, else_value);
        self.record(literal, level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(
        graph: &mut LogicGraph,
        count: usize,
        gate: impl Fn(&mut LogicGraph, LogicNodeId, LogicNodeId) -> LogicNodeId,
    ) -> LogicNodeId {
        let mut result = graph.variable(0).unwrap();
        for origin in 1..count {
            let variable = graph.variable(origin).unwrap();
            result = gate(graph, result, variable);
        }
        result
    }

    #[test]
    fn balances_a_single_fanout_and_chain_to_logarithmic_depth() {
        let mut graph = LogicGraph::new();
        let root = chain(&mut graph, 6, LogicGraph::and);
        graph.freeze();
        let expected = graph.truth_table(root, 6);
        let outcome = balance(&graph, &[root]);
        let balanced = remap_literal(&outcome.remap, root).unwrap();
        assert_eq!(outcome.network.truth_table(balanced, 6), expected);
        assert_eq!(outcome.network.level(balanced), 3);
    }

    #[test]
    fn balances_or_and_xor_chains_and_preserves_their_functions() {
        let mut graph = LogicGraph::new();
        let or_root = chain(&mut graph, 6, LogicGraph::or);
        let xor_root = chain(&mut graph, 6, LogicGraph::xor);
        graph.freeze();
        let or_expected = graph.truth_table(or_root, 6);
        let xor_expected = graph.truth_table(xor_root, 6);
        let outcome = balance(&graph, &[or_root, xor_root]);
        let or_balanced = remap_literal(&outcome.remap, or_root).unwrap();
        let xor_balanced = remap_literal(&outcome.remap, xor_root).unwrap();
        assert_eq!(outcome.network.truth_table(or_balanced, 6), or_expected);
        assert_eq!(outcome.network.truth_table(xor_balanced, 6), xor_expected);
        assert_eq!(outcome.network.level(or_balanced), 3);
        assert_eq!(outcome.network.level(xor_balanced), 3);
    }

    #[test]
    fn balances_a_priority_mux_chain_and_preserves_its_function() {
        let mut graph = LogicGraph::new();
        let selects: Vec<_> = (0..3).map(|i| graph.variable(i).unwrap()).collect();
        let datas: Vec<_> = (3..6).map(|i| graph.variable(i).unwrap()).collect();
        let mut result = datas[2];
        for (select, data) in [
            (selects[0], datas[1]),
            (selects[2], datas[0]),
            (selects[1], datas[1]),
            (selects[0], datas[0]),
        ] {
            result = graph.mux(select, data, result);
        }
        graph.freeze();
        let expected = graph.truth_table(result, 6);
        let linear_depth = graph.level(result);
        let outcome = balance(&graph, &[result]);
        let balanced = remap_literal(&outcome.remap, result).unwrap();
        assert_eq!(outcome.network.truth_table(balanced, 6), expected);
        assert!(outcome.network.level(balanced) < linear_depth);
    }

    #[test]
    fn follows_then_arm_mux_chains_with_an_inverted_select() {
        let mut graph = LogicGraph::new();
        let s0 = graph.variable(0).unwrap();
        let s1 = graph.variable(1).unwrap();
        let s2 = graph.variable(2).unwrap();
        let d0 = graph.variable(3).unwrap();
        let d1 = graph.variable(4).unwrap();
        let d2 = graph.variable(5).unwrap();
        let inner_tail = graph.mux(s2, d2, d0);
        let inner = graph.mux(s1, d1, inner_tail);
        let root = graph.mux(s0, inner, d0);
        graph.freeze();
        let expected = graph.truth_table(root, 6);
        let outcome = balance(&graph, &[root]);
        let balanced = remap_literal(&outcome.remap, root).unwrap();
        assert_eq!(outcome.network.truth_table(balanced, 6), expected);
    }

    #[test]
    fn keeps_shared_chain_interiors_intact() {
        let mut graph = LogicGraph::new();
        let shared = chain(&mut graph, 4, LogicGraph::and);
        let extra = graph.variable(4).unwrap();
        let root = graph.and(shared, extra);
        graph.freeze();
        let root_expected = graph.truth_table(root, 5);
        let shared_expected = graph.truth_table(shared, 5);
        let outcome = balance(&graph, &[root, shared]);
        let root_balanced = remap_literal(&outcome.remap, root).unwrap();
        let shared_balanced = remap_literal(&outcome.remap, shared).unwrap();
        assert_eq!(outcome.network.truth_table(root_balanced, 5), root_expected);
        assert_eq!(
            outcome.network.truth_table(shared_balanced, 5),
            shared_expected
        );
    }
}
