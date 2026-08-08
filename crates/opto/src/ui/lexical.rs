// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(Debug, Clone)]
pub(super) struct LexicalPart {
    pub(super) range: Range<usize>,
    pub(super) kind: LexicalKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum LexicalKind {
    Whitespace,
    Punctuation,
    Comment,
    Variable,
    String,
    Word,
}

pub(super) fn lexical_parts(line: &str) -> Vec<LexicalPart> {
    let mut parts = Vec::new();
    let mut index = 0;
    let bytes = line.as_bytes();
    let mut command_position = true;
    while index < bytes.len() {
        let start = index;
        let ch = line[index..]
            .chars()
            .next()
            .expect("index is a character boundary");
        if ch.is_whitespace() {
            index += ch.len_utf8();
            while index < bytes.len() {
                let next = line[index..]
                    .chars()
                    .next()
                    .expect("index is a character boundary");
                if !next.is_whitespace() {
                    break;
                }
                if next == '\n' {
                    command_position = true;
                }
                index += next.len_utf8();
            }
            parts.push(LexicalPart {
                range: start..index,
                kind: LexicalKind::Whitespace,
            });
        } else if ch == '#' && command_position {
            index = line[start..]
                .find('\n')
                .map_or(line.len(), |offset| start + offset);
            parts.push(LexicalPart {
                range: start..index,
                kind: LexicalKind::Comment,
            });
        } else if matches!(ch, ';' | '[' | ']') {
            index += ch.len_utf8();
            command_position = matches!(ch, ';' | '[');
            parts.push(LexicalPart {
                range: start..index,
                kind: LexicalKind::Punctuation,
            });
        } else if matches!(ch, '"' | '{') {
            let close = if ch == '"' { '"' } else { '}' };
            index += ch.len_utf8();
            let mut escaped = false;
            while index < bytes.len() {
                let next = line[index..]
                    .chars()
                    .next()
                    .expect("index is a character boundary");
                index += next.len_utf8();
                if next == close && !escaped {
                    break;
                }
                escaped = next == '\\' && !escaped;
            }
            command_position = false;
            parts.push(LexicalPart {
                range: start..index,
                kind: LexicalKind::String,
            });
        } else {
            index += ch.len_utf8();
            while index < bytes.len() {
                let next = line[index..]
                    .chars()
                    .next()
                    .expect("index is a character boundary");
                if next.is_whitespace() || matches!(next, ';' | '[' | ']' | '"' | '{') {
                    break;
                }
                index += next.len_utf8();
            }
            let kind = if ch == '$' {
                LexicalKind::Variable
            } else {
                LexicalKind::Word
            };
            command_position = false;
            parts.push(LexicalPart {
                range: start..index,
                kind,
            });
        }
    }
    parts
}
