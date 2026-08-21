// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Static AXM pass scheduling.

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::rewrite::{RewriteIncremental, remap_literal};
use hashbrown::HashMap;
use opto_runtime::ExecutionContext;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyStyle {
    Preserve,
    DecomposeMux,
}

pub(super) struct LogicPipelineOutcome {
    pub(super) network: LogicGraph,
    pub(super) remap: Box<[Option<LogicNodeId>]>,
    /// Proven alternatives keyed by the selected positive node.
    pub(super) alternatives: opto_core::PackedRows<LogicNodeId>,
}

/// MUX expansion rounds, including one retry after normalization.
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
        return finish(identity(source), roots);
    }
    let mut state = TransformState::start(roots, identity(source))?;
    let reduction = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(diagnostics.timing, || {
            "logic.pipeline.functional_reduction".to_string()
        });
        reduce_functionally(&state.network, &state.roots, diagnostics, runtime)?
    };
    if let Some(reduction) = reduction {
        state.apply(reduction)?;
    }
    let canonical = {
        let _profile = crate::api::diagnostics::ProfileSpan::new(diagnostics.timing, || {
            "logic.pipeline.canonical_optimization".to_string()
        });
        optimize_canonical(
            &state.network,
            &state.roots,
            requirements,
            diagnostics,
            runtime,
            incremental,
        )?
    };
    let _profile = crate::api::diagnostics::ProfileSpan::new(diagnostics.timing, || {
        "logic.pipeline.finalization".to_string()
    });
    finish_with_choices(&state, &canonical, roots)
}

/// Runs one SAT sweep, returning `None` when it proves no substitution.
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

/// Optimizes the canonical path, expanding MUX structure before normalization.
fn optimize_canonical(
    source: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: Option<RewriteIncremental<'_>>,
) -> Result<TransformProduct, crate::SynthError> {
    let trace = crate::api::diagnostics::SynthTrace::timing(diagnostics);
    let started = std::time::Instant::now();
    let mut state = TransformState::start(roots, copy_active(source, roots)?)?;
    crate::api::diagnostics::trace!(
        trace,
        "logic.pipeline.copy_active",
        "wall={:?}",
        started.elapsed()
    );
    let mut expanded = false;
    for round in 0..MUX_DECOMPOSITION_ROUNDS {
        let started = std::time::Instant::now();
        let Some(decomposition) = decompose_muxes(&state.network, &state.roots)? else {
            break;
        };
        state.apply(decomposition)?;
        crate::api::diagnostics::trace!(
            trace,
            "logic.pipeline.mux_decomposition",
            "round={round} wall={:?}",
            started.elapsed()
        );
        expanded = true;
        let started = std::time::Instant::now();
        let optimized = optimize_with(
            &state.network,
            &state.roots,
            requirements,
            diagnostics,
            runtime,
            None,
            OptimizationPolicy::Factored,
        )?;
        state.apply(optimized)?;
        crate::api::diagnostics::trace!(
            trace,
            "logic.pipeline.mux_optimization",
            "round={round} wall={:?}",
            started.elapsed()
        );
    }
    if expanded {
        return Ok(state.finish());
    }
    // A subject with no MUX still needs the baseline optimization once.
    optimize_with(
        source,
        roots,
        requirements,
        diagnostics,
        runtime,
        incremental,
        OptimizationPolicy::Baseline,
    )
}

/// Expands MUX nodes, returning `None` when no expansion is needed.
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

/// Rewrite policy for one optimization pass.
#[derive(Clone, Copy)]
pub(super) enum OptimizationPolicy {
    Baseline,
    Factored,
}

/// Optimizes one implementation under `policy`.
pub(super) fn optimize_with(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    requirements: &[Option<f64>],
    diagnostics: crate::SynthesisDiagnostics,
    runtime: &ExecutionContext,
    incremental: Option<RewriteIncremental<'_>>,
    policy: OptimizationPolicy,
) -> Result<TransformProduct, crate::SynthError> {
    let cache = super::rewrite::RewriteRecipeCache::default();
    let metrics = crate::incremental::IncrementalRunMetrics::default();
    let incremental = incremental.unwrap_or_else(|| RewriteIncremental::new(&cache, &metrics));
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
    canonical: TransformProduct,
    source_roots: &[LogicNodeId],
) -> Result<LogicPipelineOutcome, crate::SynthError> {
    map_roots(&canonical.remap, source_roots)?;
    let node_count = canonical.network.node_count();
    Ok(LogicPipelineOutcome {
        network: canonical.network,
        remap: canonical.remap,
        alternatives: opto_core::PackedRows::try_from_rows(
            (0..node_count).map(|_| Vec::new()).collect(),
        )
        .expect("logic node count fits the packed choice index"),
    })
}

/// Seals the reduced and canonical implementations into one arena. The
/// canonical transform's remap is the equivalence certificate: a retained
/// baseline literal and its mapped canonical literal implement the same
/// function. Exact structural duplicates collapse in the shared builder.
fn finish_with_choices(
    baseline: &TransformState,
    canonical: &TransformProduct,
    source_roots: &[LogicNodeId],
) -> Result<LogicPipelineOutcome, crate::SynthError> {
    let canonical_roots = map_roots(&canonical.remap, &baseline.roots)?;
    let baseline_live = baseline.network.live_nodes(&baseline.roots);
    let canonical_live = canonical.network.live_nodes(&canonical_roots);
    let mut network = LogicGraph::new();
    let mut variables = HashMap::new();
    let baseline_to_choice = copy_graph(
        &baseline.network,
        Some(&baseline_live),
        &mut network,
        &mut variables,
        CopyStyle::Preserve,
    )?;
    let canonical_to_choice = copy_graph(
        &canonical.network,
        Some(&canonical_live),
        &mut network,
        &mut variables,
        CopyStyle::Preserve,
    )?;
    network.freeze();

    let mut alternatives = vec![Vec::new(); network.node_count()];
    for index in 0..baseline.network.node_count() {
        let Some(baseline_node) = baseline_to_choice[index] else {
            continue;
        };
        if !baseline
            .network
            .node(LogicNodeId::from_index(index))
            .is_gate()
        {
            continue;
        }
        let Some(canonical_node) =
            canonical.remap[index].and_then(|node| remap_literal(&canonical_to_choice, node))
        else {
            continue;
        };
        let alternative = if canonical_node.is_inverted() {
            baseline_node.inverted()
        } else {
            baseline_node
        };
        let representative = canonical_node.positive();
        if alternative != representative
            && !alternatives[representative.index()].contains(&alternative)
        {
            alternatives[representative.index()].push(alternative);
        }
    }
    for row in &mut alternatives {
        row.sort_unstable();
    }
    let alternatives = opto_core::PackedRows::try_from_rows(alternatives)
        .map_err(|_| crate::SynthError::capacity("Boolean choice alternatives exceed capacity"))?;
    let baseline_to_canonical = compose_remaps(&baseline.remap, &canonical.remap);
    let remap = baseline_to_canonical
        .iter()
        .map(|&node| node.and_then(|node| remap_literal(&canonical_to_choice, node)))
        .collect();
    let outcome = LogicPipelineOutcome {
        network,
        remap,
        alternatives,
    };
    map_roots(&outcome.remap, source_roots)?;
    Ok(outcome)
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
            .expect("canonical optimization succeeds")
        };

        let serial = run(1);
        let parallel = run(4);
        assert_eq!(serial.remap, parallel.remap);
        for index in 0..serial.network.node_count() {
            let node = LogicNodeId::from_index(index);
            assert_eq!(serial.network.node(node), parallel.network.node(node));
        }
    }

    #[test]
    fn retained_choices_are_formally_equivalent_to_their_representatives() {
        let mut source = LogicGraph::new();
        let inputs = (0..7)
            .map(|origin| source.variable(origin).unwrap())
            .collect::<Vec<_>>();
        let mut root = inputs[0];
        for index in 0..24 {
            let then_value = source.xor(root, inputs[(index + 1) % inputs.len()]);
            let else_value = source.and(root, inputs[(index + 2) % inputs.len()]);
            root = source.mux(inputs[index % inputs.len()], then_value, else_value);
        }
        source.freeze();
        let runtime = ExecutionContext::new(&opto_runtime::ExecutionConfig { max_threads: 2 })
            .expect("test runtime is valid");
        let choices = optimize(
            source,
            &[root],
            &[None],
            true,
            crate::SynthesisDiagnostics::default(),
            &runtime,
            None,
        )
        .expect("choice construction succeeds");
        let pair = (0..choices.network.node_count()).find_map(|index| {
            choices.alternatives[index]
                .first()
                .copied()
                .map(|alternative| (LogicNodeId::from_index(index), alternative))
        });
        let (representative, alternative) =
            pair.expect("MUX decomposition retains a distinct proved implementation");
        let proof = prove_logic_network_equivalence(
            choices.network.storage_network(),
            &[representative.lit()],
            choices.network.storage_network(),
            &[alternative.lit()],
        )
        .expect("formal engine accepts the choice miter");
        assert!(proof.require_proved().is_ok());
    }
}
