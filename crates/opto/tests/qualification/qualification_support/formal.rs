// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::process::run;
use super::yosys_quote;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) struct MappedProof<'a> {
    pub(super) sources: &'a [PathBuf],
    pub(super) systemverilog: bool,
    pub(super) top: &'a str,
    pub(super) netlist: &'a Path,
    pub(super) library: &'a Path,
    pub(super) log: &'a Path,
    pub(super) kind: ProofKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProofKind {
    Combinational,
    Sequential,
}

#[derive(Clone)]
pub(super) struct ReferenceDesign {
    pub(super) name: String,
    pub(super) kind: ProofKind,
    pub(super) observable: bool,
}

#[derive(Debug, Deserialize)]
struct YosysJson {
    modules: BTreeMap<String, YosysModule>,
}

#[derive(Debug, Deserialize)]
struct YosysModule {
    #[serde(default)]
    attributes: BTreeMap<String, String>,
    #[serde(default)]
    ports: BTreeMap<String, YosysPort>,
    #[serde(default)]
    cells: BTreeMap<String, YosysCell>,
}

#[derive(Debug, Deserialize)]
struct YosysPort {
    direction: String,
}

#[derive(Debug, Deserialize)]
struct YosysCell {
    #[serde(rename = "type")]
    kind: String,
}

pub(super) fn defined_root_designs(
    yosys: &Path,
    sources: &[PathBuf],
    systemverilog: bool,
    json: &Path,
    log: &Path,
) -> Option<Vec<ReferenceDesign>> {
    let read_flag = if systemverilog { " -sv" } else { "" };
    let sources = sources
        .iter()
        .map(yosys_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let commands = format!(
        "read_verilog{read_flag} {sources}; proc; write_json {}",
        yosys_quote(json)
    );
    let output = run(
        yosys,
        [
            OsString::from("-Q"),
            OsString::from("-p"),
            OsString::from(commands),
        ],
        &BTreeMap::new(),
        log,
        false,
    );
    if !output.status.success() {
        return None;
    }
    let text = std::fs::read_to_string(json).ok()?;
    let design: YosysJson = serde_json::from_str(&text).ok()?;
    let mut stateful = design
        .modules
        .iter()
        .filter(|(_, module)| {
            module
                .cells
                .values()
                .any(|cell| is_stateful_cell(&cell.kind))
        })
        .map(|(name, _)| name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    loop {
        let inherited = design
            .modules
            .iter()
            .filter(|(name, module)| {
                !stateful.contains(*name)
                    && module
                        .cells
                        .values()
                        .any(|cell| stateful.contains(&cell.kind))
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if inherited.is_empty() {
            break;
        }
        stateful.extend(inherited);
    }
    let referenced = design
        .modules
        .values()
        .flat_map(|module| module.cells.values().map(|cell| cell.kind.clone()))
        .filter(|kind| design.modules.contains_key(kind))
        .collect::<std::collections::BTreeSet<_>>();
    Some(
        design
            .modules
            .iter()
            .filter(|(_, module)| {
                !module
                    .attributes
                    .get("blackbox")
                    .is_some_and(|value| value.ends_with('1'))
                    && !module.ports.is_empty()
            })
            .filter(|(name, _)| !referenced.contains(*name))
            .map(|(name, module)| ReferenceDesign {
                name: name.clone(),
                kind: if stateful.contains(name) {
                    ProofKind::Sequential
                } else {
                    ProofKind::Combinational
                },
                observable: module.ports.values().any(|port| port.direction == "output"),
            })
            .collect(),
    )
}

fn is_stateful_cell(kind: &str) -> bool {
    let kind = kind.trim_start_matches('\\').to_ascii_lowercase();
    kind.starts_with("$dff")
        || kind.starts_with("$adff")
        || kind.starts_with("$aldff")
        || kind.starts_with("$sdff")
        || kind.starts_with("$dlatch")
        || kind.starts_with("$mem")
}

pub(super) fn prove_mapped_equivalence(yosys: &Path, proof: &MappedProof<'_>) -> bool {
    match proof.kind {
        ProofKind::Combinational => {
            let mut miter = proof_prelude(proof);
            miter.extend([
                "miter -equiv -ignore_gold_x -make_assert gold gate miter".to_string(),
                "hierarchy -check -top miter".to_string(),
                "flatten; opt_clean".to_string(),
                "sat -verify -prove-asserts -set-def-inputs -enable_undef -show-inputs -show-outputs"
                    .to_string(),
            ]);
            if run_yosys_proof(yosys, &miter, proof.log) {
                return true;
            }
            let mut undef_equivalence = proof_prelude(proof);
            undef_equivalence.extend([
                "equiv_make gold gate equiv".to_string(),
                "hierarchy -check -top equiv".to_string(),
                "equiv_struct".to_string(),
                "equiv_simple -undef".to_string(),
                "equiv_status -assert".to_string(),
            ]);
            let stem = proof
                .log
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("proof");
            let alternate_log = proof.log.with_file_name(format!("{stem}-undef.log"));
            run_yosys_proof(yosys, &undef_equivalence, &alternate_log)
        }
        ProofKind::Sequential => {
            let mut commands = proof_prelude(proof);
            commands.extend([
                "equiv_make gold gate equiv".to_string(),
                "hierarchy -check -top equiv".to_string(),
                "equiv_struct".to_string(),
                "equiv_simple -undef -seq 8".to_string(),
                "equiv_induct -undef -seq 8".to_string(),
                "equiv_status -assert".to_string(),
            ]);
            run_yosys_proof(yosys, &commands, proof.log)
        }
    }
}

fn proof_prelude(proof: &MappedProof<'_>) -> Vec<String> {
    // Keep latch state explicit on both sides. `async2sync` can hide the state
    // relation behind an asserted reset and create false CEC failures.
    let read_flag = if proof.systemverilog { " -sv" } else { "" };
    let sources = proof
        .sources
        .iter()
        .map(yosys_quote)
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        format!("read_verilog{read_flag} {sources}"),
        format!("hierarchy -check -top {}", proof.top),
        "proc; flatten; memory; opt; techmap; opt; clk2fflogic; opt".to_string(),
        format!("rename {} gold", proof.top),
        "design -stash gold".to_string(),
        format!(
            "read_liberty -ignore_miss_func {}",
            yosys_quote(proof.library)
        ),
        format!("read_verilog {}", yosys_quote(proof.netlist)),
        format!("hierarchy -check -top {}", proof.top),
        "flatten; proc; memory; opt; clk2fflogic; opt".to_string(),
        format!("rename {} gate", proof.top),
        "design -stash gate".to_string(),
        "design -reset".to_string(),
        format!(
            "read_liberty -ignore_miss_func {}",
            yosys_quote(proof.library)
        ),
        "design -copy-from gold -as gold gold".to_string(),
        "design -copy-from gate -as gate gate".to_string(),
    ]
}

fn run_yosys_proof(yosys: &Path, commands: &[String], log: &Path) -> bool {
    run(
        yosys,
        [
            OsString::from("-Q"),
            OsString::from("-p"),
            OsString::from(commands.join("; ")),
        ],
        &BTreeMap::new(),
        log,
        false,
    )
    .status
    .success()
}
