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

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyStyle {
    Preserve,
    DecomposeMux,
}

/// Expansion rounds in the canonical path. Normalization can synthesize a fresh
/// MUX from expanded structure, so one retry is allowed and no more.
const MUX_DECOMPOSITION_ROUNDS: usize = 2;

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
    // One destructive state owns the whole path, so every node map composes
    // through the same mechanism and callers only ever see their own node
    // space. Functional reduction runs first, so the canonical optimization and
    // every retained alternative are built from one duplicate-free subject
    // rather than rediscovering the same equal cones.
    let mut state = TransformState::start(roots, identity(source))?;
    if let Some(reduction) = reduce_functionally(&state.network, &state.roots, diagnostics, runtime)?
    {
        state.apply(reduction)?;
    }
    let functional =
        small_support_choice(&state.network, &state.roots, requirements, diagnostics, runtime)?;
    let canonical = optimize_canonical(
        &state.network,
        &state.roots,
        requirements,
        diagnostics,
        runtime,
        incremental,
    )?;
    state.apply(canonical)?;
    finish(state.finish(), roots, functional.into_iter().collect())
}

/// Runs one SAT sweep over the freshly lowered subject.
///
/// Returns `None` when the sweep proved nothing, which lets the caller skip a
/// composition rather than compose an identity.
fn reduce_functionally(
    source: &LogicGraph,
    roots: &[LogicNodeId],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
) -> Result<Option<TransformProduct>, crate::SynthError> {
    let started = std::time::Instant::now();
    let mut metrics = super::sweep::SweepMetrics::default();
    let before = source.node_count();
    let reduced = super::sweep::reduce(source, roots, runtime, &mut metrics)?;
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
    Ok(reduced)
}

/// Optimizes the one canonical AXM implementation.
///
/// MUX decomposition is part of this path rather than a competing branch. A
/// genuine MUX node is expanded into AND/inverter structure so cover can share
/// NAND/NOR across what was one atom, and the ordinary normalizer runs after
/// each expansion. Normalization can synthesize a fresh MUX, so the expansion is
/// retried once; the bound is a fixed round budget, not a fixpoint.
///
/// Optimizing an un-expanded implementation alongside this one doubled every
/// rewrite, cut, truth, and cover pass to produce an alternative that mapping
/// then discarded. Cover still selects MUX cells, because it matches cut truth
/// tables against the target library rather than AXM node kinds.
fn optimize_canonical(
    source: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: Option<RewriteIncremental<'_>>,
) -> Result<TransformProduct, crate::SynthError> {
    let mut state = TransformState::start(roots, copy_active(source, roots)?)?;
    let mut expanded = false;
    for _ in 0..MUX_DECOMPOSITION_ROUNDS {
        let Some(decomposition) = decompose_muxes(&state.network, &state.roots)? else {
            break;
        };
        state.apply(decomposition)?;
        expanded = true;
        let optimized = optimize_factored(
            &state.network,
            &state.roots,
            requirements,
            diagnostics,
            runtime,
        )?;
        state.apply(optimized)?;
    }
    if expanded {
        return Ok(state.finish());
    }
    // A subject with no MUX node never entered the loop, so it has not been
    // optimized yet. The baseline policy owns that case because it also runs the
    // global sharing census.
    optimize_baseline(
        source,
        roots,
        requirements,
        diagnostics,
        runtime,
        incremental,
    )
}

/// Expands every genuine MUX node into AND/inverter structure.
///
/// Returns `None` when the subject has no MUX node, which is the signal that
/// the canonical path has nothing left to expand.
fn decompose_muxes(
    source: &LogicGraph,
    roots: &[LogicNodeId],
) -> Result<Option<TransformProduct>, crate::SynthError> {
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
    map_roots(&remap, roots)?;
    network.freeze();
    Ok(Some(TransformProduct {
        network,
        remap,
        analyses: TransformAnalyses::default(),
    }))
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

/// Installs the canonical implementation and every retained alternative into one
/// hash-consed subject.
///
/// Alternatives share that subject's nodes and inputs, so cut, truth, and match
/// analysis is computed once over the union rather than once per structure.
fn finish(
    canonical: TransformProduct,
    source_roots: &[LogicNodeId],
    alternatives: Vec<ChoiceProposal>,
) -> Result<LogicPipelineOutcome, crate::SynthError> {
    if alternatives.is_empty() {
        map_roots(&canonical.remap, source_roots)?;
        return Ok(LogicPipelineOutcome {
            network: canonical.network,
            remap: canonical.remap,
            alternatives: Box::new([]),
        });
    }

    let mut network = LogicGraph::new();
    let mut variables = HashMap::new();
    let canonical_remap = copy_graph(
        &canonical.network,
        None,
        &mut network,
        &mut variables,
        CopyStyle::Preserve,
    )?;
    let remap = compose_remaps(&canonical.remap, &canonical_remap);
    map_roots(&remap, source_roots)?;

    let mut installed = Vec::with_capacity(alternatives.len());
    for alternative in alternatives {
        let live = alternative.network.live_nodes(&alternative.roots);
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
    let live = network.live_nodes(roots);
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
        let decomposed = decompose_muxes(&source, &roots)
            .unwrap()
            .expect("test graph contains genuine MUX nodes");
        let decomposed_roots = map_roots(&decomposed.remap, &roots).unwrap();
        let proof = prove_logic_network_equivalence(
            source.storage_network(),
            &roots.map(LogicNodeId::lit),
            decomposed.network.storage_network(),
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
    fn canonical_optimization_is_deterministic_across_worker_counts() {
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
