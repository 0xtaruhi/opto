// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{MessageKind, ReportDocument, ReportField, ReportTable};
use opto_power::{ActivityOrigin, PowerAnalysis, PowerError};
use opto_timing::TimingModel;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// Aggregation level selected for a power report.
pub enum PowerReportKind {
    /// Design-wide internal, switching, leakage, and total power.
    #[default]
    Summary,
    /// Per-cell internal, switching, leakage, and total power.
    Cell,
    /// Per-net switching activity and dynamic power.
    Net,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Formatting controls accepted by `report_power`.
pub struct ReportPowerOptions {
    /// Select the design-wide, per-cell, or per-net schema.
    pub kind: PowerReportKind,
    /// Include primary-input nets in a net report.
    pub include_input_nets: bool,
    /// Render object names without hierarchy grouping.
    pub flat: bool,
}

impl Default for ReportPowerOptions {
    fn default() -> Self {
        Self {
            kind: PowerReportKind::Summary,
            include_input_nets: false,
            flat: false,
        }
    }
}

/// Format a sealed power analysis without recomputing timing or activity.
///
/// # Errors
///
/// Returns [`PowerError`] if the analysis refers to a cell, net, or timing
/// object that is absent from `model`.
pub fn report_power(
    model: &TimingModel,
    analysis: &PowerAnalysis,
    options: &ReportPowerOptions,
) -> Result<ReportDocument, PowerError> {
    let mut report = power_document(analysis, options);
    match options.kind {
        PowerReportKind::Summary => append_summary(model, analysis, &mut report)?,
        PowerReportKind::Cell => append_cells(model, analysis, &mut report)?,
        PowerReportKind::Net => append_nets(model, analysis, options, &mut report)?,
    }
    Ok(report)
}

fn power_document(analysis: &PowerAnalysis, options: &ReportPowerOptions) -> ReportDocument {
    let mut report = ReportDocument::new("Power report");
    let view = match options.kind {
        PowerReportKind::Summary => "Summary",
        PowerReportKind::Cell => "Cell",
        PowerReportKind::Net => "Net",
    };
    report.fields([
        ReportField::new("Design", analysis.design()),
        ReportField::new("Version", format!("opto {}", env!("CARGO_PKG_VERSION"))),
        ReportField::new("Date", crate::report_timestamp()),
        ReportField::new("View", view),
        ReportField::new(
            "Hierarchy",
            if options.flat { "Flat" } else { "Hierarchical" },
        ),
    ]);
    if !analysis.libraries().is_empty() {
        report.section("Libraries");
        report.table(
            ReportTable::new(
                ["Library", "Source"],
                analysis.libraries().iter().map(|library| {
                    [
                        library.name.clone(),
                        library.source.clone().unwrap_or_else(|| "-".to_string()),
                    ]
                }),
            )
            .expect("power library rows match the static report schema"),
        );
    }
    report.section("Operating conditions");
    report.fields([
        ReportField::new(
            "Operating conditions",
            analysis.operating_conditions().unwrap_or("nom_pvt"),
        ),
        ReportField::new(
            "Wire load model mode",
            analysis.wire_load_mode().unwrap_or("top"),
        ),
        ReportField::new(
            "Global operating voltage",
            format_decimal(analysis.voltage()),
        ),
        ReportField::new(
            "Voltage units",
            format_engineering_unit(analysis.voltage_unit_volts(), "V"),
        ),
        ReportField::new(
            "Capacitance units",
            format_engineering_unit(analysis.capacitance_unit_farads(), "F"),
        ),
        ReportField::new(
            "Time units",
            format_engineering_unit(analysis.time_unit_seconds(), "s"),
        ),
        ReportField::new(
            "Dynamic power units",
            format_engineering_unit(analysis.dynamic_power_unit_watts(), "W"),
        ),
        ReportField::new(
            "Leakage power units",
            format_engineering_unit(analysis.leakage_power_unit_watts(), "W"),
        ),
    ]);
    report
}

fn append_summary(
    model: &TimingModel,
    analysis: &PowerAnalysis,
    report: &mut ReportDocument,
) -> Result<(), PowerError> {
    let summary = analysis.summary();
    let internal = display_power(summary.internal_watts, (1e-3, "mW"));
    let switching = display_power(summary.switching_watts, (1e-3, "mW"));
    let dynamic = display_power(summary.dynamic_watts(), (1e-3, "mW"));
    let leakage = display_power(summary.leakage_watts, (1e-9, "nW"));
    let dynamic_watts = summary.dynamic_watts();
    report.section("Summary");
    report.fields([
        ReportField::new(
            "Cell internal power",
            format!(
                "{:.4} {} ({:.0}%)",
                internal.0,
                internal.1,
                dynamic_percent(summary.internal_watts, dynamic_watts)
            ),
        ),
        ReportField::new(
            "Net switching power",
            format!(
                "{:.4} {} ({:.0}%)",
                switching.0,
                switching.1,
                dynamic_percent(summary.switching_watts, dynamic_watts)
            ),
        ),
        ReportField::new(
            "Total dynamic power",
            format!("{:.4} {}", dynamic.0, dynamic.1),
        ),
        ReportField::new(
            "Cell leakage power",
            format!("{:.4} {}", leakage.0, leakage.1),
        ),
    ]);
    report.message(
        MessageKind::Information,
        "Power group summary excludes estimated clock-tree power.",
    );

    let combinational =
        analysis
            .cells(model)?
            .filter(|cell| !cell.sequential)
            .fold([0.0; 3], |mut sum, cell| {
                sum[0] += cell.internal_watts;
                sum[1] += cell.switching_watts;
                sum[2] += cell.leakage_watts;
                sum
            });
    let sequential =
        analysis
            .cells(model)?
            .filter(|cell| cell.sequential)
            .fold([0.0; 3], |mut sum, cell| {
                sum[0] += cell.internal_watts;
                sum[1] += cell.switching_watts;
                sum[2] += cell.leakage_watts;
                sum
            });
    let mut rows = [
        ("io_pad", [0.0; 3], ""),
        ("memory", [0.0; 3], ""),
        ("black_box", [0.0; 3], ""),
        ("clock_network", [0.0; 3], "i"),
        ("register", [0.0; 3], ""),
        ("sequential", sequential, ""),
        ("combinational", combinational, ""),
    ]
    .into_iter()
    .map(|(name, values, attrs)| power_group_row(analysis, name, values, attrs))
    .collect::<Vec<_>>();
    rows.push(power_group_row(
        analysis,
        "Total",
        [
            summary.internal_watts,
            summary.switching_watts,
            summary.leakage_watts,
        ],
        "",
    ));
    report.section("Power groups");
    report.table(
        ReportTable::new(
            [
                "Power group",
                "Internal",
                "Switching",
                "Leakage",
                "Total",
                "%",
                "Attrs",
            ],
            rows,
        )
        .expect("power group rows match the static report schema"),
    );
    Ok(())
}

fn power_group_row(
    analysis: &PowerAnalysis,
    name: &str,
    values: [f64; 3],
    attributes: &str,
) -> Vec<String> {
    let total = values.iter().sum::<f64>();
    vec![
        name.to_string(),
        format_power_value(
            group_dynamic(values[0], analysis.dynamic_power_unit_watts()),
            analysis.dynamic_power_unit_watts(),
            true,
        ),
        format_power_value(
            group_dynamic(values[1], analysis.dynamic_power_unit_watts()),
            analysis.dynamic_power_unit_watts(),
            true,
        ),
        format_power_value(
            group_leakage(values[2], analysis.leakage_power_unit_watts()),
            analysis.leakage_power_unit_watts(),
            false,
        ),
        format_power_value(
            group_dynamic(total, analysis.dynamic_power_unit_watts()),
            analysis.dynamic_power_unit_watts(),
            false,
        ),
        format!(
            "{:.2}",
            dynamic_percent(total, analysis.summary().total_watts())
        ),
        attributes.to_string(),
    ]
}

fn append_cells(
    model: &TimingModel,
    analysis: &PowerAnalysis,
    report: &mut ReportDocument,
) -> Result<(), PowerError> {
    let summary = analysis.summary();
    let total_dynamic = summary.dynamic_watts();
    let mut cells = analysis.cells(model)?.collect::<Vec<_>>();
    cells.sort_by(|left, right| {
        right
            .internal_watts
            .total_cmp(&left.internal_watts)
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut rows = cells
        .into_iter()
        .map(|cell| {
            let dynamic_watts = cell.dynamic_watts();
            vec![
                cell.name.into_owned(),
                format_fixed(group_dynamic(
                    cell.internal_watts,
                    analysis.dynamic_power_unit_watts(),
                )),
                format_fixed(group_dynamic(
                    cell.switching_watts,
                    analysis.dynamic_power_unit_watts(),
                )),
                format_scientific(
                    group_dynamic(dynamic_watts, analysis.dynamic_power_unit_watts()),
                    2,
                ),
                format!("{:.0}", dynamic_percent(dynamic_watts, total_dynamic)),
                format_fixed(group_leakage(
                    cell.leakage_watts,
                    analysis.leakage_power_unit_watts(),
                )),
                String::new(),
            ]
        })
        .collect::<Vec<_>>();
    rows.push(vec![
        format!("Total ({} cells)", model.instance_count()),
        format_fixed(group_dynamic(
            summary.internal_watts,
            analysis.dynamic_power_unit_watts(),
        )),
        format_fixed(group_dynamic(
            summary.switching_watts,
            analysis.dynamic_power_unit_watts(),
        )),
        format_scientific(
            group_dynamic(summary.dynamic_watts(), analysis.dynamic_power_unit_watts()),
            2,
        ),
        format!("{:.0}", dynamic_percent(total_dynamic, total_dynamic)),
        format_fixed(group_leakage(
            summary.leakage_watts,
            analysis.leakage_power_unit_watts(),
        )),
        String::new(),
    ]);
    report.section("Cells");
    report.table(
        ReportTable::new(
            [
                "Cell",
                "Internal",
                "Switching",
                "Dynamic",
                "%",
                "Leakage",
                "Attrs",
            ],
            rows,
        )
        .expect("cell power rows match the static report schema"),
    );
    Ok(())
}

fn append_nets(
    model: &TimingModel,
    analysis: &PowerAnalysis,
    options: &ReportPowerOptions,
    report: &mut ReportDocument,
) -> Result<(), PowerError> {
    let mut nets = analysis
        .nets(model)?
        .filter(|net| !net.input_port || options.include_input_nets)
        .collect::<Vec<_>>();
    nets.sort_by(|left, right| {
        right
            .switching_watts
            .total_cmp(&left.switching_watts)
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut total_watts = 0.0;
    let mut rows = nets
        .iter()
        .map(|net| {
            let toggle_per_ns = net.activity.toggle_rate() * 1e-9 / analysis.time_unit_seconds();
            let capacitance_pf = net.capacitance * analysis.capacitance_unit_farads() / 1e-12;
            let attribute = match net.origin {
                ActivityOrigin::Annotated => "a",
                ActivityOrigin::Propagated | ActivityOrigin::Default => "d",
            };
            total_watts += net.switching_watts;
            vec![
                net.name.clone().into_owned(),
                format!("{capacitance_pf:.3}"),
                format!("{:.3}", net.activity.static_probability()),
                format!("{toggle_per_ns:.4}"),
                format_fixed(group_dynamic(
                    net.switching_watts,
                    analysis.dynamic_power_unit_watts(),
                )),
                attribute.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    rows.push(vec![
        format!("Total ({} nets)", nets.len()),
        String::new(),
        String::new(),
        String::new(),
        format_fixed(group_dynamic(
            total_watts,
            analysis.dynamic_power_unit_watts(),
        )),
        String::new(),
    ]);
    report.section("Nets");
    report.table(
        ReportTable::new(
            [
                "Net",
                "Load (pF)",
                "Static probability",
                "Toggle rate",
                "Switching",
                "Attrs",
            ],
            rows,
        )
        .expect("net power rows match the static report schema"),
    );
    Ok(())
}

fn format_power_value(value: f64, unit_watts: f64, scientific: bool) -> String {
    let value = if scientific {
        format_scientific_or_zero(value)
    } else {
        format_fixed(value)
    };
    format!("{value} {}", format_engineering_symbol(unit_watts, "W"))
}

fn group_dynamic(watts: f64, unit_watts: f64) -> f64 {
    watts / unit_watts
}

fn group_leakage(watts: f64, unit_watts: f64) -> f64 {
    watts / unit_watts
}

fn format_scientific_or_zero(value: f64) -> String {
    if value == 0.0 {
        "0.0000".to_string()
    } else {
        format_scientific(value, 4)
    }
}

fn format_scientific(value: f64, precision: usize) -> String {
    let raw = format!("{value:.precision$e}");
    let Some((mantissa, exponent)) = raw.split_once('e') else {
        return raw;
    };
    let exponent = exponent.parse::<i32>().unwrap_or(0);
    format!("{mantissa}e{exponent:+03}")
}

fn format_fixed(value: f64) -> String {
    format!("{value:.4}")
}

fn dynamic_percent(value: f64, total: f64) -> f64 {
    if total == 0.0 {
        0.0
    } else {
        value / total * 100.0
    }
}

fn display_power(watts: f64, zero: (f64, &'static str)) -> (f64, &'static str) {
    if watts == 0.0 {
        (0.0, zero.1)
    } else if watts.abs() >= 1e-3 {
        (watts * 1e3, "mW")
    } else if watts.abs() >= 1e-6 {
        (watts * 1e6, "uW")
    } else if watts.abs() >= 1e-9 {
        (watts * 1e9, "nW")
    } else if watts.abs() >= 1e-12 {
        (watts * 1e12, "pW")
    } else {
        (watts / zero.0, zero.1)
    }
}

fn format_engineering_unit(value: f64, suffix: &str) -> String {
    let prefixes = [
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "u"),
        (1e-9, "n"),
        (1e-12, "p"),
        (1e-15, "f"),
    ];
    prefixes
        .into_iter()
        .find_map(|(scale, prefix)| {
            let factor = value / scale;
            ((factor - factor.round()).abs() < 1e-9 && (1.0..1000.0).contains(&factor))
                .then(|| format!("{factor:.0}{prefix}{suffix}"))
        })
        .unwrap_or_else(|| format!("{value:.6e}{suffix}"))
}

fn format_engineering_symbol(value: f64, suffix: &str) -> String {
    let prefixes = [
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "u"),
        (1e-9, "n"),
        (1e-12, "p"),
        (1e-15, "f"),
    ];
    prefixes
        .into_iter()
        .find_map(|(scale, prefix)| {
            ((value / scale - 1.0).abs() < 1e-9).then(|| format!("{prefix}{suffix}"))
        })
        .unwrap_or_else(|| format_engineering_unit(value, suffix))
}

fn format_decimal(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.4}")
    }
}
