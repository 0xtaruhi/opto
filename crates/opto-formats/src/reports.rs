// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    AreaCellKind, AreaReportContext, MessageKind, ReportDocument, ReportField, ReportTable,
};
use opto_ir::word;
use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

/// Render area for a Word IR design containing instantiated cells.
///
/// Operations not yet represented as instances do not contribute area.
#[must_use]
pub fn report_area(module: &word::WordModule, context: &AreaReportContext) -> ReportDocument {
    let summary = area_summary(module, context);
    format_area_report(
        module.name(),
        module
            .ports()
            .iter()
            .map(|port| port.ty.width() as usize)
            .sum::<usize>(),
        report_net_count(module),
        module.instances().len(),
        &summary,
        context,
    )
}

#[derive(Default)]
struct AreaSummary {
    combinational_cells: usize,
    sequential_cells: usize,
    macro_cells: usize,
    buffer_inverter_cells: usize,
    references: BTreeSet<String>,
    combinational_area: f64,
    buffer_inverter_area: f64,
    noncombinational_area: f64,
    macro_area: f64,
    total_area: f64,
    /// References that contributed no area because the bound libraries do not
    /// characterize them. Reported explicitly so a total is never silently
    /// understated by an uncharacterized cell.
    uncharacterized: BTreeSet<String>,
}

fn format_area_report(
    design: &str,
    ports: usize,
    nets: usize,
    cells: usize,
    summary: &AreaSummary,
    context: &AreaReportContext,
) -> ReportDocument {
    let mut report = ReportDocument::new("Area report");
    report.fields([
        ReportField::new("Design", design),
        ReportField::new("Version", format!("opto {}", env!("CARGO_PKG_VERSION"))),
        ReportField::new("Date", report_timestamp()),
    ]);
    report.message(MessageKind::Information, "Updating design information...");
    if !context.libraries.is_empty() {
        report.section("Libraries");
        report.table(
            ReportTable::new(
                ["Library", "Source"],
                context
                    .libraries
                    .iter()
                    .map(|library| [library.name.clone(), library.source.clone()]),
            )
            .expect("area library rows match the static report schema"),
        );
    }
    report.section("Counts");
    report.fields([
        ReportField::new("Number of ports", ports),
        ReportField::new("Number of nets", nets),
        ReportField::new("Number of cells", cells),
        ReportField::new("Number of combinational cells", summary.combinational_cells),
        ReportField::new("Number of sequential cells", summary.sequential_cells),
        ReportField::new("Number of macros/black boxes", summary.macro_cells),
        ReportField::new("Number of buf/inv", summary.buffer_inverter_cells),
        ReportField::new("Number of references", summary.references.len()),
    ]);
    report.section("Area");
    report.fields([
        ReportField::new(
            "Combinational area",
            format!("{:.6}", summary.combinational_area),
        ),
        ReportField::new(
            "Buf/Inv area",
            format!("{:.6}", summary.buffer_inverter_area),
        ),
        ReportField::new(
            "Noncombinational area",
            format!("{:.6}", summary.noncombinational_area),
        ),
        ReportField::new("Macro/Black Box area", format!("{:.6}", summary.macro_area)),
        ReportField::new(
            "Net Interconnect area",
            "undefined (wire load has zero net area)",
        ),
        ReportField::new("Total cell area", format!("{:.6}", summary.total_area)),
        ReportField::new("Total area", "undefined"),
    ]);
    if !summary.uncharacterized.is_empty() {
        report.message(
            MessageKind::Warning,
            format!(
                "{} reference(s) contribute no area because the bound libraries do not \
                 characterize them: {}",
                summary.uncharacterized.len(),
                summary
                    .uncharacterized
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        );
    }
    report
}

/// Return the current UTC timestamp in the report-header format.
#[must_use]
pub fn report_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let second_of_day = seconds % 86_400;
    let hour = second_of_day / 3_600;
    let minute = second_of_day % 3_600 / 60;
    let second = second_of_day % 60;
    let (year, month, day) = civil_date_from_unix_days(days);
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        [usize::try_from((days + 4).rem_euclid(7)).unwrap_or(0)];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][usize::try_from(month.saturating_sub(1)).unwrap_or(0)];
    format!("{weekday} {month} {day:02} {hour:02}:{minute:02}:{second:02} {year} UTC")
}

fn civil_date_from_unix_days(days: i64) -> (i64, i64, i64) {
    // Convert a Unix day number through 400-year Gregorian eras. This is the
    // proleptic-Gregorian civil-date algorithm; keeping it integer-only avoids
    // locale and timezone dependencies in deterministic report formatting.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn area_summary(module: &word::WordModule, context: &AreaReportContext) -> AreaSummary {
    let mut summary = AreaSummary::default();
    for instance in module.instances() {
        let reference = module.name_str(instance.module);
        let area = context.library_cell_area.get(reference).copied();
        if area.is_none() {
            summary.uncharacterized.insert(reference.to_string());
        }
        let area = area.unwrap_or(0.0);
        summary.references.insert(reference.to_string());
        summary.total_area += area;
        match context
            .library_cell_kind
            .get(reference)
            .copied()
            .unwrap_or(AreaCellKind::Macro)
        {
            AreaCellKind::Combinational => {
                summary.combinational_cells += 1;
                summary.combinational_area += area;
            }
            AreaCellKind::BufferInverter => {
                summary.combinational_cells += 1;
                summary.buffer_inverter_cells += 1;
                summary.combinational_area += area;
                summary.buffer_inverter_area += area;
            }
            AreaCellKind::Sequential => {
                summary.sequential_cells += 1;
                summary.noncombinational_area += area;
            }
            AreaCellKind::Macro => {
                summary.macro_cells += 1;
                summary.macro_area += area;
            }
        }
    }
    summary
}

/// Render area from a sealed mapped netlist and target-library metadata.
#[must_use]
pub fn report_mapped_area(
    netlist: &opto_ir::mapped::MappedNetlist,
    context: &AreaReportContext,
) -> ReportDocument {
    let summary = mapped_area_summary(netlist, context);
    format_area_report(
        netlist.name(),
        mapped_port_count(netlist),
        netlist.net_count(),
        netlist.cell_count() + netlist.design_instance_count(),
        &summary,
        context,
    )
}

/// Render the compact mapped-design `QoR` summary.
///
/// `timing_paths` is supplied by timing analysis; this formatter does not
/// trigger analysis implicitly.
#[must_use]
pub fn report_mapped_qor(
    netlist: &opto_ir::mapped::MappedNetlist,
    context: &AreaReportContext,
    timing: Option<&opto_timing::TimingQuality>,
) -> ReportDocument {
    let area = mapped_area_summary(netlist, context);
    format_qor_report(
        netlist.name(),
        area.combinational_cells,
        area.sequential_cells,
        timing,
    )
}

/// Render area accumulated across hierarchy definitions and occurrence counts.
///
/// `modules` contains each unique mapped definition paired with its elaborated
/// occurrence count. Counts and area are scaled without flattening netlists.
#[must_use]
pub fn report_hierarchical_mapped_area(
    root: &opto_ir::mapped::MappedNetlist,
    modules: &[(&opto_ir::mapped::MappedNetlist, u64)],
    context: &AreaReportContext,
) -> ReportDocument {
    let mut summary = AreaSummary::default();
    let mut ports = 0usize;
    let mut nets = 0usize;
    let mut cells = 0usize;
    for &(netlist, occurrences) in modules {
        let occurrences_usize = usize::try_from(occurrences).unwrap_or(usize::MAX);
        ports = ports.saturating_add(mapped_port_count(netlist).saturating_mul(occurrences_usize));
        nets = nets.saturating_add(netlist.net_count().saturating_mul(occurrences_usize));
        cells = cells.saturating_add(
            (netlist.cell_count() + netlist.design_instance_count())
                .saturating_mul(occurrences_usize),
        );
        summary.add_scaled(&mapped_area_summary(netlist, context), occurrences);
    }
    format_area_report(root.name(), ports, nets, cells, &summary, context)
}

fn mapped_port_count(netlist: &opto_ir::mapped::MappedNetlist) -> usize {
    use opto_ir::mapped::PortId;

    netlist
        .ports()
        .iter()
        .enumerate()
        .map(|(index, _)| {
            PortId::from_index(index)
                .ok()
                .and_then(|port| netlist.port_nets(port))
                .map_or(0, <[opto_ir::mapped::NetId]>::len)
        })
        .sum()
}

/// Render `QoR` accumulated across hierarchy definitions and occurrence counts.
#[must_use]
pub fn report_hierarchical_mapped_qor(
    root: &opto_ir::mapped::MappedNetlist,
    modules: &[(&opto_ir::mapped::MappedNetlist, u64)],
    context: &AreaReportContext,
    timing: Option<&opto_timing::TimingQuality>,
) -> ReportDocument {
    let mut summary = AreaSummary::default();
    for &(netlist, occurrences) in modules {
        summary.add_scaled(&mapped_area_summary(netlist, context), occurrences);
    }
    format_qor_report(
        root.name(),
        summary.combinational_cells,
        summary.sequential_cells,
        timing,
    )
}

fn mapped_area_summary(
    netlist: &opto_ir::mapped::MappedNetlist,
    context: &AreaReportContext,
) -> AreaSummary {
    let mut summary = AreaSummary::default();
    for cell in netlist.cell_ids() {
        let Some(reference) = netlist.cell_type(cell) else {
            continue;
        };
        let area = context
            .library_cell_area
            .get(reference)
            .copied()
            .unwrap_or(0.0);
        let kind = context
            .library_cell_kind
            .get(reference)
            .copied()
            .unwrap_or(AreaCellKind::Macro);
        summary.references.insert(reference.to_string());
        summary.total_area += area;
        match kind {
            AreaCellKind::Combinational => {
                summary.combinational_cells += 1;
                summary.combinational_area += area;
            }
            AreaCellKind::BufferInverter => {
                summary.combinational_cells += 1;
                summary.buffer_inverter_cells += 1;
                summary.combinational_area += area;
                summary.buffer_inverter_area += area;
            }
            AreaCellKind::Sequential => {
                summary.sequential_cells += 1;
                summary.noncombinational_area += area;
            }
            AreaCellKind::Macro => {
                summary.macro_cells += 1;
                summary.macro_area += area;
            }
        }
    }
    for instance in netlist.design_instance_ids() {
        let Some(reference) = netlist.design_instance_module(instance) else {
            continue;
        };
        let area = context.library_cell_area.get(reference).copied();
        if area.is_none() {
            summary.uncharacterized.insert(reference.to_string());
        }
        let area = area.unwrap_or(0.0);
        summary.references.insert(reference.to_string());
        summary.macro_cells += 1;
        summary.macro_area += area;
        summary.total_area += area;
    }
    summary
}

impl AreaSummary {
    fn add_scaled(&mut self, other: &Self, occurrences: u64) {
        let count = usize::try_from(occurrences).unwrap_or(usize::MAX);
        self.combinational_cells = self
            .combinational_cells
            .saturating_add(other.combinational_cells.saturating_mul(count));
        self.sequential_cells = self
            .sequential_cells
            .saturating_add(other.sequential_cells.saturating_mul(count));
        self.macro_cells = self
            .macro_cells
            .saturating_add(other.macro_cells.saturating_mul(count));
        self.buffer_inverter_cells = self
            .buffer_inverter_cells
            .saturating_add(other.buffer_inverter_cells.saturating_mul(count));
        self.references.extend(other.references.iter().cloned());
        #[allow(
            clippy::cast_precision_loss,
            reason = "reported area is floating-point and occurrence counts scale that quantity"
        )]
        let scale = occurrences as f64;
        self.combinational_area += other.combinational_area * scale;
        self.buffer_inverter_area += other.buffer_inverter_area * scale;
        self.noncombinational_area += other.noncombinational_area * scale;
        self.macro_area += other.macro_area * scale;
        self.total_area += other.total_area * scale;
    }
}

/// Render `QoR` for a Word IR design, including reachable live operations that
/// have not yet become cell instances.
#[must_use]
pub fn report_qor(
    module: &word::WordModule,
    context: &AreaReportContext,
    timing: Option<&opto_timing::TimingQuality>,
) -> ReportDocument {
    let area = area_summary(module, context);
    let (live_combinational, live_sequential) = live_operation_counts(module);
    let combinational = area.combinational_cells + live_combinational;
    let sequential = area.sequential_cells + live_sequential;
    format_qor_report(module.name(), combinational, sequential, timing)
}

fn format_qor_report(
    design: &str,
    combinational: usize,
    sequential: usize,
    timing: Option<&opto_timing::TimingQuality>,
) -> ReportDocument {
    let mut report = ReportDocument::new("QoR report");
    report.fields([
        ReportField::new("Design", design),
        ReportField::new("Combinational cells", combinational),
        ReportField::new("Sequential cells", sequential),
        ReportField::new(
            "Timing paths",
            timing.map_or(0, opto_timing::TimingQuality::path_count),
        ),
    ]);
    if let Some(timing) = timing {
        report.section("Timing");
        report.fields([
            ReportField::new("Critical Path Length", format!("{:.6}", timing.arrival())),
            ReportField::new(
                "Critical Path Slack",
                timing.wns().map_or_else(
                    || "unconstrained".to_string(),
                    |slack| format!("{slack:.6}"),
                ),
            ),
            ReportField::new("Total Negative Slack", format!("{:.6}", timing.tns())),
            ReportField::new("No. of Violating Paths", timing.violating_paths()),
        ]);
    }
    report
}

fn live_operation_counts(module: &word::WordModule) -> (usize, usize) {
    // Walk backward only from observable connects and instance pins. Counting
    // every arena operation would include dead normalization residue and inflate
    // pre-map QoR relative to the published design.
    let mut visited = vec![false; module.operations().len()];
    let mut work = module
        .connects()
        .iter()
        .map(|connect| connect.value)
        .chain(
            module
                .instances()
                .iter()
                .flat_map(|instance| instance.connections.iter())
                .map(|connection| connection.value),
        )
        .collect::<Vec<_>>();
    let mut combinational = 0usize;
    let mut sequential = 0usize;

    while let Some(value_id) = work.pop() {
        let Some(value) = module.value(value_id) else {
            continue;
        };
        let word::ValueKind::Operation(operation_id) = value.kind else {
            continue;
        };
        let index = operation_id.index();
        let Some(was_visited) = visited.get_mut(index) else {
            continue;
        };
        if *was_visited {
            continue;
        }
        *was_visited = true;
        let Some(operation) = module.operation(operation_id) else {
            continue;
        };
        match &operation.kind {
            word::OpKind::Unary { arg, .. } => {
                combinational += 1;
                work.push(*arg);
            }
            word::OpKind::Binary { left, right, .. } => {
                combinational += 1;
                work.extend([*left, *right]);
            }
            word::OpKind::Mux {
                cond,
                then_value,
                else_value,
            } => {
                combinational += 1;
                work.extend([*cond, *then_value, *else_value]);
            }
            word::OpKind::TriState { data, enable } => {
                combinational += 1;
                work.extend([*data, enable.value]);
            }
            word::OpKind::Concat { parts } => {
                combinational += 1;
                work.extend(parts.iter().copied());
            }
            word::OpKind::Extract { value, .. } | word::OpKind::Cast { value, .. } => {
                combinational += 1;
                work.push(*value);
            }
            word::OpKind::DynamicExtract { value, offset, .. } => {
                combinational += 1;
                work.extend([*value, *offset]);
            }
            word::OpKind::DynamicInsert {
                value,
                offset,
                replacement,
            } => {
                combinational += 1;
                work.extend([*value, *offset, *replacement]);
            }
            word::OpKind::Register(register) => {
                sequential += 1;
                work.extend([register.d, register.clock]);
                if let Some(enable) = register.enable {
                    work.push(enable.value);
                }
                for reset in &register.resets {
                    work.extend([reset.value, reset.reset_value]);
                }
            }
            word::OpKind::Latch(latch) => {
                sequential += 1;
                work.extend([latch.d, latch.enable.value]);
                for reset in &latch.resets {
                    work.extend([reset.value, reset.reset_value]);
                }
            }
        }
    }
    (combinational, sequential)
}

fn report_net_count(module: &word::WordModule) -> usize {
    let mut signal_offsets = Vec::with_capacity(module.signals().len() + 1);
    signal_offsets.push(0usize);
    for signal in module.signals() {
        let next = signal_offsets
            .last()
            .copied()
            .expect("signal offset table has an initial entry")
            .checked_add(signal.ty.width() as usize)
            .expect("RTL signal bit count exceeds addressable memory");
        signal_offsets.push(next);
    }
    let total_bits = signal_offsets.last().copied().unwrap_or(0);
    let mut parent = (0..total_bits).collect::<Vec<_>>();
    for connect in module.connects() {
        let Some(value) = module.value(connect.value) else {
            continue;
        };
        let word::ValueKind::Signal(reference) = &value.kind else {
            continue;
        };
        let Some(target_signal) = module.signal(connect.target.signal) else {
            continue;
        };
        let target_lsb = connect
            .target
            .range
            .map_or(0, |range| range.msb.min(range.lsb));
        let target_width = connect
            .target
            .range
            .map_or(target_signal.ty.width(), word::BitRange::width);
        if target_width != reference.width() {
            continue;
        }
        for bit in 0..target_width {
            let target = signal_bit_index(&signal_offsets, connect.target.signal, target_lsb + bit);
            let source = signal_bit_index(&signal_offsets, reference.signal, reference.lsb + bit);
            let (Some(target), Some(source)) = (target, source) else {
                continue;
            };
            union_roots(&mut parent, target, source);
        }
    }

    let mut roots = BTreeSet::new();
    for index in 0..parent.len() {
        roots.insert(disjoint_set_root(&mut parent, index));
    }
    roots.len()
}

fn signal_bit_index(offsets: &[usize], signal: word::SignalId, bit: u32) -> Option<usize> {
    let signal = signal.index();
    let start = *offsets.get(signal)?;
    let end = *offsets.get(signal + 1)?;
    let index = start.checked_add(bit as usize)?;
    (index < end).then_some(index)
}

fn union_roots(parent: &mut [usize], left: usize, right: usize) {
    let left_root = disjoint_set_root(parent, left);
    let right_root = disjoint_set_root(parent, right);
    if left_root != right_root {
        parent[right_root] = left_root;
    }
}

fn disjoint_set_root(parent: &mut [usize], index: usize) -> usize {
    let mut root = index;
    while parent[root] != root {
        root = parent[root];
    }
    let mut current = index;
    while parent[current] != current {
        let next = parent[current];
        parent[current] = root;
        current = next;
    }
    root
}

#[cfg(test)]
mod tests {
    use super::{AreaReportContext, civil_date_from_unix_days, report_mapped_area};
    use opto_ir::{RevisionId, mapped::MappedBuilder};

    #[test]
    fn unix_day_conversion_handles_epoch_and_leap_days() {
        assert_eq!(civil_date_from_unix_days(0), (1970, 1, 1));
        assert_eq!(civil_date_from_unix_days(11_016), (2000, 2, 29));
    }

    #[test]
    fn retained_design_instances_are_reported_as_black_boxes() {
        let mut builder = MappedBuilder::new("top", RevisionId::INITIAL).unwrap();
        let net = builder.add_net(Some("data")).unwrap();
        builder
            .add_design_instance(
                "memory",
                "sram",
                &[(
                    "data".to_string(),
                    vec![opto_ir::mapped::ConnectionSignal::Net(net)],
                )],
            )
            .unwrap();

        let report = report_mapped_area(&builder.freeze().unwrap(), &AreaReportContext::default())
            .render_plain();
        assert!(report.contains("Number of cells: 1"));
        assert!(report.contains("Number of macros/black boxes: 1"));
        assert!(report.contains("sram"));
    }
}
