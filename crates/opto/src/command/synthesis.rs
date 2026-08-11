// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(TclCommand)]
#[command(name = "synth", handler = synth)]
pub(crate) struct SynthArgs {}

#[derive(TclCommand)]
#[command(name = "report_area", handler = report_area)]
pub(crate) struct ReportAreaArgs {}

#[derive(TclCommand)]
#[command(name = "report_qor", handler = report_qor)]
pub(crate) struct ReportQorArgs {}

#[derive(TclCommand)]
#[command(name = "report_resources", handler = report_resources)]
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
    let mut observer = |event: SynthesisEvent| {
        crate::ui::print_progress(&synthesis_event_text(&event), state.ui, state.interactive);
        let _ = io::stdout().flush();
    };
    let trace = std::sync::Arc::new(OptimizationTrace::new());
    let heartbeat = OptimizationHeartbeat::spawn(std::sync::Arc::clone(&trace));
    let trace_sink = std::sync::Arc::clone(&trace);
    let result = state
        .session
        .borrow_mut()
        .synthesize_traced(&mut observer, &move |event| {
            trace_sink.record(event);
        });
    drop(heartbeat);
    result.map_err(crate::ShellError::from)
}

struct OptimizationTrace {
    started: std::time::Instant,
    reported_seconds: std::sync::atomic::AtomicU64,
    active_stages: std::sync::Mutex<Vec<ActiveStage>>,
}

struct ActiveStage {
    design: std::sync::Arc<str>,
    stage: opto_session::StageId,
    started: std::time::Instant,
}

struct OptimizationHeartbeat {
    stop: std::sync::mpsc::SyncSender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl OptimizationTrace {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            reported_seconds: std::sync::atomic::AtomicU64::new(0),
            active_stages: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn record(&self, event: opto_session::SynthesisTrace) {
        use std::sync::atomic::Ordering;

        let progress = &event.progress;
        if progress.optimization.is_none() {
            self.record_stage(event.design, progress.stage, progress.status);
            return;
        }
        let Some(area) = progress.area else {
            return;
        };
        let Some(phase) = progress.optimization else {
            return;
        };
        let elapsed = self.started.elapsed().as_secs();
        let elapsed_stamp = elapsed.saturating_add(1);
        if self
            .reported_seconds
            .fetch_max(elapsed_stamp, Ordering::Relaxed)
            >= elapsed_stamp
        {
            return;
        }
        let wns = progress
            .worst_slack
            .map_or_else(|| "--".to_string(), |wns| format!("{wns:.2}"));
        let tns = progress
            .total_negative_slack
            .map_or_else(|| "--".to_string(), |tns| format!("{tns:.1}"));
        let violations = progress
            .violations
            .map_or_else(|| "--".to_string(), |count| count.to_string());
        let cells = progress
            .cells
            .map_or_else(|| "--".to_string(), |cells| cells.to_string());
        println!(
            "Optimization: elapsed={:02}:{:02}:{:02} phase=\"{}\" area={area:.1} \
             worst_slack={wns} total_negative_slack={tns} violations={violations} cells={cells}",
            elapsed / 3600,
            (elapsed / 60) % 60,
            elapsed % 60,
            phase_title(phase),
        );
        let _ = io::stdout().flush();
    }

    fn record_stage(
        &self,
        design: std::sync::Arc<str>,
        stage: opto_session::StageId,
        status: opto_session::SynthesisProgressStatus,
    ) {
        let now = std::time::Instant::now();
        let (status_text, stage_elapsed) = match status {
            opto_session::SynthesisProgressStatus::Started => {
                self.active_stages
                    .lock()
                    .expect("synthesis stage trace lock is poisoned")
                    .push(ActiveStage {
                        design: std::sync::Arc::clone(&design),
                        stage,
                        started: now,
                    });
                ("started", None)
            }
            opto_session::SynthesisProgressStatus::Completed
            | opto_session::SynthesisProgressStatus::Failed => {
                let mut active = self
                    .active_stages
                    .lock()
                    .expect("synthesis stage trace lock is poisoned");
                let elapsed = active
                    .iter()
                    .rposition(|active| active.design == design && active.stage == stage)
                    .map(|index| now.duration_since(active.remove(index).started));
                let status = match status {
                    opto_session::SynthesisProgressStatus::Completed => "completed",
                    opto_session::SynthesisProgressStatus::Failed => "failed",
                    opto_session::SynthesisProgressStatus::Started => unreachable!(),
                };
                (status, elapsed)
            }
        };
        let elapsed = format_elapsed(self.started.elapsed());
        let stage_elapsed = stage_elapsed.map_or_else(|| "--:--:--".to_string(), format_elapsed);
        println!(
            "Synthesis stage: elapsed={elapsed} design=\"{design}\" stage=\"{}\" status={status_text} stage_elapsed={stage_elapsed}",
            stage_title(stage),
        );
        let _ = io::stdout().flush();
    }

    fn heartbeat(&self) {
        let active = self
            .active_stages
            .lock()
            .expect("synthesis stage trace lock is poisoned");
        let Some(active) = active.last() else {
            return;
        };
        println!(
            "Synthesis heartbeat: elapsed={} design=\"{}\" stage=\"{}\" stage_elapsed={}",
            format_elapsed(self.started.elapsed()),
            active.design,
            stage_title(active.stage),
            format_elapsed(active.started.elapsed()),
        );
        let _ = io::stdout().flush();
    }
}

impl OptimizationHeartbeat {
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

    fn spawn(trace: std::sync::Arc<OptimizationTrace>) -> Self {
        let (stop, receiver) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::Builder::new()
            .name("opto-progress".to_string())
            .spawn(move || {
                while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    receiver.recv_timeout(Self::INTERVAL)
                {
                    trace.heartbeat();
                }
            })
            .expect("failed to create synthesis progress heartbeat");
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for OptimizationHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
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
                SynthesisEffort::Low => "low effort",
                SynthesisEffort::Medium => "medium effort",
                SynthesisEffort::High => "high effort",
            };
            format!(
                "Beginning technology mapping for '{design}' with {parallelism} workers \
                 ({effort}).\n"
            )
        }
        SynthesisEvent::ArtifactCompleted { design, metrics } => {
            format!(
                "Synthesis artifact for '{design}' is complete; preparing the mapped object \
                 index.\nRegional synthesis: regions={} rebuilt={} reused={} plans={} epochs={}.\n",
                metrics.synthesis_regions,
                metrics.regional_decision_misses,
                metrics.regional_decision_hits,
                metrics.regional_cover_plans,
                metrics.regional_epochs,
            )
        }
        SynthesisEvent::DesignInformationUpdateStarted { design, effort: _ } => {
            format!("Publishing mapped design information for '{design}'.\n")
        }
        SynthesisEvent::Completed {
            design,
            synthesized: false,
        } => format!(
            "Information: Design '{design}' is unchanged; reusing synthesized root artifact.\n"
        ),
        SynthesisEvent::Completed {
            design: _,
            synthesized: true,
        } => "Optimization complete.\n".to_string(),
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
    interp: *mut TclInterp,
    command: &'static str,
    _args: SynthArgs,
) -> Result<CommandResult, crate::ShellError> {
    let _ = (interp, command);
    synth_command(state).map(CommandResult::Complete)
}

pub(crate) fn report_area(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    _args: ReportAreaArgs,
) -> Result<CommandResult, crate::ShellError> {
    let _ = (interp, command);
    state
        .session
        .borrow()
        .report_area()
        .map(CommandResult::Complete)
        .map_err(crate::ShellError::from)
}

pub(crate) fn report_qor(
    state: &ShellState,
    interp: *mut TclInterp,
    command: &'static str,
    _args: ReportQorArgs,
) -> Result<CommandResult, crate::ShellError> {
    let _ = (interp, command);
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
    command: &'static str,
    args: ReportResourcesArgs<'_>,
) -> Result<CommandResult, crate::ShellError> {
    let _ = command;
    report_resources_command(state, interp, args).map(CommandResult::Complete)
}
