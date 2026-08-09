// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Flat procedural control flow with source-ordered effects.
//!
//! [`ProcBuilder`] is intentionally transient. Sealing it sorts nothing and
//! flattens blocks in insertion order, so every final arena is compact and
//! deterministic. Effects in each block execute in arena order; control-flow
//! edges preserve that order without storing a redundant token graph.

use crate::word::{BitRange, Edge, SourceSpan, ValueId};
use crate::word::{MemoryId, SignalId};
use opto_core::DenseId;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

mod builder;

pub use builder::{ProcBuilder, SwitchArmSpec};
use std::num::NonZeroU32;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
/// Construction or validation failure in procedural IR.
pub struct ProcError(String);

impl ProcError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

macro_rules! define_id {
    ($name:ident, $tag:ident, $kind:literal) => {
        enum $tag {}

        #[doc = concat!("Dense ", $kind, " identifier local to one [`ProcModule`].")]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub struct $name(DenseId<$tag>);

        impl $name {
            #[doc = concat!("First valid ", $kind, " identifier.")]
            pub const FIRST: Self = Self(DenseId::FIRST);

            #[doc = concat!("Creates a ", $kind, " identifier from a dense arena index.")]
            ///
            /// # Errors
            ///
            /// Returns [`ProcError`] when `index` exceeds the nonzero 32-bit
            /// encoding used by procedural arena references.
            pub fn from_index(index: usize) -> Result<Self, ProcError> {
                DenseId::from_index(index)
                    .map(Self)
                    .map_err(|_| ProcError::new(concat!($kind, " ID exceeds 32-bit capacity")))
            }

            #[doc = concat!("Returns the dense arena index of this ", $kind, " identifier.")]
            pub fn index(self) -> usize {
                self.0.index()
            }

            #[doc = concat!("Returns the zero-based 32-bit encoding of this ", $kind, " identifier.")]
            pub fn raw(self) -> u32 {
                self.0.get().get() - 1
            }
        }
    };
}

define_id!(ProcedureId, ProcedureTag, "procedure");
define_id!(BlockId, BlockTag, "procedural block");
define_id!(EffectId, EffectTag, "procedural effect");
define_id!(EdgeId, EdgeTag, "procedural edge");
define_id!(SwitchArmId, SwitchArmTag, "switch arm");
define_id!(EventId, EventTag, "sensitivity event");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Contiguous slice of a typed procedural arena.
///
/// Ranges are produced while sealing a [`ProcBuilder`] and are valid only for
/// the resulting [`ProcModule`].
pub struct ArenaRange<I> {
    start: u32,
    len: u32,
    marker: PhantomData<fn() -> I>,
}

impl<I> ArenaRange<I> {
    fn new(start: usize, len: usize, kind: &str) -> Result<Self, ProcError> {
        Ok(Self {
            start: u32::try_from(start)
                .map_err(|_| ProcError::new(format!("{kind} range exceeds 32-bit capacity")))?,
            len: u32::try_from(len)
                .map_err(|_| ProcError::new(format!("{kind} range exceeds 32-bit capacity")))?,
            marker: PhantomData,
        })
    }

    /// Returns the number of IDs in the range.
    #[must_use]
    pub fn len(self) -> usize {
        self.len as usize
    }

    /// Returns whether the range contains no IDs.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len == 0
    }

    fn indices(self) -> std::ops::Range<usize> {
        self.start as usize..self.start as usize + self.len as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
/// Behavioral classification inferred for one procedure.
pub enum ProcedureKind {
    /// All assigned outputs are driven on every control-flow path.
    Combinational,
    /// Procedure may be combinational or may infer storage after analysis.
    CombinationalOrLatch,
    /// Level-sensitive storage procedure.
    Latch,
    /// Edge-sensitive sequential procedure.
    FlipFlop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// One signal edge in an explicit sensitivity list.
pub struct SensitivityEvent {
    /// Scalar signal sampled by the event.
    pub signal: SignalId,
    /// Active edge polarity.
    pub edge: Edge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Trigger model for a procedure.
pub enum Sensitivity {
    /// Sensitivity is derived from the complete combinational read set.
    Implicit,
    /// Explicit edge events stored in the module event arena.
    Edges(ArenaRange<EventId>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Sealed procedure header and its owned block range.
pub struct Procedure {
    /// Behavioral classification.
    pub kind: ProcedureKind,
    /// Implicit or edge-sensitive trigger model.
    pub sensitivity: Sensitivity,
    /// Entry block, which may differ from the first source-order block.
    pub entry: BlockId,
    /// Source span of the procedure declaration.
    pub source: SourceSpan,
    blocks: ArenaRange<BlockId>,
}

impl Procedure {
    /// Returns the number of blocks owned by this procedure.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
/// Scheduling semantics of a procedural assignment.
pub enum AssignmentMode {
    /// Updates the target immediately in procedural order.
    Blocking,
    /// Schedules the update for the nonblocking assignment phase.
    Nonblocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Whole-value, fixed-range, or runtime-indexed assignment selection.
pub enum TargetSelect {
    /// Selects the complete target.
    Whole,
    /// Selects a statically known inclusive bit range.
    Static(BitRange),
    /// Selects `width` bits beginning at runtime `offset`.
    Dynamic {
        /// Unsigned runtime least-significant bit offset.
        offset: ValueId,
        /// Nonzero selected width.
        width: NonZeroU32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Signal or memory element written by a procedural effect.
pub enum ProcTarget {
    /// Signal assignment target.
    Signal {
        /// Destination signal.
        signal: SignalId,
        /// Selected signal bits.
        select: TargetSelect,
    },
    /// Addressed memory assignment target.
    Memory {
        /// Destination memory.
        memory: MemoryId,
        /// Runtime element address.
        address: ValueId,
        /// Selected bits within the addressed element.
        select: TargetSelect,
    },
}

impl ProcTarget {
    /// Creates a whole-signal assignment target.
    #[must_use]
    pub const fn signal(signal: SignalId) -> Self {
        Self::Signal {
            signal,
            select: TargetSelect::Whole,
        }
    }

    /// Creates a whole-word memory assignment target.
    #[must_use]
    pub const fn memory(memory: MemoryId, address: ValueId) -> Self {
        Self::Memory {
            memory,
            address,
            select: TargetSelect::Whole,
        }
    }

    /// Replaces the bit selection while preserving the target owner.
    #[must_use]
    pub const fn with_select(self, select: TargetSelect) -> Self {
        match self {
            Self::Signal { signal, .. } => Self::Signal { signal, select },
            Self::Memory {
                memory, address, ..
            } => Self::Memory {
                memory,
                address,
                select,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One source-ordered procedural assignment.
pub struct Effect {
    /// Blocking or nonblocking scheduling mode.
    pub mode: AssignmentMode,
    /// Destination signal or memory element.
    pub target: ProcTarget,
    /// Assigned word-level value.
    pub value: ValueId,
    /// Source span of the assignment.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Explicit control-flow edge between two blocks in one procedure.
pub struct EdgeRecord {
    /// Source block owning the terminating edge.
    pub from: BlockId,
    /// Destination block.
    pub target: BlockId,
    /// Source span of the branch or jump producing the edge.
    pub source_span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Pattern and control-flow edge for one switch arm.
pub struct SwitchArm {
    /// Word-level pattern compared with the switch selector.
    pub pattern: ValueId,
    /// Edge taken when the pattern matches.
    pub edge: EdgeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Complete terminator shape for a sealed procedural block.
pub enum TerminatorKind {
    /// Returns from the current procedure.
    Return,
    /// Unconditional control-flow transfer.
    Jump {
        /// Sole outgoing edge.
        edge: EdgeId,
    },
    /// Boolean conditional transfer.
    Branch {
        /// One-bit branch condition.
        condition: ValueId,
        /// Edge taken when the condition is true.
        then_edge: EdgeId,
        /// Edge taken when the condition is false.
        else_edge: EdgeId,
    },
    /// Ordered multi-way selection with a default edge.
    Switch {
        /// Selector compared with arm patterns.
        selector: ValueId,
        /// Contiguous range in the switch-arm arena.
        arms: ArenaRange<SwitchArmId>,
        /// Edge taken when no arm matches.
        default: EdgeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Block terminator plus its complete source span.
pub struct Terminator {
    /// Control-flow operation.
    pub kind: TerminatorKind,
    /// Source span of the terminating statement.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Basic block containing an ordered effect range and one terminator.
pub struct Block {
    /// Procedure that exclusively owns this block.
    pub procedure: ProcedureId,
    /// Required final control-flow operation.
    pub terminator: Terminator,
    /// Source span covering the block.
    pub source: SourceSpan,
    effects: ArenaRange<EffectId>,
}

impl Block {
    /// Returns the number of source-ordered effects in this block.
    #[must_use]
    pub fn effect_count(&self) -> usize {
        self.effects.len()
    }
}

/// Immutable, sealed procedural IR. All collections are single dense arenas.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProcModule {
    procedures: Box<[Procedure]>,
    blocks: Box<[Block]>,
    effects: Box<[Effect]>,
    edges: Box<[EdgeRecord]>,
    switch_arms: Box<[SwitchArm]>,
    events: Box<[SensitivityEvent]>,
}

impl ProcModule {
    /// Returns procedure headers in stable insertion order.
    #[must_use]
    pub fn procedures(&self) -> &[Procedure] {
        &self.procedures
    }

    /// Returns all blocks grouped contiguously by owning procedure.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// Returns all effects grouped contiguously by owning block.
    #[must_use]
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    /// Returns all control-flow edges grouped by source block.
    #[must_use]
    pub fn edges(&self) -> &[EdgeRecord] {
        &self.edges
    }

    /// Returns all sensitivity events grouped by procedure.
    #[must_use]
    pub fn events(&self) -> &[SensitivityEvent] {
        &self.events
    }

    /// Looks up a procedure by ID.
    #[must_use]
    pub fn procedure(&self, id: ProcedureId) -> Option<&Procedure> {
        self.procedures.get(id.index())
    }

    /// Looks up a basic block by ID.
    #[must_use]
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id.index())
    }

    /// Looks up a procedural effect by ID.
    #[must_use]
    pub fn effect(&self, id: EffectId) -> Option<&Effect> {
        self.effects.get(id.index())
    }

    /// Looks up a control-flow edge by ID.
    #[must_use]
    pub fn edge(&self, id: EdgeId) -> Option<&EdgeRecord> {
        self.edges.get(id.index())
    }

    /// Iterates block IDs owned by `procedure` in source order.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed procedure contains a block range that cannot be
    /// represented by [`BlockId`]; construction and validation prevent this.
    #[must_use]
    pub fn procedure_blocks(
        &self,
        procedure: ProcedureId,
    ) -> Option<impl ExactSizeIterator<Item = BlockId> + '_> {
        let range = self.procedure(procedure)?.blocks;
        Some(range.indices().map(|index| {
            BlockId::from_index(index).expect("sealed block ranges contain valid IDs")
        }))
    }

    /// Iterates IDs and effects owned by `block` in execution order.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed block contains an effect range outside the
    /// compact effect-ID space.
    #[must_use]
    pub fn block_effects(
        &self,
        block: BlockId,
    ) -> Option<impl ExactSizeIterator<Item = (EffectId, &Effect)> + '_> {
        let range = self.block(block)?.effects;
        Some(range.indices().map(|index| {
            let id = EffectId::from_index(index).expect("sealed effect ranges contain valid IDs");
            (id, &self.effects[index])
        }))
    }

    /// Iterates explicit edge events for `procedure`.
    ///
    /// Returns `None` for an unknown procedure or implicit sensitivity.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed event range cannot be represented by [`EventId`].
    #[must_use]
    pub fn sensitivity_events(
        &self,
        procedure: ProcedureId,
    ) -> Option<impl ExactSizeIterator<Item = (EventId, &SensitivityEvent)> + '_> {
        let Sensitivity::Edges(range) = self.procedure(procedure)?.sensitivity else {
            return None;
        };
        Some(range.indices().map(|index| {
            let id = EventId::from_index(index).expect("sealed event ranges contain valid IDs");
            (id, &self.events[index])
        }))
    }

    /// Iterates ordered switch arms for `block`.
    ///
    /// Returns `None` when the block is unknown or has another terminator.
    ///
    /// # Panics
    ///
    /// Panics only if a sealed switch range cannot be represented by
    /// [`SwitchArmId`].
    #[must_use]
    pub fn switch_arms(
        &self,
        block: BlockId,
    ) -> Option<impl ExactSizeIterator<Item = (SwitchArmId, &SwitchArm)> + '_> {
        let TerminatorKind::Switch { arms, .. } = self.block(block)?.terminator.kind else {
            return None;
        };
        Some(arms.indices().map(|index| {
            let id =
                SwitchArmId::from_index(index).expect("sealed switch ranges contain valid IDs");
            (id, &self.switch_arms[index])
        }))
    }

    /// Validates arena ownership, ID ranges, edge use, and reachability.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] on the first violated range, ownership,
    /// terminator, edge, or reachability invariant.
    pub fn validate(&self) -> Result<(), ProcError> {
        let mut next_block = 0usize;
        let mut next_event = 0usize;
        for (index, procedure) in self.procedures.iter().enumerate() {
            let id = ProcedureId::from_index(index)?;
            if procedure.blocks.start as usize != next_block || procedure.blocks.is_empty() {
                return Err(ProcError::new(format!(
                    "procedure {id:?} does not own a non-empty contiguous block range"
                )));
            }
            let block_end = checked_end(procedure.blocks, self.blocks.len(), "procedure block")?;
            if !procedure
                .blocks
                .indices()
                .contains(&procedure.entry.index())
            {
                return Err(ProcError::new(format!(
                    "procedure {id:?} entry block is outside the procedure"
                )));
            }
            for block_index in procedure.blocks.indices() {
                if self.blocks[block_index].procedure != id {
                    return Err(ProcError::new(format!(
                        "block {block_index} has the wrong owning procedure"
                    )));
                }
            }
            next_block = block_end;

            match (procedure.kind, procedure.sensitivity) {
                (
                    ProcedureKind::Combinational
                    | ProcedureKind::CombinationalOrLatch
                    | ProcedureKind::Latch,
                    Sensitivity::Implicit,
                ) => {}
                (ProcedureKind::FlipFlop, Sensitivity::Edges(events)) if !events.is_empty() => {
                    if events.start as usize != next_event {
                        return Err(ProcError::new(
                            "sensitivity event ranges are not contiguous",
                        ));
                    }
                    next_event = checked_end(events, self.events.len(), "sensitivity event")?;
                }
                _ => {
                    return Err(ProcError::new(format!(
                        "procedure {id:?} has incompatible kind and sensitivity"
                    )));
                }
            }
        }
        if next_block != self.blocks.len() || next_event != self.events.len() {
            return Err(ProcError::new("procedural arenas contain unowned records"));
        }

        let mut next_effect = 0usize;
        let mut edge_uses = vec![0u8; self.edges.len()];
        let mut next_arm = 0usize;
        for (index, block) in self.blocks.iter().enumerate() {
            let id = BlockId::from_index(index)?;
            if block.effects.start as usize != next_effect {
                return Err(ProcError::new("block effect ranges are not contiguous"));
            }
            next_effect = checked_end(block.effects, self.effects.len(), "block effect")?;
            self.validate_terminator(id, &block.terminator, &mut edge_uses, &mut next_arm)?;
        }
        if next_effect != self.effects.len()
            || next_arm != self.switch_arms.len()
            || edge_uses.iter().any(|&uses| uses != 1)
        {
            return Err(ProcError::new("procedural arenas contain unowned records"));
        }
        drop(edge_uses);
        self.validate_reachability()
    }

    /// Deterministic upper bound for temporary arenas used by
    /// [`Self::validate`].
    #[must_use]
    pub fn validation_memory_bytes(&self) -> usize {
        let edges = opto_core::resident::slice_bytes::<u8>(self.edges.len());
        let reachability = opto_core::resident::slice_bytes::<u8>(self.blocks.len())
            .saturating_add(opto_core::resident::slice_bytes::<BlockId>(
                self.blocks.len(),
            ));
        edges.max(reachability)
    }

    fn validate_terminator(
        &self,
        block: BlockId,
        terminator: &Terminator,
        edge_uses: &mut [u8],
        next_arm: &mut usize,
    ) -> Result<(), ProcError> {
        let mut use_edge = |edge: EdgeId| -> Result<(), ProcError> {
            let record = self
                .edge(edge)
                .ok_or_else(|| ProcError::new(format!("unknown procedural edge {edge:?}")))?;
            if record.from != block {
                return Err(ProcError::new(format!(
                    "edge {edge:?} has the wrong source block"
                )));
            }
            let target = self
                .block(record.target)
                .ok_or_else(|| ProcError::new(format!("edge {edge:?} has an unknown target")))?;
            if target.procedure != self.blocks[block.index()].procedure {
                return Err(ProcError::new(format!(
                    "edge {edge:?} crosses procedure boundaries"
                )));
            }
            let uses = &mut edge_uses[edge.index()];
            *uses = uses
                .checked_add(1)
                .ok_or_else(|| ProcError::new("procedural edge use count overflow"))?;
            Ok(())
        };

        match terminator.kind {
            TerminatorKind::Return => Ok(()),
            TerminatorKind::Jump { edge } => use_edge(edge),
            TerminatorKind::Branch {
                then_edge,
                else_edge,
                ..
            } => {
                use_edge(then_edge)?;
                use_edge(else_edge)
            }
            TerminatorKind::Switch { arms, default, .. } => {
                if arms.is_empty() || arms.start as usize != *next_arm {
                    return Err(ProcError::new(
                        "switch must own a non-empty contiguous arm range",
                    ));
                }
                *next_arm = checked_end(arms, self.switch_arms.len(), "switch arm")?;
                for arm_index in arms.indices() {
                    use_edge(self.switch_arms[arm_index].edge)?;
                }
                use_edge(default)
            }
        }
    }

    fn validate_reachability(&self) -> Result<(), ProcError> {
        let mut reached = vec![0u8; self.blocks.len()];
        let mut pending = Vec::with_capacity(self.blocks.len());
        for procedure in &self.procedures {
            pending.push(procedure.entry);
            while let Some(block) = pending.pop() {
                if std::mem::replace(&mut reached[block.index()], 1) != 0 {
                    continue;
                }
                let terminator = &self.blocks[block.index()].terminator.kind;
                match terminator {
                    TerminatorKind::Return => {}
                    TerminatorKind::Jump { edge } => pending.push(self.edges[edge.index()].target),
                    TerminatorKind::Branch {
                        then_edge,
                        else_edge,
                        ..
                    } => {
                        pending.push(self.edges[else_edge.index()].target);
                        pending.push(self.edges[then_edge.index()].target);
                    }
                    TerminatorKind::Switch { arms, default, .. } => {
                        pending.push(self.edges[default.index()].target);
                        pending.extend(
                            arms.indices().rev().map(|index| {
                                self.edges[self.switch_arms[index].edge.index()].target
                            }),
                        );
                    }
                }
            }
            if procedure.blocks.indices().any(|index| reached[index] == 0) {
                return Err(ProcError::new(
                    "procedure contains a block unreachable from its entry",
                ));
            }
        }
        Ok(())
    }
}

fn checked_end<I>(range: ArenaRange<I>, arena_len: usize, kind: &str) -> Result<usize, ProcError> {
    let end = (range.start as usize)
        .checked_add(range.len())
        .ok_or_else(|| ProcError::new(format!("{kind} range overflows")))?;
    if end > arena_len {
        return Err(ProcError::new(format!("{kind} range exceeds its arena")));
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(index: usize) -> ValueId {
        ValueId::from_index(index).unwrap()
    }

    fn signal(index: usize) -> SignalId {
        SignalId::from_index(index).unwrap()
    }

    #[test]
    fn seal_flattens_effects_in_source_order() {
        let mut builder = ProcBuilder::new();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let entry = builder.add_block(procedure, SourceSpan::default()).unwrap();
        let exit = builder.add_block(procedure, SourceSpan::default()).unwrap();
        builder
            .assign(
                entry,
                AssignmentMode::Blocking,
                ProcTarget::signal(signal(0)),
                value(0),
                SourceSpan::default(),
            )
            .unwrap();
        builder
            .assign(
                entry,
                AssignmentMode::Nonblocking,
                ProcTarget::signal(signal(1)),
                value(1),
                SourceSpan::default(),
            )
            .unwrap();
        builder
            .terminate_jump(entry, exit, SourceSpan::default())
            .unwrap();
        builder
            .terminate_return(exit, SourceSpan::default())
            .unwrap();

        let module = builder.seal().unwrap();
        let effects = module.block_effects(entry).unwrap().collect::<Vec<_>>();
        assert_eq!(effects[0].0.index(), 0);
        assert_eq!(effects[1].0.index(), 1);
        let TerminatorKind::Jump { edge } = module.block(entry).unwrap().terminator.kind else {
            panic!("entry must jump");
        };
        assert_eq!(module.edge(edge).unwrap().from, entry);
        assert_eq!(module.edge(edge).unwrap().target, exit);
    }

    #[test]
    fn ids_and_ranges_are_compact() {
        assert_eq!(std::mem::size_of::<BlockId>(), 4);
        assert_eq!(std::mem::size_of::<EffectId>(), 4);
        assert_eq!(std::mem::size_of::<ArenaRange<BlockId>>(), 8);
    }

    #[test]
    fn explicit_entry_does_not_reorder_source_blocks() {
        let mut builder = ProcBuilder::new();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let exit = builder.add_block(procedure, SourceSpan::default()).unwrap();
        let entry = builder.add_block(procedure, SourceSpan::default()).unwrap();
        builder.set_entry(procedure, entry).unwrap();
        builder
            .terminate_return(exit, SourceSpan::default())
            .unwrap();
        builder
            .terminate_jump(entry, exit, SourceSpan::default())
            .unwrap();

        let module = builder.seal().unwrap();
        assert_eq!(module.procedure(procedure).unwrap().entry, entry);
        assert_eq!(
            module
                .procedure_blocks(procedure)
                .unwrap()
                .collect::<Vec<_>>(),
            [exit, entry]
        );
    }

    #[test]
    fn seal_rejects_cross_procedure_edges_and_unreachable_blocks() {
        let mut builder = ProcBuilder::new();
        let first = builder
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let first_entry = builder.add_block(first, SourceSpan::default()).unwrap();
        let dead = builder.add_block(first, SourceSpan::default()).unwrap();
        builder
            .terminate_return(first_entry, SourceSpan::default())
            .unwrap();
        builder
            .terminate_return(dead, SourceSpan::default())
            .unwrap();
        assert!(
            builder
                .seal()
                .unwrap_err()
                .to_string()
                .contains("unreachable")
        );

        let mut builder = ProcBuilder::new();
        let first = builder
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let first_entry = builder.add_block(first, SourceSpan::default()).unwrap();
        let second = builder
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let second_entry = builder.add_block(second, SourceSpan::default()).unwrap();
        builder
            .terminate_jump(first_entry, second_entry, SourceSpan::default())
            .unwrap();
        builder
            .terminate_return(second_entry, SourceSpan::default())
            .unwrap();
        assert!(
            builder
                .seal()
                .unwrap_err()
                .to_string()
                .contains("crosses procedure")
        );
    }

    #[test]
    fn switch_ids_follow_block_and_arm_source_order() {
        let mut builder = ProcBuilder::new();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, SourceSpan::default())
            .unwrap();
        let entry = builder.add_block(procedure, SourceSpan::default()).unwrap();
        let left = builder.add_block(procedure, SourceSpan::default()).unwrap();
        let right = builder.add_block(procedure, SourceSpan::default()).unwrap();
        builder
            .terminate_switch(
                entry,
                value(0),
                [
                    SwitchArmSpec {
                        pattern: value(1),
                        target: left,
                        source: SourceSpan::default(),
                    },
                    SwitchArmSpec {
                        pattern: value(2),
                        target: right,
                        source: SourceSpan::default(),
                    },
                ],
                right,
                SourceSpan::default(),
            )
            .unwrap();
        builder
            .terminate_return(left, SourceSpan::default())
            .unwrap();
        builder
            .terminate_return(right, SourceSpan::default())
            .unwrap();

        let module = builder.seal().unwrap();
        let arms = module.switch_arms(entry).unwrap().collect::<Vec<_>>();
        assert_eq!(arms[0].0.index(), 0);
        assert_eq!(arms[1].0.index(), 1);
        assert_eq!(module.edges()[arms[0].1.edge.index()].target, left);
        assert_eq!(module.edges()[arms[1].1.edge.index()].target, right);
    }
}
