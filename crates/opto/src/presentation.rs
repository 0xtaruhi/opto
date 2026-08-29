// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::ui::Palette;
use comfy_table::presets::{NOTHING, UTF8_HORIZONTAL_ONLY};
use comfy_table::{
    Attribute, Cell, CellAlignment, Color, ColumnConstraint, ContentArrangement, Row, Table, Width,
};
use comfy_table::{ContentLineStyle, LineStyle, TableStyle};
use opto_formats::{MessageKind, ReportBlock, ReportDocument, ReportField, ReportTable};
use std::fmt::Write;

const MIN_REPORT_WIDTH: u16 = 48;
const ASCII_PROGRESS_TABLE_START: TableStyle = TableStyle::new()
    .top_border(LineStyle::none().fill('-').junction('-'))
    .header_separator(LineStyle::none().fill('-').junction('-'));
const ASCII_PROGRESS_TABLE_ROW: TableStyle =
    TableStyle::new().content_lines(ContentLineStyle::none().junction(' '));

pub(crate) fn is_report(text: &str) -> bool {
    ReportDocument::parse(text).is_some()
}

pub(crate) fn render_report(
    text: &str,
    palette: Palette,
    colors: bool,
    width: Option<u16>,
) -> String {
    let Some(document) = ReportDocument::parse(text) else {
        return text.to_string();
    };
    render_document(
        &document,
        palette,
        colors,
        width.map(|width| width.max(MIN_REPORT_WIDTH)),
        text.ends_with('\n'),
    )
}

fn render_document(
    document: &ReportDocument,
    palette: Palette,
    colors: bool,
    width: Option<u16>,
    trailing_newline: bool,
) -> String {
    let mut blocks = Vec::with_capacity(document.blocks().len() + 1);
    let mut title = String::new();
    push_styled(&mut title, document.title(), palette.primary, true, colors);
    blocks.push(title);
    for block in document.blocks() {
        blocks.push(match block {
            ReportBlock::Section(section) => {
                let mut rendered = String::new();
                push_styled(&mut rendered, section, palette.text, true, colors);
                rendered
            }
            ReportBlock::Fields(fields) => render_fields(fields, palette, colors, width),
            ReportBlock::Table(table) => render_table(table, palette, colors, width),
            ReportBlock::Message { kind, text } => {
                let (color, bold) = match kind {
                    MessageKind::Information => (palette.info, false),
                    MessageKind::Warning => (palette.warning, true),
                    MessageKind::Success => (palette.success, true),
                    MessageKind::Error => (palette.error, true),
                };
                let mut rendered = String::new();
                push_styled(&mut rendered, text, color, bold, colors);
                rendered
            }
        });
    }
    let mut output = blocks.join("\n\n");
    if trailing_newline {
        output.push('\n');
    }
    output
}

fn render_table(
    report: &ReportTable,
    palette: Palette,
    colors: bool,
    width: Option<u16>,
) -> String {
    let mut table = Table::new();
    table
        .load_style(UTF8_HORIZONTAL_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic);
    if let Some(width) = width {
        table.set_width(width);
    }
    if colors {
        table.enforce_styling();
    }
    table.set_header(
        report
            .headers()
            .iter()
            .map(|header| styled_cell(header, palette.text, true, colors)),
    );
    for row in report.rows() {
        table.add_row(Row::from(
            row.iter()
                .map(|value| {
                    let alignment = if is_numeric_cell(value) {
                        CellAlignment::Right
                    } else {
                        CellAlignment::Left
                    };
                    styled_cell(value, palette.text, false, colors).set_alignment(alignment)
                })
                .collect::<Vec<_>>(),
        ));
    }
    for column_index in 0..report.headers().len() {
        if let Some(column) = table.column_mut(column_index) {
            column.set_padding((0, 1));
        }
    }
    preserve_numeric_columns(&mut table, report);
    table.trim_fmt()
}

pub(crate) fn render_live_table(
    headers: Option<&[&str]>,
    row: &[String],
    column_widths: &[u16],
) -> String {
    let mut table = Table::new();
    table
        .load_style(if headers.is_some() {
            ASCII_PROGRESS_TABLE_START
        } else {
            ASCII_PROGRESS_TABLE_ROW
        })
        .set_content_arrangement(ContentArrangement::Disabled);
    if let Some(headers) = headers {
        table.set_header(headers.iter().map(Cell::new));
    }
    table.add_row(Row::from(
        row.iter()
            .map(|value| {
                Cell::new(value).set_alignment(if is_numeric_cell(value) {
                    CellAlignment::Right
                } else {
                    CellAlignment::Left
                })
            })
            .collect::<Vec<_>>(),
    ));
    for (column_index, width) in column_widths.iter().copied().enumerate() {
        if let Some(column) = table.column_mut(column_index) {
            column
                .set_constraint(ColumnConstraint::Absolute(Width::Fixed(width)))
                .set_padding((0, 1));
        }
    }
    table.trim_fmt()
}

fn preserve_numeric_columns(table: &mut Table, report: &ReportTable) {
    for column_index in 1..report.headers().len() {
        let values = report
            .rows()
            .iter()
            .filter_map(|row| row.get(column_index))
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if values.is_empty() || !values.iter().all(|value| is_numeric_cell(value)) {
            continue;
        }
        if let Some(column) = table.column_mut(column_index) {
            column.set_constraint(ColumnConstraint::ContentWidth);
        }
    }
}

fn render_fields(
    fields: &[ReportField],
    palette: Palette,
    colors: bool,
    width: Option<u16>,
) -> String {
    let mut table = Table::new();
    table
        .load_style(NOTHING)
        .set_content_arrangement(ContentArrangement::Dynamic);
    if let Some(width) = width {
        table.set_width(width);
    }
    if colors {
        table.enforce_styling();
    }
    for field in fields {
        let alignment = if is_numeric_cell(field.value()) {
            CellAlignment::Right
        } else {
            CellAlignment::Left
        };
        table.add_row(vec![
            styled_cell(field.label(), palette.muted, false, colors),
            styled_cell(field.value(), palette.text, false, colors).set_alignment(alignment),
        ]);
    }
    table.trim_fmt()
}

fn styled_cell(text: impl ToString, color: (u8, u8, u8), bold: bool, colors: bool) -> Cell {
    let mut cell = Cell::new(text);
    if colors {
        cell = cell.fg(Color::Rgb {
            r: color.0,
            g: color.1,
            b: color.2,
        });
        if bold {
            cell = cell.add_attribute(Attribute::Bold);
        }
    }
    cell
}

fn push_styled(output: &mut String, text: &str, color: (u8, u8, u8), bold: bool, colors: bool) {
    if colors {
        let mut style = Palette::terminal(color);
        if bold {
            style = style.bold();
        }
        let _ = write!(output, "{style}{text}{style:#}");
    } else {
        output.push_str(text);
    }
}

fn is_numeric_cell(value: &str) -> bool {
    let first = value
        .trim()
        .trim_start_matches(['(', '<', '>'])
        .chars()
        .next();
    first.is_some_and(|character| character.is_ascii_digit() || matches!(character, '+' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Theme;

    fn render(input: &str, width: u16) -> String {
        render_report(input, Theme::Dark.palette(), false, Some(width))
    }

    #[test]
    fn renders_canonical_fields_and_sections() {
        let input = "# Area report\n\nDesign: top\nVersion: opto 0.1\n\n## Counts\n\nCells: 12";
        let output = render(input, 60);
        assert!(output.starts_with("Area report"), "{output}");
        assert!(output.contains("Design"), "{output}");
        assert!(output.contains("Counts"), "{output}");
        assert!(output.contains("12"), "{output}");
    }

    #[test]
    fn renders_canonical_tables() {
        let input = "# Resources report\n\n| Resource | Module | Width |\n|----------|--------|-------|\n| r1       | DW01_add | 4 |";
        let output = render(input, 80);
        assert!(output.contains("Resource"), "{output}");
        assert!(output.contains("DW01_add"), "{output}");
        assert!(output.contains('─'), "{output}");
        assert!(!output.contains('|'), "{output}");
    }

    #[test]
    fn live_table_rows_keep_library_managed_column_alignment() {
        let headers = ["Step", "Elapsed", "Area"];
        let widths = [13, 9, 9];
        let first = render_live_table(
            Some(&headers),
            &["Mapping".into(), "00:00:01".into(), "100.0".into()],
            &widths,
        );
        let next = render_live_table(
            None,
            &["Sizing".into(), "00:00:02".into(), "90.0".into()],
            &widths,
        );
        let first_row = first.lines().find(|line| line.contains("Mapping")).unwrap();
        let next_row = next.lines().find(|line| line.contains("Sizing")).unwrap();
        assert_eq!(first_row.find("00:00:01"), next_row.find("00:00:02"));
        assert_eq!(first_row.find(".0"), next_row.find(".0"));
    }

    #[test]
    fn narrow_tables_wrap_long_names() {
        let input = "# Resources report\n\n| Resource | Module |\n|----------|--------|\n| top/u_very_long_instance_name/arith/another_level/deep_datapath | DW01_add |";
        let output = render(input, 48);
        assert!(output.lines().count() > 5, "{output}");
        assert!(output.contains("u_very_long"), "{output}");
    }

    #[test]
    fn messages_and_color_are_preserved() {
        let input = "# Timing report\n\nWarning: Path is unconstrained\n\n设计名称: 顶层";
        let plain = render(input, 60);
        assert!(plain.contains("Path is unconstrained"));
        assert!(plain.contains("设计名称"));
        let styled = render_report(input, Theme::Dark.palette(), true, Some(60));
        assert!(styled.contains("\u{1b}["));
        assert!(styled.contains("顶层"));
    }

    #[test]
    fn non_reports_are_left_unchanged() {
        assert_eq!(
            render_report("ordinary output", Theme::Dark.palette(), false, None),
            "ordinary output"
        );
    }
}
