// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Compact Boolean networks with complemented edges.
//!
//! [`LogicNetwork`] owns a dense node arena. [`Lit`] stores inversion
//! in the edge rather than allocating inverter nodes, and node zero represents
//! the shared constant. Builders hash structurally equivalent nodes so equal
//! operations converge on one canonical ID.
//!
//! IDs belong to one network and become stable when construction ends. Rewrite
//! and mapping passes must carry explicit remaps when compacting a network;
//! numeric equality across two networks has no semantic meaning.

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Capacity failure while constructing a compact logic network.
pub struct LogicError(String);

impl LogicError {
    fn capacity(kind: &str) -> Self {
        Self(format!("logic {kind} exceeds 32-bit capacity"))
    }
}

impl fmt::Display for LogicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LogicError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
/// Dense node identifier local to one [`LogicNetwork`] or [`LogicBuilder`].
pub struct NodeId(u32);

impl NodeId {
    /// Identifier reserved for the shared constant node.
    pub const CONSTANT: Self = Self(0);

    /// Converts a dense arena index into a node ID.
    ///
    /// Returns an error when `index` cannot be represented by the 32-bit
    /// storage used in literals.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] when `index` exceeds the 32-bit node arena.
    pub fn from_index(index: usize) -> Result<Self, LogicError> {
        u32::try_from(index)
            .map(Self)
            .map_err(|_| LogicError::capacity("node ID"))
    }

    #[must_use]
    /// Returns the zero-based arena index.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
/// A logic edge encoded as a node ID plus an inversion bit.
///
/// Bit zero stores the phase, so inverting a literal never allocates a node.
pub struct Lit(u32);

impl Lit {
    /// Constant-zero literal.
    pub const FALSE: Self = Self(0);
    /// Constant-one literal.
    pub const TRUE: Self = Self(1);

    /// Creates the positive-phase literal for `node`.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] when shifting the node ID to reserve the phase bit
    /// would overflow the literal encoding.
    pub fn from_node(node: NodeId) -> Result<Self, LogicError> {
        node.0
            .checked_mul(2)
            .map(Self)
            .ok_or_else(|| LogicError::capacity("literal"))
    }

    #[must_use]
    /// Returns the node referenced by this edge, discarding its phase.
    pub const fn node(self) -> NodeId {
        NodeId(self.0 >> 1)
    }

    #[must_use]
    /// Returns whether the edge complements the referenced node.
    pub const fn is_inverted(self) -> bool {
        self.0 & 1 != 0
    }

    #[must_use]
    /// Returns the same edge with its phase toggled.
    pub const fn inverted(self) -> Self {
        Self(self.0 ^ 1)
    }

    #[must_use]
    /// Returns the positive-phase edge for the referenced node.
    pub const fn positive(self) -> Self {
        Self(self.0 & !1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
/// Primitive operation represented by a logic-network node.
pub enum NodeKind {
    /// The shared constant node at [`NodeId::CONSTANT`].
    Constant,
    /// An externally driven primary or boundary input.
    Input,
    /// Two-input conjunction.
    And,
    /// Two-input exclusive-or.
    Xor,
    /// Three-input `(select, then, else)` multiplexer.
    Mux,
}

#[derive(Debug, Clone)]
/// Immutable, dense Boolean network produced by [`LogicBuilder::freeze`].
pub struct LogicNetwork {
    kinds: Box<[NodeKind]>,
    fanin0: Box<[Lit]>,
    fanin1: Box<[Lit]>,
    fanin2: Box<[Lit]>,
    levels: Box<[u32]>,
    origins: Box<[u32]>,
}

impl LogicNetwork {
    #[must_use]
    /// Returns the number of nodes, including the shared constant node.
    pub fn node_count(&self) -> usize {
        self.kinds.len()
    }

    #[must_use]
    /// Returns the primitive kind of `node`, or `None` for a foreign ID.
    pub fn kind(&self, node: NodeId) -> Option<NodeKind> {
        self.kinds.get(node.index()).copied()
    }

    #[must_use]
    /// Returns the arity of `node`, or `None` for a foreign ID.
    pub fn fanin_count(&self, node: NodeId) -> Option<usize> {
        Some(match self.kinds.get(node.index())? {
            NodeKind::Constant | NodeKind::Input => 0,
            NodeKind::And | NodeKind::Xor => 2,
            NodeKind::Mux => 3,
        })
    }

    #[must_use]
    ///
    /// `None` means either the node ID or fanin index is out of range.
    pub fn fanin(&self, node: NodeId, index: usize) -> Option<Lit> {
        let node = node.index();
        match index {
            0 => self.fanin0.get(node).copied(),
            1 => self.fanin1.get(node).copied(),
            2 => self.fanin2.get(node).copied(),
            _ => None,
        }
    }

    #[must_use]
    /// Returns the topological level of a node.
    pub fn level(&self, node: NodeId) -> Option<u32> {
        self.levels.get(node.index()).copied()
    }

    #[must_use]
    /// Returns the caller-defined provenance token attached to `node`.
    pub fn origin(&self, node: NodeId) -> Option<u32> {
        self.origins.get(node.index()).copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum NodeKey {
    And(Lit, Lit),
    Xor(Lit, Lit),
    Mux(Lit, Lit, Lit),
}

#[derive(Debug, Clone)]
/// Canonicalizing builder for [`LogicNetwork`].
///
/// Commutative fanins are ordered, structurally equal nodes are interned, and
/// local Boolean identities are simplified before allocation.
pub struct LogicBuilder {
    kinds: Vec<NodeKind>,
    fanin0: Vec<Lit>,
    fanin1: Vec<Lit>,
    fanin2: Vec<Lit>,
    levels: Vec<u32>,
    origins: Vec<u32>,
    interned: HashMap<NodeKey, NodeId>,
}

impl Default for LogicBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl LogicBuilder {
    #[must_use]
    /// Creates a builder containing only the shared constant node.
    pub fn new() -> Self {
        Self {
            kinds: vec![NodeKind::Constant],
            fanin0: vec![Lit::FALSE],
            fanin1: vec![Lit::FALSE],
            fanin2: vec![Lit::FALSE],
            levels: vec![0],
            origins: vec![0],
            interned: HashMap::new(),
        }
    }

    /// Appends an unconstrained input carrying the supplied provenance token.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] when the compact node or literal arena is full.
    pub fn input(&mut self, origin: u32) -> Result<Lit, LogicError> {
        self.push(NodeKind::Input, Lit::FALSE, Lit::FALSE, Lit::FALSE, origin)
    }

    #[must_use]
    /// Returns the number of nodes allocated so far.
    pub fn node_count(&self) -> usize {
        self.kinds.len()
    }

    #[must_use]
    /// Returns the primitive kind of a builder-local node.
    pub fn kind(&self, node: NodeId) -> Option<NodeKind> {
        self.kinds.get(node.index()).copied()
    }

    #[must_use]
    /// Returns fanin `index` of a builder-local node.
    pub fn fanin(&self, node: NodeId, index: usize) -> Option<Lit> {
        let node = node.index();
        match index {
            0 => self.fanin0.get(node).copied(),
            1 => self.fanin1.get(node).copied(),
            2 => self.fanin2.get(node).copied(),
            _ => None,
        }
    }

    #[must_use]
    /// Returns the current topological level of a builder-local node.
    pub fn level(&self, node: NodeId) -> Option<u32> {
        self.levels.get(node.index()).copied()
    }

    #[must_use]
    /// Returns the canonical literal for a Boolean constant.
    pub const fn constant(value: bool) -> Lit {
        if value { Lit::TRUE } else { Lit::FALSE }
    }

    #[must_use]
    /// Complements a literal without allocating an inverter node.
    pub const fn not(value: Lit) -> Lit {
        value.inverted()
    }

    /// Builds a canonical disjunction and applies local simplifications.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] if the canonical result requires a node beyond
    /// the compact arena capacity.
    pub fn or(&mut self, left: Lit, right: Lit, origin: u32) -> Result<Lit, LogicError> {
        canonical_or(self, left, right, origin)
    }

    /// Builds a canonical conjunction and applies local simplifications.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] if the canonical result requires a node beyond
    /// the compact arena capacity.
    pub fn and(&mut self, left: Lit, right: Lit, origin: u32) -> Result<Lit, LogicError> {
        canonical_and(self, left, right, origin)
    }

    /// Builds a canonical exclusive-or and applies parity simplifications.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] if the canonical result requires a node beyond
    /// the compact arena capacity.
    pub fn xor(&mut self, left: Lit, right: Lit, origin: u32) -> Result<Lit, LogicError> {
        canonical_xor(self, left, right, origin)
    }

    /// Builds a canonical two-way multiplexer.
    ///
    /// `select = true` chooses `then_value`; `select = false` chooses
    /// `else_value`.
    ///
    /// # Errors
    ///
    /// Returns [`LogicError`] if the canonical result requires a node beyond
    /// the compact arena capacity.
    pub fn mux(
        &mut self,
        select: Lit,
        then_value: Lit,
        else_value: Lit,
        origin: u32,
    ) -> Result<Lit, LogicError> {
        canonical_mux(self, select, then_value, else_value, origin)
    }

    /// Seals the builder into immutable, tightly sized storage.
    #[must_use]
    pub fn freeze(self) -> LogicNetwork {
        LogicNetwork {
            kinds: self.kinds.into_boxed_slice(),
            fanin0: self.fanin0.into_boxed_slice(),
            fanin1: self.fanin1.into_boxed_slice(),
            fanin2: self.fanin2.into_boxed_slice(),
            levels: self.levels.into_boxed_slice(),
            origins: self.origins.into_boxed_slice(),
        }
    }

    fn intern(
        &mut self,
        key: NodeKey,
        kind: NodeKind,
        fanin0: Lit,
        fanin1: Lit,
        fanin2: Lit,
        origin: u32,
    ) -> Result<Lit, LogicError> {
        if let Some(&node) = self.interned.get(&key) {
            return Lit::from_node(node);
        }
        let literal = self.push(kind, fanin0, fanin1, fanin2, origin)?;
        self.interned.insert(key, literal.node());
        Ok(literal)
    }

    fn push(
        &mut self,
        kind: NodeKind,
        fanin0: Lit,
        fanin1: Lit,
        fanin2: Lit,
        origin: u32,
    ) -> Result<Lit, LogicError> {
        let node = NodeId::from_index(self.kinds.len())?;
        let level = [fanin0, fanin1, fanin2]
            .into_iter()
            .take(match kind {
                NodeKind::Constant | NodeKind::Input => 0,
                NodeKind::And | NodeKind::Xor => 2,
                NodeKind::Mux => 3,
            })
            .filter_map(|fanin| self.levels.get(fanin.node().index()).copied())
            .max()
            .unwrap_or(0)
            .checked_add(u32::from(!matches!(
                kind,
                NodeKind::Constant | NodeKind::Input
            )))
            .ok_or_else(|| LogicError::capacity("level"))?;
        self.kinds.push(kind);
        self.fanin0.push(fanin0);
        self.fanin1.push(fanin1);
        self.fanin2.push(fanin2);
        self.levels.push(level);
        self.origins.push(origin);
        Lit::from_node(node)
    }
}

fn ordered(left: Lit, right: Lit) -> (Lit, Lit) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn xor_remaining(fanins: (Lit, Lit), shared: Lit) -> Option<Lit> {
    if fanins.0 == shared {
        Some(fanins.1)
    } else if fanins.1 == shared {
        Some(fanins.0)
    } else {
        None
    }
}

fn xor_shared_remaining(left: (Lit, Lit), right: (Lit, Lit)) -> Option<(Lit, Lit)> {
    if let Some(remaining) = xor_remaining(right, left.0) {
        Some((left.1, remaining))
    } else {
        xor_remaining(right, left.1).map(|remaining| (left.0, remaining))
    }
}

fn complementary_product_factor(left: (Lit, Lit), right: (Lit, Lit)) -> Option<(Lit, Lit, Lit)> {
    for (left_select, when_true) in [(left.0, left.1), (left.1, left.0)] {
        if right.0 == left_select.inverted() {
            return Some((left_select, when_true, right.1));
        }
        if right.1 == left_select.inverted() {
            return Some((left_select, when_true, right.0));
        }
    }
    None
}

fn with_phase(value: Lit, inverted: bool) -> Lit {
    if inverted { value.inverted() } else { value }
}

/// The read-and-resolve surface the canonicalization rules need.
///
/// Implemented once by [`LogicBuilder`], which allocates a node whenever a
/// canonical key is missing, and once by [`LogicProbe`], which reports the key
/// as absent instead. Both therefore agree on which structural node a given
/// expression denotes, which is what lets a caller price a replacement by the
/// nodes it would actually have to create.
trait Canonical {
    /// Reported when a canonical key has no node, as [`LogicProbe`] does.
    type Error;

    fn kind(&self, node: NodeId) -> Option<NodeKind>;

    fn fanin(&self, node: NodeId, index: usize) -> Option<Lit>;

    fn resolve(
        &mut self,
        key: NodeKey,
        kind: NodeKind,
        fanin0: Lit,
        fanin1: Lit,
        fanin2: Lit,
        origin: u32,
    ) -> Result<Lit, Self::Error>;
}

impl Canonical for LogicBuilder {
    type Error = LogicError;

    fn kind(&self, node: NodeId) -> Option<NodeKind> {
        Self::kind(self, node)
    }

    fn fanin(&self, node: NodeId, index: usize) -> Option<Lit> {
        Self::fanin(self, node, index)
    }

    fn resolve(
        &mut self,
        key: NodeKey,
        kind: NodeKind,
        fanin0: Lit,
        fanin1: Lit,
        fanin2: Lit,
        origin: u32,
    ) -> Result<Lit, Self::Error> {
        self.intern(key, kind, fanin0, fanin1, fanin2, origin)
    }
}

/// Structural hash table of a frozen [`LogicNetwork`], rebuilt from its nodes.
///
/// [`LogicBuilder::freeze`] drops the builder's table because the frozen
/// network never allocates again; a caller that wants to ask whether an
/// expression is already present rebuilds it once and shares it.
#[derive(Debug, Clone, Default)]
pub struct StructuralIndex {
    nodes: HashMap<NodeKey, NodeId>,
}

impl StructuralIndex {
    #[must_use]
    /// Indexes every gate of `network` under its canonical structural key.
    pub fn of(network: &LogicNetwork) -> Self {
        let mut nodes = HashMap::new();
        for index in 0..network.node_count() {
            let Some(node) = NodeId::from_index(index).ok() else {
                break;
            };
            let fanin = |slot| network.fanin(node, slot).unwrap_or(Lit::FALSE);
            let key = match network.kind(node) {
                Some(NodeKind::And) => NodeKey::And(fanin(0), fanin(1)),
                Some(NodeKind::Xor) => NodeKey::Xor(fanin(0), fanin(1)),
                Some(NodeKind::Mux) => NodeKey::Mux(fanin(0), fanin(1), fanin(2)),
                _ => continue,
            };
            nodes.insert(key, node);
        }
        Self { nodes }
    }
}

/// Marks a canonical key that the indexed network does not contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Absent;

/// Read-only view that answers which literal an expression *would* denote in an
/// already frozen network, without building anything.
///
/// Every method returns [`Absent`] as soon as one canonical key is missing.
/// That is exact rather than conservative: a node the network does not contain
/// cannot be a fanin of one it does, so no ancestor can be present either.
#[derive(Debug, Clone, Copy)]
pub struct LogicProbe<'a> {
    network: &'a LogicNetwork,
    index: &'a StructuralIndex,
}

impl<'a> LogicProbe<'a> {
    #[must_use]
    /// Views `network` through `index`, which must have been built from it.
    pub fn new(network: &'a LogicNetwork, index: &'a StructuralIndex) -> Self {
        Self { network, index }
    }

    /// Resolves a conjunction against the network.
    ///
    /// # Errors
    ///
    /// Returns [`Absent`] when the network contains no such node.
    pub fn and(&mut self, left: Lit, right: Lit) -> Result<Lit, Absent> {
        canonical_and(self, left, right, 0)
    }

    /// Resolves a disjunction against the network.
    ///
    /// # Errors
    ///
    /// Returns [`Absent`] when the network contains no such node.
    pub fn or(&mut self, left: Lit, right: Lit) -> Result<Lit, Absent> {
        canonical_or(self, left, right, 0)
    }

    /// Resolves an exclusive-or against the network.
    ///
    /// # Errors
    ///
    /// Returns [`Absent`] when the network contains no such node.
    pub fn xor(&mut self, left: Lit, right: Lit) -> Result<Lit, Absent> {
        canonical_xor(self, left, right, 0)
    }

    /// Resolves a multiplexer against the network.
    ///
    /// # Errors
    ///
    /// Returns [`Absent`] when the network contains no such node.
    pub fn mux(&mut self, select: Lit, then_value: Lit, else_value: Lit) -> Result<Lit, Absent> {
        canonical_mux(self, select, then_value, else_value, 0)
    }
}

impl Canonical for LogicProbe<'_> {
    type Error = Absent;

    fn kind(&self, node: NodeId) -> Option<NodeKind> {
        self.network.kind(node)
    }

    fn fanin(&self, node: NodeId, index: usize) -> Option<Lit> {
        self.network.fanin(node, index)
    }

    fn resolve(
        &mut self,
        key: NodeKey,
        _kind: NodeKind,
        _fanin0: Lit,
        _fanin1: Lit,
        _fanin2: Lit,
        _origin: u32,
    ) -> Result<Lit, Self::Error> {
        let node = self.index.nodes.get(&key).copied().ok_or(Absent)?;
        Lit::from_node(node).map_err(|_| Absent)
    }
}

/// Canonicalizes a conjunction. Shared by every [`Canonical`] store so the builder and
/// the read-only probe agree on which structural node an expression denotes.
fn canonical_and<C: Canonical + ?Sized>(
    store: &mut C,
    left: Lit,
    right: Lit,
    origin: u32,
) -> Result<Lit, C::Error> {
    // Constant, idempotence, absorption, and consensus identities run
    // before interning so semantically redundant nodes never enter the
    // structural hash table.
    if left == Lit::FALSE || right == Lit::FALSE || left == right.inverted() {
        return Ok(Lit::FALSE);
    }
    if left == Lit::TRUE {
        return Ok(right);
    }
    if right == Lit::TRUE || left == right {
        return Ok(left);
    }
    if binary_fanins(store, left, NodeKind::And)
        .is_some_and(|fanins| fanins.0 == right || fanins.1 == right)
    {
        return Ok(left);
    }
    if binary_fanins(store, right, NodeKind::And)
        .is_some_and(|fanins| fanins.0 == left || fanins.1 == left)
    {
        return Ok(right);
    }
    if right.is_inverted()
        && binary_fanins(store, right.positive(), NodeKind::And)
            .is_some_and(|fanins| fanins.0 == left.inverted() || fanins.1 == left.inverted())
    {
        return Ok(left);
    }
    if left.is_inverted()
        && binary_fanins(store, left.positive(), NodeKind::And)
            .is_some_and(|fanins| fanins.0 == right.inverted() || fanins.1 == right.inverted())
    {
        return Ok(right);
    }
    let (left, right) = ordered(left, right);
    store.resolve(
        NodeKey::And(left, right),
        NodeKind::And,
        left,
        right,
        Lit::FALSE,
        origin,
    )
}

/// Canonicalizes a disjunction. Shared by every [`Canonical`] store so the builder and
/// the read-only probe agree on which structural node an expression denotes.
fn canonical_or<C: Canonical + ?Sized>(
    store: &mut C,
    left: Lit,
    right: Lit,
    origin: u32,
) -> Result<Lit, C::Error> {
    // Recognize the sum-of-products mux form before De Morgan lowering;
    // doing so preserves a mux primitive for mapping and avoids four nodes.
    if let (Some(left_product), Some(right_product)) = (
        binary_fanins(store, left, NodeKind::And),
        binary_fanins(store, right, NodeKind::And),
    ) && let Some((select, when_true, when_false)) =
        complementary_product_factor(left_product, right_product)
    {
        return canonical_mux(store, select, when_true, when_false, origin);
    }
    canonical_and(store, left.inverted(), right.inverted(), origin).map(Lit::inverted)
}

/// Canonicalizes a exclusive-or. Shared by every [`Canonical`] store so the builder and
/// the read-only probe agree on which structural node an expression denotes.
fn canonical_xor<C: Canonical + ?Sized>(
    store: &mut C,
    left: Lit,
    right: Lit,
    origin: u32,
) -> Result<Lit, C::Error> {
    if left == Lit::FALSE {
        return Ok(right);
    }
    if right == Lit::FALSE {
        return Ok(left);
    }
    if left == Lit::TRUE {
        return Ok(right.inverted());
    }
    if right == Lit::TRUE {
        return Ok(left.inverted());
    }
    if left == right {
        return Ok(Lit::FALSE);
    }
    if left == right.inverted() {
        return Ok(Lit::TRUE);
    }
    // Phase is normalized onto the result; the structural key therefore
    // contains only positive fanins and has a unique commutative form.
    let inverted = left.is_inverted() ^ right.is_inverted();
    let left = left.positive();
    let right = right.positive();
    if left == right {
        return Ok(LogicBuilder::constant(inverted));
    }
    let left_fanins = binary_fanins(store, left, NodeKind::Xor);
    let right_fanins = binary_fanins(store, right, NodeKind::Xor);
    if let Some(remaining) = left_fanins.and_then(|fanins| xor_remaining(fanins, right)) {
        return Ok(with_phase(remaining, inverted));
    }
    if let Some(remaining) = right_fanins.and_then(|fanins| xor_remaining(fanins, left)) {
        return Ok(with_phase(remaining, inverted));
    }
    if let (Some(left_fanins), Some(right_fanins)) = (left_fanins, right_fanins)
        && let Some((left_remaining, right_remaining)) =
            xor_shared_remaining(left_fanins, right_fanins)
    {
        return canonical_xor(store, left_remaining, right_remaining, origin)
            .map(|value| with_phase(value, inverted));
    }
    let (left, right) = ordered(left, right);
    store
        .resolve(
            NodeKey::Xor(left, right),
            NodeKind::Xor,
            left,
            right,
            Lit::FALSE,
            origin,
        )
        .map(|value| with_phase(value, inverted))
}

/// Canonicalizes a two-way multiplexer. Shared by every [`Canonical`] store so the builder and
/// the read-only probe agree on which structural node an expression denotes.
#[allow(
    clippy::too_many_lines,
    reason = "the ordered mux identities form one canonicalization decision table"
)]
fn canonical_mux<C: Canonical + ?Sized>(
    store: &mut C,
    select: Lit,
    then_value: Lit,
    else_value: Lit,
    origin: u32,
) -> Result<Lit, C::Error> {
    if select == Lit::FALSE {
        return Ok(else_value);
    }
    if select == Lit::TRUE || then_value == else_value {
        return Ok(then_value);
    }
    if then_value == Lit::TRUE && else_value == Lit::FALSE {
        return Ok(select);
    }
    if then_value == Lit::FALSE && else_value == Lit::TRUE {
        return Ok(select.inverted());
    }
    // Normalize an inverted selector by swapping arms, then push a common
    // inverted arm phase onto the result. This makes the intern key unique.
    let (select, mut then_value, mut else_value) = if select.is_inverted() {
        (select.positive(), else_value, then_value)
    } else {
        (select, then_value, else_value)
    };
    let inverted = then_value.is_inverted() && else_value.is_inverted();
    if inverted {
        then_value = then_value.positive();
        else_value = else_value.positive();
    }
    if then_value == else_value.inverted() {
        return canonical_xor(store, select, else_value, origin)
            .map(|value| with_phase(value, inverted));
    }
    let reduced = if then_value == Lit::TRUE || then_value == select {
        Some(canonical_or(store, select, else_value, origin)?)
    } else if then_value == Lit::FALSE || then_value == select.inverted() {
        Some(canonical_and(store, select.inverted(), else_value, origin)?)
    } else if else_value == Lit::FALSE || else_value == select {
        Some(canonical_and(store, select, then_value, origin)?)
    } else if else_value == Lit::TRUE || else_value == select.inverted() {
        Some(canonical_or(store, select.inverted(), then_value, origin)?)
    } else {
        None
    };
    if let Some(reduced) = reduced {
        return Ok(with_phase(reduced, inverted));
    }
    // Collapse nested muxes controlled by the same selector before trying
    // the more expensive cross-arm factoring rules below.
    let mut changed = false;
    if let Some((inner_select, inner_then, _)) = mux_fanins(store, then_value)
        && inner_select == select
    {
        then_value = inner_then;
        changed = true;
    }
    if let Some((inner_select, _, inner_else)) = mux_fanins(store, else_value)
        && inner_select == select
    {
        else_value = inner_else;
        changed = true;
    }
    if changed {
        return canonical_mux(store, select, then_value, else_value, origin)
            .map(|value| with_phase(value, inverted));
    }
    if let Some((inner_select, inner_then, inner_else)) = mux_fanins(store, then_value) {
        let combined = if else_value == inner_else {
            Some(canonical_and(store, select, inner_select, origin)?)
        } else if else_value == inner_then {
            Some(canonical_or(
                store,
                select.inverted(),
                inner_select,
                origin,
            )?)
        } else {
            None
        };
        if let Some(combined) = combined {
            return canonical_mux(store, combined, inner_then, inner_else, origin)
                .map(|value| with_phase(value, inverted));
        }
    }
    if let Some((inner_select, inner_then, inner_else)) = mux_fanins(store, else_value) {
        let combined = if then_value == inner_then {
            Some(canonical_or(store, select, inner_select, origin)?)
        } else if then_value == inner_else {
            Some(canonical_and(
                store,
                select.inverted(),
                inner_select,
                origin,
            )?)
        } else {
            None
        };
        if let Some(combined) = combined {
            return canonical_mux(store, combined, inner_then, inner_else, origin)
                .map(|value| with_phase(value, inverted));
        }
    }
    if let (Some(then_mux), Some(else_mux)) =
        (mux_fanins(store, then_value), mux_fanins(store, else_value))
        && then_mux.0 == else_mux.0
    {
        let factored = if then_mux.1 == else_mux.1 {
            let remaining = canonical_mux(store, select, then_mux.2, else_mux.2, origin)?;
            Some(canonical_mux(
                store, then_mux.0, then_mux.1, remaining, origin,
            )?)
        } else if then_mux.2 == else_mux.2 {
            let remaining = canonical_mux(store, select, then_mux.1, else_mux.1, origin)?;
            Some(canonical_mux(
                store, then_mux.0, remaining, then_mux.2, origin,
            )?)
        } else {
            None
        };
        if let Some(factored) = factored {
            return Ok(with_phase(factored, inverted));
        }
    }
    let result = store.resolve(
        NodeKey::Mux(select, then_value, else_value),
        NodeKind::Mux,
        select,
        then_value,
        else_value,
        origin,
    )?;
    Ok(with_phase(result, inverted))
}

fn binary_fanins<C: Canonical + ?Sized>(
    store: &C,
    value: Lit,
    kind: NodeKind,
) -> Option<(Lit, Lit)> {
    if value.is_inverted() || store.kind(value.node())? != kind {
        return None;
    }
    Some((store.fanin(value.node(), 0)?, store.fanin(value.node(), 1)?))
}

fn mux_fanins<C: Canonical + ?Sized>(store: &C, value: Lit) -> Option<(Lit, Lit, Lit)> {
    if value.is_inverted() || store.kind(value.node())? != NodeKind::Mux {
        return None;
    }
    Some((
        store.fanin(value.node(), 0)?,
        store.fanin(value.node(), 1)?,
        store.fanin(value.node(), 2)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_resolves_exactly_the_expressions_the_builder_would_not_allocate() {
        let mut builder = LogicBuilder::new();
        let inputs = (1..=4)
            .map(|origin| builder.input(origin).unwrap())
            .collect::<Vec<_>>();
        let [a, b, c, d] = [inputs[0], inputs[1], inputs[2], inputs[3]];
        let ab = builder.and(a, b, 0).unwrap();
        let cd = builder.and(c, d, 0).unwrap();
        let parity = builder.xor(a, c, 0).unwrap();
        let chosen = builder.mux(b, ab, cd, 0).unwrap();
        let network = builder.clone().freeze();
        let index = StructuralIndex::of(&network);
        let mut probe = LogicProbe::new(&network, &index);

        // Every expression already built resolves to the same literal, through
        // interning, commutativity, and edge-phase normalization alike.
        assert_eq!(probe.and(b, a), Ok(ab));
        assert_eq!(probe.and(d, c), Ok(cd));
        assert_eq!(probe.xor(c, a), Ok(parity));
        assert_eq!(probe.xor(c.inverted(), a), Ok(parity.inverted()));
        assert_eq!(probe.mux(b, ab, cd), Ok(chosen));

        // Identities that allocate nothing hold without consulting the index.
        assert_eq!(probe.and(ab, ab), Ok(ab));
        assert_eq!(probe.and(ab, ab.inverted()), Ok(Lit::FALSE));
        assert_eq!(probe.and(ab, a), Ok(ab));

        // An expression with no node reports itself absent, and so does every
        // expression built on top of it.
        let missing = builder.and(a, c, 0).unwrap();
        assert_eq!(probe.and(a, c), Err(Absent));
        assert_eq!(probe.and(builder.and(a, c, 0).unwrap(), d), Err(Absent));
        assert_eq!(builder.and(a, c, 0).unwrap(), missing);
    }

    #[test]
    fn mixed_network_hashes_nodes_and_encodes_inversion_on_edges() {
        let mut builder = LogicBuilder::new();
        let a = builder.input(1).unwrap();
        let b = builder.input(2).unwrap();
        let ab = builder.and(a, b, 3).unwrap();
        let ba = builder.and(b, a, 4).unwrap();
        let parity = builder.xor(ab, a.inverted(), 5).unwrap();
        let root = builder.mux(a, parity, ab, 6).unwrap();
        let network = builder.freeze();

        assert_eq!(ab, ba);
        assert_eq!(network.kind(root.node()), Some(NodeKind::Mux));
        assert!(network.fanin(root.node(), 1).is_some());
        assert!(a.inverted().is_inverted());
    }

    #[test]
    fn xor_canonicalizes_edge_phases_and_cancels_shared_terms() {
        let mut builder = LogicBuilder::new();
        let a = builder.input(1).unwrap();
        let b = builder.input(2).unwrap();
        let c = builder.input(3).unwrap();
        let ab = builder.xor(a, b, 4).unwrap();

        assert_eq!(builder.xor(a.inverted(), b, 5).unwrap(), ab.inverted());
        assert_eq!(builder.xor(ab, a, 6).unwrap(), b);

        let ac = builder.xor(a, c, 7).unwrap();
        let bc = builder.xor(b, c, 8).unwrap();
        assert_eq!(builder.xor(ab, ac, 9).unwrap(), bc);
    }

    #[test]
    fn mux_canonicalizes_phases_and_absorbs_same_select_nodes() {
        let mut builder = LogicBuilder::new();
        let select = builder.input(1).unwrap();
        let a = builder.input(2).unwrap();
        let b = builder.input(3).unwrap();
        let c = builder.input(4).unwrap();
        let mux = builder.mux(select, a, b, 5).unwrap();

        assert_eq!(builder.mux(select.inverted(), b, a, 6).unwrap(), mux);
        assert_eq!(
            builder.mux(select, a.inverted(), b.inverted(), 7).unwrap(),
            mux.inverted()
        );
        assert_eq!(
            builder.mux(select, mux, c, 8).unwrap(),
            builder.mux(select, a, c, 9).unwrap()
        );
        assert_eq!(
            builder.mux(select, a, a.inverted(), 10).unwrap(),
            builder.xor(select, a.inverted(), 11).unwrap()
        );
    }

    #[test]
    fn mux_factors_common_branches_and_nested_selectors() {
        let mut builder = LogicBuilder::new();
        let outer_select = builder.input(1).unwrap();
        let inner_select = builder.input(2).unwrap();
        let common = builder.input(3).unwrap();
        let outer_false = builder.input(4).unwrap();
        let inner_false = builder.input(5).unwrap();
        let nested = builder.mux(inner_select, common, outer_false, 6).unwrap();
        let both_selects = builder.and(outer_select, inner_select, 7).unwrap();

        assert_eq!(
            builder.mux(outer_select, nested, outer_false, 8).unwrap(),
            builder.mux(both_selects, common, outer_false, 9).unwrap()
        );

        let left = builder.mux(inner_select, common, outer_false, 10).unwrap();
        let right = builder.mux(inner_select, common, inner_false, 11).unwrap();
        let remaining = builder
            .mux(outer_select, outer_false, inner_false, 12)
            .unwrap();
        assert_eq!(
            builder.mux(outer_select, left, right, 13).unwrap(),
            builder.mux(inner_select, common, remaining, 14).unwrap()
        );
    }

    #[test]
    fn and_applies_boolean_absorption_without_changing_basis() {
        let mut builder = LogicBuilder::new();
        let a = builder.input(1).unwrap();
        let b = builder.input(2).unwrap();
        let ab = builder.and(a, b, 3).unwrap();
        let a_or_b = builder.or(a, b, 4).unwrap();

        assert_eq!(builder.and(a, ab, 5).unwrap(), ab);
        assert_eq!(builder.and(a, a_or_b, 6).unwrap(), a);
    }

    #[test]
    fn or_recovers_muxes_from_complementary_products() {
        let mut builder = LogicBuilder::new();
        let select = builder.input(1).unwrap();
        let when_true = builder.input(3).unwrap();
        let when_false = builder.input(4).unwrap();

        let selected_true = builder.and(select, when_true, 10).unwrap();
        let selected_false = builder.and(select.inverted(), when_false, 11).unwrap();
        let recovered = builder.or(selected_true, selected_false, 12).unwrap();
        assert_eq!(
            recovered,
            builder.mux(select, when_true, when_false, 13).unwrap()
        );
    }
}
