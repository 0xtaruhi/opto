// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::cuts::KCut;
#[cfg(test)]
use super::cuts::{CutDatabase, CutRange, CutSet};
use crate::boolean::logic::{MAX_MATCH_INPUTS, TruthTable};
use opto_ir::logic::{Lit, LogicBuilder, LogicNetwork as StoredLogicNetwork, NodeId, NodeKind};
#[cfg(test)]
use opto_runtime::ExecutionContext;
use std::sync::OnceLock;

thread_local! {
    static TRUTH_SCRATCH: std::cell::RefCell<TruthScratch> =
        std::cell::RefCell::new(TruthScratch::default());
}

#[derive(Default)]
struct TruthScratch {
    bits: Vec<u64>,
    epochs: Vec<u32>,
    epoch: u32,
    pending: Vec<(LogicNodeId, bool)>,
    touched: Vec<LogicNodeId>,
}

impl TruthScratch {
    fn begin(&mut self, node_count: usize) {
        self.bits.resize(node_count, 0);
        self.epochs.resize(node_count, 0);
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.epochs.fill(0);
            self.epoch = 1;
        }
        self.pending.clear();
        self.touched.clear();
    }

    fn contains(&self, node: LogicNodeId) -> bool {
        self.epochs[node.index()] == self.epoch
    }

    fn get(&self, node: LogicNodeId, valid_bits: u64) -> u64 {
        debug_assert!(self.contains(node.positive()));
        node.apply_to_bits(self.bits[node.index()], valid_bits)
    }

    fn set(&mut self, node: LogicNodeId, bits: u64) {
        let node = node.positive();
        if !self.contains(node) {
            self.touched.push(node);
        }
        self.bits[node.index()] = bits;
        self.epochs[node.index()] = self.epoch;
    }

    fn evaluate(&mut self, network: &LogicGraph, roots: &[LogicNodeId], valid_bits: u64) {
        for &root in roots {
            self.pending.push((root.positive(), false));
            while let Some((node, expanded)) = self.pending.pop() {
                let node = node.positive();
                if self.contains(node) {
                    continue;
                }
                let logic = network.node(node);
                if !expanded {
                    self.pending.push((node, true));
                    for fanin in LogicGraph::node_fanins(logic).iter() {
                        if !self.contains(fanin.positive()) {
                            self.pending.push((fanin.positive(), false));
                        }
                    }
                    continue;
                }
                let bits = match logic {
                    LogicNode::Const(false) | LogicNode::Var(_) => 0,
                    LogicNode::Const(true) => valid_bits,
                    LogicNode::And(left, right) => {
                        self.get(left, valid_bits) & self.get(right, valid_bits)
                    }
                    LogicNode::Xor(left, right) => {
                        self.get(left, valid_bits) ^ self.get(right, valid_bits)
                    }
                    LogicNode::Mux {
                        cond,
                        then_value,
                        else_value,
                    } => {
                        let select = self.get(cond, valid_bits);
                        (select & self.get(then_value, valid_bits))
                            | (!select & self.get(else_value, valid_bits) & valid_bits)
                    }
                };
                self.set(node, bits);
            }
        }
    }
}

pub(super) const MAX_CUT_LEAVES: usize = MAX_MATCH_INPUTS;
pub(super) const MAX_CUTS_PER_NODE: usize = 32;
#[cfg(test)]
const WORD_BITS: usize = u64::BITS as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LogicNodeId(Lit);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LogicNode {
    Const(bool),
    Var(u32),
    And(LogicNodeId, LogicNodeId),
    Xor(LogicNodeId, LogicNodeId),
    Mux {
        cond: LogicNodeId,
        then_value: LogicNodeId,
        else_value: LogicNodeId,
    },
}

#[derive(Debug)]
pub(crate) struct LogicGraph {
    builder: Option<LogicBuilder>,
    storage: OnceLock<StoredLogicNetwork>,
}

impl LogicGraph {
    /// Counts how many times each node is referenced, treating every root as one
    /// external reference.
    ///
    /// Rewriting, balancing, and recipe replay all size their MFFCs from this
    /// same count.
    pub(crate) fn reference_counts(&self, roots: &[LogicNodeId]) -> Vec<u32> {
        let node_count = self.node_count();
        let mut references = vec![0u32; node_count];
        for index in 0..node_count {
            for fanin in self.node(LogicNodeId::from_index(index)).fanins() {
                references[fanin.index()] += 1;
            }
        }
        for root in roots {
            references[root.index()] += 1;
        }
        references
    }

    pub(crate) fn new() -> Self {
        Self {
            builder: Some(LogicBuilder::new()),
            storage: OnceLock::new(),
        }
    }

    pub(crate) fn constant(value: bool) -> LogicNodeId {
        LogicNodeId(LogicBuilder::constant(value))
    }

    pub(crate) fn variable(&mut self, index: usize) -> Option<LogicNodeId> {
        let origin = u32::try_from(index).ok()?;
        self.builder.as_mut()?.input(origin).ok().map(LogicNodeId)
    }

    pub(crate) fn not(arg: LogicNodeId) -> LogicNodeId {
        LogicNodeId(LogicBuilder::not(arg.0))
    }

    pub(crate) fn and(&mut self, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
        LogicNodeId(
            self.mutable_builder()
                .and(left.0, right.0, 0)
                .expect("logic network exceeds compact capacity"),
        )
    }

    pub(crate) fn or(&mut self, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
        LogicNodeId(
            self.mutable_builder()
                .or(left.0, right.0, 0)
                .expect("logic network exceeds compact capacity"),
        )
    }

    pub(crate) fn xor(&mut self, left: LogicNodeId, right: LogicNodeId) -> LogicNodeId {
        LogicNodeId(
            self.mutable_builder()
                .xor(left.0, right.0, 0)
                .expect("logic network exceeds compact capacity"),
        )
    }

    pub(crate) fn mux(
        &mut self,
        cond: LogicNodeId,
        then_value: LogicNodeId,
        else_value: LogicNodeId,
    ) -> LogicNodeId {
        LogicNodeId(
            self.mutable_builder()
                .mux(cond.0, then_value.0, else_value.0, 0)
                .expect("logic network exceeds compact capacity"),
        )
    }

    pub(crate) fn freeze(&mut self) {
        if let Some(builder) = self.builder.take()
            && self.storage.get().is_none()
        {
            self.storage
                .set(builder.freeze())
                .expect("logic network storage is initialized exactly once");
        }
    }

    #[cfg(test)]
    pub(crate) fn storage_network(&self) -> &StoredLogicNetwork {
        self.storage()
    }

    fn mutable_builder(&mut self) -> &mut LogicBuilder {
        assert!(
            self.storage.get().is_none(),
            "logic network cannot be extended after analysis starts"
        );
        self.builder
            .as_mut()
            .expect("logic network is immutable after analysis starts")
    }

    fn storage(&self) -> &StoredLogicNetwork {
        self.storage.get_or_init(|| {
            self.builder
                .as_ref()
                .expect("logic network builder exists before explicit freeze")
                .clone()
                .freeze()
        })
    }

    pub(crate) fn node(&self, id: LogicNodeId) -> LogicNode {
        let node = id.node();
        let storage = self.storage();
        match storage
            .kind(node)
            .expect("logic node ID belongs to the frozen network")
        {
            NodeKind::Constant => LogicNode::Const(false),
            NodeKind::Input => LogicNode::Var(storage.origin(node).unwrap_or(0)),
            NodeKind::And => LogicNode::And(
                LogicNodeId(storage.fanin(node, 0).unwrap()),
                LogicNodeId(storage.fanin(node, 1).unwrap()),
            ),
            NodeKind::Xor => LogicNode::Xor(
                LogicNodeId(storage.fanin(node, 0).unwrap()),
                LogicNodeId(storage.fanin(node, 1).unwrap()),
            ),
            NodeKind::Mux => LogicNode::Mux {
                cond: LogicNodeId(storage.fanin(node, 0).unwrap()),
                then_value: LogicNodeId(storage.fanin(node, 1).unwrap()),
                else_value: LogicNodeId(storage.fanin(node, 2).unwrap()),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn truth_table(&self, root: LogicNodeId, input_count: usize) -> TruthTable {
        self.truth_table_with_inputs(root, input_count)
    }

    pub(crate) fn truth_table_for_cut(&self, root: LogicNodeId, cut: KCut) -> TruthTable {
        let inputs = cut.leaves();
        let input_count = inputs.len();
        assert!(input_count <= MAX_CUT_LEAVES);

        let assignment_count = 1usize << input_count;
        let valid_bits = low_bits_mask(assignment_count);
        TRUTH_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.begin(self.node_count());
            for (index, &input) in inputs.iter().enumerate() {
                let bits = variable_bits(index, assignment_count);
                scratch.set(input, input.apply_to_bits(bits, valid_bits));
            }
            scratch.evaluate(self, &[root.positive()], valid_bits);
            TruthTable {
                input_count,
                bits: scratch.get(root, valid_bits),
            }
        })
    }

    pub(crate) fn truth_tables_for_inputs(
        &self,
        root: LogicNodeId,
        inputs: &[LogicNodeId],
        observed: &[LogicNodeId],
    ) -> NetworkTruthTables {
        let input_count = inputs.len();
        assert!(input_count <= MAX_CUT_LEAVES);

        let assignment_count = 1usize << input_count;
        let valid_bits = low_bits_mask(assignment_count);
        TRUTH_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.begin(self.node_count());
            for (index, &input) in inputs.iter().enumerate() {
                let bits = variable_bits(index, assignment_count);
                scratch.set(input, input.apply_to_bits(bits, valid_bits));
            }
            let mut requested = Vec::with_capacity(inputs.len() + observed.len() + 1);
            requested.extend(inputs.iter().map(|input| input.positive()));
            requested.push(root.positive());
            requested.extend(observed.iter().map(|node| node.positive()));
            requested.sort_unstable();
            requested.dedup();
            scratch.evaluate(self, &requested, valid_bits);
            scratch.touched.sort_unstable();
            let node_bits = scratch
                .touched
                .iter()
                .copied()
                .map(|node| (node, scratch.bits[node.index()]))
                .collect();
            NetworkTruthTables {
                input_count,
                valid_bits,
                node_bits,
            }
        })
    }

    #[cfg(test)]
    fn truth_table_with_inputs(&self, root: LogicNodeId, input_count: usize) -> TruthTable {
        assert!(input_count <= MAX_CUT_LEAVES);
        let node_count = root.index() + 1;
        let mut bits = 0u64;
        let mut values = NodeValueBits::new(node_count);
        for assignment in 0..(1usize << input_count) {
            for index in 0..node_count {
                let node_id = LogicNodeId::from_index(index);
                let value = match self.node(node_id) {
                    LogicNode::Const(value) => value,
                    LogicNode::Var(input) => {
                        let input = input as usize;
                        assert!(input < input_count);
                        ((assignment >> input) & 1) == 1
                    }
                    LogicNode::And(left, right) => values.get(left) & values.get(right),
                    LogicNode::Xor(left, right) => values.get(left) ^ values.get(right),
                    LogicNode::Mux {
                        cond,
                        then_value,
                        else_value,
                    } => {
                        if values.get(cond) {
                            values.get(then_value)
                        } else {
                            values.get(else_value)
                        }
                    }
                };
                values.set(node_id, value);
            }
            if values.get(root) {
                bits |= 1u64 << assignment;
            }
        }
        TruthTable { input_count, bits }
    }

    #[cfg(test)]
    pub(crate) fn cuts(&self, root: LogicNodeId, max_leaves: usize) -> CutSet {
        self.cut_database(max_leaves, MAX_CUTS_PER_NODE)
            .cut_set(root)
    }

    #[cfg(test)]
    pub(crate) fn is_valid_cut(&self, root: LogicNodeId, cut: KCut) -> bool {
        if cut.contains(root) {
            return true;
        }
        let mut covered = Vec::with_capacity(root.index() + 1);
        for index in 0..=root.index() {
            let node_id = LogicNodeId::from_index(index);
            let value = if cut.contains(node_id) {
                true
            } else {
                match self.node(node_id) {
                    LogicNode::Const(_) => true,
                    LogicNode::Var(_) => false,
                    LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                        covered[left.index()] && covered[right.index()]
                    }
                    LogicNode::Mux {
                        cond,
                        then_value,
                        else_value,
                    } => {
                        covered[cond.index()]
                            && covered[then_value.index()]
                            && covered[else_value.index()]
                    }
                }
            };
            covered.push(value);
        }
        covered[root.index()]
    }

    pub(crate) fn node_count(&self) -> usize {
        self.builder
            .as_ref()
            .map_or_else(|| self.storage().node_count(), LogicBuilder::node_count)
    }

    pub(crate) fn level(&self, node: LogicNodeId) -> u32 {
        self.storage().level(node.node()).unwrap_or(0)
    }

    pub(crate) fn max_level(&self) -> usize {
        (0..self.node_count())
            .map(|index| self.level(LogicNodeId::from_index(index)) as usize)
            .max()
            .unwrap_or(0)
    }

    fn node_fanins(node: LogicNode) -> Fanins {
        match node {
            LogicNode::Const(_) | LogicNode::Var(_) => Fanins::default(),
            LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                Fanins::from_slice(&[left, right])
            }
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => Fanins::from_slice(&[cond, then_value, else_value]),
        }
    }
}

impl LogicNode {
    pub(crate) fn fanins(self) -> impl Iterator<Item = LogicNodeId> {
        let fanins = match self {
            LogicNode::Const(_) | LogicNode::Var(_) => [None, None, None],
            LogicNode::And(left, right) | LogicNode::Xor(left, right) => {
                [Some(left), Some(right), None]
            }
            LogicNode::Mux {
                cond,
                then_value,
                else_value,
            } => [Some(cond), Some(then_value), Some(else_value)],
        };
        fanins.into_iter().flatten()
    }

    pub(crate) fn is_gate(self) -> bool {
        matches!(
            self,
            LogicNode::And(..) | LogicNode::Xor(..) | LogicNode::Mux { .. }
        )
    }

    pub(crate) fn is_cover_node(self) -> bool {
        self.is_gate()
    }
}

impl LogicNodeId {
    pub(super) const CONSTANT: Self = Self(Lit::FALSE);

    pub(crate) fn from_index(index: usize) -> Self {
        let node =
            NodeId::from_index(index).expect("logic node index exceeds 32-bit node ID capacity");
        Self(Lit::from_node(node).expect("logic literal exceeds 32-bit capacity"))
    }

    pub(crate) fn index(self) -> usize {
        self.0.node().index()
    }

    #[cfg(test)]
    pub(crate) const fn lit(self) -> Lit {
        self.0
    }

    fn node(self) -> NodeId {
        self.0.node()
    }

    pub(crate) fn positive(self) -> Self {
        Self(self.0.positive())
    }

    pub(crate) fn inverted(self) -> Self {
        Self(self.0.inverted())
    }

    pub(crate) fn is_inverted(self) -> bool {
        self.0.is_inverted()
    }

    fn apply_to_bits(self, bits: u64, valid_bits: u64) -> u64 {
        if self.is_inverted() {
            !bits & valid_bits
        } else {
            bits
        }
    }
}

#[derive(Debug)]
pub(crate) struct NetworkTruthTables {
    input_count: usize,
    valid_bits: u64,
    node_bits: Box<[(LogicNodeId, u64)]>,
}

impl NetworkTruthTables {
    #[cfg(test)]
    pub(crate) fn function_of(
        &self,
        root: LogicNodeId,
        inputs: &[LogicNodeId],
    ) -> Option<TruthTable> {
        let (truth, care) = self.care_projection(root, inputs)?;
        let assignment_count = 1usize << inputs.len();
        if care != low_bits_mask(assignment_count) {
            return None;
        }
        Some(truth)
    }

    pub(crate) fn care_projection(
        &self,
        root: LogicNodeId,
        inputs: &[LogicNodeId],
    ) -> Option<(TruthTable, u64)> {
        if inputs.len() > MAX_CUT_LEAVES {
            return None;
        }

        let root_bits = self.bits(root)?;
        let mut input_bits = [0u64; MAX_CUT_LEAVES];
        for (input_index, input) in inputs.iter().enumerate() {
            input_bits[input_index] = self.bits(*input)?;
        }

        let mut seen = 0u64;
        let mut result = 0u64;
        for base_assignment in 0..(1usize << self.input_count) {
            let mut input_assignment = 0usize;
            for (input_index, bits) in input_bits.iter().enumerate().take(inputs.len()) {
                if bit_at(*bits, base_assignment) {
                    input_assignment |= 1usize << input_index;
                }
            }

            let assignment_bit = 1u64 << input_assignment;
            let output = bit_at(root_bits, base_assignment);
            if seen & assignment_bit != 0 && bit_at(result, input_assignment) != output {
                return None;
            }
            seen |= assignment_bit;
            if output {
                result |= assignment_bit;
            }
        }

        Some((
            TruthTable {
                input_count: inputs.len(),
                bits: result,
            },
            seen,
        ))
    }

    fn bits(&self, node: LogicNodeId) -> Option<u64> {
        self.node_bits
            .binary_search_by_key(&node.positive(), |&(node, _)| node)
            .ok()
            .map(|index| self.node_bits[index].1)
            .map(|bits| node.apply_to_bits(bits, self.valid_bits))
    }
}

#[cfg(test)]
#[derive(Debug)]
struct NodeValueBits {
    words: Vec<u64>,
}

#[cfg(test)]
impl NodeValueBits {
    fn new(bit_count: usize) -> Self {
        Self {
            words: vec![0; bit_count.div_ceil(WORD_BITS)],
        }
    }

    fn get(&self, node: LogicNodeId) -> bool {
        let index = node.index();
        let value = (self.words[index / WORD_BITS] & bit_mask(index)) != 0;
        value ^ node.is_inverted()
    }

    fn set(&mut self, node: LogicNodeId, value: bool) {
        let index = node.index();
        let word = &mut self.words[index / WORD_BITS];
        let mask = bit_mask(index);
        if value {
            *word |= mask;
        } else {
            *word &= !mask;
        }
    }
}

#[cfg(test)]
fn bit_mask(index: usize) -> u64 {
    1u64 << (index % WORD_BITS)
}

fn bit_at(bits: u64, index: usize) -> bool {
    bits & (1u64 << index) != 0
}

fn low_bits_mask(bit_count: usize) -> u64 {
    if bit_count == u64::BITS as usize {
        u64::MAX
    } else {
        (1u64 << bit_count) - 1
    }
}

fn variable_bits(input: usize, assignment_count: usize) -> u64 {
    let mut bits = 0u64;
    for assignment in 0..assignment_count {
        if assignment & (1usize << input) != 0 {
            bits |= 1u64 << assignment;
        }
    }
    bits
}

#[derive(Debug)]
struct Fanins {
    len: usize,
    values: [LogicNodeId; 3],
}

impl Default for Fanins {
    fn default() -> Self {
        Self {
            len: 0,
            values: [LogicNodeId::CONSTANT; 3],
        }
    }
}

impl Fanins {
    fn from_slice(values: &[LogicNodeId]) -> Self {
        let mut fanins = Self::default();
        fanins.values[..values.len()].copy_from_slice(values);
        fanins.len = values.len();
        fanins
    }

    fn iter(&self) -> impl Iterator<Item = LogicNodeId> + '_ {
        self.values[..self.len].iter().copied()
    }
}

#[cfg(test)]
mod tests;
