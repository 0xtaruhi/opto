// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNodeId};
use opto_ir::word;
use opto_runtime::ExecutionContext;

/// One immutable Boolean arena with proved implementation alternatives.
pub(crate) struct ChoiceGraph {
    network: LogicGraph,
    alternatives: opto_core::PackedRows<LogicNodeId>,
    value_nodes: Box<[(word::ValueId, LogicNodeId)]>,
    dont_care_values: Box<[word::ValueId]>,
    inputs: Box<[word::ValueId]>,
}

/// Canonical AXM network and its stable region-local Word binding identities.
pub(crate) struct CanonicalRegionLogic {
    pub(crate) network: LogicGraph,
    pub(crate) value_nodes: Box<[(word::ValueId, LogicNodeId)]>,
    pub(crate) dont_care_values: Box<[word::ValueId]>,
    pub(crate) inputs: Box<[word::ValueId]>,
}

#[derive(Clone, Copy)]
pub(crate) struct RegionLogicOptions<'a> {
    pub(crate) optimize: bool,
    pub(crate) config: crate::SynthesisConfig,
    pub(crate) runtime: &'a ExecutionContext,
    pub(crate) incremental: Option<super::rewrite::RewriteIncremental<'a>>,
}

impl ChoiceGraph {
    pub(crate) fn from_canonical(
        mut subject: CanonicalRegionLogic,
        roots: &[word::ValueId],
        requirements: &[Option<f64>],
        options: RegionLogicOptions<'_>,
    ) -> Result<Self, crate::SynthError> {
        if roots.len() != requirements.len() {
            return Err(crate::SynthError::invariant(
                "Boolean subject requirements do not align with roots",
            ));
        }
        let RegionLogicOptions {
            optimize,
            config,
            runtime,
            incremental,
        } = options;
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(config.diagnostics),
            "logic.network",
            "nodes={}",
            subject.network.node_count()
        );
        let root_entries = roots
            .iter()
            .zip(requirements)
            .filter_map(|(&root, &requirement)| {
                subject
                    .value_nodes
                    .binary_search_by_key(&root, |&(value, _)| value)
                    .ok()
                    .map(|index| (root, subject.value_nodes[index].1, requirement))
            })
            .collect::<Vec<_>>();
        let root_nodes = root_entries
            .iter()
            .map(|&(_, node, _)| node)
            .collect::<Vec<_>>();
        let root_requirements = root_entries
            .iter()
            .map(|&(_, _, requirement)| requirement)
            .collect::<Vec<_>>();
        let optimized = super::pipeline::optimize(
            std::mem::replace(&mut subject.network, LogicGraph::new()),
            &root_nodes,
            &root_requirements,
            optimize,
            config.diagnostics,
            runtime,
            incremental,
        )?;
        let value_nodes = subject
            .value_nodes
            .into_vec()
            .into_iter()
            .filter_map(|(value, node)| {
                super::rewrite::remap_literal(&optimized.remap, node).map(|node| (value, node))
            })
            .collect::<Vec<_>>();
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(config.diagnostics),
            "logic.network_optimized",
            "nodes={}",
            optimized.network.node_count()
        );
        Ok(Self {
            network: optimized.network,
            alternatives: optimized.alternatives,
            value_nodes: value_nodes.into_boxed_slice(),
            dont_care_values: subject.dont_care_values,
            inputs: subject.inputs,
        })
    }

    pub(crate) fn network(&self) -> &LogicGraph {
        &self.network
    }

    /// Structurally distinct literals proved equal to `representative`.
    pub(crate) fn alternatives(&self, representative: LogicNodeId) -> &[LogicNodeId] {
        self.alternatives.row(representative.positive().index())
    }

    /// Marks the selected cone and every retained implementation cone that can
    /// cover one of its nodes.
    pub(crate) fn live_nodes(&self, roots: &[LogicNodeId]) -> Box<[bool]> {
        let mut live = vec![false; self.network.node_count()];
        let mut pending = roots
            .iter()
            .copied()
            .map(LogicNodeId::positive)
            .collect::<Vec<_>>();
        while let Some(node) = pending.pop() {
            if std::mem::replace(&mut live[node.index()], true) {
                continue;
            }
            pending.extend(self.network.node(node).fanins().map(LogicNodeId::positive));
            pending.extend(
                self.alternatives(node)
                    .iter()
                    .copied()
                    .map(LogicNodeId::positive),
            );
        }
        live.into_boxed_slice()
    }

    pub(crate) fn inputs(&self) -> &[word::ValueId] {
        &self.inputs
    }

    /// The subject node implementing one region-local Word value.
    pub(crate) fn node(&self, value: word::ValueId) -> Option<LogicNodeId> {
        let index = self
            .value_nodes
            .binary_search_by_key(&value, |&(candidate, _)| candidate)
            .ok()?;
        Some(self.value_nodes[index].1)
    }

    /// Return whether a published value has no Boolean care obligation.
    pub(crate) fn is_dont_care(&self, value: word::ValueId) -> bool {
        self.dont_care_values.binary_search(&value).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boolean::logic::cuts::{CutDatabase, CutTruthDatabase};

    #[test]
    fn compiled_cuts_reach_a_proved_alternative_cone() {
        let mut network = LogicGraph::new();
        let inputs = (0..7)
            .map(|origin| network.variable(origin).unwrap())
            .collect::<Vec<_>>();
        let mut root = inputs[0];
        for index in 0..24 {
            let then_value = network.xor(root, inputs[(index + 1) % inputs.len()]);
            let else_value = network.and(root, inputs[(index + 2) % inputs.len()]);
            root = network.mux(inputs[index % inputs.len()], then_value, else_value);
        }
        network.freeze();
        let value = word::ValueId::FIRST;
        let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 2 })
            .expect("test runtime is valid");
        let choices = ChoiceGraph::from_canonical(
            CanonicalRegionLogic {
                network,
                value_nodes: vec![(value, root)].into_boxed_slice(),
                dont_care_values: Box::new([]),
                inputs: Box::new([]),
            },
            &[value],
            &[None],
            RegionLogicOptions {
                optimize: true,
                config: crate::SynthesisConfig::default(),
                runtime: &runtime,
                incremental: None,
            },
        )
        .expect("choice graph construction succeeds");
        let root = choices.node(value).expect("root binding is retained");
        let cuts = CutDatabase::build_choices_parallel(
            &choices,
            crate::boolean::logic::MAX_MATCH_INPUTS,
            &runtime,
        )
        .expect("choice cut compilation succeeds");
        let truths = CutTruthDatabase::build_parallel(choices.network(), &cuts, &runtime)
            .expect("choice truth compilation succeeds");
        let retained = (0..choices.network().node_count()).find_map(|index| {
            let node = LogicNodeId::from_index(index);
            cuts.cuts(node)
                .iter()
                .enumerate()
                .find(|&(cut, candidate)| {
                    cuts.origin(node, cut) != node && !candidate.contains(node)
                })
                .map(|(cut, candidate)| (node, cut, *candidate))
        });
        let (node, cut, candidate) =
            retained.expect("compiled mapping retains a usable alternative cut");
        assert_eq!(truths.truth(node, cut).input_count, candidate.len());
        assert!(choices.live_nodes(&[root])[cuts.origin(node, cut).index()]);
    }
}
