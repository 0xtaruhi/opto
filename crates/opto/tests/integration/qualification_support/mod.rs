// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

mod differential;
mod formal;
mod inventory;
mod medium;
mod process;
mod schema;
mod semantics;
mod sv_tests;
mod yosys_tests;

use process::run;
use schema::{
    Case, CaseKind, EquivalenceStatus, Expectation, ResultDocument, ResultEntry, ResultStatus,
    Suite, ToolIdentity,
};
use sha2::{Digest, Sha256};
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunMode {
    Presubmit,
    Equivalence,
    Upstream,
    Qor,
}

pub(super) fn run_semantic_matrix(mode: RunMode) {
    semantics::run(mode);
}

pub(super) fn run_real_medium_gate(relative_path: &str) {
    medium::run(&workspace_root().join(relative_path));
}

/// Prove that every mapping fixture reproduces its recorded area and cell
/// composition using Opto alone.
///
/// These microcases protect a named mapping mechanism; representative quality
/// and resource acceptance belongs to the real medium-scale gate.
pub(super) fn run_mapping_fixture_gate(relative_path: &str) {
    let root = workspace_root();
    let suite_path = root.join(relative_path);
    let (suite, cases) = load_suite(&root, &suite_path);
    let output = output_directory(&format!("{}-quality", suite.name));
    if output.exists() {
        std::fs::remove_dir_all(&output)
            .unwrap_or_else(|error| panic!("remove {}: {error}", output.display()));
    }
    std::fs::create_dir_all(&output)
        .unwrap_or_else(|error| panic!("create {}: {error}", output.display()));
    let opto = PathBuf::from(env!("CARGO_BIN_EXE_opto"));
    assert!(!cases.is_empty(), "QoR quality gate selected no cases");

    let mut failures = Vec::new();
    for case in cases {
        assert_eq!(
            case.spec.kind,
            CaseKind::Qor,
            "case {} is not a QoR case",
            case.spec.id
        );
        let measurement = qor::measure_opto(&case, &opto, &output);
        for diagnostic in qor::quality_expectation_failures(&case, &measurement.result) {
            failures.push(format!("{}: {diagnostic}", case.spec.id));
        }
    }
    assert!(
        failures.is_empty(),
        "mapping-fixture baselines are host-dependent:\n{}",
        failures.join("\n")
    );
}

pub(super) fn run_generated_differential() {
    differential::run();
}

pub(super) fn validate_inventory() {
    inventory::validate();
}

pub(super) fn run_sv_tests() {
    sv_tests::run();
}

pub(super) fn run_yosys_tests() {
    yosys_tests::run();
}

pub(super) fn run_named_suite(relative_path: &str, mode: RunMode) {
    let root = workspace_root();
    let suite_path = root.join(relative_path);
    let (suite, mut cases) = load_suite(&root, &suite_path);
    if let Some(filter) = std::env::var_os("OPTO_REGRESSION_CASE") {
        let requested = filter
            .to_string_lossy()
            .split(',')
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        cases.retain(|case| requested.contains(&case.spec.id));
        assert_eq!(
            cases.len(),
            requested.len(),
            "OPTO_REGRESSION_CASE names a case outside the selected suite"
        );
    }
    let output = output_directory(&suite.name);
    if output.exists() {
        std::fs::remove_dir_all(&output)
            .unwrap_or_else(|error| panic!("remove {}: {error}", output.display()));
    }
    std::fs::create_dir_all(&output)
        .unwrap_or_else(|error| panic!("create {}: {error}", output.display()));
    let opto = if mode == RunMode::Qor {
        optional_executable("OPTO_QOR_BINARY")
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_opto")))
    } else {
        PathBuf::from(env!("CARGO_BIN_EXE_opto"))
    };
    let yosys = matches!(mode, RunMode::Equivalence | RunMode::Qor)
        .then(|| required_executable("OPTO_YOSYS"));
    let mut results = Vec::new();
    for case in cases {
        eprintln!("RUN  {} ({:?})", case.spec.id, case.spec.kind);
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match case.spec.kind {
                CaseKind::Regression => {
                    assert!(
                        matches!(mode, RunMode::Presubmit | RunMode::Equivalence),
                        "regression case {} belongs in a regression suite",
                        case.spec.id
                    );
                    run_regression_case(&case, &opto, yosys.as_deref(), &output, mode)
                }
                CaseKind::Upstream => {
                    assert_eq!(
                        mode,
                        RunMode::Upstream,
                        "upstream suite requires upstream mode"
                    );
                    run_upstream_case(&case, &opto, &output)
                }
                CaseKind::Qor => {
                    assert_eq!(mode, RunMode::Qor, "QoR suite requires QoR mode");
                    run_qor_case(
                        &case,
                        &opto,
                        yosys.as_deref().expect("QoR mode resolved Yosys"),
                        &output,
                    )
                }
            }))
            .unwrap_or_else(|payload| failed_result(&case, mode, panic_message(payload.as_ref())));
        if result.status == ResultStatus::Pass {
            eprintln!("PASS {}", case.spec.id);
        } else {
            eprintln!("FAIL {}: {}", case.spec.id, result.diagnostics.join("; "));
        }
        results.push(result);
    }
    std::fs::write(output.join("summary.tsv"), render_summary(&results))
        .expect("write regression summary");
    let document = ResultDocument {
        format: schema::FORMAT_VERSION,
        suite: suite.name,
        opto: tool_identity(&opto, &["-version"]),
        yosys: yosys.as_deref().map(|path| tool_identity(path, &["-V"])),
        results,
    };
    let serialized = serde_json::to_string_pretty(&document).expect("serialize regression results");
    std::fs::write(output.join("results.json"), format!("{serialized}\n"))
        .expect("write regression results");
    let failures = document
        .results
        .iter()
        .filter(|result| result.status != ResultStatus::Pass)
        .map(|result| result.id.as_str())
        .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "{} qualification case(s) failed: {}; see {}",
        failures.len(),
        failures.join(", "),
        output.display()
    );
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<String>().map_or_else(
        || {
            payload.downcast_ref::<&str>().map_or_else(
                || "qualification case panicked".to_string(),
                |text| (*text).to_string(),
            )
        },
        Clone::clone,
    )
}

fn failed_result(case: &Case, mode: RunMode, diagnostic: String) -> ResultEntry {
    let library = case
        .spec
        .library
        .as_deref()
        .map(|path| case.relative_path(path))
        .or_else(|| {
            case.spec.library_key.as_deref().and_then(|key| {
                std::env::var_os(format!("OPTO_LIBRARY_{}", key.to_ascii_uppercase()))
                    .map(PathBuf::from)
            })
        });
    ResultEntry {
        id: case.spec.id.clone(),
        kind: case.spec.kind,
        status: ResultStatus::Fail,
        diagnostics: vec![diagnostic],
        inputs: case_inputs(case, library.as_deref()),
        category: case.spec.category.clone(),
        class: case.spec.class.clone(),
        scenario: case.spec.scenario.clone(),
        opto: None,
        yosys_abc: None,
        equivalence: match (case.spec.kind, mode, case.spec.equivalence) {
            (CaseKind::Upstream, _, _) => EquivalenceStatus::NotAvailable,
            (_, RunMode::Equivalence, true) => EquivalenceStatus::Fail,
            _ => EquivalenceStatus::NotRequested,
        },
    }
}

fn render_summary(results: &[ResultEntry]) -> String {
    let mut text = String::from(
        "case\tkind\tstatus\tdiagnostics\tequivalence\topto_area\tyosys_area\topto_cells\tyosys_cells\topto_critical_delay\tyosys_critical_delay\topto_wns\tyosys_wns\topto_tns\tyosys_tns\topto_violating_paths\tyosys_violating_paths\topto_wall_s\tyosys_wall_s\topto_cpu_s\tyosys_cpu_s\topto_peak_rss_kib\tyosys_peak_rss_kib\topto_cell_histogram\tyosys_cell_histogram\n",
    );
    for result in results {
        let opto = result.opto.as_ref();
        let yosys = result.yosys_abc.as_ref();
        let timing = opto.and_then(|value| value.timing.as_ref());
        let yosys_timing = yosys.and_then(|value| value.timing.as_ref());
        let fields = [
            result.id.clone(),
            format!("{:?}", result.kind),
            result.status.as_str().to_string(),
            result
                .diagnostics
                .join("; ")
                .replace(['\t', '\n', '\r'], " "),
            result.equivalence.as_str().to_string(),
            opto.map_or_else(String::new, |value| value.area.to_string()),
            yosys.map_or_else(String::new, |value| value.area.to_string()),
            opto.map_or_else(String::new, |value| value.cells.to_string()),
            yosys.map_or_else(String::new, |value| value.cells.to_string()),
            timing.map_or_else(String::new, |value| value.critical_delay.to_string()),
            yosys_timing.map_or_else(String::new, |value| value.critical_delay.to_string()),
            timing.map_or_else(String::new, |value| value.worst_slack.to_string()),
            yosys_timing.map_or_else(String::new, |value| value.worst_slack.to_string()),
            timing.map_or_else(String::new, |value| value.total_negative_slack.to_string()),
            yosys_timing.map_or_else(String::new, |value| value.total_negative_slack.to_string()),
            timing.map_or_else(String::new, |value| value.violating_paths.to_string()),
            yosys_timing.map_or_else(String::new, |value| value.violating_paths.to_string()),
            opto.map_or_else(String::new, |value| value.metrics.wall_seconds.to_string()),
            yosys.map_or_else(String::new, |value| value.metrics.wall_seconds.to_string()),
            opto.map_or_else(String::new, |value| value.metrics.cpu_seconds.to_string()),
            yosys.map_or_else(String::new, |value| value.metrics.cpu_seconds.to_string()),
            opto.map_or_else(String::new, |value| value.metrics.peak_rss_kib.to_string()),
            yosys.map_or_else(String::new, |value| value.metrics.peak_rss_kib.to_string()),
            opto.map_or_else(String::new, |value| {
                serde_json::to_string(&value.cell_histogram).expect("serialize Opto cell histogram")
            }),
            yosys.map_or_else(String::new, |value| {
                serde_json::to_string(&value.cell_histogram)
                    .expect("serialize Yosys cell histogram")
            }),
        ];
        writeln!(text, "{}", fields.join("\t")).unwrap();
    }
    text
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonical workspace root")
}

fn output_directory(suite: &str) -> PathBuf {
    std::env::var_os("OPTO_REGRESSION_OUTPUT").map_or_else(
        || std::env::temp_dir().join(format!("opto-{suite}-{}", std::process::id())),
        |root| PathBuf::from(root).join(suite),
    )
}

fn load_suite(root: &Path, path: &Path) -> (Suite, Vec<Case>) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read suite {}: {error}", path.display()));
    let suite: Suite = toml::from_str(&text)
        .unwrap_or_else(|error| panic!("parse suite {}: {error}", path.display()));
    assert_eq!(
        suite.format,
        schema::FORMAT_VERSION,
        "suite format mismatch"
    );
    assert!(!suite.name.is_empty(), "suite name is empty");
    let mut identifiers = BTreeSet::new();
    let cases = suite
        .cases
        .iter()
        .map(|relative| {
            let case_path = root.join(relative);
            let text = std::fs::read_to_string(&case_path)
                .unwrap_or_else(|error| panic!("read case {}: {error}", case_path.display()));
            let spec = toml::from_str(&text)
                .unwrap_or_else(|error| panic!("parse case {}: {error}", case_path.display()));
            let case = Case {
                path: case_path,
                spec,
            };
            validate_case(&case);
            assert!(
                identifiers.insert(case.spec.id.clone()),
                "duplicate case id {}",
                case.spec.id
            );
            case
        })
        .collect();
    (suite, cases)
}

fn validate_case(case: &Case) {
    assert_eq!(
        case.spec.format,
        schema::FORMAT_VERSION,
        "case format mismatch"
    );
    assert!(!case.spec.id.is_empty(), "case id is empty");
    match case.spec.kind {
        CaseKind::Regression | CaseKind::Qor => {
            assert!(!case.spec.top.is_empty(), "case top is empty");
            assert!(!case.spec.sources.is_empty(), "case has no sources");
            assert!(
                matches!(case.spec.language.as_str(), "verilog" | "sverilog"),
                "unsupported language {}",
                case.spec.language
            );
            for source in case.sources() {
                assert!(source.is_file(), "missing case source {}", source.display());
            }
        }
        CaseKind::Upstream => {}
    }
    if case.spec.equivalence_initial_state.is_some() {
        assert!(
            case.spec.equivalence && case.spec.sequential,
            "equivalence_initial_state requires sequential equivalence"
        );
    }
    match case.spec.kind {
        CaseKind::Regression => {
            assert!(
                case.spec.category.is_some(),
                "regression case has no coverage category"
            );
        }
        CaseKind::Qor => {
            assert!(case.spec.class.is_some(), "QoR case has no class");
            assert!(case.spec.scenario.is_some(), "QoR case has no scenario");
            assert_ne!(
                case.spec.library.is_some(),
                case.spec.library_key.is_some(),
                "QoR case must declare exactly one of library or library_key"
            );
            assert!(
                matches!(
                    case.spec.flow,
                    schema::Flow::Synth | schema::Flow::SynthHigh
                ),
                "QoR cases must run a synthesis flow"
            );
            assert!(
                case.spec.script.is_none(),
                "QoR cases use the generated, audited synthesis flow"
            );
            assert!(
                case.spec
                    .expected_area
                    .is_some_and(|area| area.is_finite() && area >= 0.0),
                "QoR case requires a finite non-negative expected_area"
            );
            assert!(
                case.spec
                    .area_tolerance
                    .is_some_and(|tolerance| tolerance.is_finite() && tolerance >= 0.0),
                "QoR case requires a finite non-negative area_tolerance"
            );
            assert_eq!(
                case.spec.expected_cells.is_some(),
                case.spec.cell_count_tolerance.is_some(),
                "QoR expected_cells and cell_count_tolerance must be declared together"
            );
            if let Some(tolerance) = case.spec.cell_count_tolerance {
                assert!(
                    tolerance.is_finite() && tolerance >= 0.0,
                    "QoR cell_count_tolerance must be finite and non-negative"
                );
            }
            if !case.spec.expected_cell_histogram.is_empty() {
                let expected_cells = case
                    .spec
                    .expected_cells
                    .expect("cell histogram requires expected_cells");
                let histogram_cells = case
                    .spec
                    .expected_cell_histogram
                    .values()
                    .try_fold(0_u64, |total, count| total.checked_add(*count))
                    .expect("QoR cell histogram count overflow");
                assert_eq!(
                    histogram_cells, expected_cells,
                    "QoR cell histogram must sum to expected_cells"
                );
            }
            for (label, maximum) in [
                ("maximum_wall_seconds", case.spec.maximum_wall_seconds),
                ("maximum_cpu_seconds", case.spec.maximum_cpu_seconds),
            ] {
                if let Some(maximum) = maximum {
                    assert!(
                        maximum.is_finite() && maximum > 0.0,
                        "QoR {label} must be finite and positive"
                    );
                }
            }
            if let Some(maximum) = case.spec.maximum_peak_rss_kib {
                assert!(maximum > 0, "QoR maximum_peak_rss_kib must be positive");
            }
            match case.spec.scenario.as_deref() {
                Some("area_unconstrained") => {
                    assert!(
                        !case.spec.report_timing,
                        "unconstrained QoR case must not request timing reports"
                    );
                    assert!(
                        case.spec.clock_period.is_none(),
                        "unconstrained QoR case must not declare a clock period"
                    );
                    assert!(
                        case.spec.expected_worst_slack.is_none()
                            && case.spec.worst_slack_tolerance.is_none()
                            && case.spec.expected_total_negative_slack.is_none()
                            && case.spec.total_negative_slack_tolerance.is_none()
                            && case.spec.maximum_violating_paths.is_none(),
                        "unconstrained QoR case must not declare timing expectations"
                    );
                }
                Some("timing_constrained") => {
                    assert!(
                        case.spec.report_timing,
                        "timing QoR case must report timing"
                    );
                    assert!(
                        case.spec
                            .clock_period
                            .is_some_and(|period| period.is_finite() && period > 0.0),
                        "timing QoR case requires a positive finite clock period"
                    );
                    assert!(
                        !case.spec.constraints.is_empty(),
                        "timing QoR case requires synthesis constraints"
                    );
                    assert!(
                        case.spec.expected_worst_slack.is_some_and(f64::is_finite),
                        "timing QoR case requires a finite expected_worst_slack"
                    );
                    assert!(
                        case.spec
                            .worst_slack_tolerance
                            .is_some_and(|tolerance| { tolerance.is_finite() && tolerance >= 0.0 }),
                        "timing QoR case requires a finite non-negative worst_slack_tolerance"
                    );
                    assert_eq!(
                        case.spec.expected_total_negative_slack.is_some(),
                        case.spec.total_negative_slack_tolerance.is_some(),
                        "expected_total_negative_slack and total_negative_slack_tolerance must be declared together"
                    );
                    if let Some(expected) = case.spec.expected_total_negative_slack {
                        assert!(
                            expected.is_finite() && expected <= 0.0,
                            "expected_total_negative_slack must be finite and non-positive"
                        );
                    }
                    if let Some(tolerance) = case.spec.total_negative_slack_tolerance {
                        assert!(
                            tolerance.is_finite() && tolerance >= 0.0,
                            "total_negative_slack_tolerance must be finite and non-negative"
                        );
                    }
                }
                Some(other) => panic!("unsupported QoR scenario {other}"),
                None => unreachable!("QoR scenario presence was validated"),
            }
        }
        CaseKind::Upstream => {
            assert!(
                case.spec.source_root.is_some(),
                "upstream case has no source root"
            );
            assert!(
                case.spec.revision.is_some(),
                "upstream case has no revision"
            );
            assert!(
                case.spec.manifest.is_some(),
                "upstream case has no manifest"
            );
            assert!(case.spec.script.is_some(), "upstream case has no script");
        }
    }
}

fn run_regression_case(
    case: &Case,
    opto: &Path,
    yosys: Option<&Path>,
    output_root: &Path,
    mode: RunMode,
) -> ResultEntry {
    let output = prepare_case_output(output_root, &case.spec.id);
    let library = case
        .spec
        .library
        .as_deref()
        .map(|path| case.relative_path(path));
    let script = if let Some(script) = &case.spec.script {
        case.relative_path(script)
    } else {
        let path = output.join("run.tcl");
        std::fs::write(&path, case_tcl(case, &output, library.as_deref()))
            .expect("write generated regression Tcl");
        path
    };
    let environment = BTreeMap::from([
        (
            "OPTO_CASE_ROOT".to_string(),
            case.path.parent().expect("case parent").to_path_buf(),
        ),
        ("OPTO_CASE_OUTPUT".to_string(), output.clone()),
    ]);
    let run = run(
        opto,
        ["-no_init".as_ref(), "-f".as_ref(), script.as_os_str()],
        &environment,
        &output.join("opto.log"),
        false,
    );
    match case.spec.expect {
        Expectation::Pass => assert!(
            run.status.success(),
            "{} failed; see {}",
            case.spec.id,
            output.join("opto.log").display()
        ),
        Expectation::Fail => assert!(
            !run.status.success(),
            "{} was expected to fail",
            case.spec.id
        ),
    }
    assert_log(case, &output.join("opto.log"));
    if case.spec.expect == Expectation::Pass {
        assert_reports(case, &output);
        if mode == RunMode::Equivalence
            && case.spec.equivalence
            && let Err(diagnostic) = run_equivalence(
                case,
                yosys.expect("equivalence mode requires OPTO_YOSYS"),
                library.as_deref().expect("equivalence case has a library"),
                &output.join("mapped.v"),
                &output.join("equivalence.log"),
            )
        {
            panic!("{diagnostic}");
        }
    }
    ResultEntry {
        id: case.spec.id.clone(),
        kind: case.spec.kind,
        status: ResultStatus::Pass,
        diagnostics: Vec::new(),
        inputs: case_inputs(case, library.as_deref()),
        category: case.spec.category.clone(),
        class: None,
        scenario: None,
        opto: None,
        yosys_abc: None,
        equivalence: if mode == RunMode::Equivalence && case.spec.equivalence {
            EquivalenceStatus::Pass
        } else {
            EquivalenceStatus::NotRequested
        },
    }
}

fn prepare_case_output(root: &Path, identifier: &str) -> PathBuf {
    let output = root.join(identifier);
    if output.exists() {
        std::fs::remove_dir_all(&output).expect("remove stale case output");
    }
    std::fs::create_dir_all(&output).expect("create case output");
    output
}

fn tcl_word(path: impl AsRef<Path>) -> String {
    format!(
        "{{{}}}",
        path.as_ref()
            .to_string_lossy()
            .replace('\\', "/")
            .replace('}', "\\}")
    )
}

fn case_tcl(case: &Case, output: &Path, library: Option<&Path>) -> String {
    let mut script = String::from("# Generated by the Opto regression harness\n");
    if let Some(library) = library {
        writeln!(script, "read_libs {}", tcl_word(library)).unwrap();
    }
    let define = if case.spec.defines.is_empty() {
        String::new()
    } else {
        format!(" -define {{{}}}", case.spec.defines.join(" "))
    };
    let sources = case
        .sources()
        .iter()
        .map(tcl_word)
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(script, "read_hdl{define} [list {sources}]").unwrap();
    writeln!(script, "elaborate {}", case.spec.top).unwrap();
    writeln!(
        script,
        "redirect -file {} {{ check_design }}",
        tcl_word(output.join("check.rpt"))
    )
    .unwrap();
    for constraint in &case.spec.constraints {
        writeln!(script, "{constraint}").unwrap();
    }
    if let Some(command) = case.spec.flow.command() {
        writeln!(script, "{command}").unwrap();
        writeln!(
            script,
            "redirect -file {} {{ report_area }}",
            tcl_word(output.join("area.rpt"))
        )
        .unwrap();
        if case.spec.report_timing {
            writeln!(
                script,
                "redirect -file {} {{ report_timing }}",
                tcl_word(output.join("timing.rpt"))
            )
            .unwrap();
        }
        writeln!(
            script,
            "redirect -file {} {{ report_qor }}",
            tcl_word(output.join("qor.rpt"))
        )
        .unwrap();
        writeln!(
            script,
            "write_hdl -hierarchy {}",
            tcl_word(output.join("mapped.v"))
        )
        .unwrap();
    } else {
        writeln!(
            script,
            "redirect -file {} {{ report_area }}",
            tcl_word(output.join("area.rpt"))
        )
        .unwrap();
    }
    script.push_str("exit\n");
    script
}

fn assert_log(case: &Case, path: &Path) {
    let text = std::fs::read_to_string(path).expect("read case log");
    for expected in &case.spec.expect_log {
        assert!(
            text.contains(expected),
            "{} log does not contain {expected:?}",
            case.spec.id
        );
    }
}

fn report_integer(path: &Path, label: &str) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix(label))?
        .trim_start_matches(':')
        .trim()
        .parse()
        .ok()
}

fn assert_reports(case: &Case, output: &Path) {
    assert!(
        output.join("check.rpt").is_file(),
        "check report is missing"
    );
    let area = output.join("area.rpt");
    let check = output.join("check.rpt");
    let report = if area.is_file() {
        area.as_path()
    } else {
        check.as_path()
    };
    for (minimum, label) in [
        (case.spec.assertions.ports, "Number of ports"),
        (case.spec.assertions.nets, "Number of nets"),
        (case.spec.assertions.cells, "Number of cells"),
    ] {
        if let Some(minimum) = minimum {
            let actual = report_integer(report, label);
            assert!(
                actual.is_some_and(|value| value >= minimum),
                "{} expected {label} >= {minimum}, got {actual:?}",
                case.spec.id
            );
        }
    }
}

fn required_executable(variable: &str) -> PathBuf {
    let value =
        std::env::var_os(variable).unwrap_or_else(|| panic!("{variable} must name an executable"));
    let path = PathBuf::from(value);
    assert!(path.is_file(), "{} does not exist", path.display());
    path
}

fn optional_executable(variable: &str) -> Option<PathBuf> {
    let path = std::env::var_os(variable).map(PathBuf::from)?;
    assert!(path.is_file(), "{} does not exist", path.display());
    Some(path)
}

fn yosys_quote(path: impl AsRef<Path>) -> String {
    format!(
        "\"{}\"",
        path.as_ref()
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

fn run_equivalence(
    case: &Case,
    yosys: &Path,
    library: &Path,
    netlist: &Path,
    log: &Path,
) -> Result<(), String> {
    let read_flag = if case.spec.language == "sverilog" {
        " -sv"
    } else {
        ""
    };
    let sources = case
        .sources()
        .iter()
        .map(yosys_quote)
        .collect::<Vec<_>>()
        .join(" ");
    // Preserve latch state identity across both sides of a sequential proof.
    let gold_lowering = if case.spec.sequential {
        "proc; flatten; memory; opt; techmap; opt; clk2fflogic; opt"
    } else {
        "proc; flatten; memory; opt; techmap; opt"
    };
    let gate_lowering = if case.spec.sequential {
        "flatten; opt; clk2fflogic; opt"
    } else {
        "flatten; opt"
    };
    let mut commands = vec![
        format!("read_verilog{read_flag} {sources}"),
        format!("hierarchy -check -top {}", case.spec.top),
        gold_lowering.to_string(),
        format!("rename {} gold", case.spec.top),
        "design -stash gold".to_string(),
        format!("read_liberty -ignore_miss_func {}", yosys_quote(library)),
        format!("read_verilog {}", yosys_quote(netlist)),
        gate_lowering.to_string(),
        format!("hierarchy -check -top {}", case.spec.top),
        format!("rename {} gate", case.spec.top),
        "design -stash gate".to_string(),
        "design -reset".to_string(),
        format!("read_liberty -ignore_miss_func {}", yosys_quote(library)),
        "design -copy-from gold -as gold gold".to_string(),
        "design -copy-from gate -as gate gate".to_string(),
    ];
    match case.spec.equivalence_initial_state {
        Some(schema::EquivalenceInitialState::Zero) => {
            commands.extend([
                "miter -equiv -flatten gold gate miter".to_string(),
                "hierarchy -check -top miter".to_string(),
                "sat -verify -seq 8 -tempinduct -maxsteps 16 -set-init-zero -prove trigger 0 miter"
                    .to_string(),
            ]);
        }
        None => {
            commands.extend([
                "equiv_make gold gate equiv".to_string(),
                "hierarchy -check -top equiv".to_string(),
                "equiv_struct".to_string(),
            ]);
            if case.spec.sequential {
                commands.extend([
                    "equiv_simple -seq 8".to_string(),
                    "equiv_induct -seq 8".to_string(),
                ]);
            } else {
                commands.push("equiv_simple".to_string());
            }
            commands.push("equiv_status -assert".to_string());
        }
    }
    let output = run(
        yosys,
        [
            OsString::from("-Q"),
            OsString::from("-p"),
            OsString::from(commands.join("; ")),
        ],
        &BTreeMap::new(),
        log,
        false,
    );
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("equivalence failed; see {}", log.display()))
    }
}

fn sha256(path: &Path) -> String {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    hex_lower(Sha256::digest(bytes))
}

fn hex_lower(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn case_inputs(case: &Case, library: Option<&Path>) -> BTreeMap<String, String> {
    let mut inputs = case
        .spec
        .sources
        .iter()
        .zip(case.sources())
        .map(|(relative, source)| (relative.display().to_string(), sha256(&source)))
        .collect::<BTreeMap<_, _>>();
    inputs.insert("case.toml".to_string(), sha256(&case.path));
    if let Some(script) = &case.spec.script {
        inputs.insert("script".to_string(), sha256(&case.relative_path(script)));
    }
    if let Some(library) = library {
        inputs.insert("library".to_string(), sha256(library));
    }
    inputs
}

fn tool_identity(path: &Path, version_arguments: &[&str]) -> ToolIdentity {
    let output = std::process::Command::new(path)
        .args(version_arguments)
        .output()
        .unwrap_or_else(|error| panic!("read version from {}: {error}", path.display()));
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("unknown")
        .to_string();
    ToolIdentity {
        path: path.display().to_string(),
        sha256: sha256(path),
        version,
    }
}

fn run_upstream_case(case: &Case, opto: &Path, output_root: &Path) -> ResultEntry {
    upstream::run(case, opto, output_root)
}

fn run_qor_case(case: &Case, opto: &Path, yosys: &Path, output_root: &Path) -> ResultEntry {
    qor::run(case, opto, yosys, output_root)
}

mod qor;
mod upstream;
