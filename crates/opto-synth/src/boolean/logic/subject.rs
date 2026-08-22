// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNodeId};
use opto_ir::word;
use opto_runtime::{ExecutionContext, Task, TaskKey};

const CHOICE_SCOPE_TASK_DOMAIN: u32 = 0x4348_4f49;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ChoiceScopeId(u32);

impl ChoiceScopeId {
    pub(crate) fn from_index(index: usize) -> Result<Self, crate::SynthError> {
        Ok(Self(u32::try_from(index).map_err(|_| {
            crate::SynthError::capacity("choice scope ID exceeds 32-bit capacity")
        })?))
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// One immutable Boolean arena with proved implementation alternatives.
pub(crate) struct ChoiceGraph {
    network: LogicGraph,
    alternatives: opto_core::PackedRows<LogicNodeId>,
    value_nodes: opto_core::PackedRows<(word::ValueId, LogicNodeId)>,
    dont_care_values: opto_core::PackedRows<word::ValueId>,
    inputs: opto_core::PackedRows<word::ValueId>,
}

/// Independently compiled Boolean scopes for one design-wide selection epoch.
pub(crate) struct ChoiceDesign {
    scopes: Box<[ChoiceGraph]>,
}

pub(crate) struct ChoiceSubject {
    pub(crate) canonical: CanonicalRegionLogic,
    pub(crate) roots: Box<[word::ValueId]>,
    pub(crate) requirements: Box<[Option<f64>]>,
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

impl ChoiceDesign {
    pub(crate) fn from_subjects(
        subjects: Vec<ChoiceSubject>,
        options: RegionLogicOptions<'_>,
    ) -> Result<Self, crate::SynthError> {
        let tasks = subjects
            .into_iter()
            .enumerate()
            .map(|(index, subject)| {
                let work = subject.canonical.network.node_count().max(1) as u64;
                Task::new(
                    TaskKey::new(CHOICE_SCOPE_TASK_DOMAIN, index as u64),
                    subject,
                )
                .with_estimated_work(work)
                .with_estimated_memory(work)
            })
            .collect();
        let optimized = options
            .runtime
            .map_ordered_composite(tasks, |subject, runtime| {
                ChoiceGraph::from_canonical(
                    subject.canonical,
                    &subject.roots,
                    &subject.requirements,
                    RegionLogicOptions { runtime, ..options },
                )
            })?;
        Ok(Self {
            scopes: optimized.into_boxed_slice(),
        })
    }

    pub(crate) fn graph(&self, scope: ChoiceScopeId) -> &ChoiceGraph {
        &self.scopes[scope.index()]
    }

    pub(crate) fn scope_count(&self) -> usize {
        self.scopes.len()
    }

    pub(crate) fn inputs(&self, scope: ChoiceScopeId) -> &[word::ValueId] {
        self.graph(scope).inputs()
    }

    pub(crate) fn node(&self, scope: ChoiceScopeId, value: word::ValueId) -> Option<LogicNodeId> {
        self.graph(scope).node(value)
    }

    pub(crate) fn is_dont_care(&self, scope: ChoiceScopeId, value: word::ValueId) -> bool {
        self.graph(scope).is_dont_care(value)
    }
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
            .map(|(&root, &requirement)| {
                let index = subject
                    .value_nodes
                    .binary_search_by_key(&root, |&(value, _)| value)
                    .map_err(|_| {
                        crate::SynthError::invariant(format!(
                            "Boolean compilation root {root:?} has no canonical subject binding"
                        ))
                    })?;
                Ok((root, subject.value_nodes[index].1, requirement))
            })
            .collect::<Result<Vec<_>, crate::SynthError>>()?;
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
            value_nodes: opto_core::PackedRows::try_from_rows(vec![value_nodes])
                .map_err(|_| crate::SynthError::capacity("choice value bindings"))?,
            dont_care_values: opto_core::PackedRows::try_from_rows(vec![
                subject.dont_care_values.into_vec(),
            ])
            .map_err(|_| crate::SynthError::capacity("choice don't-care bindings"))?,
            inputs: opto_core::PackedRows::try_from_rows(vec![subject.inputs.into_vec()])
                .map_err(|_| crate::SynthError::capacity("choice input bindings"))?,
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
        &self.inputs[0]
    }

    /// The subject node implementing one region-local Word value.
    pub(crate) fn node(&self, value: word::ValueId) -> Option<LogicNodeId> {
        let values = &self.value_nodes[0];
        let index = values
            .binary_search_by_key(&value, |&(candidate, _)| candidate)
            .ok()?;
        Some(values[index].1)
    }

    /// Return whether a published value has no Boolean care obligation.
    pub(crate) fn is_dont_care(&self, value: word::ValueId) -> bool {
        self.dont_care_values[0].binary_search(&value).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boolean::logic::cuts::{CutDatabase, CutTruthDatabase};

    #[test]
    fn rejects_a_root_outside_the_frozen_subject() {
        let mut network = LogicGraph::new();
        let input = network.variable(0).unwrap();
        network.freeze();
        let bound = word::ValueId::FIRST;
        let missing = word::ValueId::from_index(1).unwrap();
        let runtime = ExecutionContext::default();
        let error = ChoiceGraph::from_canonical(
            CanonicalRegionLogic {
                network,
                value_nodes: vec![(bound, input)].into_boxed_slice(),
                dont_care_values: Box::new([]),
                inputs: Box::new([]),
            },
            &[missing],
            &[None],
            RegionLogicOptions {
                optimize: false,
                config: crate::SynthesisConfig::default(),
                runtime: &runtime,
                incremental: None,
            },
        )
        .err()
        .expect("a missing frozen root is rejected");

        assert!(error.to_string().contains("no canonical subject binding"));
    }

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

    #[test]
    fn design_wide_scopes_keep_local_input_domains_disjoint() {
        let value = word::ValueId::FIRST;
        let build = |xor: bool| {
            let mut network = LogicGraph::new();
            let left = network.variable(0).unwrap();
            let right = network.variable(1).unwrap();
            let root = if xor {
                network.xor(left, right)
            } else {
                network.and(left, right)
            };
            network.freeze();
            ChoiceSubject {
                canonical: CanonicalRegionLogic {
                    network,
                    value_nodes: Box::new([(value, root)]),
                    dont_care_values: Box::new([]),
                    inputs: Box::new([value, word::ValueId::from_index(1).unwrap()]),
                },
                roots: Box::new([value]),
                requirements: Box::new([None]),
            }
        };
        let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 2 })
            .expect("test runtime is valid");
        let choices = ChoiceDesign::from_subjects(
            vec![build(false), build(true)],
            RegionLogicOptions {
                optimize: false,
                config: crate::SynthesisConfig::default(),
                runtime: &runtime,
                incremental: None,
            },
        )
        .expect("design-wide choice construction succeeds");
        let first = ChoiceScopeId(0);
        let second = ChoiceScopeId(1);
        assert_eq!(choices.inputs(first).len(), 2);
        assert_eq!(choices.inputs(second).len(), 2);
        let first = choices.node(first, value).unwrap();
        let second = choices.node(second, value).unwrap();
        assert_eq!(
            choices
                .graph(ChoiceScopeId(0))
                .network
                .truth_table(first, 2)
                .bits,
            0x8
        );
        assert_eq!(
            choices
                .graph(ChoiceScopeId(1))
                .network
                .truth_table(second, 2)
                .bits,
            0x6
        );
    }
}
