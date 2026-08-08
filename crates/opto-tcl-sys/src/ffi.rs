// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal raw bindings to the Tcl C API used by opto.
//!
//! The declarations mirror Tcl's public ABI. Safe code should use the owning
//! wrappers in the parent crate instead of retaining these pointers directly.

use std::ffi::{c_char, c_int, c_void};

/// Tcl command completed successfully.
pub const TCL_OK: c_int = 0;
/// Tcl command reported an error.
pub const TCL_ERROR: c_int = 1;
/// Tcl script requested a procedure return.
pub const TCL_RETURN: c_int = 2;
/// Tcl script requested a loop break.
pub const TCL_BREAK: c_int = 3;
/// Tcl script requested a loop continuation.
pub const TCL_CONTINUE: c_int = 4;
/// Token kind for a word that requires no substitutions.
pub const TCL_TOKEN_SIMPLE_WORD: c_int = 2;
/// Token kind for literal text.
pub const TCL_TOKEN_TEXT: c_int = 4;
/// Token kind for a nested command substitution.
pub const TCL_TOKEN_COMMAND: c_int = 16;

/// Number of token slots embedded in [`TclParse`].
pub const TCL_NUM_STATIC_TOKENS: usize = 20;

#[repr(C)]
#[derive(Debug)]
/// Opaque Tcl interpreter handle.
pub struct TclInterp {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug)]
/// Opaque Tcl value object.
pub struct TclObj {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
/// One token produced by `Tcl_ParseCommand`.
pub struct TclToken {
    /// Tcl token-kind discriminator.
    pub token_type: c_int,
    /// First byte of the token in the parsed input.
    pub start: *const c_char,
    /// Token length in bytes.
    pub size: c_int,
    /// Number of immediately following component tokens.
    pub num_components: c_int,
}

#[repr(C)]
#[derive(Debug)]
/// Parse state populated and released by the Tcl parser API.
pub struct TclParse {
    /// First byte of the leading comment, when present.
    pub comment_start: *const c_char,
    /// Leading comment length in bytes.
    pub comment_size: c_int,
    /// First byte of the parsed command.
    pub command_start: *const c_char,
    /// Parsed command length in bytes.
    pub command_size: c_int,
    /// Number of words in the command.
    pub num_words: c_int,
    /// Pointer to the token storage.
    pub token_ptr: *mut TclToken,
    /// Number of populated tokens.
    pub num_tokens: c_int,
    /// Total number of available token slots.
    pub tokens_available: c_int,
    /// Tcl parser error-kind discriminator.
    pub error_type: c_int,
    /// First byte of the complete input string.
    pub string: *const c_char,
    /// One-past-the-end pointer for the parsed range.
    pub end: *const c_char,
    /// Interpreter receiving parse diagnostics.
    pub interp: *mut TclInterp,
    /// Pointer at which parsing terminated.
    pub term: *const c_char,
    /// Nonzero when more input is required to complete the command.
    pub incomplete: c_int,
    /// Inline token storage used before Tcl allocates a larger array.
    pub static_tokens: [TclToken; TCL_NUM_STATIC_TOKENS],
}

/// ABI of a Tcl object command callback.
pub type TclObjCmdProc = extern "C" fn(
    client_data: *mut c_void,
    interp: *mut TclInterp,
    objc: c_int,
    objv: *const *mut TclObj,
) -> c_int;

unsafe extern "C" {
    /// Creates a new Tcl interpreter.
    pub fn Tcl_CreateInterp() -> *mut TclInterp;
    /// Initializes Tcl's standard commands in `interp`.
    pub fn Tcl_Init(interp: *mut TclInterp) -> c_int;
    /// Restricts `interp` to Tcl's safe command set.
    pub fn Tcl_MakeSafe(interp: *mut TclInterp) -> c_int;
    /// Destroys an interpreter created by `Tcl_CreateInterp`.
    pub fn Tcl_DeleteInterp(interp: *mut TclInterp);
    /// Initializes Tcl's executable-location bookkeeping.
    pub fn Tcl_FindExecutable(argv0: *const c_char);
    /// Evaluates a NUL-terminated Tcl script.
    pub fn Tcl_Eval(interp: *mut TclInterp, script: *const c_char) -> c_int;
    /// Evaluates a byte-counted Tcl script.
    pub fn Tcl_EvalEx(
        interp: *mut TclInterp,
        script: *const c_char,
        length: c_int,
        flags: c_int,
    ) -> c_int;
    /// Evaluates a Tcl object as a script.
    pub fn Tcl_EvalObjEx(interp: *mut TclInterp, object: *mut TclObj, flags: c_int) -> c_int;
    /// Evaluates a Tcl script file.
    pub fn Tcl_EvalFile(interp: *mut TclInterp, file_name: *const c_char) -> c_int;
    /// Returns the one-based source line of the latest interpreter error.
    pub fn Tcl_GetErrorLine(interp: *mut TclInterp) -> c_int;
    /// Tests whether a NUL-terminated string contains a complete Tcl command.
    pub fn Tcl_CommandComplete(command: *const c_char) -> c_int;
    /// Parses the first Tcl command in a byte range.
    pub fn Tcl_ParseCommand(
        interp: *mut TclInterp,
        start: *const c_char,
        num_bytes: c_int,
        nested: c_int,
        parse: *mut TclParse,
    ) -> c_int;
    /// Releases dynamic storage owned by a completed parse state.
    pub fn Tcl_FreeParse(parse: *mut TclParse);
    /// Registers an object command in an interpreter.
    pub fn Tcl_CreateObjCommand(
        interp: *mut TclInterp,
        cmd_name: *const c_char,
        proc: TclObjCmdProc,
        client_data: *mut c_void,
        delete_proc: Option<extern "C" fn(*mut c_void)>,
    ) -> *mut c_void;
    /// Finds an existing command in the current namespace.
    pub fn Tcl_FindCommand(
        interp: *mut TclInterp,
        name: *const c_char,
        namespace: *mut c_void,
        flags: c_int,
    ) -> *mut c_void;
    /// Returns an object's NUL-terminated string representation.
    pub fn Tcl_GetString(obj: *mut TclObj) -> *const c_char;
    /// Returns an object's string bytes and length.
    pub fn Tcl_GetStringFromObj(obj: *mut TclObj, length: *mut c_int) -> *const c_char;
    /// Borrows the interpreter's current result object.
    pub fn Tcl_GetObjResult(interp: *mut TclInterp) -> *mut TclObj;
    /// Creates a Tcl string object.
    pub fn Tcl_NewStringObj(bytes: *const c_char, length: c_int) -> *mut TclObj;
    /// Creates a Tcl list object.
    pub fn Tcl_NewListObj(count: c_int, elements: *const *mut TclObj) -> *mut TclObj;
    /// Appends one element to a Tcl list object.
    pub fn Tcl_ListObjAppendElement(
        interp: *mut TclInterp,
        list: *mut TclObj,
        element: *mut TclObj,
    ) -> c_int;
    /// Replaces the interpreter's result object.
    pub fn Tcl_SetObjResult(interp: *mut TclInterp, obj: *mut TclObj);
    /// Borrows a Tcl list object's element array.
    pub fn Tcl_ListObjGetElements(
        interp: *mut TclInterp,
        list: *mut TclObj,
        count: *mut c_int,
        elements: *mut *mut *mut TclObj,
    ) -> c_int;
    /// Sets a scalar Tcl variable from a NUL-terminated string.
    pub fn Tcl_SetVar(
        interp: *mut TclInterp,
        var_name: *const c_char,
        new_value: *const c_char,
        flags: c_int,
    ) -> *const c_char;
    /// Sets a scalar or array-element variable from a Tcl object.
    pub fn Tcl_SetVar2Ex(
        interp: *mut TclInterp,
        name1: *const c_char,
        name2: *const c_char,
        value: *mut TclObj,
        flags: c_int,
    ) -> *mut TclObj;
    /// Gets a scalar or array-element variable as a Tcl object.
    pub fn Tcl_GetVar2Ex(
        interp: *mut TclInterp,
        name1: *const c_char,
        name2: *const c_char,
        flags: c_int,
    ) -> *mut TclObj;

    pub(crate) fn opto_tcl_vfs_register() -> c_int;
}
