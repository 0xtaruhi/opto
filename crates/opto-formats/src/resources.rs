// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{MessageKind, ReportDocument, ReportField, ReportTable};

/// Resource provenance prepared for presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceReportEntry {
    /// Elaborated design name shown in the report header.
    pub design: String,
    /// Synthesized regions, or `None` when the design has not been synthesized.
    pub implementations: Option<Vec<ResourceImplementationEntry>>,
}

/// One implementation row in a resource report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceImplementationEntry {
    /// One-based region number used to form the user-facing `rN` identifier.
    pub number: u32,
    /// Module in which the shared resource was inferred.
    pub module: String,
    /// Datapath width in bits.
    pub width: u32,
    /// Stable mnemonic prefixed to each contributing source line.
    pub operation_mnemonic: String,
    /// Source file containing the representative operation.
    pub source_file: String,
    /// One-based source line used for the report location.
    pub source_line: u32,
    /// One-based source lines of every operation merged into the resource.
    pub source_lines: Vec<u32>,
    /// Selected implementation or target-cell name.
    pub implementation: String,
}

/// Build resource-sharing reports for the supplied designs in input order.
///
/// # Panics
///
/// Panics only if the statically defined resource table schema is internally
/// inconsistent with its rows.
#[must_use]
pub fn report_resources(reports: &[ResourceReportEntry]) -> ReportDocument {
    let mut document = ReportDocument::new("Resources report");
    for report in reports {
        if reports.len() > 1 {
            document.section(&report.design);
        }
        document.fields([ReportField::new("Design", &report.design)]);
        let Some(implementations) = &report.implementations else {
            document.message(
                MessageKind::Information,
                "Synth the design before reporting resources.",
            );
            continue;
        };
        if implementations.is_empty() {
            document.message(
                MessageKind::Information,
                "No synthesized resources were found.",
            );
            continue;
        }
        let rows = implementations.iter().map(|implementation| {
            let operations = implementation
                .source_lines
                .iter()
                .map(|line| format!("{}_{line}", implementation.operation_mnemonic))
                .collect::<Vec<_>>()
                .join(" ");
            vec![
                format!("r{}", implementation.number),
                implementation.module.clone(),
                implementation.width.to_string(),
                operations,
                implementation.implementation.clone(),
                format!(
                    "{}:{}",
                    implementation.source_file, implementation.source_line
                ),
            ]
        });
        document.table(
            ReportTable::new(
                [
                    "Resource",
                    "Module",
                    "Width",
                    "Operations",
                    "Implementation",
                    "Source",
                ],
                rows,
            )
            .expect("resource rows match the static report schema"),
        );
    }
    document
}
