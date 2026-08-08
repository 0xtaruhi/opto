// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    CutDatabase, ExecutionContext, HashMap, KCut, LogicGraph, LogicNode, LogicNodeId, TruthTable,
};

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
    let observed = cut_list
        .iter()
        .flat_map(|cut| cut.leaves().iter().copied())
        .collect::<Vec<_>>();
    let tables = network.truth_tables_for_inputs(node, base.leaves(), &observed);
    let mut coverage = CoverageCheck::new(network, base.leaves());
    let cares = cut_list
        .iter()
        .map(|cut| {
            if cut.leaves() == base.leaves() {
                return u64::MAX;
            }
            if !cut
                .leaves()
                .iter()
                .all(|leaf| coverage.covered(*leaf) == Some(true))
            {
                return u64::MAX;
            }
            tables
                .care_projection(node, cut.leaves())
                .map_or(u64::MAX, |(_, care)| care)
        })
        .collect::<Box<[u64]>>();
    Some(cares)
}

pub(crate) struct CoverageCheck<'a> {
    network: &'a LogicGraph,
    seeds: hashbrown::HashSet<usize>,
    memo: HashMap<usize, bool>,
    budget: usize,
}

impl<'a> CoverageCheck<'a> {
    pub(crate) fn new(network: &'a LogicGraph, seeds: &[LogicNodeId]) -> Self {
        Self {
            network,
            seeds: seeds.iter().map(|seed| seed.index()).collect(),
            memo: HashMap::new(),
            budget: COVERAGE_NODE_BUDGET,
        }
    }

    pub(crate) fn covered(&mut self, start: LogicNodeId) -> Option<bool> {
        let mut stack = vec![(start.positive(), false)];
        while let Some((node, expanded)) = stack.pop() {
            let key = node.index();
            if self.memo.contains_key(&key) {
                continue;
            }
            if self.seeds.contains(&key) {
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
    entries: Box<[SupportEntry]>,
    entry_ranges: HashMap<KCut, SupportRange>,
    truths: Box<[TruthTable]>,
    truth_ranges: Box<[TruthRange]>,
}

#[derive(Debug, Clone, Copy)]
struct SupportRange {
    start: u32,
    len: u32,
}

#[derive(Debug, Clone, Copy)]
struct TruthRange {
    start: u32,
    len: u8,
}

impl SupportIndex {
    pub(super) fn entries<'index>(
        &'index self,
        key: &[u32],
    ) -> impl Iterator<Item = &'index (u32, u64)> + 'index {
        let range = KCut::from_indices(key)
            .and_then(|key| self.entry_ranges.get(&key))
            .map_or(0..0, |range| {
                let start = range.start as usize;
                start..start + range.len as usize
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
                if cut.contains(node) || cut.len() < 2 {
                    truths.push(network.truth_table_for_cut(node, cut));
                    continue;
                }
                let truth = network.truth_table_for_cut(node, cut);
                truths.push(truth);
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
    let range_count = usize::from(!entries.is_empty())
        + entries
            .windows(2)
            .filter(|pair| pair[0].key != pair[1].key)
            .count();
    let mut entry_ranges = HashMap::with_capacity(range_count);
    let mut start = 0usize;
    while start < entries.len() {
        let key = entries[start].key;
        let end = entries[start..]
            .partition_point(|entry| entry.key == key)
            .checked_add(start)
            .ok_or_else(|| crate::SynthError::capacity("support range end"))?;
        let range = SupportRange {
            start: start
                .try_into()
                .map_err(|_| crate::SynthError::capacity("support range start"))?,
            len: (end - start)
                .try_into()
                .map_err(|_| crate::SynthError::capacity("support range length"))?,
        };
        if entry_ranges.insert(key, range).is_some() {
            return Err(crate::SynthError::invariant(
                "support index contains a duplicate key range",
            ));
        }
        start = end;
    }
    Ok(SupportIndex {
        entries: entries.into_boxed_slice(),
        entry_ranges,
        truths: truths.into_boxed_slice(),
        truth_ranges: truth_ranges.into_boxed_slice(),
    })
}
