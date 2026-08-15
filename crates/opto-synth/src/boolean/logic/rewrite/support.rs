// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CutDatabase, ExecutionContext, HashMap, KCut, LogicGraph, LogicNode, LogicNodeId, TruthTable,
};
use crate::boolean::logic::MAX_MATCH_INPUTS as MAX_CUT_LEAVES;

const COVERAGE_NODE_BUDGET: usize = 256;

pub(crate) fn window_cares(
    network: &LogicGraph,
    cuts: &CutDatabase,
    node: LogicNodeId,
) -> Option<Box<[u64]>> {
    let cut_list = cuts.cuts(node);
    let base = cut_list
        .iter()
        .copied()
        .filter(|cut| !cut.contains(node) && cut.len() >= 2)
        .max_by_key(|cut| cut.len())?;
    let mut coverage = CoverageCheck::new(network, base.leaves());
    let projected = projected_cuts(&mut coverage, cut_list, |cut| cut.leaves() == base.leaves());
    let observed = projected_leaves(cut_list, &projected).collect::<Vec<_>>();
    let tables = network.truth_tables_for_inputs(node, base.leaves(), &observed);
    let cares = cut_list
        .iter()
        .zip(projected.iter())
        .map(|(cut, &projected)| {
            if !projected {
                return u64::MAX;
            }
            tables
                .care_projection(node, cut.leaves())
                .map_or(u64::MAX, |(_, care)| care)
        })
        .collect::<Box<[u64]>>();
    Some(cares)
}

/// Marks cuts contained by the window without evaluating outside its inputs.
pub(crate) fn projected_cuts(
    coverage: &mut CoverageCheck<'_>,
    cuts: &[KCut],
    mut skip: impl FnMut(&KCut) -> bool,
) -> Box<[bool]> {
    cuts.iter()
        .map(|cut| {
            !skip(cut)
                && cut
                    .leaves()
                    .iter()
                    .all(|leaf| coverage.covered(*leaf) == Some(true))
        })
        .collect()
}

/// Iterates leaves of cuts contained by the window.
pub(crate) fn projected_leaves<'cuts>(
    cuts: &'cuts [KCut],
    projected: &'cuts [bool],
) -> impl Iterator<Item = LogicNodeId> + 'cuts {
    cuts.iter()
        .zip(projected)
        .filter(|&(_, &projected)| projected)
        .flat_map(|(cut, _)| cut.leaves().iter().copied())
}

/// Bounded, memoized cone-containment checker for one cut-sized seed set.
pub(crate) struct CoverageCheck<'a> {
    network: &'a LogicGraph,
    seeds: smallvec::SmallVec<[usize; MAX_CUT_LEAVES]>,
    memo: HashMap<usize, bool>,
    stack: Vec<(LogicNodeId, bool)>,
    budget: usize,
}

impl<'a> CoverageCheck<'a> {
    pub(crate) fn new(network: &'a LogicGraph, seeds: &[LogicNodeId]) -> Self {
        Self {
            network,
            seeds: seeds.iter().map(|seed| seed.index()).collect(),
            memo: HashMap::new(),
            stack: Vec::new(),
            budget: COVERAGE_NODE_BUDGET,
        }
    }

    fn is_seed(&self, node: usize) -> bool {
        self.seeds.contains(&node)
    }

    pub(crate) fn covered(&mut self, start: LogicNodeId) -> Option<bool> {
        let mut stack = std::mem::take(&mut self.stack);
        stack.clear();
        stack.push((start.positive(), false));
        let answer = self.walk(&mut stack, start);
        self.stack = stack;
        answer
    }

    fn walk(&mut self, stack: &mut Vec<(LogicNodeId, bool)>, start: LogicNodeId) -> Option<bool> {
        while let Some((node, expanded)) = stack.pop() {
            let key = node.index();
            if self.memo.contains_key(&key) {
                continue;
            }
            if self.is_seed(key) {
                self.memo.insert(key, true);
                continue;
            }
            let stored = self.network.node(node);
            match stored {
                LogicNode::Const(_) => {
                    self.memo.insert(key, true);
                }
                LogicNode::Var(_) => {
                    self.memo.insert(key, false);
                }
                _ if !expanded => {
                    self.budget = self.budget.checked_sub(1)?;
                    stack.push((node, true));
                    for fanin in stored.fanins() {
                        stack.push((fanin.positive(), false));
                    }
                }
                _ => {
                    let all = stored.fanins().all(|fanin| {
                        self.seeds.contains(&fanin.index())
                            || self.memo.get(&fanin.index()).copied().unwrap_or(false)
                    });

                    self.memo.insert(key, all);
                }
            }
        }
        self.memo.get(&start.index()).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SupportEntry {
    key: KCut,
    value: (u32, u64),
}

pub(super) struct SupportIndex {
    /// Support entries sorted by key for binary range lookup.
    entries: Box<[SupportEntry]>,
    /// Exact-negative filter; set bits still require an `entries` lookup.
    key_filter: Box<[u64]>,
    truths: Box<[TruthTable]>,
    truth_ranges: Box<[TruthRange]>,
}

/// Mixes one dense leaf index for order-independent XOR combination.
pub(super) const fn leaf_fingerprint(leaf: u32) -> u64 {
    let mut value = (leaf as u64)
        .wrapping_add(1)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    value ^= value >> 29;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^ (value >> 32)
}

/// Combines one support set's fingerprints without order dependence.
pub(super) fn subset_fingerprint(leaves: impl IntoIterator<Item = u32>) -> u64 {
    leaves
        .into_iter()
        .fold(0, |value, leaf| value ^ leaf_fingerprint(leaf))
}

/// Deterministic filter density per distinct support key.
const KEY_FILTER_BITS_PER_KEY: usize = 32;
const MINIMUM_KEY_FILTER_WORDS: usize = 64;

#[derive(Debug, Clone, Copy)]
struct TruthRange {
    start: u32,
    len: u8,
}

impl SupportIndex {
    /// Returns `false` only when the fingerprint is definitely absent.
    pub(super) fn may_contain(&self, fingerprint: u64) -> bool {
        let bits = self.key_filter.len() * u64::BITS as usize;
        let index = usize::try_from(fingerprint & (bits as u64 - 1))
            .expect("filter length is a usize, so the masked index fits one");
        self.key_filter[index / u64::BITS as usize] & (1 << (index % u64::BITS as usize)) != 0
    }

    pub(super) fn entries<'index>(
        &'index self,
        key: &[u32],
    ) -> impl Iterator<Item = &'index (u32, u64)> + 'index {
        let range = KCut::from_indices(key).map_or(0..0, |key| {
            let start = self.entries.partition_point(|entry| entry.key < key);
            let end = self.entries[start..].partition_point(|entry| entry.key == key) + start;
            start..end
        });
        self.entries[range].iter().map(|entry| &entry.value)
    }

    pub(super) fn truth(&self, node: LogicNodeId, cut: usize) -> TruthTable {
        let range = self.truth_ranges[node.index()];
        assert!(cut < usize::from(range.len));
        self.truths[range.start as usize + cut]
    }
}
pub(super) const REWRITE_CUTS_PER_NODE: usize = 32;

pub(super) fn build_support_index(
    network: &LogicGraph,
    cuts: &CutDatabase,
    references: &[u32],
    runtime: &ExecutionContext,
) -> Result<SupportIndex, crate::SynthError> {
    let node_count = network.node_count();
    let shards = runtime.fold_indexed(
        node_count,
        || (Vec::new(), Vec::new(), Vec::new()),
        |(entries, truths, truth_lengths), index| {
            let reference_count = references[index];
            let node = LogicNodeId::from_index(index);
            if !network.node(node).is_gate() || reference_count == 0 {
                truth_lengths.push(0);
                return Ok::<_, crate::SynthError>(());
            }
            let truth_count = cuts.cuts(node).len().try_into().map_err(|_| {
                crate::SynthError::capacity("support truth range exceeds compact capacity")
            })?;
            for cut in cuts.cuts(node).iter().copied() {
                let self_cut = cut.contains(node);
                let truth = if self_cut {
                    // Preserve the skipped self-cut row without evaluating it.
                    TruthTable {
                        input_count: cut.len(),
                        bits: 0,
                    }
                } else {
                    network.truth_table_for_cut(node, cut)
                };
                truths.push(truth);
                if self_cut || cut.len() < 2 {
                    continue;
                }
                entries.push(SupportEntry {
                    key: cut,
                    value: (
                        u32::try_from(index)
                            .expect("logic node index is bounded by compact graph storage"),
                        truth.bits,
                    ),
                });
            }
            truth_lengths.push(truth_count);
            Ok(())
        },
    )?;
    let entry_count = shards.iter().map(|(entries, _, _)| entries.len()).sum();
    let truth_count = shards.iter().map(|(_, truths, _)| truths.len()).sum();
    let mut entries = Vec::with_capacity(entry_count);
    let mut truths = Vec::with_capacity(truth_count);
    let mut truth_ranges = Vec::with_capacity(node_count);
    for (chunk_entries, chunk_truths, chunk_lengths) in shards {
        let mut start = truths.len();
        for len in chunk_lengths {
            truth_ranges.push(TruthRange {
                start: start.try_into().map_err(|_| {
                    crate::SynthError::capacity("support truth arena exceeds 32-bit capacity")
                })?,
                len,
            });
            start += usize::from(len);
        }
        truths.extend(chunk_truths);
        entries.extend(chunk_entries);
    }
    runtime.sort_unstable(&mut entries);
    let key_count = usize::from(!entries.is_empty())
        + entries
            .windows(2)
            .filter(|pair| pair[0].key != pair[1].key)
            .count();
    let filter_words = (key_count * KEY_FILTER_BITS_PER_KEY)
        .div_ceil(u64::BITS as usize)
        .next_power_of_two()
        .max(MINIMUM_KEY_FILTER_WORDS);
    let mut key_filter = vec![0u64; filter_words];
    let filter_bits = filter_words * u64::BITS as usize;
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 && entries[index - 1].key == entry.key {
            continue;
        }
        let fingerprint = subset_fingerprint(entry.key.leaves().iter().map(|leaf| {
            u32::try_from(leaf.index()).expect("logic node index is bounded by compact storage")
        }));
        let bit = usize::try_from(fingerprint & (filter_bits as u64 - 1))
            .expect("filter length is a usize, so the masked index fits one");
        key_filter[bit / u64::BITS as usize] |= 1 << (bit % u64::BITS as usize);
    }
    Ok(SupportIndex {
        entries: entries.into_boxed_slice(),
        key_filter: key_filter.into_boxed_slice(),
        truths: truths.into_boxed_slice(),
        truth_ranges: truth_ranges.into_boxed_slice(),
    })
}
