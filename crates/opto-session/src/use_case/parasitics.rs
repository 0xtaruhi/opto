// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod names;
mod network;

use super::timing_model;
use names::{NameTransform, validate_design};
use network::{complete_networks, network_has_loop, rc_networks};

use crate::{Session, SessionError};
use opto_formats::{Spef, parse_spef};
use opto_timing::{ParasiticAnalysisOptions, ParasiticDelayModel, Parasitics, TimingLibrary};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Model used to complete nets absent from imported parasitics.
pub enum ReadParasiticsCompletion {
    /// Complete missing nets with zero parasitics.
    Zero,
    /// Complete missing nets from the active wire-load model.
    WireLoad,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Controls for SPEF import.
pub struct ReadParasiticsOptions {
    /// Delay calculation model selected for imported RC networks.
    pub delay_model: ParasiticDelayModel,
    /// Merge with existing parasitics instead of replacing them.
    pub increment: bool,
    /// Treat SPEF capacitance as already including library pin capacitance.
    pub pin_capacitance_included: bool,
    /// Retain only lumped net capacitance, discarding distributed resistance.
    pub net_capacitance_only: bool,
    /// Completion policy for design nets missing from SPEF.
    pub completion: Option<ReadParasiticsCompletion>,
    /// Hierarchy prefix added while matching SPEF names.
    pub path: Option<String>,
    /// Hierarchy prefix removed while matching SPEF names.
    pub strip_path: Option<String>,
    /// Validate input without publishing parasitics.
    pub syntax_only: bool,
    /// Include detailed import diagnostics in command output.
    pub verbose: bool,
}

impl Default for ReadParasiticsOptions {
    fn default() -> Self {
        Self {
            delay_model: ParasiticDelayModel::None,
            increment: false,
            pin_capacitance_included: false,
            net_capacitance_only: false,
            completion: None,
            path: None,
            strip_path: None,
            syntax_only: false,
            verbose: false,
        }
    }
}

fn format_read_preamble(
    library: &TimingLibrary,
    parsed: &[(PathBuf, Spef)],
    loop_nets: &[Vec<String>],
) -> Result<String, SessionError> {
    let time = library.units.time_seconds.ok_or_else(|| {
        SessionError::state("read_parasitics: active_library_set has no time_unit declaration")
    })?;
    let capacitance = library.units.capacitance_farads.ok_or_else(|| {
        SessionError::state(
            "read_parasitics: active_library_set has no capacitive_load_unit declaration",
        )
    })?;
    if !time.is_finite() || time <= 0.0 || !capacitance.is_finite() || capacitance <= 0.0 {
        return Err(SessionError::state(
            "read_parasitics: active_library_set timing units must be positive",
        ));
    }
    let resistance = time / capacitance;
    let mut output = format!(
        "Information: Library unit = {:.6} ps. (SPEF-10)\n\
Information: Derived delay scale factor = {:.6}. (SPEF-11)\n\
Information: Library unit = {:.6} pF. (SPEF-10)\n\
Information: Derived capacitance scale factor = {:.6}. (SPEF-12)\n\
Information: Library unit = {:.6} kOhm. (SPEF-10)\n\
Information: Derived resistance scale factor = {:.6}. (SPEF-13)\n",
        time / 1e-12,
        1e-12 / time,
        capacitance / 1e-12,
        1e-12 / capacitance,
        resistance / 1e3,
        1.0 / resistance,
    );
    for (index, (path, spef)) in parsed.iter().enumerate() {
        let _ = write!(
            output,
            "\nReading {} ...\n\n\
Information: Path delimiter = {}. (SPEF-2)\n\
Information: Pin delimiter = {}. (SPEF-3)\n",
            path.display(),
            spef.divider,
            spef.delimiter,
        );
        for net in loop_nets.get(index).into_iter().flatten() {
            let _ = writeln!(
                output,
                "Warning: Net '{net}' contains an interconnection loop. The delays and transition times computed for this net may be inaccurate. (SPEF-19)"
            );
        }
    }
    let nets = parsed
        .iter()
        .map(|(_, spef)| spef.nets.len())
        .sum::<usize>();
    let _ = write!(
        output,
        "\n{nets} RNET/DNET {} been read.\n",
        if nets == 1 { "has" } else { "have" },
    );
    let loop_count = loop_nets.iter().map(Vec::len).sum::<usize>();
    if loop_count != 0 {
        let _ = write!(
            output,
            "\nWarning: '{loop_count}' nets with interconnection loops have been read. (SPEF-21)\n"
        );
    }
    Ok(output)
}

fn append_import_summary(
    output: &mut String,
    completion_steps: usize,
    delay_model: ParasiticDelayModel,
    annotation: opto_timing::ParasiticAnnotationSummary,
) {
    let _ = writeln!(
        output,
        "{completion_steps} net completion {} {} been performed.",
        if completion_steps == 1 {
            "step"
        } else {
            "steps"
        },
        if completion_steps == 1 { "has" } else { "have" },
    );
    if delay_model != ParasiticDelayModel::None {
        let _ = writeln!(
            output,
            "{} pin-to-pin {} {} been annotated on {} {}",
            annotation.pin_to_pin_delays,
            if annotation.pin_to_pin_delays == 1 {
                "delay"
            } else {
                "delays"
            },
            if annotation.pin_to_pin_delays == 1 {
                "has"
            } else {
                "have"
            },
            annotation.annotated_nets,
            if annotation.annotated_nets == 1 {
                "net"
            } else {
                "nets"
            },
        );
    }
    let _ = writeln!(
        output,
        "{} {} {} been skipped due to partial parasitics",
        annotation.skipped_nets,
        if annotation.skipped_nets == 1 {
            "net"
        } else {
            "nets"
        },
        if annotation.skipped_nets == 1 {
            "has"
        } else {
            "have"
        },
    );
}
impl Session {
    /// Parse and attach SPEF parasitics according to the requested options.
    pub fn read_parasitics(
        &mut self,
        files: &[PathBuf],
        options: &ReadParasiticsOptions,
    ) -> Result<String, SessionError> {
        if files.is_empty() {
            return Err(SessionError::state(
                "read_parasitics: no parasitic input file specified",
            ));
        }
        let mut parsed = Vec::with_capacity(files.len());
        for file in files {
            let path = self.resolve_lib_search_path_file("read_parasitics", file)?;
            let text = fs::read_to_string(&path).map_err(|source| SessionError::Io {
                operation: "read parasitics",
                path: path.clone(),
                source,
            })?;
            parsed.push((path, parse_spef(&text)?));
        }
        let design_name = self.current_design_name()?.to_string();
        if options.syntax_only {
            let library = self.timing_library()?;
            let mut output = format_read_preamble(&library, &parsed, &[])?;
            if options.verbose {
                let parasitics = self
                    .state
                    .parasitics
                    .get(&design_name)
                    .map(|(_, parasitics)| parasitics)
                    .cloned()
                    .unwrap_or_default();
                output.push('\n');
                let rows = parasitics.annotation_rows()?;
                output.push_str(
                    &opto_formats::report_parasitic_annotations(&design_name, &rows, false)
                        .render_plain(),
                );
            }
            return Ok(output);
        }

        let model = timing_model::current_timing_model(self)?;
        let library = model.library();
        let transform = NameTransform::new(options.path.as_deref(), options.strip_path.as_deref());
        let mut networks = Vec::new();
        let mut loop_nets = Vec::with_capacity(parsed.len());
        for (_, spef) in &parsed {
            validate_design(spef, &model, &transform)?;
            let file_networks = rc_networks(spef, &model, &transform, options)?;
            loop_nets.push(
                file_networks
                    .iter()
                    .filter(|network| network_has_loop(network))
                    .map(|network| network.name.clone())
                    .collect::<Vec<_>>(),
            );
            networks.extend(file_networks);
        }
        let completion_steps = complete_networks(&mut networks, library, options.completion)?;
        let imported = Parasitics::from_rc_networks(
            networks,
            library.units,
            ParasiticAnalysisOptions {
                delay_model: options.delay_model,
                pin_capacitance_included: options.pin_capacitance_included,
                net_capacitance_only: options.net_capacitance_only,
            },
        )?;
        let previous = self
            .state
            .parasitics
            .get(&design_name)
            .map(|(_, parasitics)| parasitics);
        let annotation =
            imported.annotation_summary(if options.increment { previous } else { None })?;
        let parasitics = match previous {
            Some(existing) => existing.overlay(imported, options.increment)?,
            None => imported,
        };
        let mut output = format_read_preamble(library, &parsed, &loop_nets)?;
        output.push_str("\n\n");
        append_import_summary(
            &mut output,
            completion_steps,
            options.delay_model,
            annotation,
        );
        if options.verbose {
            output.push('\n');
            let rows = parasitics.annotation_rows()?;
            output.push_str(
                &opto_formats::report_parasitic_annotations(&design_name, &rows, true)
                    .render_plain(),
            );
        }

        self.state.parasitics.validate_publish()?;
        drop(model);
        self.process.clear_analysis_caches();
        let timing_model = timing_model::current_timing_model_with_parasitics(
            self,
            &design_name,
            parasitics.clone(),
        )?;
        self.state.parasitics.publish(design_name, parasitics)?;
        timing_model::install_current_timing_model(self, timing_model)?;
        Ok(output)
    }
}
