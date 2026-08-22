// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Typed word-level dataflow, signals, memories, and instances.
//!
//! [`WordModule`] stores values and operations in dense arenas and keeps source
//! signals, connects, memories, and child instances as explicit structural
//! objects. Every operation checks operand and result types at insertion time;
//! [`WordModule::validate`] rechecks the complete graph at publication and
//! checkpoint boundaries.
//!
//! Vector fragments are least-significant-bit first internally even when their
//! source ranges descend or use non-zero indices. Source-facing
//! [`TypeLayoutSpec`] metadata retains those indices for stable names and
//! reports. Linked elaboration returns a [`NetlistRemap`] rather than leaving
//! IDs from child arenas embedded in the root.

use crate::NameError;
use opto_core::DenseId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod compact;
pub use compact::NetlistRemap;
mod linked_elaboration;
pub use linked_elaboration::elaborate_linked_root;
pub(crate) use linked_elaboration::{ModuleRemap, SignalBindingOffset, elaborate_linked_root_with};
mod validate;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
/// Construction or validation failure in word-level IR.
pub enum WordError {
    /// A graph, type, range, or ownership invariant is violated.
    #[error("{0}")]
    Invariant(String),
    /// A name cannot be interned or resolved.
    #[error(transparent)]
    Name(#[from] NameError),
}

impl WordError {
    fn new(message: impl Into<String>) -> Self {
        Self::Invariant(message.into())
    }
}

macro_rules! define_dense_id {
    ($name:ident, $tag:ident, $kind:literal) => {
        enum $tag {}

        #[doc = concat!("Dense ", $kind, " identifier local to one [`WordModule`].")]
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
            /// Returns [`WordError::Invariant`] when `index` exceeds the
            /// nonzero 32-bit encoding used by word-level arena references.
            pub fn from_index(index: usize) -> Result<Self, WordError> {
                DenseId::from_index(index)
                    .map(Self)
                    .map_err(|_| WordError::new(concat!($kind, " ID exceeds 32-bit capacity")))
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

define_dense_id!(ModuleId, ModuleTag, "module");
define_dense_id!(PortId, PortTag, "port");
define_dense_id!(SignalId, SignalTag, "signal");
define_dense_id!(TypeLayoutId, TypeLayoutTag, "type layout");
define_dense_id!(ValueId, ValueTag, "value");
define_dense_id!(OpId, OpTag, "operation");
define_dense_id!(InstId, InstanceTag, "instance");
define_dense_id!(MemoryId, MemoryTag, "memory");
define_dense_id!(MemoryReadPortId, MemoryReadPortTag, "memory read port");
define_dense_id!(MemoryWritePortId, MemoryWritePortTag, "memory write port");

mod annotation;
pub use annotation::{
    Annotation, AnnotationTarget, AnnotationValue, AnnotationValueSpec, DefinitionKind,
    SynthesisDirective, SynthesisDirectiveKind,
};

mod model;
pub use model::*;

mod layout;
pub use layout::*;

mod range;
pub use range::{
    UnsignedValueAnalysis, UnsignedValueRange, unsigned_value_alignment, unsigned_value_range,
};
mod known_bits;
pub use known_bits::{KnownBit, KnownBits128, KnownBitsAnalysis};

mod module;
pub use module::{
    FragmentKey, PublicationWave, PublishedFragment, PublishedWave, SpeculationCheckpoint,
    WordFragment, WordFragmentBuilder, WordModule,
};

#[cfg(test)]
mod tests;
