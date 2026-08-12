// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::process::run as run_process;
use super::{hex_lower, output_directory, sha256, tcl_word, workspace_root};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const REVISION: &str = "2913f075dcd10e4d64b7d912fe7d4675dd0a1e29";
const LICENSE_SHA256: &str = "47862ece873b4d3c93aeebcfd0913447a174ea924dd68320fb0119e5e23b9235";
const UPSTREAM_CASES: usize = 1_027;

#[derive(Debug)]
struct SvTest {
    relative: String,
    source: PathBuf,
    top: Option<String>,
    defines: Vec<String>,
    tags: Vec<String>,
    category: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Flow {
    Analysis,
    ElaborateAndSynthesisPortedDesign,
}

#[derive(Debug, Serialize)]
struct SvTestResult {
    relative: String,
    category: String,
    tags: Vec<String>,
    flow: Flow,
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    format: u32,
    feature: Vec<Feature>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Feature {
    category: String,
    directory: Option<String>,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    format: u32,
    source_url: String,
    revision: String,
    license: String,
    license_sha256: String,
    upstream_cases: usize,
    required_cases: usize,
    passing_cases: usize,
    required_sha256: String,
}

#[derive(Debug, Serialize)]
struct Report {
    format: u32,
    revision: &'static str,
    upstream_cases: usize,
    required_cases: usize,
    passing_cases: usize,
    required_sha256: String,
    results: Vec<SvTestResult>,
}

pub(super) fn run() {
    let root = std::env::var_os("OPTO_SOURCE_SV_TESTS")
        .map(PathBuf::from)
        .expect("OPTO_SOURCE_SV_TESTS must point to the pinned sv-tests checkout");
    validate_checkout(&root);
    let tests = discover_tests(&root);
    let output = output_directory("sv-tests-conformance");
    if output.exists() {
        std::fs::remove_dir_all(&output).expect("remove old sv-tests output");
    }
    std::fs::create_dir_all(&output).expect("create sv-tests output");
    let opto = PathBuf::from(env!("CARGO_BIN_EXE_opto"));
    let tests = Arc::new(tests);
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::with_capacity(tests.len())));
    let jobs = std::env::var("OPTO_SV_TESTS_JOBS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|jobs| *jobs > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
        .min(tests.len());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let tests = Arc::clone(&tests);
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            let output = output.clone();
            let opto = opto.clone();
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(test) = tests.get(index) else {
                        break;
                    };
                    let result = run_one(test, index, &opto, &output);
                    results.lock().expect("sv-tests result lock").push(result);
                }
            });
        }
    });
    let mut results = Arc::into_inner(results)
        .expect("sv-tests workers released results")
        .into_inner()
        .expect("sv-tests result lock");
    results.sort_by(|left, right| left.relative.cmp(&right.relative));
    let passing_cases = results.iter().filter(|result| result.passed).count();
    let required = results
        .iter()
        .map(|result| result.relative.as_str())
        .collect::<Vec<_>>();
    let report = Report {
        format: 2,
        revision: REVISION,
        upstream_cases: UPSTREAM_CASES,
        required_cases: results.len(),
        passing_cases,
        required_sha256: digest_lines(&required),
        results,
    };
    let serialized = serde_json::to_string_pretty(&report).expect("serialize sv-tests report");
    std::fs::write(output.join("results.json"), format!("{serialized}\n"))
        .expect("write sv-tests report");
    eprintln!(
        "sv-tests ASIC subset: {}/{} required cases pass; {} upstream HDL cases audited",
        report.passing_cases, report.required_cases, report.upstream_cases
    );
    assert_eq!(
        report.passing_cases, report.required_cases,
        "required sv-tests ASIC cases regressed"
    );
    validate_baseline(&report);
}

fn validate_checkout(root: &Path) {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            root.to_str().expect("UTF-8 sv-tests path"),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .expect("run git rev-parse for sv-tests");
    assert!(output.status.success(), "sv-tests git rev-parse failed");
    let revision = String::from_utf8(output.stdout).expect("sv-tests revision is UTF-8");
    assert_eq!(revision.trim(), REVISION, "sv-tests revision mismatch");
    assert_eq!(
        sha256(&root.join("LICENSE")),
        LICENSE_SHA256,
        "sv-tests license changed; perform a new license audit before updating"
    );
}

fn discover_tests(root: &Path) -> Vec<SvTest> {
    let test_root = root.join("tests");
    let mut sources = Vec::new();
    collect_hdl(&test_root, &mut sources);
    sources.sort();
    assert_eq!(
        sources.len(),
        UPSTREAM_CASES,
        "sv-tests HDL inventory changed; audit the new revision before updating"
    );
    let inventory = sources
        .into_iter()
        .map(|source| {
            let relative = relative_path(&test_root, &source);
            let text = std::fs::read_to_string(&source)
                .unwrap_or_else(|error| panic!("read {}: {error}", source.display()));
            assert!(
                text.contains("SPDX-License-Identifier: ISC"),
                "sv-tests source lacks its audited ISC marker: {}",
                source.display()
            );
            (relative, (source, text))
        })
        .collect::<BTreeMap<_, _>>();
    let scope = load_scope();
    let mut selected = BTreeMap::<String, String>::new();
    for feature in scope.feature {
        assert!(
            !feature.category.trim().is_empty(),
            "sv-tests scope category is empty"
        );
        assert_eq!(
            feature.directory.is_some(),
            feature.paths.is_empty(),
            "sv-tests feature '{}' must define exactly one of directory or paths",
            feature.category
        );
        if let Some(directory) = feature.directory {
            let prefix = format!("{}/", directory.trim_end_matches('/'));
            let mut matched = 0;
            for (relative, (_, text)) in &inventory {
                if relative.starts_with(&prefix) && !is_upstream_negative(text) {
                    select_case(&mut selected, relative, &feature.category);
                    matched += 1;
                }
            }
            assert!(
                matched > 0,
                "sv-tests scope directory '{directory}' matched no positive cases"
            );
        } else {
            for relative in feature.paths {
                let (_, text) = inventory.get(&relative).unwrap_or_else(|| {
                    panic!("sv-tests scope references missing case '{relative}'")
                });
                assert!(
                    !is_upstream_negative(text),
                    "sv-tests positive ASIC scope includes upstream negative case '{relative}'"
                );
                select_case(&mut selected, &relative, &feature.category);
            }
        }
    }
    selected
        .into_iter()
        .map(|(relative, category)| {
            let (source, text) = inventory
                .get(&relative)
                .expect("selected sv-tests case exists in inventory");
            parse_test(relative, source.clone(), text, category)
        })
        .collect()
}

fn load_scope() -> Scope {
    let path = workspace_root().join("qualification/upstream/sv-tests/scope.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let scope: Scope =
        toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    assert_eq!(scope.format, 1, "sv-tests scope format mismatch");
    assert!(!scope.feature.is_empty(), "sv-tests scope has no features");
    scope
}

fn select_case(selected: &mut BTreeMap<String, String>, relative: &str, category: &str) {
    assert!(
        selected
            .insert(relative.to_string(), category.to_string())
            .is_none(),
        "sv-tests scope selects '{relative}' more than once"
    );
}

fn relative_path(test_root: &Path, source: &Path) -> String {
    source
        .strip_prefix(test_root)
        .expect("sv-tests source below tests root")
        .to_string_lossy()
        .replace('\\', "/")
}

fn collect_hdl(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
    {
        let path = entry.expect("read sv-tests directory entry").path();
        if path.is_dir() {
            collect_hdl(&path, sources);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("sv" | "v")
        ) {
            sources.push(path);
        }
    }
}

fn is_upstream_negative(text: &str) -> bool {
    metadata(text, "should_fail_because").is_some() || metadata(text, "should_fail") == Some("1")
}

fn parse_test(relative: String, source: PathBuf, text: &str, category: String) -> SvTest {
    assert!(
        metadata(text, "unsynthesizable").is_none_or(|value| value == "0"),
        "sv-tests ASIC scope includes upstream unsynthesizable case '{relative}'"
    );
    let types = metadata(text, "type").unwrap_or("parsing elaboration");
    assert!(
        !types.split_whitespace().any(|value| value == "simulation"),
        "sv-tests ASIC scope includes simulation case '{relative}'"
    );
    assert!(
        types
            .split_whitespace()
            .any(|value| matches!(value, "parsing" | "elaboration" | "preprocessing")),
        "sv-tests ASIC scope case '{relative}' has no frontend test mode"
    );
    let top = metadata(text, "top_module")
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| explicit_top(&relative).map(str::to_string))
        .or_else(|| infer_top(text));
    let defines = metadata(text, "defines")
        .map(|value| value.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let tags = metadata(text, "tags")
        .map(|value| value.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    SvTest {
        relative,
        source,
        top,
        defines,
        tags,
        category,
    }
}

fn explicit_top(relative: &str) -> Option<&'static str> {
    match relative {
        "chapter-6/6.10--implicit_port_connection.sv" => Some("top"),
        _ => None,
    }
}

fn metadata<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!(":{key}:");
    text.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix).map(str::trim))
}

fn infer_top(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        let mut words = line
            .split_once("//")
            .map_or(line, |(code, _)| code)
            .split_whitespace();
        let kind = words.next()?;
        if !matches!(kind, "module" | "interface" | "program") {
            return None;
        }
        let mut name = words.next()?;
        if matches!(name, "automatic" | "static") {
            name = words.next()?;
        }
        if kind == "interface" && name == "class" {
            return None;
        }
        Some(
            name.trim_start_matches('\\')
                .split(['(', ';', '#'])
                .next()
                .expect("module name has a prefix")
                .to_string(),
        )
    })
}

fn run_one(test: &SvTest, index: usize, opto: &Path, output: &Path) -> SvTestResult {
    let directory = output.join(format!("case-{index:04}"));
    std::fs::create_dir_all(&directory).expect("create sv-tests case output");
    let script = directory.join("run.tcl");
    let define = if test.defines.is_empty() {
        String::new()
    } else {
        format!(" -define {{{}}}", test.defines.join(" "))
    };
    let mut tcl = format!("read_hdl{define} [list {}]\n", tcl_word(&test.source));
    let flow = if let Some(top) = &test.top {
        let library = workspace_root().join("qualification/libraries/opto_test.lib");
        write!(
            tcl,
            "elaborate {{{top}}}\nif {{[llength [get_ports *]] > 0}} {{ read_libs [list {}]; synth }}\n",
            tcl_word(&library)
        )
        .expect("writing to a String cannot fail");
        Flow::ElaborateAndSynthesisPortedDesign
    } else {
        Flow::Analysis
    };
    std::fs::write(&script, tcl).expect("write sv-tests Tcl");
    let process = run_process(
        opto,
        [
            OsString::from("--no-init"),
            OsString::from("-f"),
            script.as_os_str().to_owned(),
        ],
        &BTreeMap::new(),
        &directory.join("opto.log"),
        false,
    );
    let passed = process.status.success();
    if passed {
        std::fs::remove_dir_all(&directory).expect("remove passing sv-tests directory");
    }
    SvTestResult {
        relative: test.relative.clone(),
        category: test.category.clone(),
        tags: test.tags.clone(),
        flow,
        passed,
        diagnostic: (!passed).then(|| format!("case-{index:04}/opto.log")),
    }
}

fn digest_lines(lines: &[&str]) -> String {
    let mut digest = Sha256::new();
    for line in lines {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    hex_lower(digest.finalize())
}

fn validate_baseline(report: &Report) {
    let path = workspace_root().join("qualification/upstream/sv-tests/baseline.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let baseline: Baseline =
        toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    assert_eq!(baseline.format, 2, "sv-tests baseline format mismatch");
    assert_eq!(
        baseline.source_url,
        "https://github.com/chipsalliance/sv-tests.git"
    );
    assert_eq!(
        baseline.revision, REVISION,
        "sv-tests baseline revision mismatch"
    );
    assert_eq!(baseline.license, "ISC");
    assert_eq!(baseline.license_sha256, LICENSE_SHA256);
    assert_eq!(baseline.upstream_cases, report.upstream_cases);
    assert_eq!(baseline.required_cases, report.required_cases);
    assert_eq!(baseline.passing_cases, report.passing_cases);
    assert_eq!(baseline.required_sha256, report.required_sha256);
}

#[cfg(test)]
mod tests {
    use super::{explicit_top, infer_top};

    #[test]
    fn implicit_port_connection_uses_the_design_under_test() {
        let source = "module top; test helper(); endmodule\nmodule test; endmodule\n";
        assert_eq!(infer_top(source).as_deref(), Some("test"));
        assert_eq!(
            explicit_top("chapter-6/6.10--implicit_port_connection.sv"),
            Some("top")
        );
    }
}
