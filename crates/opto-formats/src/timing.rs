// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{MessageKind, ReportDocument, ReportField, ReportTable};
use opto_timing::{
    CheckTimingAnalysis, ClockReportRow, ParasiticAnnotationRow, TimingAnalysis, TimingRequirement,
};

/// Format timing paths in analysis order using each path's display precision.
#[must_use]
pub fn report_timing(analyses: &[TimingAnalysis]) -> ReportDocument {
    let mut report = ReportDocument::new("Timing report");
    if let Some(first) = analyses.first() {
        report.fields([
            ReportField::new("Design", first.design()),
            ReportField::new("Path type", first.delay_type().report_name()),
            ReportField::new("Paths", analyses.len()),
        ]);
    }
    for (index, analysis) in analyses.iter().enumerate() {
        if analyses.len() > 1 {
            report.section(format!("Path {}", index + 1));
        }
        append_path(analysis, &mut report);
    }
    report
}

#[allow(
    clippy::too_many_lines,
    reason = "one path block follows the canonical timing-report section order"
)]
fn append_path(analysis: &TimingAnalysis, report: &mut ReportDocument) {
    report.fields([
        ReportField::new(
            "Startpoint",
            format!(
                "{} ({})",
                analysis.startpoint(),
                analysis.startpoint_description()
            ),
        ),
        ReportField::new(
            "Endpoint",
            format!(
                "{} ({})",
                analysis.endpoint(),
                analysis.endpoint_description()
            ),
        ),
        ReportField::new("Path group", analysis.path_group().unwrap_or("(none)")),
    ]);
    let library = analysis.library();
    if library.name().is_some()
        || library.operating_conditions().is_some()
        || library.wire_load().is_some()
        || library.wire_load_mode().is_some()
    {
        report.section("Library");
        report.fields([
            ReportField::new("Library", library.name().unwrap_or("-")),
            ReportField::new(
                "Operating conditions",
                library.operating_conditions().unwrap_or("-"),
            ),
            ReportField::new("Wire load model", library.wire_load().unwrap_or("-")),
            ReportField::new("Wire load mode", library.wire_load_mode().unwrap_or("-")),
        ]);
    }

    let digits = analysis.significant_digits();
    let mut rows = analysis
        .steps()
        .iter()
        .map(|step| {
            vec![
                step.point().to_string(),
                step.kind().report_name().to_string(),
                format!("{:.digits$}", step.increment()),
                format!("{:.digits$}", step.path()),
                step.edge().report_suffix().to_string(),
            ]
        })
        .collect::<Vec<_>>();
    rows.push(vec![
        analysis.endpoint_object().to_string(),
        "point".to_string(),
        format!("{:.digits$}", 0.0),
        format!("{:.digits$}", analysis.arrival()),
        analysis.endpoint_edge().report_suffix().to_string(),
    ]);
    report.section("Path");
    report.table(
        ReportTable::new(["Point", "Contribution", "Increment", "Path", "Edge"], rows)
            .expect("timing path rows match the static report schema"),
    );

    if let Some(requirement) = analysis.requirement() {
        report.section("Requirement");
        append_requirement(requirement, digits, report);
    }
    if let Some(exception) = analysis.path_exception() {
        let kind = match exception.kind() {
            opto_timing::PathExceptionKind::FalsePath => "False path",
            opto_timing::PathExceptionKind::MultiCycle { .. } => "Multicycle path",
            opto_timing::PathExceptionKind::MaxDelay { .. } => "Maximum delay",
            opto_timing::PathExceptionKind::MinDelay { .. } => "Minimum delay",
        };
        let mut fields = vec![
            ReportField::new("Type", kind),
            ReportField::new("Exception index", exception.index().to_string()),
            ReportField::new("Priority", exception.priority().to_string()),
        ];
        if !exception.comment().is_empty() {
            fields.push(ReportField::new("Comment", exception.comment()));
        }
        report.section("Path exception");
        report.fields(fields);
    }
    let mut summary = vec![ReportField::new(
        "Data arrival time",
        format!("{:.digits$}", analysis.arrival()),
    )];
    if let Some(required) = analysis.required() {
        summary.push(ReportField::new(
            "Data required time",
            format!("{required:.digits$}"),
        ));
    }
    if let Some(borrowed) = analysis.time_borrowed() {
        summary.push(ReportField::new(
            "Time borrowed",
            format!("{borrowed:.digits$}"),
        ));
    }
    let unconstrained = match analysis.slack() {
        Some(slack) => {
            summary.push(ReportField::new("Slack", format!("{slack:.digits$}")));
            summary.push(ReportField::new(
                "Status",
                if slack >= 0.0 { "MET" } else { "VIOLATED" },
            ));
            false
        }
        None => true,
    };
    report.section("Result");
    report.fields(summary);
    if unconstrained {
        report.message(MessageKind::Warning, "Path is unconstrained.");
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive requirement match keeps every timing-check schema visibly aligned"
)]
fn append_requirement(requirement: &TimingRequirement, digits: usize, report: &mut ReportDocument) {
    match requirement {
        TimingRequirement::MaxDelay => {
            report.fields([ReportField::new("Type", "Maximum delay")]);
        }
        TimingRequirement::MinDelay => {
            report.fields([ReportField::new("Type", "Minimum delay")]);
        }
        TimingRequirement::OutputDelay => {
            report.fields([ReportField::new("Type", "Output delay")]);
        }
        TimingRequirement::Setup {
            clock,
            clock_edge,
            capture_edge_time,
            clock_network_delay,
            clock_point,
            cell,
            constraint,
        } => {
            report.fields([
                ReportField::new("Type", "Setup"),
                ReportField::new("Clock", clock),
                ReportField::new("Clock edge", clock_edge.report_suffix()),
                ReportField::new("Capture edge time", format!("{capture_edge_time:.digits$}")),
                ReportField::new(
                    "Clock network delay",
                    format!("{clock_network_delay:.digits$}"),
                ),
                ReportField::new("Clock point", clock_point),
                ReportField::new("Cell", cell),
                ReportField::new("Library setup time", format!("{constraint:.digits$}")),
            ]);
        }
        TimingRequirement::Hold {
            clock,
            clock_edge,
            capture_edge_time,
            clock_network_delay,
            clock_point,
            cell,
            constraint,
        } => {
            report.fields([
                ReportField::new("Type", "Hold"),
                ReportField::new("Clock", clock),
                ReportField::new("Clock edge", clock_edge.report_suffix()),
                ReportField::new("Capture edge time", format!("{capture_edge_time:.digits$}")),
                ReportField::new(
                    "Clock network delay",
                    format!("{clock_network_delay:.digits$}"),
                ),
                ReportField::new("Clock point", clock_point),
                ReportField::new("Cell", cell),
                ReportField::new("Library hold time", format!("{constraint:.digits$}")),
            ]);
        }
        TimingRequirement::Recovery {
            clock,
            clock_edge,
            capture_edge_time,
            clock_network_delay,
            clock_point,
            cell,
            constraint,
        } => {
            report.fields([
                ReportField::new("Type", "Recovery"),
                ReportField::new("Clock", clock),
                ReportField::new("Clock edge", clock_edge.report_suffix()),
                ReportField::new("Capture edge time", format!("{capture_edge_time:.digits$}")),
                ReportField::new(
                    "Clock network delay",
                    format!("{clock_network_delay:.digits$}"),
                ),
                ReportField::new("Clock point", clock_point),
                ReportField::new("Cell", cell),
                ReportField::new("Library recovery time", format!("{constraint:.digits$}")),
            ]);
        }
        TimingRequirement::Removal {
            clock,
            clock_edge,
            capture_edge_time,
            clock_network_delay,
            clock_point,
            cell,
            constraint,
        } => {
            report.fields([
                ReportField::new("Type", "Removal"),
                ReportField::new("Clock", clock),
                ReportField::new("Clock edge", clock_edge.report_suffix()),
                ReportField::new("Capture edge time", format!("{capture_edge_time:.digits$}")),
                ReportField::new(
                    "Clock network delay",
                    format!("{clock_network_delay:.digits$}"),
                ),
                ReportField::new("Clock point", clock_point),
                ReportField::new("Cell", cell),
                ReportField::new("Library removal time", format!("{constraint:.digits$}")),
            ]);
        }
        TimingRequirement::PulseWidth {
            clock,
            pulse_edge,
            clock_point,
            cell,
            constraint,
        } => {
            report.fields([
                ReportField::new("Type", "Minimum pulse width"),
                ReportField::new("Clock", clock),
                ReportField::new("Pulse edge", pulse_edge.report_suffix()),
                ReportField::new("Clock point", clock_point),
                ReportField::new("Cell", cell),
                ReportField::new(
                    "Library minimum pulse width",
                    format!("{constraint:.digits$}"),
                ),
            ]);
        }
    }
}

/// Summarize missing clocks, input delays, and endpoint constraints.
///
/// # Panics
///
/// Panics only if a statically defined one-column diagnostics table is changed
/// inconsistently with its rows.
#[must_use]
pub fn report_timing_checks(analysis: &CheckTimingAnalysis) -> ReportDocument {
    let mut report = ReportDocument::new("Timing checks");
    if analysis.no_clocks() {
        report.message(
            MessageKind::Warning,
            "No clocks are defined in the current design.",
        );
    } else {
        report.message(MessageKind::Success, "Clock constraints are present.");
    }
    if analysis.missing_input_delays().is_empty() {
        report.message(MessageKind::Success, "All input delays are constrained.");
    } else {
        report.section("Inputs without delay constraints");
        report.table(
            ReportTable::new(
                ["Input port"],
                analysis
                    .missing_input_delays()
                    .iter()
                    .map(|port| [port.clone()]),
            )
            .expect("timing-check input rows match the static report schema"),
        );
    }
    if analysis.unconstrained_endpoints().is_empty() {
        report.message(MessageKind::Success, "All endpoints are constrained.");
    } else {
        report.section("Unconstrained endpoints");
        report.table(
            ReportTable::new(
                ["Endpoint"],
                analysis
                    .unconstrained_endpoints()
                    .iter()
                    .map(|endpoint| [endpoint.clone()]),
            )
            .expect("timing-check endpoint rows match the static report schema"),
        );
    }
    report
}

/// Format clocks in caller-supplied order, with times in the timing model unit.
///
/// # Panics
///
/// Panics only if the statically defined clock table schema is internally
/// inconsistent with its rows.
#[must_use]
pub fn report_clock(clocks: &[ClockReportRow]) -> ReportDocument {
    let mut report = ReportDocument::new("Clock report");
    if clocks.is_empty() {
        report.message(MessageKind::Information, "No clocks are defined.");
        return report;
    }
    report.table(
        ReportTable::new(
            ["Name", "Period", "Waveform", "Sources"],
            clocks.iter().map(|clock| {
                let waveform = clock.waveform.map_or_else(
                    || "-".to_string(),
                    |(rise, fall)| format!("{{{rise:.3} {fall:.3}}}"),
                );
                [
                    clock.name.clone(),
                    format!("{:.3}", clock.period),
                    waveform,
                    if clock.sources.is_empty() {
                        "<virtual>".to_string()
                    } else {
                        clock.sources.join(" ")
                    },
                ]
            }),
        )
        .expect("clock rows match the static report schema"),
    );
    report
}

/// Format parasitic coverage rows without mutating the timing model.
///
/// When `update` is true, the document records that the caller requested an
/// update; formatting itself never performs annotation or timing propagation.
///
/// # Panics
///
/// Panics only if the statically defined annotation table schema is internally
/// inconsistent with its rows.
#[must_use]
pub fn report_parasitic_annotations(
    design: &str,
    rows: &[ParasiticAnnotationRow],
    update: bool,
) -> ReportDocument {
    let mut report = ReportDocument::new("Parasitic annotations report");
    report.fields([
        ReportField::new("Design", design),
        ReportField::new("Version", format!("opto {}", env!("CARGO_PKG_VERSION"))),
        ReportField::new("Date", crate::report_timestamp()),
    ]);
    if update {
        report.message(MessageKind::Information, "Updating design information...");
    }
    if rows.is_empty() {
        report.message(MessageKind::Information, "No nets are annotated.");
        return report;
    }
    report.table(
        ReportTable::new(
            ["Net", "From", "To", "Rise", "Fall", "Load"],
            rows.iter().map(|row| {
                let rise = row
                    .delay
                    .map_or_else(|| "-".to_string(), |delay| format!("{:.2}", delay[0]));
                let fall = row
                    .delay
                    .map_or_else(|| "-".to_string(), |delay| format!("{:.2}", delay[1]));
                [
                    row.net.clone(),
                    row.from.clone(),
                    row.to.clone(),
                    rise,
                    fall,
                    format!("{:.2}", row.load),
                ]
            }),
        )
        .expect("parasitic annotation rows match the static report schema"),
    );
    report
}
