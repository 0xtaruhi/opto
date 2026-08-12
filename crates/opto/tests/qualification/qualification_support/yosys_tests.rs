// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::formal::{MappedProof, defined_root_designs, prove_mapped_equivalence};
use super::process::run as run_process;
use super::{hex_lower, output_directory, required_executable, sha256, tcl_word, workspace_root};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const REVISION: &str = "a0fbe6e13311d4909938c63eeb28b6c730467e6c";
const LICENSE_SHA256: &str = "6998b5724d4cb3f459d1c12b6bd0cdbfa9c949ef14d0fb6d7d97d97404e5b5f3";
const UPSTREAM_HDL_CASES: usize = 549;
const ELIGIBLE_CASES: usize = 136;
const DIRECTORIES: &[&str] = &["errors", "memories", "proc", "simple"];

#[derive(Debug)]
struct YosysTest {
    relative: String,
    source: PathBuf,
    expectation: Expectation,
    category: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Expectation {
    Pass,
    RejectFrontend,
    RejectSynthesis,
    KnownGap,
    Excluded,
}

#[derive(Debug, Serialize)]
struct YosysTestResult {
    relative: String,
    expectation: Expectation,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    expected: &'static str,
    actual: &'static str,
    matches_audit: bool,
    exit_code: Option<i32>,
    signal: Option<i32>,
    elaborated_designs: Vec<String>,
    synthesized_designs: Vec<String>,
    observable_designs: Vec<String>,
    proof_targets: Vec<String>,
    deferred_proofs: Vec<DeferredProof>,
    proved_designs: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    format: u32,
    source_url: String,
    revision: String,
    license: String,
    license_sha256: String,
    upstream_hdl_cases: usize,
    eligible_cases: usize,
    required_cases: usize,
    required_conforming_cases: usize,
    excluded_cases: usize,
    known_gap_cases: usize,
    required_conforming_sha256: String,
    synthesized_designs: usize,
    synthesized_designs_sha256: String,
    observable_designs: usize,
    observable_designs_sha256: String,
    proof_targets: usize,
    proved_designs: usize,
    proof_targets_sha256: String,
    deferred_proofs: usize,
    deferred_proofs_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofAudit {
    format: u32,
    #[serde(default)]
    defer: Vec<ProofAuditEntry>,
    #[serde(default)]
    defer_group: Vec<ProofAuditGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofAuditEntry {
    path: String,
    design: String,
    category: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProofAuditGroup {
    path: String,
    designs: Vec<String>,
    category: String,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
struct DeferredProof {
    design: String,
    category: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Audit {
    format: u32,
    #[serde(default)]
    reject_frontend: Vec<AuditEntry>,
    #[serde(default)]
    reject_synthesis: Vec<AuditEntry>,
    #[serde(default)]
    exclude: Vec<AuditEntry>,
    #[serde(default)]
    known_gap: Vec<AuditEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditEntry {
    path: String,
    category: String,
    reason: String,
}

#[derive(Debug)]
struct AuditedExpectation {
    expectation: Expectation,
    category: String,
    reason: String,
}

#[derive(Debug, Serialize)]
struct Report {
    format: u32,
    revision: &'static str,
    filtered: bool,
    upstream_hdl_cases: usize,
    eligible_cases: usize,
    required_cases: usize,
    required_conforming_cases: usize,
    excluded_cases: usize,
    known_gap_cases: usize,
    required_conforming_sha256: String,
    synthesized_designs: usize,
    synthesized_designs_sha256: String,
    observable_designs: usize,
    observable_designs_sha256: String,
    proof_targets: usize,
    proved_designs: usize,
    proof_targets_sha256: String,
    deferred_proofs: usize,
    deferred_proofs_sha256: String,
    results: Vec<YosysTestResult>,
}

pub(super) fn run() {
    let root = std::env::var_os("OPTO_SOURCE_YOSYS_TESTS")
        .map(PathBuf::from)
        .expect("OPTO_SOURCE_YOSYS_TESTS must point to the pinned Yosys checkout");
    validate_checkout(&root);
    let mut tests = discover_tests(&root);
    let filtered = std::env::var_os("OPTO_YOSYS_TEST_CASE").map(|value| {
        value
            .to_string_lossy()
            .split(',')
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    });
    if let Some(requested) = &filtered {
        tests.retain(|test| requested.contains(&test.relative));
        assert_eq!(
            tests.len(),
            requested.len(),
            "OPTO_YOSYS_TEST_CASE names a case outside the qualification inventory"
        );
    }
    let output = output_directory("yosys-rtl-qualification");
    if output.exists() {
        std::fs::remove_dir_all(&output).expect("remove old Yosys qualification output");
    }
    std::fs::create_dir_all(&output).expect("create Yosys qualification output");
    let opto = PathBuf::from(env!("CARGO_BIN_EXE_opto"));
    let yosys = required_executable("OPTO_YOSYS");
    let library = workspace_root().join("qualification/libraries/opto_test.lib");
    let proof_audit = Arc::new(load_proof_audit());
    let tests = Arc::new(tests);
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::with_capacity(tests.len())));
    let jobs = std::env::var("OPTO_YOSYS_TESTS_JOBS")
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
            let yosys = yosys.clone();
            let library = library.clone();
            let proof_audit = Arc::clone(&proof_audit);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(test) = tests.get(index) else {
                        break;
                    };
                    let result =
                        run_one(test, index, &opto, &yosys, &library, &output, &proof_audit);
                    results
                        .lock()
                        .expect("Yosys qualification result lock")
                        .push(result);
                }
            });
        }
    });
    let mut results = Arc::into_inner(results)
        .expect("Yosys qualification workers released results")
        .into_inner()
        .expect("Yosys qualification result lock");
    results.sort_by(|left, right| left.relative.cmp(&right.relative));
    let required = results
        .iter()
        .filter(|result| {
            matches!(
                result.expectation,
                Expectation::Pass | Expectation::RejectFrontend | Expectation::RejectSynthesis
            )
        })
        .collect::<Vec<_>>();
    let required_conforming = required
        .iter()
        .filter(|result| result.matches_audit)
        .map(|result| result.relative.as_str())
        .collect::<Vec<_>>();
    let excluded_cases = results
        .iter()
        .filter(|result| result.expectation == Expectation::Excluded)
        .count();
    let known_gap_cases = results
        .iter()
        .filter(|result| result.expectation == Expectation::KnownGap)
        .count();
    let mut synthesized_designs = results
        .iter()
        .flat_map(|result| {
            result
                .synthesized_designs
                .iter()
                .map(|design| format!("{}::{design}", result.relative))
        })
        .collect::<Vec<_>>();
    synthesized_designs.sort();
    let mut observable_designs = results
        .iter()
        .flat_map(|result| {
            result
                .observable_designs
                .iter()
                .map(|design| format!("{}::{design}", result.relative))
        })
        .collect::<Vec<_>>();
    observable_designs.sort();
    let mut proof_targets = results
        .iter()
        .flat_map(|result| {
            result
                .proof_targets
                .iter()
                .map(|design| format!("{}::{design}", result.relative))
        })
        .collect::<Vec<_>>();
    proof_targets.sort();
    let mut deferred_proofs = results
        .iter()
        .flat_map(|result| {
            result.deferred_proofs.iter().map(|proof| {
                format!(
                    "{}::{}::{}::{}",
                    result.relative, proof.design, proof.category, proof.reason
                )
            })
        })
        .collect::<Vec<_>>();
    deferred_proofs.sort();
    let audited_deferrals = results
        .iter()
        .flat_map(|result| {
            result
                .deferred_proofs
                .iter()
                .map(|proof| (result.relative.clone(), proof.design.clone()))
        })
        .collect::<std::collections::BTreeSet<_>>();
    if filtered.is_none() {
        assert_eq!(
            audited_deferrals,
            proof_audit.keys().cloned().collect(),
            "Yosys proof audit must match the exact observable design set"
        );
    }
    let proved_designs = results.iter().map(|result| result.proved_designs).sum();
    let report = Report {
        format: 3,
        revision: REVISION,
        filtered: filtered.is_some(),
        upstream_hdl_cases: UPSTREAM_HDL_CASES,
        eligible_cases: results.len(),
        required_cases: required.len(),
        required_conforming_cases: required_conforming.len(),
        excluded_cases,
        known_gap_cases,
        required_conforming_sha256: digest_lines(&required_conforming),
        synthesized_designs: synthesized_designs.len(),
        synthesized_designs_sha256: digest_owned_lines(&synthesized_designs),
        observable_designs: observable_designs.len(),
        observable_designs_sha256: digest_owned_lines(&observable_designs),
        proof_targets: proof_targets.len(),
        proved_designs,
        proof_targets_sha256: digest_owned_lines(&proof_targets),
        deferred_proofs: deferred_proofs.len(),
        deferred_proofs_sha256: digest_owned_lines(&deferred_proofs),
        results,
    };
    let serialized =
        serde_json::to_string_pretty(&report).expect("serialize Yosys qualification report");
    std::fs::write(output.join("results.json"), format!("{serialized}\n"))
        .expect("write Yosys qualification report");
    eprintln!(
        "Yosys RTL qualification: {}/{} required ASIC cases conform; {}/{} proof targets equivalent; {}/{} observable designs deferred with an explicit reason; {} synthesized designs; {} known gaps; {} explicit exclusions; {} upstream HDL files audited",
        report.required_conforming_cases,
        report.required_cases,
        report.proved_designs,
        report.proof_targets,
        report.deferred_proofs,
        report.observable_designs,
        report.synthesized_designs,
        report.known_gap_cases,
        report.excluded_cases,
        report.upstream_hdl_cases
    );
    assert_eq!(
        report.required_conforming_cases, report.required_cases,
        "required Yosys ASIC qualification cases regressed"
    );
    assert_eq!(
        report.proved_designs, report.proof_targets,
        "a synthesized Yosys design failed formal equivalence"
    );
    assert!(
        report
            .results
            .iter()
            .filter(|result| result.expectation == Expectation::KnownGap)
            .all(|result| result.matches_audit),
        "a declared Yosys capability gap changed behavior; update the audit explicitly"
    );
    if filtered.is_none() {
        validate_baseline(&report);
    }
}

fn validate_checkout(root: &Path) {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            root.to_str().expect("UTF-8 Yosys path"),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .expect("run git rev-parse for Yosys");
    assert!(output.status.success(), "Yosys git rev-parse failed");
    let revision = String::from_utf8(output.stdout).expect("Yosys revision is UTF-8");
    assert_eq!(revision.trim(), REVISION, "Yosys revision mismatch");
    assert_eq!(
        sha256(&root.join("COPYING")),
        LICENSE_SHA256,
        "Yosys license changed; perform a new license audit before updating"
    );
    let all_hdl = tracked_hdl(root);
    assert_eq!(
        all_hdl, UPSTREAM_HDL_CASES,
        "Yosys HDL inventory changed; audit the new revision before updating"
    );
}

fn tracked_hdl(root: &Path) -> usize {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            root.to_str().expect("UTF-8 Yosys path"),
            "ls-files",
            ":(glob)tests/**/*.v",
            ":(glob)tests/**/*.sv",
        ])
        .output()
        .expect("list tracked Yosys HDL files");
    assert!(output.status.success(), "git ls-files for Yosys failed");
    String::from_utf8(output.stdout)
        .expect("Yosys tracked paths are UTF-8")
        .lines()
        .count()
}

fn discover_tests(root: &Path) -> Vec<YosysTest> {
    let test_root = root.join("tests");
    let mut audit = load_audit();
    let mut sources = Vec::new();
    for directory in DIRECTORIES {
        sources.extend(discover_hdl(&test_root.join(directory)));
    }
    sources.sort();
    assert_eq!(
        sources.len(),
        ELIGIBLE_CASES,
        "selected Yosys RTL inventory changed"
    );
    let tests = sources
        .into_iter()
        .map(|source| {
            let relative = source
                .strip_prefix(&test_root)
                .expect("Yosys source below tests root")
                .to_string_lossy()
                .replace('\\', "/");
            let audited = audit.remove(&relative);
            let expectation = audited.as_ref().map_or_else(
                || {
                    if relative.starts_with("errors/") {
                        Expectation::RejectFrontend
                    } else {
                        Expectation::Pass
                    }
                },
                |audited| audited.expectation,
            );
            YosysTest {
                expectation,
                category: audited.as_ref().map(|audited| audited.category.clone()),
                reason: audited.map(|audited| audited.reason),
                relative,
                source,
            }
        })
        .collect::<Vec<_>>();
    assert!(
        audit.is_empty(),
        "Yosys audit references files outside the selected inventory: {:?}",
        audit.keys().collect::<Vec<_>>()
    );
    tests
}

fn load_audit() -> BTreeMap<String, AuditedExpectation> {
    let path = workspace_root().join("qualification/upstream/yosys/audit.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let audit: Audit =
        toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    assert_eq!(audit.format, 2, "Yosys audit format mismatch");
    let mut entries = BTreeMap::new();
    for (expectation, group) in [
        (Expectation::RejectFrontend, audit.reject_frontend),
        (Expectation::RejectSynthesis, audit.reject_synthesis),
        (Expectation::Excluded, audit.exclude),
        (Expectation::KnownGap, audit.known_gap),
    ] {
        for entry in group {
            assert!(
                !entry.category.trim().is_empty(),
                "empty Yosys audit category"
            );
            assert!(!entry.reason.trim().is_empty(), "empty Yosys audit reason");
            let path = entry.path.clone();
            assert!(
                entries
                    .insert(
                        entry.path,
                        AuditedExpectation {
                            expectation,
                            category: entry.category,
                            reason: entry.reason,
                        },
                    )
                    .is_none(),
                "Yosys audit classifies '{path}' more than once"
            );
        }
    }
    entries
}

fn load_proof_audit() -> BTreeMap<(String, String), ProofAuditEntry> {
    let path = workspace_root().join("qualification/upstream/yosys/proof.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let audit: ProofAudit =
        toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    assert_eq!(audit.format, 1, "Yosys proof audit format mismatch");
    let mut entries = BTreeMap::new();
    let mut flattened = audit.defer;
    for group in audit.defer_group {
        assert!(!group.designs.is_empty(), "empty Yosys proof design group");
        for design in group.designs {
            flattened.push(ProofAuditEntry {
                path: group.path.clone(),
                design,
                category: group.category.clone(),
                reason: group.reason.clone(),
            });
        }
    }
    for entry in flattened {
        assert!(!entry.path.trim().is_empty(), "empty Yosys proof path");
        assert!(!entry.design.trim().is_empty(), "empty Yosys proof design");
        assert!(
            !entry.category.trim().is_empty(),
            "empty Yosys proof category"
        );
        assert!(!entry.reason.trim().is_empty(), "empty Yosys proof reason");
        let key = (entry.path.clone(), entry.design.clone());
        assert!(
            entries.insert(key.clone(), entry).is_none(),
            "Yosys proof audit classifies '{}::{}' more than once",
            key.0,
            key.1
        );
    }
    entries
}

fn discover_hdl(directory: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    collect_hdl(directory, &mut sources);
    sources.sort();
    sources
}

fn collect_hdl(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
    {
        let path = entry.expect("read Yosys test directory entry").path();
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

fn run_one(
    test: &YosysTest,
    index: usize,
    opto: &Path,
    yosys: &Path,
    library: &Path,
    output: &Path,
    proof_audit: &BTreeMap<(String, String), ProofAuditEntry>,
) -> YosysTestResult {
    let directory = output.join(format!("case-{index:04}"));
    std::fs::create_dir_all(&directory).expect("create Yosys qualification case output");
    let script = directory.join("run.tcl");
    let systemverilog = test.source.extension().is_some_and(|value| value == "sv");
    let reference_designs = if matches!(
        test.expectation,
        Expectation::Pass | Expectation::RejectSynthesis | Expectation::KnownGap
    ) {
        defined_root_designs(
            yosys,
            std::slice::from_ref(&test.source),
            systemverilog,
            &directory.join("reference.json"),
            &directory.join("reference.log"),
        )
    } else {
        Some(Vec::new())
    };
    let mut tcl = format!(
        "read_libs [list {}]\nset_db synth_effort low\nread_hdl [list {}]\n",
        tcl_word(library),
        tcl_word(&test.source),
    );
    if let Some(designs) = &reference_designs {
        for (proof_index, design) in designs.iter().enumerate() {
            writeln!(tcl, "elaborate {}", tcl_word(&design.name))
                .expect("writing to a String cannot fail");
            tcl.push_str("puts \"OPTO_ELABORATED\\t[get_db [get_db current_design] .name]\"\n");
            tcl.push_str("synth\n");
            tcl.push_str("puts \"OPTO_SYNTHESIZED\\t[get_db [get_db current_design] .name]\"\n");
            if test.expectation == Expectation::Pass && design.observable {
                writeln!(
                    tcl,
                    "set proof_netlist {}",
                    tcl_word(directory.join(format!("proof-{proof_index:04}.v")))
                )
                .expect("writing to a String cannot fail");
                tcl.push_str(
                    "write_hdl -hierarchy $proof_netlist\nputs \"OPTO_CEC_TARGET\\t[get_db [get_db current_design] .name]\\t$proof_netlist\"\n",
                );
            }
        }
    }
    std::fs::write(&script, tcl).expect("write Yosys qualification Tcl");
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
    let log =
        std::fs::read_to_string(directory.join("opto.log")).expect("read Yosys qualification log");
    let elaborated_designs = marker_fields(&log, "OPTO_ELABORATED\t", 1)
        .into_iter()
        .map(|fields| fields[0].clone())
        .collect::<Vec<_>>();
    let synthesized_designs = marker_fields(&log, "OPTO_SYNTHESIZED\t", 1)
        .into_iter()
        .map(|fields| fields[0].clone())
        .collect::<Vec<_>>();
    let proof_specs = marker_fields(&log, "OPTO_CEC_TARGET\t", 2)
        .into_iter()
        .map(|fields| (fields[0].clone(), PathBuf::from(&fields[1])))
        .collect::<BTreeMap<_, _>>();
    let reference_inventory_available = reference_designs.is_some();
    let reference_designs = reference_designs.clone().unwrap_or_default();
    let expected_designs = reference_designs
        .iter()
        .map(|design| design.name.clone())
        .collect::<Vec<_>>();
    let observable_designs = reference_designs
        .iter()
        .filter(|design| design.observable)
        .map(|design| design.name.clone())
        .collect::<Vec<_>>();
    let mut deferred_proofs = Vec::new();
    let mut proof_targets = Vec::new();
    if test.expectation == Expectation::Pass {
        for design in reference_designs.iter().filter(|design| design.observable) {
            let key = (test.relative.clone(), design.name.clone());
            if let Some(entry) = proof_audit.get(&key) {
                deferred_proofs.push(DeferredProof {
                    design: design.name.clone(),
                    category: entry.category.clone(),
                    reason: entry.reason.clone(),
                });
            } else {
                proof_targets.push(design.name.clone());
            }
        }
    }
    let mut proved_designs = 0;
    for design in reference_designs
        .iter()
        .filter(|design| design.observable)
        .filter(|design| proof_targets.contains(&design.name))
    {
        let Some(netlist) = proof_specs.get(&design.name) else {
            continue;
        };
        if prove_mapped_equivalence(
            yosys,
            &MappedProof {
                sources: std::slice::from_ref(&test.source),
                systemverilog,
                top: &design.name,
                netlist,
                library,
                log: &netlist.with_extension("log"),
                kind: design.kind,
            },
        ) {
            proved_designs += 1;
        }
    }
    let completed_flow = reference_inventory_available
        && !expected_designs.is_empty()
        && elaborated_designs == expected_designs
        && synthesized_designs == expected_designs;
    let actual_pass =
        process.status.success() && completed_flow && proved_designs == proof_targets.len();
    let matches_audit = match test.expectation {
        Expectation::Pass => actual_pass,
        Expectation::RejectFrontend => !process.status.success() && elaborated_designs.is_empty(),
        Expectation::RejectSynthesis => {
            !process.status.success()
                && reference_inventory_available
                && !expected_designs.is_empty()
                && synthesized_designs.len() < expected_designs.len()
        }
        Expectation::KnownGap => !actual_pass,
        Expectation::Excluded => true,
    };
    let preserve_diagnostic = test.expectation == Expectation::KnownGap || !matches_audit;
    if !preserve_diagnostic {
        std::fs::remove_dir_all(&directory).expect("remove audited Yosys qualification directory");
    }
    YosysTestResult {
        relative: test.relative.clone(),
        expectation: test.expectation,
        category: test.category.clone(),
        reason: test.reason.clone(),
        expected: match test.expectation {
            Expectation::Pass => "pass",
            Expectation::RejectFrontend => "frontend reject",
            Expectation::RejectSynthesis => "synthesis reject",
            Expectation::KnownGap => "known failure",
            Expectation::Excluded => "excluded",
        },
        actual: if actual_pass { "pass" } else { "fail" },
        matches_audit,
        exit_code: process.status.code(),
        signal: exit_signal(process.status),
        elaborated_designs,
        synthesized_designs,
        observable_designs,
        proof_targets,
        deferred_proofs,
        proved_designs,
        diagnostic: preserve_diagnostic.then(|| format!("case-{index:04}/opto.log")),
    }
}

#[cfg(unix)]
fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: std::process::ExitStatus) -> Option<i32> {
    None
}

fn marker_fields(log: &str, prefix: &str, fields: usize) -> Vec<Vec<String>> {
    log.lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .map(|line| {
            let values = line.split('\t').map(str::to_string).collect::<Vec<_>>();
            assert_eq!(
                values.len(),
                fields,
                "malformed Yosys qualification marker '{line}'"
            );
            values
        })
        .collect()
}

fn digest_lines(lines: &[&str]) -> String {
    let mut digest = Sha256::new();
    for line in lines {
        digest.update(line.as_bytes());
        digest.update(b"\n");
    }
    hex_lower(digest.finalize())
}

fn digest_owned_lines(lines: &[String]) -> String {
    digest_lines(&lines.iter().map(String::as_str).collect::<Vec<_>>())
}

fn validate_baseline(report: &Report) {
    let path = workspace_root().join("qualification/upstream/yosys/baseline.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let baseline: Baseline =
        toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
    assert_eq!(baseline.format, 2, "Yosys baseline format mismatch");
    assert_eq!(baseline.source_url, "https://github.com/YosysHQ/yosys.git");
    assert_eq!(
        baseline.revision, REVISION,
        "Yosys baseline revision mismatch"
    );
    assert_eq!(baseline.license, "ISC");
    assert_eq!(baseline.license_sha256, LICENSE_SHA256);
    assert_eq!(baseline.upstream_hdl_cases, report.upstream_hdl_cases);
    assert_eq!(baseline.eligible_cases, report.eligible_cases);
    assert_eq!(baseline.required_cases, report.required_cases);
    assert_eq!(
        baseline.required_conforming_cases,
        report.required_conforming_cases
    );
    assert_eq!(baseline.excluded_cases, report.excluded_cases);
    assert_eq!(baseline.known_gap_cases, report.known_gap_cases);
    assert_eq!(
        baseline.required_conforming_sha256,
        report.required_conforming_sha256
    );
    assert_eq!(baseline.synthesized_designs, report.synthesized_designs);
    assert_eq!(
        baseline.synthesized_designs_sha256,
        report.synthesized_designs_sha256
    );
    assert_eq!(baseline.observable_designs, report.observable_designs);
    assert_eq!(
        baseline.observable_designs_sha256,
        report.observable_designs_sha256
    );
    assert_eq!(baseline.proof_targets, report.proof_targets);
    assert_eq!(baseline.proved_designs, report.proved_designs);
    assert_eq!(baseline.proof_targets_sha256, report.proof_targets_sha256);
    assert_eq!(baseline.deferred_proofs, report.deferred_proofs);
    assert_eq!(
        baseline.deferred_proofs_sha256,
        report.deferred_proofs_sha256
    );
}
