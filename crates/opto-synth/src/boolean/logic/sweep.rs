// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Simulation-guided SAT sweeping over one frozen AXM subject.
//!
//! Structural hash consing in [`LogicGraph`] merges only syntactically identical
//! nodes. Bit lowering, operator recipes, and independently rewritten cones all
//! produce nodes that compute the same function through different structure, and
//! those duplicates survive rewriting because a local window never observes the
//! distant twin. This pass removes them once, before cut enumeration and cover,
//! so the mapper sees each function exactly once.
//!
//! Ownership and phase. The pass owns no persistent state. It reads one frozen
//! graph plus its roots and returns one [`TransformProduct`] whose remap is
//! composed by [`TransformState`] like any other destructive pass. It runs
//! before the optimization portfolio so every implementation shares its result.
//!
//! Determinism. Simulation vectors come from a fixed seed and depend only on the
//! input origin and the word index, never on node identity, worker count, or
//! iteration order. Candidate classes are emitted in ascending node order and
//! each class elects its lowest-ID member as representative, so the substitution
//! set does not depend on which proof finishes first.
//!
//! Soundness. Equal simulation signatures only nominate a pair. A substitution
//! is installed exclusively for a pair whose miter `opto-formal` proved
//! unsatisfiable; a refuted or budget-exhausted pair leaves both nodes intact.

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::pipeline::{TransformAnalyses, TransformProduct};
use hashbrown::HashMap;
use opto_runtime::ExecutionContext;

/// Random simulation words per node. Each word carries 64 input patterns, so
/// the pass filters candidate pairs with 512 random patterns before any solver
/// call.
const RANDOM_WORDS: usize = 8;

/// Words reserved for patterns learned from refutations. Every refuted pair
/// contributes the boundary assignment that separates it, and one such pattern
/// usually splits a whole class of biased nodes that random stimulus could not
/// separate.
const LEARNED_WORDS: usize = 96;

/// Refinement rounds. Each round nominates from the current stimulus, proves a
/// bounded number of pairs, and folds the refutations back into the stimulus.
const MAX_REFINEMENT_ROUNDS: usize = 8;

/// Representative rounds inside one proof call. A later round re-elects a
/// representative from the members the previous round refuted.
const MAX_REPRESENTATIVE_ROUNDS: usize = 2;

/// Proved-or-refuted pair budget for one refinement round.
const MAX_ROUND_PAIRS: usize = 4_000;

/// Total pair budget for one subject. Exhausting it leaves the remaining
/// classes unmerged and is reported, never silently merged.
const MAX_PROOF_PAIRS: usize = 24_000;

/// Classes per parallel proof shard. Each shard encodes its own CNF, so a small
/// shard wastes encoding work while a large one serializes solving; this is the
/// measured balance for subjects in the ten-thousand-node range.
const SHARD_CLASSES: usize = 12;

/// Largest candidate class the pass will sweep in one round. A very wide class
/// is dominated by refutations that stimulus refinement should separate first,
/// and its pair count is quadratic in the member count.
const MAX_CLASS_MEMBERS: usize = 16;

/// One node's disposition after sweeping: the earlier node it collapses into,
/// and whether the collapse inverts its phase.
#[derive(Clone, Copy)]
struct Substitution {
    target: u32,
    inverted: bool,
}

/// Counts reported by one sweep. Every field is a diagnostic; none of them
/// participates in a synthesis decision.
#[derive(Clone, Copy, Default)]
pub(super) struct SweepMetrics {
    pub(super) rounds: usize,
    pub(super) classes: usize,
    pub(super) candidates: usize,
    pub(super) proved: usize,
    pub(super) refuted: usize,
    pub(super) budget_exhausted: bool,
}

/// Sweeps `network` and returns the reduced graph, or `None` when simulation
/// nominates no candidate pair at all.
///
/// The returned product's remap is expressed in `network`'s node space, so the
/// caller composes it exactly like a rewrite product.
pub(super) fn reduce(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    runtime: &ExecutionContext,
    metrics: &mut SweepMetrics,
) -> Result<Option<TransformProduct>, crate::SynthError> {
    let mut live = network.live_nodes(roots);
    // The constant is a class representative even when no root reaches it, so a
    // cone proved constant can collapse onto it.
    live[0] = true;
    let mut stimulus = Stimulus::random();
    let mut substitutions = vec![None; network.node_count()].into_boxed_slice();
    let mut budget = MAX_PROOF_PAIRS;
    let mut signatures = Signatures::new(network.node_count());
    let mut resume = 0usize;

    for _ in 0..MAX_REFINEMENT_ROUNDS {
        if budget == 0 {
            metrics.budget_exhausted = true;
            break;
        }
        metrics.rounds += 1;
        simulate(network, &live, &stimulus, &mut signatures, resume);
        let classes = nominate(network, &live, &substitutions, &signatures, metrics);
        if classes.is_empty() {
            break;
        }
        let round = prove(
            network,
            &classes,
            MAX_ROUND_PAIRS.min(budget),
            runtime,
            &mut substitutions,
        )?;
        budget -= round.attempted().min(budget);
        metrics.proved += round.proved;
        metrics.refuted += round.refutations.len();
        if round.refutations.is_empty() {
            break;
        }
        let Some(changed) = stimulus.learn(&round.refutations) else {
            // No room left for learned patterns; another round would nominate
            // exactly the same classes and repeat the same refutations.
            break;
        };
        resume = changed;
    }

    if metrics.proved == 0 {
        return Ok(None);
    }
    Ok(Some(rebuild(network, &live, &substitutions)))
}

/// The stimulus applied to boundary inputs.
///
/// The random half is a pure function of origin and word index, so it never
/// depends on node identity, construction order, or worker count. The learned
/// half accumulates the boundary assignments that separated refuted pairs, in
/// the order the prover reported them; because the prover is driven by an
/// order-stable nomination, that order is itself stable.
struct Stimulus {
    /// One entry per learned pattern: the origins the solver assigned and their
    /// values, in ascending origin order.
    learned: Vec<Vec<(u32, bool)>>,
}

impl Stimulus {
    fn random() -> Self {
        Self {
            learned: Vec::new(),
        }
    }

    fn words(&self) -> usize {
        RANDOM_WORDS + self.learned.len().div_ceil(u64::BITS as usize)
    }

    /// Appends the boundary assignments of `refutations`.
    ///
    /// Returns the index of the first stimulus word whose content changed, or
    /// `None` when the learned budget is full and nothing was appended. Every
    /// earlier word is immutable once written, which is what lets simulation
    /// resume instead of restarting.
    fn learn(&mut self, refutations: &[opto_formal::BoundaryRefutation]) -> Option<usize> {
        let capacity = LEARNED_WORDS * u64::BITS as usize;
        let before = self.learned.len();
        for refutation in refutations {
            if self.learned.len() == capacity {
                break;
            }
            self.learned.push(refutation.assignment().to_vec());
        }
        (self.learned.len() > before).then(|| RANDOM_WORDS + before / u64::BITS as usize)
    }

    /// Builds the complete stimulus word for one boundary origin.
    ///
    /// A learned pattern that left this origin unassigned falls back to the
    /// random bit for that position, so an unconstrained input keeps varying
    /// instead of collapsing every learned pattern onto the same value.
    fn input_word(&self, origin: u32, word: usize) -> u64 {
        if word < RANDOM_WORDS {
            return random_word(origin, word);
        }
        let start = (word - RANDOM_WORDS) * u64::BITS as usize;
        let mut bits = random_word(origin, word);
        for offset in 0..u64::BITS as usize {
            let Some(pattern) = self.learned.get(start + offset) else {
                break;
            };
            if let Ok(position) = pattern.binary_search_by_key(&origin, |&(origin, _)| origin) {
                let mask = 1u64 << offset;
                if pattern[position].1 {
                    bits |= mask;
                } else {
                    bits &= !mask;
                }
            }
        }
        bits
    }
}

/// Deterministic random word. The pattern for one input depends only on its
/// boundary origin and the word index, so two runs, two worker counts, and two
/// node orderings all simulate the same stimulus.
fn random_word(origin: u32, word: usize) -> u64 {
    let mut value = (u64::from(origin) << 32)
        ^ (word as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ 0xd1b5_4a32_d192_ed03;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Simulation signatures, one fixed-stride row per node.
///
/// The stride is the maximum stimulus width rather than the current one, so a
/// later round appends words without moving any earlier value. That is what
/// makes refinement incremental: learning only ever appends patterns, so
/// resuming at the first changed word reproduces exactly the signatures a full
/// re-simulation would produce.
struct Signatures {
    values: Vec<u64>,
    active: usize,
}

const SIGNATURE_STRIDE: usize = RANDOM_WORDS + LEARNED_WORDS;

impl Signatures {
    fn new(node_count: usize) -> Self {
        Self {
            values: vec![0u64; node_count * SIGNATURE_STRIDE],
            active: 0,
        }
    }

    fn row(&self, node: usize) -> &[u64] {
        let base = node * SIGNATURE_STRIDE;
        &self.values[base..base + self.active]
    }
}

/// Fills stimulus words `from..` for every live node, leaving earlier words as
/// the previous round computed them.
fn simulate(
    network: &LogicGraph,
    live: &[bool],
    stimulus: &Stimulus,
    signatures: &mut Signatures,
    from: usize,
) {
    let node_count = network.node_count();
    let words = stimulus.words();
    debug_assert!(words <= SIGNATURE_STRIDE);
    signatures.active = words;
    if from >= words {
        return;
    }
    let values = &mut signatures.values;
    for (index, &live) in live.iter().enumerate().take(node_count) {
        if !live {
            continue;
        }
        let node = LogicNodeId::from_index(index);
        let base = index * SIGNATURE_STRIDE;
        match network.node(node) {
            LogicNode::Const(false) => values[base + from..base + words].fill(0),
            LogicNode::Const(true) => values[base + from..base + words].fill(u64::MAX),
            LogicNode::Var(origin) => {
                for word in from..words {
                    values[base + word] = stimulus.input_word(origin, word);
                }
            }
            LogicNode::And(left, right) => {
                for word in from..words {
                    values[base + word] =
                        operand(values, left, word) & operand(values, right, word);
                }
            }
            LogicNode::Xor(left, right) => {
                for word in from..words {
                    values[base + word] =
                        operand(values, left, word) ^ operand(values, right, word);
                }
            }
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => {
                for word in from..words {
                    let select = operand(values, cond, word);
                    values[base + word] = (select & operand(values, then_value, word))
                        | (!select & operand(values, else_value, word));
                }
            }
        }
    }
}

fn operand(signatures: &[u64], literal: LogicNodeId, word: usize) -> u64 {
    let bits = signatures[literal.index() * SIGNATURE_STRIDE + word];
    if literal.is_inverted() { !bits } else { bits }
}

/// One nominated class: phase-normalized members in ascending node order.
struct Class {
    members: Vec<(LogicNodeId, bool)>,
}

fn nominate(
    network: &LogicGraph,
    live: &[bool],
    substitutions: &[Option<Substitution>],
    signatures: &Signatures,
    metrics: &mut SweepMetrics,
) -> Vec<Class> {
    let mut buckets: HashMap<Box<[u64]>, Vec<(LogicNodeId, bool)>> = HashMap::new();
    for (index, (&live, substitution)) in live.iter().zip(substitutions).enumerate() {
        if !live || substitution.is_some() {
            continue;
        }
        let node = LogicNodeId::from_index(index);
        // Constants and inputs are nominated as well as gates. A gate that is
        // provably constant or provably a projection of one input removes its
        // whole cone, which is the largest single win available to this pass,
        // and index order makes the constant or input the class representative.
        // The constant-true literal is the complement of node zero and never
        // exists as its own node, so nominating it would duplicate the constant
        // class.
        if matches!(network.node(node), LogicNode::Const(true)) {
            continue;
        }
        let mut key = signatures.row(index).to_vec();
        // Normalize by the phase of the first simulated pattern so a node and its
        // complement nominate one class instead of two.
        let inverted = key[0] & 1 == 1;
        if inverted {
            for word in &mut key {
                *word = !*word;
            }
        }
        buckets
            .entry(key.into_boxed_slice())
            .or_default()
            .push((node, inverted));
    }

    let mut classes = buckets
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|mut members| {
            // A class wider than the per-round bound is truncated to its
            // lowest-ID members. Truncation is deterministic and the remainder
            // returns in the next round once refinement has split it.
            members.truncate(MAX_CLASS_MEMBERS);
            members
        })
        .collect::<Vec<_>>();
    // Members arrive in ascending node order because the scan is index-ordered.
    // Sorting classes by their first member gives the whole nomination a stable
    // total order independent of hash iteration.
    classes.sort_unstable_by_key(|members| members[0].0);
    metrics.classes = metrics.classes.max(classes.len());
    metrics.candidates += classes.iter().map(|members| members.len() - 1).sum::<usize>();
    classes
        .into_iter()
        .map(|members| Class { members })
        .collect()
}

/// What one proof round established.
struct Round {
    proved: usize,
    refutations: Vec<opto_formal::BoundaryRefutation>,
}

impl Round {
    fn attempted(&self) -> usize {
        self.proved + self.refutations.len()
    }
}

fn prove(
    network: &LogicGraph,
    classes: &[Class],
    max_pairs: usize,
    runtime: &ExecutionContext,
    substitutions: &mut [Option<Substitution>],
) -> Result<Round, crate::SynthError> {
    let literals = classes
        .iter()
        .map(|class| {
            class
                .members
                .iter()
                .map(|&(node, inverted)| {
                    let literal = node.lit();
                    if inverted { literal.inverted() } else { literal }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    // Classes are independent proof problems over one immutable network, so each
    // shard owns its own solver. Shards are contiguous ranges of the stable
    // class order and their results are reassembled in that order, which keeps
    // the substitution set independent of completion order and worker count.
    let shard_size = SHARD_CLASSES.min(literals.len().max(1));
    let shard_count = literals.len().div_ceil(shard_size);
    let shard_pairs = max_pairs.div_ceil(shard_count.max(1));
    let shards = runtime.analyze_indexed_with_grain(
        shard_count,
        std::num::NonZeroUsize::MIN,
        |shard| {
            let start = shard * shard_size;
            let end = (start + shard_size).min(literals.len());
            let mut shard_refutations = Vec::new();
            let partitions = opto_formal::prove_logic_literal_partitions(
                network.storage_network(),
                &literals[start..end],
                MAX_REPRESENTATIVE_ROUNDS,
                shard_pairs,
                &mut shard_refutations,
            )
            .map_err(|error| {
                crate::SynthError::invariant(format!(
                    "AXM functional reduction proof failed: {error}"
                ))
            })?;
            Ok::<_, crate::SynthError>((partitions, shard_refutations))
        },
    )?;
    let mut partitions = Vec::with_capacity(classes.len());
    let mut refutations = Vec::new();
    for (shard_partitions, shard_refutations) in shards {
        partitions.extend(shard_partitions);
        refutations.extend(shard_refutations);
    }

    let mut proved = 0usize;
    for (class, representatives) in classes.iter().zip(&partitions) {
        for (member, representative) in representatives.iter().enumerate() {
            let Some(representative) = *representative else {
                continue;
            };
            let (node, inverted) = class.members[member];
            let (target, target_inverted) = class.members[representative];
            debug_assert!(target.index() < node.index());
            // A representative may itself have collapsed in an earlier round.
            // Following that chain keeps every substitution target a surviving
            // node, so the rebuild never dereferences a removed node.
            let (target, target_inverted) =
                resolve(substitutions, target, inverted != target_inverted);
            substitutions[node.index()] = Some(Substitution {
                target: u32::try_from(target.index())
                    .expect("logic node index is bounded by compact graph storage"),
                inverted: target_inverted,
            });
            proved += 1;
        }
    }
    Ok(Round {
        proved,
        refutations,
    })
}

/// Follows an existing substitution chain to the surviving node, accumulating
/// phase. Chains are acyclic because every substitution points at a strictly
/// lower node index.
fn resolve(
    substitutions: &[Option<Substitution>],
    node: LogicNodeId,
    inverted: bool,
) -> (LogicNodeId, bool) {
    let mut node = node;
    let mut inverted = inverted;
    while let Some(substitution) = substitutions[node.index()] {
        node = LogicNodeId::from_index(substitution.target as usize);
        inverted ^= substitution.inverted;
    }
    (node, inverted)
}

fn rebuild(
    network: &LogicGraph,
    live: &[bool],
    substitutions: &[Option<Substitution>],
) -> TransformProduct {
    let mut reduced = LogicGraph::new();
    let mut variables = HashMap::new();
    let mut remap = vec![None; network.node_count()];
    for index in 0..network.node_count() {
        if !live[index] {
            continue;
        }
        if let Some(substitution) = substitutions[index] {
            let target = remap[substitution.target as usize]
                .expect("functional reduction elects an earlier live representative");
            remap[index] = Some(if substitution.inverted {
                LogicGraph::not(target)
            } else {
                target
            });
            continue;
        }
        let node = LogicNodeId::from_index(index);
        let mapped = match network.node(node) {
            LogicNode::Const(value) => LogicGraph::constant(value),
            LogicNode::Var(origin) => *variables.entry(origin).or_insert_with(|| {
                reduced
                    .variable(origin as usize)
                    .expect("AXM input stays within compact capacity")
            }),
            LogicNode::And(left, right) => {
                let left = mapped_literal(&remap, left);
                let right = mapped_literal(&remap, right);
                reduced.and(left, right)
            }
            LogicNode::Xor(left, right) => {
                let left = mapped_literal(&remap, left);
                let right = mapped_literal(&remap, right);
                reduced.xor(left, right)
            }
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => {
                let cond = mapped_literal(&remap, cond);
                let then_value = mapped_literal(&remap, then_value);
                let else_value = mapped_literal(&remap, else_value);
                reduced.mux(cond, then_value, else_value)
            }
        };
        remap[index] = Some(mapped);
    }
    reduced.freeze();
    TransformProduct {
        network: reduced,
        remap: remap.into_boxed_slice(),
        analyses: TransformAnalyses::default(),
    }
}

fn mapped_literal(remap: &[Option<LogicNodeId>], literal: LogicNodeId) -> LogicNodeId {
    let mapped =
        remap[literal.index()].expect("AXM graph is topological within its live cone");
    if literal.is_inverted() {
        mapped.inverted()
    } else {
        mapped
    }
}

#[cfg(test)]
mod tests;
