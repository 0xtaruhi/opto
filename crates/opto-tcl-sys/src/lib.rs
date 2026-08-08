// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#![allow(
    unsafe_code,
    reason = "this crate is the audited Rust boundary for the native Tcl runtime"
)]

//! Embedded Tcl 8.6 runtime and safe interpreter wrapper.
//!
//! [`Interpreter`] owns one native Tcl interpreter and exposes evaluation,
//! command registration, variable access, and object conversion through
//! Rust-shaped results. The standard Tcl library is compiled into a read-only
//! virtual filesystem at [`TCL_LIBRARY_PATH`], so the product executable does
//! not depend on a host Tcl installation.
//!
//! Interpreters are deliberately thread-affine and therefore neither `Send` nor
//! `Sync`. Callbacks must not retain borrowed Tcl object pointers beyond the
//! native invocation. The [`ffi`] module is public only for the shell adapter
//! and follows Tcl's ownership rules verbatim.

/// Raw Tcl C API declarations used by the product shell.
///
/// Prefer [`Interpreter`] unless implementing a native Tcl command. Functions
/// in this module are unsafe and retain Tcl's original ownership conventions.
pub mod ffi;

use ffi::{TCL_OK, TclInterp};
use std::ffi::{CStr, CString, c_char, c_int, c_uchar};
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use thiserror::Error;

/// Patch level of the embedded Tcl runtime.
pub const TCL_PATCH_LEVEL: &str = "8.6.11";
/// Virtual filesystem path containing the embedded Tcl standard library.
pub const TCL_LIBRARY_PATH: &str = "opto:/tcl8.6";

struct EmbeddedEntry {
    path: &'static [u8],
    data: &'static [u8],
    is_dir: bool,
}

impl EmbeddedEntry {
    const fn directory(path: &'static [u8]) -> Self {
        Self {
            path,
            data: &[],
            is_dir: true,
        }
    }

    const fn file(path: &'static [u8], data: &'static [u8]) -> Self {
        Self {
            path,
            data,
            is_dir: false,
        }
    }
}

include!(concat!(env!("OUT_DIR"), "/library_index.rs"));

#[unsafe(no_mangle)]
extern "C" fn opto_tcl_embedded_entry_count() -> usize {
    EMBEDDED_ENTRIES.len()
}

#[unsafe(no_mangle)]
extern "C" fn opto_tcl_embedded_entry_path(index: usize) -> *const c_char {
    EMBEDDED_ENTRIES
        .get(index)
        .map_or(std::ptr::null(), |entry| entry.path.as_ptr().cast())
}

#[unsafe(no_mangle)]
extern "C" fn opto_tcl_embedded_entry_data(index: usize) -> *const c_uchar {
    EMBEDDED_ENTRIES
        .get(index)
        .map_or(std::ptr::null(), |entry| entry.data.as_ptr())
}

#[unsafe(no_mangle)]
extern "C" fn opto_tcl_embedded_entry_length(index: usize) -> usize {
    EMBEDDED_ENTRIES
        .get(index)
        .map_or(0, |entry| entry.data.len())
}

#[unsafe(no_mangle)]
extern "C" fn opto_tcl_embedded_entry_is_dir(index: usize) -> c_int {
    EMBEDDED_ENTRIES
        .get(index)
        .map_or(0, |entry| c_int::from(entry.is_dir))
}

/// Failure while initializing or using the embedded Tcl runtime.
#[derive(Debug, Clone, Error)]
pub enum TclError {
    /// The host process executable path could not be queried.
    #[error("failed to locate current executable: {0}")]
    CurrentExecutable(#[source] Arc<std::io::Error>),
    /// The executable path cannot be represented as a Tcl C string.
    #[error("executable path contains NUL: {0}")]
    ExecutablePathNul(#[source] std::ffi::NulError),
    /// Registration of the read-only embedded filesystem failed.
    #[error("failed to register the embedded Tcl filesystem")]
    RegisterFilesystem,
    /// Tcl failed to allocate an interpreter.
    #[error("Tcl_CreateInterp returned null")]
    NullInterpreter,
    /// The interpreter rejected the embedded standard-library path.
    #[error("failed to set tcl_library: {0}")]
    SetLibraryPath(String),
    /// Tcl standard-library initialization failed.
    #[error("Tcl_Init failed: {0}")]
    Initialize(String),
    /// Tcl's parser rejected input or returned an invalid token range.
    #[error("Tcl parse failed: {0}")]
    Parse(String),
    /// A command name cannot be represented by Tcl's lookup API.
    #[error("Tcl command name contains NUL: {0}")]
    CommandNameNul(#[source] std::ffi::NulError),
}

/// Parsed command and its byte location within the original script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// UTF-8 byte offset of the first command byte.
    pub byte_offset: usize,
    /// Command words in Tcl evaluation order.
    pub words: Vec<ParsedWord>,
}

/// Static information extracted from one Tcl command word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWord {
    /// Unquoted value when the complete word is literal.
    pub literal: Option<String>,
    /// Nested command bodies, without their surrounding brackets.
    pub command_substitutions: Vec<String>,
}

static INITIALIZATION: OnceLock<Result<(), TclError>> = OnceLock::new();

/// Owning, thread-affine wrapper around one initialized Tcl interpreter.
///
/// Dropping the value destroys the native interpreter. The raw pointer returned
/// by [`Self::as_ptr`] is valid only while this owner remains alive.
#[derive(Debug)]
pub struct Interpreter {
    raw: NonNull<TclInterp>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl Interpreter {
    /// Creates an interpreter backed by the embedded Tcl standard library.
    ///
    /// # Errors
    ///
    /// Returns an error if process-wide Tcl initialization fails, Tcl cannot
    /// allocate or initialize the interpreter, or the embedded library path
    /// cannot be installed in the new interpreter.
    pub fn new() -> Result<Self, TclError> {
        initialize_process()?;
        // SAFETY: process-wide Tcl initialization has completed and the call has no arguments.
        let raw =
            NonNull::new(unsafe { ffi::Tcl_CreateInterp() }).ok_or(TclError::NullInterpreter)?;
        let interpreter = Self {
            raw,
            _not_send_or_sync: PhantomData,
        };
        interpreter.set_library_path()?;
        // SAFETY: `interpreter` owns a live Tcl interpreter until its `Drop` implementation.
        let code = unsafe { ffi::Tcl_Init(interpreter.as_ptr()) };
        if code != TCL_OK {
            return Err(TclError::Initialize(interpreter.result_string()));
        }
        Ok(interpreter)
    }

    /// Returns the borrowed native interpreter pointer.
    ///
    /// The pointer must not be destroyed or retained beyond `self`.
    #[must_use]
    pub fn as_ptr(&self) -> *mut TclInterp {
        self.raw.as_ptr()
    }

    /// Copies the interpreter's current result object into a Rust string.
    #[must_use]
    pub fn result_string(&self) -> String {
        // SAFETY: `self` owns a live interpreter and keeps its result object alive for the call.
        unsafe { result_string(self.as_ptr()) }
    }

    /// Parses every command without evaluating substitutions or command bodies.
    ///
    /// Literal word values omit their Tcl quoting delimiters. Dynamic words retain
    /// only their nested command substitutions so callers can recursively inspect
    /// code that Tcl would otherwise execute conditionally.
    ///
    /// # Errors
    ///
    /// Returns [`TclError::Parse`] when the script exceeds Tcl's parser limits,
    /// Tcl rejects its syntax, or the native parser reports a byte range outside
    /// the supplied script.
    pub fn parse_commands(&self, script: &str) -> Result<Vec<ParsedCommand>, TclError> {
        let mut commands = Vec::new();
        let mut start = script.as_ptr();
        let mut remaining = script.len();
        while remaining != 0 {
            let byte_count = c_int::try_from(remaining)
                .map_err(|_| TclError::Parse("script exceeds Tcl's parser limit".to_string()))?;
            // SAFETY: Tcl initializes every public field before returning from Tcl_ParseCommand.
            let mut parse = unsafe { std::mem::zeroed::<ffi::TclParse>() };
            // SAFETY: `start` addresses `remaining` live bytes in `script`; `parse` is writable.
            let code = unsafe {
                ffi::Tcl_ParseCommand(self.as_ptr(), start.cast(), byte_count, 0, &raw mut parse)
            };
            if code != TCL_OK {
                return Err(TclError::Parse(self.result_string()));
            }
            let parsed = (|| {
                let mut command = parsed_command(&parse)?;
                let command_start = parse.command_start.cast::<u8>();
                let command_size = usize::try_from(parse.command_size).map_err(|_| {
                    TclError::Parse("Tcl returned an invalid command size".to_string())
                })?;
                // SAFETY: Tcl guarantees command_start points into the supplied script range.
                let skipped = unsafe { command_start.offset_from(start) };
                let consumed = usize::try_from(skipped)
                    .ok()
                    .and_then(|skipped| skipped.checked_add(command_size))
                    .filter(|consumed| *consumed <= remaining && *consumed != 0)
                    .ok_or_else(|| {
                        TclError::Parse("Tcl returned an invalid command range".to_string())
                    })?;
                command.byte_offset = script.len() - remaining
                    + usize::try_from(skipped).map_err(|_| {
                        TclError::Parse("Tcl returned an invalid command offset".to_string())
                    })?;
                Ok::<_, TclError>((command, consumed))
            })();
            // SAFETY: this successful parse may own a dynamic token buffer and is freed once.
            unsafe { ffi::Tcl_FreeParse(&raw mut parse) };
            let (command, consumed) = parsed?;
            if !command.words.is_empty() {
                commands.push(command);
            }
            // SAFETY: `consumed` was checked to remain within the original script allocation.
            start = unsafe { start.add(consumed) };
            remaining -= consumed;
        }
        Ok(commands)
    }

    /// Returns whether the interpreter currently exposes `name`.
    ///
    /// # Errors
    ///
    /// Returns [`TclError::CommandNameNul`] when `name` contains an interior NUL
    /// byte and therefore cannot be represented as a Tcl command name.
    pub fn has_command(&self, name: &str) -> Result<bool, TclError> {
        let name = CString::new(name).map_err(TclError::CommandNameNul)?;
        // SAFETY: `self` owns a live interpreter, `name` is NUL-terminated, and a null namespace
        // asks Tcl to search from the interpreter's current namespace.
        Ok(
            !unsafe { ffi::Tcl_FindCommand(self.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) }
                .is_null(),
        )
    }

    fn set_library_path(&self) -> Result<(), TclError> {
        let name = c"tcl_library";
        let value = CString::new(TCL_LIBRARY_PATH).expect("static Tcl library path has no NUL");
        // SAFETY: the interpreter is live and both C strings remain valid for the duration of the call.
        let result = unsafe { ffi::Tcl_SetVar(self.as_ptr(), name.as_ptr(), value.as_ptr(), 0) };
        if result.is_null() {
            return Err(TclError::SetLibraryPath(self.result_string()));
        }
        Ok(())
    }
}

fn parsed_command(parse: &ffi::TclParse) -> Result<ParsedCommand, TclError> {
    let token_count = usize::try_from(parse.num_tokens)
        .map_err(|_| TclError::Parse("Tcl returned an invalid token count".to_string()))?;
    let word_count = usize::try_from(parse.num_words)
        .map_err(|_| TclError::Parse("Tcl returned an invalid word count".to_string()))?;
    if token_count == 0 || word_count == 0 {
        return Ok(ParsedCommand {
            byte_offset: 0,
            words: Vec::new(),
        });
    }
    if parse.token_ptr.is_null() {
        return Err(TclError::Parse(
            "Tcl returned a null token buffer".to_string(),
        ));
    }
    // SAFETY: a successful parse exposes exactly num_tokens initialized entries until FreeParse.
    let tokens = unsafe { std::slice::from_raw_parts(parse.token_ptr, token_count) };
    let mut words = Vec::with_capacity(word_count);
    let mut token_index = 0usize;
    for _ in 0..word_count {
        let word = tokens
            .get(token_index)
            .ok_or_else(|| TclError::Parse("Tcl word token is out of range".to_string()))?;
        let component_count = usize::try_from(word.num_components).map_err(|_| {
            TclError::Parse("Tcl returned an invalid word component count".to_string())
        })?;
        let component_start = token_index
            .checked_add(1)
            .ok_or_else(|| TclError::Parse("Tcl token index overflow".to_string()))?;
        let component_end = component_start
            .checked_add(component_count)
            .filter(|end| *end <= tokens.len())
            .ok_or_else(|| TclError::Parse("Tcl word components are out of range".to_string()))?;
        let components = &tokens[component_start..component_end];
        let literal = if word.token_type == ffi::TCL_TOKEN_SIMPLE_WORD
            && components.len() == 1
            && components[0].token_type == ffi::TCL_TOKEN_TEXT
        {
            Some(token_text(&components[0])?.to_string())
        } else {
            None
        };
        let command_substitutions = components
            .iter()
            .filter(|token| token.token_type == ffi::TCL_TOKEN_COMMAND)
            .map(command_substitution)
            .collect::<Result<Vec<_>, _>>()?;
        words.push(ParsedWord {
            literal,
            command_substitutions,
        });
        token_index = component_end;
    }
    Ok(ParsedCommand {
        byte_offset: 0,
        words,
    })
}

fn token_text(token: &ffi::TclToken) -> Result<&str, TclError> {
    let size = usize::try_from(token.size)
        .map_err(|_| TclError::Parse("Tcl returned an invalid token size".to_string()))?;
    if token.start.is_null() && size != 0 {
        return Err(TclError::Parse(
            "Tcl returned a null non-empty token".to_string(),
        ));
    }
    if size == 0 {
        return Ok("");
    }
    // SAFETY: token ranges are owned by the live input script for the duration of parsing.
    let bytes = unsafe { std::slice::from_raw_parts(token.start.cast::<u8>(), size) };
    std::str::from_utf8(bytes)
        .map_err(|_| TclError::Parse("Tcl split a token outside UTF-8 boundaries".to_string()))
}

fn command_substitution(token: &ffi::TclToken) -> Result<String, TclError> {
    let text = token_text(token)?;
    text.strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
        .map(str::to_string)
        .ok_or_else(|| TclError::Parse("Tcl returned an invalid command token".to_string()))
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        // SAFETY: `raw` was returned by `Tcl_CreateInterp`, is uniquely owned, and is deleted once.
        unsafe { ffi::Tcl_DeleteInterp(self.as_ptr()) };
    }
}

fn initialize_process() -> Result<(), TclError> {
    INITIALIZATION
        .get_or_init(|| {
            let executable = std::env::current_exe()
                .map_err(|error| TclError::CurrentExecutable(Arc::new(error)))?;
            let executable = executable.to_string_lossy();
            let executable =
                CString::new(executable.as_bytes()).map_err(TclError::ExecutablePathNul)?;
            // SAFETY: `executable` is NUL-terminated and lives through Tcl's initialization call.
            unsafe { ffi::Tcl_FindExecutable(executable.as_ptr()) };
            // SAFETY: the embedded entry callbacks expose static data and registration is serialized by `OnceLock`.
            let code = unsafe { ffi::opto_tcl_vfs_register() };
            if code != TCL_OK {
                return Err(TclError::RegisterFilesystem);
            }
            Ok(())
        })
        .clone()
}

unsafe fn result_string(interp: *mut TclInterp) -> String {
    // SAFETY: the caller guarantees `interp` points to a live Tcl interpreter.
    let object = unsafe { ffi::Tcl_GetObjResult(interp) };
    if object.is_null() {
        return String::new();
    }
    // SAFETY: Tcl returned `object` from the live interpreter and keeps it valid during this call.
    let raw = unsafe { ffi::Tcl_GetString(object) };
    if raw.is_null() {
        String::new()
    } else {
        // SAFETY: Tcl guarantees `Tcl_GetString` returns a NUL-terminated string for a non-null object.
        unsafe { CStr::from_ptr(raw) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_from_the_embedded_library() {
        let interpreter = Interpreter::new().unwrap();
        let script = c"list [info patchlevel] $tcl_library [file exists opto:/tcl8.6/init.tcl]";
        // SAFETY: the interpreter is live and `script` is a static NUL-terminated C string.
        let code = unsafe { ffi::Tcl_Eval(interpreter.as_ptr(), script.as_ptr()) };
        assert_eq!(code, TCL_OK, "{}", interpreter.result_string());
        assert_eq!(
            interpreter.result_string(),
            format!("{TCL_PATCH_LEVEL} {TCL_LIBRARY_PATH} 1")
        );
    }

    #[test]
    fn safe_interpreter_hides_host_side_commands() {
        let interpreter = Interpreter::new().unwrap();
        // SAFETY: the interpreter is live and exclusively owned by this test.
        let code = unsafe { ffi::Tcl_MakeSafe(interpreter.as_ptr()) };
        assert_eq!(code, TCL_OK, "{}", interpreter.result_string());

        let script = c"open /tmp/opto-safe-interpreter-probe w";
        // SAFETY: the interpreter and static NUL-terminated script are live.
        let code = unsafe { ffi::Tcl_Eval(interpreter.as_ptr(), script.as_ptr()) };
        assert_eq!(code, ffi::TCL_ERROR);
        assert!(interpreter.result_string().contains("open"));
    }

    #[test]
    fn constructs_native_list_results() {
        let interpreter = Interpreter::new().unwrap();
        // SAFETY: Tcl accepts a null element array for a new empty list.
        let list = unsafe { ffi::Tcl_NewListObj(0, std::ptr::null()) };
        // SAFETY: `value` is NUL-terminated and a length of -1 asks Tcl to copy through the NUL.
        let value = unsafe { ffi::Tcl_NewStringObj(c"clock one".as_ptr(), -1) };
        // SAFETY: all three objects are live and the list takes ownership of the element.
        let code = unsafe { ffi::Tcl_ListObjAppendElement(interpreter.as_ptr(), list, value) };
        assert_eq!(code, TCL_OK, "{}", interpreter.result_string());
        // SAFETY: the live interpreter takes ownership of the list result.
        unsafe { ffi::Tcl_SetObjResult(interpreter.as_ptr(), list) };
        assert_eq!(interpreter.result_string(), "{clock one}");
    }

    #[test]
    fn parses_literal_words_and_nested_command_substitutions_without_evaluation() {
        let interpreter = Interpreter::new().unwrap();
        let commands = interpreter
            .parse_commands(
                "# heading\nif {0} { create_clock -name \"dead clock\" -period }\nset x [list a]\n",
            )
            .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].byte_offset, 10);
        assert_eq!(commands[0].words[0].literal.as_deref(), Some("if"));
        assert_eq!(
            commands[0].words[2].literal.as_deref(),
            Some(" create_clock -name \"dead clock\" -period ")
        );
        assert_eq!(commands[1].words[2].command_substitutions, ["list a"]);
    }
}
