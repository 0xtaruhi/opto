// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Audited conversion helpers for native out-parameters and borrowed arrays.

use crate::{SlangError, ffi};
use std::mem::MaybeUninit;

/// Reads a native out-parameter only after the bridge reports initialization.
///
/// # Safety
///
/// On an `ffi::OK` return, `fill` must have initialized the complete `T` at the
/// supplied pointer. It must not retain that pointer after returning.
pub(crate) unsafe fn read<T>(
    context: &str,
    fill: impl FnOnce(*mut T) -> std::ffi::c_int,
) -> Result<T, SlangError> {
    let mut value = MaybeUninit::uninit();
    if fill(value.as_mut_ptr()) != ffi::OK {
        return Err(SlangError::BridgeInvariant(format!(
            "native slang bridge could not read {context}"
        )));
    }
    // SAFETY: a successful bridge status guarantees the out parameter was fully initialized.
    Ok(unsafe { value.assume_init() })
}

/// Reads an out-parameter whose failure would violate the native bridge ABI.
///
/// # Safety
///
/// This has the same initialization requirements as [`read`]. The caller must
/// additionally establish that a non-success status is impossible for valid
/// bridge state.
pub(crate) unsafe fn read_invariant<T>(
    context: &str,
    fill: impl FnOnce(*mut T) -> std::ffi::c_int,
) -> T {
    // SAFETY: this wrapper preserves `read`'s requirement that `fill` initialize on success.
    unsafe { read(context, fill) }
        .unwrap_or_else(|error| panic!("native slang bridge invariant failed: {error}"))
}

/// Returns one pointer from a native pointer array.
///
/// # Safety
///
/// `base` must reference an array containing at least `index + 1` initialized
/// pointer elements whose pointees remain valid for the caller's use.
pub(crate) unsafe fn pointer_element<T>(
    base: *const *const T,
    index: usize,
    context: &str,
) -> *const T {
    assert!(
        !base.is_null(),
        "native slang bridge returned null {context} storage"
    );
    // SAFETY: the caller supplies an index below the native view's associated element count.
    unsafe { *base.add(index) }
}
