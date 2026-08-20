// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Simulation-guided SAT sweeping over one frozen AXM subject.
//!
//! Simulation only nominates equivalences; `opto-formal` must prove each
//! substitution. Stable stimulus, class, representative, and shard ordering
//! make the result independent of worker scheduling.

use super::network::{LogicGraph, LogicNode, LogicNodeId};
use super::pipeline::{TransformAnalyses, TransformProduct};
use hashbrown::HashMap;
use opto_runtime::ExecutionContext;

/// Random simulation words per node; each word carries 64 patterns.
const RANDOM_WORDS: usize = 8;

/// Words reserved for boundary assignments learned from refutations.
const LEARNED_WORDS: usize = 96;

/// Maximum simulation/proof refinement rounds.
const MAX_REFINEMENT_ROUNDS: usize = 8;

/// Representative re-election rounds inside one proof call.
const MAX_REPRESENTATIVE_ROUNDS: usize = 2;

/// Proved-or-refuted pair budget for one refinement round.
const MAX_ROUND_PAIRS: usize = 4_000;

/// Total pair budget for one subject.
const MAX_PROOF_PAIRS: usize = 24_000;

/// Largest transitive logic cone admitted to one incremental SAT instance.
const MAX_PROOF_ENCODING_NODES: usize = 8_192;

/// Classes per proof shard; each shard owns one solver encoding.
const SHARD_CLASSES: usize = 12;

/// Largest candidate class swept in one round.
const MAX_CLASS_MEMBERS: usize = 16;

/// One node's disposition after sweeping: the earlier node it collapses into,
/// and whether the collapse inverts its phase.
#[derive(Clone, Copy)]
struct Substitution {
    target: u32,
    inverted: bool,
}

/// Diagnostic counts reported by one sweep.
#[derive(Clone, Copy, Default)]
pub(super) struct SweepMetrics {
    pub(super) rounds: usize,
    pub(super) classes: usize,
    pub(super) candidates: usize,
    pub(super) proved: usize,
    pub(super) refuted: usize,
    pub(super) budget_exhausted: bool,
}

/// Returns a proven reduction whose remap is expressed in `network`'s space.
pub(super) fn reduce(
    network: &LogicGraph,
    roots: &[LogicNodeId],
    runtime: &ExecutionContext,
    metrics: &mut SweepMetrics,
) -> Result<Option<TransformProduct>, crate::SynthError> {
    let mut stimulus = Stimulus::random();
    let mut budget = MAX_PROOF_PAIRS;
    let mut reduced: Option<TransformProduct> = None;
    let mut roots = roots.to_vec();
    let mut subject = network;
    let mut live = live_cone(subject, &roots);
    let mut signatures = Signatures::new(subject.node_count());
    let mut resume = 0usize;

    for _ in 0..MAX_REFINEMENT_ROUNDS {
        if budget == 0 {
            metrics.budget_exhausted = true;
            break;
        }
        metrics.rounds += 1;
        let mut substitutions = vec![None; subject.node_count()].into_boxed_slice();
        simulate(subject, &live, &stimulus, &mut signatures, resume);
        let classes = nominate(subject, &live, &substitutions, &signatures, metrics);
        if classes.is_empty() {
            break;
        }
        let round = prove(
            subject,
            &classes,
            MAX_ROUND_PAIRS.min(budget),
            runtime,
            &mut substitutions,
        )?;
        debug_assert!(round.attempted() <= budget);
        budget = budget.saturating_sub(round.attempted());
        metrics.proved += round.proved;
        metrics.refuted += round.refutations.len();
        metrics.budget_exhausted |= round.encoding_budget_exhausted;
        if round.proved != 0 {
            let product = rebuild(subject, &live, &substitutions);
            signatures = signatures.projected(&product.remap, product.network.node_count());
            roots = roots
                .iter()
                .map(|&root| mapped_literal(&product.remap, root))
                .collect();
            reduced = Some(match &reduced {
                Some(previous) => compose(previous, product),
                None => product,
            });
            subject = &reduced.as_ref().expect("a merge just produced one").network;
            live = live_cone(subject, &roots);
        }
        if round.refutations.is_empty() {
            break;
        }
        let Some(changed) = stimulus.learn(&round.refutations) else {
            break;
        };
        resume = changed;
    }

    Ok(reduced)
}

/// The live cone of the roots, retaining the constant as a representative.
fn live_cone(network: &LogicGraph, roots: &[LogicNodeId]) -> Box<[bool]> {
    let mut live = network.live_nodes(roots);
    live[0] = true;
    live
}

fn compose(first: &TransformProduct, second: TransformProduct) -> TransformProduct {
    TransformProduct {
        remap: super::pipeline::compose_remaps(&first.remap, &second.remap),
        network: second.network,
        analyses: TransformAnalyses::default(),
    }
}

/// Deterministic random stimulus plus learned boundary assignments.
struct Stimulus {
    /// Assigned origins for each learned pattern, sorted by origin.
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

    /// Appends refutations and returns the first changed word.
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

    /// Builds one input word, retaining random bits for unassigned origins.
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

/// Deterministic random word keyed only by boundary origin and word index.
fn random_word(origin: u32, word: usize) -> u64 {
    let mut value = (u64::from(origin) << 32)
        ^ (word as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ 0xd1b5_4a32_d192_ed03;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Fixed-stride simulation rows that permit incremental stimulus extension.
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

    /// Projects active rows onto a merged node space.
    fn projected(&self, remap: &[Option<LogicNodeId>], node_count: usize) -> Self {
        let mut projected = Self::new(node_count);
        projected.active = self.active;
        for (node, mapped) in remap.iter().enumerate() {
            let Some(mapped) = *mapped else {
                continue;
            };
            let source = node * SIGNATURE_STRIDE;
            let target = mapped.index() * SIGNATURE_STRIDE;
            for word in 0..self.active {
                let value = self.values[source + word];
                projected.values[target + word] = if mapped.is_inverted() { !value } else { value };
            }
        }
        projected
    }
}

/// Fills stimulus words `from..` while retaining earlier results.
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
        // True is the complemented constant-zero literal, not a distinct node.
        if matches!(network.node(node), LogicNode::Const(true)) {
            continue;
        }
        let mut key = signatures.row(index).to_vec();
        // Normalize phase so a node and its complement nominate one class.
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
            members.truncate(MAX_CLASS_MEMBERS);
            members
        })
        .collect::<Vec<_>>();
    // Restore stable order after hash-map collection.
    classes.sort_unstable_by_key(|members| members[0].0);
    metrics.classes = metrics.classes.max(classes.len());
    metrics.candidates += classes
        .iter()
        .map(|members| members.len() - 1)
        .sum::<usize>();
    classes
        .into_iter()
        .map(|members| Class { members })
        .collect()
}

/// Partitions the round budget exactly across shards.
fn shard_quota(max_pairs: usize, shard_count: usize, shard: usize) -> usize {
    let shard_count = shard_count.max(1);
    max_pairs / shard_count + usize::from(shard < max_pairs % shard_count)
}

/// What one proof round established.
struct Round {
    proved: usize,
    refutations: Vec<opto_formal::BoundaryRefutation>,
    encoding_budget_exhausted: bool,
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
                    if inverted {
                        literal.inverted()
                    } else {
                        literal
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    // Contiguous stable shards own independent solver encodings.
    let shard_size = SHARD_CLASSES.min(literals.len().max(1));
    let shard_count = literals.len().div_ceil(shard_size);
    let shards =
        runtime.analyze_indexed_with_grain(shard_count, std::num::NonZeroUsize::MIN, |shard| {
            let start = shard * shard_size;
            let end = (start + shard_size).min(literals.len());
            let mut shard_refutations = Vec::new();
            let partitions = opto_formal::prove_logic_literal_partitions(
                network.storage_network(),
                &literals[start..end],
                MAX_REPRESENTATIVE_ROUNDS,
                shard_quota(max_pairs, shard_count, shard),
                MAX_PROOF_ENCODING_NODES,
                &mut shard_refutations,
            )
            .map_err(|error| {
                crate::SynthError::invariant(format!(
                    "AXM functional reduction proof failed: {error}"
                ))
            })?;
            let encoding_budget_exhausted = partitions.is_none();
            let partitions = partitions.unwrap_or_else(|| {
                literals[start..end]
                    .iter()
                    .map(|class| vec![None; class.len()])
                    .collect()
            });
            Ok::<_, crate::SynthError>((partitions, shard_refutations, encoding_budget_exhausted))
        })?;
    let mut partitions = Vec::with_capacity(classes.len());
    let mut refutations = Vec::new();
    let mut encoding_budget_exhausted = false;
    for (shard_partitions, shard_refutations, shard_budget_exhausted) in shards {
        partitions.extend(shard_partitions);
        refutations.extend(shard_refutations);
        encoding_budget_exhausted |= shard_budget_exhausted;
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
        encoding_budget_exhausted,
    })
}

/// Resolves a lower-ID substitution chain and its accumulated phase.
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
    let mapped = remap[literal.index()].expect("AXM graph is topological within its live cone");
    if literal.is_inverted() {
        mapped.inverted()
    } else {
        mapped
    }
}

#[cfg(test)]
mod tests;
