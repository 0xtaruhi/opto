// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

//! Worker-count sweep for the RFC 0013 Phase 3 scheduling acceptance gates.
//!
//! The gate answers one question: does the ownerless work graph actually scale?
//! It synthesizes the same sealed design at several worker counts and compares
//! the measurements against the thresholds pinned in `benchmarks/scale/scale.toml`.
//!
//! Every threshold lives in the manifest rather than here, because RFC 0013
//! permits revising them only through an amendment backed by checked evidence.
//! A runner that carried its own constants could weaken a gate in a diff that
//! reads like a refactor.

use super::process::run as run_process;
use super::schema::Metrics;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    name: String,
    description: String,
    generator: String,
    top: String,
    clocks: Vec<Clock>,
    guard: Guard,
    tiers: Vec<Tier>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Clock {
    port: String,
    period: f64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Guard {
    minimum_speedup_at_sixteen_workers: f64,
    minimum_average_worker_utilization: f64,
    maximum_coordinator_fraction: f64,
    minimum_ready_tasks_per_worker: u64,
    maximum_peak_memory_ratio: f64,
    worker_counts: Vec<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tier {
    id: String,
    /// Generator tier name; `tier` in the manifest, renamed here because a
    /// field may not repeat its struct's name.
    #[serde(rename = "tier")]
    generator_name: String,
    category: String,
    target_normalized_operations: u64,
    measured_normalized_operations: u64,
    gates_phase_three: bool,
    files: Vec<SourceFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    name: String,
    sha256: String,
}

/// Counters parsed from one synthesis run's log.
#[derive(Clone, Copy, Default, Serialize)]
struct Counters {
    normalized_operations: u64,
    normalized_values: u64,
    lowered_operations: u64,
    mapped_cells: u64,
    batches: u64,
    active_ns: u64,
    wall_ns: u64,
    worker_capacity_ns: u64,
    longest_task_ns: u64,
    estimated_work: u64,
    peak_ready_tasks: u64,
    peak_admitted_memory: u64,
}

#[derive(Serialize)]
struct Sample {
    workers: usize,
    counters: Counters,
    metrics: Metrics,
    /// Fraction of wall time not accounted for by measured scheduler batches.
    ///
    /// RFC 0013 asks for coordinator, partition-publication and commit time.
    /// Those are exactly the parts of the run that are not inside a composite
    /// batch, so the residual is the honest proxy: it cannot undercount the
    /// serial surface, and it charges any unmeasured stage against the gate.
    coordinator_fraction: f64,
    /// `active / worker_capacity` over composite batches.
    worker_utilization: f64,
    ready_tasks_per_worker: f64,
}

#[derive(Serialize)]
struct ResultDocument {
    suite: String,
    description: String,
    tier: String,
    category: String,
    top: String,
    guard: Guard,
    target_normalized_operations: u64,
    samples: Vec<Sample>,
    speedup_at_maximum_workers: f64,
    peak_memory_ratio: f64,
    failures: Vec<String>,
}

pub(super) fn run(relative_manifest: &str) {
    let root = super::workspace_root();
    let manifest_path = root.join(relative_manifest);
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display()));
    let manifest: Manifest = toml::from_str(&text)
        .unwrap_or_else(|error| panic!("parse {}: {error}", manifest_path.display()));
    assert_eq!(manifest.format, 1, "unsupported scale manifest format");

    let tier = manifest
        .tiers
        .iter()
        .find(|tier| tier.gates_phase_three)
        .unwrap_or_else(|| {
            panic!(
                "{}: no tier carries the Phase 3 gate",
                manifest_path.display()
            )
        });

    // The manifest checker already rejects an uncalibrated gating tier, but the
    // runner repeats the check: a measurement is worthless if the design it ran
    // on never reached the size the phase requires.
    assert!(
        tier.measured_normalized_operations >= 1_000_000,
        "tier '{}' records {} normalized operations; RFC 0013 Phase 3 requires at least one million",
        tier.id,
        tier.measured_normalized_operations
    );

    let output = super::output_directory(&manifest.name);
    if output.exists() {
        std::fs::remove_dir_all(&output)
            .unwrap_or_else(|error| panic!("remove {}: {error}", output.display()));
    }
    std::fs::create_dir_all(&output)
        .unwrap_or_else(|error| panic!("create {}: {error}", output.display()));

    let sources = output.join("rtl");
    generate_sources(
        &manifest_path,
        &manifest.generator,
        &tier.generator_name,
        &sources,
    );
    verify_sources(tier, &sources);

    let library = required_library();
    let opto = PathBuf::from(env!("CARGO_BIN_EXE_opto"));

    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for &workers in &manifest.guard.worker_counts {
        let sample = measure(&opto, &manifest, tier, &sources, &library, &output, workers);

        // The sealed design must be identical at every worker count. If it is
        // not, the sweep is comparing different designs and no ratio computed
        // from it means anything, so this is checked before the gates.
        if let Some(first) = samples.first() {
            let first: &Sample = first;
            assert_eq!(
                sample.counters.normalized_operations, first.counters.normalized_operations,
                "sealed operation count changed between {} and {workers} workers; \
                 the sweep is not measuring one design",
                first.workers
            );
        }
        samples.push(sample);
    }

    let baseline = samples
        .first()
        .expect("worker_counts is non-empty and validated by the manifest checker");
    let scaled = samples
        .last()
        .expect("worker_counts is non-empty and validated by the manifest checker");

    let speedup = if scaled.metrics.wall_seconds > 0.0 {
        baseline.metrics.wall_seconds / scaled.metrics.wall_seconds
    } else {
        0.0
    };
    let memory_ratio = if baseline.metrics.peak_rss_kib > 0 {
        f64::from(u32::try_from(scaled.metrics.peak_rss_kib).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(baseline.metrics.peak_rss_kib).unwrap_or(u32::MAX))
    } else {
        f64::INFINITY
    };

    let guard = &manifest.guard;
    if speedup < guard.minimum_speedup_at_sixteen_workers {
        failures.push(format!(
            "speedup at {} workers is {speedup:.2}x, below the required {:.2}x",
            scaled.workers, guard.minimum_speedup_at_sixteen_workers
        ));
    }
    if memory_ratio > guard.maximum_peak_memory_ratio {
        failures.push(format!(
            "peak resident memory at {} workers is {memory_ratio:.2}x the one-worker path, \
             above the permitted {:.2}x",
            scaled.workers, guard.maximum_peak_memory_ratio
        ));
    }
    for sample in &samples {
        if sample.worker_utilization < guard.minimum_average_worker_utilization {
            failures.push(format!(
                "worker utilization at {} workers is {:.1}%, below the required {:.1}%",
                sample.workers,
                sample.worker_utilization * 100.0,
                guard.minimum_average_worker_utilization * 100.0
            ));
        }
        if sample.coordinator_fraction > guard.maximum_coordinator_fraction {
            failures.push(format!(
                "coordinator and commit time at {} workers is {:.1}% of wall time, \
                 above the permitted {:.1}%",
                sample.workers,
                sample.coordinator_fraction * 100.0,
                guard.maximum_coordinator_fraction * 100.0
            ));
        }
        // The RFC exempts a graph that genuinely exposes less parallelism, so
        // the single-worker point is not held to the ready-depth target.
        let ready_floor = guard.minimum_ready_tasks_per_worker;
        #[allow(clippy::cast_precision_loss, reason = "task counts are far below 2^53")]
        let floor = ready_floor as f64;
        if sample.workers > 1 && sample.ready_tasks_per_worker < floor {
            failures.push(format!(
                "ready fine tasks per worker at {} workers is {:.1}, below the required {floor:.1}",
                sample.workers, sample.ready_tasks_per_worker
            ));
        }
    }

    let document = ResultDocument {
        suite: manifest.name.clone(),
        description: manifest.description.clone(),
        tier: tier.id.clone(),
        category: tier.category.clone(),
        top: manifest.top.clone(),
        guard: guard.clone(),
        target_normalized_operations: tier.target_normalized_operations,
        samples,
        speedup_at_maximum_workers: speedup,
        peak_memory_ratio: memory_ratio,
        failures: failures.clone(),
    };
    let serialized =
        serde_json::to_string_pretty(&document).expect("serialize scale scaling result");
    // Written before any assertion so a failing gate still leaves the evidence
    // that explains it.
    std::fs::write(output.join("results.json"), format!("{serialized}\n"))
        .unwrap_or_else(|error| panic!("write scale results: {error}"));

    assert!(
        failures.is_empty(),
        "RFC 0013 Phase 3 scaling gate failed:\n  {}",
        failures.join("\n  ")
    );
}

fn generate_sources(manifest_path: &Path, generator: &str, tier: &str, destination: &Path) {
    let generator = manifest_path
        .parent()
        .unwrap_or_else(|| panic!("{} has no parent", manifest_path.display()))
        .join(generator);
    let python = std::env::var_os("OPTO_PYTHON").unwrap_or_else(|| OsString::from("python3"));
    let status = Command::new(&python)
        .arg(&generator)
        .arg("--tier")
        .arg(tier)
        .arg(destination)
        .status()
        .unwrap_or_else(|error| panic!("run {}: {error}", generator.display()));
    assert!(
        status.success(),
        "{} failed for tier {tier}",
        generator.display()
    );
}

fn verify_sources(tier: &Tier, sources: &Path) {
    for file in &tier.files {
        let path = sources.join(&file.name);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let digest = super::hex_lower(Sha256::digest(&bytes));
        assert_eq!(
            digest, file.sha256,
            "generated {} does not match the hash pinned for tier '{}'",
            file.name, tier.id
        );
    }
}

fn required_library() -> PathBuf {
    let variable = "OPTO_LIBRARY_SCALE";
    let value = std::env::var_os(variable)
        .unwrap_or_else(|| panic!("{variable} must point at the Liberty library for the sweep"));
    let path = PathBuf::from(value);
    assert!(
        path.is_file(),
        "{variable} is not a file: {}",
        path.display()
    );
    path
}

fn measure(
    opto: &Path,
    manifest: &Manifest,
    tier: &Tier,
    sources: &Path,
    library: &Path,
    output: &Path,
    workers: usize,
) -> Sample {
    let case_output = output.join(format!("workers-{workers}"));
    std::fs::create_dir_all(&case_output)
        .unwrap_or_else(|error| panic!("create {}: {error}", case_output.display()));

    let script = case_output.join("flow.tcl");
    std::fs::write(&script, flow_tcl(manifest, tier, sources, library))
        .unwrap_or_else(|error| panic!("write {}: {error}", script.display()));

    let log = case_output.join("synthesis.log");
    let result = run_process(
        opto,
        [
            OsString::from("--no-home-init"),
            OsString::from("--no-local-init"),
            OsString::from("--threads"),
            OsString::from(workers.to_string()),
            OsString::from("-f"),
            script.clone().into_os_string(),
        ],
        &BTreeMap::new(),
        &log,
        true,
    );
    assert!(
        result.status.success(),
        "synthesis at {workers} workers failed; see {}",
        log.display()
    );

    let counters = parse_counters(&log);
    let sample_metrics = result.metrics;

    #[allow(
        clippy::cast_precision_loss,
        reason = "nanosecond counters stay well below 2^53 for any realistic run"
    )]
    let worker_utilization = if counters.worker_capacity_ns > 0 {
        counters.active_ns as f64 / counters.worker_capacity_ns as f64
    } else {
        0.0
    };

    #[allow(
        clippy::cast_precision_loss,
        reason = "nanosecond counters stay well below 2^53 for any realistic run"
    )]
    let scheduler_seconds = counters.wall_ns as f64 / 1e9;
    let coordinator_fraction = if sample_metrics.wall_seconds > 0.0 {
        ((sample_metrics.wall_seconds - scheduler_seconds) / sample_metrics.wall_seconds).max(0.0)
    } else {
        1.0
    };

    #[allow(clippy::cast_precision_loss, reason = "task counts are far below 2^53")]
    let ready_tasks_per_worker = counters.peak_ready_tasks as f64 / workers as f64;

    Sample {
        workers,
        counters,
        metrics: sample_metrics,
        coordinator_fraction,
        worker_utilization,
        ready_tasks_per_worker,
    }
}

fn flow_tcl(manifest: &Manifest, tier: &Tier, sources: &Path, library: &Path) -> String {
    let mut script = String::new();
    writeln!(script, "read_libs {}", super::tcl_word(library)).expect("format flow");
    let files = tier
        .files
        .iter()
        .map(|file| super::tcl_word(sources.join(&file.name)))
        .collect::<Vec<_>>()
        .join(" ");
    writeln!(script, "read_hdl [list {files}]").expect("format flow");
    writeln!(script, "elaborate {}", manifest.top).expect("format flow");
    for clock in &manifest.clocks {
        writeln!(
            script,
            "create_clock -name {} -period {} [get_ports {}]",
            clock.port, clock.period, clock.port
        )
        .expect("format flow");
    }
    writeln!(script, "synth").expect("format flow");
    writeln!(script, "exit").expect("format flow");
    script
}

/// Read the counters Opto prints when a synthesis artifact completes.
fn parse_counters(log: &Path) -> Counters {
    let text = std::fs::read_to_string(log)
        .unwrap_or_else(|error| panic!("read {}: {error}", log.display()));
    let mut counters = Counters::default();
    let mut sealed_seen = false;
    let mut execution_seen = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Sealed design:") {
            let fields = key_values(rest);
            counters.normalized_operations = field(&fields, "normalized_operations");
            counters.normalized_values = field(&fields, "normalized_values");
            counters.lowered_operations = field(&fields, "lowered_operations");
            counters.mapped_cells = field(&fields, "mapped_cells");
            sealed_seen = true;
        } else if let Some(rest) = line.strip_prefix("Scheduler execution:") {
            let fields = key_values(rest);
            counters.batches = field(&fields, "batches");
            counters.active_ns = field(&fields, "active_ns");
            counters.wall_ns = field(&fields, "wall_ns");
            counters.worker_capacity_ns = field(&fields, "worker_capacity_ns");
            counters.longest_task_ns = field(&fields, "longest_task_ns");
            counters.estimated_work = field(&fields, "estimated_work");
            counters.peak_ready_tasks = field(&fields, "peak_ready_tasks");
            counters.peak_admitted_memory = field(&fields, "peak_admitted_memory");
            execution_seen = true;
        }
    }
    assert!(
        sealed_seen && execution_seen,
        "{} does not contain the sealed-design and scheduler-execution counters",
        log.display()
    );
    counters
}

fn key_values(text: &str) -> BTreeMap<&str, &str> {
    text.trim()
        .trim_end_matches('.')
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .collect()
}

fn field(fields: &BTreeMap<&str, &str>, name: &str) -> u64 {
    fields
        .get(name)
        .unwrap_or_else(|| panic!("counter '{name}' is missing from the synthesis log"))
        .parse()
        .unwrap_or_else(|error| panic!("counter '{name}' is not an integer: {error}"))
}
