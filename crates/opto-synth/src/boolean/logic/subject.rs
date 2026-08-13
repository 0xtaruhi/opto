// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNodeId};
use opto_ir::word;
use opto_runtime::ExecutionContext;

/// Immutable region-local Boolean graph consumed by technology mapping.
pub(crate) struct RegionLogicGraph {
    network: LogicGraph,
    implementations: Box<[RegionLogicImplementation]>,
    inputs: Box<[word::ValueId]>,
}

pub(crate) struct RegionLogicImplementation {
    pass: &'static str,
    value_nodes: Box<[(word::ValueId, LogicNodeId)]>,
}

/// Canonical AXM network and its stable region-local Word binding identities.
pub(crate) struct CanonicalRegionLogic {
    pub(crate) network: LogicGraph,
    pub(crate) value_nodes: Box<[(word::ValueId, LogicNodeId)]>,
    pub(crate) inputs: Box<[word::ValueId]>,
}

#[derive(Clone, Copy)]
pub(crate) struct RegionLogicOptions<'a> {
    pub(crate) optimize: bool,
    pub(crate) config: crate::SynthesisConfig,
    pub(crate) runtime: &'a ExecutionContext,
    pub(crate) incremental: Option<super::rewrite::RewriteIncremental<'a>>,
}

impl RegionLogicGraph {
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
        let mut implementations = Vec::with_capacity(1 + optimized.alternatives.len());
        implementations.push(RegionLogicImplementation {
            pass: "baseline",
            value_nodes: value_nodes.into_boxed_slice(),
        });
        for alternative in optimized.alternatives {
            if alternative.roots.len() != root_entries.len() {
                return Err(crate::SynthError::invariant(
                    "AXM alternative roots do not align with subject roots",
                ));
            }
            let mut value_nodes = root_entries
                .iter()
                .map(|&(value, _, _)| value)
                .zip(alternative.roots)
                .collect::<Vec<_>>();
            value_nodes.sort_unstable_by_key(|&(value, _)| value);
            value_nodes.dedup_by_key(|(value, _)| *value);
            implementations.push(RegionLogicImplementation {
                pass: alternative.pass,
                value_nodes: value_nodes.into_boxed_slice(),
            });
        }
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(config.diagnostics),
            "logic.network_optimized",
            "nodes={} implementations={}",
            optimized.network.node_count(),
            implementations.len()
        );
        Ok(Self {
            network: optimized.network,
            implementations: implementations.into_boxed_slice(),
            inputs: subject.inputs,
        })
    }

    pub(crate) fn network(&self) -> &LogicGraph {
        &self.network
    }

    pub(crate) fn inputs(&self) -> &[word::ValueId] {
        &self.inputs
    }

    pub(crate) fn implementations(&self) -> &[RegionLogicImplementation] {
        &self.implementations
    }
}

impl RegionLogicImplementation {
    pub(crate) const fn pass(&self) -> &'static str {
        self.pass
    }

    pub(crate) fn node(&self, value: word::ValueId) -> Option<LogicNodeId> {
        let index = self
            .value_nodes
            .binary_search_by_key(&value, |&(candidate, _)| candidate)
            .ok()?;
        Some(self.value_nodes[index].1)
    }
}
