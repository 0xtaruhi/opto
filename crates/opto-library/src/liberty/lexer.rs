// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

use logos::Logos;

#[derive(Logos, Debug, Clone, Copy, PartialEq, Eq)]
#[logos(skip r"[ \t\r\n\f]+")]
pub(super) enum Token {
    #[regex(r"/\*([^*]|\*+[^*/])*\*+/", logos::skip)]
    #[regex(r"//[^\n]*", logos::skip, allow_greedy = true)]
    Comment,
    #[token("(")]
    LeftParen,
    #[token(")")]
    RightParen,
    #[token("{")]
    LeftBrace,
    #[token("}")]
    RightBrace,
    #[token(":")]
    Colon,
    #[token(";")]
    Semicolon,
    #[token(",")]
    Comma,
    #[token("=")]
    Equals,
    #[regex(r#"\"([^\"\\]|\\[^\x00])*\""#)]
    Quoted,
    #[regex(r#"[^ \t\r\n\f(){}:;,=\"]+"#)]
    Bare,
}

impl Token {
    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::LeftParen => "'('",
            Self::RightParen => "')'",
            Self::LeftBrace => "'{'",
            Self::RightBrace => "'}'",
            Self::Colon => "':'",
            Self::Semicolon => "';'",
            Self::Comma => "','",
            Self::Equals => "'='",
            Self::Quoted => "quoted string",
            Self::Bare => "name or value",
        }
    }
}
