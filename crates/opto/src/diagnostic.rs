// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::error::ErrorSource;
use crate::ui::Palette;
use crate::{ShellError, UiOptions};
use annotate_snippets::{AnnotationKind, Group, Level, Renderer, Snippet, renderer::DecorStyle};
use opto_core::{Diagnostic, DiagnosticLabel, DiagnosticSeverity, DiagnosticSource};
use std::io::{self, IsTerminal};
use std::path::Path;

/// Renders a shell error to standard error using the selected UI policy.
///
/// Errors carrying source text receive a line-numbered primary annotation;
/// other errors use a compact title-only diagnostic.
pub fn print_error(error: &ShellError, options: UiOptions) {
    let rendered = render_error(error, options);
    anstream::eprintln!("{rendered}");
}

/// Renders one successful-operation diagnostic to standard error.
pub(crate) fn print_diagnostic(diagnostic: &Diagnostic, options: UiOptions) {
    let rendered = render_structured_diagnostic(diagnostic, None, options);
    anstream::eprintln!("{rendered}");
}

fn render_error(error: &ShellError, options: UiOptions) -> String {
    if let Some(diagnostic) = error.diagnostic() {
        return render_structured_diagnostic(&diagnostic, error.invocation_context(), options);
    }
    let message = error.to_string();
    if let Some(source) = error.source_context() {
        render_source_error(
            &message,
            "error occurs here",
            source,
            error.invocation_context(),
            options,
        )
    } else if let Some(invocation) = error.invocation_context() {
        render_title_with_invocation(&message, invocation, options)
    } else {
        render_title(&message, options)
    }
}

pub(crate) fn print_source_error(message: &str, source: &ErrorSource, options: UiOptions) {
    let rendered = render_source_error(message, "error occurs here", source, None, options);
    anstream::eprintln!("{rendered}");
}

fn render_source_error(
    message: &str,
    label: &str,
    source: &ErrorSource,
    invocation: Option<&ErrorSource>,
    options: UiOptions,
) -> String {
    let range = source_range(source);
    let snippet = Snippet::source(source.text.as_str())
        .path(source.name.as_str())
        .annotation(AnnotationKind::Primary.span(range).label(label));
    let mut report = vec![Level::ERROR.primary_title(message).element(snippet)];
    if let Some(invocation) = invocation {
        report.push(invocation_group(invocation));
    }
    renderer(options).render(&report)
}

fn render_title(message: &str, options: UiOptions) -> String {
    let report = &[Group::with_title(Level::ERROR.primary_title(message))];
    renderer(options).render(report)
}

fn render_title_with_invocation(
    message: &str,
    invocation: &ErrorSource,
    options: UiOptions,
) -> String {
    let report = &[
        Group::with_title(Level::ERROR.primary_title(message)),
        invocation_group(invocation),
    ];
    renderer(options).render(report)
}

struct LoadedDiagnosticSource {
    path: String,
    text: String,
    labels: Vec<LoadedDiagnosticLabel>,
}

struct LoadedDiagnosticLabel {
    line: usize,
    column: Option<usize>,
    length: usize,
    message: String,
    primary: bool,
}

fn render_structured_diagnostic(
    diagnostic: &Diagnostic,
    invocation: Option<&ErrorSource>,
    options: UiOptions,
) -> String {
    let mut sources = Vec::<LoadedDiagnosticSource>::new();
    let mut unavailable = Vec::new();
    if let Some(primary) = diagnostic.primary() {
        load_diagnostic_label(&mut sources, &mut unavailable, primary, true);
    }
    for related in diagnostic.related() {
        load_diagnostic_label(&mut sources, &mut unavailable, related, false);
    }

    let snippets = sources
        .iter()
        .map(|source| {
            let mut snippet = Snippet::source(source.text.as_str()).path(source.path.as_str());
            for label in &source.labels {
                let range =
                    source_range_at(source.text.as_str(), label.line, label.column, label.length);
                let kind = if label.primary {
                    AnnotationKind::Primary
                } else {
                    AnnotationKind::Context
                };
                snippet = snippet.annotation(kind.span(range).label(label.message.as_str()));
            }
            snippet
        })
        .collect::<Vec<_>>();
    let title = match diagnostic.severity() {
        DiagnosticSeverity::Warning => Level::WARNING.primary_title(diagnostic.title()),
        DiagnosticSeverity::Error => Level::ERROR.primary_title(diagnostic.title()),
    };
    let mut report = vec![title.id(diagnostic.code()).elements(snippets)];
    for note in diagnostic.notes() {
        report.push(Group::with_title(Level::NOTE.secondary_title(note)));
    }
    for location in unavailable {
        report.push(Group::with_title(Level::NOTE.secondary_title(format!(
            "source location unavailable: {location}"
        ))));
    }
    for help in diagnostic.help() {
        report.push(Group::with_title(Level::HELP.secondary_title(help)));
    }
    if let Some(invocation) = invocation {
        report.push(invocation_group(invocation));
    }
    renderer(options).render(&report)
}

fn load_diagnostic_label(
    sources: &mut Vec<LoadedDiagnosticSource>,
    unavailable: &mut Vec<String>,
    label: &DiagnosticLabel,
    primary: bool,
) {
    let location = label.location();
    let path = location.path();
    if let Some(source) = sources.iter_mut().find(|source| source.path == path) {
        source.labels.push(LoadedDiagnosticLabel {
            line: location.line() as usize,
            column: location.column().map(|column| column as usize),
            length: location.length() as usize,
            message: label.message().to_string(),
            primary,
        });
        return;
    }
    let Ok(text) = std::fs::read_to_string(Path::new(path)) else {
        let location = format!("{path}:{}", location.line());
        if !unavailable.contains(&location) {
            unavailable.push(location);
        }
        return;
    };
    sources.push(LoadedDiagnosticSource {
        path: path.to_string(),
        text,
        labels: vec![LoadedDiagnosticLabel {
            line: location.line() as usize,
            column: location.column().map(|column| column as usize),
            length: location.length() as usize,
            message: label.message().to_string(),
            primary,
        }],
    });
}

fn invocation_group(source: &ErrorSource) -> Group<'_> {
    Group::with_title(Level::NOTE.secondary_title("command invocation")).element(
        Snippet::source(source.text.as_str())
            .path(source.name.as_str())
            .annotation(
                AnnotationKind::Primary
                    .span(source_range(source))
                    .label("this command triggered the diagnostic"),
            ),
    )
}

fn renderer(options: UiOptions) -> Renderer {
    let colors = match options.color {
        crate::ColorMode::Always => true,
        crate::ColorMode::Never => false,
        crate::ColorMode::Auto => {
            std::env::var_os("NO_COLOR").is_none()
                && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
                && io::stderr().is_terminal()
        }
    };
    if colors {
        let palette = options.theme.palette();
        Renderer::styled()
            .decor_style(DecorStyle::Unicode)
            .error(Palette::terminal(palette.error).bold())
            .warning(Palette::terminal(palette.warning).bold())
            .info(Palette::terminal(palette.info).bold())
            .help(Palette::terminal(palette.success))
            .line_num(Palette::terminal(palette.muted))
            .emphasis(Palette::terminal(palette.text).bold())
    } else {
        Renderer::plain().decor_style(DecorStyle::Ascii)
    }
}

fn source_range(source: &ErrorSource) -> std::ops::Range<usize> {
    source_range_at(
        source.text.as_str(),
        source.line,
        source.column,
        source.length,
    )
}

fn source_range_at(
    text: &str,
    line: usize,
    column: Option<usize>,
    length: usize,
) -> std::ops::Range<usize> {
    let line = line.max(1);
    let line_start = text
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>()
        .min(text.len());
    let line_end = text[line_start..]
        .find('\n')
        .map_or(text.len(), |offset| line_start + offset);
    let line_text = &text[line_start..line_end];
    if column.is_none() {
        let leading = line_text.len() - line_text.trim_start().len();
        let trailing = line_text.trim_end().len();
        let start = line_start + leading;
        let end = line_start + trailing.max(leading);
        return start..end.max((start + 1).min(text.len()));
    }
    let column = column.unwrap_or(1).max(1);
    let start_in_line = line_text
        .char_indices()
        .nth(column - 1)
        .map_or(line_text.len(), |(offset, _)| offset);
    let start = line_start + start_in_line;
    let length = length.max(1);
    let end_in_line = line_text[start_in_line..]
        .char_indices()
        .nth(length)
        .map_or(line_text.len(), |(offset, _)| start_in_line + offset);
    let end = (line_start + end_in_line).max(start).min(line_end);
    start..end.max((start + 1).min(text.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_range_handles_utf8_columns() {
        let source = ErrorSource {
            name: "test.tcl".to_string(),
            text: "puts 好\nunknown\n".to_string(),
            line: 1,
            column: Some(6),
            length: 1,
        };
        assert_eq!(&source.text[source_range(&source)], "好");
    }

    #[test]
    fn structured_diagnostic_renders_hdl_before_command_invocation() {
        let path = std::env::temp_dir().join(format!(
            "opto-diagnostic-{}-structured.sv",
            std::process::id()
        ));
        std::fs::write(
            &path,
            "module top(input logic a, output logic y);\n  assign y = y & a;\nendmodule\n",
        )
        .unwrap();
        let diagnostic = Diagnostic::new("OPT-SYN-001", "combinational loop detected")
            .with_primary(DiagnosticLabel::new(
                opto_core::DiagnosticLocation::new(path.to_string_lossy(), 2, Some(14)),
                "feedback path enters signal 'y'",
            ))
            .with_help("break the feedback path with sequential state");
        let invocation = ErrorSource {
            name: "run.tcl".to_string(),
            text: "elaborate top\nsynth\n".to_string(),
            line: 2,
            column: None,
            length: 1,
        };

        let rendered = render_structured_diagnostic(
            &diagnostic,
            Some(&invocation),
            UiOptions {
                color: crate::ColorMode::Never,
                ..UiOptions::default()
            },
        );
        std::fs::remove_file(path).unwrap();

        assert!(rendered.contains("error[OPT-SYN-001]: combinational loop detected"));
        assert!(rendered.contains("assign y = y & a"));
        assert!(rendered.contains("feedback path enters signal 'y'"));
        assert!(rendered.contains("help: break the feedback path"));
        assert!(rendered.contains("note: command invocation"));
        assert!(rendered.contains("synth"));
        assert!(
            rendered.find("assign y = y & a").unwrap()
                < rendered.find("note: command invocation").unwrap()
        );
    }

    #[test]
    fn typed_error_without_hdl_location_treats_synthesis_as_secondary() {
        let error = ShellError::Session(opto_session::SessionError::State(
            "technology mapping failed".to_string(),
        ))
        .with_source("run.tcl", "elaborate top\nsynth\n", 2);

        let rendered = render_error(
            &error,
            UiOptions {
                color: crate::ColorMode::Never,
                ..UiOptions::default()
            },
        );

        assert!(rendered.contains("error[OPT-SES-002]: technology mapping failed"));
        assert!(rendered.contains("note: command invocation"));
        assert!(rendered.contains("this command triggered the diagnostic"));
        assert!(!rendered.contains("error occurs here"));
    }
}
