// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Static AXM pass scheduling and graph-choice composition.

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::rewrite::{RewriteIncremental, remap_literal};
use hashbrown::HashMap;
use opto_runtime::ExecutionContext;

pub(super) struct LogicPipelineOutcome {
    pub(super) network: LogicGraph,
    pub(super) remap: Box<[Option<LogicNodeId>]>,
    pub(super) alternatives: Box<[LogicAlternative]>,
}

pub(super) struct LogicAlternative {
    pub(super) pass: &'static str,
    pub(super) roots: Box<[LogicNodeId]>,
}

struct ChoiceProposal {
    pass: &'static str,
    network: LogicGraph,
    roots: Box<[LogicNodeId]>,
}

type LogicRoots = (LogicGraph, Box<[LogicNodeId]>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyStyle {
    Preserve,
    DecomposeMux,
}

pub(super) struct TransformProduct {
    pub(super) network: LogicGraph,
    pub(super) remap: Box<[Option<LogicNodeId>]>,
    pub(super) analyses: TransformAnalyses,
}

#[derive(Default)]
pub(super) struct TransformAnalyses {
    pub(super) rewrite: Option<super::rewrite::CutReuse>,
}

/// Owns the only destructive AXM implementation while passes are converging.
/// Every applied transform is composed back to the original node space here.
pub(super) struct TransformState {
    pub(super) network: LogicGraph,
    pub(super) roots: Box<[LogicNodeId]>,
    pub(super) remap: Box<[Option<LogicNodeId>]>,
    pub(super) analyses: TransformAnalyses,
}

impl TransformState {
    pub(super) fn start(
        source_roots: &[LogicNodeId],
        outcome: TransformProduct,
    ) -> Result<Self, crate::SynthError> {
        let roots = map_roots(&outcome.remap, source_roots)?;
        Ok(Self {
            network: outcome.network,
            roots,
            remap: outcome.remap,
            analyses: outcome.analyses,
        })
    }

    pub(super) fn apply(&mut self, outcome: TransformProduct) -> Result<(), crate::SynthError> {
        self.roots = map_roots(&outcome.remap, &self.roots)?;
        self.remap = compose_remaps(&self.remap, &outcome.remap);
        self.network = outcome.network;
        self.analyses = outcome.analyses;
        Ok(())
    }

    pub(super) fn finish(self) -> TransformProduct {
        TransformProduct {
            network: self.network,
            remap: self.remap,
            analyses: self.analyses,
        }
    }
}

pub(super) fn optimize(
    source: LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    enabled: bool,
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: Option<RewriteIncremental<'_>>,
) -> Result<LogicPipelineOutcome, crate::SynthError> {
    if roots.len() != requirements.len() {
        return Err(crate::SynthError::invariant(
            "AXM pipeline requirements do not align with roots",
        ));
    }
    if !enabled {
        return finish(identity(source), roots, Vec::new());
    }
    let functional = small_support_choice(&source, roots, requirements, diagnostics, runtime)?;
    let baseline = optimize_baseline(
        &source,
        roots,
        requirements,
        diagnostics,
        runtime,
        incremental,
    )?;
    let mut alternatives = functional.into_iter().collect::<Vec<_>>();
    alternatives.extend(decomposed_choice(
        &source,
        roots,
        requirements,
        diagnostics,
        runtime,
    )?);
    finish(baseline, roots, alternatives)
}

fn decomposed_choice(
    source: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<Option<ChoiceProposal>, crate::SynthError> {
    let Some((network, roots)) = decompose_muxes(source, roots)? else {
        return Ok(None);
    };
    let first = optimize_factored(&network, &roots, requirements, diagnostics, runtime)?;
    let first_roots = map_roots(&first.remap, &roots)?;
    let (optimized, roots) = if let Some((network, roots)) =
        decompose_muxes(&first.network, &first_roots)?
    {
        let optimized = optimize_factored(&network, &roots, requirements, diagnostics, runtime)?;
        let roots = map_roots(&optimized.remap, &roots)?;
        (optimized, roots)
    } else {
        (first, first_roots)
    };
    Ok(Some(ChoiceProposal {
        pass: "mux_decomposition",
        network: optimized.network,
        roots,
    }))
}

fn decompose_muxes(
    source: &LogicGraph,
    roots: &[LogicNodeId],
) -> Result<Option<LogicRoots>, crate::SynthError> {
    if !(0..source.node_count()).any(|index| {
        matches!(
            source.node(LogicNodeId::from_index(index)),
            LogicNode::Mux { .. }
        )
    }) {
        return Ok(None);
    }
    let mut network = LogicGraph::new();
    let mut variables = HashMap::new();
    let remap = copy_graph(
        source,
        None,
        &mut network,
        &mut variables,
        CopyStyle::DecomposeMux,
    )?;
    let roots = map_roots(&remap, roots)?;
    network.freeze();
    Ok(Some((network, roots)))
}

fn small_support_choice(
    source: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<Option<ChoiceProposal>, crate::SynthError> {
    let factoring_started = std::time::Instant::now();
    let Some(subject) = super::pla::build_multi_output(source, roots, runtime)? else {
        return Ok(None);
    };
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(diagnostics),
        "logic.multi_output.factor",
        "nodes={} cover={:?} factoring={:?} resubstitution={:?} checks={} plans={} wall={:?}",
        subject.network.node_count(),
        subject.profile.cover,
        subject.profile.factoring,
        subject.profile.resubstitution,
        subject.profile.relation_checks,
        subject.profile.plan_queries,
        factoring_started.elapsed()
    );
    let normalization_started = std::time::Instant::now();
    let normalized = optimize_factored(
        &subject.network,
        &subject.roots,
        requirements,
        diagnostics,
        runtime,
    )?;
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(diagnostics),
        "logic.multi_output.normalize",
        "nodes={} wall={:?}",
        normalized.network.node_count(),
        normalization_started.elapsed()
    );
    let normalized_roots = map_roots(&normalized.remap, &subject.roots)?;
    Ok(Some(ChoiceProposal {
        pass: "multi_output_factoring",
        network: normalized.network,
        roots: normalized_roots,
    }))
}

pub(super) fn optimize_baseline(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: Option<RewriteIncremental<'_>>,
) -> Result<TransformProduct, crate::SynthError> {
    if let Some(incremental) = incremental {
        optimize_with(
            network,
            roots,
            requirements,
            diagnostics,
            runtime,
            (incremental, super::rewrite::resynthesize, false),
        )
    } else {
        let cache = super::rewrite::RewriteRecipeCache::default();
        let metrics = crate::incremental::IncrementalRunMetrics::default();
        optimize_with(
            network,
            roots,
            requirements,
            diagnostics,
            runtime,
            (
                RewriteIncremental::new(&cache, &metrics),
                super::rewrite::resynthesize,
                false,
            ),
        )
    }
}

fn optimize_factored(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<TransformProduct, crate::SynthError> {
    let cache = super::rewrite::RewriteRecipeCache::default();
    let metrics = crate::incremental::IncrementalRunMetrics::default();
    optimize_with(
        network,
        roots,
        requirements,
        diagnostics,
        runtime,
        (
            RewriteIncremental::new(&cache, &metrics),
            super::rewrite::normalize,
            true,
        ),
    )
}

type RewriteStage = fn(
    &mut TransformState,
    &[Option<f64>],
    crate::SynthesisDiagnostics,
    &ExecutionContext,
    RewriteIncremental<'_>,
) -> Result<(), crate::SynthError>;

type OptimizationStage<'a> = (RewriteIncremental<'a>, RewriteStage, bool);

fn optimize_with(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    options: OptimizationStage<'_>,
) -> Result<TransformProduct, crate::SynthError> {
    if roots.len() != requirements.len() {
        return Err(crate::SynthError::invariant(
            "AXM pass requirements do not align with roots",
        ));
    }
    let (incremental, rewrite, balance_unconstrained) = options;
    let mut state = TransformState::start(roots, copy_active(network, roots)?)?;
    rewrite(&mut state, requirements, diagnostics, runtime, incremental)?;
    state.network.freeze();
    state.analyses = TransformAnalyses::default();

    let started = std::time::Instant::now();
    let balanced = super::balance::balance(&state.network, &state.roots);
    let balanced_roots = map_roots(&balanced.remap, &state.roots)?;
    let profile = |network, roots| {
        if balance_unconstrained {
            super::rewrite::balance_profile(network, roots, requirements)
        } else {
            super::rewrite::timing_profile(network, roots, requirements)
        }
    };
    let accepted =
        profile(&balanced.network, &balanced_roots) < profile(&state.network, &state.roots);
    if accepted {
        state.apply(balanced)?;
    }
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(diagnostics),
        "logic.balance",
        "accepted={accepted} wall={:?}",
        started.elapsed()
    );
    Ok(state.finish())
}

fn finish(
    baseline: TransformProduct,
    source_roots: &[LogicNodeId],
    alternatives: Vec<ChoiceProposal>,
) -> Result<LogicPipelineOutcome, crate::SynthError> {
    if alternatives.is_empty() {
        map_roots(&baseline.remap, source_roots)?;
        return Ok(LogicPipelineOutcome {
            network: baseline.network,
            remap: baseline.remap,
            alternatives: Box::new([]),
        });
    }

    let mut network = LogicGraph::new();
    let mut variables = HashMap::new();
    let baseline_remap = copy_graph(
        &baseline.network,
        None,
        &mut network,
        &mut variables,
        CopyStyle::Preserve,
    )?;
    let remap = compose_remaps(&baseline.remap, &baseline_remap);
    map_roots(&remap, source_roots)?;

    let mut installed = Vec::with_capacity(alternatives.len());
    for alternative in alternatives {
        let live = live_nodes(&alternative.network, &alternative.roots);
        let alternative_remap = copy_graph(
            &alternative.network,
            Some(&live),
            &mut network,
            &mut variables,
            CopyStyle::Preserve,
        )?;
        installed.push(LogicAlternative {
            pass: alternative.pass,
            roots: map_roots(&alternative_remap, &alternative.roots)?,
        });
    }
    network.freeze();
    Ok(LogicPipelineOutcome {
        network,
        remap,
        alternatives: installed.into_boxed_slice(),
    })
}

fn identity(network: LogicGraph) -> TransformProduct {
    let remap = (0..network.node_count())
        .map(|index| Some(LogicNodeId::from_index(index)))
        .collect();
    TransformProduct {
        network,
        remap,
        analyses: TransformAnalyses::default(),
    }
}

fn copy_active(
    network: &LogicGraph,
    roots: &[LogicNodeId],
) -> Result<TransformProduct, crate::SynthError> {
    let live = live_nodes(network, roots);
    let mut copied = LogicGraph::new();
    let mut variables = HashMap::new();
    let remap = copy_graph(
        network,
        Some(&live),
        &mut copied,
        &mut variables,
        CopyStyle::Preserve,
    )?;
    copied.freeze();
    Ok(TransformProduct {
        network: copied,
        remap,
        analyses: TransformAnalyses::default(),
    })
}

pub(super) fn map_roots(
    remap: &[Option<LogicNodeId>],
    roots: &[LogicNodeId],
) -> Result<Box<[LogicNodeId]>, crate::SynthError> {
    roots
        .iter()
        .map(|&root| {
            remap_literal(remap, root)
                .ok_or_else(|| crate::SynthError::invariant("AXM pass omitted an active root"))
        })
        .collect()
}

pub(super) fn compose_remaps(
    first: &[Option<LogicNodeId>],
    second: &[Option<LogicNodeId>],
) -> Box<[Option<LogicNodeId>]> {
    first
        .iter()
        .map(|&literal| literal.and_then(|literal| remap_literal(second, literal)))
        .collect()
}

fn live_nodes(network: &LogicGraph, roots: &[LogicNodeId]) -> Box<[bool]> {
    let mut live = vec![false; network.node_count()];
    let mut pending = roots.iter().map(|root| root.positive()).collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        if std::mem::replace(&mut live[node.index()], true) {
            continue;
        }
        pending.extend(network.node(node).fanins().map(LogicNodeId::positive));
    }
    live.into_boxed_slice()
}

fn copy_graph(
    source: &LogicGraph,
    live: Option<&[bool]>,
    target: &mut LogicGraph,
    variables: &mut HashMap<u32, LogicNodeId>,
    style: CopyStyle,
) -> Result<Box<[Option<LogicNodeId>]>, crate::SynthError> {
    let mut remap = vec![None; source.node_count()];
    for index in 0..source.node_count() {
        if live.is_some_and(|live| !live[index]) {
            continue;
        }
        let node = LogicNodeId::from_index(index);
        let mapped = match source.node(node) {
            LogicNode::Const(value) => LogicGraph::constant(value),
            LogicNode::Var(origin) => *variables.entry(origin).or_insert_with(|| {
                target
                    .variable(origin as usize)
                    .expect("AXM input stays within compact capacity")
            }),
            LogicNode::And(left, right) => target.and(
                mapped_literal(&remap, left)?,
                mapped_literal(&remap, right)?,
            ),
            LogicNode::Xor(left, right) => target.xor(
                mapped_literal(&remap, left)?,
                mapped_literal(&remap, right)?,
            ),
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => {
                let cond = mapped_literal(&remap, cond)?;
                let then_value = mapped_literal(&remap, then_value)?;
                let else_value = mapped_literal(&remap, else_value)?;
                if style == CopyStyle::DecomposeMux {
                    let selected = target.and(cond, then_value);
                    let rejected = target.and(cond.inverted(), else_value);
                    target.or(selected, rejected)
                } else {
                    target.mux(cond, then_value, else_value)
                }
            }
        };
        remap[index] = Some(mapped);
    }
    Ok(remap.into_boxed_slice())
}

fn mapped_literal(
    remap: &[Option<LogicNodeId>],
    literal: LogicNodeId,
) -> Result<LogicNodeId, crate::SynthError> {
    remap_literal(remap, literal).ok_or_else(|| {
        crate::SynthError::invariant("AXM graph is not topological within its active cone")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opto_formal::prove_logic_network_equivalence;

    fn graph(
        gate: fn(&mut LogicGraph, LogicNodeId, LogicNodeId) -> LogicNodeId,
    ) -> (LogicGraph, LogicNodeId) {
        let mut network = LogicGraph::new();
        let left = network.variable(0).unwrap();
        let right = network.variable(1).unwrap();
        let root = gate(&mut network, left, right);
        network.freeze();
        (network, root)
    }

    #[test]
    fn installs_multiple_choices_once_and_structurally_shares_them() {
        let (baseline, baseline_root) = graph(LogicGraph::and);
        let (first, first_root) = graph(LogicGraph::xor);
        let (second, second_root) = graph(LogicGraph::xor);
        let outcome = finish(
            identity(baseline),
            &[baseline_root],
            vec![
                ChoiceProposal {
                    pass: "first",
                    network: first,
                    roots: Box::new([first_root]),
                },
                ChoiceProposal {
                    pass: "second",
                    network: second,
                    roots: Box::new([second_root]),
                },
            ],
        )
        .unwrap();

        assert_eq!(outcome.alternatives.len(), 2);
        assert_eq!(outcome.alternatives[0].roots, outcome.alternatives[1].roots);
        assert!(remap_literal(&outcome.remap, baseline_root).is_some());
    }

    #[test]
    fn small_support_installs_one_equivalent_mapper_choice() {
        let mut network = LogicGraph::new();
        let a = network.variable(0).unwrap();
        let b = network.variable(1).unwrap();
        let c = network.variable(2).unwrap();
        let shared = network.and(a, b);
        let roots = [network.xor(shared, c), network.and(shared, c.inverted())];
        network.freeze();
        let expected = roots.map(|root| network.truth_table(root, 3));

        let outcome = optimize(
            network,
            &roots,
            &[None, None],
            true,
            crate::SynthesisDiagnostics::default(),
            crate::test_runtime(),
            None,
        )
        .unwrap();
        let actual_roots = map_roots(&outcome.remap, &roots).unwrap();

        assert_eq!(outcome.alternatives.len(), 1);
        for (&actual, expected) in actual_roots.iter().zip(expected) {
            assert_eq!(outcome.network.truth_table(actual, 3), expected);
        }
        for (&actual, expected) in outcome.alternatives[0].roots.iter().zip(expected) {
            assert_eq!(outcome.network.truth_table(actual, 3), expected);
        }
    }

    #[test]
    fn mux_decomposition_preserves_a_wide_multi_output_graph() {
        let mut source = LogicGraph::new();
        let inputs = (0..9)
            .map(|origin| source.variable(origin).unwrap())
            .collect::<Vec<_>>();
        let left = source.mux(inputs[0], inputs[1], inputs[2]);
        let right = source.mux(inputs[3], inputs[4], inputs[5]);
        let roots = [
            source.mux(inputs[6], left, right),
            source.xor(left, inputs[7]),
            source.mux(inputs[8], right, left),
        ];
        source.freeze();
        let (decomposed, decomposed_roots) = decompose_muxes(&source, &roots)
            .unwrap()
            .expect("test graph contains genuine MUX nodes");
        let proof = prove_logic_network_equivalence(
            source.storage_network(),
            &roots.map(LogicNodeId::lit),
            decomposed.storage_network(),
            &decomposed_roots
                .iter()
                .copied()
                .map(LogicNodeId::lit)
                .collect::<Vec<_>>(),
        )
        .expect("formal engine accepts the decomposition miter");

        assert!(proof.require_proved().is_ok());
    }
}
