// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use comfy_table::Table;
use comfy_table::presets::ASCII_MARKDOWN;

/// A presentation-neutral report composed from semantic blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportDocument {
    title: String,
    blocks: Vec<ReportBlock>,
}

impl ReportDocument {
    /// Create an empty document whose title becomes the leading level-one heading.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            blocks: Vec::new(),
        }
    }

    /// Return the title without its plain-text heading marker.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the blocks in presentation order.
    #[must_use]
    pub fn blocks(&self) -> &[ReportBlock] {
        &self.blocks
    }

    /// Append a level-two section heading.
    pub fn section(&mut self, title: impl Into<String>) {
        self.blocks.push(ReportBlock::Section(title.into()));
    }

    /// Append one scalar-field block when the iterator is nonempty.
    pub fn fields(&mut self, fields: impl IntoIterator<Item = ReportField>) {
        let fields = fields.into_iter().collect::<Vec<_>>();
        if !fields.is_empty() {
            self.blocks.push(ReportBlock::Fields(fields));
        }
    }

    /// Append a rectangular table.
    pub fn table(&mut self, table: ReportTable) {
        self.blocks.push(ReportBlock::Table(table));
    }

    /// Append a classified user-facing message.
    pub fn message(&mut self, kind: MessageKind, text: impl Into<String>) {
        self.blocks.push(ReportBlock::Message {
            kind,
            text: text.into(),
        });
    }

    /// Render the canonical plain-text representation used by Tcl and files.
    pub fn render_plain(&self) -> String {
        let mut rendered = vec![format!("# {}", self.title)];
        rendered.extend(self.blocks.iter().map(ReportBlock::render_plain));
        rendered.join("\n\n")
    }

    /// Parse the canonical plain-text report representation.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let mut lines = input.lines().peekable();
        let title = lines.next()?.strip_prefix("# ")?.trim();
        if title.is_empty() {
            return None;
        }
        let mut document = Self::new(title);
        while lines.peek().is_some() {
            while lines.peek().is_some_and(|line| line.trim().is_empty()) {
                lines.next();
            }
            let Some(line) = lines.next() else {
                break;
            };
            if let Some(section) = line.strip_prefix("## ") {
                document.section(section.trim());
                continue;
            }
            if is_table_separator(lines.peek().copied().unwrap_or_default()) && is_table_row(line) {
                let headers = split_table_row(line)?;
                lines.next();
                let mut rows = Vec::new();
                while lines
                    .peek()
                    .is_some_and(|candidate| is_table_row(candidate))
                {
                    rows.push(split_table_row(lines.next()?)?);
                }
                document.table(ReportTable::new(headers, rows)?);
                continue;
            }
            if let Some((kind, text)) = parse_message(line) {
                document.message(kind, text);
                continue;
            }
            if let Some(field) = parse_field(line) {
                let mut fields = vec![field];
                while let Some(field) = lines.peek().and_then(|line| parse_field(line)) {
                    fields.push(field);
                    lines.next();
                }
                document.fields(fields);
                continue;
            }
            return None;
        }
        Some(document)
    }
}

/// One semantic block in a report document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportBlock {
    /// A level-two heading separating related report content.
    Section(String),
    /// Consecutive labeled scalar values.
    Fields(Vec<ReportField>),
    /// A table whose row widths match its header width.
    Table(ReportTable),
    /// A diagnostic or status message.
    Message {
        /// Severity used to select the canonical textual prefix.
        kind: MessageKind,
        /// Message body without the severity prefix.
        text: String,
    },
}

impl ReportBlock {
    fn render_plain(&self) -> String {
        match self {
            Self::Section(title) => format!("## {title}"),
            Self::Fields(fields) => fields
                .iter()
                .map(|field| format!("{}: {}", field.label, field.value))
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Table(table) => table.render_plain(),
            Self::Message { kind, text } => format!("{}: {text}", kind.label()),
        }
    }
}

/// A labeled scalar value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportField {
    label: String,
    value: String,
}

impl ReportField {
    /// Create a field, formatting its value with [`ToString`].
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the value-oriented API accepts owned and borrowed display scalars uniformly"
    )]
    pub fn new(label: impl Into<String>, value: impl ToString) -> Self {
        Self {
            label: label.into(),
            value: value.to_string(),
        }
    }

    /// Return the label without the canonical `: ` separator.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the already-formatted scalar value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// A rectangular table with one header row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl ReportTable {
    /// Create a table, rejecting rows whose width differs from the header.
    pub fn new(
        headers: impl IntoIterator<Item = impl Into<String>>,
        rows: impl IntoIterator<Item = impl IntoIterator<Item = impl Into<String>>>,
    ) -> Option<Self> {
        let headers = headers.into_iter().map(Into::into).collect::<Vec<_>>();
        if headers.is_empty() {
            return None;
        }
        let rows = rows
            .into_iter()
            .map(|row| row.into_iter().map(Into::into).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        if rows.iter().any(|row| row.len() != headers.len()) {
            return None;
        }
        Some(Self { headers, rows })
    }

    /// Return column labels in display order.
    #[must_use]
    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    /// Return data rows in display order.
    ///
    /// Every row has exactly [`Self::headers`]`.len()` cells.
    #[must_use]
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    fn render_plain(&self) -> String {
        let mut table = Table::new();
        table
            .load_style(ASCII_MARKDOWN)
            .set_header(self.headers.iter().map(|value| escape_cell(value)));
        for row in &self.rows {
            table.add_row(row.iter().map(|value| escape_cell(value)));
        }
        table.trim_fmt()
    }
}

/// User-facing message classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Neutral context that does not require corrective action.
    Information,
    /// A condition that may make the result incomplete or surprising.
    Warning,
    /// Confirmation that a checked condition holds.
    Success,
    /// A condition that prevents the requested result from being valid.
    Error,
}

impl MessageKind {
    fn label(self) -> &'static str {
        match self {
            Self::Information => "Information",
            Self::Warning => "Warning",
            Self::Success => "Success",
            Self::Error => "Error",
        }
    }
}

fn parse_message(line: &str) -> Option<(MessageKind, &str)> {
    [
        (MessageKind::Information, "Information: "),
        (MessageKind::Warning, "Warning: "),
        (MessageKind::Success, "Success: "),
        (MessageKind::Error, "Error: "),
    ]
    .into_iter()
    .find_map(|(kind, prefix)| line.strip_prefix(prefix).map(|text| (kind, text)))
}

fn parse_field(line: &str) -> Option<ReportField> {
    let (label, value) = line.split_once(": ")?;
    (!label.is_empty() && !value.is_empty()).then(|| ReportField::new(label, value))
}

fn is_table_row(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('|') && line.ends_with('|')
}

fn is_table_separator(line: &str) -> bool {
    let Some(columns) = split_table_row(line) else {
        return false;
    };
    !columns.is_empty()
        && columns.iter().all(|column| {
            let column = column.trim_matches(':');
            column.len() >= 3 && column.chars().all(|character| character == '-')
        })
}

fn escape_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '|' => escaped.push_str("\\|"),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn split_table_row(line: &str) -> Option<Vec<String>> {
    if !is_table_row(line) {
        return None;
    }
    let mut columns = Vec::new();
    let mut column = String::new();
    let mut escaped = false;
    for character in line.trim()[1..line.trim().len() - 1].chars() {
        if escaped {
            match character {
                'n' => column.push('\n'),
                other => column.push(other),
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '|' {
            columns.push(column.trim().to_string());
            column.clear();
        } else {
            column.push(character);
        }
    }
    if escaped {
        column.push('\\');
    }
    columns.push(column.trim().to_string());
    Some(columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_report_round_trips_escaped_tables() {
        let mut report = ReportDocument::new("Resource report");
        report.fields([ReportField::new("Design", "top")]);
        report.table(
            ReportTable::new(["Name", "Source"], [[r"alu|0", "rtl\\top.v\nline 4"]]).unwrap(),
        );
        report.message(MessageKind::Information, "Complete");

        let plain = report.render_plain();
        assert_eq!(ReportDocument::parse(&plain), Some(report));
    }

    #[test]
    fn rejects_text_outside_the_canonical_grammar() {
        assert!(ReportDocument::parse("# Report\n\nunclassified text").is_none());
    }
}
