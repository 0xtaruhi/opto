// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Source-facing aggregate layouts for flattened word-level signals.
//!
//! Synthesis uses flat bit vectors, while diagnostics and writers must preserve
//! packed ranges, unpacked indices, and structure field names. This module
//! interns recursive layouts into the module arena and provides both allocating
//! expansion and allocation-free traversal.

use super::{SignalId, TypeLayoutId, WordError, WordModule};
use crate::NameId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// Source-facing array range with preserved declaration orientation.
pub struct IndexRange {
    /// Left endpoint as written in the source declaration.
    pub left: i32,
    /// Right endpoint as written in the source declaration.
    pub right: i32,
}

impl IndexRange {
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] if the inclusive width exceeds 32-bit capacity.
    pub fn width(self) -> Result<u32, WordError> {
        self.left
            .abs_diff(self.right)
            .checked_add(1)
            .ok_or_else(|| WordError::new("type layout range width exceeds 32-bit capacity"))
    }

    /// Converts an offset from the least-significant end into a source index.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when `position` is outside the range or index
    /// arithmetic overflows.
    pub fn index_from_lsb(self, position: u32) -> Result<i32, WordError> {
        if position >= self.width()? {
            return Err(WordError::new(format!(
                "type layout index position {position} exceeds range [{}:{}]",
                self.left, self.right
            )));
        }
        let position = i32::try_from(position)
            .map_err(|_| WordError::new("type layout index exceeds signed 32-bit capacity"))?;
        if self.left >= self.right {
            self.right
                .checked_add(position)
                .ok_or_else(|| WordError::new("type layout index overflow"))
        } else {
            self.right
                .checked_sub(position)
                .ok_or_else(|| WordError::new("type layout index overflow"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Recursive, source-facing description of a signal's aggregate bit layout.
pub enum TypeLayoutSpec {
    /// One scalar bit.
    Scalar,
    /// Packed or unpacked array.
    Array {
        /// Storage relationship between adjacent elements.
        kind: ArrayKind,
        /// Source indices and declaration orientation.
        range: IndexRange,
        /// Layout of one array element.
        element: Box<TypeLayoutSpec>,
    },
    /// Aggregate with explicitly positioned fields.
    Struct {
        /// Fields and their least-significant bit offsets.
        fields: Vec<TypeLayoutFieldSpec>,
    },
}

impl TypeLayoutSpec {
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when an array or structure width exceeds 32-bit
    /// capacity or fields overlap.
    pub fn width(&self) -> Result<u32, WordError> {
        match self {
            Self::Scalar => Ok(1),
            Self::Array { range, element, .. } => range
                .width()?
                .checked_mul(element.width()?)
                .ok_or_else(|| WordError::new("array type layout width exceeds 32-bit capacity")),
            Self::Struct { fields } => struct_width(fields),
        }
    }

    #[must_use]
    /// Returns whether this layout contains an unpacked array at any depth.
    pub fn contains_unpacked_array(&self) -> bool {
        match self {
            Self::Scalar => false,
            Self::Array { kind, element, .. } => {
                *kind == ArrayKind::Unpacked || element.contains_unpacked_array()
            }
            Self::Struct { fields } => fields
                .iter()
                .any(|field| field.layout.contains_unpacked_array()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
/// Packed or unpacked relationship between array elements.
pub enum ArrayKind {
    /// Elements occupy one contiguous packed bit vector.
    Packed,
    /// Elements retain separate source-level aggregate identity.
    Unpacked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Named structure field in a [`TypeLayoutSpec`].
pub struct TypeLayoutFieldSpec {
    /// Source-facing field name.
    pub name: String,
    /// Least-significant bit offset within the containing structure.
    pub bit_offset: u32,
    /// Recursive field layout.
    pub layout: TypeLayoutSpec,
}

/// A pre-order event from the compact type-layout arena.
///
/// Unlike [`TypeLayoutSpec`], this view borrows the arena and does not allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeLayoutEvent<'a> {
    /// Visits one scalar bit.
    Scalar,
    /// Begins an array node.
    Array {
        /// Packed or unpacked element relationship.
        kind: ArrayKind,
        /// Source index range.
        range: IndexRange,
    },
    /// Begins a structure node.
    Struct {
        /// Number of fields that immediately follow.
        field_count: usize,
    },
    /// Begins one named structure field.
    Field {
        /// Borrowed field name.
        name: &'a str,
        /// Least-significant bit offset in the containing structure.
        bit_offset: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
/// Outcome of a non-allocating type-layout traversal.
pub enum TypeLayoutTraversal {
    /// The signal has no source-facing layout metadata.
    Absent,
    /// The complete layout was visited.
    Complete,
    /// The signal or an arena reference was invalid.
    Invalid = 255,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TypeLayout {
    pub(crate) width: u32,
    pub(crate) kind: TypeLayoutKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TypeLayoutKind {
    Scalar,
    Array {
        kind: ArrayKind,
        range: IndexRange,
        element: TypeLayoutId,
    },
    Struct {
        fields: Vec<TypeLayoutField>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TypeLayoutField {
    pub(crate) name: NameId,
    pub(crate) bit_offset: u32,
    pub(crate) layout: TypeLayoutId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// One source-facing selector identifying a flattened signal bit.
pub enum TypeSelector {
    /// Selects a named structure field.
    Field(NameId),
    /// Selects an array index.
    Index(i32),
}

impl WordModule {
    /// Attaches a source-facing aggregate layout to `signal`.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the signal is unknown, the layout is invalid,
    /// or its flattened width differs from the signal width.
    pub fn set_signal_type_layout(
        &mut self,
        signal: SignalId,
        spec: &TypeLayoutSpec,
    ) -> Result<(), WordError> {
        let signal_width = self
            .signal(signal)
            .ok_or_else(|| WordError::new(format!("unknown RTL signal {signal:?}")))?
            .ty
            .width();
        let layout_width = spec.width()?;
        if layout_width != signal_width {
            return Err(WordError::new(format!(
                "type layout width {layout_width} does not match signal width {signal_width}"
            )));
        }
        let layout = self.intern_type_layout(spec)?;
        self.signals[signal.index()].type_layout = Some(layout);
        Ok(())
    }

    /// Expands a signal's compact layout into an owned recursive specification.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the signal or a referenced layout node is
    /// invalid.
    pub fn signal_type_layout_spec(
        &self,
        signal: SignalId,
    ) -> Result<Option<TypeLayoutSpec>, WordError> {
        let signal = self
            .signal(signal)
            .ok_or_else(|| WordError::new(format!("unknown RTL signal {signal:?}")))?;
        signal
            .type_layout
            .map(|layout| self.expand_type_layout(layout))
            .transpose()
    }

    /// Visits a signal's type layout without expanding its compact arena nodes.
    pub fn visit_signal_type_layout<'a>(
        &'a self,
        signal: SignalId,
        mut visit: impl FnMut(TypeLayoutEvent<'a>),
    ) -> TypeLayoutTraversal {
        let Some(signal) = self.signal(signal) else {
            return TypeLayoutTraversal::Invalid;
        };
        let Some(layout) = signal.type_layout else {
            return TypeLayoutTraversal::Absent;
        };
        if self.visit_type_layout(layout, &mut visit) {
            TypeLayoutTraversal::Complete
        } else {
            TypeLayoutTraversal::Invalid
        }
    }

    /// Returns the aggregate selectors that identify one flattened signal bit.
    ///
    /// Selectors are ordered from outermost aggregate to innermost element.
    /// `Ok(None)` means that the signal has no attached layout.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the signal is unknown, `bit_offset` is out of
    /// range, or the stored layout is invalid.
    pub fn signal_bit_selectors(
        &self,
        signal: SignalId,
        bit_offset: u32,
    ) -> Result<Option<Vec<TypeSelector>>, WordError> {
        let signal = self
            .signal(signal)
            .ok_or_else(|| WordError::new(format!("unknown RTL signal {signal:?}")))?;
        if bit_offset >= signal.ty.width() {
            return Err(WordError::new(format!(
                "bit offset {bit_offset} exceeds signal width {}",
                signal.ty.width()
            )));
        }
        let Some(layout) = signal.type_layout else {
            return Ok(None);
        };
        let mut selectors = Vec::new();
        self.collect_bit_selectors(layout, bit_offset, &mut selectors)?;
        Ok(Some(selectors))
    }

    /// Returns the source index range for a one-dimensional packed bit vector.
    ///
    /// More complex layouts and signals without layout metadata return
    /// `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns [`WordError`] when the signal or compact layout is invalid.
    pub fn signal_simple_packed_range(
        &self,
        signal: SignalId,
    ) -> Result<Option<IndexRange>, WordError> {
        let signal = self
            .signal(signal)
            .ok_or_else(|| WordError::new(format!("unknown RTL signal {signal:?}")))?;
        let Some(layout) = signal.type_layout else {
            return Ok(None);
        };
        let layout = self.type_layout(layout)?;
        let TypeLayoutKind::Array {
            kind: ArrayKind::Packed,
            range,
            element,
        } = layout.kind
        else {
            return Ok(None);
        };
        if matches!(self.type_layout(element)?.kind, TypeLayoutKind::Scalar) {
            Ok(Some(range))
        } else {
            Ok(None)
        }
    }

    fn intern_type_layout(&mut self, spec: &TypeLayoutSpec) -> Result<TypeLayoutId, WordError> {
        let kind = match spec {
            TypeLayoutSpec::Scalar => TypeLayoutKind::Scalar,
            TypeLayoutSpec::Array {
                kind,
                range,
                element,
            } => TypeLayoutKind::Array {
                kind: *kind,
                range: *range,
                element: self.intern_type_layout(element)?,
            },
            TypeLayoutSpec::Struct { fields } => {
                validate_struct_fields(fields)?;
                let mut lowered = Vec::with_capacity(fields.len());
                for field in fields {
                    lowered.push(TypeLayoutField {
                        name: self.intern_name(&field.name)?,
                        bit_offset: field.bit_offset,
                        layout: self.intern_type_layout(&field.layout)?,
                    });
                }
                TypeLayoutKind::Struct { fields: lowered }
            }
        };
        let layout = TypeLayout {
            width: spec.width()?,
            kind,
        };
        if let Some(index) = self.type_layouts.iter().position(|entry| entry == &layout) {
            return TypeLayoutId::from_index(index);
        }
        let id = TypeLayoutId::from_index(self.type_layouts.len())?;
        self.type_layouts.push(layout);
        Ok(id)
    }

    fn expand_type_layout(&self, id: TypeLayoutId) -> Result<TypeLayoutSpec, WordError> {
        let layout = self.type_layout(id)?;
        match &layout.kind {
            TypeLayoutKind::Scalar => Ok(TypeLayoutSpec::Scalar),
            TypeLayoutKind::Array {
                kind,
                range,
                element,
            } => Ok(TypeLayoutSpec::Array {
                kind: *kind,
                range: *range,
                element: Box::new(self.expand_type_layout(*element)?),
            }),
            TypeLayoutKind::Struct { fields } => Ok(TypeLayoutSpec::Struct {
                fields: fields
                    .iter()
                    .map(|field| {
                        Ok(TypeLayoutFieldSpec {
                            name: self.name_str(field.name).to_string(),
                            bit_offset: field.bit_offset,
                            layout: self.expand_type_layout(field.layout)?,
                        })
                    })
                    .collect::<Result<Vec<_>, WordError>>()?,
            }),
        }
    }

    fn visit_type_layout<'a>(
        &'a self,
        id: TypeLayoutId,
        visit: &mut impl FnMut(TypeLayoutEvent<'a>),
    ) -> bool {
        let Some(layout) = self.type_layouts.get(id.index()) else {
            return false;
        };
        match &layout.kind {
            TypeLayoutKind::Scalar => visit(TypeLayoutEvent::Scalar),
            TypeLayoutKind::Array {
                kind,
                range,
                element,
            } => {
                visit(TypeLayoutEvent::Array {
                    kind: *kind,
                    range: *range,
                });
                if !self.visit_type_layout(*element, visit) {
                    return false;
                }
            }
            TypeLayoutKind::Struct { fields } => {
                visit(TypeLayoutEvent::Struct {
                    field_count: fields.len(),
                });
                for field in fields {
                    let Some(name) = self.resolve_name(field.name) else {
                        return false;
                    };
                    visit(TypeLayoutEvent::Field {
                        name,
                        bit_offset: field.bit_offset,
                    });
                    if !self.visit_type_layout(field.layout, visit) {
                        return false;
                    }
                }
            }
        }
        true
    }

    fn type_layout(&self, id: TypeLayoutId) -> Result<&TypeLayout, WordError> {
        self.type_layouts
            .get(id.index())
            .ok_or_else(|| WordError::new(format!("unknown type layout {id:?}")))
    }

    fn collect_bit_selectors(
        &self,
        id: TypeLayoutId,
        bit_offset: u32,
        selectors: &mut Vec<TypeSelector>,
    ) -> Result<(), WordError> {
        let layout = self.type_layout(id)?;
        if bit_offset >= layout.width {
            return Err(WordError::new(format!(
                "bit offset {bit_offset} exceeds type layout width {}",
                layout.width
            )));
        }
        match &layout.kind {
            TypeLayoutKind::Scalar => Ok(()),
            TypeLayoutKind::Array { range, element, .. } => {
                let element_width = self.type_layout(*element)?.width;
                let position = bit_offset / element_width;
                selectors.push(TypeSelector::Index(range.index_from_lsb(position)?));
                self.collect_bit_selectors(*element, bit_offset % element_width, selectors)
            }
            TypeLayoutKind::Struct { fields } => {
                let mut selected = None;
                for field in fields {
                    let field_layout = self.type_layout(field.layout)?;
                    if bit_offset >= field.bit_offset
                        && bit_offset - field.bit_offset < field_layout.width
                    {
                        selected = Some(field);
                        break;
                    }
                }
                let field = selected.ok_or_else(|| {
                    WordError::new(format!(
                        "bit offset {bit_offset} is not covered by a type layout field"
                    ))
                })?;
                selectors.push(TypeSelector::Field(field.name));
                self.collect_bit_selectors(field.layout, bit_offset - field.bit_offset, selectors)
            }
        }
    }
}

fn struct_width(fields: &[TypeLayoutFieldSpec]) -> Result<u32, WordError> {
    validate_struct_fields(fields)?;
    fields.iter().try_fold(0, |width, field| {
        field
            .bit_offset
            .checked_add(field.layout.width()?)
            .map(|end| width.max(end))
            .ok_or_else(|| WordError::new("struct type layout width exceeds 32-bit capacity"))
    })
}

fn validate_struct_fields(fields: &[TypeLayoutFieldSpec]) -> Result<(), WordError> {
    if fields.is_empty() {
        return Err(WordError::new("struct type layout must contain a field"));
    }
    let mut spans = Vec::with_capacity(fields.len());
    let mut names = BTreeSet::new();
    for field in fields {
        if field.name.is_empty() {
            return Err(WordError::new("type layout field name cannot be empty"));
        }
        if !names.insert(&field.name) {
            return Err(WordError::new(format!(
                "duplicate type layout field '{}'",
                field.name
            )));
        }
        let end = field
            .bit_offset
            .checked_add(field.layout.width()?)
            .ok_or_else(|| WordError::new("struct type layout field exceeds 32-bit capacity"))?;
        spans.push((field.bit_offset, end));
    }
    spans.sort_unstable();
    if spans[0].0 != 0 || spans.windows(2).any(|pair| pair[0].1 != pair[1].0) {
        return Err(WordError::new(
            "struct type layout fields must be contiguous and non-overlapping",
        ));
    }
    Ok(())
}
