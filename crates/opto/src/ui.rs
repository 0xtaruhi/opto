// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::ShellError;
use crate::command::EvalResult;
use crate::command_catalog::{CommandRegistry, ValueHint};
use crate::runtime::Runtime;
use crate::tcl::command_complete as tcl_command_complete;
use directories::ProjectDirs;
use nu_ansi_term::{Color as AnsiColor, Style};
use opto_session::ObjectClass;
use reedline::{
    ColumnarMenu, Completer, DefaultHinter, Emacs, FileBackedHistory, Highlighter, KeyCode,
    KeyModifiers, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, Span, StyledText, Suggestion, ValidationResult, Validator,
    default_emacs_keybindings,
};
use std::borrow::Cow;
use std::collections::{BTreeSet, BinaryHeap};
use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

mod lexical;

use lexical::{LexicalKind, lexical_parts};

const HISTORY_CAPACITY: usize = 10_000;
const COMPLETION_LIMIT: usize = 100;

/// Policy controlling ANSI color emission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// Enable color only for a capable terminal and when `NO_COLOR` is absent.
    #[default]
    Auto,
    /// Always emit color escape sequences.
    Always,
    /// Never emit color escape sequences.
    Never,
}

/// Color palette selected for interactive and diagnostic output.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Theme {
    /// Palette designed for dark terminal backgrounds.
    #[default]
    Dark,
    /// Palette designed for light terminal backgrounds.
    Light,
}

/// Presentation settings that do not affect Tcl or synthesis semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiOptions {
    /// ANSI color policy.
    pub color: ColorMode,
    /// Selected terminal palette.
    pub theme: Theme,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Palette {
    pub(crate) primary: (u8, u8, u8),
    pub(crate) accent: (u8, u8, u8),
    pub(crate) text: (u8, u8, u8),
    pub(crate) muted: (u8, u8, u8),
    pub(crate) info: (u8, u8, u8),
    pub(crate) success: (u8, u8, u8),
    pub(crate) warning: (u8, u8, u8),
    pub(crate) error: (u8, u8, u8),
}

impl Theme {
    pub(crate) const fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette {
                primary: (0xd9, 0x77, 0x57),
                accent: (0x7a, 0x9a, 0xa8),
                text: (0xd6, 0xd3, 0xd1),
                muted: (0x8a, 0x86, 0x80),
                info: (0x82, 0x9b, 0xa6),
                success: (0x82, 0x9b, 0x72),
                warning: (0xc4, 0x9a, 0x55),
                error: (0xc9, 0x68, 0x68),
            },
            Self::Light => Palette {
                primary: (0xb8, 0x5c, 0x3b),
                accent: (0x55, 0x70, 0x7c),
                text: (0x29, 0x25, 0x24),
                muted: (0x78, 0x71, 0x6c),
                info: (0x4f, 0x6f, 0x7e),
                success: (0x55, 0x78, 0x48),
                warning: (0x9b, 0x68, 0x28),
                error: (0xaa, 0x45, 0x45),
            },
        }
    }
}

impl Palette {
    fn reedline(color: (u8, u8, u8)) -> Style {
        Style::new().fg(AnsiColor::Rgb(color.0, color.1, color.2))
    }

    pub(crate) fn terminal(color: (u8, u8, u8)) -> anstyle::Style {
        anstyle::Style::new().fg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(
            color.0, color.1, color.2,
        ))))
    }
}

pub(crate) fn command_complete(command: &str) -> Result<bool, ShellError> {
    let command = CString::new(command).map_err(|source| ShellError::Nul {
        context: "Tcl command",
        source,
    })?;
    Ok(tcl_command_complete(&command))
}

pub(crate) fn run_repl(runtime: &mut Runtime, options: UiOptions) -> Result<i32, ShellError> {
    let colors = colors_enabled(options.color);
    let palette = options.theme.palette();
    print_banner(palette, colors);

    let shared = Arc::new(RwLock::new(CompletionData::default()));
    let prompt_design = Arc::new(RwLock::new(None));
    let history = Box::new(FileBackedHistory::with_file(
        HISTORY_CAPACITY,
        history_path()?,
    )?);
    let completer = Box::new(OptoCompleter {
        data: Arc::clone(&shared),
        commands: runtime.state.commands.clone(),
        palette,
    });
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    let menu = ColumnarMenu::default().with_name("completion_menu");
    let hinter = DefaultHinter::default().with_style(Palette::reedline(palette.muted).italic());
    let mut editor = Reedline::create()
        .with_history(history)
        .with_completer(completer)
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_highlighter(Box::new(TclHighlighter { palette }))
        .with_hinter(Box::new(hinter))
        .with_validator(Box::new(TclValidator))
        .with_ansi_colors(colors);

    loop {
        refresh_completion_data(runtime, &shared)?;
        *prompt_design
            .write()
            .expect("prompt design lock must not be poisoned") = runtime
            .state
            .session
            .borrow()
            .current_design()
            .map(str::to_owned);
        let prompt = OptoPrompt {
            design: Arc::clone(&prompt_design),
            palette,
        };
        let signal = editor
            .read_line(&prompt)
            .map_err(|source| ShellError::Output {
                action: "failed to read interactive input",
                source,
            })?;
        match signal {
            Signal::Success(line) => match runtime.eval(&line) {
                Ok(EvalResult::Complete(result)) if !result.is_empty() => {
                    print_result(&result, options, true);
                }
                Ok(EvalResult::Complete(_)) => {}
                Ok(EvalResult::Exit(code)) => return Ok(code),
                Err(err) => crate::diagnostic::print_error(&err, options),
            },
            Signal::CtrlD => {
                println!();
                return Ok(0);
            }
            _ => {}
        }
    }
}

fn colors_enabled(mode: ColorMode) -> bool {
    resolve_color_mode(
        mode,
        std::env::var_os("NO_COLOR").is_some(),
        std::env::var_os("TERM").is_some_and(|term| term == "dumb"),
        io::stdout().is_terminal(),
    )
}

fn resolve_color_mode(
    mode: ColorMode,
    no_color: bool,
    dumb_terminal: bool,
    is_terminal: bool,
) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => !no_color && !dumb_terminal && is_terminal,
    }
}

fn print_banner(palette: Palette, colors: bool) {
    if colors {
        let brand = Palette::terminal(palette.primary).bold();
        let muted = Palette::terminal(palette.muted);
        anstream::println!(
            "{brand}opto{brand:#} {muted}{} · deterministic synthesis shell{muted:#}",
            env!("CARGO_PKG_VERSION")
        );
        anstream::println!("{muted}Tcl 8.6 · Tab complete · Ctrl-R history{muted:#}\n");
    } else {
        println!(
            "opto {} · deterministic synthesis shell",
            env!("CARGO_PKG_VERSION")
        );
        println!("Tcl 8.6 · Tab complete · Ctrl-R history\n");
    }
}

pub(crate) fn print_progress(text: &str, options: UiOptions, interactive: bool) {
    if !interactive || !colors_enabled(options.color) {
        print!("{text}");
        return;
    }
    let palette = options.theme.palette();
    for line in text.split_inclusive('\n') {
        let (display, color, bold) =
            if line.contains("Optimization complete") || line.contains("unchanged") {
                (
                    line.trim_end_matches('\n').to_string(),
                    palette.success,
                    true,
                )
            } else if line.contains("Warning") {
                (
                    line.trim_end_matches('\n').to_string(),
                    palette.warning,
                    true,
                )
            } else if line.contains("Information:") {
                (line.trim_end_matches('\n').to_string(), palette.info, false)
            } else if line.contains("Beginning ") {
                (
                    line.trim_end_matches('\n').to_string(),
                    palette.primary,
                    true,
                )
            } else if line.contains("Processing")
                || line.contains("Structuring")
                || line.contains("Mapping")
            {
                (
                    line.trim_end_matches('\n').to_string(),
                    palette.accent,
                    false,
                )
            } else {
                (line.trim_end_matches('\n').to_string(), palette.text, false)
            };
        let mut style = Palette::terminal(color);
        if bold {
            style = style.bold();
        }
        let newline = if line.ends_with('\n') { "\n" } else { "" };
        anstream::print!("{style}{display}{style:#}{newline}");
    }
}

pub(crate) fn print_result(text: &str, options: UiOptions, interactive: bool) {
    if interactive && crate::presentation::is_report(text) {
        let rendered = crate::presentation::render_report(
            text,
            options.theme.palette(),
            colors_enabled(options.color),
            terminal_width(),
        );
        anstream::println!("{rendered}");
    } else {
        println!("{text}");
    }
}

fn terminal_width() -> Option<u16> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
}

fn history_path() -> Result<PathBuf, ShellError> {
    let project = ProjectDirs::from("io.github", "0xtaruhi", "opto")
        .ok_or_else(|| ShellError::command("cannot determine the interactive history directory"))?;
    let directory = project
        .state_dir()
        .unwrap_or_else(|| project.data_local_dir());
    std::fs::create_dir_all(directory).map_err(|source| ShellError::FileIo {
        operation: "cannot create interactive history directory",
        path: directory.to_path_buf(),
        source,
    })?;
    let path = directory.join("history");
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| ShellError::FileIo {
            operation: "cannot open interactive history",
            path: path.clone(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| ShellError::FileIo {
                operation: "cannot secure interactive history",
                path: path.clone(),
                source,
            },
        )?;
    }
    Ok(path)
}

struct OptoPrompt {
    design: Arc<RwLock<Option<String>>>,
    palette: Palette,
}

impl Prompt for OptoPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        match self
            .design
            .read()
            .expect("prompt design lock must not be poisoned")
            .as_deref()
        {
            Some(design) => Cow::Owned(format!("opto [{design}]")),
            None => Cow::Borrowed("opto"),
        }
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(" ❯ ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("··· ❯ ")
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Owned(format!("(reverse-search: {}) ", history_search.term))
    }

    fn get_prompt_color(&self) -> reedline::Color {
        let (r, g, b) = self.palette.primary;
        reedline::Color::Rgb { r, g, b }
    }

    fn get_prompt_multiline_color(&self) -> AnsiColor {
        let (r, g, b) = self.palette.muted;
        AnsiColor::Rgb(r, g, b)
    }

    fn get_indicator_color(&self) -> reedline::Color {
        let (r, g, b) = self.palette.accent;
        reedline::Color::Rgb { r, g, b }
    }
}

struct TclValidator;

impl Validator for TclValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        match command_complete(line) {
            Ok(true) | Err(_) => ValidationResult::Complete,
            Ok(false) => ValidationResult::Incomplete,
        }
    }
}

struct TclHighlighter {
    palette: Palette,
}

impl Highlighter for TclHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        let mut command_position = true;
        for part in lexical_parts(line) {
            let text = &line[part.range.clone()];
            let style = match part.kind {
                LexicalKind::Whitespace | LexicalKind::Punctuation => {
                    if matches!(text, ";" | "[" | "\n") {
                        command_position = true;
                    }
                    Palette::reedline(self.palette.text)
                }
                LexicalKind::Comment => Palette::reedline(self.palette.muted).italic(),
                LexicalKind::Variable => Palette::reedline(self.palette.accent),
                LexicalKind::String => Palette::reedline(self.palette.info),
                LexicalKind::Word if command_position => {
                    command_position = false;
                    Palette::reedline(self.palette.primary).bold()
                }
                LexicalKind::Word if text.starts_with('-') => {
                    Palette::reedline(self.palette.accent)
                }
                LexicalKind::Word => Palette::reedline(self.palette.text),
            };
            styled.push((style, text.to_string()));
        }
        styled
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CandidateKind {
    Command,
    Design,
    Library,
    Port,
    Cell,
    Pin,
    Net,
    Clock,
}

#[derive(Debug, Clone, Copy)]
struct ArenaEntry {
    start: u32,
    len: u32,
    kind: CandidateKind,
}

#[derive(Default)]
struct CompletionData {
    arena: String,
    entries: Vec<ArenaEntry>,
    object_key: Option<(u64, Option<String>)>,
}

impl CompletionData {
    fn push(&mut self, kind: CandidateKind, text: &str) {
        push_entry(&mut self.arena, &mut self.entries, kind, text);
    }

    fn clear_kind(&mut self, kind: CandidateKind) {
        if self.entries.iter().any(|entry| entry.kind == kind) {
            self.retain_without(&[kind]);
        }
    }

    fn retain_without(&mut self, kinds: &[CandidateKind]) {
        let mut arena = String::new();
        let mut entries = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if kinds.contains(&entry.kind) {
                continue;
            }
            let text = self.text(*entry).to_owned();
            push_entry(&mut arena, &mut entries, entry.kind, &text);
        }
        self.arena = arena;
        self.entries = entries;
    }

    fn text(&self, entry: ArenaEntry) -> &str {
        let start = entry.start as usize;
        &self.arena[start..start + entry.len as usize]
    }

    fn matches(&self, kind: CandidateKind, prefix: &str) -> Vec<String> {
        let mut result = BTreeSet::new();
        for entry in &self.entries {
            if entry.kind == kind {
                let text = self.text(*entry);
                if text.starts_with(prefix) {
                    result.insert(text.to_string());
                    if result.len() > COMPLETION_LIMIT {
                        result.pop_last();
                    }
                }
            }
        }
        result.into_iter().collect()
    }
}

fn push_entry(arena: &mut String, entries: &mut Vec<ArenaEntry>, kind: CandidateKind, text: &str) {
    let Ok(start) = u32::try_from(arena.len()) else {
        return;
    };
    let Ok(len) = u32::try_from(text.len()) else {
        return;
    };
    arena.push_str(text);
    entries.push(ArenaEntry { start, len, kind });
}

fn refresh_completion_data(
    runtime: &mut Runtime,
    shared: &Arc<RwLock<CompletionData>>,
) -> Result<(), ShellError> {
    let commands = match runtime.eval("info commands")? {
        EvalResult::Complete(commands) => commands,
        EvalResult::Exit(_) => {
            return Err(ShellError::command("info commands unexpectedly exited"));
        }
    };
    let session = runtime.state.session.borrow();
    let key = (
        session.revision().get().get(),
        session.current_design().map(str::to_owned),
    );
    let mut data = shared
        .write()
        .expect("completion data lock must not be poisoned");
    data.clear_kind(CandidateKind::Command);
    for command in commands.split_whitespace() {
        data.push(CandidateKind::Command, command);
    }
    if data.object_key.as_ref() == Some(&key) {
        return Ok(());
    }
    data.retain_without(&[
        CandidateKind::Design,
        CandidateKind::Library,
        CandidateKind::Port,
        CandidateKind::Cell,
        CandidateKind::Pin,
        CandidateKind::Net,
        CandidateKind::Clock,
    ]);
    for (class, kind) in [
        (ObjectClass::Design, CandidateKind::Design),
        (ObjectClass::Port, CandidateKind::Port),
        (ObjectClass::Cell, CandidateKind::Cell),
        (ObjectClass::Pin, CandidateKind::Pin),
        (ObjectClass::Net, CandidateKind::Net),
        (ObjectClass::Clock, CandidateKind::Clock),
    ] {
        session.visit_object_names(class, |name| {
            data.push(kind, name);
        });
    }
    for name in session.library_names() {
        data.push(CandidateKind::Library, &name);
    }
    data.object_key = Some(key);
    Ok(())
}

struct OptoCompleter {
    data: Arc<RwLock<CompletionData>>,
    commands: CommandRegistry,
    palette: Palette,
}

impl Completer for OptoCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let context = cursor_context(line, pos);
        let data = self
            .data
            .read()
            .expect("completion data lock must not be poisoned");
        if context.at_command {
            return suggestions(
                data.matches(CandidateKind::Command, &context.prefix),
                context.span,
                "command",
                Palette::reedline(self.palette.primary),
                true,
            );
        }
        let Some(command) = self.commands.find(&context.command) else {
            return Vec::new();
        };
        let spec = command.syntax();
        if context.prefix.starts_with('-') {
            let values = spec
                .options
                .iter()
                .map(|option| option.name)
                .filter(|option| option.starts_with(&context.prefix))
                .map(str::to_owned)
                .take(COMPLETION_LIMIT)
                .collect();
            return suggestions(
                values,
                context.span,
                "option",
                Palette::reedline(self.palette.accent),
                true,
            );
        }
        let hint = context
            .previous
            .as_deref()
            .and_then(|previous| spec.options.iter().find(|option| option.name == previous))
            .and_then(|option| option.value)
            .or_else(|| {
                spec.positional_at(completed_positional_count(
                    &context.completed_arguments,
                    spec,
                ))
                .map(|positional| positional.value)
            });
        let Some(hint) = hint else {
            return Vec::new();
        };
        if matches!(hint, ValueHint::File | ValueHint::Directory) {
            return path_suggestions(&context, hint, self.palette);
        }
        if let ValueHint::OneOf {
            suggested: values, ..
        }
        | ValueHint::Suggested(values) = hint
        {
            return suggestions(
                values
                    .iter()
                    .copied()
                    .filter(|value| value.starts_with(&context.prefix))
                    .map(str::to_owned)
                    .collect(),
                context.span,
                "value",
                Palette::reedline(self.palette.info),
                true,
            );
        }
        let kind = match hint {
            ValueHint::Design => CandidateKind::Design,
            ValueHint::Port => CandidateKind::Port,
            ValueHint::Cell => CandidateKind::Cell,
            ValueHint::Pin => CandidateKind::Pin,
            ValueHint::Net => CandidateKind::Net,
            ValueHint::Clock => CandidateKind::Clock,
            ValueHint::Text
            | ValueHint::File
            | ValueHint::Directory
            | ValueHint::OneOf { .. }
            | ValueHint::Suggested(_) => {
                return Vec::new();
            }
        };
        if matches!(
            kind,
            CandidateKind::Port | CandidateKind::Cell | CandidateKind::Pin | CandidateKind::Net
        ) && context.prefix.chars().count() < 2
        {
            return Vec::new();
        }
        suggestions(
            data.matches(kind, &context.prefix),
            context.span,
            candidate_description(kind),
            Palette::reedline(self.palette.info),
            true,
        )
    }
}

fn candidate_description(kind: CandidateKind) -> &'static str {
    match kind {
        CandidateKind::Command => "command",
        CandidateKind::Design => "design",
        CandidateKind::Library => "library",
        CandidateKind::Port => "port",
        CandidateKind::Cell => "cell",
        CandidateKind::Pin => "pin",
        CandidateKind::Net => "net",
        CandidateKind::Clock => "clock",
    }
}

fn suggestions(
    values: Vec<String>,
    span: Span,
    description: &'static str,
    style: Style,
    append_whitespace: bool,
) -> Vec<Suggestion> {
    values
        .into_iter()
        .take(COMPLETION_LIMIT)
        .map(|value| Suggestion {
            value,
            description: Some(description.to_string()),
            style: Some(style),
            span,
            append_whitespace,
            ..Suggestion::default()
        })
        .collect()
}

#[derive(Debug)]
struct CursorContext {
    command: String,
    at_command: bool,
    previous: Option<String>,
    completed_arguments: Vec<String>,
    prefix: String,
    span: Span,
    grouped: bool,
}

fn cursor_context(line: &str, pos: usize) -> CursorContext {
    let pos = pos.min(line.len());
    let segment_start = command_segment_start(&line[..pos]);
    let segment = &line[segment_start..pos];
    let tokens = token_ranges(segment);
    let ends_with_separator = segment
        .chars()
        .next_back()
        .is_none_or(|ch| ch.is_whitespace() || matches!(ch, ';' | '['));
    let current = if ends_with_separator {
        None
    } else {
        tokens.last().cloned()
    };
    let prefix = current
        .as_ref()
        .map_or("", |token| token_text(segment, token.clone()));
    let grouped = prefix.starts_with('{') || prefix.starts_with('"');
    let clean_prefix = prefix
        .strip_prefix(['{', '"'])
        .unwrap_or(prefix)
        .trim_end_matches(['}', '"']);
    let span_start = current.as_ref().map_or(pos, |range| {
        segment_start + range.start + usize::from(grouped)
    });
    let command = tokens
        .first()
        .map(|range| clean_token(token_text(segment, range.clone())))
        .unwrap_or_default();
    let previous_index = if current.is_some() {
        tokens.len().checked_sub(2)
    } else {
        tokens.len().checked_sub(1)
    };
    let previous =
        previous_index.map(|index| clean_token(token_text(segment, tokens[index].clone())));
    let completed_end = if current.is_some() {
        tokens.len().saturating_sub(1)
    } else {
        tokens.len()
    };
    let completed_arguments = tokens
        .get(1..completed_end)
        .unwrap_or_default()
        .iter()
        .map(|range| clean_token(token_text(segment, range.clone())))
        .collect();
    let at_command = tokens.is_empty() || (tokens.len() == 1 && current.is_some());
    CursorContext {
        command,
        at_command,
        previous,
        completed_arguments,
        prefix: clean_prefix.to_string(),
        span: Span::new(span_start, pos),
        grouped,
    }
}

fn completed_positional_count(
    arguments: &[String],
    syntax: &crate::command_catalog::CommandSyntax,
) -> usize {
    let mut count = 0usize;
    let mut index = 0usize;
    let mut options_terminated = false;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        if !options_terminated && argument == "--" {
            options_terminated = true;
            index += 1;
            continue;
        }
        if !options_terminated
            && let Some(option) = syntax
                .options
                .iter()
                .chain(&syntax.unsupported_options)
                .find(|option| option.name == argument)
        {
            index += 1 + usize::from(option.value.is_some());
            continue;
        }
        count += 1;
        index += 1;
    }
    count
}

fn command_segment_start(line: &str) -> usize {
    let mut start = 0;
    let mut brace_depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if brace_depth == 0 && ch == '"' {
            quoted = !quoted;
        } else if !quoted {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                ';' | '\n' | '[' if brace_depth == 0 => start = index + ch.len_utf8(),
                _ => {}
            }
        }
    }
    start
}

fn token_ranges(segment: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut brace_depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in segment.char_indices() {
        if start.is_none() && !ch.is_whitespace() {
            start = Some(index);
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
        } else if brace_depth == 0 && ch == '"' {
            quoted = !quoted;
        } else if !quoted {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth = brace_depth.saturating_sub(1),
                _ => {}
            }
        }
        if ch.is_whitespace()
            && !quoted
            && brace_depth == 0
            && let Some(token_start) = start.take()
        {
            ranges.push(token_start..index);
        }
    }
    if let Some(token_start) = start {
        ranges.push(token_start..segment.len());
    }
    ranges
}

fn token_text(segment: &str, range: Range<usize>) -> &str {
    &segment[range]
}

fn clean_token(token: &str) -> String {
    token
        .strip_prefix(['{', '"'])
        .unwrap_or(token)
        .trim_end_matches(['}', '"'])
        .to_string()
}

#[derive(Debug)]
struct PathCompletionQuery<'a> {
    explicit_directory: Option<&'a Path>,
    name_prefix: &'a str,
}

impl<'a> PathCompletionQuery<'a> {
    fn parse(prefix: &'a str) -> Self {
        let path = Path::new(prefix);
        if prefix
            .chars()
            .next_back()
            .is_some_and(std::path::is_separator)
        {
            return Self {
                explicit_directory: Some(path),
                name_prefix: "",
            };
        }

        let name_prefix = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        match path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            Some(parent) => Self {
                explicit_directory: Some(parent),
                name_prefix,
            },
            None => Self {
                explicit_directory: None,
                name_prefix,
            },
        }
    }

    fn directory(&self) -> &Path {
        self.explicit_directory.unwrap_or_else(|| Path::new("."))
    }

    fn candidate(&self, name: &str) -> PathBuf {
        self.explicit_directory
            .map_or_else(|| PathBuf::from(name), |directory| directory.join(name))
    }
}

fn path_suggestions(context: &CursorContext, hint: ValueHint, palette: Palette) -> Vec<Suggestion> {
    let query = PathCompletionQuery::parse(&context.prefix);
    let Ok(read_dir) = std::fs::read_dir(query.directory()) else {
        return Vec::new();
    };
    let mut values = BinaryHeap::new();
    for entry in read_dir.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(query.name_prefix) {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if hint == ValueHint::Directory && !is_dir {
            continue;
        }
        let value = query.candidate(&name).to_string_lossy().into_owned();
        let value = if is_dir {
            format!("{value}{}", std::path::MAIN_SEPARATOR)
        } else if context.grouped || !value.contains(char::is_whitespace) {
            value
        } else {
            format!("{{{value}}}")
        };
        values.push(value);
        if values.len() > COMPLETION_LIMIT {
            values.pop();
        }
    }
    let values = values.into_sorted_vec();
    suggestions(
        values,
        context.span,
        "path",
        Palette::reedline(palette.info),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_command_and_option_value_context() {
        let context = cursor_context("set x [report_timing -from cl", 36);
        assert_eq!(context.command, "report_timing");
        assert_eq!(context.previous.as_deref(), Some("-from"));
        assert_eq!(context.completed_arguments, ["-from"]);
        assert_eq!(context.prefix, "cl");
    }

    #[test]
    fn recognizes_partial_first_word_as_command_context() {
        let context = cursor_context("rep", 3);
        assert!(context.at_command);
        assert_eq!(context.prefix, "rep");
    }

    #[test]
    fn tcl_validator_detects_multiline_commands() {
        assert!(!command_complete("if {1} {").unwrap());
        assert!(command_complete("if {1} {puts ok}").unwrap());
    }

    #[test]
    fn completion_arena_limits_and_sorts_matches() {
        let mut data = CompletionData::default();
        for index in (0..100_100).rev() {
            data.push(CandidateKind::Port, &format!("p{index:03}"));
        }
        let matches = data.matches(CandidateKind::Port, "p");
        assert_eq!(matches.len(), COMPLETION_LIMIT);
        assert_eq!(matches.first().map(String::as_str), Some("p000"));
    }

    #[test]
    fn path_completion_descends_into_a_prefix_ending_in_a_separator() {
        let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
        let prefix = format!("{}{}", docs.display(), std::path::MAIN_SEPARATOR);
        let context = CursorContext {
            command: "read_hdl".to_string(),
            at_command: false,
            previous: Some("read_hdl".to_string()),
            completed_arguments: Vec::new(),
            prefix: prefix.clone(),
            span: Span::new(0, prefix.len()),
            grouped: true,
        };

        let values = path_suggestions(&context, ValueHint::File, Theme::Dark.palette())
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect::<Vec<_>>();

        assert!(values.contains(&format!("{prefix}architecture.md")));
        assert!(!values.contains(&prefix));
    }

    #[test]
    fn positional_completion_uses_the_current_positional_hint() {
        let mut commands = CommandRegistry::new();
        commands
            .register(crate::commands::SET_CLOCK_TRANSITION)
            .unwrap();
        let syntax = commands.find("set_clock_transition").unwrap().syntax();
        let context = cursor_context("set_clock_transition 0.10 sys", 31);

        assert_eq!(context.completed_arguments, ["0.10"]);
        assert_eq!(
            syntax
                .positional_at(completed_positional_count(
                    &context.completed_arguments,
                    syntax,
                ))
                .map(|hint| hint.value),
            Some(ValueHint::Clock)
        );
    }

    #[test]
    fn path_completion_query_retains_an_explicit_current_directory() {
        let prefix = format!(".{}", std::path::MAIN_SEPARATOR);
        let query = PathCompletionQuery::parse(&prefix);

        assert_eq!(query.explicit_directory, Some(Path::new(".")));
        assert_eq!(query.name_prefix, "");
        assert_eq!(query.candidate("top.sv"), Path::new(".").join("top.sv"));
    }

    #[test]
    fn color_mode_precedence_is_explicit_and_deterministic() {
        assert!(resolve_color_mode(ColorMode::Always, true, true, false));
        assert!(!resolve_color_mode(ColorMode::Never, false, false, true));
        assert!(!resolve_color_mode(ColorMode::Auto, true, false, true));
        assert!(!resolve_color_mode(ColorMode::Auto, false, true, true));
        assert!(resolve_color_mode(ColorMode::Auto, false, false, true));
    }
}
