// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNodeId};
use opto_ir::word;
use opto_runtime::ExecutionContext;

/// Immutable region-local Boolean graph consumed by technology mapping.
pub(crate) struct RegionLogicGraph {
    network: LogicGraph,
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
        crate::api::diagnostics::trace!(
            crate::api::diagnostics::SynthTrace::timing(config.diagnostics),
            "logic.network_optimized",
            "nodes={}",
            optimized.network.node_count()
        );
        Ok(Self {
            network: optimized.network,
            value_nodes: value_nodes.into_boxed_slice(),
            dont_care_values: subject.dont_care_values,
            inputs: subject.inputs,
        })
    }

    pub(crate) fn network(&self) -> &LogicGraph {
        &self.network
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
