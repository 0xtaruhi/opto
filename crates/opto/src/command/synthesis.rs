// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(
    name = "synth",
    handler = synth,
    summary = "Synthesize the current design through Opto's single mapping pipeline.",
    requires = "A current elaborated design and a non-empty target library are required.",
    example = "synth"
)]
pub(crate) struct SynthArgs {}

#[derive(TclCommand)]
#[command(
    name = "report_area",
    handler = report_area,
    summary = "Report mapped cell area for the current synthesized design.",
    requires = "The current design must be synthesized with a loaded target library."
)]
pub(crate) struct ReportAreaArgs {}

#[derive(TclCommand)]
#[command(
    name = "report_qor",
    handler = report_qor,
    summary = "Report the current synthesis quality-of-results summary.",
    requires = "A completed synthesis result is required."
)]
pub(crate) struct ReportQorArgs {}

#[derive(TclCommand)]
#[command(
    name = "report_resources",
    handler = report_resources,
    summary = "Report inferred implementation resources for selected designs.",
    requires = "Selected designs must be elaborated; mapped details require synthesis.",
    example = "report_resources -hierarchy [get_db current_design]"
)]
pub(crate) struct ReportResourcesArgs<'a> {
    #[arg(long = "-hierarchy")]
    hierarchy: bool,
    #[arg(long = "-context", unsupported)]
    _context: (),
    #[arg(long = "-minpower", unsupported)]
    _minpower: (),
    #[arg(long = "-html_file_name", unsupported, value_hint = ValueHint::File)]
    _html_file_name: (),
    #[arg(positional, value_hint = ValueHint::Design)]
    designs: Option<TclArg<'a>>,
}

fn synth_command(state: &ShellState) -> Result<String, crate::ShellError> {
    let trace = std::sync::Arc::new(OptimizationTrace::new(state.ui));
    let event_trace = std::sync::Arc::clone(&trace);
    let mut observer = move |event: SynthesisEvent| event_trace.record_event(&event);
    let trace_sink = std::sync::Arc::clone(&trace);
    let result = state
        .session
        .borrow_mut()
        .synthesize_traced(&mut observer, &move |event| {
            trace_sink.record(event);
        });
    result.map_err(crate::ShellError::from)
}

struct OptimizationTrace {
    started: std::time::Instant,
    ui: crate::UiOptions,
    output: std::sync::Mutex<bool>,
}

impl OptimizationTrace {
    fn new(ui: crate::UiOptions) -> Self {
        Self {
            started: std::time::Instant::now(),
            ui,
            output: std::sync::Mutex::new(false),
        }
    }

    fn record_event(&self, event: &SynthesisEvent) {
        self.print(&synthesis_event_text(event));
    }

    fn record(&self, event: opto_session::SynthesisTrace) {
        match event.progress {
            opto_session::SynthesisProgress::Stage { stage, status } => {
                if status == opto_session::SynthesisProgressStatus::Completed {
                    return;
                }
                if status == opto_session::SynthesisProgressStatus::Started
                    && matches!(
                        stage,
                        opto_session::StageId::NORMALIZATION_CFG_ANALYSIS
                            | opto_session::StageId::NORMALIZATION_PROCEDURE_COMMIT
                    )
                {
                    return;
                }
                let (level, action) = match status {
                    opto_session::SynthesisProgressStatus::Started => ("Info", "Running"),
                    opto_session::SynthesisProgressStatus::Failed => ("Error", "Failed"),
                    opto_session::SynthesisProgressStatus::Completed => unreachable!(),
                };
                self.print(&format!(
                    "{level:<8}: {action} {} for '{}' (elapsed {}).\n",
                    stage_title(stage),
                    event.design,
                    format_elapsed(self.started.elapsed()),
                ));
            }
            opto_session::SynthesisProgress::Candidate {
                phase,
                area,
                cells,
                timing,
            } => self.record_candidate(event.design, phase, area, cells, timing),
        }
    }

    fn record_candidate(
        &self,
        design: std::sync::Arc<str>,
        phase: opto_session::OptimizationPhase,
        area: f64,
        cells: usize,
        timing: Option<opto_session::SynthesisTimingProgress>,
    ) {
        let [wns, tns, paths, evaluations] = timing.map_or_else(
            || std::array::from_fn(|_| "-".to_string()),
            |timing| {
                [
                    timing
                        .worst_slack
                        .map_or_else(|| "-".to_string(), |value| format!("{value:.2}")),
                    format!("{:.1}", timing.total_negative_slack),
                    timing.violations.to_string(),
                    timing.evaluations.to_string(),
                ]
            },
        );
        let mut header_printed = self
            .output
            .lock()
            .expect("synthesis progress output lock is poisoned");
        let first = !*header_printed;
        *header_printed = true;
        let table = crate::presentation::render_live_table(
            first.then_some(QOR_HEADERS.as_slice()),
            &[
                phase_title(phase).to_string(),
                format_elapsed(self.started.elapsed()),
                format!("{area:.1}"),
                cells.to_string(),
                wns,
                tns,
                paths,
                evaluations,
            ],
            &QOR_COLUMN_WIDTHS,
        );
        crate::ui::print_progress(
            &if first {
                format!("\nOptimization Summary: {design}\n{table}\n")
            } else {
                format!("{table}\n")
            },
            self.ui,
        );
        let _ = io::stdout().flush();
    }

    fn print(&self, text: &str) {
        let _header_printed = self
            .output
            .lock()
            .expect("synthesis progress output lock is poisoned");
        crate::ui::print_progress(text, self.ui);
        let _ = io::stdout().flush();
    }
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60,
    )
}

const QOR_HEADERS: [&str; 8] = [
    "Step", "Elapsed", "Area", "Cells", "WNS", "TNS", "Paths", "Eval",
];
const QOR_COLUMN_WIDTHS: [u16; 8] = [29, 9, 11, 10, 10, 11, 8, 7];

fn stage_title(stage: opto_session::StageId) -> &'static str {
    match stage {
        opto_session::StageId::LINKED_ELABORATION => "Linked Hierarchy Elaboration",
        opto_session::StageId::NORMALIZATION => "RTL Normalization",
        opto_session::StageId::NORMALIZATION_CFG_ANALYSIS => "Parallel Procedure CFG Analysis",
        opto_session::StageId::NORMALIZATION_PROCEDURE_COMMIT => "Deterministic Procedure Commit",
        opto_session::StageId::REGIONAL_PLANNING => "Regional Planning",
        opto_session::StageId::LOGIC_LOWERING => "Logic Lowering",
        opto_session::StageId::INITIAL_MAPPING => "Initial Technology Mapping",
        opto_session::StageId::MAPPED_NETLIST => "Mapped Netlist Construction",
        opto_session::StageId::POSTMAP_OPTIMIZATION => "Post-map Optimization",
        opto_session::StageId::FINALIZATION => "Artifact Finalization",
        _ => stage.as_str(),
    }
}

fn phase_title(phase: opto_session::OptimizationPhase) -> &'static str {
    match phase {
        opto_session::OptimizationPhase::TechnologyMapping => "Technology Mapping",
        opto_session::OptimizationPhase::BooleanResynthesis => "Boolean Resynthesis",
        opto_session::OptimizationPhase::RegisterOptimization => "Register Optimization",
        opto_session::OptimizationPhase::MonotonicSizing => "Monotonic Gate Sizing",
        opto_session::OptimizationPhase::TradeoffSizing => "Area/Delay Gate Sizing",
        opto_session::OptimizationPhase::FanoutTreeSynthesis => "Fanout Tree Synthesis",
        opto_session::OptimizationPhase::CriticalFanoutCloning => "Residual Fanout Cloning",
        opto_session::OptimizationPhase::DesignRuleRepair => "Design Rule Repair",
        opto_session::OptimizationPhase::PinSwap => "Pin Swap Optimization",
    }
}

pub(crate) fn synthesis_event_text(event: &SynthesisEvent) -> String {
    match event {
        SynthesisEvent::Started {
            design,
            effort,
            parallelism,
        } => {
            let effort = match effort {
                SynthesisEffort::Low => "low",
                SynthesisEffort::Medium => "medium",
                SynthesisEffort::High => "high",
            };
            let workers = if *parallelism == 1 {
                "worker"
            } else {
                "workers"
            };
            format!(
                "Info    : Synthesizing '{design}' using '{effort}' effort with {parallelism} {workers}.\n"
            )
        }
        SynthesisEvent::ArtifactCompleted { design, metrics } => format!(
            "Info    : Mapped '{design}': cells={} nets={} regions={} rebuilt={} reused={}.\n",
            metrics.mapped_cells,
            metrics.mapped_nets,
            metrics.synthesis_regions,
            metrics.regional_decision_misses,
            metrics.regional_decision_hits,
        ),
        SynthesisEvent::DesignInformationUpdateStarted { design, .. } => {
            format!("Info    : Publishing mapped design information for '{design}'.\n")
        }
        SynthesisEvent::Completed {
            design,
            synthesized: false,
        } => format!("Info    : Reused synthesized artifact for '{design}'.\n"),
        SynthesisEvent::Completed {
            design,
            synthesized: true,
        } => format!("Info    : Done synthesizing '{design}'.\n"),
    }
}

fn report_resources_command(
    state: &ShellState,
    interp: *mut TclInterp,
    args: ReportResourcesArgs<'_>,
) -> Result<String, crate::ShellError> {
    let mut hierarchy = args.hierarchy;
    let designs = args
        .designs
        .map(|value| split_tcl_list(interp, &value))
        .transpose()?
        .unwrap_or_default();
    if hierarchy && !designs.is_empty() {
        hierarchy = false;
    }
    state
        .session
        .borrow()
        .report_resources(&designs, hierarchy)
        .map_err(crate::ShellError::from)
}

pub(crate) fn synth(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: SynthArgs,
) -> Result<CommandResult, crate::ShellError> {
    synth_command(state).map(CommandResult::Complete)
}

pub(crate) fn report_area(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: ReportAreaArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .report_area()
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn report_qor(
    state: &ShellState,
    _interp: *mut TclInterp,
    _command: &'static str,
    _args: ReportQorArgs,
) -> Result<CommandResult, crate::ShellError> {
    state
        .session
        .borrow()
        .report_qor()
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn report_resources(
    state: &ShellState,
    interp: *mut TclInterp,
    _command: &'static str,
    args: ReportResourcesArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    report_resources_command(state, interp, args).map(CommandResult::Complete)
}
