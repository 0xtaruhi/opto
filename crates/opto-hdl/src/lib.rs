// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! HDL source ingestion and elaboration.
//!
//! [`Frontend`] is the Rust-facing HDL entry point. Ingestion parses source
//! units and retains their dependency inventory; elaboration combines those
//! units, selects a top definition, and lowers the resulting native design into
//! the RTL IR.
//!
//! Source text remains owned by the source set. Native slang objects do
//! not escape the bridge, and lowering publishes Rust-owned modules only after
//! the complete design has been validated.

mod error;

pub use error::HdlError;

use opto_slang_sys::{
    SlangCompileOptions, SlangDefine, SlangLanguage, SlangSourceFile, SlangSourceUnit,
};
pub use opto_slang_sys::{
    SlangDiagnostic, SlangDiagnosticLocation, SlangDiagnosticSeverity, SlangError,
};
use std::path::PathBuf;

mod lower;

/// Stable digest of the exact native frontend implementation used by this build.
pub const NATIVE_FRONTEND_FINGERPRINT: &str = opto_slang_sys::NATIVE_FRONTEND_FINGERPRINT;

/// Options shared by HDL analysis and one-shot elaboration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontendOptions {
    /// Optional top definition for one-shot reads.
    pub top: Option<String>,
    /// Include search paths in user-specified order.
    pub include_paths: Vec<PathBuf>,
    /// Preprocessor definitions as `(name, optional value)` pairs.
    pub defines: Vec<(String, Option<String>)>,
    /// Source-language revision.
    pub language: VerilogLanguage,
}

/// Verilog-family language revision accepted by the frontend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerilogLanguage {
    /// IEEE 1364-2005 Verilog.
    Verilog2005,
    /// IEEE 1800-2017 language revision.
    #[default]
    SystemVerilog2017,
}

/// Fully lowered HDL definitions ready for transactional session insertion.
#[derive(Debug, Clone, Default)]
pub struct DbUpdate {
    /// Rust-owned RTL definitions in deterministic elaboration order.
    pub modules: Vec<opto_ir::rtl::RtlModule>,
    /// Selected top definition, when elaboration established one.
    pub top: Option<String>,
    /// Recoverable frontend diagnostics emitted while producing the update.
    pub diagnostics: Vec<SlangDiagnostic>,
}

/// Ingested source set retained for later elaboration.
///
/// The value owns the source text and dependency closure required to recreate
/// native frontend state. It can therefore outlive the frontend call that
/// produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerilogSourceSet {
    source_units: Vec<SlangSourceUnit>,
    definitions: Vec<String>,
    packages: Vec<String>,
    diagnostics: Vec<SlangDiagnostic>,
}
impl VerilogSourceSet {
    /// Returns definition names discovered in this source set.
    #[must_use]
    pub fn definitions(&self) -> &[String] {
        &self.definitions
    }

    /// Returns package names discovered in this source set.
    #[must_use]
    pub fn packages(&self) -> &[String] {
        &self.packages
    }

    /// Returns recoverable diagnostics emitted while ingesting this source set.
    #[must_use]
    pub fn diagnostics(&self) -> &[SlangDiagnostic] {
        &self.diagnostics
    }
}

/// Stateless entry point for HDL ingestion and elaboration.
#[derive(Debug)]
pub struct Frontend;

impl Frontend {
    /// Ingests source units without selecting a top.
    ///
    /// Primary files are independent compilation units, matching normal
    /// `SystemVerilog` file-scope macro semantics. Shared command-line defines
    /// and include paths apply to every unit; include dependencies are captured
    /// in the returned [`VerilogSourceSet`].
    ///
    /// # Errors
    ///
    /// Returns an error for an empty file list, unreadable source or include
    /// dependency, invalid frontend options, or native parsing failure.
    pub fn ingest_verilog(
        files: &[PathBuf],
        options: &FrontendOptions,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<VerilogSourceSet, HdlError> {
        require_files(files)?;
        let mut units = slang_source_units(files, options, runtime)?;
        let analysis = opto_slang_sys::analyze(&units, Some(runtime.parallelism()))
            .map_err(HdlError::Slang)?;
        units[0].dependencies = analysis.dependencies;
        Ok(VerilogSourceSet {
            source_units: units,
            definitions: analysis.definitions,
            packages: analysis.packages,
            diagnostics: analysis.diagnostics,
        })
    }

    /// Elaborates ingested units under `top` and lowers them into the RTL IR.
    ///
    /// Independent modules may be materialized in parallel through `runtime`;
    /// publication order remains deterministic.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty source set, native elaboration or
    /// materialization failure, an invalid/missing top, unsupported HDL
    /// semantics, IR validation failure, or runtime task failure.
    pub fn elaborate_verilog(
        source_sets: &[VerilogSourceSet],
        top: &str,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<DbUpdate, HdlError> {
        if source_sets.is_empty() {
            return Err(HdlError::NoSourceUnits);
        }
        let units = source_sets
            .iter()
            .flat_map(|source_set| source_set.source_units.iter().cloned())
            .collect::<Vec<_>>();
        let compilation =
            opto_slang_sys::compile_units_lazy(&units, top, Some(runtime.parallelism()))
                .map_err(HdlError::Slang)?;
        let diagnostics = compilation.diagnostics().to_vec();
        let mut update = lower::compilation(
            &compilation,
            &FrontendOptions {
                top: Some(top.to_string()),
                ..FrontendOptions::default()
            },
            runtime,
        )?;
        update.diagnostics = diagnostics;
        Ok(update)
    }

    /// Performs analysis and elaboration as one operation.
    ///
    /// When [`FrontendOptions::top`] is absent, slang chooses the unique top or
    /// reports ambiguity.
    ///
    /// # Errors
    ///
    /// Returns the same source, option, native frontend, lowering, IR, and
    /// runtime failures as the separate analysis/elaboration flow.
    pub fn read_verilog(
        files: &[PathBuf],
        options: &FrontendOptions,
        runtime: &opto_runtime::ExecutionContext,
    ) -> Result<DbUpdate, HdlError> {
        require_files(files)?;

        let compilation =
            opto_slang_sys::compile_lazy(files, &slang_options(options, runtime.parallelism()))
                .map_err(HdlError::Slang)?;
        let diagnostics = compilation.diagnostics().to_vec();
        let mut update = lower::compilation(&compilation, options, runtime)?;
        update.diagnostics = diagnostics;
        Ok(update)
    }
}

fn require_files(files: &[PathBuf]) -> Result<(), HdlError> {
    if files.is_empty() {
        return Err(HdlError::NoInputFiles);
    }
    Ok(())
}

fn slang_source_units(
    files: &[PathBuf],
    options: &FrontendOptions,
    runtime: &opto_runtime::ExecutionContext,
) -> Result<Vec<SlangSourceUnit>, HdlError> {
    let sources =
        runtime.analyze_indexed_with_grain(files.len(), std::num::NonZeroUsize::MIN, |index| {
            let path = &files[index];
            std::fs::read_to_string(path)
                .map(|text| SlangSourceFile {
                    path: path.clone(),
                    text,
                })
                .map_err(|source| HdlError::ReadSource {
                    path: path.clone(),
                    source,
                })
        })?;
    Ok(sources
        .into_iter()
        .map(|source| SlangSourceUnit {
            files: vec![source],
            dependencies: Vec::new(),
            include_paths: options.include_paths.clone(),
            defines: slang_defines(options),
            language: slang_language(options.language),
        })
        .collect())
}

fn slang_options(options: &FrontendOptions, max_threads: usize) -> SlangCompileOptions {
    SlangCompileOptions {
        top: options.top.clone(),
        include_paths: options.include_paths.clone(),
        defines: slang_defines(options),
        language: slang_language(options.language),
        max_threads: Some(max_threads),
    }
}

fn slang_language(language: VerilogLanguage) -> SlangLanguage {
    match language {
        VerilogLanguage::Verilog2005 => SlangLanguage::Verilog2005,
        VerilogLanguage::SystemVerilog2017 => SlangLanguage::SystemVerilog2017,
    }
}

fn slang_defines(options: &FrontendOptions) -> Vec<SlangDefine> {
    options
        .defines
        .iter()
        .map(|(name, value)| SlangDefine {
            name: name.clone(),
            value: value.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
