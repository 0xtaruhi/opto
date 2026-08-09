// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Append-only construction and deterministic sealing of procedural IR.
//!
//! Drafts use convenient nested vectors while frontends emit control flow.
//! Sealing validates completeness and flattens them into contiguous arenas
//! without reordering source effects or blocks.

use super::{
    ArenaRange, AssignmentMode, Block, BlockId, EdgeId, EdgeRecord, Effect, ProcError, ProcModule,
    ProcTarget, Procedure, ProcedureId, ProcedureKind, Sensitivity, SensitivityEvent, SourceSpan,
    SwitchArm, Terminator, TerminatorKind, ValueId,
};

#[derive(Debug)]
struct ProcedureDraft {
    kind: ProcedureKind,
    events: Vec<SensitivityEvent>,
    source: SourceSpan,
    block_start: usize,
    block_count: usize,
    entry: Option<BlockId>,
}

#[derive(Debug)]
struct BlockDraft {
    procedure: ProcedureId,
    effects: Vec<Effect>,
    terminator: Option<TerminatorDraft>,
    source: SourceSpan,
}

#[derive(Debug)]
enum TerminatorDraft {
    Return(SourceSpan),
    Jump {
        target: BlockId,
        source: SourceSpan,
    },
    Branch {
        condition: ValueId,
        then_target: BlockId,
        else_target: BlockId,
        source: SourceSpan,
    },
    Switch {
        selector: ValueId,
        arms: Vec<SwitchArmSpec>,
        default: BlockId,
        source: SourceSpan,
    },
}

impl TerminatorDraft {
    fn arena_counts(&self) -> Option<(usize, usize)> {
        match self {
            Self::Return(_) => Some((0, 0)),
            Self::Jump { .. } => Some((1, 0)),
            Self::Branch { .. } => Some((2, 0)),
            Self::Switch { arms, .. } => arms.len().checked_add(1).map(|edges| (edges, arms.len())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Unsealed switch-arm input accepted by [`ProcBuilder`].
pub struct SwitchArmSpec {
    /// Pattern compared with the switch selector.
    pub pattern: ValueId,
    /// Block entered when the pattern matches.
    pub target: BlockId,
    /// Source span of the arm label.
    pub source: SourceSpan,
}

/// Transient, append-oriented frontend builder.
#[derive(Debug, Default)]
pub struct ProcBuilder {
    procedures: Vec<ProcedureDraft>,
    blocks: Vec<BlockDraft>,
}

impl ProcBuilder {
    /// Creates an empty procedural IR builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts an implicitly sensitive combinational or latch-like procedure.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for a flip-flop kind or when the procedure arena
    /// has exhausted its compact ID space.
    pub fn add_combinational_procedure(
        &mut self,
        kind: ProcedureKind,
        source: SourceSpan,
    ) -> Result<ProcedureId, ProcError> {
        if kind == ProcedureKind::FlipFlop {
            return Err(ProcError::new(
                "flip-flop procedure requires edge sensitivity",
            ));
        }
        self.push_procedure(kind, Vec::new(), source)
    }

    /// Starts a flip-flop procedure with explicit edge events.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when no edge event is supplied or the procedure
    /// arena has exhausted its compact ID space.
    pub fn add_clocked_procedure(
        &mut self,
        events: impl IntoIterator<Item = SensitivityEvent>,
        source: SourceSpan,
    ) -> Result<ProcedureId, ProcError> {
        let events = events.into_iter().collect::<Vec<_>>();
        if events.is_empty() {
            return Err(ProcError::new(
                "flip-flop procedure requires at least one edge event",
            ));
        }
        self.push_procedure(ProcedureKind::FlipFlop, events, source)
    }

    fn push_procedure(
        &mut self,
        kind: ProcedureKind,
        events: Vec<SensitivityEvent>,
        source: SourceSpan,
    ) -> Result<ProcedureId, ProcError> {
        let id = ProcedureId::from_index(self.procedures.len())?;
        self.procedures.push(ProcedureDraft {
            kind,
            events,
            source,
            block_start: self.blocks.len(),
            block_count: 0,
            entry: None,
        });
        Ok(id)
    }

    /// Appends a source-order block to the current procedure.
    ///
    /// Procedures must be completed contiguously; appending to an earlier
    /// procedure is rejected so sealing can use one range per owner.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] if `procedure` is not the current procedure, the
    /// block arena is full, or its compact block count overflows.
    pub fn add_block(
        &mut self,
        procedure: ProcedureId,
        source: SourceSpan,
    ) -> Result<BlockId, ProcError> {
        if procedure.index() + 1 != self.procedures.len() {
            return Err(ProcError::new(
                "blocks must be appended while their procedure is current",
            ));
        }
        let id = BlockId::from_index(self.blocks.len())?;
        self.blocks.push(BlockDraft {
            procedure,
            effects: Vec::new(),
            terminator: None,
            source,
        });
        self.procedures[procedure.index()].block_count = self.procedures[procedure.index()]
            .block_count
            .checked_add(1)
            .ok_or_else(|| ProcError::new("procedure block count exceeds address capacity"))?;
        self.procedures[procedure.index()].entry.get_or_insert(id);
        Ok(id)
    }

    /// Selects a procedure entry without changing source-order block storage.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when either ID is unknown or `entry` belongs to a
    /// different procedure.
    pub fn set_entry(&mut self, procedure: ProcedureId, entry: BlockId) -> Result<(), ProcError> {
        let block = self
            .blocks
            .get(entry.index())
            .ok_or_else(|| ProcError::new(format!("unknown procedural block {entry:?}")))?;
        if block.procedure != procedure {
            return Err(ProcError::new(
                "procedure entry must name a block owned by that procedure",
            ));
        }
        self.procedures
            .get_mut(procedure.index())
            .ok_or_else(|| ProcError::new(format!("unknown procedure {procedure:?}")))?
            .entry = Some(entry);
        Ok(())
    }

    /// Appends a procedural assignment to `block`.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when `block` is not present in this builder.
    pub fn assign(
        &mut self,
        block: BlockId,
        mode: AssignmentMode,
        target: ProcTarget,
        value: ValueId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.block_mut(block)?.effects.push(Effect {
            mode,
            target,
            value,
            source,
        });
        Ok(())
    }

    /// Terminates `block` by returning from the procedure.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when `block` is unknown or already terminated.
    pub fn terminate_return(
        &mut self,
        block: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.terminate(block, TerminatorDraft::Return(source))
    }

    /// Terminates `block` with an unconditional transfer to `target`.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when `block` is unknown or already terminated.
    /// Target ownership is validated when the builder is sealed.
    pub fn terminate_jump(
        &mut self,
        block: BlockId,
        target: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.terminate(block, TerminatorDraft::Jump { target, source })
    }

    /// Terminates `block` with a Boolean branch.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when `block` is unknown or already terminated.
    /// Conditions and targets are validated when the builder is sealed.
    pub fn terminate_branch(
        &mut self,
        block: BlockId,
        condition: ValueId,
        then_target: BlockId,
        else_target: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.terminate(
            block,
            TerminatorDraft::Branch {
                condition,
                then_target,
                else_target,
                source,
            },
        )
    }

    /// Terminates `block` with an ordered multi-way switch.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an empty arm list, an unknown block, or a block
    /// that already has a terminator.
    pub fn terminate_switch(
        &mut self,
        block: BlockId,
        selector: ValueId,
        arms: impl IntoIterator<Item = SwitchArmSpec>,
        default: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        let arms = arms.into_iter().collect::<Vec<_>>();
        if arms.is_empty() {
            return Err(ProcError::new("switch requires at least one arm"));
        }
        self.terminate(
            block,
            TerminatorDraft::Switch {
                selector,
                arms,
                default,
                source,
            },
        )
    }

    fn terminate(&mut self, block: BlockId, terminator: TerminatorDraft) -> Result<(), ProcError> {
        let slot = &mut self.block_mut(block)?.terminator;
        if slot.is_some() {
            return Err(ProcError::new(format!(
                "procedural block {block:?} is already terminated"
            )));
        }
        *slot = Some(terminator);
        Ok(())
    }

    fn block_mut(&mut self, block: BlockId) -> Result<&mut BlockDraft, ProcError> {
        self.blocks
            .get_mut(block.index())
            .ok_or_else(|| ProcError::new(format!("unknown procedural block {block:?}")))
    }

    /// Validates and compacts all drafts into immutable procedural arenas.
    ///
    /// Sealing preserves procedure, block, effect, event, and switch-arm
    /// insertion order and rejects incomplete or invalid control flow.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for capacity overflow, missing terminators, invalid
    /// cross-procedure edges, malformed switch arms, or unreachable blocks.
    pub fn seal(self) -> Result<ProcModule, ProcError> {
        let event_count = self
            .procedures
            .iter()
            .try_fold(0usize, |total, procedure| {
                total.checked_add(procedure.events.len())
            })
            .ok_or_else(|| ProcError::new("sensitivity event count exceeds address capacity"))?;
        let mut events = Vec::with_capacity(event_count);
        let mut procedures = Vec::with_capacity(self.procedures.len());
        for procedure in self.procedures {
            let event_range =
                ArenaRange::new(events.len(), procedure.events.len(), "sensitivity event")?;
            events.extend(procedure.events);
            let blocks = ArenaRange::new(
                procedure.block_start,
                procedure.block_count,
                "procedure block",
            )?;
            let entry = procedure
                .entry
                .ok_or_else(|| ProcError::new("procedure has no entry block"))?;
            procedures.push(Procedure {
                kind: procedure.kind,
                sensitivity: if event_range.is_empty() {
                    Sensitivity::Implicit
                } else {
                    Sensitivity::Edges(event_range)
                },
                entry,
                source: procedure.source,
                blocks,
            });
        }

        let (effect_count, edge_count, arm_count) = self
            .blocks
            .iter()
            .try_fold((0usize, 0usize, 0usize), |(effects, edges, arms), block| {
                let (block_edges, block_arms) = block
                    .terminator
                    .as_ref()
                    .and_then(TerminatorDraft::arena_counts)
                    .unwrap_or_default();
                Some((
                    effects.checked_add(block.effects.len())?,
                    edges.checked_add(block_edges)?,
                    arms.checked_add(block_arms)?,
                ))
            })
            .ok_or_else(|| ProcError::new("procedural arena size overflow"))?;
        let mut effects = Vec::with_capacity(effect_count);
        let mut blocks = Vec::with_capacity(self.blocks.len());
        let mut edges = Vec::with_capacity(edge_count);
        let mut switch_arms = Vec::with_capacity(arm_count);
        for (index, block) in self.blocks.into_iter().enumerate() {
            let id = BlockId::from_index(index)?;
            let effect_range = ArenaRange::new(effects.len(), block.effects.len(), "effect")?;
            effects.extend(block.effects);
            let terminator = block.terminator.ok_or_else(|| {
                ProcError::new(format!("procedural block {id:?} has no terminator"))
            })?;
            let terminator = materialize_terminator(id, terminator, &mut edges, &mut switch_arms)?;
            blocks.push(Block {
                procedure: block.procedure,
                terminator,
                source: block.source,
                effects: effect_range,
            });
        }
        let module = ProcModule {
            procedures: procedures.into_boxed_slice(),
            blocks: blocks.into_boxed_slice(),
            effects: effects.into_boxed_slice(),
            edges: edges.into_boxed_slice(),
            switch_arms: switch_arms.into_boxed_slice(),
            events: events.into_boxed_slice(),
        };
        module.validate()?;
        Ok(module)
    }
}

fn materialize_terminator(
    block: BlockId,
    draft: TerminatorDraft,
    edges: &mut Vec<EdgeRecord>,
    switch_arms: &mut Vec<SwitchArm>,
) -> Result<Terminator, ProcError> {
    let push_edge = |target: BlockId,
                     source: SourceSpan,
                     edges: &mut Vec<EdgeRecord>|
     -> Result<EdgeId, ProcError> {
        let id = EdgeId::from_index(edges.len())?;
        edges.push(EdgeRecord {
            from: block,
            target,
            source_span: source,
        });
        Ok(id)
    };
    let (kind, source) = match draft {
        TerminatorDraft::Return(source) => (TerminatorKind::Return, source),
        TerminatorDraft::Jump { target, source } => {
            let edge = push_edge(target, source.clone(), edges)?;
            (TerminatorKind::Jump { edge }, source)
        }
        TerminatorDraft::Branch {
            condition,
            then_target,
            else_target,
            source,
        } => {
            let then_edge = push_edge(then_target, source.clone(), edges)?;
            let else_edge = push_edge(else_target, source.clone(), edges)?;
            (
                TerminatorKind::Branch {
                    condition,
                    then_edge,
                    else_edge,
                },
                source,
            )
        }
        TerminatorDraft::Switch {
            selector,
            arms,
            default,
            source,
        } => {
            let arm_start = switch_arms.len();
            for arm in arms {
                let edge = push_edge(arm.target, arm.source, edges)?;
                switch_arms.push(SwitchArm {
                    pattern: arm.pattern,
                    edge,
                });
            }
            let arms = ArenaRange::new(arm_start, switch_arms.len() - arm_start, "switch arm")?;
            let default = push_edge(default, source.clone(), edges)?;
            (
                TerminatorKind::Switch {
                    selector,
                    arms,
                    default,
                },
                source,
            )
        }
    };
    Ok(Terminator { kind, source })
}
