// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Static AXM pass scheduling and graph-choice composition.

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::rewrite::{RewriteIncremental, remap_literal};
use hashbrown::HashMap;
use opto_runtime::{ExecutionContext, Task, TaskKey};

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
type ProposalTransform =
    fn(&LogicGraph, &[LogicNodeId]) -> Result<Option<LogicRoots>, crate::SynthError>;

#[derive(Clone, Copy)]
struct ProposalSpec {
    pass: &'static str,
    transform: ProposalTransform,
    round_budget: u8,
    optimization: OptimizationPolicy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyStyle {
    Preserve,
    DecomposeMux,
}

const MUX_DECOMPOSITION: ProposalSpec = ProposalSpec {
    pass: "mux_decomposition",
    transform: decompose_muxes,
    round_budget: 2,
    optimization: OptimizationPolicy::Factored,
};

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
    // Functional reduction runs before the portfolio so every implementation is
    // built from one duplicate-free subject instead of rediscovering the same
    // equal cones independently.
    let reduced = reduce_functionally(source, roots, diagnostics, runtime)?;
    let source = reduced.network;
    let roots: &[LogicNodeId] = reduced.roots.as_deref().unwrap_or(roots);
    let functional = small_support_choice(&source, roots, requirements, diagnostics, runtime)?;
    let mut products = runtime.map_ordered_composite(
        vec![
            Task::new(TaskKey::new(7, 0), OptimizationTask::Baseline)
                .with_estimated_work(source.node_count() as u64),
            Task::new(
                TaskKey::new(7, 1),
                OptimizationTask::Proposal(MUX_DECOMPOSITION),
            )
            .with_estimated_work(source.node_count() as u64),
        ],
        |task, nested| match task {
            OptimizationTask::Baseline => optimize_baseline(
                &source,
                roots,
                requirements,
                diagnostics,
                nested,
                incremental,
            )
            .map(OptimizationProduct::Baseline),
            OptimizationTask::Proposal(spec) => {
                build_proposal(spec, &source, roots, requirements, diagnostics, nested)
                    .map(OptimizationProduct::Proposal)
            }
        },
    )?;
    let OptimizationProduct::Baseline(baseline) = products.remove(0) else {
        return Err(crate::SynthError::invariant(
            "AXM optimization portfolio returned products out of order",
        ));
    };
    let OptimizationProduct::Proposal(proposal) = products.remove(0) else {
        return Err(crate::SynthError::invariant(
            "AXM optimization portfolio returned products out of order",
        ));
    };
    let mut alternatives = functional.into_iter().collect::<Vec<_>>();
    alternatives.extend(proposal);
    let mut outcome = finish(baseline, roots, alternatives)?;
    if let Some(reduction) = &reduced.remap {
        outcome.remap = compose_remaps(reduction, &outcome.remap);
    }
    Ok(outcome)
}

/// Runs one SAT sweep over the freshly lowered subject.
///
/// Returns the reduced graph, its roots, and the remap from the caller's node
/// space into the reduced space. The remap is composed back into the pipeline
/// outcome so callers never observe the intermediate node space. When the sweep
/// finds nothing, the original graph and roots are returned unchanged and no
/// composition is required.
fn reduce_functionally(
    source: LogicGraph,
    roots: &[LogicNodeId],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<ReducedSubject, crate::SynthError> {
    let started = std::time::Instant::now();
    let mut metrics = super::sweep::SweepMetrics::default();
    let before = source.node_count();
    let reduced = super::sweep::reduce(&source, roots, runtime, &mut metrics)?;
    crate::api::diagnostics::trace!(
        crate::api::diagnostics::SynthTrace::timing(diagnostics),
        "logic.sweep",
        "nodes={before}->{} rounds={} classes={} candidates={} proved={} refuted={} \
         exhausted={} wall={:?}",
        reduced
            .as_ref()
            .map_or(before, |product| product.network.node_count()),
        metrics.rounds,
        metrics.classes,
        metrics.candidates,
        metrics.proved,
        metrics.refuted,
        metrics.budget_exhausted,
        started.elapsed()
    );
    let Some(reduced) = reduced else {
        return Ok(ReducedSubject {
            network: source,
            roots: None,
            remap: None,
        });
    };
    let reduced_roots = map_roots(&reduced.remap, roots)?;
    Ok(ReducedSubject {
        network: reduced.network,
        roots: Some(reduced_roots),
        remap: Some(reduced.remap),
    })
}

/// The subject after functional reduction.
///
/// `roots` and `remap` are absent exactly when the sweep changed nothing, which
/// lets the caller keep borrowing its original roots and skip one remap
/// composition.
struct ReducedSubject {
    network: LogicGraph,
    roots: Option<Box<[LogicNodeId]>>,
    remap: Option<Box<[Option<LogicNodeId>]>>,
}

#[derive(Clone, Copy)]
enum OptimizationTask {
    Baseline,
    Proposal(ProposalSpec),
}

enum OptimizationProduct {
    Baseline(TransformProduct),
    Proposal(Option<ChoiceProposal>),
}

fn build_proposal(
    spec: ProposalSpec,
    source: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<Option<ChoiceProposal>, crate::SynthError> {
    let mut transformed = (spec.transform)(source, roots)?;
    let mut proposal = None;
    for round in 0..spec.round_budget {
        let Some((network, roots)) = transformed else {
            break;
        };
        let cache = super::rewrite::RewriteRecipeCache::default();
        let metrics = crate::incremental::IncrementalRunMetrics::default();
        let optimized = optimize_with(
            &network,
            &roots,
            requirements,
            diagnostics,
            runtime,
            RewriteIncremental::new(&cache, &metrics),
            spec.optimization,
        )?;
        let roots = map_roots(&optimized.remap, &roots)?;
        let candidate = ChoiceProposal {
            pass: spec.pass,
            network: optimized.network,
            roots,
        };
        transformed = if round + 1 < spec.round_budget {
            (spec.transform)(&candidate.network, &candidate.roots)?
        } else {
            None
        };
        proposal = Some(candidate);
    }
    Ok(proposal)
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
            incremental,
            OptimizationPolicy::Baseline,
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
            RewriteIncremental::new(&cache, &metrics),
            OptimizationPolicy::Baseline,
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
        RewriteIncremental::new(&cache, &metrics),
        OptimizationPolicy::Factored,
    )
}

#[derive(Clone, Copy)]
enum OptimizationPolicy {
    Baseline,
    Factored,
}

fn optimize_with(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: RewriteIncremental<'_>,
    policy: OptimizationPolicy,
) -> Result<TransformProduct, crate::SynthError> {
    if roots.len() != requirements.len() {
        return Err(crate::SynthError::invariant(
            "AXM pass requirements do not align with roots",
        ));
    }
    let mut state = TransformState::start(roots, copy_active(network, roots)?)?;
    let rewrite = match policy {
        OptimizationPolicy::Baseline => super::rewrite::resynthesize,
        OptimizationPolicy::Factored => super::rewrite::normalize,
    };
    rewrite(&mut state, requirements, diagnostics, runtime, incremental)?;
    state.network.freeze();
    state.analyses = TransformAnalyses::default();

    let started = std::time::Instant::now();
    let balanced = super::balance::balance(&state.network, &state.roots);
    let balanced_roots = map_roots(&balanced.remap, &state.roots)?;
    let profile = |network: &LogicGraph, roots: &[LogicNodeId]| {
        if matches!(policy, OptimizationPolicy::Baseline)
            || requirements.iter().any(Option::is_some)
        {
            return super::rewrite::timing_profile(network, roots, requirements);
        }
        roots.iter().fold((0, 0), |(maximum, total), &root| {
            let depth = network.level(root);
            (maximum.max(depth), total + u64::from(depth))
        })
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

    #[test]
    fn optimization_portfolio_is_deterministic_across_worker_counts() {
        let build = || {
            let mut source = LogicGraph::new();
            let inputs = (0..6)
                .map(|origin| source.variable(origin).unwrap())
                .collect::<Vec<_>>();
            let mut root = inputs[0];
            for index in 0..24 {
                let then_value = source.xor(root, inputs[(index + 1) % inputs.len()]);
                let else_value = source.and(root, inputs[(index + 2) % inputs.len()]);
                root = source.mux(inputs[index % inputs.len()], then_value, else_value);
            }
            source.freeze();
            (source, root)
        };
        let run = |max_threads| {
            let (source, root) = build();
            let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads })
                .expect("test runtime is valid");
            optimize(
                source,
                &[root],
                &[None],
                true,
                crate::SynthesisDiagnostics::default(),
                &runtime,
                None,
            )
            .expect("optimization portfolio succeeds")
        };

        let serial = run(1);
        let parallel = run(4);
        assert_eq!(serial.remap, parallel.remap);
        assert_eq!(serial.alternatives.len(), parallel.alternatives.len());
        for index in 0..serial.network.node_count() {
            let node = LogicNodeId::from_index(index);
            assert_eq!(serial.network.node(node), parallel.network.node(node));
        }
        for (serial, parallel) in serial.alternatives.iter().zip(&parallel.alternatives) {
            assert_eq!(serial.pass, parallel.pass);
            assert_eq!(serial.roots, parallel.roots);
        }
    }
}
