// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Transient cyclic procedural IR with activation-scoped values.
//!
//! This phase owns source-level procedural expressions and locals before loop
//! elimination. Its CFG uses the same construction core and typed block IDs as
//! the final [`super::ProcModule`], but ordinary backedges are legal here.
//! Only [`TransientProcModule::materialize_acyclic`] can cross into the final
//! procedural IR, and that boundary rejects residual cycles or locals.

use super::builder::{ProcGraphBuilder, TerminatorDraft};
use super::{
    ArenaRange, AssignmentMode, BlockId, LoopRegionId, ProcBuilder, ProcError, ProcExprId,
    ProcLocalId, ProcModule, ProcTarget, ProcedureId, ProcedureKind, Sensitivity, SensitivityEvent,
    SwitchArmSpec, TargetSelect, TransientEffectId,
};
use crate::word::{
    BinaryOp, BitRange, CastKind, Enable, LogicStateKind, MemoryId, MemoryReadPort,
    MemoryReadTiming, ReadDuringWrite, SignalId, SourceSpan, UnaryOp, ValueId, WordModule,
    WordType,
};
use crate::{BitVal, ConstBits};
use std::num::NonZeroU32;

mod exact;
mod locals;
mod loops;
mod promotion;

pub use loops::{LoopAnalysisLimits, LoopBoundednessAnalysis, LoopProof, LoopProofMethod};

#[derive(Debug, Clone, PartialEq, Eq)]
/// One automatic value scoped to a single elaborated procedure activation.
///
/// A frontend allocates a fresh ID for every inlined activation. Static-lifetime
/// source variables are persistent hardware and therefore use a module signal
/// instead of this arena.
pub struct ProcLocal {
    /// Module-unique transient name used only until procedure normalization.
    pub name: Box<str>,
    /// Fixed value type of the local.
    pub ty: WordType,
    /// Source declaration location.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Owned procedural expression evaluated at the control-flow use site.
///
/// In particular, [`ProcExprKind::LocalRead`] is a place read, not an SSA value;
/// it must not be commoned across effects or terminators before local-state
/// analysis establishes an equivalent version.
pub struct ProcExpr {
    /// Result type.
    pub ty: WordType,
    /// Expression operation.
    pub kind: ProcExprKind,
    /// Source expression location.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Operation in the owned transient expression arena.
pub enum ProcExprKind {
    /// Existing module-level expression with no transient-local operands.
    ModuleValue(ValueId),
    /// Owned constant.
    Constant(ConstBits),
    /// Reads the current blocking value of an automatic local at this use site.
    LocalRead(ProcLocalId),
    /// Reads an asynchronous memory element through a procedural address.
    MemoryRead {
        /// Source memory.
        memory: MemoryId,
        /// Element address.
        address: ProcExprId,
        /// Optional element selection.
        select: TransientTargetSelect,
    },
    /// Unary operation.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        arg: ProcExprId,
    },
    /// Binary operation.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        left: ProcExprId,
        /// Right operand.
        right: ProcExprId,
    },
    /// Two-way value selection.
    Mux {
        /// Scalar condition.
        condition: ProcExprId,
        /// Value selected when true.
        then_value: ProcExprId,
        /// Value selected when false.
        else_value: ProcExprId,
    },
    /// Conditionally enabled high-impedance contribution.
    TriState {
        /// Driven data.
        data: ProcExprId,
        /// Scalar enable.
        enable: ProcExprId,
        /// Whether one enables the driver.
        active_high: bool,
    },
    /// Most-significant-part-first concatenation.
    Concat(Box<[ProcExprId]>),
    /// Fixed-position bit extraction.
    Extract {
        /// Aggregate value.
        value: ProcExprId,
        /// Least-significant storage offset.
        lsb: u32,
        /// Selected width.
        width: NonZeroU32,
    },
    /// Runtime-positioned bit extraction.
    DynamicExtract {
        /// Aggregate value.
        value: ProcExprId,
        /// Unsigned runtime offset.
        offset: ProcExprId,
        /// Selected width.
        width: NonZeroU32,
    },
    /// Replaces a statically positioned range and returns the whole value.
    Insert {
        /// Original aggregate value.
        value: ProcExprId,
        /// Least-significant storage offset.
        lsb: u32,
        /// Replacement value; its width defines the selected range.
        replacement: ProcExprId,
    },
    /// Replaces a runtime-positioned range and returns the whole value.
    DynamicInsert {
        /// Original aggregate value.
        value: ProcExprId,
        /// Unsigned runtime offset.
        offset: ProcExprId,
        /// Replacement value; its width defines the selected range.
        replacement: ProcExprId,
    },
    /// Explicit width or signedness conversion.
    Cast {
        /// Conversion operation.
        kind: CastKind,
        /// Converted value.
        value: ProcExprId,
    },
}

impl ProcExprKind {
    fn for_each_operand(&self, mut visit: impl FnMut(ProcExprId)) {
        match self {
            Self::ModuleValue(_) | Self::Constant(_) | Self::LocalRead(_) => {}
            Self::MemoryRead {
                address, select, ..
            } => {
                visit(*address);
                if let TransientTargetSelect::Dynamic { offset, .. } = select {
                    visit(*offset);
                }
            }
            Self::Unary { arg, .. } => visit(*arg),
            Self::Binary { left, right, .. } => {
                visit(*left);
                visit(*right);
            }
            Self::Mux {
                condition,
                then_value,
                else_value,
            } => {
                visit(*condition);
                visit(*then_value);
                visit(*else_value);
            }
            Self::TriState { data, enable, .. } => {
                visit(*data);
                visit(*enable);
            }
            Self::Concat(parts) => {
                for &part in parts {
                    visit(part);
                }
            }
            Self::Extract { value, .. } | Self::Cast { value, .. } => visit(*value),
            Self::DynamicExtract { value, offset, .. } => {
                visit(*value);
                visit(*offset);
            }
            Self::Insert {
                value, replacement, ..
            } => {
                visit(*value);
                visit(*replacement);
            }
            Self::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                visit(*value);
                visit(*offset);
                visit(*replacement);
            }
        }
    }

    fn for_each_operand_mut(&mut self, mut visit: impl FnMut(&mut ProcExprId)) {
        match self {
            Self::ModuleValue(_) | Self::Constant(_) | Self::LocalRead(_) => {}
            Self::MemoryRead {
                address, select, ..
            } => {
                visit(address);
                if let TransientTargetSelect::Dynamic { offset, .. } = select {
                    visit(offset);
                }
            }
            Self::Unary { arg, .. } => visit(arg),
            Self::Binary { left, right, .. } => {
                visit(left);
                visit(right);
            }
            Self::Mux {
                condition,
                then_value,
                else_value,
            } => {
                visit(condition);
                visit(then_value);
                visit(else_value);
            }
            Self::TriState { data, enable, .. } => {
                visit(data);
                visit(enable);
            }
            Self::Concat(parts) => {
                for part in parts {
                    visit(part);
                }
            }
            Self::Extract { value, .. } | Self::Cast { value, .. } => visit(value),
            Self::DynamicExtract { value, offset, .. } => {
                visit(value);
                visit(offset);
            }
            Self::Insert {
                value, replacement, ..
            } => {
                visit(value);
                visit(replacement);
            }
            Self::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                visit(value);
                visit(offset);
                visit(replacement);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Whole, static, or runtime selection in a transient assignment place.
pub enum TransientTargetSelect {
    /// Complete value.
    Whole,
    /// Statically positioned inclusive range.
    Static(BitRange),
    /// Runtime-positioned fixed-width range.
    Dynamic {
        /// Unsigned runtime offset expression.
        offset: ProcExprId,
        /// Selected width.
        width: NonZeroU32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Assignment place in the transient procedural graph.
pub enum TransientTarget {
    /// Automatic local assignment.
    Local {
        /// Destination local.
        local: ProcLocalId,
        /// Selected local bits.
        select: TransientTargetSelect,
    },
    /// Persistent module signal assignment.
    Signal {
        /// Destination signal.
        signal: SignalId,
        /// Selected signal bits.
        select: TransientTargetSelect,
    },
    /// Addressed memory assignment.
    Memory {
        /// Destination memory.
        memory: MemoryId,
        /// Runtime element address.
        address: ProcExprId,
        /// Selected element bits.
        select: TransientTargetSelect,
    },
}

impl TransientTarget {
    /// Creates a whole module-signal target.
    #[must_use]
    pub const fn signal(signal: SignalId) -> Self {
        Self::Signal {
            signal,
            select: TransientTargetSelect::Whole,
        }
    }

    /// Creates a whole automatic-local target.
    #[must_use]
    pub const fn local(local: ProcLocalId) -> Self {
        Self::Local {
            local,
            select: TransientTargetSelect::Whole,
        }
    }

    /// Replaces the selected bits without changing the owner.
    #[must_use]
    pub const fn with_select(self, select: TransientTargetSelect) -> Self {
        match self {
            Self::Local { local, .. } => Self::Local { local, select },
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

#[derive(Debug, Clone, PartialEq, Eq)]
/// Source-ordered transient assignment effect.
pub struct TransientEffect {
    /// Blocking or nonblocking scheduling mode.
    pub mode: AssignmentMode,
    /// Destination place.
    pub target: TransientTarget,
    /// Value evaluated at this effect.
    pub value: ProcExprId,
    /// Source assignment location.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One ordered switch arm in transient control flow.
pub struct TransientSwitchArm {
    /// Match pattern.
    pub pattern: ProcExprId,
    /// Arm destination.
    pub target: BlockId,
    /// Source arm location.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Transient block terminator using ordinary CFG edges, including backedges.
pub enum TransientTerminatorKind {
    /// Completes the current procedure activation.
    Return,
    /// Unconditional transfer.
    Jump(BlockId),
    /// Conditional transfer.
    Branch {
        /// Scalar condition evaluated at the terminator.
        condition: ProcExprId,
        /// True destination.
        then_target: BlockId,
        /// False destination.
        else_target: BlockId,
    },
    /// Ordered multi-way selection.
    Switch {
        /// Selector evaluated at the terminator.
        selector: ProcExprId,
        /// Explicit arms in source order.
        arms: Box<[TransientSwitchArm]>,
        /// Default destination.
        default: BlockId,
    },
}

impl TransientTerminatorKind {
    fn for_each_target(&self, mut visit: impl FnMut(BlockId)) {
        match self {
            Self::Return => {}
            Self::Jump(target) => visit(*target),
            Self::Branch {
                then_target,
                else_target,
                ..
            } => {
                visit(*then_target);
                visit(*else_target);
            }
            Self::Switch { arms, default, .. } => {
                arms.iter().for_each(|arm| visit(arm.target));
                visit(*default);
            }
        }
    }

    fn for_each_expression(&self, mut visit: impl FnMut(ProcExprId)) {
        match self {
            Self::Return | Self::Jump(_) => {}
            Self::Branch { condition, .. } => visit(*condition),
            Self::Switch { selector, arms, .. } => {
                visit(*selector);
                arms.iter().for_each(|arm| visit(arm.pattern));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Complete transient terminator and source location.
pub struct TransientTerminator {
    /// Control operation.
    pub kind: TransientTerminatorKind,
    /// Source statement location.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Transient basic block with a flat source-ordered effect range.
pub struct TransientBlock {
    /// Owning procedure.
    pub procedure: ProcedureId,
    /// Required terminator.
    pub terminator: TransientTerminator,
    /// Source block location.
    pub source: SourceSpan,
    effects: ArenaRange<TransientEffectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TransientProcedure {
    kind: ProcedureKind,
    sensitivity: Sensitivity,
    entry: BlockId,
    source: SourceSpan,
    blocks: Box<[BlockId]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Source loop placement of its condition.
pub enum LoopForm {
    /// Header condition precedes the body (`for`, `while`, and `repeat`).
    PreTest,
    /// Latch condition follows the body (`do-while`).
    PostTest,
    /// No source condition (`forever`).
    Unconditional,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Validated source-provided natural-loop metadata.
///
/// The graph edges remain the sole control-flow semantics. This record avoids
/// rediscovering structured regions but is checked against ownership and the
/// actual latch-to-header backedge before analysis may consume it.
pub struct LoopRegion {
    /// Owning procedure.
    pub procedure: ProcedureId,
    /// Canonical header.
    pub header: BlockId,
    /// Canonical body entry.
    pub body: BlockId,
    /// Canonical continue/latch block.
    pub latch: BlockId,
    /// Canonical loop exit.
    pub exit: BlockId,
    /// Condition placement.
    pub form: LoopForm,
    /// Lexically enclosing loop, if any.
    pub parent: Option<LoopRegionId>,
    /// Source loop location.
    pub source: SourceSpan,
}

#[derive(Debug)]
/// Builder owning a shared CFG core plus transient local and expression arenas.
pub struct TransientProcBuilder {
    control: ProcGraphBuilder<ProcExprId, TransientTarget>,
    locals: Vec<ProcLocal>,
    expressions: Vec<ProcExpr>,
    loop_regions: Vec<LoopRegion>,
}

impl Default for TransientProcBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TransientProcBuilder {
    /// Creates an empty transient procedural module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            control: ProcGraphBuilder::new(),
            locals: Vec::new(),
            expressions: Vec::new(),
            loop_regions: Vec::new(),
        }
    }

    /// Allocates one automatic local.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when the compact local-ID space is exhausted.
    pub fn add_local(&mut self, local: ProcLocal) -> Result<ProcLocalId, ProcError> {
        let id = ProcLocalId::from_index(self.locals.len())?;
        self.locals.push(local);
        Ok(id)
    }

    /// Appends an owned expression. Operands must reference earlier expressions.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown local, a non-prior operand, or ID
    /// capacity exhaustion.
    pub fn add_expression(&mut self, expression: ProcExpr) -> Result<ProcExprId, ProcError> {
        let id = ProcExprId::from_index(self.expressions.len())?;
        let mut invalid = None;
        expression.kind.for_each_operand(|operand| {
            if operand.index() >= id.index() {
                invalid = Some(operand);
            }
        });
        if let Some(operand) = invalid {
            return Err(ProcError::new(format!(
                "transient expression {id:?} has non-prior operand {operand:?}"
            )));
        }
        if let ProcExprKind::LocalRead(local) = &expression.kind
            && local.index() >= self.locals.len()
        {
            return Err(ProcError::new(format!(
                "transient expression reads unknown local {local:?}"
            )));
        }
        self.expressions.push(expression);
        Ok(id)
    }

    /// Imports an already-built module-level value as a transient leaf.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when expression ID capacity is exhausted.
    pub fn add_module_value(
        &mut self,
        value: ValueId,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::ModuleValue(value),
            source,
        })
    }

    /// Looks up the type of an expression already owned by this builder.
    #[must_use]
    pub fn expression_type(&self, expression: ProcExprId) -> Option<WordType> {
        self.expressions
            .get(expression.index())
            .map(|value| value.ty)
    }

    /// Returns one expression already owned by this builder.
    #[must_use]
    pub fn expression(&self, expression: ProcExprId) -> Option<&ProcExpr> {
        self.expressions.get(expression.index())
    }

    /// Adds an owned typed constant.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for a width mismatch, unknown bits in a two-state
    /// type, or expression-ID capacity exhaustion.
    pub fn constant(
        &mut self,
        bits: ConstBits,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        if bits.width() != ty.width() {
            return Err(ProcError::new(
                "transient constant width differs from its type",
            ));
        }
        if ty.state() == LogicStateKind::TwoState
            && bits
                .as_slice()
                .iter()
                .any(|bit| matches!(bit, BitVal::X | BitVal::Z))
        {
            return Err(ProcError::new(
                "two-state transient constant cannot contain x or z bits",
            ));
        }
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::Constant(bits),
            source,
        })
    }

    /// Adds a place-like read of an automatic local.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown local or expression-ID exhaustion.
    pub fn read_local(
        &mut self,
        local: ProcLocalId,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let ty = self
            .locals
            .get(local.index())
            .ok_or_else(|| ProcError::new(format!("unknown procedural local {local:?}")))?
            .ty;
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::LocalRead(local),
            source,
        })
    }

    /// Adds a typed unary expression.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown operand or expression-ID exhaustion.
    pub fn unary(
        &mut self,
        op: UnaryOp,
        arg: ProcExprId,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let arg_ty = self.require_expression_type(arg)?;
        let ty = match op {
            UnaryOp::LogicalNot
            | UnaryOp::ReductionAnd
            | UnaryOp::ReductionOr
            | UnaryOp::ReductionXor => transient_type(1, false, arg_ty.state())?,
            UnaryOp::BitNot => arg_ty,
        };
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::Unary { op, arg },
            source,
        })
    }

    /// Adds a binary expression using the Word IR result-typing contract.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown operand, invalid result type, or
    /// expression-ID exhaustion.
    pub fn binary(
        &mut self,
        op: BinaryOp,
        left: ProcExprId,
        right: ProcExprId,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let left_ty = self.require_expression_type(left)?;
        let right_ty = self.require_expression_type(right)?;
        let state = merge_logic_state(left_ty.state(), right_ty.state());
        let ty = match op {
            BinaryOp::LogicalAnd
            | BinaryOp::LogicalOr
            | BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => transient_type(1, false, state)?,
            BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Ashr => {
                transient_type(left_ty.width(), left_ty.is_signed(), state)?
            }
            _ => transient_type(
                left_ty.width().max(right_ty.width()),
                left_ty.is_signed() && right_ty.is_signed(),
                state,
            )?,
        };
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::Binary { op, left, right },
            source,
        })
    }

    /// Adds a two-way owned expression selection.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] unless the condition is scalar and both values
    /// have identical types.
    pub fn mux(
        &mut self,
        condition: ProcExprId,
        then_value: ProcExprId,
        else_value: ProcExprId,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        if self.require_expression_type(condition)?.width() != 1 {
            return Err(ProcError::new("transient mux condition must be scalar"));
        }
        let ty = self.require_expression_type(then_value)?;
        if self.require_expression_type(else_value)? != ty {
            return Err(ProcError::new("transient mux value types differ"));
        }
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::Mux {
                condition,
                then_value,
                else_value,
            },
            source,
        })
    }

    /// Adds a conditionally enabled high-impedance expression.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] unless the enable is scalar or an operand is known.
    pub fn tri_state(
        &mut self,
        data: ProcExprId,
        enable: ProcExprId,
        active_high: bool,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        if self.require_expression_type(enable)?.width() != 1 {
            return Err(ProcError::new("transient tri-state enable must be scalar"));
        }
        let ty = self.require_expression_type(data)?;
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::TriState {
                data,
                enable,
                active_high,
            },
            source,
        })
    }

    /// Concatenates owned expressions, most-significant part first.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an empty list, unknown operand, width overflow,
    /// or expression-ID exhaustion.
    pub fn concat(
        &mut self,
        parts: impl IntoIterator<Item = ProcExprId>,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let parts = parts.into_iter().collect::<Vec<_>>();
        if parts.is_empty() {
            return Err(ProcError::new(
                "transient concat requires at least one part",
            ));
        }
        let mut width = 0u32;
        let mut state = LogicStateKind::TwoState;
        for part in &parts {
            let ty = self.require_expression_type(*part)?;
            width = width
                .checked_add(ty.width())
                .ok_or_else(|| ProcError::new("transient concat width exceeds 32-bit capacity"))?;
            state = merge_logic_state(state, ty.state());
        }
        self.add_expression(ProcExpr {
            ty: transient_type(width, false, state)?,
            kind: ProcExprKind::Concat(parts.into_boxed_slice()),
            source,
        })
    }

    /// Adds a fixed-position extraction.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for zero width, an unknown value, or an out-of-range
    /// extraction.
    pub fn extract(
        &mut self,
        value: ProcExprId,
        lsb: u32,
        width: u32,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let width = NonZeroU32::new(width)
            .ok_or_else(|| ProcError::new("transient extract width must be non-zero"))?;
        let value_ty = self.require_expression_type(value)?;
        if lsb
            .checked_add(width.get())
            .is_none_or(|end| end > value_ty.width())
        {
            return Err(ProcError::new("transient extract is out of range"));
        }
        self.add_expression(ProcExpr {
            ty: transient_type(width.get(), value_ty.is_signed(), value_ty.state())?,
            kind: ProcExprKind::Extract { value, lsb, width },
            source,
        })
    }

    /// Adds a runtime-positioned fixed-width extraction.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown operand, signed offset, zero width,
    /// or width larger than the source value.
    pub fn dynamic_extract(
        &mut self,
        value: ProcExprId,
        offset: ProcExprId,
        width: u32,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let width = NonZeroU32::new(width)
            .ok_or_else(|| ProcError::new("transient dynamic extract width must be non-zero"))?;
        let value_ty = self.require_expression_type(value)?;
        if self.require_expression_type(offset)?.is_signed() {
            return Err(ProcError::new(
                "transient dynamic extract offset must be unsigned",
            ));
        }
        if width.get() > value_ty.width() {
            return Err(ProcError::new(
                "transient dynamic extract width exceeds its value",
            ));
        }
        self.add_expression(ProcExpr {
            ty: transient_type(width.get(), value_ty.is_signed(), value_ty.state())?,
            kind: ProcExprKind::DynamicExtract {
                value,
                offset,
                width,
            },
            source,
        })
    }

    /// Adds a statically positioned range replacement.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for unknown operands, an out-of-range replacement,
    /// differing logic-state domains, or expression-ID exhaustion.
    pub fn insert(
        &mut self,
        value: ProcExprId,
        lsb: u32,
        replacement: ProcExprId,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let value_ty = self.require_expression_type(value)?;
        let replacement_ty = self.require_expression_type(replacement)?;
        if lsb
            .checked_add(replacement_ty.width())
            .is_none_or(|end| end > value_ty.width())
        {
            return Err(ProcError::new("transient insert is out of range"));
        }
        if replacement_ty.state() != value_ty.state() {
            return Err(ProcError::new(
                "transient insert replacement logic state differs from its value",
            ));
        }
        self.add_expression(ProcExpr {
            ty: value_ty,
            kind: ProcExprKind::Insert {
                value,
                lsb,
                replacement,
            },
            source,
        })
    }

    /// Adds a runtime-positioned range replacement.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for unknown operands, a signed offset, an excessive
    /// replacement width, differing logic-state domains, or ID exhaustion.
    pub fn dynamic_insert(
        &mut self,
        value: ProcExprId,
        offset: ProcExprId,
        replacement: ProcExprId,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let value_ty = self.require_expression_type(value)?;
        let offset_ty = self.require_expression_type(offset)?;
        let replacement_ty = self.require_expression_type(replacement)?;
        if offset_ty.is_signed() {
            return Err(ProcError::new(
                "transient dynamic insert offset must be unsigned",
            ));
        }
        if replacement_ty.width() > value_ty.width() {
            return Err(ProcError::new(
                "transient dynamic insert replacement exceeds its value",
            ));
        }
        if replacement_ty.state() != value_ty.state() {
            return Err(ProcError::new(
                "transient dynamic insert replacement logic state differs from its value",
            ));
        }
        self.add_expression(ProcExpr {
            ty: value_ty,
            kind: ProcExprKind::DynamicInsert {
                value,
                offset,
                replacement,
            },
            source,
        })
    }

    /// Adds an explicit width or signedness conversion.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when the conversion direction conflicts with the
    /// source and target widths or the operand is unknown.
    pub fn cast(
        &mut self,
        kind: CastKind,
        value: ProcExprId,
        target: WordType,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        let source_ty = self.require_expression_type(value)?;
        match kind {
            CastKind::ZeroExtend | CastKind::SignExtend if target.width() < source_ty.width() => {
                return Err(ProcError::new("transient extend cast cannot shrink"));
            }
            CastKind::Truncate if target.width() > source_ty.width() => {
                return Err(ProcError::new("transient truncate cast cannot widen"));
            }
            _ => {}
        }
        self.add_expression(ProcExpr {
            ty: target,
            kind: ProcExprKind::Cast { kind, value },
            source,
        })
    }

    /// Adds an asynchronous procedural memory read.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown address or dynamic offset, an
    /// invalid result type, or expression-ID exhaustion. Memory ownership and
    /// element-type agreement are validated during Word materialization.
    pub fn memory_read(
        &mut self,
        memory: MemoryId,
        address: ProcExprId,
        select: TransientTargetSelect,
        ty: WordType,
        source: SourceSpan,
    ) -> Result<ProcExprId, ProcError> {
        self.require_expression_type(address)?;
        if let TransientTargetSelect::Dynamic { offset, width } = select {
            if self.require_expression_type(offset)?.is_signed() {
                return Err(ProcError::new(
                    "transient memory-read select offset must be unsigned",
                ));
            }
            if width.get() != ty.width() {
                return Err(ProcError::new(
                    "transient memory-read select width differs from its result",
                ));
            }
        }
        self.add_expression(ProcExpr {
            ty,
            kind: ProcExprKind::MemoryRead {
                memory,
                address,
                select,
            },
            source,
        })
    }

    fn require_expression_type(&self, expression: ProcExprId) -> Result<WordType, ProcError> {
        self.expression_type(expression)
            .ok_or_else(|| ProcError::new(format!("unknown transient expression {expression:?}")))
    }

    /// Starts an implicitly sensitive procedure.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for a flip-flop kind or procedure-ID exhaustion.
    pub fn add_combinational_procedure(
        &mut self,
        kind: ProcedureKind,
        source: SourceSpan,
    ) -> Result<ProcedureId, ProcError> {
        self.control.add_combinational_procedure(kind, source)
    }

    /// Starts an edge-sensitive procedure.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an empty event set or procedure-ID exhaustion.
    pub fn add_clocked_procedure(
        &mut self,
        events: impl IntoIterator<Item = SensitivityEvent>,
        source: SourceSpan,
    ) -> Result<ProcedureId, ProcError> {
        self.control.add_clocked_procedure(events, source)
    }

    /// Appends a block to the current procedure.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for a non-current procedure or block capacity
    /// exhaustion.
    pub fn add_block(
        &mut self,
        procedure: ProcedureId,
        source: SourceSpan,
    ) -> Result<BlockId, ProcError> {
        self.control.add_block(procedure, source)
    }

    /// Selects the entry block of a procedure.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an unknown or foreign block.
    pub fn set_entry(&mut self, procedure: ProcedureId, entry: BlockId) -> Result<(), ProcError> {
        self.control.set_entry(procedure, entry)
    }

    /// Appends a source-ordered effect.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when `block` is unknown.
    pub fn assign(
        &mut self,
        block: BlockId,
        mode: AssignmentMode,
        target: TransientTarget,
        value: ProcExprId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.control.assign(block, mode, target, value, source)
    }

    /// Terminates a block by returning.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when the block is unknown or already terminated.
    pub fn terminate_return(
        &mut self,
        block: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.control.terminate_return(block, source)
    }

    /// Terminates a block with an ordinary jump, including a loop backedge.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when the block is unknown or already terminated.
    pub fn terminate_jump(
        &mut self,
        block: BlockId,
        target: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.control.terminate_jump(block, target, source)
    }

    /// Terminates a block with a conditional transfer.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when the block is unknown or already terminated.
    pub fn terminate_branch(
        &mut self,
        block: BlockId,
        condition: ProcExprId,
        then_target: BlockId,
        else_target: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.control
            .terminate_branch(block, condition, then_target, else_target, source)
    }

    /// Terminates a block with an ordered switch.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for an empty arm list, unknown block, or duplicate
    /// terminator.
    pub fn terminate_switch(
        &mut self,
        block: BlockId,
        selector: ProcExprId,
        arms: impl IntoIterator<Item = SwitchArmSpec<ProcExprId>>,
        default: BlockId,
        source: SourceSpan,
    ) -> Result<(), ProcError> {
        self.control
            .terminate_switch(block, selector, arms, default, source)
    }

    /// Records source-provided loop-region metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for a forward parent reference or region-ID
    /// capacity exhaustion.
    pub fn add_loop_region(&mut self, region: LoopRegion) -> Result<LoopRegionId, ProcError> {
        let id = LoopRegionId::from_index(self.loop_regions.len())?;
        if region
            .parent
            .is_some_and(|parent| parent.index() >= id.index())
        {
            return Err(ProcError::new(
                "transient loop parent must be an earlier region",
            ));
        }
        self.loop_regions.push(region);
        Ok(id)
    }

    /// Seals the structurally valid graph while retaining ordinary backedges.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for incomplete, unreachable, foreign, or malformed
    /// control, effect, expression, or loop-region records.
    pub fn seal(self) -> Result<TransientProcModule, ProcError> {
        TransientProcModule::from_builder(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Structurally validated transient procedural module. Cycles are permitted.
pub struct TransientProcModule {
    procedures: Box<[TransientProcedure]>,
    blocks: Box<[TransientBlock]>,
    effects: Box<[TransientEffect]>,
    events: Box<[SensitivityEvent]>,
    locals: Box<[ProcLocal]>,
    expressions: Box<[ProcExpr]>,
    loop_regions: Box<[LoopRegion]>,
}

impl TransientProcModule {
    fn from_builder(builder: TransientProcBuilder) -> Result<Self, ProcError> {
        let event_count = builder
            .control
            .procedures
            .iter()
            .try_fold(0usize, |count, procedure| {
                count.checked_add(procedure.events.len())
            })
            .ok_or_else(|| ProcError::new("transient event arena size overflow"))?;
        let mut events = Vec::with_capacity(event_count);
        let mut procedures = Vec::with_capacity(builder.control.procedures.len());
        for procedure in builder.control.procedures {
            let event_range = ArenaRange::new(events.len(), procedure.events.len(), "event")?;
            events.extend(procedure.events);
            procedures.push(TransientProcedure {
                kind: procedure.kind,
                sensitivity: if event_range.is_empty() {
                    Sensitivity::Implicit
                } else {
                    Sensitivity::Edges(event_range)
                },
                entry: procedure
                    .entry
                    .ok_or_else(|| ProcError::new("transient procedure has no entry block"))?,
                source: procedure.source,
                blocks: (procedure.block_start
                    ..procedure
                        .block_start
                        .checked_add(procedure.block_count)
                        .ok_or_else(|| {
                            ProcError::new("transient procedure block range overflows")
                        })?)
                    .map(BlockId::from_index)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
            });
        }

        let effect_count = builder
            .control
            .blocks
            .iter()
            .try_fold(0usize, |count, block| {
                count.checked_add(block.effects.len())
            })
            .ok_or_else(|| ProcError::new("transient effect arena size overflow"))?;
        let mut effects = Vec::with_capacity(effect_count);
        let mut blocks = Vec::with_capacity(builder.control.blocks.len());
        for block in builder.control.blocks {
            let effect_range =
                ArenaRange::new(effects.len(), block.effects.len(), "transient effect")?;
            effects.extend(block.effects.into_iter().map(|effect| TransientEffect {
                mode: effect.mode,
                target: effect.target,
                value: effect.value,
                source: effect.source,
            }));
            let terminator =
                materialize_transient_terminator(block.terminator.ok_or_else(|| {
                    ProcError::new("transient procedural block has no terminator")
                })?);
            blocks.push(TransientBlock {
                procedure: block.procedure,
                terminator,
                source: block.source,
                effects: effect_range,
            });
        }

        let module = Self {
            procedures: procedures.into_boxed_slice(),
            blocks: blocks.into_boxed_slice(),
            effects: effects.into_boxed_slice(),
            events: events.into_boxed_slice(),
            locals: builder.locals.into_boxed_slice(),
            expressions: builder.expressions.into_boxed_slice(),
            loop_regions: builder.loop_regions.into_boxed_slice(),
        };
        module.validate()?;
        Ok(module)
    }

    /// Returns all transient blocks in dense source order.
    #[must_use]
    pub fn blocks(&self) -> &[TransientBlock] {
        &self.blocks
    }

    /// Returns all automatic locals.
    #[must_use]
    pub fn locals(&self) -> &[ProcLocal] {
        &self.locals
    }

    /// Returns all owned expressions.
    #[must_use]
    pub fn expressions(&self) -> &[ProcExpr] {
        &self.expressions
    }

    /// Returns validated loop metadata in lexical insertion order.
    #[must_use]
    pub fn loop_regions(&self) -> &[LoopRegion] {
        &self.loop_regions
    }

    /// Iterates effects owned by one transient block.
    #[must_use]
    pub fn block_effects(
        &self,
        block: BlockId,
    ) -> Option<impl ExactSizeIterator<Item = &TransientEffect> + '_> {
        let range = self.blocks.get(block.index())?.effects;
        Some(range.indices().map(|index| &self.effects[index]))
    }

    /// Validates structural ownership, arena references, reachability, and loop
    /// metadata. Ordinary CFG cycles are intentionally accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] when any arena, reference, owner, edge, reachable
    /// block, or loop-region invariant is violated.
    pub fn validate(&self) -> Result<(), ProcError> {
        self.validate_expressions()?;
        let mut next_event = 0usize;
        let mut block_owners = vec![None; self.blocks.len()];
        for (index, procedure) in self.procedures.iter().enumerate() {
            let procedure_id = ProcedureId::from_index(index)?;
            if procedure.blocks.is_empty() || !procedure.blocks.contains(&procedure.entry) {
                return Err(ProcError::new(
                    "transient procedure has no blocks or its entry is not owned",
                ));
            }
            for &block in &procedure.blocks {
                let stored = self
                    .blocks
                    .get(block.index())
                    .ok_or_else(|| ProcError::new("transient procedure owns an unknown block"))?;
                if stored.procedure != procedure_id {
                    return Err(ProcError::new(
                        "transient block has the wrong procedure owner",
                    ));
                }
                if block_owners[block.index()].replace(procedure_id).is_some() {
                    return Err(ProcError::new(
                        "transient block is owned by multiple procedures",
                    ));
                }
            }
            match (procedure.kind, procedure.sensitivity) {
                (
                    ProcedureKind::Combinational
                    | ProcedureKind::CombinationalOrLatch
                    | ProcedureKind::Latch,
                    Sensitivity::Implicit,
                ) => {}
                (ProcedureKind::FlipFlop, Sensitivity::Edges(range)) if !range.is_empty() => {
                    if range.start as usize != next_event {
                        return Err(ProcError::new("transient event ranges are not contiguous"));
                    }
                    next_event = super::checked_end(range, self.events.len(), "transient event")?;
                }
                _ => {
                    return Err(ProcError::new(
                        "transient procedure kind and sensitivity are inconsistent",
                    ));
                }
            }
        }
        if block_owners.iter().any(Option::is_none) || next_event != self.events.len() {
            return Err(ProcError::new(
                "transient procedural arenas contain unowned records",
            ));
        }

        let mut effect_owners = vec![false; self.effects.len()];
        for (index, block) in self.blocks.iter().enumerate() {
            let id = BlockId::from_index(index)?;
            super::checked_end(block.effects, self.effects.len(), "transient effect")?;
            for effect_index in block.effects.indices() {
                if std::mem::replace(&mut effect_owners[effect_index], true) {
                    return Err(ProcError::new(
                        "transient effect is owned by multiple blocks",
                    ));
                }
                let effect = &self.effects[effect_index];
                self.validate_expression_id(effect.value)?;
                self.validate_target(effect.target)?;
            }
            self.validate_terminator(id, &block.terminator.kind)?;
        }
        if effect_owners.iter().any(|owned| !owned) {
            return Err(ProcError::new(
                "transient effect arena contains unowned records",
            ));
        }
        self.validate_reachability()?;
        self.validate_loop_regions()
    }

    fn validate_expressions(&self) -> Result<(), ProcError> {
        for (index, expression) in self.expressions.iter().enumerate() {
            let id = ProcExprId::from_index(index)?;
            let mut invalid = None;
            expression.kind.for_each_operand(|operand| {
                if operand.index() >= id.index() {
                    invalid = Some(operand);
                }
            });
            if let Some(operand) = invalid {
                return Err(ProcError::new(format!(
                    "transient expression {id:?} has non-prior operand {operand:?}"
                )));
            }
            if let ProcExprKind::LocalRead(local) = &expression.kind
                && local.index() >= self.locals.len()
            {
                return Err(ProcError::new(format!(
                    "transient expression reads unknown local {local:?}"
                )));
            }
        }
        Ok(())
    }

    fn validate_expression_id(&self, expression: ProcExprId) -> Result<(), ProcError> {
        if expression.index() >= self.expressions.len() {
            return Err(ProcError::new(format!(
                "unknown transient expression {expression:?}"
            )));
        }
        Ok(())
    }

    fn validate_target(&self, target: TransientTarget) -> Result<(), ProcError> {
        let select = match target {
            TransientTarget::Local { local, select } => {
                if local.index() >= self.locals.len() {
                    return Err(ProcError::new(format!(
                        "transient assignment targets unknown local {local:?}"
                    )));
                }
                select
            }
            TransientTarget::Signal { select, .. } => select,
            TransientTarget::Memory {
                address, select, ..
            } => {
                self.validate_expression_id(address)?;
                select
            }
        };
        if let TransientTargetSelect::Dynamic { offset, .. } = select {
            self.validate_expression_id(offset)?;
        }
        Ok(())
    }

    fn validate_terminator(
        &self,
        block: BlockId,
        terminator: &TransientTerminatorKind,
    ) -> Result<(), ProcError> {
        let owner = self.blocks[block.index()].procedure;
        let mut error = None;
        terminator.for_each_target(|target| {
            if self
                .blocks
                .get(target.index())
                .is_none_or(|target_block| target_block.procedure != owner)
            {
                error = Some(ProcError::new(
                    "transient CFG edge is unknown or crosses procedure ownership",
                ));
            }
        });
        if let Some(error) = error {
            return Err(error);
        }
        let mut expression_error = None;
        terminator.for_each_expression(|expression| {
            if expression.index() >= self.expressions.len() {
                expression_error = Some(ProcError::new(
                    "transient terminator references an unknown expression",
                ));
            }
        });
        expression_error.map_or(Ok(()), Err)
    }

    fn validate_reachability(&self) -> Result<(), ProcError> {
        let mut reached = vec![false; self.blocks.len()];
        let mut pending = Vec::with_capacity(self.blocks.len());
        for procedure in &self.procedures {
            pending.push(procedure.entry);
            while let Some(block) = pending.pop() {
                if std::mem::replace(&mut reached[block.index()], true) {
                    continue;
                }
                self.blocks[block.index()]
                    .terminator
                    .kind
                    .for_each_target(|target| pending.push(target));
            }
            if procedure.blocks.iter().any(|block| !reached[block.index()]) {
                return Err(ProcError::new(
                    "transient procedure contains a block unreachable from its entry",
                ));
            }
        }
        Ok(())
    }

    fn validate_loop_regions(&self) -> Result<(), ProcError> {
        for (index, region) in self.loop_regions.iter().enumerate() {
            let id = LoopRegionId::from_index(index)?;
            if region
                .parent
                .is_some_and(|parent| parent.index() >= id.index())
            {
                return Err(ProcError::new(
                    "transient loop parent must precede its child",
                ));
            }
            let nodes = [region.header, region.body, region.latch, region.exit];
            for node in nodes {
                if self
                    .blocks
                    .get(node.index())
                    .is_none_or(|block| block.procedure != region.procedure)
                {
                    return Err(ProcError::new(
                        "transient loop region references a block outside its procedure",
                    ));
                }
            }
            for left in 0..nodes.len() {
                if nodes[left + 1..].contains(&nodes[left]) {
                    return Err(ProcError::new(
                        "canonical transient loop nodes must be distinct",
                    ));
                }
            }
            let mut has_backedge = false;
            self.blocks[region.latch.index()]
                .terminator
                .kind
                .for_each_target(|target| has_backedge |= target == region.header);
            if !has_backedge {
                return Err(ProcError::new(
                    "transient loop region has no latch-to-header backedge",
                ));
            }
        }
        Ok(())
    }

    fn validate_acyclic(&self) -> Result<(), ProcError> {
        let mut indegree = vec![0u32; self.blocks.len()];
        for block in &self.blocks {
            let mut overflow = false;
            block.terminator.kind.for_each_target(|target| {
                if let Some(next) = indegree[target.index()].checked_add(1) {
                    indegree[target.index()] = next;
                } else {
                    overflow = true;
                }
            });
            if overflow {
                return Err(ProcError::new(
                    "transient procedural block indegree exceeds 32-bit capacity",
                ));
            }
        }
        let mut ready = Vec::with_capacity(self.blocks.len());
        for procedure in &self.procedures {
            ready.extend(
                procedure
                    .blocks
                    .iter()
                    .copied()
                    .filter(|block| indegree[block.index()] == 0),
            );
            let mut visited = 0usize;
            while let Some(block) = ready.pop() {
                visited += 1;
                let mut underflow = false;
                self.blocks[block.index()]
                    .terminator
                    .kind
                    .for_each_target(|target| {
                        if let Some(next) = indegree[target.index()].checked_sub(1) {
                            indegree[target.index()] = next;
                            if next == 0 {
                                ready.push(target);
                            }
                        } else {
                            underflow = true;
                        }
                    });
                if underflow {
                    return Err(ProcError::new(
                        "transient procedural CFG indegree is inconsistent",
                    ));
                }
            }
            if visited != procedure.blocks.len() {
                return Err(ProcError::new(
                    "transient procedure retains a control-flow cycle at materialization",
                ));
            }
        }
        Ok(())
    }

    /// Materializes an acyclic graph with published locals into final procedural IR.
    ///
    /// The expression closure of exact-reachable CFG blocks is appended to
    /// `word` transactionally. Limiting publication to that closure is
    /// semantically required because a procedural memory-read expression
    /// creates a structural read port rather than a removable pure operation.
    /// A graph with a residual backedge or automatic-local dependency must
    /// first pass loop elimination and typed process-local publication.
    ///
    /// # Errors
    ///
    /// Returns [`ProcError`] for a residual cycle or local, an invalid Word
    /// expression, malformed target, or final procedural validation failure.
    pub fn materialize_acyclic(self, word: &mut WordModule) -> Result<ProcModule, ProcError> {
        self.validate()?;
        self.validate_acyclic()?;
        if !self.locals.is_empty()
            || self
                .expressions
                .iter()
                .any(|expression| matches!(&expression.kind, ProcExprKind::LocalRead(_)))
            || self
                .effects
                .iter()
                .any(|effect| matches!(effect.target, TransientTarget::Local { .. }))
        {
            return Err(ProcError::new(
                "transient locals must be materialized before final procedure publication",
            ));
        }

        let checkpoint = word.speculation_checkpoint();
        let result = self.materialize_validated(word);
        if result.is_err() {
            word.rollback_speculation(checkpoint).map_err(|error| {
                ProcError::new(format!(
                    "cannot roll back failed transient expression materialization: {error}"
                ))
            })?;
        }
        result
    }

    fn materialize_validated(self, word: &mut WordModule) -> Result<ProcModule, ProcError> {
        let reachable_blocks = self.materialization_reachable_blocks();
        let reachable_expressions = self.materialization_reachable_expressions(&reachable_blocks);
        let mut values = Vec::with_capacity(self.expressions.len());
        for (expression_index, expression) in self.expressions.iter().enumerate() {
            if !reachable_expressions[expression_index] {
                values.push(None);
                continue;
            }
            let resolve = |id: ProcExprId, values: &[Option<ValueId>]| {
                values.get(id.index()).copied().flatten().ok_or_else(|| {
                    ProcError::new("transient expression operand is not materialized")
                })
            };
            let value = match &expression.kind {
                ProcExprKind::ModuleValue(value) => {
                    let stored = word
                        .value(*value)
                        .ok_or_else(|| ProcError::new(format!("unknown module value {value:?}")))?;
                    if stored.ty != expression.ty {
                        return Err(ProcError::new(
                            "transient module-value leaf has the wrong type",
                        ));
                    }
                    *value
                }
                ProcExprKind::Constant(bits) => word
                    .constant(bits.clone(), expression.ty, expression.source.clone())
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::LocalRead(_) => {
                    return Err(ProcError::new(
                        "cannot materialize an unresolved transient local read",
                    ));
                }
                ProcExprKind::MemoryRead {
                    memory,
                    address,
                    select,
                } => materialize_memory_read(
                    word,
                    *memory,
                    resolve(*address, &values)?,
                    *select,
                    &values,
                    expression.source.clone(),
                )?,
                ProcExprKind::Unary { op, arg } => word
                    .unary(*op, resolve(*arg, &values)?, expression.source.clone())
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::Binary { op, left, right } => word
                    .binary(
                        *op,
                        resolve(*left, &values)?,
                        resolve(*right, &values)?,
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::Mux {
                    condition,
                    then_value,
                    else_value,
                } => word
                    .mux(
                        resolve(*condition, &values)?,
                        resolve(*then_value, &values)?,
                        resolve(*else_value, &values)?,
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::TriState {
                    data,
                    enable,
                    active_high,
                } => word
                    .tri_state(
                        resolve(*data, &values)?,
                        Enable {
                            value: resolve(*enable, &values)?,
                            active_high: *active_high,
                        },
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::Concat(parts) => word
                    .concat(
                        parts
                            .iter()
                            .map(|part| resolve(*part, &values))
                            .collect::<Result<Vec<_>, _>>()?,
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::Extract { value, lsb, width } => word
                    .extract(
                        resolve(*value, &values)?,
                        *lsb,
                        width.get(),
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::DynamicExtract {
                    value,
                    offset,
                    width,
                } => word
                    .dynamic_extract(
                        resolve(*value, &values)?,
                        resolve(*offset, &values)?,
                        width.get(),
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::Insert {
                    value,
                    lsb,
                    replacement,
                } => materialize_static_insert(
                    word,
                    resolve(*value, &values)?,
                    *lsb,
                    resolve(*replacement, &values)?,
                    expression.source.clone(),
                )?,
                ProcExprKind::DynamicInsert {
                    value,
                    offset,
                    replacement,
                } => word
                    .dynamic_insert(
                        resolve(*value, &values)?,
                        resolve(*offset, &values)?,
                        resolve(*replacement, &values)?,
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
                ProcExprKind::Cast { kind, value } => word
                    .cast(
                        *kind,
                        resolve(*value, &values)?,
                        expression.ty,
                        expression.source.clone(),
                    )
                    .map_err(|error| ProcError::new(error.to_string()))?,
            };
            let actual_ty = word.value(value).map(|stored| stored.ty);
            if actual_ty != Some(expression.ty) {
                return Err(ProcError::new(format!(
                    "materialized transient expression {expression_index} has result type \
                     {actual_ty:?}, expected {:?} for {:?}",
                    expression.ty, expression.kind
                )));
            }
            values.push(Some(value));
        }

        let mut output = ProcBuilder::new();
        let mut block_ids = vec![None; self.blocks.len()];
        for procedure in &self.procedures {
            let id = match procedure.sensitivity {
                Sensitivity::Implicit => {
                    output.add_combinational_procedure(procedure.kind, procedure.source.clone())?
                }
                Sensitivity::Edges(events) => output.add_clocked_procedure(
                    events.indices().map(|event| self.events[event]),
                    procedure.source.clone(),
                )?,
            };
            for block_index in procedure
                .blocks
                .iter()
                .map(|block| block.index())
                .filter(|block| reachable_blocks[*block])
            {
                let block = &self.blocks[block_index];
                block_ids[block_index] = Some(output.add_block(id, block.source.clone())?);
            }
            output.set_entry(
                id,
                block_ids[procedure.entry.index()]
                    .expect("validated procedure entry has a materialized block"),
            )?;
        }
        for (index, block) in self.blocks.iter().enumerate() {
            let Some(output_block) = block_ids[index] else {
                continue;
            };
            for effect in block.effects.indices().map(|effect| &self.effects[effect]) {
                output.assign(
                    output_block,
                    effect.mode,
                    materialize_target(effect.target, &values, &self.expressions)?,
                    materialized_expression(&values, effect.value)?,
                    effect.source.clone(),
                )?;
            }
            match &block.terminator.kind {
                TransientTerminatorKind::Return => {
                    output.terminate_return(output_block, block.terminator.source.clone())?;
                }
                TransientTerminatorKind::Jump(target) => output.terminate_jump(
                    output_block,
                    mapped_reachable_block(&block_ids, *target)?,
                    block.terminator.source.clone(),
                )?,
                TransientTerminatorKind::Branch {
                    condition,
                    then_target,
                    else_target,
                } => {
                    if let Some(decision) = self.exact_branch_decision(*condition) {
                        output.terminate_jump(
                            output_block,
                            mapped_reachable_block(
                                &block_ids,
                                if decision { *then_target } else { *else_target },
                            )?,
                            block.terminator.source.clone(),
                        )?;
                    } else {
                        output.terminate_branch(
                            output_block,
                            materialized_expression(&values, *condition)?,
                            mapped_reachable_block(&block_ids, *then_target)?,
                            mapped_reachable_block(&block_ids, *else_target)?,
                            block.terminator.source.clone(),
                        )?;
                    }
                }
                TransientTerminatorKind::Switch {
                    selector,
                    arms,
                    default,
                } => output.terminate_switch(
                    output_block,
                    materialized_expression(&values, *selector)?,
                    arms.iter()
                        .map(|arm| {
                            Ok(SwitchArmSpec {
                                pattern: materialized_expression(&values, arm.pattern)?,
                                target: mapped_reachable_block(&block_ids, arm.target)?,
                                source: arm.source.clone(),
                            })
                        })
                        .collect::<Result<Vec<_>, ProcError>>()?,
                    mapped_reachable_block(&block_ids, *default)?,
                    block.terminator.source.clone(),
                )?,
            }
        }
        output.seal()
    }

    fn exact_branch_decision(&self, condition: ProcExprId) -> Option<bool> {
        let ProcExprKind::Constant(bits) = &self.expressions[condition.index()].kind else {
            return None;
        };
        bits.as_slice()
            .iter()
            .try_fold(false, |truth, bit| match bit {
                BitVal::Zero => Some(truth),
                BitVal::One => Some(true),
                BitVal::X | BitVal::Z => None,
            })
    }

    fn materialization_reachable_blocks(&self) -> Vec<bool> {
        let mut reachable = vec![false; self.blocks.len()];
        for procedure in &self.procedures {
            let mut pending = vec![procedure.entry];
            while let Some(block) = pending.pop() {
                if std::mem::replace(&mut reachable[block.index()], true) {
                    continue;
                }
                match &self.blocks[block.index()].terminator.kind {
                    TransientTerminatorKind::Return => {}
                    TransientTerminatorKind::Jump(target) => pending.push(*target),
                    TransientTerminatorKind::Branch {
                        condition,
                        then_target,
                        else_target,
                    } => match self.exact_branch_decision(*condition) {
                        Some(true) => pending.push(*then_target),
                        Some(false) => pending.push(*else_target),
                        None => {
                            pending.push(*else_target);
                            pending.push(*then_target);
                        }
                    },
                    TransientTerminatorKind::Switch { arms, default, .. } => {
                        pending.push(*default);
                        pending.extend(arms.iter().map(|arm| arm.target));
                    }
                }
            }
        }
        reachable
    }

    fn materialization_reachable_expressions(&self, blocks: &[bool]) -> Vec<bool> {
        let mut reachable = vec![false; self.expressions.len()];
        let mut pending = Vec::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            if !blocks[block_index] {
                continue;
            }
            for effect_index in block.effects.indices() {
                let effect = &self.effects[effect_index];
                pending.push(effect.value);
                let select = match effect.target {
                    TransientTarget::Local { select, .. }
                    | TransientTarget::Signal { select, .. } => select,
                    TransientTarget::Memory {
                        address, select, ..
                    } => {
                        pending.push(address);
                        select
                    }
                };
                if let TransientTargetSelect::Dynamic { offset, .. } = select {
                    pending.push(offset);
                }
            }
            block
                .terminator
                .kind
                .for_each_expression(|expression| pending.push(expression));
        }
        while let Some(expression) = pending.pop() {
            if std::mem::replace(&mut reachable[expression.index()], true) {
                continue;
            }
            self.expressions[expression.index()]
                .kind
                .for_each_operand(|operand| pending.push(operand));
        }
        reachable
    }
}

fn materialized_expression(
    values: &[Option<ValueId>],
    expression: ProcExprId,
) -> Result<ValueId, ProcError> {
    values
        .get(expression.index())
        .copied()
        .flatten()
        .ok_or_else(|| ProcError::new("reachable transient expression was not materialized"))
}

fn mapped_reachable_block(
    blocks: &[Option<BlockId>],
    block: BlockId,
) -> Result<BlockId, ProcError> {
    blocks
        .get(block.index())
        .copied()
        .flatten()
        .ok_or_else(|| ProcError::new(format!("reachable edge targets pruned block {block:?}")))
}

fn transient_type(width: u32, signed: bool, state: LogicStateKind) -> Result<WordType, ProcError> {
    WordType::new(width, signed, state).map_err(|error| ProcError::new(error.to_string()))
}

const fn merge_logic_state(left: LogicStateKind, right: LogicStateKind) -> LogicStateKind {
    if matches!(left, LogicStateKind::FourState) || matches!(right, LogicStateKind::FourState) {
        LogicStateKind::FourState
    } else {
        LogicStateKind::TwoState
    }
}

fn materialize_transient_terminator(
    terminator: TerminatorDraft<ProcExprId>,
) -> TransientTerminator {
    let (kind, source) = match terminator {
        TerminatorDraft::Return(source) => (TransientTerminatorKind::Return, source),
        TerminatorDraft::Jump { target, source } => (TransientTerminatorKind::Jump(target), source),
        TerminatorDraft::Branch {
            condition,
            then_target,
            else_target,
            source,
        } => (
            TransientTerminatorKind::Branch {
                condition,
                then_target,
                else_target,
            },
            source,
        ),
        TerminatorDraft::Switch {
            selector,
            arms,
            default,
            source,
        } => (
            TransientTerminatorKind::Switch {
                selector,
                arms: arms
                    .into_iter()
                    .map(|arm| TransientSwitchArm {
                        pattern: arm.pattern,
                        target: arm.target,
                        source: arm.source,
                    })
                    .collect(),
                default,
            },
            source,
        ),
    };
    TransientTerminator { kind, source }
}

fn materialize_static_insert(
    word: &mut WordModule,
    value: ValueId,
    lsb: u32,
    replacement: ValueId,
    source: SourceSpan,
) -> Result<ValueId, ProcError> {
    let value_ty = word
        .value(value)
        .ok_or_else(|| ProcError::new(format!("unknown static-insert value {value:?}")))?
        .ty;
    let offset_width = (u32::BITS - (value_ty.width() - 1).leading_zeros()).max(1);
    let offset_ty = transient_type(offset_width, false, LogicStateKind::TwoState)?;
    let bits = (0..offset_width)
        .rev()
        .map(|bit| {
            if lsb.checked_shr(bit).unwrap_or(0) & 1 == 0 {
                BitVal::Zero
            } else {
                BitVal::One
            }
        })
        .collect();
    let offset = word
        .constant(
            ConstBits::from_bits(bits).map_err(|error| ProcError::new(error.to_string()))?,
            offset_ty,
            source.clone(),
        )
        .map_err(|error| ProcError::new(error.to_string()))?;
    word.dynamic_insert(value, offset, replacement, source)
        .map_err(|error| ProcError::new(error.to_string()))
}

fn materialize_target(
    target: TransientTarget,
    values: &[Option<ValueId>],
    expressions: &[ProcExpr],
) -> Result<ProcTarget, ProcError> {
    let select = |select: TransientTargetSelect| -> Result<TargetSelect, ProcError> {
        Ok(match select {
            TransientTargetSelect::Whole => TargetSelect::Whole,
            TransientTargetSelect::Static(range) => TargetSelect::Static(range),
            TransientTargetSelect::Dynamic { offset, width } => {
                if let Some(lsb) = exact_unsigned_expression(expressions, offset)
                    .and_then(|offset| u32::try_from(offset).ok())
                    && let Some(msb) = lsb.checked_add(width.get() - 1)
                {
                    TargetSelect::Static(BitRange { msb, lsb })
                } else {
                    TargetSelect::Dynamic {
                        offset: materialized_expression(values, offset)?,
                        width,
                    }
                }
            }
        })
    };
    Ok(match target {
        TransientTarget::Local { .. } => {
            return Err(ProcError::new(
                "cannot materialize an unresolved transient local target",
            ));
        }
        TransientTarget::Signal {
            signal,
            select: target_select,
        } => ProcTarget::signal(signal).with_select(select(target_select)?),
        TransientTarget::Memory {
            memory,
            address,
            select: target_select,
        } => ProcTarget::memory(memory, materialized_expression(values, address)?)
            .with_select(select(target_select)?),
    })
}

fn exact_unsigned_expression(expressions: &[ProcExpr], expression: ProcExprId) -> Option<usize> {
    let stored = expressions.get(expression.index())?;
    let ProcExprKind::Constant(bits) = &stored.kind else {
        return None;
    };
    exact::ExactValue::from_constant(bits, stored.ty)?.unsigned_usize()
}

fn materialize_memory_read(
    word: &mut WordModule,
    memory: MemoryId,
    address: ValueId,
    select: TransientTargetSelect,
    values: &[Option<ValueId>],
    source: SourceSpan,
) -> Result<ValueId, ProcError> {
    let definition = word
        .memory(memory)
        .ok_or_else(|| ProcError::new(format!("unknown memory {memory:?}")))?;
    let element_type = definition.element_type;
    let base = word.name_str(definition.name).to_string();
    let mut ordinal = word.memory_read_ports().len();
    let name = loop {
        let candidate = format!("{base}$read${ordinal}");
        if word.signal_id(&candidate).is_none() && word.memory_id(&candidate).is_none() {
            break candidate;
        }
        ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| ProcError::new("memory-read name space is exhausted"))?;
    };
    let data = word
        .add_wire(name, element_type, source.clone())
        .map_err(|error| ProcError::new(error.to_string()))?;
    word.add_memory_read_port(MemoryReadPort {
        memory,
        address,
        data,
        timing: MemoryReadTiming::Asynchronous,
        read_during_write: ReadDuringWrite::OldData,
        source: source.clone(),
    })
    .map_err(|error| ProcError::new(error.to_string()))?;
    let value = word
        .read_signal(data, source.clone())
        .map_err(|error| ProcError::new(error.to_string()))?;
    match select {
        TransientTargetSelect::Whole => Ok(value),
        TransientTargetSelect::Static(range) => word
            .extract(value, range.msb.min(range.lsb), range.width(), source)
            .map_err(|error| ProcError::new(error.to_string())),
        TransientTargetSelect::Dynamic { offset, width } => word
            .dynamic_extract(
                value,
                materialized_expression(values, offset)?,
                width.get(),
                source,
            )
            .map_err(|error| ProcError::new(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::word::{LogicStateKind, PortDirection, SignalKind, ValueKind};

    fn bit() -> WordType {
        WordType::new(1, false, LogicStateKind::FourState).unwrap()
    }

    fn nibble() -> WordType {
        WordType::new(4, false, LogicStateKind::FourState).unwrap()
    }

    fn signed_nibble() -> WordType {
        WordType::new(4, true, LogicStateKind::FourState).unwrap()
    }

    fn signed_integer() -> WordType {
        WordType::new(32, true, LogicStateKind::FourState).unwrap()
    }

    fn span() -> SourceSpan {
        SourceSpan::stable("transient procedure test")
    }

    fn runtime_bound_loop(comparison: BinaryOp) -> (WordModule, TransientProcModule, LoopRegionId) {
        runtime_bound_loop_with_types(comparison, nibble(), nibble(), None)
    }

    fn runtime_bound_loop_with_types(
        comparison: BinaryOp,
        induction_ty: WordType,
        limit_ty: WordType,
        limit_cast: Option<CastKind>,
    ) -> (WordModule, TransientProcModule, LoopRegionId) {
        let mut word = WordModule::new("top");
        let limit_port = word
            .add_port("limit", PortDirection::Input, limit_ty, span())
            .unwrap();
        let limit_signal = word.port(limit_port).unwrap().signal;
        let limit_value = word.read_signal(limit_signal, span()).unwrap();
        let mut builder = TransientProcBuilder::new();
        let local = builder
            .add_local(ProcLocal {
                name: "induction".into(),
                ty: induction_ty,
                source: span(),
            })
            .unwrap();
        let zero_bits = vec![BitVal::Zero; induction_ty.width() as usize];
        let zero = builder
            .constant(
                ConstBits::from_bits(zero_bits).unwrap(),
                induction_ty,
                span(),
            )
            .unwrap();
        let mut one_bits = vec![BitVal::Zero; induction_ty.width() as usize];
        *one_bits.last_mut().unwrap() = BitVal::One;
        let one = builder
            .constant(
                ConstBits::from_bits(one_bits).unwrap(),
                induction_ty,
                span(),
            )
            .unwrap();
        let local_read = builder.read_local(local, span()).unwrap();
        let mut limit = builder
            .add_module_value(limit_value, limit_ty, span())
            .unwrap();
        if let Some(kind) = limit_cast {
            limit = builder.cast(kind, limit, induction_ty, span()).unwrap();
        }
        let condition = builder
            .binary(comparison, local_read, limit, span())
            .unwrap();
        let increment = builder
            .binary(BinaryOp::Add, local_read, one, span())
            .unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let entry = builder.add_block(procedure, span()).unwrap();
        let header = builder.add_block(procedure, span()).unwrap();
        let body = builder.add_block(procedure, span()).unwrap();
        let latch = builder.add_block(procedure, span()).unwrap();
        let exit = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                entry,
                AssignmentMode::Blocking,
                TransientTarget::local(local),
                zero,
                span(),
            )
            .unwrap();
        builder.terminate_jump(entry, header, span()).unwrap();
        builder
            .terminate_branch(header, condition, body, exit, span())
            .unwrap();
        builder.terminate_jump(body, latch, span()).unwrap();
        builder
            .assign(
                latch,
                AssignmentMode::Blocking,
                TransientTarget::local(local),
                increment,
                span(),
            )
            .unwrap();
        builder.terminate_jump(latch, header, span()).unwrap();
        builder.terminate_return(exit, span()).unwrap();
        let region = builder
            .add_loop_region(LoopRegion {
                procedure,
                header,
                body,
                latch,
                exit,
                form: LoopForm::PreTest,
                parent: None,
                source: span(),
            })
            .unwrap();
        (word, builder.seal().unwrap(), region)
    }

    #[test]
    fn cyclic_graph_accepts_a_validated_natural_loop_but_final_materialization_rejects_it() {
        let mut word = WordModule::new("top");
        let input = word
            .add_port("a", PortDirection::Input, bit(), span())
            .unwrap();
        let signal = word.port(input).unwrap().signal;
        let value = word.read_signal(signal, span()).unwrap();
        let mut builder = TransientProcBuilder::new();
        let condition = builder.add_module_value(value, bit(), span()).unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let header = builder.add_block(procedure, span()).unwrap();
        let body = builder.add_block(procedure, span()).unwrap();
        let latch = builder.add_block(procedure, span()).unwrap();
        let exit = builder.add_block(procedure, span()).unwrap();
        builder
            .terminate_branch(header, condition, body, exit, span())
            .unwrap();
        builder.terminate_jump(body, latch, span()).unwrap();
        builder.terminate_jump(latch, header, span()).unwrap();
        builder.terminate_return(exit, span()).unwrap();
        builder
            .add_loop_region(LoopRegion {
                procedure,
                header,
                body,
                latch,
                exit,
                form: LoopForm::PreTest,
                parent: None,
                source: span(),
            })
            .unwrap();
        let graph = builder.seal().unwrap();
        assert_eq!(graph.loop_regions().len(), 1);
        assert!(
            graph
                .materialize_acyclic(&mut word)
                .unwrap_err()
                .to_string()
                .contains("retains a control-flow cycle")
        );
    }

    #[test]
    fn cfg_promotion_owns_signal_recurrence_and_copyback_policy() {
        let mut word = WordModule::new("top");
        let state = word.add_wire("state", nibble(), span()).unwrap();
        let output = word
            .add_port("result", PortDirection::Output, nibble(), span())
            .unwrap();
        let output = word.port(output).unwrap().signal;
        let state_value = word.read_signal(state, span()).unwrap();

        let mut builder = TransientProcBuilder::new();
        let zero = builder
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero; 4]).unwrap(),
                nibble(),
                span(),
            )
            .unwrap();
        let one = builder
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero, BitVal::Zero, BitVal::Zero, BitVal::One])
                    .unwrap(),
                nibble(),
                span(),
            )
            .unwrap();
        let limit = builder
            .constant(
                ConstBits::from_bits(vec![BitVal::Zero, BitVal::Zero, BitVal::One, BitVal::One])
                    .unwrap(),
                nibble(),
                span(),
            )
            .unwrap();
        let loop_read = builder
            .add_module_value(state_value, nibble(), span())
            .unwrap();
        let condition = builder
            .binary(BinaryOp::Lt, loop_read, limit, span())
            .unwrap();
        let increment = builder
            .binary(BinaryOp::Add, loop_read, one, span())
            .unwrap();
        let exit_read = builder
            .add_module_value(state_value, nibble(), span())
            .unwrap();

        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let entry = builder.add_block(procedure, span()).unwrap();
        let header = builder.add_block(procedure, span()).unwrap();
        let body = builder.add_block(procedure, span()).unwrap();
        let latch = builder.add_block(procedure, span()).unwrap();
        let exit = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                entry,
                AssignmentMode::Blocking,
                TransientTarget::signal(state),
                zero,
                span(),
            )
            .unwrap();
        builder.terminate_jump(entry, header, span()).unwrap();
        builder
            .terminate_branch(header, condition, body, exit, span())
            .unwrap();
        builder.terminate_jump(body, latch, span()).unwrap();
        builder
            .assign(
                latch,
                AssignmentMode::Blocking,
                TransientTarget::signal(state),
                increment,
                span(),
            )
            .unwrap();
        builder.terminate_jump(latch, header, span()).unwrap();
        builder
            .assign(
                exit,
                AssignmentMode::Blocking,
                TransientTarget::signal(output),
                exit_read,
                span(),
            )
            .unwrap();
        builder.terminate_return(exit, span()).unwrap();
        builder
            .add_loop_region(LoopRegion {
                procedure,
                header,
                body,
                latch,
                exit,
                form: LoopForm::PreTest,
                parent: None,
                source: span(),
            })
            .unwrap();

        let promoted = builder
            .seal()
            .unwrap()
            .promote_loop_signal_state(&word)
            .unwrap();
        assert_eq!(promoted.locals().len(), 1);
        assert!(
            promoted
                .block_effects(latch)
                .unwrap()
                .any(|effect| matches!(effect.target, TransientTarget::Local { .. }))
        );
        assert!(promoted.block_effects(exit).unwrap().any(|effect| matches!(
            effect.target,
            TransientTarget::Signal { signal, .. } if signal == state
        )));
        promoted
            .prove_and_eliminate_loops(&word, LoopAnalysisLimits::default())
            .unwrap();
    }

    #[test]
    fn owned_acyclic_expressions_materialize_into_word_and_final_proc_ir() {
        let mut word = WordModule::new("top");
        let input = word
            .add_port("a", PortDirection::Input, bit(), span())
            .unwrap();
        let output = word
            .add_port("y", PortDirection::Output, bit(), span())
            .unwrap();
        let input_signal = word.port(input).unwrap().signal;
        let output_signal = word.port(output).unwrap().signal;
        let input_value = word.read_signal(input_signal, span()).unwrap();
        let mut builder = TransientProcBuilder::new();
        let input = builder
            .add_module_value(input_value, bit(), span())
            .unwrap();
        let inverted = builder
            .add_expression(ProcExpr {
                ty: bit(),
                kind: ProcExprKind::Unary {
                    op: UnaryOp::BitNot,
                    arg: input,
                },
                source: span(),
            })
            .unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let block = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                block,
                AssignmentMode::Blocking,
                TransientTarget::signal(output_signal),
                inverted,
                span(),
            )
            .unwrap();
        builder.terminate_return(block, span()).unwrap();

        let procedures = builder
            .seal()
            .unwrap()
            .materialize_acyclic(&mut word)
            .unwrap();
        assert_eq!(procedures.effects().len(), 1);
        assert_eq!(word.operations().len(), 1);
    }

    #[test]
    fn final_publication_requires_local_materialization() {
        let mut word = WordModule::new("top");
        let mut builder = TransientProcBuilder::new();
        let local = builder
            .add_local(ProcLocal {
                name: "local".into(),
                ty: bit(),
                source: span(),
            })
            .unwrap();
        let local_value = builder
            .add_expression(ProcExpr {
                ty: bit(),
                kind: ProcExprKind::LocalRead(local),
                source: span(),
            })
            .unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let block = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                block,
                AssignmentMode::Blocking,
                TransientTarget::local(local),
                local_value,
                span(),
            )
            .unwrap();
        builder.terminate_return(block, span()).unwrap();

        assert!(
            builder
                .seal()
                .unwrap()
                .materialize_acyclic(&mut word)
                .unwrap_err()
                .to_string()
                .contains("locals must be materialized")
        );
    }

    #[test]
    fn partial_local_assignments_remain_ordered_process_local_effects() {
        let mut word = WordModule::new("top");
        let output = word
            .add_port("y", PortDirection::Output, nibble(), span())
            .unwrap();
        let output_signal = word.port(output).unwrap().signal;
        let mut builder = TransientProcBuilder::new();
        let local = builder
            .add_local(ProcLocal {
                name: "local".into(),
                ty: nibble(),
                source: span(),
            })
            .unwrap();
        let low = builder
            .constant(
                ConstBits::from_bin_str("01").unwrap(),
                WordType::new(2, false, LogicStateKind::FourState).unwrap(),
                span(),
            )
            .unwrap();
        let high = builder
            .constant(
                ConstBits::from_bin_str("10").unwrap(),
                WordType::new(2, false, LogicStateKind::FourState).unwrap(),
                span(),
            )
            .unwrap();
        let local_read = builder.read_local(local, span()).unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let block = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                block,
                AssignmentMode::Blocking,
                TransientTarget::local(local)
                    .with_select(TransientTargetSelect::Static(BitRange { msb: 1, lsb: 0 })),
                low,
                span(),
            )
            .unwrap();
        builder
            .assign(
                block,
                AssignmentMode::Blocking,
                TransientTarget::local(local)
                    .with_select(TransientTargetSelect::Static(BitRange { msb: 3, lsb: 2 })),
                high,
                span(),
            )
            .unwrap();
        builder
            .assign(
                block,
                AssignmentMode::Blocking,
                TransientTarget::signal(output_signal),
                local_read,
                span(),
            )
            .unwrap();
        builder.terminate_return(block, span()).unwrap();

        let graph = builder
            .seal()
            .unwrap()
            .materialize_locals(&mut word)
            .unwrap();
        assert!(graph.locals().is_empty());
        let local_signal = word.signal_id("local").unwrap();
        assert_eq!(
            word.signal(local_signal).unwrap().kind,
            SignalKind::ProcessLocal
        );
        let procedures = graph.materialize_acyclic(&mut word).unwrap();

        assert_eq!(procedures.effects().len(), 3);
        assert!(procedures.effects()[..2].iter().all(|effect| matches!(
            effect.target,
            ProcTarget::Signal { signal, .. } if signal == local_signal
        )));
        assert!(matches!(
            word.value(procedures.effects()[2].value).map(|value| &value.kind),
            Some(ValueKind::Signal(reference)) if reference.signal == local_signal
        ));
    }

    #[test]
    fn exact_local_state_proof_certifies_and_eliminates_one_backedge() {
        let mut word = WordModule::new("top");
        let output = word
            .add_port("y", PortDirection::Output, nibble(), span())
            .unwrap();
        let output_signal = word.port(output).unwrap().signal;
        let mut builder = TransientProcBuilder::new();
        let local = builder
            .add_local(ProcLocal {
                name: "induction".into(),
                ty: nibble(),
                source: span(),
            })
            .unwrap();
        let zero = builder
            .add_expression(ProcExpr {
                ty: nibble(),
                kind: ProcExprKind::Constant(ConstBits::from_bin_str("0000").unwrap()),
                source: span(),
            })
            .unwrap();
        let one = builder
            .add_expression(ProcExpr {
                ty: nibble(),
                kind: ProcExprKind::Constant(ConstBits::from_bin_str("0001").unwrap()),
                source: span(),
            })
            .unwrap();
        let three = builder
            .add_expression(ProcExpr {
                ty: nibble(),
                kind: ProcExprKind::Constant(ConstBits::from_bin_str("0011").unwrap()),
                source: span(),
            })
            .unwrap();
        let local_read = builder
            .add_expression(ProcExpr {
                ty: nibble(),
                kind: ProcExprKind::LocalRead(local),
                source: span(),
            })
            .unwrap();
        let condition = builder
            .add_expression(ProcExpr {
                ty: bit(),
                kind: ProcExprKind::Binary {
                    op: BinaryOp::Lt,
                    left: local_read,
                    right: three,
                },
                source: span(),
            })
            .unwrap();
        let increment = builder
            .add_expression(ProcExpr {
                ty: nibble(),
                kind: ProcExprKind::Binary {
                    op: BinaryOp::Add,
                    left: local_read,
                    right: one,
                },
                source: span(),
            })
            .unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let entry = builder.add_block(procedure, span()).unwrap();
        let header = builder.add_block(procedure, span()).unwrap();
        let body = builder.add_block(procedure, span()).unwrap();
        let latch = builder.add_block(procedure, span()).unwrap();
        let exit = builder.add_block(procedure, span()).unwrap();
        builder
            .assign(
                entry,
                AssignmentMode::Blocking,
                TransientTarget::local(local),
                zero,
                span(),
            )
            .unwrap();
        builder.terminate_jump(entry, header, span()).unwrap();
        builder
            .terminate_branch(header, condition, body, exit, span())
            .unwrap();
        builder
            .assign(
                body,
                AssignmentMode::Blocking,
                TransientTarget::signal(output_signal),
                local_read,
                span(),
            )
            .unwrap();
        builder.terminate_jump(body, latch, span()).unwrap();
        builder
            .assign(
                latch,
                AssignmentMode::Blocking,
                TransientTarget::local(local),
                increment,
                span(),
            )
            .unwrap();
        builder.terminate_jump(latch, header, span()).unwrap();
        builder.terminate_return(exit, span()).unwrap();
        let region = builder
            .add_loop_region(LoopRegion {
                procedure,
                header,
                body,
                latch,
                exit,
                form: LoopForm::PreTest,
                parent: None,
                source: span(),
            })
            .unwrap();

        let unrelated_procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let unrelated_block = builder.add_block(unrelated_procedure, span()).unwrap();
        builder.terminate_return(unrelated_block, span()).unwrap();

        let graph = builder.seal().unwrap();
        let limits = LoopAnalysisLimits {
            max_expanded_blocks: 32,
            ..LoopAnalysisLimits::default()
        };
        {
            let proof = LoopBoundednessAnalysis::new(&graph, &word, limits)
                .prove_exact(region)
                .unwrap();
            assert_eq!(proof.max_header_visits(), 4);
            assert_eq!(proof.method(), LoopProofMethod::ExactStateEnumeration);
        }

        let graph = graph.prove_and_eliminate_loops(&word, limits).unwrap();
        assert!(graph.loop_regions().is_empty());
        assert_eq!(graph.blocks().len(), 15);
        assert_eq!(
            graph.blocks()[unrelated_block.index()].procedure,
            unrelated_procedure,
            "loop elimination must not renumber an unrelated procedure"
        );
        graph.validate_acyclic().unwrap();

        let graph = graph.materialize_locals(&mut word).unwrap();
        assert!(graph.locals().is_empty());
        let procedures = graph.materialize_acyclic(&mut word).unwrap();
        assert_eq!(
            procedures.blocks().len(),
            13,
            "exact-infeasible control and dead local updates are not published"
        );
    }

    #[test]
    fn range_facts_prove_a_loop_bounded_by_an_unsigned_runtime_value() {
        let (word, graph, region) = runtime_bound_loop(BinaryOp::Lt);
        let limits = LoopAnalysisLimits {
            max_expanded_blocks: 128,
            ..LoopAnalysisLimits::default()
        };

        {
            let proof = LoopBoundednessAnalysis::new(&graph, &word, limits)
                .prove_exact(region)
                .unwrap();

            assert_eq!(proof.max_header_visits(), 16);
            assert_eq!(proof.explored_states(), 16);
        }
        graph
            .prove_and_eliminate_loops(&word, limits)
            .unwrap()
            .validate_acyclic()
            .unwrap();
    }

    #[test]
    fn loop_limits_distinguish_profile_structure_from_analysis_exhaustion() {
        let (word, graph, region) = runtime_bound_loop(BinaryOp::Lt);
        let structural = LoopBoundednessAnalysis::new(
            &graph,
            &word,
            LoopAnalysisLimits {
                max_expanded_blocks: graph.blocks().len(),
                ..LoopAnalysisLimits::default()
            },
        )
        .prove_exact(region)
        .unwrap_err();
        assert!(
            structural
                .to_string()
                .contains("source-profile structural limit")
        );

        let analysis = LoopBoundednessAnalysis::new(
            &graph,
            &word,
            LoopAnalysisLimits {
                max_expanded_blocks: 128,
                max_analysis_states: 1,
                ..LoopAnalysisLimits::default()
            },
        )
        .prove_exact(region)
        .unwrap_err();
        assert!(
            analysis
                .to_string()
                .contains("boundedness-analysis capability gap")
        );
    }

    #[test]
    fn range_facts_do_not_hide_runtime_bound_wraparound() {
        let (word, graph, region) = runtime_bound_loop(BinaryOp::Le);

        let error = LoopBoundednessAnalysis::new(
            &graph,
            &word,
            LoopAnalysisLimits {
                max_expanded_blocks: 128,
                ..LoopAnalysisLimits::default()
            },
        )
        .prove_exact(region)
        .unwrap_err();

        assert!(error.to_string().contains("reaches the header twice"));
    }

    #[test]
    fn range_facts_preserve_a_narrow_signed_bound_through_sign_extension() {
        let (word, graph, region) = runtime_bound_loop_with_types(
            BinaryOp::Lt,
            signed_integer(),
            signed_nibble(),
            Some(CastKind::SignExtend),
        );
        let limits = LoopAnalysisLimits {
            max_expanded_blocks: 128,
            ..LoopAnalysisLimits::default()
        };

        let proof = LoopBoundednessAnalysis::new(&graph, &word, limits)
            .prove_exact(region)
            .unwrap();

        assert_eq!(proof.max_header_visits(), 8);
    }

    #[test]
    fn exact_loop_proof_rejects_a_repeated_reachable_state() {
        let word = WordModule::new("top");
        let mut builder = TransientProcBuilder::new();
        let always = builder
            .add_expression(ProcExpr {
                ty: bit(),
                kind: ProcExprKind::Constant(ConstBits::from_bin_str("1").unwrap()),
                source: span(),
            })
            .unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let header = builder.add_block(procedure, span()).unwrap();
        let body = builder.add_block(procedure, span()).unwrap();
        let latch = builder.add_block(procedure, span()).unwrap();
        let exit = builder.add_block(procedure, span()).unwrap();
        builder
            .terminate_branch(header, always, body, exit, span())
            .unwrap();
        builder.terminate_jump(body, latch, span()).unwrap();
        builder.terminate_jump(latch, header, span()).unwrap();
        builder.terminate_return(exit, span()).unwrap();
        let region = builder
            .add_loop_region(LoopRegion {
                procedure,
                header,
                body,
                latch,
                exit,
                form: LoopForm::Unconditional,
                parent: None,
                source: span(),
            })
            .unwrap();
        let graph = builder.seal().unwrap();

        let error = LoopBoundednessAnalysis::new(&graph, &word, LoopAnalysisLimits::default())
            .prove_exact(region)
            .unwrap_err();
        assert!(error.to_string().contains("reaches the header twice"));
    }

    #[test]
    fn exact_loop_proof_distinguishes_cfg_joins_from_undeclared_cycles() {
        let word = WordModule::new("top");
        let mut builder = TransientProcBuilder::new();
        let condition = builder
            .constant(ConstBits::from_bin_str("1").unwrap(), bit(), span())
            .unwrap();
        let procedure = builder
            .add_combinational_procedure(ProcedureKind::Combinational, span())
            .unwrap();
        let header = builder.add_block(procedure, span()).unwrap();
        let body = builder.add_block(procedure, span()).unwrap();
        let internal = builder.add_block(procedure, span()).unwrap();
        let latch = builder.add_block(procedure, span()).unwrap();
        let exit = builder.add_block(procedure, span()).unwrap();
        builder
            .terminate_branch(header, condition, body, exit, span())
            .unwrap();
        builder
            .terminate_branch(body, condition, internal, latch, span())
            .unwrap();
        builder.terminate_jump(internal, body, span()).unwrap();
        builder.terminate_jump(latch, header, span()).unwrap();
        builder.terminate_return(exit, span()).unwrap();
        let region = builder
            .add_loop_region(LoopRegion {
                procedure,
                header,
                body,
                latch,
                exit,
                form: LoopForm::PreTest,
                parent: None,
                source: span(),
            })
            .unwrap();
        let graph = builder.seal().unwrap();

        let error = LoopBoundednessAnalysis::new(&graph, &word, LoopAnalysisLimits::default())
            .prove_exact(region)
            .unwrap_err();
        assert!(error.to_string().contains("internal cycle"));
    }
}
