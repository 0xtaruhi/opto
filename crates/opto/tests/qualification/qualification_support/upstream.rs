// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::process::run as run_process;
use super::schema::{Case, EquivalenceStatus, ResultEntry, ResultStatus};
use super::{prepare_case_output, report_integer, sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) fn run(case: &Case, opto: &Path, output_root: &Path) -> ResultEntry {
    let source_key = case
        .spec
        .source_root
        .as_deref()
        .expect("validated source root");
    let source_variable = format!("OPTO_SOURCE_{}", source_key.to_ascii_uppercase());
    let source_root = std::env::var_os(&source_variable).map_or_else(
        || panic!("{source_variable} must point to the pinned checkout"),
        PathBuf::from,
    );
    let manifest = case.relative_path(case.spec.manifest.as_deref().expect("validated manifest"));
    validate_checkout(case, &source_root, &manifest);
    let output = prepare_case_output(output_root, &case.spec.id);
    let script = case.relative_path(case.spec.script.as_deref().expect("validated script"));
    let runs = upstream_runs(case, &source_root);
    let mut failures = Vec::new();
    for config in runs {
        let report = output.join(format!("check-{}.rpt", config.id));
        let mut environment = BTreeMap::from([
            (
                case.spec
                    .root_environment
                    .clone()
                    .expect("upstream root environment"),
                source_root.clone(),
            ),
            (
                case.spec
                    .manifest_environment
                    .clone()
                    .expect("upstream manifest environment"),
                manifest.clone(),
            ),
            (
                case.spec
                    .report_environment
                    .clone()
                    .expect("upstream report environment"),
                report.clone(),
            ),
        ]);
        if let Some((key, value)) = &config.environment {
            environment.insert(key.clone(), value.clone());
        }
        let process = run_process(
            opto,
            vec![
                OsString::from("--no-init"),
                OsString::from("-f"),
                script.as_os_str().to_owned(),
            ],
            &environment,
            &output.join(format!("opto-{}.log", config.id)),
            true,
        );
        if !process.status.success() {
            failures.push(format!("{}: synthesis process failed", config.id));
            continue;
        }
        let ports = report_integer(&report, "Number of ports");
        let nets = report_integer(&report, "Number of nets");
        if ports.is_none_or(|value| value < config.min_ports) {
            failures.push(format!(
                "{}: expected ports >= {}, got {ports:?}",
                config.id, config.min_ports
            ));
        }
        if nets.is_none_or(|value| value < config.min_nets) {
            failures.push(format!(
                "{}: expected nets >= {}, got {nets:?}",
                config.id, config.min_nets
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "upstream case {} failed: {}",
        case.spec.id,
        failures.join("; ")
    );
    ResultEntry {
        id: case.spec.id.clone(),
        kind: case.spec.kind,
        status: ResultStatus::Pass,
        diagnostics: Vec::new(),
        inputs: upstream_inputs(case, &manifest),
        category: case.spec.category.clone(),
        class: None,
        scenario: None,
        opto: None,
        yosys_abc: None,
        equivalence: EquivalenceStatus::NotAvailable,
    }
}

fn upstream_inputs(case: &Case, manifest: &Path) -> BTreeMap<String, String> {
    let script = case.relative_path(case.spec.script.as_deref().expect("validated script"));
    let mut inputs = BTreeMap::from([
        ("case.toml".to_string(), sha256(&case.path)),
        ("flow.tcl".to_string(), sha256(&script)),
        ("manifest".to_string(), sha256(manifest)),
    ]);
    if let Some(configs) = &case.spec.configs {
        inputs.insert("configs".to_string(), sha256(&case.relative_path(configs)));
    }
    if let Some(designs) = &case.spec.designs {
        inputs.insert("designs".to_string(), sha256(&case.relative_path(designs)));
    }
    inputs
}

struct UpstreamRun {
    id: String,
    environment: Option<(String, PathBuf)>,
    min_ports: u64,
    min_nets: u64,
}

fn upstream_runs(case: &Case, source_root: &Path) -> Vec<UpstreamRun> {
    if let Some(configs) = &case.spec.configs {
        let path = case.relative_path(configs);
        let runs = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .map(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                assert_eq!(fields.len(), 5, "invalid config row: {line}");
                let config = source_root.join(fields[1]);
                assert!(config.is_file(), "missing config {}", config.display());
                assert_eq!(sha256(&config), fields[2], "config hash mismatch");
                UpstreamRun {
                    id: fields[0].to_string(),
                    environment: Some((
                        case.spec
                            .config_environment
                            .clone()
                            .expect("validated upstream config environment"),
                        config,
                    )),
                    min_ports: fields[3].parse().expect("minimum ports is an integer"),
                    min_nets: fields[4].parse().expect("minimum nets is an integer"),
                }
            })
            .collect();
        return validate_runs(runs);
    }
    if let Some(designs) = &case.spec.designs {
        let path = case.relative_path(designs);
        let runs = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
            .map(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                assert_eq!(fields.len(), 4, "invalid design row: {line}");
                let id = fields[0].trim();
                let top = fields[1].trim();
                assert!(!id.is_empty(), "upstream design id is empty");
                assert!(
                    !top.is_empty() && !top.chars().any(char::is_whitespace),
                    "invalid upstream design top: {top}"
                );
                UpstreamRun {
                    id: id.to_string(),
                    environment: Some((
                        case.spec
                            .design_environment
                            .clone()
                            .expect("validated upstream design environment"),
                        PathBuf::from(top),
                    )),
                    min_ports: fields[2].parse().expect("minimum ports is an integer"),
                    min_nets: fields[3].parse().expect("minimum nets is an integer"),
                }
            })
            .collect();
        return validate_runs(runs);
    }
    vec![UpstreamRun {
        id: case.spec.id.clone(),
        environment: None,
        min_ports: case.spec.assertions.ports.unwrap_or(0),
        min_nets: case.spec.assertions.nets.unwrap_or(0),
    }]
}

fn validate_runs(runs: Vec<UpstreamRun>) -> Vec<UpstreamRun> {
    assert!(!runs.is_empty(), "upstream run table is empty");
    let mut ids = std::collections::BTreeSet::new();
    for run in &runs {
        assert!(
            run.id.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            ),
            "invalid upstream run id: {}",
            run.id
        );
        assert!(ids.insert(&run.id), "duplicate upstream run id: {}", run.id);
    }
    runs
}

fn validate_checkout(case: &Case, source_root: &Path, manifest: &Path) {
    let revision = git_revision(source_root);
    assert_eq!(
        revision,
        case.spec.revision.as_deref().expect("validated revision"),
        "{} revision mismatch",
        case.spec.id
    );
    let text = std::fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest.display()));
    let lines = text.lines().collect::<Vec<_>>();
    assert!(lines.len() >= 4, "manifest is incomplete");
    assert_eq!(lines[0], "# repository_commit", "invalid manifest header");
    assert_eq!(lines[1], revision, "manifest revision mismatch");
    assert_eq!(
        lines[2].replace("\\t", "\t"),
        "# relative_path\tsha256",
        "invalid manifest column header"
    );
    let mut validated_sources = 0usize;
    for line in &lines[3..] {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (relative, expected) = line.split_once('\t').expect("manifest row has two fields");
        if relative == "@CONFIG@" {
            continue;
        }
        let source = source_root.join(relative);
        assert!(
            source.is_file(),
            "missing pinned source {}",
            source.display()
        );
        assert_eq!(
            sha256(&source),
            expected,
            "source hash mismatch: {relative}"
        );
        validated_sources += 1;
    }
    assert!(
        validated_sources > 0,
        "{} manifest records no pinned source hashes",
        case.spec.id
    );
}

fn git_revision(root: &Path) -> String {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            root.to_str().expect("UTF-8 upstream path"),
            "rev-parse",
            "HEAD",
        ])
        .output()
        .expect("run git rev-parse");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8(output.stdout)
        .expect("Git revision is UTF-8")
        .trim()
        .to_string()
}
