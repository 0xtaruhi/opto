// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use super::lexer::Token;
use logos::{Lexer, Logos as _};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::ops::Range;

#[derive(Debug, Clone, Copy)]
pub(super) struct SourceSlice<'a> {
    pub(super) text: &'a str,
    pub(super) offset: usize,
}

impl<'a> SourceSlice<'a> {
    pub(super) const fn new(text: &'a str) -> Self {
        Self { text, offset: 0 }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Value<'a> {
    text: &'a str,
    quoted: bool,
}

impl<'a> Value<'a> {
    pub(super) fn decoded(&self) -> Cow<'a, str> {
        if !self.quoted {
            return Cow::Borrowed(self.text);
        }
        let inner = &self.text[1..self.text.len() - 1];
        if !inner.as_bytes().contains(&b'\\') {
            return Cow::Borrowed(inner);
        }
        let mut decoded = String::with_capacity(inner.len());
        let mut bytes = inner.bytes();
        while let Some(byte) = bytes.next() {
            if byte != b'\\' {
                decoded.push(char::from(byte));
                continue;
            }
            match bytes.next() {
                Some(b'\n') => {}
                Some(b'\r') => {
                    if bytes.clone().next() == Some(b'\n') {
                        _ = bytes.next();
                    }
                }
                Some(escaped) => decoded.push(char::from(escaped)),
                None => decoded.push('\\'),
            }
        }
        Cow::Owned(decoded)
    }
}

pub(super) type Values<'a> = SmallVec<[Value<'a>; 2]>;

#[derive(Debug)]
pub(super) enum StatementKind<'a> {
    Simple(Values<'a>),
    Complex(Values<'a>),
    Group {
        arguments: Values<'a>,
        body: SourceSlice<'a>,
    },
    Assignment,
}

#[derive(Debug)]
pub(super) struct Statement<'a> {
    pub(super) name: &'a str,
    pub(super) offset: usize,
    pub(super) kind: StatementKind<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SyntaxErrorKind {
    InvalidToken,
    UnexpectedEnd {
        expected: &'static str,
    },
    UnexpectedToken {
        expected: &'static str,
        found: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SyntaxError {
    pub(super) offset: usize,
    pub(super) kind: SyntaxErrorKind,
}

#[derive(Debug, Clone)]
struct Lexeme<'a> {
    token: Token,
    text: &'a str,
    span: Range<usize>,
}

pub(super) struct Cursor<'a> {
    source: SourceSlice<'a>,
    lexer: Lexer<'a, Token>,
    peeked: Option<Result<Lexeme<'a>, SyntaxError>>,
}

impl<'a> Cursor<'a> {
    pub(super) fn new(source: SourceSlice<'a>) -> Self {
        Self {
            source,
            lexer: Token::lexer(source.text),
            peeked: None,
        }
    }

    pub(super) fn next_statement(&mut self) -> Result<Option<Statement<'a>>, SyntaxError> {
        let name = loop {
            let Some(lexeme) = self.next()? else {
                return Ok(None);
            };
            if lexeme.token == Token::Semicolon {
                continue;
            }
            if lexeme.token == Token::RightBrace {
                return Err(self.unexpected(&lexeme, "attribute or group name"));
            }
            if lexeme.token != Token::Bare {
                return Err(self.unexpected(&lexeme, "attribute or group name"));
            }
            break lexeme;
        };
        let separator = self.required("':', '=', or '('")?;
        let kind = match separator.token {
            Token::Colon => StatementKind::Simple(self.simple_values(separator.span.end)?),
            Token::Equals => {
                self.skip_until_semicolon()?;
                StatementKind::Assignment
            }
            Token::LeftParen => {
                let (arguments, list_end) = self.values_until(Token::RightParen)?;
                let Some(terminator) = self.next()? else {
                    return Ok(Some(Statement {
                        name: name.text,
                        offset: self.source.offset + name.span.start,
                        kind: StatementKind::Complex(arguments),
                    }));
                };
                match terminator.token {
                    Token::Semicolon => StatementKind::Complex(arguments),
                    Token::LeftBrace => StatementKind::Group {
                        arguments,
                        body: self.group_body(terminator.span.end)?,
                    },
                    _ if self.source.text[list_end..terminator.span.start].contains('\n') => {
                        self.peeked = Some(Ok(terminator));
                        StatementKind::Complex(arguments)
                    }
                    _ => return Err(self.unexpected(&terminator, "'{' or ';'")),
                }
            }
            _ => return Err(self.unexpected(&separator, "':', '=', or '('")),
        };
        Ok(Some(Statement {
            name: name.text,
            offset: self.source.offset + name.span.start,
            kind,
        }))
    }

    fn values_until(&mut self, terminator: Token) -> Result<(Values<'a>, usize), SyntaxError> {
        let mut values = Values::new();
        loop {
            let lexeme = self.required(terminator.description())?;
            if lexeme.token == terminator {
                return Ok((values, lexeme.span.end));
            }
            match lexeme.token {
                Token::Bare if lexeme.text == "\\" => {}
                Token::Bare | Token::Quoted => values.push(Value {
                    text: lexeme.text,
                    quoted: lexeme.token == Token::Quoted,
                }),
                Token::Comma => {}
                _ => return Err(self.unexpected(&lexeme, "value or list terminator")),
            }
        }
    }

    fn simple_values(&mut self, mut previous_end: usize) -> Result<Values<'a>, SyntaxError> {
        let mut values = Values::new();
        loop {
            let Some(lexeme) = self.next()? else {
                if values.is_empty() {
                    return Err(SyntaxError {
                        offset: self.source.offset + self.source.text.len(),
                        kind: SyntaxErrorKind::UnexpectedEnd {
                            expected: "attribute value",
                        },
                    });
                }
                return Ok(values);
            };
            if lexeme.token == Token::Semicolon {
                return Ok(values);
            }
            let starts_next_attribute = !values.is_empty()
                && self.source.text[previous_end..lexeme.span.start].contains('\n')
                && lexeme.token == Token::Bare
                && self.source.text[lexeme.span.end..]
                    .trim_start_matches([' ', '\t', '\r'])
                    .starts_with([':', '(']);
            if starts_next_attribute {
                self.peeked = Some(Ok(lexeme));
                return Ok(values);
            }
            previous_end = lexeme.span.end;
            match lexeme.token {
                Token::Bare if lexeme.text == "\\" => {}
                Token::Bare | Token::Quoted => values.push(Value {
                    text: lexeme.text,
                    quoted: lexeme.token == Token::Quoted,
                }),
                Token::Comma => {}
                _ => return Err(self.unexpected(&lexeme, "attribute value or ';'")),
            }
        }
    }

    fn skip_until_semicolon(&mut self) -> Result<(), SyntaxError> {
        loop {
            let lexeme = self.required("';'")?;
            if lexeme.token == Token::Semicolon {
                return Ok(());
            }
        }
    }

    fn group_body(&mut self, body_start: usize) -> Result<SourceSlice<'a>, SyntaxError> {
        let mut depth = 1usize;
        loop {
            let lexeme = self.required("'}'")?;
            match lexeme.token {
                Token::LeftBrace => depth += 1,
                Token::RightBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(SourceSlice {
                            text: &self.source.text[body_start..lexeme.span.start],
                            offset: self.source.offset + body_start,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    fn required(&mut self, expected: &'static str) -> Result<Lexeme<'a>, SyntaxError> {
        self.next()?.ok_or(SyntaxError {
            offset: self.source.offset + self.source.text.len(),
            kind: SyntaxErrorKind::UnexpectedEnd { expected },
        })
    }

    fn next(&mut self) -> Result<Option<Lexeme<'a>>, SyntaxError> {
        if let Some(peeked) = self.peeked.take() {
            return peeked.map(Some);
        }
        let Some(token) = self.lexer.next() else {
            return Ok(None);
        };
        let span = self.lexer.span();
        match token {
            Ok(token) => Ok(Some(Lexeme {
                token,
                text: &self.source.text[span.clone()],
                span,
            })),
            Err(()) => Err(SyntaxError {
                offset: self.source.offset + span.start,
                kind: SyntaxErrorKind::InvalidToken,
            }),
        }
    }

    fn unexpected(&self, lexeme: &Lexeme<'_>, expected: &'static str) -> SyntaxError {
        SyntaxError {
            offset: self.source.offset + lexeme.span.start,
            kind: SyntaxErrorKind::UnexpectedToken {
                expected,
                found: lexeme.token.description(),
            },
        }
    }
}
