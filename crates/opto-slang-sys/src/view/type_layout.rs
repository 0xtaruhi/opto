// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::required_str;
use crate::SlangError;
use crate::bridge::{read, read_invariant};
use crate::ffi;
use std::marker::PhantomData;
use std::ptr::NonNull;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Top-level shape of an elaborated type layout.
pub enum SlangTypeLayoutKind {
    /// Scalar or packed integral leaf.
    Scalar,
    /// Packed or unpacked array.
    Array,
    /// Packed or unpacked struct.
    Struct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Declared left and right bounds of an array dimension.
pub struct SlangIndexRange {
    /// Left source bound.
    pub left: i32,
    /// Right source bound.
    pub right: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Storage category of a `SystemVerilog` array dimension.
pub enum SlangArrayKind {
    /// Packed bit-level dimension.
    Packed,
    /// Unpacked aggregate dimension.
    Unpacked,
}

#[derive(Debug, Clone, Copy)]
/// Borrowed recursive view of an elaborated type's bit layout.
pub struct SlangTypeLayout<'a> {
    raw: NonNull<ffi::TypeLayout>,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangTypeLayout<'a> {
    pub(super) fn from_raw(raw: *const ffi::TypeLayout, context: &str) -> Result<Self, SlangError> {
        let raw = NonNull::new(raw.cast_mut()).ok_or_else(|| {
            SlangError::BridgeInvariant(format!("native slang bridge returned null {context}"))
        })?;
        Ok(Self {
            raw,
            _lifetime: PhantomData,
        })
    }

    pub(super) fn from_optional_raw(
        raw: *const ffi::TypeLayout,
    ) -> Result<Option<Self>, SlangError> {
        if raw.is_null() {
            Ok(None)
        } else {
            Self::from_raw(raw, "type layout").map(Some)
        }
    }

    fn view(self) -> Result<ffi::TypeLayoutView, SlangError> {
        // SAFETY: `raw` comes from the live snapshot and the bridge initializes the view on success.
        unsafe {
            read("type layout", |view| {
                ffi::opto_slang_type_layout_view(self.raw.as_ptr(), view)
            })
        }
    }

    /// Returns the top-level layout shape.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the layout pointer is null or
    /// the bridge reports an unknown layout tag.
    pub fn kind(self) -> Result<SlangTypeLayoutKind, SlangError> {
        Self::kind_from_raw(self.view()?.kind)
    }

    fn kind_from_raw(raw: std::ffi::c_int) -> Result<SlangTypeLayoutKind, SlangError> {
        match raw {
            ffi::TYPE_SCALAR => Ok(SlangTypeLayoutKind::Scalar),
            ffi::TYPE_ARRAY => Ok(SlangTypeLayoutKind::Array),
            ffi::TYPE_STRUCT => Ok(SlangTypeLayoutKind::Struct),
            raw => Err(SlangError::BridgeInvariant(format!(
                "native slang bridge returned unknown type layout kind {raw}"
            ))),
        }
    }

    #[must_use]
    /// Returns the flattened bit width.
    ///
    /// # Panics
    ///
    /// Panics if this borrowed layout no longer resolves through the live native snapshot.
    pub fn width(self) -> u32 {
        self.view()
            .expect("type layout pointer originates from a native aggregate view")
            .width
    }

    /// Returns the declared bounds of an array layout.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] unless this is a valid array layout.
    pub fn array_range(self) -> Result<SlangIndexRange, SlangError> {
        let view = self.require_kind(SlangTypeLayoutKind::Array, "array range")?;
        Ok(SlangIndexRange {
            left: view.array_left,
            right: view.array_right,
        })
    }

    /// Returns the element layout of an array.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] unless this is an array with a
    /// nonnull element layout.
    pub fn array_element(self) -> Result<Self, SlangError> {
        let view = self.require_kind(SlangTypeLayoutKind::Array, "array element")?;
        Self::from_raw(view.array_element, "array element type layout")
    }

    /// Returns whether an array dimension is packed or unpacked.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] unless this is a valid array layout.
    pub fn array_kind(self) -> Result<SlangArrayKind, SlangError> {
        let view = self.require_kind(SlangTypeLayoutKind::Array, "array kind")?;
        if view.array_is_packed != 0 {
            Ok(SlangArrayKind::Packed)
        } else {
            Ok(SlangArrayKind::Unpacked)
        }
    }

    /// Iterates over a struct's fields in declaration order.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] unless this is a valid struct layout.
    pub fn fields(self) -> Result<impl ExactSizeIterator<Item = SlangTypeField<'a>>, SlangError> {
        let view = self.require_kind(SlangTypeLayoutKind::Struct, "fields")?;
        Ok((0..view.field_count).map(move |index| {
            // SAFETY: `index` is bounded by the field count for this live struct layout.
            let view = unsafe {
                read_invariant("type field", |view| {
                    ffi::opto_slang_type_field_view(self.raw.as_ptr(), index, view)
                })
            };
            SlangTypeField {
                view,
                _lifetime: PhantomData,
            }
        }))
    }

    fn require_kind(
        self,
        expected: SlangTypeLayoutKind,
        operation: &str,
    ) -> Result<ffi::TypeLayoutView, SlangError> {
        let view = self.view()?;
        if Self::kind_from_raw(view.kind)? == expected {
            Ok(view)
        } else {
            Err(SlangError::BridgeInvariant(format!(
                "{operation} requested from incompatible type layout"
            )))
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// Borrowed view of one field in a struct layout.
pub struct SlangTypeField<'a> {
    view: ffi::TypeFieldView,
    _lifetime: PhantomData<&'a ()>,
}

impl<'a> SlangTypeField<'a> {
    /// Returns the source field name.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native field name is null or invalid UTF-8.
    pub fn name(self) -> Result<&'a str, SlangError> {
        // SAFETY: field-view strings are owned by the snapshot and remain live for `'a`.
        unsafe { required_str(self.view.name, "type layout field name") }
    }

    /// Returns the field's least-significant flattened bit offset.
    #[must_use]
    pub fn bit_offset(self) -> u32 {
        self.view.bit_offset
    }

    /// Returns the field's recursive type layout.
    ///
    /// # Errors
    ///
    /// Returns [`SlangError::BridgeInvariant`] if the native field layout pointer is null.
    pub fn layout(self) -> Result<SlangTypeLayout<'a>, SlangError> {
        SlangTypeLayout::from_raw(self.view.layout, "field type layout")
    }
}
