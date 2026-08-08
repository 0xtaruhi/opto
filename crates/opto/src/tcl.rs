// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    unsafe_code,
    reason = "this module is the audited adapter between safe shell code and Tcl's C callback ABI"
)]

use crate::command::{CommandResult, EvalResult, dispatch};
use crate::command_catalog::{self, RegisteredCommand};
use crate::runtime::ShellState;
use opto_tcl_sys::ffi::{
    TCL_ERROR, TCL_OK, TCL_RETURN, Tcl_CommandComplete, Tcl_CreateObjCommand, Tcl_EvalEx,
    Tcl_EvalFile, Tcl_GetErrorLine, Tcl_GetObjResult, Tcl_GetStringFromObj, Tcl_GetVar2Ex,
    Tcl_ListObjAppendElement, Tcl_ListObjGetElements, Tcl_MakeSafe, Tcl_NewListObj,
    Tcl_NewStringObj, Tcl_SetObjResult, Tcl_SetVar2Ex, TclInterp, TclObj,
};
use std::ffi::{CStr, CString, c_int, c_void};
use std::ops::Deref;
use std::path::Path;

#[derive(Clone, Copy)]
pub(super) struct TclArg<'a> {
    pub(super) object: *mut TclObj,
    text: &'a str,
}

impl TclArg<'_> {
    pub(super) fn as_str(&self) -> &str {
        self.text
    }
}

impl AsRef<str> for TclArg<'_> {
    fn as_ref(&self) -> &str {
        self.text
    }
}

impl Deref for TclArg<'_> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.text
    }
}

impl std::fmt::Display for TclArg<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.text)
    }
}

pub(super) fn register_command_specs<'a>(
    interp: *mut TclInterp,
    state: &ShellState,
    commands: impl IntoIterator<Item = &'a RegisteredCommand>,
) -> Result<(), crate::ShellError> {
    let client_data = std::ptr::from_ref(state).cast_mut().cast::<c_void>();
    for command in commands {
        let spec = command.spec();
        let name = CString::new(spec.name).map_err(|source| crate::ShellError::Nul {
            context: "Tcl command name",
            source,
        })?;
        // SAFETY: Tcl owns the live interpreter, the command name is NUL-terminated, and `state`
        // outlives every registered command because commands are deleted with the interpreter.
        let token = unsafe {
            Tcl_CreateObjCommand(interp, name.as_ptr(), command_trampoline, client_data, None)
        };
        if token.is_null() {
            return Err(crate::ShellError::command(format!(
                "failed to register Tcl command '{}'",
                spec.name
            )));
        }
    }
    Ok(())
}

pub(super) fn register_validation_command_specs<'a>(
    interp: *mut TclInterp,
    commands: impl IntoIterator<Item = &'a RegisteredCommand>,
) -> Result<(), crate::ShellError> {
    for command in commands {
        let spec = command.spec();
        let name = CString::new(spec.name).map_err(|source| crate::ShellError::Nul {
            context: "Tcl command name",
            source,
        })?;
        let client_data = std::ptr::from_ref(command).cast_mut().cast::<c_void>();
        // SAFETY: Tcl owns the live interpreter, `command` outlives it, and the command name is
        // NUL-terminated for the duration of registration.
        let token = unsafe {
            Tcl_CreateObjCommand(
                interp,
                name.as_ptr(),
                validation_command_trampoline,
                client_data,
                None,
            )
        };
        if token.is_null() {
            return Err(crate::ShellError::command(format!(
                "failed to register SDC validator for '{}'",
                spec.name
            )));
        }
    }
    Ok(())
}

pub(super) fn eval_result(
    state: &ShellState,
    interp: *mut TclInterp,
    code: c_int,
) -> Result<EvalResult, crate::ShellError> {
    // SAFETY: callers pass the live interpreter that produced `code`.
    let result = tcl_result(interp);
    let pending = state.pending_command_error.borrow_mut().take();
    if let Some(exit_code) = *state.exit_code.borrow() {
        Ok(EvalResult::Exit(exit_code))
    } else if code == TCL_OK {
        Ok(EvalResult::Complete(result))
    } else if let Some((pending_result, error)) = pending
        && pending_result == result
    {
        Err(error)
    } else {
        Err(crate::ShellError::command(result))
    }
}

pub(super) fn path_to_cstring(path: &Path) -> Result<CString, crate::ShellError> {
    let text = path.to_str().ok_or_else(|| {
        crate::ShellError::parse(format!("{}: path is not valid UTF-8", path.display()))
    })?;
    CString::new(text).map_err(|source| crate::ShellError::Nul {
        context: "Tcl path",
        source,
    })
}

pub(super) fn split_tcl_list(
    interp: *mut TclInterp,
    raw: &TclArg<'_>,
) -> Result<Vec<String>, crate::ShellError> {
    let mut count = 0;
    let mut elements = std::ptr::null_mut();
    // SAFETY: `interp` and `raw.object` are live; both out parameters are valid for this call.
    let code =
        unsafe { Tcl_ListObjGetElements(interp, raw.object, &raw mut count, &raw mut elements) };
    if code != TCL_OK {
        // SAFETY: `interp` is live and owns the result set by the failed list conversion.
        let result = tcl_result(interp);
        return Err(crate::ShellError::command(result));
    }

    let count = usize::try_from(count)
        .map_err(|_| crate::ShellError::command("Tcl returned a negative list length"))?;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        // SAFETY: Tcl returned an array of `count` object pointers that remains live until mutation.
        let object = unsafe { *elements.add(index) };
        // SAFETY: every element returned by Tcl is a live object for the duration of this function.
        values.push(unsafe { tcl_object_text(object) }?.to_owned());
    }
    Ok(values)
}

pub(super) fn set_tcl_var(
    interp: *mut TclInterp,
    name: &str,
    value: &str,
) -> Result<(), crate::ShellError> {
    let c_name = CString::new(name).map_err(|source| crate::ShellError::Nul {
        context: "Tcl variable name",
        source,
    })?;
    let value_length = c_int::try_from(value.len()).map_err(|_| {
        crate::ShellError::parse(format!("Tcl variable '{name}' value is too large"))
    })?;
    // SAFETY: the byte pointer is valid for `value_length`; Tcl copies it into a new managed object.
    let object = unsafe { Tcl_NewStringObj(value.as_ptr().cast(), value_length) };
    // SAFETY: the interpreter, name, and newly allocated Tcl object are live for the call.
    let result = unsafe { Tcl_SetVar2Ex(interp, c_name.as_ptr(), std::ptr::null(), object, 0) };
    if result.is_null() {
        return Err(crate::ShellError::command(format!(
            "failed to set Tcl variable '{name}'"
        )));
    }
    Ok(())
}

pub(super) fn get_tcl_var(
    interp: *mut TclInterp,
    name: &str,
) -> Result<Option<String>, crate::ShellError> {
    let c_name = CString::new(name).map_err(|source| crate::ShellError::Nul {
        context: "Tcl variable name",
        source,
    })?;
    // SAFETY: the interpreter is live and `c_name` is a valid NUL-terminated variable name.
    let object = unsafe { Tcl_GetVar2Ex(interp, c_name.as_ptr(), std::ptr::null(), 0) };
    if object.is_null() {
        return Ok(None);
    }
    // SAFETY: a non-null object returned by Tcl is live until the interpreter is mutated.
    Ok(Some(unsafe { tcl_object_text(object) }?.to_owned()))
}

unsafe fn collect_args<'a>(
    objc: c_int,
    objv: *const *mut TclObj,
) -> Result<Vec<TclArg<'a>>, crate::ShellError> {
    let argument_count = usize::try_from(objc).map_err(|_| {
        crate::ShellError::command("Tcl callback received a negative argument count")
    })?;
    let mut args = Vec::with_capacity(argument_count);
    for index in 0..argument_count {
        // SAFETY: the Tcl callback contract provides `objc` live entries in `objv`.
        let object = unsafe { *objv.add(index) };
        // SAFETY: each callback argument object remains live for the callback duration.
        let text = unsafe { tcl_object_text(object) }?;
        args.push(TclArg { object, text });
    }
    Ok(args)
}

fn set_error(interp: *mut TclInterp, message: &str) -> c_int {
    set_result(interp, message);
    TCL_ERROR
}

fn set_command_error(
    state: &ShellState,
    interp: *mut TclInterp,
    error: crate::ShellError,
) -> c_int {
    let message = error.to_string();
    state
        .pending_command_error
        .replace(Some((message.clone(), error)));
    set_error(interp, &message)
}

fn set_result(interp: *mut TclInterp, message: &str) {
    let Ok(length) = c_int::try_from(message.len()) else {
        return set_result(interp, "Tcl result exceeds the supported size");
    };
    // SAFETY: `interp` is live, the message pointer matches `length`, and Tcl copies the bytes.
    unsafe {
        let obj = Tcl_NewStringObj(message.as_ptr().cast(), length);
        Tcl_SetObjResult(interp, obj);
    }
}

fn set_list_result(interp: *mut TclInterp, values: &[String]) -> Result<(), crate::ShellError> {
    // SAFETY: a zero-element Tcl list accepts a null element array and returns a managed object.
    let list = unsafe { Tcl_NewListObj(0, std::ptr::null()) };
    if list.is_null() {
        return Err(crate::ShellError::command(
            "failed to allocate Tcl list result",
        ));
    }
    for value in values {
        let length = c_int::try_from(value.len())
            .map_err(|_| crate::ShellError::parse("Tcl list element is too large"))?;
        // SAFETY: Tcl copies `value` into a managed object before this call returns.
        let element = unsafe { Tcl_NewStringObj(value.as_ptr().cast(), length) };
        // SAFETY: the interpreter and both Tcl objects are live; the list owns the appended value.
        if unsafe { Tcl_ListObjAppendElement(interp, list, element) } != TCL_OK {
            // SAFETY: the live interpreter owns the diagnostic from the failed append.
            return Err(crate::ShellError::command(tcl_result(interp)));
        }
    }
    // SAFETY: the live interpreter takes ownership of the newly built result object.
    unsafe { Tcl_SetObjResult(interp, list) };
    Ok(())
}

pub(super) fn tcl_result(interp: *mut TclInterp) -> String {
    // SAFETY: the caller guarantees `interp` points to a live Tcl interpreter.
    let object = unsafe { Tcl_GetObjResult(interp) };
    if object.is_null() {
        return String::new();
    }
    // SAFETY: the non-null result object is owned by and live with the interpreter.
    unsafe { tcl_object_text(object) }.map_or_else(|error| error.to_string(), ToOwned::to_owned)
}

unsafe fn tcl_object_text<'a>(object: *mut TclObj) -> Result<&'a str, crate::ShellError> {
    if object.is_null() {
        return Err(crate::ShellError::command("Tcl object pointer is null"));
    }
    let mut length = 0;
    // SAFETY: the caller guarantees `object` is live; `length` is a valid out parameter.
    let raw = unsafe { Tcl_GetStringFromObj(object, &raw mut length) };
    if raw.is_null() || length < 0 {
        return Err(crate::ShellError::command(
            "Tcl object has no valid string representation",
        ));
    }
    let length = usize::try_from(length).expect("negative Tcl string lengths were rejected above");
    // SAFETY: Tcl returned `raw` with exactly `length` readable bytes tied to the live object.
    let bytes = unsafe { std::slice::from_raw_parts(raw.cast::<u8>(), length) };
    std::str::from_utf8(bytes).map_err(crate::ShellError::Utf8)
}

pub(super) fn eval_script(
    interp: *mut TclInterp,
    script: &str,
) -> Result<c_int, crate::ShellError> {
    let length = c_int::try_from(script.len())
        .map_err(|_| crate::ShellError::parse("Tcl script is too large"))?;
    // SAFETY: `interp` is live and the script pointer is readable for exactly `length` bytes.
    Ok(unsafe { Tcl_EvalEx(interp, script.as_ptr().cast(), length, 0) })
}

pub(super) fn eval_file(interp: *mut TclInterp, path: &CStr) -> c_int {
    // SAFETY: shell callers provide a live interpreter and a NUL-terminated path for this call.
    unsafe { Tcl_EvalFile(interp, path.as_ptr()) }
}

pub(super) fn error_line(interp: *mut TclInterp) -> usize {
    // SAFETY: shell callers provide the live interpreter that produced the current result.
    usize::try_from(unsafe { Tcl_GetErrorLine(interp) }).unwrap_or(1)
}

pub(super) fn command_complete(command: &CStr) -> bool {
    // SAFETY: `command` is NUL-terminated for the duration of the parser query.
    (unsafe { Tcl_CommandComplete(command.as_ptr()) }) != 0
}

pub(super) fn make_safe(interp: *mut TclInterp) -> c_int {
    // SAFETY: validation owns this interpreter exclusively before registering commands.
    unsafe { Tcl_MakeSafe(interp) }
}

extern "C" fn command_trampoline(
    client_data: *mut c_void,
    interp: *mut TclInterp,
    objc: c_int,
    objv: *const *mut TclObj,
) -> c_int {
    if let Ok(code) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        command_trampoline_impl(client_data, interp, objc, objv)
    })) {
        code
    } else {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            set_error(interp, "internal panic while executing Tcl command")
        }));
        TCL_ERROR
    }
}

fn command_trampoline_impl(
    client_data: *mut c_void,
    interp: *mut TclInterp,
    objc: c_int,
    objv: *const *mut TclObj,
) -> c_int {
    // SAFETY: registration stores a pointer to `ShellState`, which outlives the Tcl interpreter.
    let state = unsafe { &*(client_data as *const ShellState) };
    state.pending_command_error.replace(None);
    // SAFETY: Tcl invokes this callback with `objc` live object pointers in `objv`.
    let args = match unsafe { collect_args(objc, objv) } {
        Ok(args) => args,
        Err(error) => return set_command_error(state, interp, error),
    };
    if args.is_empty() {
        return set_command_error(
            state,
            interp,
            crate::ShellError::command("internal Tcl command dispatch error"),
        );
    }

    let Some(command) = state.commands.find(args[0].as_str()) else {
        return set_command_error(
            state,
            interp,
            crate::ShellError::command(format!("unknown command '{}'", args[0])),
        );
    };

    match dispatch(state, interp, command, &args[1..]) {
        Ok(CommandResult::Complete(result)) => {
            state.pending_command_error.replace(None);
            set_result(interp, &result);
            TCL_OK
        }
        Ok(CommandResult::List(values)) => {
            state.pending_command_error.replace(None);
            match set_list_result(interp, &values) {
                Ok(()) => TCL_OK,
                Err(error) => set_command_error(state, interp, error),
            }
        }
        Ok(CommandResult::Exit(code)) => {
            state.pending_command_error.replace(None);
            if !state.domain.get().is_sdc() {
                state.exit_code.replace(Some(code));
            }
            set_result(interp, "");
            TCL_RETURN
        }
        Err(error) => set_command_error(state, interp, error),
    }
}

extern "C" fn validation_command_trampoline(
    client_data: *mut c_void,
    interp: *mut TclInterp,
    objc: c_int,
    objv: *const *mut TclObj,
) -> c_int {
    if let Ok(code) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        validation_command_trampoline_impl(client_data, interp, objc, objv)
    })) {
        code
    } else {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            set_error(interp, "internal panic while validating SDC command")
        }));
        TCL_ERROR
    }
}

fn validation_command_trampoline_impl(
    client_data: *mut c_void,
    interp: *mut TclInterp,
    objc: c_int,
    objv: *const *mut TclObj,
) -> c_int {
    // SAFETY: validation registration stores a `RegisteredCommand` pointer that outlives the
    // validation interpreter.
    let command = unsafe { &*(client_data as *const RegisteredCommand) };
    let spec = command.spec();
    // SAFETY: Tcl invokes this callback with `objc` live object pointers in `objv`.
    let args = match unsafe { collect_args(objc, objv) } {
        Ok(args) => args,
        Err(error) => return set_error(interp, &error.to_string()),
    };
    let Some((_, command_args)) = args.split_first() else {
        return set_error(interp, "internal SDC validation dispatch error");
    };
    if let Err(error) = command_catalog::validate_sdc_invocation(command, command_args) {
        return set_error(interp, &error.to_string());
    }
    if spec.name == "source" {
        let path = &command_args[0];
        let Ok(path) = CString::new(path.as_str()) else {
            return set_error(interp, "source: file name contains NUL");
        };
        // SAFETY: the safe interpreter is live and `path` is NUL-terminated. Nested scripts use
        // the same restricted command set, so reading them cannot expose host-side operations.
        return eval_file(interp, &path);
    }
    if spec.name == "exit" {
        set_result(interp, "");
        return TCL_RETURN;
    }
    set_result(interp, "");
    TCL_OK
}
