// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::bridge::read;
use crate::ffi;
use crate::{
    SlangAnalysis, SlangCompilation, SlangCompileOptions, SlangError, SlangLanguage,
    SlangSourceFile, SlangSourceUnit,
};
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};

pub(crate) fn compile(
    files: &[PathBuf],
    options: &SlangCompileOptions,
) -> Result<SlangCompilation, SlangError> {
    configured_path_compiler(files, options)?.compile()
}

pub(crate) fn compile_lazy(
    files: &[PathBuf],
    options: &SlangCompileOptions,
) -> Result<SlangCompilation, SlangError> {
    configured_path_compiler(files, options)?.compile_lazy()
}

fn configured_path_compiler(
    files: &[PathBuf],
    options: &SlangCompileOptions,
) -> Result<Compiler, SlangError> {
    let compiler = Compiler::new()?;
    compiler.set_language(options.language)?;
    if let Some(max_threads) = options.max_threads {
        compiler.set_max_threads(max_threads)?;
    }
    for path in files {
        compiler.begin_source_unit()?;
        compiler.add_source_path(path)?;
        for include_path in &options.include_paths {
            compiler.add_include_dir(include_path)?;
        }
        for define in &options.defines {
            compiler.add_define(&define.name, define.value.as_deref())?;
        }
    }
    if let Some(top) = &options.top {
        compiler.set_top(top)?;
    }
    Ok(compiler)
}

pub(crate) fn analyze(
    units: &[SlangSourceUnit],
    max_threads: Option<usize>,
) -> Result<SlangAnalysis, SlangError> {
    let compiler = configured_compiler(units, None, max_threads)?;
    compiler.analyze()
}

pub(crate) fn compile_units(
    units: &[SlangSourceUnit],
    top: &str,
    max_threads: Option<usize>,
) -> Result<SlangCompilation, SlangError> {
    if top.trim().is_empty() {
        return Err(SlangError::InvalidInput(
            "slang top module name is empty".to_string(),
        ));
    }
    compile_configured(units, Some(top), max_threads, false)
}

pub(crate) fn compile_units_lazy(
    units: &[SlangSourceUnit],
    top: &str,
    max_threads: Option<usize>,
) -> Result<SlangCompilation, SlangError> {
    if top.trim().is_empty() {
        return Err(SlangError::InvalidInput(
            "slang top module name is empty".to_string(),
        ));
    }
    compile_configured(units, Some(top), max_threads, true)
}

fn compile_configured(
    units: &[SlangSourceUnit],
    top: Option<&str>,
    max_threads: Option<usize>,
    lazy: bool,
) -> Result<SlangCompilation, SlangError> {
    let compiler = configured_compiler(units, top, max_threads)?;
    if lazy {
        compiler.compile_lazy()
    } else {
        compiler.compile()
    }
}

fn configured_compiler(
    units: &[SlangSourceUnit],
    top: Option<&str>,
    max_threads: Option<usize>,
) -> Result<Compiler, SlangError> {
    if units.is_empty() {
        return Err(SlangError::InvalidInput(
            "slang requires at least one source unit".to_string(),
        ));
    }
    let compiler = Compiler::new()?;
    compiler.set_language(
        units
            .iter()
            .map(|unit| unit.language)
            .max_by_key(|language| match language {
                SlangLanguage::Verilog2005 => 0,
                SlangLanguage::SystemVerilog2017 => 1,
            })
            .expect("non-empty source units"),
    )?;
    if let Some(max_threads) = max_threads {
        compiler.set_max_threads(max_threads)?;
    }
    for unit in units {
        if unit.files.is_empty() {
            return Err(SlangError::InvalidInput(
                "slang source unit requires at least one input file".to_string(),
            ));
        }
        compiler.begin_source_unit()?;
        for file in &unit.files {
            compiler.add_source_file(file)?;
        }
        for dependency in &unit.dependencies {
            compiler.add_source_dependency(dependency)?;
        }
        for include_path in &unit.include_paths {
            compiler.add_include_dir(include_path)?;
        }
        for define in &unit.defines {
            compiler.add_define(&define.name, define.value.as_deref())?;
        }
    }
    if let Some(top) = top {
        compiler.set_top(top)?;
    }
    Ok(compiler)
}

struct Compiler {
    raw: NonNull<ffi::Compiler>,
}

impl Compiler {
    fn new() -> Result<Self, SlangError> {
        // SAFETY: the constructor takes no pointers; ownership of a non-null result transfers to Rust.
        let raw = unsafe { ffi::opto_slang_compiler_new() };
        let raw = NonNull::new(raw).ok_or_else(|| {
            SlangError::BridgeInvariant("native slang bridge failed to create compiler".to_string())
        })?;
        Ok(Self { raw })
    }

    fn begin_source_unit(&self) -> Result<(), SlangError> {
        // SAFETY: `self.raw` is a live, uniquely owned compiler handle.
        self.check_status(unsafe { ffi::opto_slang_compiler_begin_source_unit(self.raw.as_ptr()) })
    }

    fn add_source_file(&self, source: &SlangSourceFile) -> Result<(), SlangError> {
        let path = path_to_cstring(&source.path)?;
        let text = string_to_cstring(&source.text, "source text")?;
        // SAFETY: the compiler is live and both NUL-terminated inputs outlive the call.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_add_source_file(
                self.raw.as_ptr(),
                path.as_ptr(),
                text.as_ptr(),
            )
        })
    }

    fn add_source_path(&self, source: &Path) -> Result<(), SlangError> {
        let path = path_to_cstring(source)?;
        // SAFETY: the compiler is live and `path` is NUL-terminated and valid through the call.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_add_source_path(self.raw.as_ptr(), path.as_ptr())
        })
    }

    fn add_source_dependency(&self, source: &SlangSourceFile) -> Result<(), SlangError> {
        let path = path_to_cstring(&source.path)?;
        let text = string_to_cstring(&source.text, "source dependency text")?;
        // SAFETY: the compiler is live and both NUL-terminated inputs outlive the call.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_add_source_dependency(
                self.raw.as_ptr(),
                path.as_ptr(),
                text.as_ptr(),
            )
        })
    }

    fn add_include_dir(&self, path: &Path) -> Result<(), SlangError> {
        let path = path_to_cstring(path)?;
        // SAFETY: the compiler is live and `path` is NUL-terminated and valid through the call.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_add_include_dir(self.raw.as_ptr(), path.as_ptr())
        })
    }

    fn add_define(&self, name: &str, value: Option<&str>) -> Result<(), SlangError> {
        let name = string_to_cstring(name, "define name")?;
        let value = value
            .map(|value| string_to_cstring(value, "define value"))
            .transpose()?;
        let value_ptr = value.as_ref().map_or(ptr::null(), |value| value.as_ptr());
        // SAFETY: all non-null string pointers are NUL-terminated and remain live through the call.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_add_define(self.raw.as_ptr(), name.as_ptr(), value_ptr)
        })
    }

    fn set_top(&self, top: &str) -> Result<(), SlangError> {
        let top = string_to_cstring(top, "top module")?;
        // SAFETY: the compiler is live and `top` is NUL-terminated and valid through the call.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_set_top(self.raw.as_ptr(), top.as_ptr())
        })
    }

    fn set_language(&self, language: SlangLanguage) -> Result<(), SlangError> {
        let language = match language {
            SlangLanguage::Verilog2005 => ffi::LANGUAGE_VERILOG_2005,
            SlangLanguage::SystemVerilog2017 => ffi::LANGUAGE_SYSTEM_VERILOG_2017,
        };
        // SAFETY: the compiler handle is live and `language` is one of the bridge constants.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_set_language(self.raw.as_ptr(), language)
        })
    }

    fn set_max_threads(&self, max_threads: usize) -> Result<(), SlangError> {
        let max_threads = u32::try_from(max_threads).map_err(|_| {
            SlangError::InvalidInput(format!(
                "slang max thread count {max_threads} exceeds the native limit"
            ))
        })?;
        if max_threads == 0 {
            return Err(SlangError::InvalidInput(
                "slang max thread count must be positive".to_string(),
            ));
        }
        // SAFETY: the compiler handle is live and the positive count fits the bridge's `u32` ABI.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_set_max_threads(self.raw.as_ptr(), max_threads)
        })
    }

    fn compile(&self) -> Result<SlangCompilation, SlangError> {
        let compilation = self.compile_lazy()?;
        compilation.materialize_all()?;
        Ok(compilation)
    }

    fn compile_lazy(&self) -> Result<SlangCompilation, SlangError> {
        let mut design = ptr::null_mut();
        // SAFETY: the compiler is live and `design` is a valid writable out parameter.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_compile(self.raw.as_ptr(), &raw mut design)
        })?;
        SlangCompilation::from_raw(design)
    }

    fn analyze(&self) -> Result<SlangAnalysis, SlangError> {
        let mut raw = ptr::null_mut();
        // SAFETY: the compiler is live and `raw` is a valid writable out parameter.
        self.check_status(unsafe {
            ffi::opto_slang_compiler_analyze(self.raw.as_ptr(), &raw mut raw)
        })?;
        let raw = NonNull::new(raw).ok_or_else(|| {
            SlangError::BridgeInvariant("native slang bridge returned a null analysis".to_string())
        })?;
        let handle = AnalysisHandle(raw);
        // SAFETY: `handle` owns a live analysis and the bridge initializes the view on success.
        let view = unsafe {
            read("analysis", |view| {
                ffi::opto_slang_analysis_view(handle.0.as_ptr(), view)
            })
        }?;
        let definitions = copy_analysis_names(
            handle.0,
            view.definition_count,
            ffi::opto_slang_analysis_definition_name,
            "definition name",
        )?;
        let packages = copy_analysis_names(
            handle.0,
            view.package_count,
            ffi::opto_slang_analysis_package_name,
            "package name",
        )?;
        let dependencies = (0..view.dependency_count)
            .map(|index| {
                // SAFETY: `index` is bounded by the dependency count returned for this live analysis.
                let source = unsafe {
                    read("analysis dependency", |view| {
                        ffi::opto_slang_analysis_dependency_view(handle.0.as_ptr(), index, view)
                    })
                }?;
                let path = copy_required_string(source.path, "analysis dependency path")?;
                let text = copy_required_string(source.text, "analysis dependency text")?;
                Ok(SlangSourceFile {
                    path: PathBuf::from(path),
                    text,
                })
            })
            .collect::<Result<Vec<_>, SlangError>>()?;
        Ok(SlangAnalysis {
            definitions,
            packages,
            dependencies,
        })
    }

    fn check_status(&self, status: std::ffi::c_int) -> Result<(), SlangError> {
        if status == ffi::OK {
            return Ok(());
        }
        Err(SlangError::CompileFailed(self.last_error()))
    }

    fn last_error(&self) -> String {
        // SAFETY: the compiler is live; the returned pointer is borrowed from it and checked for null.
        let ptr = unsafe { ffi::opto_slang_compiler_last_error(self.raw.as_ptr()) };
        if ptr.is_null() {
            return "native slang bridge reported an error without a message".to_string();
        }
        // SAFETY: a non-null bridge error pointer addresses a NUL-terminated string owned by the compiler.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

struct AnalysisHandle(NonNull<ffi::Analysis>);

impl Drop for AnalysisHandle {
    fn drop(&mut self) {
        // SAFETY: this handle owns the analysis returned by the bridge and frees it exactly once.
        unsafe { ffi::opto_slang_analysis_free(self.0.as_ptr()) };
    }
}

fn copy_analysis_names(
    analysis: NonNull<ffi::Analysis>,
    count: usize,
    accessor: unsafe extern "C" fn(*const ffi::Analysis, usize) -> *const std::ffi::c_char,
    context: &str,
) -> Result<Vec<String>, SlangError> {
    (0..count)
        .map(|index| {
            // SAFETY: `analysis` is live and `index` is bounded by the count paired with this accessor.
            copy_required_string(unsafe { accessor(analysis.as_ptr(), index) }, context)
        })
        .collect()
}

fn copy_required_string(ptr: *const std::ffi::c_char, context: &str) -> Result<String, SlangError> {
    if ptr.is_null() {
        return Err(SlangError::BridgeInvariant(format!(
            "native slang bridge returned a null {context}"
        )));
    }
    // SAFETY: the checked non-null bridge pointer is documented to reference a NUL-terminated string.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_string)
        .map_err(|error| {
            SlangError::BridgeInvariant(format!(
                "native slang bridge returned non-UTF-8 {context}: {error}"
            ))
        })
}

impl Drop for Compiler {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the compiler handle and frees it exactly once.
        unsafe { ffi::opto_slang_compiler_free(self.raw.as_ptr()) };
    }
}

fn path_to_cstring(path: &Path) -> Result<CString, SlangError> {
    let text = path.to_str().ok_or_else(|| {
        SlangError::InvalidInput(format!(
            "native slang bridge requires UTF-8 paths, got '{}'",
            path.display()
        ))
    })?;
    string_to_cstring(text, "path")
}

fn string_to_cstring(value: &str, context: &str) -> Result<CString, SlangError> {
    CString::new(value).map_err(|_| {
        SlangError::InvalidInput(format!("native slang bridge {context} contains a NUL byte"))
    })
}
