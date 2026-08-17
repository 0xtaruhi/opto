// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(super) const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Suite {
    pub(super) format: u32,
    pub(super) name: String,
    pub(super) cases: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CaseKind {
    Regression,
    Qor,
    Upstream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResultStatus {
    Pass,
    Fail,
}

impl ResultStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum EquivalenceStatus {
    Pass,
    Fail,
    NotRequested,
    NotAvailable,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum EquivalenceInitialState {
    Zero,
}

impl EquivalenceStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotRequested => "not_requested",
            Self::NotAvailable => "not_available",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Flow {
    #[default]
    Elaborate,
    Synth,
    SynthHigh,
}

impl Flow {
    pub(super) const fn command(self) -> Option<&'static str> {
        match self {
            Self::Elaborate => None,
            Self::Synth => Some("synth"),
            Self::SynthHigh => Some("set_db synth_effort high; synth"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Expectation {
    #[default]
    Pass,
    Fail,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Assertions {
    #[serde(rename = "min_ports")]
    pub(super) ports: Option<u64>,
    #[serde(rename = "min_nets")]
    pub(super) nets: Option<u64>,
    #[serde(rename = "min_cells")]
    pub(super) cells: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaseSpec {
    pub(super) format: u32,
    pub(super) id: String,
    pub(super) kind: CaseKind,
    #[serde(default)]
    pub(super) covers: Vec<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) class: Option<String>,
    #[serde(default)]
    pub(super) scenario: Option<String>,
    #[serde(default = "default_language")]
    pub(super) language: String,
    #[serde(default)]
    pub(super) top: String,
    #[serde(default)]
    pub(super) sources: Vec<PathBuf>,
    #[serde(default)]
    pub(super) equivalence_sources: Vec<PathBuf>,
    #[serde(default)]
    pub(super) flow: Flow,
    #[serde(default)]
    pub(super) library: Option<PathBuf>,
    #[serde(default)]
    pub(super) library_key: Option<String>,
    #[serde(default)]
    pub(super) equivalence: bool,
    #[serde(default)]
    pub(super) equivalence_initial_state: Option<EquivalenceInitialState>,
    #[serde(default)]
    pub(super) sequential: bool,
    #[serde(default)]
    pub(super) report_timing: bool,
    #[serde(default)]
    pub(super) clock_period: Option<f64>,
    #[serde(default)]
    pub(super) expected_area: Option<f64>,
    #[serde(default)]
    pub(super) area_tolerance: Option<f64>,
    #[serde(default)]
    pub(super) expected_cells: Option<u64>,
    #[serde(default)]
    pub(super) cell_count_tolerance: Option<f64>,
    #[serde(default)]
    pub(super) expected_cell_histogram: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub(super) expected_worst_slack: Option<f64>,
    #[serde(default)]
    pub(super) worst_slack_tolerance: Option<f64>,
    #[serde(default)]
    pub(super) expected_total_negative_slack: Option<f64>,
    #[serde(default)]
    pub(super) total_negative_slack_tolerance: Option<f64>,
    #[serde(default)]
    pub(super) maximum_violating_paths: Option<u64>,
    #[serde(default)]
    pub(super) maximum_wall_seconds: Option<f64>,
    #[serde(default)]
    pub(super) maximum_cpu_seconds: Option<f64>,
    #[serde(default)]
    pub(super) maximum_peak_rss_kib: Option<u64>,
    #[serde(default)]
    pub(super) expect: Expectation,
    #[serde(default)]
    pub(super) expect_log: Vec<String>,
    #[serde(default)]
    pub(super) defines: Vec<String>,
    #[serde(default)]
    pub(super) constraints: Vec<String>,
    #[serde(default)]
    pub(super) script: Option<PathBuf>,
    #[serde(default)]
    pub(super) assertions: Assertions,
    #[serde(default)]
    pub(super) source_root: Option<String>,
    #[serde(default)]
    pub(super) revision: Option<String>,
    #[serde(default)]
    pub(super) manifest: Option<PathBuf>,
    #[serde(default)]
    pub(super) configs: Option<PathBuf>,
    #[serde(default)]
    pub(super) root_environment: Option<String>,
    #[serde(default)]
    pub(super) manifest_environment: Option<String>,
    #[serde(default)]
    pub(super) report_environment: Option<String>,
    #[serde(default)]
    pub(super) config_environment: Option<String>,
}

fn default_language() -> String {
    "sverilog".to_string()
}

pub(super) struct Case {
    pub(super) path: PathBuf,
    pub(super) spec: CaseSpec,
}

impl Case {
    pub(super) fn relative_path(&self, path: &Path) -> PathBuf {
        self.path
            .parent()
            .expect("case descriptor has a parent")
            .join(path)
    }

    pub(super) fn sources(&self) -> Vec<PathBuf> {
        self.spec
            .sources
            .iter()
            .map(|path| self.relative_path(path))
            .collect()
    }

    pub(super) fn equivalence_sources(&self) -> Vec<PathBuf> {
        let sources = if self.spec.equivalence_sources.is_empty() {
            &self.spec.sources
        } else {
            &self.spec.equivalence_sources
        };
        sources
            .iter()
            .map(|path| self.relative_path(path))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct Metrics {
    pub(super) wall_seconds: f64,
    pub(super) user_seconds: f64,
    pub(super) system_seconds: f64,
    pub(super) cpu_seconds: f64,
    pub(super) peak_rss_kib: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ToolResult {
    pub(super) area: f64,
    pub(super) cells: u64,
    pub(super) cell_histogram: std::collections::BTreeMap<String, u64>,
    pub(super) metrics: Metrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) timing: Option<TimingResult>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct TimingResult {
    pub(super) clock_period: f64,
    pub(super) critical_delay: f64,
    pub(super) worst_slack: f64,
    pub(super) total_negative_slack: f64,
    pub(super) violating_paths: u64,
}

#[derive(Debug, Serialize)]
pub(super) struct ResultEntry {
    pub(super) id: String,
    pub(super) kind: CaseKind,
    pub(super) status: ResultStatus,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) diagnostics: Vec<String>,
    pub(super) inputs: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) scenario: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) opto: Option<ToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) yosys_abc: Option<ToolResult>,
    pub(super) equivalence: EquivalenceStatus,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolIdentity {
    pub(super) path: String,
    pub(super) sha256: String,
    pub(super) version: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ResultDocument {
    pub(super) format: u32,
    pub(super) suite: String,
    pub(super) opto: ToolIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) yosys: Option<ToolIdentity>,
    pub(super) results: Vec<ResultEntry>,
}

#[cfg(test)]
mod tests {
    use super::CaseSpec;

    #[test]
    fn case_spec_defaults_omitted_covers_to_empty() {
        let spec: CaseSpec = toml::from_str("format = 1\nid = 'case'\nkind = 'regression'\n")
            .expect("deserialize case without covers");
        assert!(spec.covers.is_empty());
    }

    #[test]
    fn case_spec_deserializes_populated_covers() {
        let spec: CaseSpec = toml::from_str(
            "format = 1\nid = 'case'\nkind = 'regression'\ncovers = ['first', 'second']\n",
        )
        .expect("deserialize case with covers");
        assert_eq!(spec.covers, ["first", "second"]);
    }
}
