// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Conservative bit facts for word-level values.

use super::{
    BinaryOp, CastKind, OpKind, PortDirection, SignalId, UnaryOp, ValueId, ValueKind, WordModule,
};
use crate::{BitVal, ConstBits};
use opto_core::PackedRows;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Proven Boolean state of one word bit.
pub enum KnownBit {
    /// Proven zero.
    Zero,
    /// Proven one.
    One,
    /// Not statically known.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Fixed-size packed known-bit fact suitable for content-addressed caches.
///
/// Values wider than 128 bits deliberately do not have this representation.
pub struct KnownBits128 {
    width: u8,
    zeros: u128,
    ones: u128,
}

impl KnownBits128 {
    /// Builds a packed fact from least-significant-bit-first states.
    ///
    /// # Errors
    ///
    /// Returns [`super::WordError`] when the width is zero or exceeds 128 bits.
    pub fn from_bits(bits: impl IntoIterator<Item = KnownBit>) -> Result<Self, super::WordError> {
        let mut width = 0u8;
        let mut zeros = 0u128;
        let mut ones = 0u128;
        for bit in bits {
            if u32::from(width) == u128::BITS {
                return Err(super::WordError::new(
                    "packed known-bit width must be between 1 and 128 bits",
                ));
            }
            let mask = 1u128 << width;
            match bit {
                KnownBit::Zero => zeros |= mask,
                KnownBit::One => ones |= mask,
                KnownBit::Unknown => {}
            }
            width += 1;
        }
        if width == 0 {
            return Err(super::WordError::new(
                "packed known-bit width must be between 1 and 128 bits",
            ));
        }
        Ok(Self { width, zeros, ones })
    }

    #[must_use]
    /// Returns the represented value width.
    pub fn width(self) -> u32 {
        u32::from(self.width)
    }

    #[must_use]
    /// Returns the proven state of one bit.
    pub fn bit(self, index: u32) -> KnownBit {
        if index >= self.width() || index >= u128::BITS {
            return KnownBit::Unknown;
        }
        let mask = 1u128 << index;
        if self.zeros & mask != 0 {
            KnownBit::Zero
        } else if self.ones & mask != 0 {
            KnownBit::One
        } else {
            KnownBit::Unknown
        }
    }

    #[must_use]
    /// Returns a constant when every bit is known.
    pub fn constant(self) -> Option<ConstBits> {
        known_constant(self.width(), |index| self.bit(index))
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct FactWord {
    zeros: u64,
    ones: u64,
}

#[derive(Debug, Clone, Copy)]
struct FactRange {
    start: Option<usize>,
    width: u32,
}

impl FactRange {
    const fn unknown(width: u32) -> Self {
        Self { start: None, width }
    }

    fn word(self, arena: &[FactWord], index: usize) -> FactWord {
        self.start
            .and_then(|start| arena.get(start + index))
            .copied()
            .unwrap_or_default()
    }

    fn bit(self, arena: &[FactWord], index: u32) -> KnownBit {
        if index >= self.width {
            return KnownBit::Unknown;
        }
        let word = self.word(arena, index as usize / u64::BITS as usize);
        let mask = 1u64 << (index % u64::BITS);
        if word.zeros & mask != 0 {
            KnownBit::Zero
        } else if word.ones & mask != 0 {
            KnownBit::One
        } else {
            KnownBit::Unknown
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
enum FactState {
    #[default]
    Uncomputed,
    Computing,
    Computed(FactRange),
}

#[derive(Debug)]
struct SignalDrivers {
    connects: PackedRows<usize>,
    observed_connects: usize,
    appended: BTreeMap<SignalId, Vec<usize>>,
}

impl SignalDrivers {
    fn build(module: &WordModule) -> Self {
        Self {
            connects: PackedRows::try_from_entries(
                module.signals().len(),
                module
                    .connects()
                    .iter()
                    .enumerate()
                    .map(|(index, connect)| (connect.target.signal.index(), index)),
            )
            .expect("validated Word IR fits the packed-row capacity"),
            observed_connects: module.connects().len(),
            appended: BTreeMap::new(),
        }
    }

    fn sync_append_only(&mut self, module: &WordModule) -> Option<BTreeSet<SignalId>> {
        if module.connects().len() < self.observed_connects {
            *self = Self::build(module);
            return None;
        }
        let mut changed = BTreeSet::new();
        for (index, connect) in module
            .connects()
            .iter()
            .enumerate()
            .skip(self.observed_connects)
        {
            changed.insert(connect.target.signal);
            self.appended
                .entry(connect.target.signal)
                .or_default()
                .push(index);
        }
        self.observed_connects = module.connects().len();
        Some(changed)
    }

    fn for_signal(&self, signal: SignalId) -> impl Iterator<Item = usize> + '_ {
        self.connects
            .get(signal.index())
            .unwrap_or_default()
            .iter()
            .copied()
            .chain(self.appended.get(&signal).into_iter().flatten().copied())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FactNode {
    Value(ValueId),
    Signal(SignalId),
}

/// Memoized whole-module known-bit analysis.
///
/// Facts are retained in one packed word arena. Signal reads follow static,
/// single-driver connections; external, dynamic, multiply-driven, cyclic, and
/// state-holding values remain conservative.
#[derive(Debug)]
pub struct KnownBitsAnalysis {
    values: Vec<FactState>,
    signals: Vec<FactState>,
    arena: Vec<FactWord>,
    /// Reverse dependency edges, indexed by the dependency's dense index.
    ///
    /// `FactNode` wraps a dense `ValueId`/`SignalId`, so a map keyed by it pays
    /// a B-tree search for what is already an array index. Invalidation walks
    /// these edges for every appended connection, which made that search the
    /// single hottest operation in RTL normalization.
    value_dependents: Vec<Vec<FactNode>>,
    signal_dependents: Vec<Vec<FactNode>>,
    /// Epoch-stamped visit marks reused across `invalidate` calls, so a walk
    /// costs no allocation and no ordered-set insert.
    value_visited: Vec<u32>,
    signal_visited: Vec<u32>,
    visit_epoch: u32,
    drivers: SignalDrivers,
    external_signals: Vec<bool>,
    observed_ports: usize,
}

impl KnownBitsAnalysis {
    #[must_use]
    /// Creates an empty memoization state for `module`.
    pub fn new(module: &WordModule) -> Self {
        let mut external_signals = vec![false; module.signals().len()];
        for port in module.ports() {
            if matches!(port.direction, PortDirection::Input | PortDirection::Inout) {
                external_signals[port.signal.index()] = true;
            }
        }
        Self {
            values: vec![FactState::Uncomputed; module.values().len()],
            signals: vec![FactState::Uncomputed; module.signals().len()],
            arena: Vec::new(),
            value_dependents: vec![Vec::new(); module.values().len()],
            signal_dependents: vec![Vec::new(); module.signals().len()],
            value_visited: vec![0; module.values().len()],
            signal_visited: vec![0; module.signals().len()],
            visit_epoch: 0,
            drivers: SignalDrivers::build(module),
            external_signals,
            observed_ports: module.ports().len(),
        }
    }

    /// Synchronizes this memoization state after append-only Word IR changes.
    ///
    /// Newly appended values and signals retain facts for the immutable prefix.
    /// Appended connections or ports invalidate only the transitive dependents
    /// of changed signals and update driver metadata incrementally.
    pub fn sync_append_only(&mut self, module: &WordModule) {
        if module.values().len() < self.values.len()
            || module.signals().len() < self.signals.len()
            || module.ports().len() < self.observed_ports
        {
            *self = Self::new(module);
            return;
        }
        self.values
            .resize(module.values().len(), FactState::Uncomputed);
        self.signals
            .resize(module.signals().len(), FactState::Uncomputed);
        self.value_dependents
            .resize(module.values().len(), Vec::new());
        self.signal_dependents
            .resize(module.signals().len(), Vec::new());
        self.value_visited.resize(module.values().len(), 0);
        self.signal_visited.resize(module.signals().len(), 0);
        self.external_signals.resize(module.signals().len(), false);

        let Some(mut changed_signals) = self.drivers.sync_append_only(module) else {
            *self = Self::new(module);
            return;
        };
        for port in module.ports().iter().skip(self.observed_ports) {
            if matches!(port.direction, PortDirection::Input | PortDirection::Inout) {
                self.external_signals[port.signal.index()] = true;
                changed_signals.insert(port.signal);
            }
        }
        self.observed_ports = module.ports().len();
        self.invalidate(changed_signals.into_iter().map(FactNode::Signal));
    }

    /// Extends dense value storage without admitting newly published drivers.
    ///
    /// A normalization epoch may append private expression values while its
    /// completed signal assignments remain outside the epoch's analysis
    /// snapshot. In that case new values may consume facts from the frozen
    /// prefix, but appended connects must not trigger repeated whole-cone
    /// invalidation or become visible to later tasks in the same epoch.
    pub fn extend_frozen_connectivity(&mut self, module: &WordModule) {
        if module.values().len() < self.values.len() || module.signals().len() < self.signals.len()
        {
            *self = Self::new(module);
            return;
        }
        self.values
            .resize(module.values().len(), FactState::Uncomputed);
        self.signals
            .resize(module.signals().len(), FactState::Uncomputed);
        self.value_dependents
            .resize(module.values().len(), Vec::new());
        self.signal_dependents
            .resize(module.signals().len(), Vec::new());
        self.value_visited.resize(module.values().len(), 0);
        self.signal_visited.resize(module.signals().len(), 0);
        self.external_signals.resize(module.signals().len(), false);
    }

    fn dependents_mut(&mut self, node: FactNode) -> Option<&mut Vec<FactNode>> {
        match node {
            FactNode::Value(value) => self.value_dependents.get_mut(value.index()),
            FactNode::Signal(signal) => self.signal_dependents.get_mut(signal.index()),
        }
    }

    fn dependents_of(&self, node: FactNode) -> &[FactNode] {
        let dependents = match node {
            FactNode::Value(value) => self.value_dependents.get(value.index()),
            FactNode::Signal(signal) => self.signal_dependents.get(signal.index()),
        };
        dependents.map_or(&[], Vec::as_slice)
    }

    fn record_dependency(&mut self, dependency: FactNode, dependent: FactNode) {
        if let Some(dependents) = self.dependents_mut(dependency)
            && !dependents.contains(&dependent)
        {
            dependents.push(dependent);
        }
    }

    /// Returns whether `node` had not yet been visited in this walk.
    fn mark_visited(&mut self, node: FactNode) -> bool {
        let epoch = self.visit_epoch;
        let mark = match node {
            FactNode::Value(value) => self.value_visited.get_mut(value.index()),
            FactNode::Signal(signal) => self.signal_visited.get_mut(signal.index()),
        };
        match mark {
            Some(mark) if *mark == epoch => false,
            Some(mark) => {
                *mark = epoch;
                true
            }
            None => false,
        }
    }

    fn invalidate(&mut self, roots: impl IntoIterator<Item = FactNode>) {
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        // A fresh epoch retires every previous mark without clearing the
        // arrays. On wraparound the marks are reset once instead.
        self.visit_epoch = if let Some(epoch) = self.visit_epoch.checked_add(1) {
            epoch
        } else {
            self.value_visited.fill(0);
            self.signal_visited.fill(0);
            1
        };
        while let Some(node) = pending.pop() {
            if !self.mark_visited(node) {
                continue;
            }
            match node {
                FactNode::Value(value) => {
                    if let Some(state) = self.values.get_mut(value.index()) {
                        *state = FactState::Uncomputed;
                    }
                }
                FactNode::Signal(signal) => {
                    if let Some(state) = self.signals.get_mut(signal.index()) {
                        *state = FactState::Uncomputed;
                    }
                }
            }
            pending.extend_from_slice(self.dependents_of(node));
        }
    }

    /// Returns the proven state of `value[index]`.
    pub fn bit(&mut self, module: &WordModule, value: ValueId, index: u32) -> KnownBit {
        derive_value(module, value, self)
            .map_or(KnownBit::Unknown, |facts| facts.bit(&self.arena, index))
    }

    /// Returns a two-state constant when every bit of `value` is proven.
    pub fn constant(&mut self, module: &WordModule, value: ValueId) -> Option<ConstBits> {
        let facts = derive_value(module, value, self)?;
        known_constant(facts.width, |index| facts.bit(&self.arena, index))
    }

    /// Returns the smallest low-bit prefix containing every bit below `limit`
    /// that is not proven zero.
    pub fn active_width(&mut self, module: &WordModule, value: ValueId, limit: u32) -> u32 {
        let Some(facts) = derive_value(module, value, self) else {
            return limit;
        };
        let mut width = limit.min(facts.width);
        while width > 0 && facts.bit(&self.arena, width - 1) == KnownBit::Zero {
            width -= 1;
        }
        width
    }

    /// Derives a compact complete fact for values up to 128 bits.
    pub fn packed128(&mut self, module: &WordModule, value: ValueId) -> Option<KnownBits128> {
        let facts = derive_value(module, value, self)?;
        if facts.width > u128::BITS {
            return None;
        }
        let width = u8::try_from(facts.width).ok()?;
        let low = facts.word(&self.arena, 0);
        let high = facts.word(&self.arena, 1);
        let zeros = u128::from(low.zeros) | (u128::from(high.zeros) << u64::BITS);
        let ones = u128::from(low.ones) | (u128::from(high.ones) << u64::BITS);
        Some(KnownBits128 { width, zeros, ones })
    }
}

fn known_constant(width: u32, mut bit: impl FnMut(u32) -> KnownBit) -> Option<ConstBits> {
    let bits = (0..width)
        .rev()
        .map(|index| match bit(index) {
            KnownBit::Zero => Some(BitVal::Zero),
            KnownBit::One => Some(BitVal::One),
            KnownBit::Unknown => None,
        })
        .collect::<Option<Vec<_>>>()?;
    ConstBits::from_bits(bits).ok()
}

fn derive_value(
    module: &WordModule,
    id: ValueId,
    analysis: &mut KnownBitsAnalysis,
) -> Option<FactRange> {
    derive_node(module, FactNode::Value(id), analysis)
}

fn derive_node(
    module: &WordModule,
    root: FactNode,
    analysis: &mut KnownBitsAnalysis,
) -> Option<FactRange> {
    node_width(module, root)?;
    let mut pending = vec![(root, false)];
    while let Some((node, exiting)) = pending.pop() {
        if exiting {
            let facts = evaluate_node(module, node, analysis)
                .unwrap_or_else(|| FactRange::unknown(node_width(module, node).unwrap_or(0)));
            set_node_state(analysis, node, FactState::Computed(facts))?;
            continue;
        }
        match node_state(analysis, node)? {
            FactState::Computed(_) | FactState::Computing => continue,
            FactState::Uncomputed => set_node_state(analysis, node, FactState::Computing)?,
        }
        pending.push((node, true));
        for dependency in node_dependencies(module, node, analysis)?.into_iter().rev() {
            analysis.record_dependency(dependency, node);
            if matches!(
                node_state(analysis, dependency),
                Some(FactState::Uncomputed)
            ) {
                pending.push((dependency, false));
            }
        }
    }
    match node_state(analysis, root)? {
        FactState::Computed(facts) => Some(facts),
        FactState::Uncomputed | FactState::Computing => None,
    }
}

fn node_width(module: &WordModule, node: FactNode) -> Option<u32> {
    match node {
        FactNode::Value(value) => module.value(value).map(|stored| stored.ty.width()),
        FactNode::Signal(signal) => module.signal(signal).map(|stored| stored.ty.width()),
    }
}

fn node_state(analysis: &KnownBitsAnalysis, node: FactNode) -> Option<FactState> {
    match node {
        FactNode::Value(value) => analysis.values.get(value.index()).copied(),
        FactNode::Signal(signal) => analysis.signals.get(signal.index()).copied(),
    }
}

fn set_node_state(
    analysis: &mut KnownBitsAnalysis,
    node: FactNode,
    state: FactState,
) -> Option<()> {
    match node {
        FactNode::Value(value) => *analysis.values.get_mut(value.index())? = state,
        FactNode::Signal(signal) => *analysis.signals.get_mut(signal.index())? = state,
    }
    Some(())
}

fn node_dependencies(
    module: &WordModule,
    node: FactNode,
    analysis: &KnownBitsAnalysis,
) -> Option<Vec<FactNode>> {
    match node {
        FactNode::Value(value) => match &module.value(value)?.kind {
            ValueKind::Signal(reference) => Some(vec![FactNode::Signal(reference.signal)]),
            ValueKind::Constant(_) => Some(Vec::new()),
            ValueKind::Operation(operation) => {
                let mut dependencies = Vec::new();
                module
                    .operation(*operation)?
                    .kind
                    .for_each_input(|input| dependencies.push(FactNode::Value(input)));
                Some(dependencies)
            }
        },
        FactNode::Signal(signal) => {
            let connects = analysis.drivers.for_signal(signal).collect::<Vec<_>>();
            if analysis.external_signals.get(signal.index()).copied()?
                || connects.is_empty()
                || connects.iter().any(|&index| {
                    module
                        .connects()
                        .get(index)
                        .is_some_and(|connect| connect.target.dynamic.is_some())
                })
            {
                return Some(Vec::new());
            }
            connects
                .into_iter()
                .map(|index| {
                    module
                        .connects()
                        .get(index)
                        .map(|connect| FactNode::Value(connect.value))
                })
                .collect()
        }
    }
}

fn evaluate_node(
    module: &WordModule,
    node: FactNode,
    analysis: &mut KnownBitsAnalysis,
) -> Option<FactRange> {
    match node {
        FactNode::Value(id) => {
            let value = module.value(id)?;
            Some(match &value.kind {
                ValueKind::Signal(reference) => {
                    let signal = node_facts(module, FactNode::Signal(reference.signal), analysis)?;
                    slice_facts(signal, reference.lsb, reference.width(), analysis)
                }
                ValueKind::Constant(bits) => store_bits(
                    bits.as_slice().iter().rev().map(|bit| match bit {
                        BitVal::Zero => KnownBit::Zero,
                        BitVal::One => KnownBit::One,
                        BitVal::X | BitVal::Z => KnownBit::Unknown,
                    }),
                    bits.width(),
                    &mut analysis.arena,
                ),
                ValueKind::Operation(operation) => {
                    let operation = module.operation(*operation)?;
                    derive_operation(module, &operation.kind, value.ty, analysis)
                }
            })
        }
        FactNode::Signal(signal) => evaluate_signal(module, signal, analysis),
    }
}

fn evaluate_signal(
    module: &WordModule,
    signal: SignalId,
    analysis: &mut KnownBitsAnalysis,
) -> Option<FactRange> {
    let width = module.signal(signal)?.ty.width();
    let connect_indices = analysis.drivers.for_signal(signal).collect::<Vec<_>>();
    let external = analysis.external_signals[signal.index()];
    if external
        || connect_indices.is_empty()
        || connect_indices.iter().any(|&index| {
            module
                .connects()
                .get(index)
                .is_some_and(|connect| connect.target.dynamic.is_some())
        })
    {
        return Some(FactRange::unknown(width));
    }
    let words = word_count(width);
    let mut assigned = vec![0u64; words];
    let mut bits = vec![FactWord::default(); words];
    for index in connect_indices {
        let connect = module.connects().get(index)?;
        let source = node_facts(module, FactNode::Value(connect.value), analysis)?;
        let target_width = connect
            .target
            .range
            .map_or(width, super::model::BitRange::width);
        for offset in 0..target_width {
            let target = match connect.target.range {
                Some(range) if range.msb >= range.lsb => range.lsb.checked_add(offset)?,
                Some(range) => range.lsb.checked_sub(offset)?,
                None => offset,
            };
            let word = target as usize / u64::BITS as usize;
            let mask = 1u64 << (target % u64::BITS);
            if assigned[word] & mask == 0 {
                assigned[word] |= mask;
                set_bit(&mut bits, target, source.bit(&analysis.arena, offset));
            } else {
                set_bit(&mut bits, target, KnownBit::Unknown);
            }
        }
    }
    Some(store_words(bits, width, &mut analysis.arena))
}

fn node_facts(
    module: &WordModule,
    node: FactNode,
    analysis: &KnownBitsAnalysis,
) -> Option<FactRange> {
    match node_state(analysis, node)? {
        FactState::Computed(facts) => Some(facts),
        FactState::Computing | FactState::Uncomputed => {
            Some(FactRange::unknown(node_width(module, node)?))
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive operation transfer table is kept adjacent for auditability"
)]
fn derive_operation(
    module: &WordModule,
    operation: &OpKind,
    result_ty: super::WordType,
    analysis: &mut KnownBitsAnalysis,
) -> FactRange {
    let width = result_ty.width();
    match operation {
        OpKind::Unary { op, arg } => {
            let input = value_facts(module, *arg, analysis);
            match op {
                UnaryOp::BitNot => store_generated(analysis, width, |arena, index| {
                    invert(input.bit(arena, index))
                }),
                UnaryOp::LogicalNot => store_scalar(
                    invert(logical_value(input, &analysis.arena)),
                    &mut analysis.arena,
                ),
                UnaryOp::ReductionAnd => {
                    store_scalar(reduction_and(input, &analysis.arena), &mut analysis.arena)
                }
                UnaryOp::ReductionOr => {
                    store_scalar(logical_value(input, &analysis.arena), &mut analysis.arena)
                }
                UnaryOp::ReductionXor => {
                    store_scalar(reduction_xor(input, &analysis.arena), &mut analysis.arena)
                }
            }
        }
        OpKind::Binary { op, left, right } => {
            let left_id = *left;
            let right_id = *right;
            let left = value_facts(module, left_id, analysis);
            let right = value_facts(module, right_id, analysis);
            let comparison_signed = module
                .value(left_id)
                .zip(module.value(right_id))
                .is_some_and(|(left, right)| left.ty.is_signed() && right.ty.is_signed());
            derive_binary(*op, left, right, result_ty, comparison_signed, analysis)
        }
        OpKind::Mux {
            cond,
            then_value,
            else_value,
        } => {
            let cond = value_facts(module, *cond, analysis);
            let then_value = value_facts(module, *then_value, analysis);
            let else_value = value_facts(module, *else_value, analysis);
            match logical_value(cond, &analysis.arena) {
                KnownBit::One => copy_facts(then_value, analysis),
                KnownBit::Zero => copy_facts(else_value, analysis),
                KnownBit::Unknown => store_generated(analysis, width, |arena, index| {
                    merge_equal(then_value.bit(arena, index), else_value.bit(arena, index))
                }),
            }
        }
        OpKind::Concat { parts } => {
            let inputs = parts
                .iter()
                .rev()
                .map(|&part| value_facts(module, part, analysis))
                .collect::<Vec<_>>();
            let bits = {
                let arena = &analysis.arena;
                inputs
                    .iter()
                    .flat_map(|facts| (0..facts.width).map(|index| facts.bit(arena, index)))
                    .collect::<Vec<_>>()
            };
            store_bits(bits, width, &mut analysis.arena)
        }
        OpKind::Extract { value, lsb, width } => {
            let input = value_facts(module, *value, analysis);
            slice_facts(input, *lsb, width.get(), analysis)
        }
        OpKind::DynamicExtract {
            value,
            offset,
            width,
        } => {
            let input = value_facts(module, *value, analysis);
            let offset = value_facts(module, *offset, analysis);
            match known_usize(offset, &analysis.arena) {
                Some(offset) => match u32::try_from(offset)
                    .ok()
                    .filter(|offset| offset.saturating_add(width.get()) <= input.width)
                {
                    Some(offset) => slice_facts(input, offset, width.get(), analysis),
                    None => store_generated(analysis, width.get(), |_, _| KnownBit::Zero),
                },
                None if is_zero(input, &analysis.arena) => {
                    store_generated(analysis, width.get(), |_, _| KnownBit::Zero)
                }
                None => FactRange::unknown(width.get()),
            }
        }
        OpKind::DynamicInsert {
            value,
            offset,
            replacement,
        } => {
            let input = value_facts(module, *value, analysis);
            let offset = value_facts(module, *offset, analysis);
            let replacement = value_facts(module, *replacement, analysis);
            let Some(offset) = known_usize(offset, &analysis.arena)
                .and_then(|offset| u32::try_from(offset).ok())
                .filter(|offset| offset.saturating_add(replacement.width) <= width)
            else {
                return FactRange::unknown(width);
            };
            store_generated(analysis, width, |arena, index| {
                if (offset..offset + replacement.width).contains(&index) {
                    replacement.bit(arena, index - offset)
                } else {
                    input.bit(arena, index)
                }
            })
        }
        OpKind::Cast {
            kind,
            value,
            target,
        } => {
            let input = value_facts(module, *value, analysis);
            store_generated(analysis, target.width(), |arena, index| {
                if index < input.width {
                    input.bit(arena, index)
                } else {
                    match kind {
                        CastKind::ZeroExtend => KnownBit::Zero,
                        CastKind::SignExtend => input.bit(arena, input.width.saturating_sub(1)),
                        CastKind::Truncate => KnownBit::Unknown,
                    }
                }
            })
        }
        OpKind::TriState { .. } | OpKind::Register(_) | OpKind::Latch(_) => {
            FactRange::unknown(width)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive binary transfer table shares carry, comparison, and shift semantics"
)]
fn derive_binary(
    op: BinaryOp,
    left: FactRange,
    right: FactRange,
    result_ty: super::WordType,
    comparison_signed: bool,
    analysis: &mut KnownBitsAnalysis,
) -> FactRange {
    let width = result_ty.width();
    match op {
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor => {
            store_generated(analysis, width, |arena, index| {
                bitwise(op, left.bit(arena, index), right.bit(arena, index))
            })
        }
        BinaryOp::Add | BinaryOp::Sub => {
            let subtract = op == BinaryOp::Sub;
            let mut carry = if subtract {
                KnownBit::One
            } else {
                KnownBit::Zero
            };
            let bits = (0..width)
                .map(|index| {
                    let a = left.bit(&analysis.arena, index);
                    let mut b = right.bit(&analysis.arena, index);
                    if subtract {
                        b = invert(b);
                    }
                    let (sum, next) = full_adder(a, b, carry);
                    carry = next;
                    sum
                })
                .collect::<Vec<_>>();
            store_bits(bits, width, &mut analysis.arena)
        }
        BinaryOp::Mul => {
            if is_zero(left, &analysis.arena) || is_zero(right, &analysis.arena) {
                return store_bits(
                    std::iter::repeat_n(KnownBit::Zero, width as usize),
                    width,
                    &mut analysis.arena,
                );
            }
            if is_one(left, &analysis.arena) && right.width == width {
                return copy_facts(right, analysis);
            }
            if is_one(right, &analysis.arena) && left.width == width {
                return copy_facts(left, analysis);
            }
            if left.width == width
                && let Some(shift) = power_of_two(right, &analysis.arena)
            {
                return store_generated(analysis, width, |arena, index| {
                    index
                        .checked_sub(shift)
                        .map_or(KnownBit::Zero, |source| left.bit(arena, source))
                });
            }
            if right.width == width
                && let Some(shift) = power_of_two(left, &analysis.arena)
            {
                return store_generated(analysis, width, |arena, index| {
                    index
                        .checked_sub(shift)
                        .map_or(KnownBit::Zero, |source| right.bit(arena, source))
                });
            }
            let zeros = trailing_zeros(left, &analysis.arena)
                .saturating_add(trailing_zeros(right, &analysis.arena))
                .min(width);
            store_bits(
                (0..width).map(|index| {
                    if index < zeros {
                        KnownBit::Zero
                    } else {
                        KnownBit::Unknown
                    }
                }),
                width,
                &mut analysis.arena,
            )
        }
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr => store_scalar(
            logical_binary(
                op,
                logical_value(left, &analysis.arena),
                logical_value(right, &analysis.arena),
            ),
            &mut analysis.arena,
        ),
        BinaryOp::Eq | BinaryOp::Ne => {
            let equal = equality(left, right, &analysis.arena);
            store_scalar(
                if op == BinaryOp::Eq {
                    equal
                } else {
                    invert(equal)
                },
                &mut analysis.arena,
            )
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr => {
            let Some(shift) = known_usize(right, &analysis.arena) else {
                return FactRange::unknown(width);
            };
            store_generated(analysis, width, |arena, index| match op {
                BinaryOp::Shl if usize::try_from(index).expect("u32 fits usize") < shift => {
                    KnownBit::Zero
                }
                BinaryOp::Shl => left.bit(
                    arena,
                    index - u32::try_from(shift).expect("a shift below index fits u32"),
                ),
                BinaryOp::Shr
                    if usize::try_from(index)
                        .expect("u32 fits usize")
                        .saturating_add(shift)
                        >= usize::try_from(left.width).expect("u32 fits usize") =>
                {
                    KnownBit::Zero
                }
                BinaryOp::Ashr
                    if usize::try_from(index)
                        .expect("u32 fits usize")
                        .saturating_add(shift)
                        >= usize::try_from(left.width).expect("u32 fits usize") =>
                {
                    if result_ty.is_signed() {
                        left.bit(arena, left.width.saturating_sub(1))
                    } else {
                        KnownBit::Zero
                    }
                }
                BinaryOp::Shr | BinaryOp::Ashr => left.bit(
                    arena,
                    index + u32::try_from(shift).expect("an in-range shift fits u32"),
                ),
                _ => KnownBit::Unknown,
            })
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let value =
                compare_facts(left, right, comparison_signed, &analysis.arena).map(|ordering| {
                    match op {
                        BinaryOp::Lt => ordering.is_lt(),
                        BinaryOp::Le => ordering.is_le(),
                        BinaryOp::Gt => ordering.is_gt(),
                        BinaryOp::Ge => ordering.is_ge(),
                        _ => unreachable!("comparison operation was filtered"),
                    }
                });
            store_scalar(
                value.map_or(KnownBit::Unknown, |value| {
                    if value { KnownBit::One } else { KnownBit::Zero }
                }),
                &mut analysis.arena,
            )
        }
        BinaryOp::Div | BinaryOp::Mod => FactRange::unknown(width),
    }
}

fn compare_facts(
    left: FactRange,
    right: FactRange,
    signed: bool,
    arena: &[FactWord],
) -> Option<std::cmp::Ordering> {
    let width = left.width.max(right.width);
    let extended_bit = |value: FactRange, index: u32| {
        if index < value.width {
            value.bit(arena, index)
        } else if signed {
            value.bit(arena, value.width.saturating_sub(1))
        } else {
            KnownBit::Zero
        }
    };
    if signed {
        let left_negative = extended_bit(left, width.saturating_sub(1));
        let right_negative = extended_bit(right, width.saturating_sub(1));
        match (left_negative, right_negative) {
            (KnownBit::One, KnownBit::Zero) => return Some(std::cmp::Ordering::Less),
            (KnownBit::Zero, KnownBit::One) => return Some(std::cmp::Ordering::Greater),
            (KnownBit::Unknown, _) | (_, KnownBit::Unknown) => return None,
            _ => {}
        }
    }
    for index in (0..width).rev() {
        match (extended_bit(left, index), extended_bit(right, index)) {
            (KnownBit::Zero, KnownBit::One) => return Some(std::cmp::Ordering::Less),
            (KnownBit::One, KnownBit::Zero) => return Some(std::cmp::Ordering::Greater),
            (KnownBit::Zero, KnownBit::Zero) | (KnownBit::One, KnownBit::One) => {}
            (KnownBit::Unknown, _) | (_, KnownBit::Unknown) => return None,
        }
    }
    Some(std::cmp::Ordering::Equal)
}

fn value_facts(module: &WordModule, value: ValueId, analysis: &mut KnownBitsAnalysis) -> FactRange {
    node_facts(module, FactNode::Value(value), analysis).unwrap_or_else(|| {
        FactRange::unknown(module.value(value).map_or(0, |stored| stored.ty.width()))
    })
}

fn slice_facts(
    input: FactRange,
    lsb: u32,
    width: u32,
    analysis: &mut KnownBitsAnalysis,
) -> FactRange {
    if lsb == 0 && width == input.width {
        return input;
    }
    store_generated(analysis, width, |arena, index| {
        input.bit(arena, lsb + index)
    })
}

fn copy_facts(input: FactRange, analysis: &mut KnownBitsAnalysis) -> FactRange {
    store_generated(analysis, input.width, |arena, index| {
        input.bit(arena, index)
    })
}

fn store_scalar(bit: KnownBit, arena: &mut Vec<FactWord>) -> FactRange {
    store_bits([bit], 1, arena)
}

fn store_bits(
    bits: impl IntoIterator<Item = KnownBit>,
    width: u32,
    arena: &mut Vec<FactWord>,
) -> FactRange {
    let mut words = vec![FactWord::default(); word_count(width)];
    for (index, bit) in bits
        .into_iter()
        .take(usize::try_from(width).expect("u32 fits usize"))
        .enumerate()
    {
        set_bit(
            &mut words,
            u32::try_from(index).expect("enumeration is bounded by width"),
            bit,
        );
    }
    store_words(words, width, arena)
}

fn store_generated(
    analysis: &mut KnownBitsAnalysis,
    width: u32,
    mut bit: impl FnMut(&[FactWord], u32) -> KnownBit,
) -> FactRange {
    let mut words = vec![FactWord::default(); word_count(width)];
    {
        let arena = &analysis.arena;
        for index in 0..width {
            set_bit(&mut words, index, bit(arena, index));
        }
    }
    store_words(words, width, &mut analysis.arena)
}

fn store_words(mut words: Vec<FactWord>, width: u32, arena: &mut Vec<FactWord>) -> FactRange {
    if let Some(last) = words.last_mut() {
        let tail = width % u64::BITS;
        if tail != 0 {
            let mask = (1u64 << tail) - 1;
            last.zeros &= mask;
            last.ones &= mask;
        }
    }
    if words.iter().all(|word| word.zeros == 0 && word.ones == 0) {
        return FactRange::unknown(width);
    }
    let start = arena.len();
    arena.extend(words);
    FactRange {
        start: Some(start),
        width,
    }
}

fn word_count(width: u32) -> usize {
    width.div_ceil(u64::BITS) as usize
}

fn set_bit(words: &mut [FactWord], index: u32, bit: KnownBit) {
    let Some(word) = words.get_mut(index as usize / u64::BITS as usize) else {
        return;
    };
    let mask = 1u64 << (index % u64::BITS);
    word.zeros &= !mask;
    word.ones &= !mask;
    match bit {
        KnownBit::Zero => word.zeros |= mask,
        KnownBit::One => word.ones |= mask,
        KnownBit::Unknown => {}
    }
}

fn invert(bit: KnownBit) -> KnownBit {
    match bit {
        KnownBit::Zero => KnownBit::One,
        KnownBit::One => KnownBit::Zero,
        KnownBit::Unknown => KnownBit::Unknown,
    }
}

fn merge_equal(left: KnownBit, right: KnownBit) -> KnownBit {
    if left == right {
        left
    } else {
        KnownBit::Unknown
    }
}

fn bitwise(op: BinaryOp, left: KnownBit, right: KnownBit) -> KnownBit {
    match op {
        BinaryOp::BitAnd => match (left, right) {
            (KnownBit::Zero, _) | (_, KnownBit::Zero) => KnownBit::Zero,
            (KnownBit::One, KnownBit::One) => KnownBit::One,
            _ => KnownBit::Unknown,
        },
        BinaryOp::BitOr => match (left, right) {
            (KnownBit::One, _) | (_, KnownBit::One) => KnownBit::One,
            (KnownBit::Zero, KnownBit::Zero) => KnownBit::Zero,
            _ => KnownBit::Unknown,
        },
        BinaryOp::BitXor => match (left, right) {
            (KnownBit::Zero, KnownBit::Zero) | (KnownBit::One, KnownBit::One) => KnownBit::Zero,
            (KnownBit::Zero, KnownBit::One) | (KnownBit::One, KnownBit::Zero) => KnownBit::One,
            _ => KnownBit::Unknown,
        },
        _ => KnownBit::Unknown,
    }
}

fn logical_value(input: FactRange, arena: &[FactWord]) -> KnownBit {
    let mut all_zero = true;
    for index in 0..input.width {
        match input.bit(arena, index) {
            KnownBit::One => return KnownBit::One,
            KnownBit::Unknown => all_zero = false,
            KnownBit::Zero => {}
        }
    }
    if all_zero {
        KnownBit::Zero
    } else {
        KnownBit::Unknown
    }
}

fn reduction_and(input: FactRange, arena: &[FactWord]) -> KnownBit {
    let mut all_one = true;
    for index in 0..input.width {
        match input.bit(arena, index) {
            KnownBit::Zero => return KnownBit::Zero,
            KnownBit::Unknown => all_one = false,
            KnownBit::One => {}
        }
    }
    if all_one {
        KnownBit::One
    } else {
        KnownBit::Unknown
    }
}

fn reduction_xor(input: FactRange, arena: &[FactWord]) -> KnownBit {
    let mut parity = false;
    for index in 0..input.width {
        match input.bit(arena, index) {
            KnownBit::Zero => {}
            KnownBit::One => parity = !parity,
            KnownBit::Unknown => return KnownBit::Unknown,
        }
    }
    if parity {
        KnownBit::One
    } else {
        KnownBit::Zero
    }
}

fn logical_binary(op: BinaryOp, left: KnownBit, right: KnownBit) -> KnownBit {
    match op {
        BinaryOp::LogicalAnd => bitwise(BinaryOp::BitAnd, left, right),
        BinaryOp::LogicalOr => bitwise(BinaryOp::BitOr, left, right),
        _ => KnownBit::Unknown,
    }
}

fn equality(left: FactRange, right: FactRange, arena: &[FactWord]) -> KnownBit {
    let mut complete = true;
    for index in 0..left.width.max(right.width) {
        let left = left.bit(arena, index);
        let right = right.bit(arena, index);
        if matches!(
            (left, right),
            (KnownBit::Zero, KnownBit::One) | (KnownBit::One, KnownBit::Zero)
        ) {
            return KnownBit::Zero;
        }
        complete &= left != KnownBit::Unknown && right != KnownBit::Unknown;
    }
    if complete {
        KnownBit::One
    } else {
        KnownBit::Unknown
    }
}

fn full_adder(left: KnownBit, right: KnownBit, carry: KnownBit) -> (KnownBit, KnownBit) {
    let mut sum = MergedBool::Unset;
    let mut next = MergedBool::Unset;
    for left in possibilities(left) {
        for right in possibilities(right) {
            for carry in possibilities(carry) {
                let value = u8::from(*left) + u8::from(*right) + u8::from(*carry);
                sum = merge_bool(sum, value & 1 != 0);
                next = merge_bool(next, value >= 2);
            }
        }
    }
    (known_bool(sum), known_bool(next))
}

fn possibilities(bit: KnownBit) -> &'static [bool] {
    match bit {
        KnownBit::Zero => &[false],
        KnownBit::One => &[true],
        KnownBit::Unknown => &[false, true],
    }
}

#[derive(Clone, Copy)]
enum MergedBool {
    Unset,
    Known(bool),
    Unknown,
}

fn merge_bool(current: MergedBool, value: bool) -> MergedBool {
    match current {
        MergedBool::Unset => MergedBool::Known(value),
        MergedBool::Known(current) if current == value => MergedBool::Known(value),
        MergedBool::Known(_) | MergedBool::Unknown => MergedBool::Unknown,
    }
}

fn known_bool(value: MergedBool) -> KnownBit {
    match value {
        MergedBool::Known(false) => KnownBit::Zero,
        MergedBool::Known(true) => KnownBit::One,
        MergedBool::Unset | MergedBool::Unknown => KnownBit::Unknown,
    }
}

fn trailing_zeros(input: FactRange, arena: &[FactWord]) -> u32 {
    (0..input.width)
        .take_while(|&index| input.bit(arena, index) == KnownBit::Zero)
        .count()
        .try_into()
        .expect("the count is bounded by a u32 signal width")
}

fn is_zero(input: FactRange, arena: &[FactWord]) -> bool {
    trailing_zeros(input, arena) == input.width
}

fn is_one(input: FactRange, arena: &[FactWord]) -> bool {
    input.width > 0
        && input.bit(arena, 0) == KnownBit::One
        && (1..input.width).all(|index| input.bit(arena, index) == KnownBit::Zero)
}

fn power_of_two(input: FactRange, arena: &[FactWord]) -> Option<u32> {
    let mut one = None;
    for index in 0..input.width {
        match input.bit(arena, index) {
            KnownBit::Zero => {}
            KnownBit::One if one.is_none() => one = Some(index),
            KnownBit::One | KnownBit::Unknown => return None,
        }
    }
    one
}

fn known_usize(input: FactRange, arena: &[FactWord]) -> Option<usize> {
    let mut value = 0usize;
    for index in 0..input.width {
        match input.bit(arena, index) {
            KnownBit::Zero => {}
            KnownBit::One if index < usize::BITS => value |= 1usize << index,
            KnownBit::One | KnownBit::Unknown => return None,
        }
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word::{BitRange, LValue, LogicStateKind, SourceSpan, WordType};

    fn ty(width: u32) -> WordType {
        WordType::new(width, false, LogicStateKind::FourState).unwrap()
    }

    #[test]
    fn evaluates_ordering_after_exact_arithmetic() {
        let mut module = WordModule::new("comparisons");
        let signed = WordType::new(4, true, LogicStateKind::FourState).unwrap();
        let zero = module
            .constant(
                ConstBits::from_bin_str("0000").unwrap(),
                signed,
                SourceSpan::default(),
            )
            .unwrap();
        let one = module
            .constant(
                ConstBits::from_bin_str("0001").unwrap(),
                signed,
                SourceSpan::default(),
            )
            .unwrap();
        let four = module
            .constant(
                ConstBits::from_bin_str("0100").unwrap(),
                signed,
                SourceSpan::default(),
            )
            .unwrap();
        let minus_one = module
            .constant(
                ConstBits::from_bin_str("1111").unwrap(),
                signed,
                SourceSpan::default(),
            )
            .unwrap();
        let incremented = module
            .binary(BinaryOp::Add, zero, one, SourceSpan::default())
            .unwrap();
        let below_bound = module
            .binary(BinaryOp::Lt, incremented, four, SourceSpan::default())
            .unwrap();
        let negative_below_zero = module
            .binary(BinaryOp::Lt, minus_one, zero, SourceSpan::default())
            .unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(
            facts.constant(&module, below_bound),
            Some(ConstBits::from_bin_str("1").unwrap())
        );
        assert_eq!(
            facts.constant(&module, negative_below_zero),
            Some(ConstBits::from_bin_str("1").unwrap())
        );
    }

    #[test]
    fn evaluates_multiplication_by_an_exact_power_of_two() {
        let mut module = WordModule::new("power_of_two_product");
        let signed = WordType::new(32, true, LogicStateKind::FourState).unwrap();
        let unsigned = WordType::new(32, false, LogicStateKind::FourState).unwrap();
        let offset = WordType::new(35, false, LogicStateKind::FourState).unwrap();
        let three = module
            .constant(
                ConstBits::from_bin_str(&format!("{}11", "0".repeat(30))).unwrap(),
                signed,
                SourceSpan::default(),
            )
            .unwrap();
        let three = module
            .cast(CastKind::ZeroExtend, three, unsigned, SourceSpan::default())
            .unwrap();
        let three = module
            .cast(CastKind::ZeroExtend, three, offset, SourceSpan::default())
            .unwrap();
        let eight = module
            .constant(
                ConstBits::from_bin_str(&format!("{}1000", "0".repeat(31))).unwrap(),
                offset,
                SourceSpan::default(),
            )
            .unwrap();
        let product = module
            .binary(BinaryOp::Mul, three, eight, SourceSpan::default())
            .unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(
            facts.constant(&module, product),
            Some(ConstBits::from_bin_str(&format!("{}11000", "0".repeat(30))).unwrap())
        );
    }

    #[test]
    fn proves_partial_bitwise_and_shift_facts() {
        let mut module = WordModule::new("facts");
        let input = module
            .add_port("a", PortDirection::Input, ty(8), SourceSpan::default())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let mask = module
            .constant(
                ConstBits::from_bin_str("11110000").unwrap(),
                ty(8),
                SourceSpan::default(),
            )
            .unwrap();
        let masked = module
            .binary(BinaryOp::BitAnd, input, mask, SourceSpan::default())
            .unwrap();
        let shift = module
            .constant(
                ConstBits::from_bin_str("00000011").unwrap(),
                ty(8),
                SourceSpan::default(),
            )
            .unwrap();
        let shifted = module
            .binary(BinaryOp::Shl, masked, shift, SourceSpan::default())
            .unwrap();

        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.bit(&module, shifted, 7), KnownBit::Unknown);
    }

    #[test]
    fn follows_static_vector_connections() {
        let mut module = WordModule::new("signals");
        let signal = module
            .add_wire("value", ty(4), SourceSpan::default())
            .unwrap();
        let constant = module
            .constant(
                ConstBits::from_bin_str("1010").unwrap(),
                ty(4),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(signal), constant, SourceSpan::default())
            .unwrap();
        let slice = module
            .read_signal_slice(signal, 1, 2, SourceSpan::default())
            .unwrap();

        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(
            facts.constant(&module, slice),
            Some(ConstBits::from_bin_str("01").unwrap())
        );
    }

    #[test]
    fn append_only_sync_updates_values_and_signal_drivers() {
        let mut module = WordModule::new("incremental");
        let signal = module
            .add_wire("value", ty(1), SourceSpan::default())
            .unwrap();
        let read = module.read_signal(signal, SourceSpan::default()).unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.constant(&module, read), None);

        let zero = module
            .constant(
                ConstBits::from_bin_str("0").unwrap(),
                ty(1),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(signal), zero, SourceSpan::default())
            .unwrap();
        facts.sync_append_only(&module);
        assert_eq!(
            facts.constant(&module, read),
            Some(ConstBits::from_bin_str("0").unwrap())
        );

        let one = module
            .constant(
                ConstBits::from_bin_str("1").unwrap(),
                ty(1),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(signal), one, SourceSpan::default())
            .unwrap();
        facts.sync_append_only(&module);
        assert_eq!(facts.constant(&module, read), None);
        assert_eq!(
            facts.constant(&module, one),
            Some(ConstBits::from_bin_str("1").unwrap())
        );
    }

    #[test]
    fn frozen_connectivity_extends_values_without_admitting_new_drivers() {
        let mut module = WordModule::new("frozen_connectivity");
        let signal = module
            .add_wire("value", ty(1), SourceSpan::default())
            .unwrap();
        let read = module.read_signal(signal, SourceSpan::default()).unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);
        assert_eq!(facts.constant(&module, read), None);

        let one = module
            .constant(
                ConstBits::from_bin_str("1").unwrap(),
                ty(1),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(signal), one, SourceSpan::default())
            .unwrap();

        facts.extend_frozen_connectivity(&module);
        assert_eq!(facts.constant(&module, read), None);
        assert_eq!(
            facts.constant(&module, one),
            Some(ConstBits::from_bin_str("1").unwrap())
        );

        facts.sync_append_only(&module);
        assert_eq!(
            facts.constant(&module, read),
            Some(ConstBits::from_bin_str("1").unwrap())
        );
    }

    #[test]
    fn append_only_sync_preserves_facts_outside_the_changed_signal_cone() {
        let mut module = WordModule::new("incremental_dependencies");
        let changed = module
            .add_wire("changed", ty(1), SourceSpan::default())
            .unwrap();
        let stable = module
            .add_wire("stable", ty(1), SourceSpan::default())
            .unwrap();
        let changed_read = module.read_signal(changed, SourceSpan::default()).unwrap();
        let stable_read = module.read_signal(stable, SourceSpan::default()).unwrap();
        let zero = module
            .constant(
                ConstBits::from_bin_str("0").unwrap(),
                ty(1),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(stable), zero, SourceSpan::default())
            .unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.constant(&module, changed_read), None);
        assert_eq!(
            facts.constant(&module, stable_read),
            Some(ConstBits::from_bin_str("0").unwrap())
        );
        assert_eq!(
            facts.constant(&module, zero),
            Some(ConstBits::from_bin_str("0").unwrap())
        );
        let arena_len = facts.arena.len();

        let one = module
            .constant(
                ConstBits::from_bin_str("1").unwrap(),
                ty(1),
                SourceSpan::default(),
            )
            .unwrap();
        module
            .connect(LValue::signal(changed), one, SourceSpan::default())
            .unwrap();
        facts.sync_append_only(&module);

        assert!(matches!(facts.values[zero.index()], FactState::Computed(_)));
        assert!(matches!(
            facts.values[stable_read.index()],
            FactState::Computed(_)
        ));
        assert!(matches!(
            facts.signals[stable.index()],
            FactState::Computed(_)
        ));
        assert!(matches!(
            facts.values[changed_read.index()],
            FactState::Uncomputed
        ));
        assert_eq!(facts.arena.len(), arena_len);
        assert_eq!(
            facts.constant(&module, changed_read),
            Some(ConstBits::from_bin_str("1").unwrap())
        );
    }

    #[test]
    fn keeps_mux_common_bits() {
        let mut module = WordModule::new("operators");
        let cond = module
            .add_port("cond", PortDirection::Input, ty(1), SourceSpan::default())
            .unwrap();
        let cond = module
            .read_signal(module.port(cond).unwrap().signal, SourceSpan::default())
            .unwrap();
        let left = module
            .constant(
                ConstBits::from_bin_str("1010").unwrap(),
                ty(4),
                SourceSpan::default(),
            )
            .unwrap();
        let right = module
            .constant(
                ConstBits::from_bin_str("1110").unwrap(),
                ty(4),
                SourceSpan::default(),
            )
            .unwrap();
        let selected = module
            .mux(cond, left, right, SourceSpan::default())
            .unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.bit(&module, selected, 0), KnownBit::Zero);
        assert_eq!(facts.bit(&module, selected, 1), KnownBit::One);
        assert_eq!(facts.bit(&module, selected, 2), KnownBit::Unknown);
        assert_eq!(facts.bit(&module, selected, 3), KnownBit::One);
    }

    #[test]
    fn treats_multiple_drivers_as_unknown() {
        let mut module = WordModule::new("multiple");
        let signal = module
            .add_wire("value", ty(1), SourceSpan::default())
            .unwrap();
        for text in ["0", "1"] {
            let value = module
                .constant(
                    ConstBits::from_bin_str(text).unwrap(),
                    ty(1),
                    SourceSpan::default(),
                )
                .unwrap();
            module
                .connect(
                    LValue::signal(signal).with_range(BitRange { msb: 0, lsb: 0 }),
                    value,
                    SourceSpan::default(),
                )
                .unwrap();
        }
        let value = module.read_signal(signal, SourceSpan::default()).unwrap();

        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.bit(&module, value, 0), KnownBit::Unknown);
    }

    #[test]
    fn preserves_four_state_self_xor_unknowns() {
        let mut module = WordModule::new("self_xor");
        let input = module
            .add_port("a", PortDirection::Input, ty(1), SourceSpan::default())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let xor = module
            .binary(BinaryOp::BitXor, input, input, SourceSpan::default())
            .unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.bit(&module, xor, 0), KnownBit::Unknown);
    }

    #[test]
    fn preserves_unknown_signed_extension_through_multiply_identity() {
        let mut module = WordModule::new("signed_width");
        let narrow = WordType::new(4, true, LogicStateKind::FourState).unwrap();
        let wide = WordType::new(8, true, LogicStateKind::FourState).unwrap();
        let input = module
            .add_port("a", PortDirection::Input, narrow, SourceSpan::default())
            .unwrap();
        let input = module
            .read_signal(module.port(input).unwrap().signal, SourceSpan::default())
            .unwrap();
        let one = module
            .constant(
                ConstBits::from_bin_str("00000001").unwrap(),
                wide,
                SourceSpan::default(),
            )
            .unwrap();
        let product = module
            .binary(BinaryOp::Mul, one, input, SourceSpan::default())
            .unwrap();

        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.active_width(&module, product, 8), 8);
    }

    #[test]
    fn packed_fact_rejects_values_wider_than_its_storage() {
        let mut module = WordModule::new("wide_facts");
        let at_limit = module
            .constant(
                ConstBits::from_bin_str(&"1".repeat(u128::BITS as usize)).unwrap(),
                ty(u128::BITS),
                SourceSpan::default(),
            )
            .unwrap();
        let over_limit = module
            .constant(
                ConstBits::from_bin_str(&"1".repeat(u128::BITS as usize + 1)).unwrap(),
                ty(u128::BITS + 1),
                SourceSpan::default(),
            )
            .unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);

        let packed = facts
            .packed128(&module, at_limit)
            .expect("128-bit facts fit the compact representation");
        assert_eq!(packed.width(), u128::BITS);
        assert_eq!(packed.bit(u128::BITS - 1), KnownBit::One);
        assert_eq!(facts.packed128(&module, over_limit), None);
        assert_eq!(facts.bit(&module, over_limit, u128::BITS), KnownBit::One);
    }

    #[test]
    fn evaluates_deep_ssa_chains_without_using_the_call_stack() {
        let mut module = WordModule::new("deep");
        let mut value = module
            .constant(
                ConstBits::from_bin_str("0").unwrap(),
                ty(1),
                SourceSpan::default(),
            )
            .unwrap();
        for _ in 0..20_000 {
            value = module
                .unary(UnaryOp::BitNot, value, SourceSpan::default())
                .unwrap();
        }
        let mut facts = KnownBitsAnalysis::new(&module);

        assert_eq!(facts.bit(&module, value, 0), KnownBit::Zero);
    }
    #[test]
    fn append_query_cycles_track_newly_driven_bits() {
        // Mirrors the RTL normalization pattern behind issue #111: append one
        // slice driver, synchronize, then query known bits before the next
        // append. Facts must track each newly driven bit without losing any
        // previously proven bit.
        let mut module = WordModule::new("append_query_loop");
        let source = SourceSpan::default();
        let width = 64;
        let signal = module.add_wire("w", ty(width), source.clone()).unwrap();
        let read = module.read_signal(signal, source.clone()).unwrap();
        let mut facts = KnownBitsAnalysis::new(&module);
        for step in 0..width {
            let value = module
                .constant(ConstBits::from_bin_str("1").unwrap(), ty(1), source.clone())
                .unwrap();
            module
                .connect(
                    LValue::signal(signal).with_range(BitRange {
                        msb: step,
                        lsb: step,
                    }),
                    value,
                    source.clone(),
                )
                .unwrap();
            facts.sync_append_only(&module);
            for bit in 0..width {
                let expected = if bit <= step {
                    KnownBit::One
                } else {
                    KnownBit::Unknown
                };
                assert_eq!(
                    facts.bit(&module, read, bit),
                    expected,
                    "bit {bit} after driving {step} slices"
                );
            }
        }
    }
}
