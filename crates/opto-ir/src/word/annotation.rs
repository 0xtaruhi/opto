// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Sparse source annotations and typed definition semantics.

use super::{
    InstId, MemoryId, MemoryReadPortId, MemoryWritePortId, OpId, PortId, SignalId, SourceSpan,
    ValueId,
};
use crate::{ConstBits, NameId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Semantic classification of one RTL definition.
pub enum DefinitionKind {
    /// Definition contains synthesizable RTL behavior.
    #[default]
    Synthesizable,
    /// Definition is an external interface leaf whose body must not be synthesized.
    BlackBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Typed owner of a sparse source annotation.
pub enum AnnotationTarget {
    /// Whole module definition.
    Module,
    /// Module port.
    Port(PortId),
    /// Signal declaration.
    Signal(SignalId),
    /// Memory declaration.
    Memory(MemoryId),
    /// Memory read port.
    MemoryReadPort(MemoryReadPortId),
    /// Memory write port.
    MemoryWritePort(MemoryWritePortId),
    /// Word value.
    Value(ValueId),
    /// Word operation.
    Operation(OpId),
    /// Child instance.
    Instance(InstId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
/// Optimization semantics decoded from source annotations or Tcl attributes.
pub enum SynthesisDirectiveKind {
    /// Prevent the selected design object from being modified or replaced.
    DontTouch,
    /// Hierarchy collapsing control. `false` preserves hierarchy.
    Ungroup,
    /// Preserve a named signal as an optimization boundary.
    KeepSignal,
    /// Mark a signal as an asynchronous clock-domain crossing register.
    AsyncRegister,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One sparse, strongly typed synthesis directive.
pub struct SynthesisDirective {
    /// Annotated IR object.
    pub target: AnnotationTarget,
    /// Directive semantics.
    pub kind: SynthesisDirectiveKind,
    /// Whether the directive is enabled.
    pub enabled: bool,
    /// Source location of the directive.
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Compact stored payload for an evaluated source annotation.
pub enum AnnotationValue {
    /// Evaluated four-state integer.
    Integer {
        /// Interned most-significant-first bit text.
        bits: NameId,
        /// Evaluated integer width.
        width: u32,
        /// Whether the integer uses signed interpretation.
        signed: bool,
    },
    /// Interned evaluated string.
    String(NameId),
    /// Canonical text for evaluated constant forms not structurally represented yet.
    Other(NameId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Owned payload accepted while constructing a source annotation.
pub enum AnnotationValueSpec {
    /// Owned four-state integer.
    Integer {
        /// Most-significant-first bit value.
        bits: ConstBits,
        /// Whether the integer uses signed interpretation.
        signed: bool,
    },
    /// Owned string value.
    String(String),
    /// Canonical text for another constant form.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One source annotation stored in deterministic insertion order.
pub struct Annotation {
    /// Annotated IR object.
    pub target: AnnotationTarget,
    /// Interned annotation name.
    pub name: NameId,
    /// Evaluated annotation payload.
    pub value: AnnotationValue,
    /// Source location of the annotation.
    pub source: SourceSpan,
}
