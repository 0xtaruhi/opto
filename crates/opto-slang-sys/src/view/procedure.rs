// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::SlangModule;
use super::convert::{has_flag, map_edge, map_loop_form, map_procedure_kind, unknown_enum};
use super::expression::{SlangExpression, SlangSignalRef, SlangSourceSpan};
use crate::bridge::read_invariant;
use crate::ffi;
use crate::{SlangAssignmentMode, SlangEdge, SlangError, SlangLoopForm, SlangProcedureKind};
use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Dense basic-block identity scoped to one [`SlangProcedure`].
pub struct SlangBlockId(u32);

impl SlangBlockId {
    #[must_use]
    /// Returns the zero-based block index.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
/// Dense loop-region identity scoped to one [`SlangProcedure`].
pub struct SlangLoopRegionId(u32);

impl SlangLoopRegionId {
    #[must_use]
    /// Returns the zero-based region index.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one elaborated procedure and its control-flow graph.
pub struct SlangProcedure<'a> {
    view: ffi::ProcedureView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangProcedure<'a> {
    pub(super) fn from_module(module: SlangModule<'_>, index: usize) -> Self {
        // SAFETY: `index` is bounded by the procedure count of this leased module.
        let view = unsafe {
            read_invariant("procedure", |view| {
                ffi::opto_slang_procedure_view(module.design.as_ptr(), module.index, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the classified procedure kind.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native procedure tag.
    pub fn kind(self) -> Result<SlangProcedureKind, SlangError> {
        map_procedure_kind(self.view.kind)
    }

    /// Returns the procedure's source location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }

    #[must_use]
    /// Returns the procedure entry block.
    pub fn entry(self) -> SlangBlockId {
        SlangBlockId(self.view.entry_block)
    }

    /// Iterates over sensitivity events in source order.
    #[must_use]
    pub fn events(self) -> impl ExactSizeIterator<Item = SlangSensitivityEvent<'a>> {
        (0..self.view.event_count)
            .map(move |index| SlangSensitivityEvent::new(self.view.identity, index))
    }

    /// Iterates over all basic blocks in dense ID order.
    #[must_use]
    pub fn blocks(self) -> impl ExactSizeIterator<Item = SlangBlock<'a>> {
        (0..self.view.block_count).map(move |index| SlangBlock::new(self.view.identity, index))
    }

    /// Iterates over canonical loop regions in parent-before-child order.
    #[must_use]
    pub fn loop_regions(self) -> impl ExactSizeIterator<Item = SlangLoopRegion<'a>> {
        (0..self.view.loop_region_count)
            .map(move |index| SlangLoopRegion::new(self.view.identity, index))
    }

    /// Looks up a basic block by its procedure-local ID.
    #[must_use]
    pub fn block(self, id: SlangBlockId) -> Option<SlangBlock<'a>> {
        (id.index() < self.view.block_count)
            .then(|| SlangBlock::new(self.view.identity, id.index()))
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed canonical natural-loop metadata; CFG edges remain authoritative.
pub struct SlangLoopRegion<'a> {
    view: ffi::LoopRegionView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangLoopRegion<'a> {
    fn new(procedure: *const ffi::Procedure, index: usize) -> Self {
        // SAFETY: the procedure is live and `index` is bounded by its region count.
        let view = unsafe {
            read_invariant("procedural loop region", |view| {
                ffi::opto_slang_loop_region_view(procedure, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    #[must_use]
    /// Returns the canonical header block.
    pub fn header(self) -> SlangBlockId {
        SlangBlockId(self.view.header)
    }

    #[must_use]
    /// Returns the body-entry block.
    pub fn body(self) -> SlangBlockId {
        SlangBlockId(self.view.body)
    }

    #[must_use]
    /// Returns the continue/latch block.
    pub fn latch(self) -> SlangBlockId {
        SlangBlockId(self.view.latch)
    }

    #[must_use]
    /// Returns the loop exit block.
    pub fn exit(self) -> SlangBlockId {
        SlangBlockId(self.view.exit)
    }

    /// Returns condition placement for this source loop.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native tag.
    pub fn form(self) -> Result<SlangLoopForm, SlangError> {
        map_loop_form(self.view.form)
    }

    #[must_use]
    /// Returns the lexically enclosing region, when present.
    pub fn parent(self) -> Option<SlangLoopRegionId> {
        has_flag(self.view.has_parent).then_some(SlangLoopRegionId(self.view.parent))
    }

    /// Returns the source loop location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed event-control item in a procedure sensitivity list.
pub struct SlangSensitivityEvent<'a> {
    view: ffi::EventView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangSensitivityEvent<'a> {
    fn new(procedure: *const ffi::Procedure, index: usize) -> Self {
        // SAFETY: the procedure is live and `index` is bounded by its event count.
        let view = unsafe {
            read_invariant("sensitivity event", |view| {
                ffi::opto_slang_event_view(procedure, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the scalar expression observed by the event control.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native event expression is absent.
    pub fn expression(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.expression, "sensitivity event expression")
    }

    /// Returns the event expression as a direct signal selection.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] when the event expression is not
    /// representable as a static signal reference.
    pub fn signal(self) -> Result<SlangSignalRef<'a>, SlangError> {
        self.expression()?.signal_ref()
    }

    /// Returns the event-local `iff` qualifier after native canonicalization.
    ///
    /// A missing value denotes an unqualified event. Compile-time true qualifiers
    /// are removed and compile-time false events are omitted by the native adapter.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if a present native qualifier is malformed.
    pub fn qualifier(self) -> Result<Option<SlangExpression<'a>>, SlangError> {
        if self.view.qualifier.is_null() {
            Ok(None)
        } else {
            SlangExpression::from_raw(self.view.qualifier, "sensitivity event qualifier").map(Some)
        }
    }

    /// Returns the active event edge.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown native edge tag.
    pub fn edge(self) -> Result<SlangEdge, SlangError> {
        map_edge(self.view.edge)
    }

    /// Returns the event's source location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed basic block containing ordered effects and one terminator.
pub struct SlangBlock<'a> {
    procedure: *const ffi::Procedure,
    index: usize,
    view: ffi::BlockView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangBlock<'a> {
    fn new(procedure: *const ffi::Procedure, index: usize) -> Self {
        // SAFETY: the procedure is live and `index` is bounded by its block count.
        let view = unsafe {
            read_invariant("procedural block", |view| {
                ffi::opto_slang_block_view(procedure, index, view)
            })
        };
        Self {
            procedure,
            index,
            view,
            _lifetime: PhantomData,
        }
    }

    #[must_use]
    /// Returns this block's procedure-local identity.
    ///
    /// # Panics
    ///
    /// Panics only if the native bridge exposes more blocks than its 32-bit ID
    /// field can address.
    pub fn id(self) -> SlangBlockId {
        SlangBlockId(u32::try_from(self.index).expect("native block indices fit in u32"))
    }

    /// Returns the block's source location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }

    /// Iterates over assignment effects in execution order.
    #[must_use]
    pub fn effects(self) -> impl ExactSizeIterator<Item = SlangEffect<'a>> {
        (0..self.view.effect_count)
            .map(move |effect| SlangEffect::new(self.procedure, self.index, effect))
    }

    #[must_use]
    /// Returns the control-flow terminator.
    pub fn terminator(self) -> SlangTerminator<'a> {
        SlangTerminator::new(self.procedure, self.index)
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed procedural assignment effect.
pub struct SlangEffect<'a> {
    view: ffi::EffectView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangEffect<'a> {
    fn new(procedure: *const ffi::Procedure, block: usize, index: usize) -> Self {
        // SAFETY: both indices are bounded by their live native arenas.
        let view = unsafe {
            read_invariant("procedural effect", |view| {
                ffi::opto_slang_effect_view(procedure, block, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the assignment destination.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native left-hand expression is null.
    pub fn lhs(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.lhs, "procedural assignment lhs")
    }

    /// Returns the assigned expression.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native right-hand expression is null.
    pub fn rhs(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.rhs, "procedural assignment rhs")
    }

    /// Returns blocking or nonblocking assignment semantics.
    #[must_use]
    pub fn mode(self) -> SlangAssignmentMode {
        if has_flag(self.view.blocking) {
            SlangAssignmentMode::Blocking
        } else {
            SlangAssignmentMode::Nonblocking
        }
    }

    /// Returns the assignment's source location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed control-flow terminator for one basic block.
pub struct SlangTerminator<'a> {
    procedure: *const ffi::Procedure,
    block: usize,
    view: ffi::TerminatorView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangTerminator<'a> {
    fn new(procedure: *const ffi::Procedure, block: usize) -> Self {
        // SAFETY: the block belongs to the live procedure and always has one terminator.
        let view = unsafe {
            read_invariant("procedural terminator", |view| {
                ffi::opto_slang_terminator_view(procedure, block, view)
            })
        };
        Self {
            procedure,
            block,
            view,
            _lifetime: PhantomData,
        }
    }

    /// Decodes the terminator and its successor edges.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] for an unknown terminator tag,
    /// missing condition/selector, or malformed successor edge.
    pub fn kind(self) -> Result<SlangTerminatorKind<'a>, SlangError> {
        match self.view.kind {
            ffi::TERMINATOR_RETURN => Ok(SlangTerminatorKind::Return),
            ffi::TERMINATOR_JUMP => Ok(SlangTerminatorKind::Jump(edge(self.view.jump_edge)?)),
            ffi::TERMINATOR_BRANCH => Ok(SlangTerminatorKind::Branch {
                condition: SlangExpression::from_raw(self.view.condition, "branch condition")?,
                then_edge: edge(self.view.then_edge)?,
                else_edge: edge(self.view.else_edge)?,
            }),
            ffi::TERMINATOR_SWITCH => Ok(SlangTerminatorKind::Switch {
                selector: SlangExpression::from_raw(self.view.selector, "switch selector")?,
                arms: SlangSwitchArms { terminator: self },
                default: edge(self.view.default_edge)?,
            }),
            raw => Err(unknown_enum("terminator kind", raw)),
        }
    }

    /// Returns the terminator's source location.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native source view is malformed.
    pub fn source(self) -> Result<SlangSourceSpan<'a>, SlangError> {
        SlangSourceSpan::from_view(self.view.source)
    }
}

#[derive(Debug, Clone, Copy)]
/// Decoded control-flow operation at the end of a basic block.
pub enum SlangTerminatorKind<'a> {
    /// Returns from the procedure.
    Return,
    /// Unconditional jump.
    Jump(SlangEdgeTarget<'a>),
    /// Conditional two-way branch.
    Branch {
        /// Boolean branch condition.
        condition: SlangExpression<'a>,
        /// Edge selected when true.
        then_edge: SlangEdgeTarget<'a>,
        /// Edge selected when false.
        else_edge: SlangEdgeTarget<'a>,
    },
    /// Multi-way value switch.
    Switch {
        /// Value compared against each explicit arm.
        selector: SlangExpression<'a>,
        /// Explicit pattern arms.
        arms: SlangSwitchArms<'a>,
        /// Successor selected when no arm matches.
        default: SlangEdgeTarget<'a>,
    },
}

#[derive(Debug, Clone, Copy)]
/// Borrowed sequence of explicit switch arms.
pub struct SlangSwitchArms<'a> {
    terminator: SlangTerminator<'a>,
}

impl<'a> SlangSwitchArms<'a> {
    /// Iterates over switch arms in source order.
    #[must_use]
    pub fn iter(self) -> impl ExactSizeIterator<Item = SlangSwitchArm<'a>> {
        (0..self.terminator.view.arm_count).map(move |index| {
            SlangSwitchArm::new(self.terminator.procedure, self.terminator.block, index)
        })
    }

    #[must_use]
    /// Returns the number of explicit switch arms.
    pub fn len(self) -> usize {
        self.terminator.view.arm_count
    }

    /// Returns `true` when the switch has no explicit arms.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed pattern and successor for one switch arm.
pub struct SlangSwitchArm<'a> {
    view: ffi::SwitchArmView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangSwitchArm<'a> {
    fn new(procedure: *const ffi::Procedure, block: usize, index: usize) -> Self {
        // SAFETY: all indices originate from the live switch view.
        let view = unsafe {
            read_invariant("switch arm", |view| {
                ffi::opto_slang_switch_arm_view(procedure, block, index, view)
            })
        };
        Self {
            view,
            _lifetime: PhantomData,
        }
    }

    /// Returns the arm match pattern.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native pattern expression is null.
    pub fn pattern(self) -> Result<SlangExpression<'a>, SlangError> {
        SlangExpression::from_raw(self.view.pattern, "switch arm pattern")
    }

    /// Returns the arm successor edge.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native edge target or source is malformed.
    pub fn edge(self) -> Result<SlangEdgeTarget<'a>, SlangError> {
        edge(self.view.edge)
    }
}

#[derive(Debug, Clone, Copy)]
/// Successor block and source location of a control-flow edge.
pub struct SlangEdgeTarget<'a> {
    /// Destination basic block.
    pub block: SlangBlockId,
    /// Source location of the branch syntax.
    pub source: SlangSourceSpan<'a>,
}

fn edge<'a>(view: ffi::EdgeTargetView) -> Result<SlangEdgeTarget<'a>, SlangError> {
    Ok(SlangEdgeTarget {
        block: SlangBlockId(view.block),
        source: SlangSourceSpan::from_view(view.source)?,
    })
}
