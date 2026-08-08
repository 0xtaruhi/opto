// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use crate::{BooleanFunctionErrorKind, LibraryError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Abstract syntax tree for a Liberty Boolean expression.
pub enum BooleanFunction {
    /// Boolean constant.
    Const(bool),
    /// Named pin or state variable.
    Pin(String),
    /// Logical negation.
    Not(Box<Self>),
    /// Logical conjunction.
    And(Box<Self>, Box<Self>),
    /// Logical disjunction.
    Or(Box<Self>, Box<Self>),
    /// Logical exclusive OR.
    Xor(Box<Self>, Box<Self>),
    /// Logical implication.
    Imp(Box<Self>, Box<Self>),
    /// Logical equivalence.
    Iff(Box<Self>, Box<Self>),
    /// Conditional expression `(condition ? true_value : false_value)`.
    Cond(Box<Self>, Box<Self>, Box<Self>),
}
impl BooleanFunction {
    /// Parses a Liberty Boolean expression.
    ///
    /// # Errors
    ///
    /// Returns [`LibraryError::BooleanFunction`] with the failing byte offset
    /// when the expression is incomplete or syntactically invalid.
    pub fn parse(text: &str) -> Result<Self, LibraryError> {
        FunctionParser::new(text)
            .parse()
            .map_err(|error| LibraryError::BooleanFunction {
                expression: text.to_owned(),
                offset: error.offset,
                kind: error.kind,
            })
    }

    /// Evaluates the expression using `lookup` for named pins.
    ///
    /// Returns `None` if the callback cannot resolve a referenced name.
    pub fn eval(&self, lookup: &mut impl FnMut(&str) -> Option<bool>) -> Option<bool> {
        enum Frame<'a> {
            Visit(&'a BooleanFunction),
            Not,
            Binary(&'a BooleanFunction),
            Conditional(&'a BooleanFunction, &'a BooleanFunction),
        }

        let mut frames = vec![Frame::Visit(self)];
        let mut values = Vec::new();
        while let Some(frame) = frames.pop() {
            match frame {
                Frame::Visit(Self::Const(value)) => values.push(*value),
                Frame::Visit(Self::Pin(name)) => values.push(lookup(name)?),
                Frame::Visit(Self::Not(argument)) => {
                    frames.push(Frame::Not);
                    frames.push(Frame::Visit(argument));
                }
                Frame::Visit(
                    binary @ (Self::And(left, right)
                    | Self::Or(left, right)
                    | Self::Xor(left, right)
                    | Self::Imp(left, right)
                    | Self::Iff(left, right)),
                ) => {
                    frames.push(Frame::Binary(binary));
                    frames.push(Frame::Visit(right));
                    frames.push(Frame::Visit(left));
                }
                Frame::Visit(Self::Cond(condition, when_true, when_false)) => {
                    frames.push(Frame::Conditional(when_true, when_false));
                    frames.push(Frame::Visit(condition));
                }
                Frame::Not => {
                    let value = values.pop()?;
                    values.push(!value);
                }
                Frame::Binary(function) => {
                    let right = values.pop()?;
                    let left = values.pop()?;
                    values.push(match function {
                        Self::And(_, _) => left & right,
                        Self::Or(_, _) => left | right,
                        Self::Xor(_, _) => left ^ right,
                        Self::Imp(_, _) => !left | right,
                        Self::Iff(_, _) => left == right,
                        _ => return None,
                    });
                }
                Frame::Conditional(when_true, when_false) => {
                    frames.push(Frame::Visit(if values.pop()? {
                        when_true
                    } else {
                        when_false
                    }));
                }
            }
        }
        (values.len() == 1).then(|| values[0])
    }

    /// Evaluates the complete truth table for `inputs`, with the first input
    /// occupying the least-significant assignment bit.
    ///
    /// Returns `None` when the function references an unknown input or when
    /// more than six inputs would exceed the returned 64-bit table.
    #[must_use]
    pub fn truth_table_bits(&self, inputs: &[&str]) -> Option<u64> {
        if inputs.len() > 6 {
            return None;
        }
        let mut bits = 0u64;
        for assignment in 0..(1usize << inputs.len()) {
            let value = self.eval(&mut |name| {
                let index = inputs.iter().position(|input| *input == name)?;
                Some(((assignment >> index) & 1) == 1)
            })?;
            if value {
                bits |= 1u64 << assignment;
            }
        }
        Some(bits)
    }

    #[must_use]
    /// Returns a `(pin, polarity)` pair when this is a single literal.
    pub fn as_literal(&self) -> Option<(&str, bool)> {
        match self {
            Self::Pin(name) => Some((name, true)),
            Self::Not(arg) => match arg.as_ref() {
                Self::Pin(name) => Some((name, false)),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a str),
    Const(bool),
    Not,
    And,
    Or,
    Xor,
    Imp,
    Iff,
    Question,
    Colon,
    LeftParen,
    RightParen,
    Apostrophe,
}

impl Token<'_> {
    const fn starts_expression(self) -> bool {
        matches!(
            self,
            Self::Name(_) | Self::Const(_) | Self::Not | Self::LeftParen
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct SpannedToken<'a> {
    token: Token<'a>,
    offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct FunctionError {
    offset: usize,
    kind: BooleanFunctionErrorKind,
}

struct FunctionParser<'a> {
    text: &'a str,
    offset: usize,
    peeked: Option<SpannedToken<'a>>,
    nodes: usize,
}

impl<'a> FunctionParser<'a> {
    const fn new(text: &'a str) -> Self {
        Self {
            text,
            offset: 0,
            peeked: None,
            nodes: 0,
        }
    }

    fn parse(mut self) -> Result<BooleanFunction, FunctionError> {
        let expression = self.expression(0, 0)?;
        if let Some(token) = self.next_token() {
            return Err(FunctionError {
                offset: token.offset,
                kind: BooleanFunctionErrorKind::TrailingInput,
            });
        }
        Ok(expression)
    }

    fn expression(
        &mut self,
        minimum_binding_power: u8,
        depth: usize,
    ) -> Result<BooleanFunction, FunctionError> {
        const MAX_NESTING: usize = 256;
        if depth > MAX_NESTING {
            return Err(self.complexity_limit());
        }
        let token = self.next_token().ok_or(FunctionError {
            offset: self.offset,
            kind: BooleanFunctionErrorKind::UnexpectedEnd,
        })?;
        let mut left = match token.token {
            Token::Name(name) => BooleanFunction::Pin(name.to_owned()),
            Token::Const(value) => BooleanFunction::Const(value),
            Token::Not => BooleanFunction::Not(Box::new(self.expression(11, depth + 1)?)),
            Token::LeftParen => {
                let nested = self.expression(0, depth + 1)?;
                match self.next_token() {
                    Some(SpannedToken {
                        token: Token::RightParen,
                        ..
                    }) => nested,
                    Some(token) => return Err(Self::unexpected(token.offset)),
                    None => return Err(self.unexpected_end()),
                }
            }
            _ => return Err(Self::unexpected(token.offset)),
        };
        self.record_node(token.offset)?;

        while let Some(next) = self.peek_token() {
            if next.token == Token::Apostrophe {
                _ = self.next_token();
                left = BooleanFunction::Not(Box::new(left));
                self.record_node(next.offset)?;
                continue;
            }
            if next.token == Token::Question {
                if minimum_binding_power > 1 {
                    break;
                }
                _ = self.next_token();
                let when_true = self.expression(0, depth + 1)?;
                match self.next_token() {
                    Some(SpannedToken {
                        token: Token::Colon,
                        ..
                    }) => {}
                    Some(token) => return Err(Self::unexpected(token.offset)),
                    None => return Err(self.unexpected_end()),
                }
                let when_false = self.expression(1, depth + 1)?;
                left = BooleanFunction::Cond(
                    Box::new(left),
                    Box::new(when_true),
                    Box::new(when_false),
                );
                self.record_node(next.offset)?;
                continue;
            }

            let (left_power, right_power, operator) = match next.token {
                Token::Iff => (2, 3, Token::Iff),
                Token::Imp => (4, 4, Token::Imp),
                Token::Or => (5, 6, Token::Or),
                Token::Xor => (7, 8, Token::Xor),
                Token::And => (9, 10, Token::And),
                token if token.starts_expression() => (9, 10, Token::And),
                _ => break,
            };
            if left_power < minimum_binding_power {
                break;
            }
            if next.token == operator {
                _ = self.next_token();
            }
            let right = self.expression(right_power, depth + 1)?;
            left = match operator {
                Token::And => BooleanFunction::And(Box::new(left), Box::new(right)),
                Token::Or => BooleanFunction::Or(Box::new(left), Box::new(right)),
                Token::Xor => BooleanFunction::Xor(Box::new(left), Box::new(right)),
                Token::Imp => BooleanFunction::Imp(Box::new(left), Box::new(right)),
                Token::Iff => BooleanFunction::Iff(Box::new(left), Box::new(right)),
                _ => unreachable!("binding table contains only binary operators"),
            };
            self.record_node(next.offset)?;
        }
        Ok(left)
    }

    fn record_node(&mut self, offset: usize) -> Result<(), FunctionError> {
        const MAX_NODES: usize = 512;
        self.nodes = self.nodes.checked_add(1).ok_or(FunctionError {
            offset,
            kind: BooleanFunctionErrorKind::ComplexityLimit,
        })?;
        if self.nodes > MAX_NODES {
            return Err(FunctionError {
                offset,
                kind: BooleanFunctionErrorKind::ComplexityLimit,
            });
        }
        Ok(())
    }

    const fn complexity_limit(&self) -> FunctionError {
        FunctionError {
            offset: self.offset,
            kind: BooleanFunctionErrorKind::ComplexityLimit,
        }
    }

    fn peek_token(&mut self) -> Option<SpannedToken<'a>> {
        if self.peeked.is_none() {
            self.peeked = self.lex_token();
        }
        self.peeked
    }

    fn next_token(&mut self) -> Option<SpannedToken<'a>> {
        self.peeked.take().or_else(|| self.lex_token())
    }

    fn lex_token(&mut self) -> Option<SpannedToken<'a>> {
        let bytes = self.text.as_bytes();
        while matches!(bytes.get(self.offset), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.offset += 1;
        }
        let start = self.offset;
        let byte = *bytes.get(start)?;
        let single = |token| SpannedToken {
            token,
            offset: start,
        };
        let token = match byte {
            b'!' | b'~' => single(Token::Not),
            b'&' | b'*' => single(Token::And),
            b'+' | b'|' => single(Token::Or),
            b'^' => single(Token::Xor),
            b'?' => single(Token::Question),
            b':' => single(Token::Colon),
            b'(' => single(Token::LeftParen),
            b')' => single(Token::RightParen),
            b'\'' => single(Token::Apostrophe),
            b'-' | b'=' if bytes.get(start + 1) == Some(&b'>') => {
                self.offset += 1;
                single(Token::Imp)
            }
            b'=' if bytes.get(start + 1) == Some(&b'=') => {
                self.offset += 1;
                single(Token::Iff)
            }
            _ => {
                let mut end = start;
                while let Some(byte) = bytes.get(end) {
                    if byte.is_ascii_whitespace()
                        || matches!(
                            byte,
                            b'!' | b'~'
                                | b'&'
                                | b'*'
                                | b'+'
                                | b'|'
                                | b'^'
                                | b'?'
                                | b':'
                                | b'('
                                | b')'
                                | b'\''
                                | b'='
                        )
                    {
                        break;
                    }
                    end += 1;
                }
                if end == start {
                    self.offset += 1;
                    return Some(single(Token::Name(&self.text[start..self.offset])));
                }
                self.offset = end - 1;
                match &self.text[start..end] {
                    "0" => single(Token::Const(false)),
                    "1" => single(Token::Const(true)),
                    name => single(Token::Name(name)),
                }
            }
        };
        self.offset += 1;
        Some(token)
    }

    const fn unexpected(offset: usize) -> FunctionError {
        FunctionError {
            offset,
            kind: BooleanFunctionErrorKind::UnexpectedToken,
        }
    }

    const fn unexpected_end(&self) -> FunctionError {
        FunctionError {
            offset: self.offset,
            kind: BooleanFunctionErrorKind::UnexpectedEnd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_conditional_implication_and_implicit_and() {
        let function = BooleanFunction::parse("A ? (B C) : (B -> C)").unwrap();
        let mut lookup = |name: &str| match name {
            "A" | "C" => Some(false),
            "B" => Some(true),
            _ => None,
        };
        assert!(!function.eval(&mut lookup).unwrap());
    }

    #[test]
    fn recognizes_positive_and_negative_literals() {
        assert_eq!(
            BooleanFunction::parse("Q").unwrap().as_literal(),
            Some(("Q", true))
        );
        assert_eq!(
            BooleanFunction::parse("!Q").unwrap().as_literal(),
            Some(("Q", false))
        );
        assert_eq!(BooleanFunction::parse("A+B").unwrap().as_literal(), None);
    }

    #[test]
    fn computes_truth_bits_in_declared_input_order() {
        let function = BooleanFunction::parse("A ^ B").unwrap();
        assert_eq!(function.truth_table_bits(&["A", "B"]), Some(0b0110));
        assert_eq!(function.truth_table_bits(&["A"]), None);
    }

    #[test]
    fn rejects_expressions_beyond_the_nesting_budget() {
        let expression = format!("{}A", "!".repeat(300));
        let error = BooleanFunction::parse(&expression).unwrap_err();
        assert!(error.to_string().contains("complexity limit"));
    }
}
