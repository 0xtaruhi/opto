// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Same-host regression gate for the pinned real medium-scale corpus.

use super::process::run as run_process;
use super::qor::{
    assert_histogram_is_complete, cell_histogram, parse_opto_area, parse_opto_timing,
    target_cell_names,
};
use super::schema::{Metrics, TimingResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    name: String,
    description: String,
    threads: usize,
    maximum_parallel_cases: usize,
    library_sha256: String,
    guard: Guard,
    sources: Vec<Source>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    id: String,
    url: String,
    sha256: String,
    revision: String,
    license: String,
    citation: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Guard {
    minimum_cases: usize,
    minimum_timing_cases: usize,
    minimum_baseline_cells: u64,
    maximum_area_geomean_ratio: f64,
    maximum_area_case_ratio: f64,
    maximum_delay_geomean_ratio: f64,
    maximum_delay_case_ratio: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    source: String,
    rtl: Vec<PathBuf>,
    top: String,
    category: String,
    scenario: Scenario,
    #[serde(default)]
    include_dirs: Vec<PathBuf>,
    #[serde(default)]
    defines: Vec<String>,
    clock_port: Option<String>,
    clock_period: Option<f64>,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    AreaUnconstrained,
    TimingConstrained,
}

#[derive(Serialize)]
struct ResultDocument {
    format: u32,
    suite: String,
    manifest_sha256: String,
    library_sha256: String,
    threads: usize,
    parallel_cases: usize,
    guard: Guard,
    baseline: ToolIdentity,
    candidate: ToolIdentity,
    results: Vec<CaseResult>,
    diagnostics: Vec<String>,
}

#[derive(Serialize)]
struct ToolIdentity {
    path: String,
    sha256: String,
    version: String,
}

#[derive(Serialize)]
struct CaseResult {
    id: String,
    category: String,
    scenario: Scenario,
    inputs: BTreeMap<String, String>,
    baseline: Option<Measurement>,
    candidate: Option<Measurement>,
    diagnostics: Vec<String>,
}

#[derive(Clone, Serialize)]
struct Measurement {
    area: f64,
    cells: u64,
    cell_histogram: BTreeMap<String, u64>,
    timing: Option<TimingResult>,
    metrics: Metrics,
}

#[derive(Clone)]
struct Sample {
    area: f64,
    cells: u64,
    cell_histogram: BTreeMap<String, u64>,
    timing: Option<TimingResult>,
    metrics: Metrics,
}

#[derive(Clone, Copy)]
struct ToolPair<'a> {
    baseline: &'a Path,
    candidate: &'a Path,
}

#[derive(Clone, Copy)]
struct BenchmarkContext<'a> {
    case: &'a Case,
    sources: &'a Path,
    library: &'a Path,
    target_cells: &'a BTreeSet<String>,
    output: &'a Path,
    threads: usize,
}

pub(super) fn run(manifest_path: &Path) {
    let manifest_text = std::fs::read_to_string(manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display()));
    let manifest: Manifest = toml::from_str(&manifest_text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", manifest_path.display()));
    validate_manifest(&manifest);

    let sources = required_directory("OPTO_SOURCE_REAL_MEDIUM");
    let library = required_file("OPTO_LIBRARY_REAL_MEDIUM");
    assert_eq!(
        sha256_file(&library),
        manifest.library_sha256,
        "real benchmark Liberty does not match the pinned library"
    );
    let baseline = required_file("OPTO_QOR_BASELINE_BINARY");
    let candidate = required_file("OPTO_QOR_BINARY");
    let output = output_directory(&manifest.name);
    if output.exists() {
        std::fs::remove_dir_all(&output)
            .unwrap_or_else(|error| panic!("remove {}: {error}", output.display()));
    }
    std::fs::create_dir_all(&output)
        .unwrap_or_else(|error| panic!("create {}: {error}", output.display()));
    let target_cells = target_cell_names(&library);
    let parallel_cases = parallel_cases(&manifest);
    let results = measure_cases(
        &manifest,
        ToolPair {
            baseline: &baseline,
            candidate: &candidate,
        },
        &sources,
        &library,
        &target_cells,
        &output,
        parallel_cases,
    );

    let mut diagnostics = guard_failures(&manifest.guard, &results);
    diagnostics.extend(results.iter().flat_map(|result| {
        result
            .diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {diagnostic}", result.id))
    }));
    let document = ResultDocument {
        format: 1,
        suite: manifest.name,
        manifest_sha256: sha256_bytes(manifest_text.as_bytes()),
        library_sha256: sha256_file(&library),
        threads: manifest.threads,
        parallel_cases,
        guard: manifest.guard,
        baseline: tool_identity(&baseline),
        candidate: tool_identity(&candidate),
        results,
        diagnostics,
    };
    let serialized = serde_json::to_string_pretty(&document).expect("serialize medium QoR result");
    std::fs::write(output.join("results.json"), format!("{serialized}\n"))
        .expect("write medium QoR result");
    assert!(
        document.diagnostics.is_empty(),
        "real medium QoR guard failed:\n{}\nsee {}",
        document.diagnostics.join("\n"),
        output.display()
    );
}

fn parallel_cases(manifest: &Manifest) -> usize {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    manifest
        .maximum_parallel_cases
        .min(available / manifest.threads)
        .max(1)
}

fn measure_cases(
    manifest: &Manifest,
    tools: ToolPair<'_>,
    sources: &Path,
    library: &Path,
    target_cells: &BTreeSet<String>,
    output: &Path,
    parallel_cases: usize,
) -> Vec<CaseResult> {
    let next = AtomicUsize::new(0);
    let slots = Mutex::new((0..manifest.cases.len()).map(|_| None).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        for _ in 0..parallel_cases {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = manifest.cases.get(index) else {
                        break;
                    };
                    let result = measure_case(
                        tools,
                        BenchmarkContext {
                            case,
                            sources,
                            library,
                            target_cells,
                            output,
                            threads: manifest.threads,
                        },
                    );
                    slots.lock().expect("benchmark result lock")[index] = Some(result);
                }
            });
        }
    });
    slots
        .into_inner()
        .expect("benchmark result lock")
        .into_iter()
        .map(|result| result.expect("benchmark worker produced every case"))
        .collect()
}

fn measure_case(tools: ToolPair<'_>, context: BenchmarkContext<'_>) -> CaseResult {
    let case = context.case;
    eprintln!("RUN  {} ({})", case.id, case.category);
    let inputs = case_inputs(case, context.sources);
    let mut diagnostics = Vec::new();
    let measured = measure_pair(tools, context)
        .map_err(|error| diagnostics.push(error))
        .ok();
    let (baseline, candidate) = measured
        .map(|(baseline, candidate)| (Some(baseline), Some(candidate)))
        .unwrap_or_default();
    CaseResult {
        id: case.id.clone(),
        category: case.category.clone(),
        scenario: case.scenario,
        inputs,
        baseline,
        candidate,
        diagnostics,
    }
}

fn validate_manifest(manifest: &Manifest) {
    assert_eq!(manifest.format, 1, "real benchmark format mismatch");
    assert!(
        !manifest.description.is_empty(),
        "real benchmark has no description"
    );
    assert!(manifest.threads > 0, "real benchmark has no workers");
    assert!(
        manifest.maximum_parallel_cases > 0,
        "real benchmark has no case workers"
    );
    assert_eq!(
        manifest.library_sha256.len(),
        64,
        "real benchmark has no pinned Liberty hash"
    );
    assert!(
        manifest.cases.len() >= manifest.guard.minimum_cases,
        "real benchmark has fewer cases than its guard requires"
    );
    let source_ids = manifest
        .sources
        .iter()
        .map(|source| {
            assert!(source.url.starts_with("https://"));
            assert_eq!(source.sha256.len(), 64);
            assert!(!source.revision.is_empty());
            assert!(!source.license.is_empty());
            assert!(!source.citation.is_empty());
            source.id.as_str()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        source_ids.len(),
        manifest.sources.len(),
        "duplicate source id"
    );
    let mut case_ids = BTreeSet::new();
    let mut timing_cases = 0;
    for case in &manifest.cases {
        assert!(case_ids.insert(&case.id), "duplicate case id {}", case.id);
        assert!(source_ids.contains(case.source.as_str()));
        assert!(!case.rtl.is_empty(), "case {} has no RTL", case.id);
        match case.scenario {
            Scenario::AreaUnconstrained => {
                assert!(case.clock_port.is_none() && case.clock_period.is_none());
            }
            Scenario::TimingConstrained => {
                timing_cases += 1;
                assert!(
                    case.clock_port
                        .as_ref()
                        .is_some_and(|port| !port.is_empty())
                );
                assert!(case.clock_period.is_some_and(|period| period > 0.0));
            }
        }
    }
    assert!(timing_cases >= manifest.guard.minimum_timing_cases);
    for ratio in [
        manifest.guard.maximum_area_geomean_ratio,
        manifest.guard.maximum_area_case_ratio,
        manifest.guard.maximum_delay_geomean_ratio,
        manifest.guard.maximum_delay_case_ratio,
    ] {
        assert!(ratio.is_finite() && ratio >= 1.0, "invalid guard ratio");
    }
}

fn measure_pair(
    tools: ToolPair<'_>,
    context: BenchmarkContext<'_>,
) -> Result<(Measurement, Measurement), String> {
    let BenchmarkContext {
        case,
        sources,
        library,
        target_cells,
        output,
        threads,
    } = context;
    let measure = |label: &str, binary: &Path| {
        measure_once(
            binary,
            case,
            sources,
            library,
            target_cells,
            &output.join(&case.id).join(label),
            threads,
        )
        .map(Measurement::from)
        .map_err(|error| format!("{label} failed: {error}"))
    };
    Ok((
        measure("baseline", tools.baseline)?,
        measure("candidate", tools.candidate)?,
    ))
}

fn measure_once(
    binary: &Path,
    case: &Case,
    sources: &Path,
    library: &Path,
    target_cells: &BTreeSet<String>,
    output: &Path,
    threads: usize,
) -> Result<Sample, String> {
    std::fs::create_dir_all(output).map_err(|error| error.to_string())?;
    let script = output.join("run.tcl");
    std::fs::write(&script, case_tcl(case, sources, library, output))
        .map_err(|error| error.to_string())?;
    let execution = run_process(
        binary,
        [
            OsString::from("--no-init"),
            OsString::from("--threads"),
            OsString::from(threads.to_string()),
            OsString::from("-f"),
            script.as_os_str().to_owned(),
        ],
        &BTreeMap::new(),
        &output.join("opto.log"),
        true,
    );
    if !execution.status.success() {
        return Err(format!(
            "process failed; see {}",
            output.join("opto.log").display()
        ));
    }
    let parsed = std::panic::catch_unwind(|| {
        let (area, cells) = parse_opto_area(&output.join("area.rpt"));
        let histogram = cell_histogram(&output.join("mapped.v"), target_cells);
        assert_histogram_is_complete("Opto", cells, &histogram);
        let timing = case.clock_period.map(|period| {
            parse_opto_timing(&output.join("qor.rpt"), period).expect("complete timing report")
        });
        (area, cells, histogram, timing)
    })
    .map_err(|payload| panic_message(payload.as_ref()))?;
    Ok(Sample {
        area: parsed.0,
        cells: parsed.1,
        cell_histogram: parsed.2,
        timing: parsed.3,
        metrics: execution.metrics,
    })
}

impl From<Sample> for Measurement {
    fn from(sample: Sample) -> Self {
        Self {
            area: sample.area,
            cells: sample.cells,
            cell_histogram: sample.cell_histogram,
            timing: sample.timing,
            metrics: sample.metrics,
        }
    }
}

fn guard_failures(guard: &Guard, results: &[CaseResult]) -> Vec<String> {
    let complete = results
        .iter()
        .filter_map(|result| {
            Some((
                result,
                result.baseline.as_ref()?,
                result.candidate.as_ref()?,
            ))
        })
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    if complete.len() < guard.minimum_cases {
        failures.push(format!(
            "only {} complete cases; at least {} are required",
            complete.len(),
            guard.minimum_cases
        ));
    }
    for (result, baseline, _) in &complete {
        if baseline.cells < guard.minimum_baseline_cells {
            failures.push(format!(
                "{} is too small for this gate: {} baseline cells, minimum {}",
                result.id, baseline.cells, guard.minimum_baseline_cells
            ));
        }
    }

    check_ratios(
        "area",
        complete.iter().map(|(result, baseline, candidate)| {
            (result.id.as_str(), candidate.area / baseline.area)
        }),
        guard.maximum_area_geomean_ratio,
        guard.maximum_area_case_ratio,
        &mut failures,
    );
    let timing = complete
        .iter()
        .filter_map(|(result, baseline, candidate)| {
            Some((
                result,
                baseline.timing.as_ref()?,
                candidate.timing.as_ref()?,
            ))
        })
        .collect::<Vec<_>>();
    if timing.len() < guard.minimum_timing_cases {
        failures.push(format!(
            "only {} complete timing cases; at least {} are required",
            timing.len(),
            guard.minimum_timing_cases
        ));
    }
    for (result, baseline, candidate) in &timing {
        if baseline.worst_slack >= 0.0 && candidate.worst_slack < 0.0 {
            failures.push(format!("{} introduces negative slack", result.id));
        }
        if candidate.violating_paths > baseline.violating_paths {
            failures.push(format!(
                "{} increases violating paths from {} to {}",
                result.id, baseline.violating_paths, candidate.violating_paths
            ));
        }
    }
    check_ratios(
        "critical delay",
        timing.iter().map(|(result, baseline, candidate)| {
            (
                result.id.as_str(),
                candidate.critical_delay / baseline.critical_delay,
            )
        }),
        guard.maximum_delay_geomean_ratio,
        guard.maximum_delay_case_ratio,
        &mut failures,
    );
    failures
}

fn check_ratios<'a>(
    metric: &str,
    ratios: impl Iterator<Item = (&'a str, f64)>,
    maximum_geomean: f64,
    maximum_case: f64,
    failures: &mut Vec<String>,
) {
    let ratios = ratios.collect::<Vec<_>>();
    let valid = ratios
        .iter()
        .all(|(_, ratio)| ratio.is_finite() && *ratio > 0.0);
    if !valid || ratios.is_empty() {
        failures.push(format!("{metric} ratios are incomplete or non-finite"));
        return;
    }
    for (case, ratio) in &ratios {
        if *ratio > maximum_case {
            failures.push(format!(
                "{case} {metric} ratio {ratio:.4} exceeds per-case limit {maximum_case:.4}"
            ));
        }
    }
    let count = u32::try_from(ratios.len()).expect("benchmark case count fits u32");
    let geomean =
        (ratios.iter().map(|(_, ratio)| ratio.ln()).sum::<f64>() / f64::from(count)).exp();
    if geomean > maximum_geomean {
        failures.push(format!(
            "{metric} geometric-mean ratio {geomean:.4} exceeds limit {maximum_geomean:.4}"
        ));
    }
}

fn case_tcl(case: &Case, sources: &Path, library: &Path, output: &Path) -> String {
    let source_root = sources.join(&case.source);
    let rtl = case
        .rtl
        .iter()
        .map(|path| super::tcl_word(source_root.join(path)))
        .collect::<Vec<_>>()
        .join(" ");
    let mut read = String::from("read_hdl");
    if !case.defines.is_empty() {
        write!(read, " -define {{{}}}", case.defines.join(" ")).unwrap();
    }
    for include in &case.include_dirs {
        write!(
            read,
            " -incdir {}",
            super::tcl_word(source_root.join(include))
        )
        .unwrap();
    }
    let mut script = format!(
        "# Generated by the real-medium QoR gate\nread_libs {}\n{read} [list {rtl}]\nelaborate {}\n",
        super::tcl_word(library),
        case.top
    );
    if let (Some(port), Some(period)) = (&case.clock_port, case.clock_period) {
        writeln!(
            script,
            "create_clock -name {port} -period {period} [get_ports {port}]"
        )
        .unwrap();
    }
    script.push_str("synth\n");
    writeln!(
        script,
        "redirect -file {} {{ report_area }}",
        super::tcl_word(output.join("area.rpt"))
    )
    .unwrap();
    writeln!(
        script,
        "redirect -file {} {{ report_qor }}",
        super::tcl_word(output.join("qor.rpt"))
    )
    .unwrap();
    writeln!(
        script,
        "write_hdl -hierarchy {}",
        super::tcl_word(output.join("mapped.v"))
    )
    .unwrap();
    script.push_str("exit\n");
    script
}

fn case_inputs(case: &Case, sources: &Path) -> BTreeMap<String, String> {
    case.rtl
        .iter()
        .map(|path| {
            let relative = Path::new(&case.source).join(path);
            let absolute = sources.join(&relative);
            (
                relative.to_string_lossy().into_owned(),
                sha256_file(&absolute),
            )
        })
        .collect()
}

fn required_file(variable: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(variable).unwrap_or_else(|| panic!("{variable} must be set")),
    );
    assert!(path.is_file(), "{} is not a file", path.display());
    path
}

fn required_directory(variable: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(variable).unwrap_or_else(|| panic!("{variable} must be set")),
    );
    assert!(path.is_dir(), "{} is not a directory", path.display());
    path
}

fn output_directory(suite: &str) -> PathBuf {
    std::env::var_os("OPTO_REGRESSION_OUTPUT").map_or_else(
        || std::env::temp_dir().join(format!("opto-{suite}-{}", std::process::id())),
        |root| PathBuf::from(root).join(suite),
    )
}

fn tool_identity(path: &Path) -> ToolIdentity {
    let output = Command::new(path)
        .arg("-version")
        .output()
        .unwrap_or_else(|error| panic!("query {} version: {error}", path.display()));
    let version = String::from_utf8_lossy(if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    })
    .trim()
    .to_string();
    ToolIdentity {
        path: path.to_string_lossy().into_owned(),
        sha256: sha256_file(path),
        version,
    }
}

fn sha256_file(path: &Path) -> String {
    sha256_bytes(
        &std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
}

fn sha256_bytes(bytes: &[u8]) -> String {
    super::hex_lower(Sha256::digest(bytes))
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<String>().map_or_else(
        || {
            payload.downcast_ref::<&str>().map_or_else(
                || "measurement panicked".to_string(),
                |text| (*text).to_string(),
            )
        },
        Clone::clone,
    )
}

#[cfg(test)]
mod tests {
    use super::{Guard, check_ratios};

    #[test]
    fn aggregate_guard_allows_local_tradeoffs_but_rejects_net_regression() {
        let mut failures = Vec::new();
        check_ratios(
            "area",
            [("improves", 0.90), ("regresses", 1.05)].into_iter(),
            1.0,
            1.10,
            &mut failures,
        );
        assert!(failures.is_empty());

        check_ratios(
            "area",
            [("left", 1.02), ("right", 1.03)].into_iter(),
            1.0,
            1.10,
            &mut failures,
        );
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("geometric-mean"))
        );
    }

    #[test]
    fn guard_policy_has_no_implicit_defaults() {
        let incomplete = "minimum_cases = 30\nminimum_timing_cases = 8\n";
        assert!(toml::from_str::<Guard>(incomplete).is_err());
    }
}
