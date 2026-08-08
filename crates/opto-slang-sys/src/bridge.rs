// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{SlangError, ffi};
use std::mem::MaybeUninit;

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

pub(crate) unsafe fn read_invariant<T>(
    context: &str,
    fill: impl FnOnce(*mut T) -> std::ffi::c_int,
) -> T {
    // SAFETY: this wrapper preserves `read`'s requirement that `fill` initialize on success.
    unsafe { read(context, fill) }
        .unwrap_or_else(|error| panic!("native slang bridge invariant failed: {error}"))
}

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
