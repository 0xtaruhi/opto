// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::network::{LogicGraph, LogicNode, LogicNodeId, MAX_CUT_LEAVES, MAX_CUTS_PER_NODE};
use opto_runtime::ExecutionContext;

use crate::boolean::logic::TruthTable;

const CUT_ANALYSIS_CHUNK_ITEMS: usize = 4096;

fn cut_segment_count(levels: &[Vec<usize>]) -> Result<usize, crate::SynthError> {
    levels.iter().try_fold(0usize, |count, level| {
        count
            .checked_add(level.len().div_ceil(CUT_ANALYSIS_CHUNK_ITEMS))
            .ok_or_else(|| crate::SynthError::capacity("logic cut segment count overflow"))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct KCut {
    len: u8,
    leaves: [LogicNodeId; MAX_CUT_LEAVES],
}

impl KCut {
    fn empty() -> Self {
        Self {
            len: 0,
            leaves: [LogicNodeId::CONSTANT; MAX_CUT_LEAVES],
        }
    }

    fn singleton(node: LogicNodeId) -> Self {
        let mut cut = Self::empty();
        cut.leaves[0] = node.positive();
        cut.len = 1;
        cut
    }

    #[cfg(test)]
    pub(crate) fn from_leaves(leaves: &[LogicNodeId]) -> Option<Self> {
        let mut cut = Self::empty();
        for leaf in leaves {
            cut.insert_leaf(*leaf)?;
        }
        Some(cut)
    }

    pub(crate) fn from_indices(leaves: &[u32]) -> Option<Self> {
        let mut cut = Self::empty();
        for &leaf in leaves {
            cut.insert_leaf(LogicNodeId::from_index(leaf as usize))?;
        }
        Some(cut)
    }

    pub(crate) fn len(self) -> usize {
        self.len as usize
    }

    pub(crate) fn leaves(&self) -> &[LogicNodeId] {
        &self.leaves[..self.len()]
    }

    pub(crate) fn contains(self, node: LogicNodeId) -> bool {
        self.leaves().contains(&node.positive())
    }

    fn merge(self, other: Self, max_leaves: usize) -> Option<Self> {
        let mut merged = Self::empty();
        for leaf in self.leaves().iter().chain(other.leaves()) {
            merged.insert_leaf(*leaf)?;
            if merged.len() > max_leaves {
                return None;
            }
        }
        Some(merged)
    }

    fn insert_leaf(&mut self, leaf: LogicNodeId) -> Option<()> {
        let leaf = leaf.positive();
        if self.leaves().contains(&leaf) {
            return Some(());
        }
        let len = self.len();
        if len == MAX_CUT_LEAVES {
            return None;
        }
        let insert_at = self
            .leaves()
            .iter()
            .position(|existing| leaf < *existing)
            .unwrap_or(len);
        for index in (insert_at..len).rev() {
            self.leaves[index + 1] = self.leaves[index];
        }
        self.leaves[insert_at] = leaf;
        self.len += 1;
        Some(())
    }

    fn rank(self) -> (u8, [LogicNodeId; MAX_CUT_LEAVES]) {
        (self.len, self.leaves)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CutSet {
    len: u8,
    cap: u8,
    cuts: [KCut; MAX_CUTS_PER_NODE],
}

impl Default for CutSet {
    fn default() -> Self {
        Self::with_cap(MAX_CUTS_PER_NODE)
    }
}

impl CutSet {
    fn with_cap(cap: usize) -> Self {
        debug_assert!((1..=MAX_CUTS_PER_NODE).contains(&cap));
        Self {
            len: 0,
            cap: u8::try_from(cap).expect("cut-set capacity is bounded by MAX_CUTS_PER_NODE"),
            cuts: [KCut::empty(); MAX_CUTS_PER_NODE],
        }
    }
}

impl CutSet {
    pub(crate) fn iter(&self) -> impl Iterator<Item = KCut> + '_ {
        self.cuts[..self.len()].iter().copied()
    }

    pub(crate) fn len(self) -> usize {
        self.len as usize
    }

    fn as_slice(&self) -> &[KCut] {
        &self.cuts[..self.len()]
    }

    #[cfg(test)]
    fn from_slice(cuts: &[KCut]) -> Self {
        assert!(cuts.len() <= MAX_CUTS_PER_NODE);
        let mut set = Self::default();
        set.cuts[..cuts.len()].copy_from_slice(cuts);
        set.len = u8::try_from(cuts.len()).expect("test cut set is bounded by its fixed storage");
        set
    }

    fn insert(&mut self, candidate: KCut) {
        if self.iter().any(|existing| existing == candidate) {
            return;
        }
        if self.len() < usize::from(self.cap) {
            self.cuts[self.len()] = candidate;
            self.len += 1;
        } else if let Some(worst_index) = self.worst_cut_index()
            && candidate.rank() < self.cuts[worst_index].rank()
        {
            self.cuts[worst_index] = candidate;
        }
        self.sort();
    }

    fn sort(&mut self) {
        let len = self.len();
        self.cuts[..len].sort_by_key(|cut| cut.rank());
    }

    fn worst_cut_index(&self) -> Option<usize> {
        self.iter()
            .enumerate()
            .max_by_key(|(_, cut)| cut.rank())
            .map(|(index, _)| index)
    }
}

#[derive(Debug)]
pub(crate) struct CutDatabase {
    rows: opto_core::PackedRows<KCut>,
}

#[derive(Debug)]
pub(crate) struct CutTruthDatabase {
    rows: opto_core::PackedRows<TruthTable>,
}

impl CutTruthDatabase {
    pub(crate) fn build_parallel(
        network: &LogicGraph,
        cuts: &CutDatabase,
        runtime: &ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        let rows = runtime.analyze_indexed(network.node_count(), |index| {
            let node = LogicNodeId::from_index(index);
            Ok::<_, crate::SynthError>(
                cuts.cuts(node)
                    .iter()
                    .copied()
                    .map(|cut| {
                        if cut.contains(node) {
                            TruthTable {
                                input_count: cut.len(),
                                bits: 0,
                            }
                        } else {
                            network.truth_table_for_cut(node, cut)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })?;
        Ok(Self {
            rows: opto_core::PackedRows::try_from_rows(rows)
                .map_err(|_| crate::SynthError::capacity("cut truth database exceeds capacity"))?,
        })
    }

    pub(crate) fn truth(&self, node: LogicNodeId, cut: usize) -> TruthTable {
        self.rows[node.index()][cut]
    }
}

#[derive(Clone, Copy)]
pub(crate) struct IncrementalCutInputs<'a> {
    pub(crate) previous: &'a CutDatabase,
    pub(crate) old_to_new: &'a [Option<LogicNodeId>],
    pub(crate) new_to_old: &'a [Option<u32>],
    pub(crate) old_predecessors: &'a [[Option<u32>; 3]],
    pub(crate) check_incremental: bool,
}

impl CutDatabase {
    #[cfg(test)]
    pub(crate) fn build(network: &LogicGraph, max_leaves: usize) -> Self {
        network.cut_database(max_leaves, MAX_CUTS_PER_NODE)
    }

    pub(crate) fn build_parallel(
        network: &LogicGraph,
        max_leaves: usize,
        runtime: &ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        network.parallel_cut_database(max_leaves, MAX_CUTS_PER_NODE, runtime)
    }

    pub(crate) fn build_with_cut_cap_parallel(
        network: &LogicGraph,
        max_leaves: usize,
        max_cuts: usize,
        runtime: &ExecutionContext,
    ) -> Result<Self, crate::SynthError> {
        network.parallel_cut_database(max_leaves, max_cuts, runtime)
    }

    pub(crate) fn build_incremental(
        network: &LogicGraph,
        max_leaves: usize,
        max_cuts: usize,
        inputs: IncrementalCutInputs<'_>,
        runtime: &ExecutionContext,
    ) -> Result<(Self, Box<[bool]>), crate::SynthError> {
        let IncrementalCutInputs {
            previous,
            old_to_new,
            new_to_old,
            old_predecessors,
            check_incremental,
        } = inputs;
        assert_eq!(network.node_count(), new_to_old.len());
        assert_eq!(network.node_count(), old_predecessors.len());
        let node_count = network.node_count();
        let mut levels = vec![Vec::new(); network.max_level() + 1];
        for index in 0..node_count {
            levels[network.level(LogicNodeId::from_index(index)) as usize].push(index);
        }
        let segment_count = cut_segment_count(&levels)?;
        let mut arena = Vec::<Box<[KCut]>>::with_capacity(segment_count);
        let mut ranges = vec![CutRange::UNINITIALIZED; node_count];
        let mut unchanged = vec![false; node_count];
        let mut reused = vec![false; node_count];
        for nodes in levels {
            let segment_base = arena.len();
            let mut level_segments = Vec::new();
            let mut level_updates = Vec::with_capacity(nodes.len());
            runtime.analyze_indexed_chunks(
                nodes.len(),
                CUT_ANALYSIS_CHUNK_ITEMS,
                |position| {
                    let index = nodes[position];
                    let node = network.node(LogicNodeId::from_index(index));
                    let fanins_unchanged = node.fanins().all(|fanin| unchanged[fanin.index()]);
                    let predecessors_correspond = node
                        .fanins()
                        .zip(old_predecessors[index].iter().flatten())
                        .all(|(new, old)| new_to_old[new.index()] == Some(*old));
                    let rank_preserved = fanins_unchanged
                        && predecessors_correspond
                        && previous.translation_preserves_predecessor_rank(
                            &old_predecessors[index],
                            old_to_new,
                        );
                    let translated = new_to_old[index].and_then(|old| {
                        previous.translate_set(
                            LogicNodeId::from_index(old as usize),
                            max_cuts,
                            old_to_new,
                        )
                    });
                    if rank_preserved && translated.is_some() && check_incremental {
                        let recomputed = network.cut_set_from_predecessors(
                            index, max_leaves, max_cuts, &ranges, &arena,
                        );
                        assert_eq!(
                            translated,
                            Some(recomputed),
                            "translated cuts are not reusable at new node {index}, old node {:?}, old predecessors {:?}",
                            new_to_old[index],
                            old_predecessors[index]
                        );
                    }
                    let set = if rank_preserved {
                        translated.unwrap_or_else(|| {
                            network.cut_set_from_predecessors(
                                index, max_leaves, max_cuts, &ranges, &arena,
                            )
                        })
                    } else {
                        network.cut_set_from_predecessors(
                            index, max_leaves, max_cuts, &ranges, &arena,
                        )
                    };
                    Ok::<_, crate::SynthError>((
                        index,
                        set,
                        translated.is_some_and(|translated| translated == set),
                        rank_preserved && translated.is_some(),
                    ))
                },
                        |_, updates| {
                    let segment = segment_base
                        .checked_add(level_segments.len())
                        .ok_or_else(|| {
                            crate::SynthError::capacity("logic cut segment index overflow")
                        })?;
                    let packed_len = updates.iter().try_fold(
                        0usize,
                        |count, (_, set, _, _)| {
                            count.checked_add(set.len()).ok_or_else(|| {
                                crate::SynthError::capacity("logic cut arena size overflow")
                            })
                        },
                    )?;
                    let mut packed = Vec::with_capacity(packed_len);
                    for (index, set, node_unchanged, node_reused) in updates {
                        let range = CutRange::append_at(&mut packed, segment, &set);
                        level_updates.push((index, range, node_unchanged, node_reused));
                    }
                    level_segments.push(packed.into_boxed_slice());
                    Ok::<_, crate::SynthError>(())
                },
            )?;
            arena.extend(level_segments);
            for (index, range, node_unchanged, node_reused) in level_updates {
                ranges[index] = range;
                unchanged[index] = node_unchanged;
                reused[index] = node_reused;
            }
        }
        let reused = reused.into_boxed_slice();
        Ok((Self::from_parts(&arena, &ranges)?, reused))
    }

    fn translate_set(
        &self,
        old: LogicNodeId,
        max_cuts: usize,
        old_to_new: &[Option<LogicNodeId>],
    ) -> Option<CutSet> {
        let mut translated = CutSet::with_cap(max_cuts);
        for old_cut in self.cuts(old) {
            let mut cut = KCut::empty();
            for leaf in old_cut.leaves() {
                cut.insert_leaf(old_to_new.get(leaf.index()).copied().flatten()?.positive())?;
            }
            translated.insert(cut);
        }
        Some(translated)
    }

    fn translation_preserves_predecessor_rank(
        &self,
        old_predecessors: &[Option<u32>; 3],
        old_to_new: &[Option<LogicNodeId>],
    ) -> bool {
        let mut leaves = old_predecessors
            .iter()
            .flatten()
            .map(|old| LogicNodeId::from_index(*old as usize))
            .flat_map(|old_fanin| self.cuts(old_fanin))
            .flat_map(KCut::leaves)
            .map(|leaf| leaf.index())
            .collect::<Vec<_>>();
        leaves.sort_unstable();
        leaves.dedup();
        let mut previous = None;
        for leaf in leaves {
            let Some(mapped) = old_to_new.get(leaf).copied().flatten() else {
                return false;
            };
            let mapped = mapped.index();
            if previous.is_some_and(|previous| previous >= mapped) {
                return false;
            }
            previous = Some(mapped);
        }
        true
    }

    pub(crate) fn assert_same(&self, other: &Self) {
        assert_eq!(self.rows.row_count(), other.rows.row_count());
        for index in 0..self.rows.row_count() {
            let node = LogicNodeId::from_index(index);
            assert_eq!(
                self.cuts(node),
                other.cuts(node),
                "incremental cuts differ at logic node {index}"
            );
        }
    }

    fn from_parts(arena: &[Box<[KCut]>], ranges: &[CutRange]) -> Result<Self, crate::SynthError> {
        let rows = opto_core::PackedRows::try_from_row_iter(
            ranges.iter().map(|range| range.get(arena).iter().copied()),
        )
        .map_err(|_| crate::SynthError::capacity("logic cut database exceeds capacity"))?;
        Ok(Self { rows })
    }

    pub(crate) fn cuts(&self, node: LogicNodeId) -> &[KCut] {
        self.rows.row(node.index())
    }

    #[cfg(test)]
    pub(crate) fn cut_set(&self, node: LogicNodeId) -> CutSet {
        CutSet::from_slice(self.cuts(node))
    }
}

impl LogicGraph {
    #[cfg(test)]
    pub(super) fn cut_database(&self, max_leaves: usize, max_cuts: usize) -> CutDatabase {
        assert!(
            max_leaves <= MAX_CUT_LEAVES,
            "requested cut size exceeds compact cut capacity"
        );
        let node_count = self.node_count();
        let mut cut_arena = Vec::with_capacity(node_count.saturating_mul(2));
        let mut cut_ranges = Vec::<CutRange>::with_capacity(node_count);
        for index in 0..node_count {
            let node_id = LogicNodeId::from_index(index);
            let mut set = CutSet::with_cap(max_cuts);
            match self.node(node_id) {
                LogicNode::Const(_) => set.insert(KCut::empty()),
                LogicNode::Var(_) => set.insert(KCut::singleton(node_id)),
                LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                    set.insert(KCut::singleton(node_id));
                    merge_binary_cuts(
                        cut_ranges[left.index()].get_flat(&cut_arena),
                        cut_ranges[right.index()].get_flat(&cut_arena),
                        max_leaves,
                        &mut set,
                    );
                }
                LogicNode::Mux {
                    cond,
                    then_value,
                    else_value,
                } => {
                    set.insert(KCut::singleton(node_id));
                    merge_ternary_cuts(
                        cut_ranges[cond.index()].get_flat(&cut_arena),
                        cut_ranges[then_value.index()].get_flat(&cut_arena),
                        cut_ranges[else_value.index()].get_flat(&cut_arena),
                        max_leaves,
                        &mut set,
                    );
                }
            }
            debug_assert_eq!(cut_ranges.len(), index);
            cut_ranges.push(CutRange::append(&mut cut_arena, &set));
        }
        CutDatabase::from_parts(&[cut_arena.into_boxed_slice()], &cut_ranges)
            .expect("test logic cut database fits compact storage")
    }

    pub(super) fn parallel_cut_database(
        &self,
        max_leaves: usize,
        max_cuts: usize,
        runtime: &ExecutionContext,
    ) -> Result<CutDatabase, crate::SynthError> {
        assert!(
            max_leaves <= MAX_CUT_LEAVES,
            "requested cut size exceeds compact cut capacity"
        );
        let node_count = self.node_count();
        let mut levels = vec![Vec::new(); self.max_level() + 1];
        for index in 0..node_count {
            levels[self.level(LogicNodeId::from_index(index)) as usize].push(index);
        }

        let segment_count = cut_segment_count(&levels)?;
        let mut arena = Vec::<Box<[KCut]>>::with_capacity(segment_count);
        let mut ranges = vec![CutRange::UNINITIALIZED; node_count];
        for nodes in levels {
            let segment_base = arena.len();
            let mut level_segments = Vec::new();
            let mut level_ranges = Vec::with_capacity(nodes.len());
            runtime.analyze_indexed_chunks(
                nodes.len(),
                CUT_ANALYSIS_CHUNK_ITEMS,
                |position| {
                    let index = nodes[position];
                    Ok::<_, crate::SynthError>((
                        index,
                        self.cut_set_from_predecessors(
                            index, max_leaves, max_cuts, &ranges, &arena,
                        ),
                    ))
                },
                |_, results| {
                    let segment =
                        segment_base
                            .checked_add(level_segments.len())
                            .ok_or_else(|| {
                                crate::SynthError::capacity("logic cut segment index overflow")
                            })?;
                    let packed_len = results.iter().try_fold(0usize, |count, (_, set)| {
                        count.checked_add(set.len()).ok_or_else(|| {
                            crate::SynthError::capacity("logic cut arena size overflow")
                        })
                    })?;
                    let mut packed = Vec::with_capacity(packed_len);
                    for (index, set) in results {
                        let range = CutRange::append_at(&mut packed, segment, &set);
                        level_ranges.push((index, range));
                    }
                    level_segments.push(packed.into_boxed_slice());
                    Ok::<_, crate::SynthError>(())
                },
            )?;
            arena.extend(level_segments);
            for (index, range) in level_ranges {
                ranges[index] = range;
            }
        }
        CutDatabase::from_parts(&arena, &ranges)
    }

    fn cut_set_from_predecessors(
        &self,
        index: usize,
        max_leaves: usize,
        max_cuts: usize,
        ranges: &[CutRange],
        arena: &[Box<[KCut]>],
    ) -> CutSet {
        let node = LogicNodeId::from_index(index);
        let predecessor = |fanin: LogicNodeId| ranges[fanin.index()].get(arena);
        let mut set = CutSet::with_cap(max_cuts);
        match self.node(node) {
            LogicNode::Const(_) => set.insert(KCut::empty()),
            LogicNode::Var(_) => set.insert(KCut::singleton(node)),
            LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                set.insert(KCut::singleton(node));
                merge_binary_cuts(predecessor(left), predecessor(right), max_leaves, &mut set);
            }
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => {
                set.insert(KCut::singleton(node));
                merge_ternary_cuts(
                    predecessor(cond),
                    predecessor(then_value),
                    predecessor(else_value),
                    max_leaves,
                    &mut set,
                );
            }
        }
        set
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CutRange {
    segment: u32,
    start_and_len: u32,
}

impl CutRange {
    const LEN_BITS: u32 = MAX_CUTS_PER_NODE.trailing_zeros();
    #[allow(
        clippy::cast_possible_truncation,
        reason = "MAX_CUTS_PER_NODE is the synthesis-time value 32"
    )]
    const LEN_MASK: u32 = MAX_CUTS_PER_NODE as u32 - 1;
    const UNINITIALIZED: Self = Self {
        segment: u32::MAX,
        start_and_len: u32::MAX,
    };

    #[cfg(test)]
    fn append(arena: &mut Vec<KCut>, set: &CutSet) -> Self {
        Self::append_at(arena, 0, set)
    }

    fn append_at(arena: &mut Vec<KCut>, segment: usize, set: &CutSet) -> Self {
        let segment = segment
            .try_into()
            .expect("logic cut segment index exceeds 32-bit capacity");
        let start: u32 = arena
            .len()
            .try_into()
            .expect("logic cut arena exceeds 32-bit offset capacity");
        let len: u32 = set
            .len()
            .try_into()
            .expect("cut set length must fit in compact range");
        debug_assert!((1..=Self::LEN_MASK + 1).contains(&len));
        let start_and_len = start
            .checked_mul(Self::LEN_MASK + 1)
            .and_then(|packed| packed.checked_add(len - 1))
            .expect("logic cut segment exceeds compact range capacity");
        arena.extend_from_slice(set.as_slice());
        Self {
            segment,
            start_and_len,
        }
    }

    fn get(self, arena: &[Box<[KCut]>]) -> &[KCut] {
        let start = (self.start_and_len >> Self::LEN_BITS) as usize;
        let len = (self.start_and_len & Self::LEN_MASK) as usize + 1;
        &arena[self.segment as usize][start..start + len]
    }

    #[cfg(test)]
    fn get_flat(self, arena: &[KCut]) -> &[KCut] {
        debug_assert_eq!(self.segment, 0);
        let start = (self.start_and_len >> Self::LEN_BITS) as usize;
        let len = (self.start_and_len & Self::LEN_MASK) as usize + 1;
        &arena[start..start + len]
    }
}

fn merge_binary_cuts(left: &[KCut], right: &[KCut], max_leaves: usize, out: &mut CutSet) {
    for &left_cut in left {
        for &right_cut in right {
            if let Some(merged) = left_cut.merge(right_cut, max_leaves) {
                out.insert(merged);
            }
        }
    }
}

fn merge_ternary_cuts(
    first: &[KCut],
    second: &[KCut],
    third: &[KCut],
    max_leaves: usize,
    out: &mut CutSet,
) {
    let mut intermediate = CutSet::default();
    merge_binary_cuts(first, second, max_leaves, &mut intermediate);
    merge_binary_cuts(intermediate.as_slice(), third, max_leaves, out);
}
