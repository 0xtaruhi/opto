// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::process::run as run_process;
use super::schema::{Case, EquivalenceStatus, ResultEntry, ResultStatus, TimingResult, ToolResult};
use super::{case_inputs, case_tcl, prepare_case_output, yosys_quote};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub(super) struct OptoMeasurement {
    pub(super) liberty: PathBuf,
    pub(super) output: PathBuf,
    pub(super) result: ToolResult,
}

pub(super) fn measure_opto(case: &Case, opto: &Path, output_root: &Path) -> OptoMeasurement {
    let liberty = resolve_liberty(case);
    assert!(
        liberty.is_file(),
        "missing Liberty file {}",
        liberty.display()
    );
    let target_cells = target_cell_names(&liberty);
    let output = prepare_case_output(output_root, &case.spec.id);
    let opto_script = output.join("opto.tcl");
    std::fs::write(&opto_script, case_tcl(case, &output, Some(&liberty))).expect("write QoR Tcl");
    let opto_run = run_process(
        opto,
        vec![
            OsString::from("--no-init"),
            OsString::from("-f"),
            opto_script.as_os_str().to_owned(),
        ],
        &BTreeMap::new(),
        &output.join("opto.log"),
        true,
    );
    assert!(
        opto_run.status.success(),
        "Opto QoR case {} failed; see {}",
        case.spec.id,
        output.join("opto.log").display()
    );
    let (opto_area, opto_cells) = parse_opto_area(&output.join("area.rpt"));
    let opto_timing = case.spec.clock_period.map(|clock_period| {
        parse_opto_timing(&output.join("qor.rpt"), clock_period)
            .expect("Opto QoR timing fields are present")
    });
    let opto_histogram = cell_histogram(&output.join("mapped.v"), &target_cells);
    assert_histogram_is_complete("Opto", opto_cells, &opto_histogram);

    OptoMeasurement {
        liberty,
        output,
        result: ToolResult {
            area: opto_area,
            cells: opto_cells,
            cell_histogram: opto_histogram,
            metrics: opto_run.metrics,
            timing: opto_timing,
        },
    }
}

pub(super) fn run(case: &Case, opto: &Path, yosys: &Path, output_root: &Path) -> ResultEntry {
    let OptoMeasurement {
        liberty,
        output,
        result: opto_result,
    } = measure_opto(case, opto, output_root);
    let target_cells = target_cell_names(&liberty);

    let sources = case
        .sources()
        .iter()
        .map(yosys_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let read_flag = if case.spec.language == "sverilog" {
        " -sv"
    } else {
        ""
    };
    let commands = [
        format!("read_verilog{read_flag} {sources}"),
        format!("hierarchy -check -top {}", case.spec.top),
        "proc; flatten; opt; memory; opt; techmap; opt".to_string(),
        format!("dfflibmap -liberty {}", yosys_quote(&liberty)),
        yosys_abc_command(case.spec.clock_period, &liberty),
        "clean -purge".to_string(),
        format!(
            "stat -liberty {} -top {}",
            yosys_quote(&liberty),
            case.spec.top
        ),
        format!(
            "write_verilog -noattr -noexpr -nodec {}",
            yosys_quote(output.join("yosys.v"))
        ),
    ];
    let yosys_run = run_process(
        yosys,
        vec![
            OsString::from("-Q"),
            OsString::from("-p"),
            OsString::from(commands.join("; ")),
        ],
        &BTreeMap::new(),
        &output.join("yosys.log"),
        true,
    );
    assert!(
        yosys_run.status.success(),
        "Yosys QoR case {} failed; see {}",
        case.spec.id,
        output.join("yosys.log").display()
    );
    let mut diagnostics = Vec::new();
    let yosys_timing = case.spec.clock_period.and_then(|clock_period| {
        let script = output.join("yosys-timing.tcl");
        let report = output.join("yosys-qor.rpt");
        std::fs::write(
            &script,
            mapped_timing_tcl(case, &liberty, &output.join("yosys.v"), &report),
        )
        .expect("write Yosys mapped-netlist timing Tcl");
        let run = run_process(
            opto,
            [
                OsString::from("--no-init"),
                OsString::from("-f"),
                script.as_os_str().to_owned(),
            ],
            &BTreeMap::new(),
            &output.join("yosys-timing.log"),
            false,
        );
        if run.status.success() {
            let timing = parse_opto_timing(&report, clock_period);
            if timing.is_none() {
                diagnostics.push(format!(
                    "Yosys+ABC mapped-netlist timing report is incomplete; see {}",
                    report.display()
                ));
            }
            timing
        } else {
            diagnostics.push(format!(
                "Yosys+ABC mapped-netlist timing analysis failed; see {}",
                output.join("yosys-timing.log").display()
            ));
            None
        }
    });
    let (yosys_area, yosys_cells) = parse_yosys_area(&output.join("yosys.log"));
    let yosys_histogram = cell_histogram(&output.join("yosys.v"), &target_cells);
    assert_histogram_is_complete("Yosys+ABC", yosys_cells, &yosys_histogram);
    let opto = opto_result;
    diagnostics.extend(expectation_failures(case, &opto));
    let equivalence = if case.spec.equivalence {
        match super::run_equivalence(
            case,
            yosys,
            &liberty,
            &output.join("mapped.v"),
            &output.join("equivalence.log"),
        ) {
            Ok(()) => EquivalenceStatus::Pass,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                EquivalenceStatus::Fail
            }
        }
    } else {
        EquivalenceStatus::NotRequested
    };
    let status = if diagnostics.is_empty() {
        ResultStatus::Pass
    } else {
        ResultStatus::Fail
    };
    ResultEntry {
        id: case.spec.id.clone(),
        kind: case.spec.kind,
        status,
        diagnostics,
        inputs: case_inputs(case, Some(&liberty)),
        category: case.spec.category.clone(),
        class: case.spec.class.clone(),
        scenario: case.spec.scenario.clone(),
        opto: Some(opto),
        yosys_abc: Some(ToolResult {
            area: yosys_area,
            cells: yosys_cells,
            cell_histogram: yosys_histogram,
            metrics: yosys_run.metrics,
            timing: yosys_timing,
        }),
        equivalence,
    }
}

fn mapped_timing_tcl(case: &Case, liberty: &Path, netlist: &Path, report: &Path) -> String {
    let mut script = String::from("# Generated by the Opto QoR harness\n");
    writeln!(script, "read_libs {}", super::tcl_word(liberty)).unwrap();
    writeln!(script, "read_hdl [list {}]", super::tcl_word(netlist)).unwrap();
    writeln!(script, "elaborate {}", case.spec.top).unwrap();
    for constraint in &case.spec.constraints {
        writeln!(script, "{constraint}").unwrap();
    }
    writeln!(
        script,
        "redirect -file {} {{ report_qor }}",
        super::tcl_word(report)
    )
    .unwrap();
    script.push_str("exit\n");
    script
}

fn yosys_abc_command(clock_period_ns: Option<f64>, liberty: &Path) -> String {
    let mut command = format!("abc -liberty {}", yosys_quote(liberty));
    if let Some(clock_period_ns) = clock_period_ns {
        let delay_picoseconds = clock_period_ns * 1_000.0;
        write!(command, " -D {delay_picoseconds:.6}").expect("writing to a String cannot fail");
    }
    command
}

fn resolve_liberty(case: &Case) -> PathBuf {
    match (&case.spec.library, &case.spec.library_key) {
        (Some(relative), None) => case.relative_path(relative),
        (None, Some(library_key)) => {
            let variable = format!("OPTO_LIBRARY_{}", library_key.to_ascii_uppercase());
            std::env::var_os(&variable).map_or_else(
                || panic!("{variable} must point to a Liberty file"),
                PathBuf::from,
            )
        }
        _ => unreachable!("validated exactly one QoR library source"),
    }
}

pub(super) fn target_cell_names(liberty: &Path) -> BTreeSet<String> {
    let library = opto_library::read_lib_input(liberty)
        .unwrap_or_else(|error| panic!("parse QoR Liberty {}: {error}", liberty.display()));
    let names = library
        .target_cells()
        .iter()
        .map(|cell| cell.name().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names.len(),
        library.target_cells().len(),
        "QoR Liberty {} contains duplicate cell names",
        liberty.display()
    );
    assert!(
        !names.is_empty(),
        "QoR Liberty {} contains no cells",
        liberty.display()
    );
    names
}

pub(super) fn assert_histogram_is_complete(
    tool: &str,
    reported_cells: u64,
    histogram: &BTreeMap<String, u64>,
) {
    let histogram_cells = histogram
        .values()
        .try_fold(0_u64, |total, count| total.checked_add(*count))
        .expect("mapped cell histogram count overflow");
    assert_eq!(
        histogram_cells, reported_cells,
        "{tool} mapped netlist cell histogram is incomplete"
    );
}

/// Compare measured area, cell composition and timing against the recorded
/// baseline. These expectations describe the netlist itself, so they must hold
/// on every supported platform and build profile.
pub(super) fn quality_expectation_failures(case: &Case, result: &ToolResult) -> Vec<String> {
    let mut failures = Vec::new();
    let expected_area = case.spec.expected_area.expect("validated expected_area");
    let area_tolerance = case.spec.area_tolerance.expect("validated area_tolerance");
    let maximum_area = expected_area * (1.0 + area_tolerance);
    if !upper_bound_f64(result.area, maximum_area) {
        failures.push(format!(
            "area regression: measured {:.6}, baseline {expected_area:.6}, tolerance {:.4}% (maximum {maximum_area:.6})",
            result.area,
            area_tolerance * 100.0,
        ));
    }

    if let Some(expected_cells) = case.spec.expected_cells {
        let tolerance = case
            .spec
            .cell_count_tolerance
            .expect("validated cell_count_tolerance");
        let maximum_cells = exact_cell_count(expected_cells) * (1.0 + tolerance);
        if !upper_bound_f64(exact_cell_count(result.cells), maximum_cells) {
            failures.push(format!(
                "cell-count regression: measured {}, baseline {expected_cells}, tolerance {:.4}% (maximum {maximum_cells:.3})",
                result.cells,
                tolerance * 100.0,
            ));
        }
    }

    if !case.spec.expected_cell_histogram.is_empty()
        && result.cell_histogram != case.spec.expected_cell_histogram
    {
        failures.push(format!(
            "cell-composition regression: measured {:?}, baseline {:?}",
            result.cell_histogram, case.spec.expected_cell_histogram
        ));
    }

    if let Some(expected_slack) = case.spec.expected_worst_slack {
        let measured_slack = result
            .timing
            .as_ref()
            .expect("validated timing result")
            .worst_slack;
        let tolerance = case
            .spec
            .worst_slack_tolerance
            .expect("validated worst_slack_tolerance");
        let minimum_slack = expected_slack - tolerance;
        if !lower_bound_f64(measured_slack, minimum_slack) {
            failures.push(format!(
                "worst-slack regression: measured {measured_slack:.6}, baseline {expected_slack:.6}, tolerance {tolerance:.6} (minimum {minimum_slack:.6})"
            ));
        }
    }

    if let Some(expected_tns) = case.spec.expected_total_negative_slack {
        let measured_tns = result
            .timing
            .as_ref()
            .expect("validated timing result")
            .total_negative_slack;
        let tolerance = case
            .spec
            .total_negative_slack_tolerance
            .expect("validated total_negative_slack_tolerance");
        let minimum_tns = expected_tns - tolerance;
        if !lower_bound_f64(measured_tns, minimum_tns) {
            failures.push(format!(
                "total-negative-slack regression: measured {measured_tns:.6}, baseline {expected_tns:.6}, tolerance {tolerance:.6} (minimum {minimum_tns:.6})"
            ));
        }
    }

    if let Some(maximum_paths) = case.spec.maximum_violating_paths {
        let measured_paths = result
            .timing
            .as_ref()
            .expect("validated timing result")
            .violating_paths;
        if measured_paths > maximum_paths {
            failures.push(format!(
                "violating-path regression: measured {measured_paths}, maximum {maximum_paths}"
            ));
        }
    }

    failures
}

fn exact_cell_count(value: u64) -> f64 {
    assert!(
        value <= (1_u64 << f64::MANTISSA_DIGITS),
        "cell count exceeds the exact integer range of f64"
    );
    #[allow(
        clippy::cast_precision_loss,
        reason = "the preceding bound proves that this integer is exactly representable as f64"
    )]
    {
        value as f64
    }
}

/// Compare quality against the baseline and resource use against the recorded
/// ceilings. Resource ceilings are only meaningful for a release-profile
/// measurement on a benchmark machine.
pub(super) fn expectation_failures(case: &Case, result: &ToolResult) -> Vec<String> {
    let mut failures = quality_expectation_failures(case, result);

    for (label, measured, maximum) in [
        (
            "wall time",
            result.metrics.wall_seconds,
            case.spec.maximum_wall_seconds,
        ),
        (
            "CPU time",
            result.metrics.cpu_seconds,
            case.spec.maximum_cpu_seconds,
        ),
    ] {
        if let Some(maximum) = maximum
            && !upper_bound_f64(measured, maximum)
        {
            failures.push(format!(
                "{label} regression: measured {measured:.3}s, maximum {maximum:.3}s"
            ));
        }
    }

    if let Some(maximum) = case.spec.maximum_peak_rss_kib
        && result.metrics.peak_rss_kib > maximum
    {
        failures.push(format!(
            "peak-RSS regression: measured {} KiB, maximum {maximum} KiB",
            result.metrics.peak_rss_kib
        ));
    }

    failures
}

fn upper_bound_f64(measured: f64, maximum: f64) -> bool {
    measured <= maximum.next_up()
}

fn lower_bound_f64(measured: f64, minimum: f64) -> bool {
    measured >= minimum.next_down()
}

pub(super) fn parse_opto_timing(path: &Path, clock_period: f64) -> Option<TimingResult> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(TimingResult {
        clock_period,
        critical_delay: report_value(&text, "Critical Path Length")?.parse().ok()?,
        worst_slack: report_value(&text, "Critical Path Slack")?.parse().ok()?,
        total_negative_slack: report_value(&text, "Total Negative Slack")?.parse().ok()?,
        violating_paths: report_value(&text, "No. of Violating Paths")?
            .parse()
            .ok()?,
    })
}

pub(super) fn parse_opto_area(path: &Path) -> (f64, u64) {
    let text = std::fs::read_to_string(path).expect("read Opto area report");
    let area = report_value(&text, "Total cell area").expect("Opto area is present");
    let cells = report_value(&text, "Number of cells").expect("Opto cell count is present");
    (
        area.parse().expect("Opto area is numeric"),
        cells.parse().expect("cell count is numeric"),
    )
}

fn parse_yosys_area(path: &Path) -> (f64, u64) {
    let text = std::fs::read_to_string(path).expect("read Yosys area report");
    let area = text
        .lines()
        .find(|line| line.contains("Chip area for module"))
        .and_then(|line| line.split_whitespace().last())
        .expect("Yosys area is present")
        .parse()
        .expect("Yosys area is numeric");
    let cells = parse_yosys_cell_count(&text).expect("Yosys cell count is present and numeric");
    (area, cells)
}

fn parse_yosys_cell_count(text: &str) -> Option<u64> {
    report_value(text, "Number of cells")
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            text.lines().find_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                if fields.len() == 3 && fields[2] == "cells" {
                    fields[0].parse::<u64>().ok()
                } else {
                    None
                }
            })
        })
}

fn report_value<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|line| line.trim_start().strip_prefix(label))?
        .trim_start_matches(':')
        .split_whitespace()
        .next()
}

pub(super) fn cell_histogram(
    path: &Path,
    target_cells: &BTreeSet<String>,
) -> BTreeMap<String, u64> {
    let text = std::fs::read_to_string(path).expect("read mapped netlist");
    cell_histogram_from_text(&text, target_cells)
}

fn cell_histogram_from_text(text: &str, target_cells: &BTreeSet<String>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    // Both harness-controlled writers put the cell type first and the opening
    // parenthesis on the instance declaration line. Exact Liberty membership
    // distinguishes instances from Verilog declarations and prefix collisions.
    for line in text.lines() {
        let line = line.split_once("//").map_or(line, |(code, _)| code);
        let Some(token) = line.split_whitespace().next() else {
            continue;
        };
        let cell = token.strip_prefix('\\').unwrap_or(token);
        if target_cells.contains(cell) && line.contains('(') {
            *counts.entry(cell.to_string()).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::{
        cell_histogram_from_text, lower_bound_f64, parse_yosys_cell_count, upper_bound_f64,
        yosys_abc_command,
    };
    use opto_library::{TargetTimingType, TimingCheckKind, TimingEdge};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    #[test]
    fn area_gate_accepts_improvement_and_boundary_but_rejects_regression() {
        assert!(upper_bound_f64(99.0, 100.0));
        assert!(upper_bound_f64(100.0, 100.0));
        assert!(!upper_bound_f64(100.01, 100.0));
    }

    #[test]
    fn slack_gate_accepts_improvement_and_boundary_but_rejects_regression() {
        assert!(lower_bound_f64(0.1, 0.0));
        assert!(lower_bound_f64(0.0, 0.0));
        assert!(!lower_bound_f64(-0.01, 0.0));
    }

    #[test]
    fn histogram_counts_only_exact_liberty_cell_names() {
        let names = BTreeSet::from(["AND2_X1".to_string(), "cell/escaped".to_string()]);
        let netlist = "\
            module top(input A, B, output Y);\n\
              AND2_X1 u0 (.A(A), .B(B), .Y(n));\n\
              AND2_X10 false_prefix_match (.A(A), .B(B), .Y());\n\
              \\cell/escaped u1 (.A(n), .Y(Y));\n\
              // AND2_X1 commented_out (.A(A));\n\
            endmodule\n";
        assert_eq!(
            cell_histogram_from_text(netlist, &names),
            BTreeMap::from([("AND2_X1".to_string(), 1), ("cell/escaped".to_string(), 1),])
        );
    }

    #[test]
    fn yosys_cell_count_supports_both_observed_stat_formats() {
        assert_eq!(parse_yosys_cell_count("  Number of cells: 36\n"), Some(36));
        assert_eq!(parse_yosys_cell_count("  36  55.5 cells\n"), Some(36));
    }

    #[test]
    fn timing_constrained_yosys_mapping_receives_the_same_delay_target() {
        assert_eq!(
            yosys_abc_command(Some(2.5), Path::new("test.lib")),
            "abc -liberty \"test.lib\" -D 2500.000000"
        );
    }

    #[test]
    fn checked_in_qor_library_preserves_its_characterized_timing_models() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../qualification/libraries/opto_test.lib");
        let library = opto_library::read_lib_input(&path).expect("parse checked-in QoR Liberty");
        let cell_names = library
            .target_cells()
            .iter()
            .map(|cell| cell.name().to_string())
            .collect::<BTreeSet<_>>();
        let characterized_cells = BTreeSet::from([
            "AND2_X1".to_string(),
            "BUF_X1".to_string(),
            "DFFSR_X1".to_string(),
            "DFF_X1".to_string(),
            "INV_X1".to_string(),
            "MUX2_X1".to_string(),
            "NAND2_X1".to_string(),
            "NOR2_X1".to_string(),
            "OR2_X1".to_string(),
            "TBUFN_X1".to_string(),
            "TBUF_X1".to_string(),
            "XNOR2_X1".to_string(),
            "XOR2_X1".to_string(),
        ]);
        assert!(characterized_cells.is_subset(&cell_names));
        let arcs = library
            .target_cells()
            .iter()
            .flat_map(opto_library::TargetCellRef::pins)
            .flat_map(opto_library::TargetPinRef::timing_arcs)
            .collect::<Vec<_>>();
        let combinational = arcs
            .iter()
            .copied()
            .filter(|arc| arc.timing_type() == TargetTimingType::Combinational)
            .collect::<Vec<_>>();
        assert_eq!(combinational.len(), 19);
        assert_eq!(
            arcs.iter()
                .filter(|arc| arc.timing_type() == TargetTimingType::ThreeStateEnable)
                .count(),
            2
        );
        assert_eq!(
            arcs.iter()
                .filter(|arc| arc.timing_type() == TargetTimingType::ThreeStateDisable)
                .count(),
            2
        );
        assert_eq!(
            arcs.iter()
                .filter(|arc| matches!(arc.timing_type(), TargetTimingType::ClockToQ(_)))
                .count(),
            2
        );
        assert_eq!(
            arcs.iter()
                .filter(|arc| matches!(
                    arc.timing_type(),
                    TargetTimingType::Check {
                        kind: TimingCheckKind::Setup,
                        ..
                    }
                ))
                .count(),
            2
        );
        for arc in combinational {
            for edge in [TimingEdge::Rise, TimingEdge::Fall] {
                assert_eq!(arc.delay_at(edge, None, None), Some(1.0));
                assert_eq!(arc.transition_at(edge, None, None), Some(0.1));
            }
        }
    }
}
