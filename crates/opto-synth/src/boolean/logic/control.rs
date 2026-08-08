// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Bounded Shannon decomposition for a multi-output AXM graph.

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use hashbrown::HashMap;

const MAX_CONTROLS: usize = u8::BITS as usize;
// This caps transient memory only; reaching it discards the optional choice.
const WORK_BUDGET: usize = 1_000_000;

pub(crate) struct ControlChoice {
    pub(crate) network: LogicGraph,
    pub(crate) roots: Box<[LogicNodeId]>,
}

/// Builds a structurally smaller Shannon decomposition. Small-support solving
/// folds its live nodes into one hash-consed mapper subject; larger subjects
/// may retain it as a separately covered implementation.
pub(crate) fn build_control_choice(
    source: &LogicGraph,
    source_roots: &[LogicNodeId],
    baseline: &LogicGraph,
    baseline_roots: &[LogicNodeId],
) -> Option<ControlChoice> {
    if source_roots.is_empty() || source_roots.len() != baseline_roots.len() {
        return None;
    }
    let controls = ranked_controls(source, source_roots);
    if controls.len() < 2 {
        return None;
    }

    let mut builder = ChoiceBuilder::new(source, &controls);
    let mut candidates = Vec::new();
    for depth in 2..=controls.len() {
        let Some(roots) = builder.decompose(source_roots, depth) else {
            break;
        };
        candidates.push((depth, roots));
    }
    builder.target.freeze();
    let baseline_gates = gate_count(baseline, baseline_roots);
    let candidate_roots = candidates
        .into_iter()
        .filter_map(|(depth, roots)| {
            let gates = gate_count(&builder.target, &roots);
            (gates < baseline_gates).then_some((gates, depth, roots))
        })
        .min_by_key(|(gates, depth, _)| (*gates, *depth))
        .map(|(_, _, roots)| roots)?;
    Some(ControlChoice {
        network: builder.target,
        roots: candidate_roots.into_boxed_slice(),
    })
}

fn ranked_controls(source: &LogicGraph, roots: &[LogicNodeId]) -> Vec<LogicNodeId> {
    let references = source.reference_counts(roots);
    let mut controls = (0..source.node_count())
        .filter_map(|index| {
            let node = LogicNodeId::from_index(index);
            matches!(source.node(node), LogicNode::Var(_)).then_some((references[index], node))
        })
        .filter(|(references, _)| *references > 1)
        .collect::<Vec<_>>();
    controls
        .sort_unstable_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    controls.truncate(MAX_CONTROLS);
    controls.into_iter().map(|(_, node)| node).collect()
}

fn gate_count(network: &LogicGraph, roots: &[LogicNodeId]) -> usize {
    let mut seen = vec![false; network.node_count()];
    let mut pending = roots.iter().map(|root| root.positive()).collect::<Vec<_>>();
    let mut gates = 0;
    while let Some(node) = pending.pop() {
        if std::mem::replace(&mut seen[node.index()], true) {
            continue;
        }
        let stored = network.node(node);
        if stored.is_gate() {
            gates += 1;
            pending.extend(stored.fanins().map(LogicNodeId::positive));
        }
    }
    gates
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CofactorKey {
    node: LogicNodeId,
    mask: u8,
    assignment: u8,
}

struct ChoiceBuilder<'a> {
    source: &'a LogicGraph,
    target: LogicGraph,
    variables: HashMap<u32, LogicNodeId>,
    control_supports: Vec<u8>,
    controls: Vec<LogicNodeId>,
    cofactors: HashMap<CofactorKey, LogicNodeId>,
    initial_nodes: usize,
}

impl<'a> ChoiceBuilder<'a> {
    fn new(source: &'a LogicGraph, controls: &[LogicNodeId]) -> Self {
        let mut control_positions = vec![None; source.node_count()];
        for (position, &control) in controls.iter().enumerate() {
            control_positions[control.index()] =
                Some(u8::try_from(position).expect("control position is bounded by MAX_CONTROLS"));
        }
        let mut control_supports = vec![0; source.node_count()];
        for index in 0..source.node_count() {
            let node = LogicNodeId::from_index(index);
            control_supports[index] = match source.node(node) {
                LogicNode::Const(_) => 0,
                LogicNode::Var(_) => control_positions[index].map_or(0, |bit| 1 << bit),
                gate => gate.fanins().fold(0, |support, fanin| {
                    support | control_supports[fanin.index()]
                }),
            };
        }
        let mut builder = Self {
            source,
            target: LogicGraph::new(),
            variables: HashMap::new(),
            control_supports,
            controls: Vec::with_capacity(controls.len()),
            cofactors: HashMap::new(),
            initial_nodes: 0,
        };
        for &control in controls {
            let LogicNode::Var(origin) = source.node(control) else {
                unreachable!("ranked controls are graph variables")
            };
            let target = builder.variable(origin);
            builder.controls.push(target);
        }
        builder.initial_nodes = builder.target.node_count();
        builder
    }

    fn variable(&mut self, origin: u32) -> LogicNodeId {
        *self.variables.entry(origin).or_insert_with(|| {
            self.target
                .variable(origin as usize)
                .expect("logic input stays within compact capacity")
        })
    }

    fn decompose(&mut self, roots: &[LogicNodeId], depth: usize) -> Option<Vec<LogicNodeId>> {
        let assignments = 1usize << depth;
        let mask = u8::try_from(assignments - 1).ok()?;
        let controls = self.controls[..depth].to_vec();
        let mut decomposed = Vec::with_capacity(roots.len());
        for &root in roots {
            let mut leaves = (0..assignments)
                .map(|assignment| self.cofactor(root, mask, u8::try_from(assignment).ok()?))
                .collect::<Option<Vec<_>>>()?;
            for &control in &controls {
                leaves = leaves
                    .chunks_exact(2)
                    .map(|pair| self.target.mux(control, pair[1], pair[0]))
                    .collect();
            }
            decomposed.push(*leaves.first()?);
        }
        Some(decomposed)
    }

    fn cofactor(&mut self, literal: LogicNodeId, mask: u8, assignment: u8) -> Option<LogicNodeId> {
        let root = literal.positive();
        let root_key = self.key(root, mask, assignment);
        if !self.cofactors.contains_key(&root_key) {
            let mut pending = vec![(root, false)];
            while let Some((node, expanded)) = pending.pop() {
                let key = self.key(node, mask, assignment);
                if self.cofactors.contains_key(&key) {
                    continue;
                }
                if !expanded {
                    pending.push((node, true));
                    pending.extend(
                        self.source
                            .node(node)
                            .fanins()
                            .map(|fanin| (fanin.positive(), false)),
                    );
                    continue;
                }
                let mapped = match self.source.node(node) {
                    LogicNode::Const(value) => LogicGraph::constant(value),
                    LogicNode::Var(origin) if key.mask == 0 => self.variable(origin),
                    LogicNode::Var(_) => LogicGraph::constant(key.assignment != 0),
                    LogicNode::And(left, right) => self.target.and(
                        self.cached(left, mask, assignment)?,
                        self.cached(right, mask, assignment)?,
                    ),
                    LogicNode::Xor(left, right) => self.target.xor(
                        self.cached(left, mask, assignment)?,
                        self.cached(right, mask, assignment)?,
                    ),
                    LogicNode::Mux {
                        cond,
                        then_value,
                        else_value,
                    } => self.target.mux(
                        self.cached(cond, mask, assignment)?,
                        self.cached(then_value, mask, assignment)?,
                        self.cached(else_value, mask, assignment)?,
                    ),
                };
                self.cofactors.insert(key, mapped);
                if self.cofactors.len() > WORK_BUDGET
                    || self.target.node_count() - self.initial_nodes > WORK_BUDGET
                {
                    return None;
                }
            }
        }
        let mapped = *self.cofactors.get(&root_key)?;
        Some(apply_phase(mapped, literal))
    }

    fn cached(&self, literal: LogicNodeId, mask: u8, assignment: u8) -> Option<LogicNodeId> {
        self.cofactors
            .get(&self.key(literal.positive(), mask, assignment))
            .copied()
            .map(|mapped| apply_phase(mapped, literal))
    }

    fn key(&self, node: LogicNodeId, mask: u8, assignment: u8) -> CofactorKey {
        let mask = mask & self.control_supports[node.index()];
        CofactorKey {
            node,
            mask,
            assignment: assignment & mask,
        }
    }
}

fn apply_phase(mapped: LogicNodeId, source: LogicNodeId) -> LogicNodeId {
    if source.is_inverted() {
        LogicGraph::not(mapped)
    } else {
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_formal::prove_logic_network_equivalence;

    fn flattened_selector(output_count: usize) -> (LogicGraph, Vec<LogicNodeId>) {
        let mut source = LogicGraph::new();
        let data = (0..8)
            .map(|index| source.variable(index).unwrap())
            .collect::<Vec<_>>();
        let controls = (8..11)
            .map(|index| source.variable(index).unwrap())
            .collect::<Vec<_>>();
        let mut roots = Vec::new();
        for rotation in 0..output_count {
            let mut terms = Vec::new();
            for assignment in 0..8 {
                let mut term = data[(rotation + assignment) % 8];
                for (bit, &control) in controls.iter().enumerate() {
                    let condition = if assignment & (1 << bit) == 0 {
                        LogicGraph::not(control)
                    } else {
                        control
                    };
                    term = source.and(term, condition);
                }
                terms.push(term);
            }
            while terms.len() > 1 {
                let right = terms.pop().unwrap();
                let left = terms.pop().unwrap();
                terms.push(source.or(left, right));
            }
            roots.push(terms[0]);
        }
        source.freeze();
        (source, roots)
    }

    #[test]
    fn shared_control_choice_preserves_a_flattened_multi_output_selector() {
        let (source, roots) = flattened_selector(8);
        let choice = build_control_choice(&source, &roots, &source, &roots)
            .expect("selector has a smaller decomposition");
        let reference_outputs = roots.iter().map(|root| root.lit()).collect::<Vec<_>>();
        let candidate_outputs = choice
            .roots
            .iter()
            .map(|root| root.lit())
            .collect::<Vec<_>>();
        let proof = prove_logic_network_equivalence(
            source.storage_network(),
            &reference_outputs,
            choice.network.storage_network(),
            &candidate_outputs,
        )
        .expect("formal engine accepts the decomposition miter");
        assert!(proof.require_proved().is_ok());
    }

    #[test]
    fn decomposes_a_single_output_without_pattern_rules() {
        let (source, roots) = flattened_selector(1);
        assert!(build_control_choice(&source, &roots, &source, &roots).is_some());
    }

    #[test]
    fn rejects_a_larger_arithmetic_decomposition() {
        let mut network = LogicGraph::new();
        let mut carry = network.variable(0).unwrap();
        let mut roots = Vec::new();
        for bit in 0..8 {
            let a = network.variable(bit * 2 + 1).unwrap();
            let b = network.variable(bit * 2 + 2).unwrap();
            let propagate = network.xor(a, b);
            roots.push(network.xor(propagate, carry));
            carry = network.mux(propagate, carry, a);
        }
        roots.push(carry);
        network.freeze();

        assert!(build_control_choice(&network, &roots, &network, &roots).is_none());
    }
}
