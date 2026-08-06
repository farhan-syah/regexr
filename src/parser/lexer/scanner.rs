//! The character scanner: turns pattern text into [`Token`]s.

use super::token::{Token, TokenKind};
use crate::error::{Result, Span};

/// The lexer for regex patterns.
pub struct Lexer<'a> {
    /// The source pattern.
    pub(super) src: &'a str,
    /// Iterator over characters.
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    /// Current position in bytes.
    pub(super) pos: usize,
    /// Nesting depth of character classes (0 = outside any class). Tracked as a
    /// depth (not a bool) so nested classes like `[a[b]c]` lex correctly.
    class_depth: usize,
    /// Extended mode (`x`): unescaped ASCII whitespace and `#` comments outside
    /// a character class are not part of the pattern.
    extended: bool,
}

impl<'a> Lexer<'a> {
    /// Creates a new lexer.
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            chars: src.char_indices().peekable(),
            pos: 0,
            class_depth: 0,
            extended: false,
        }
    }

    /// Returns the source pattern.
    pub fn source(&self) -> &str {
        self.src
    }

    /// Peeks at the next character without consuming it.
    pub(super) fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().map(|(_, c)| *c)
    }

    /// Consumes the next character.
    pub(super) fn next_char(&mut self) -> Option<(usize, char)> {
        let result = self.chars.next();
        if let Some((pos, c)) = result {
            self.pos = pos + c.len_utf8();
        }
        result
    }

    /// Enter (`true`) or exit (`false`) a character class, maintaining the
    /// nesting depth so nested classes lex in class mode until fully closed.
    pub fn set_in_class(&mut self, in_class: bool) {
        if in_class {
            self.class_depth += 1;
        } else {
            self.class_depth = self.class_depth.saturating_sub(1);
        }
    }

    /// Turns extended (`x`) mode on or off for subsequent tokens.
    ///
    /// The parser drives this as it enters and leaves `(?x)` / `(?x:…)` scopes;
    /// the lexer needs it because whitespace and comments are removed before a
    /// token is ever produced.
    pub fn set_extended(&mut self, extended: bool) {
        self.extended = extended;
    }

    /// Returns the next token.
    pub fn next_token(&mut self) -> Result<Token> {
        if self.extended && self.class_depth == 0 {
            self.skip_extended_trivia();
        }

        let (start, c) = match self.next_char() {
            Some(pair) => pair,
            None => {
                return Ok(Token {
                    kind: TokenKind::Eof,
                    span: Span::point(self.pos),
                });
            }
        };

        let kind = if self.class_depth > 0 {
            self.lex_class_char(c, start)?
        } else {
            self.lex_char(c, start)?
        };

        Ok(Token {
            kind,
            span: Span::new(start, self.pos),
        })
    }

    /// Consumes whitespace and `#` comments that extended mode strips from the
    /// pattern. Only reached outside a character class, where whitespace stays
    /// literal in every engine that implements this mode.
    fn skip_extended_trivia(&mut self) {
        loop {
            match self.peek_char() {
                Some(c) if c.is_ascii_whitespace() => {
                    self.next_char();
                }
                Some('#') => {
                    while let Some((_, c)) = self.next_char() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => return,
            }
        }
    }

    /// Lexes a character outside a character class.
    fn lex_char(&mut self, c: char, start: usize) -> Result<TokenKind> {
        match c {
            '.' => Ok(TokenKind::Dot),
            '*' => Ok(TokenKind::Star),
            '+' => Ok(TokenKind::Plus),
            '?' => Ok(TokenKind::Question),
            '|' => Ok(TokenKind::Pipe),
            '^' => Ok(TokenKind::Caret),
            '$' => Ok(TokenKind::Dollar),
            '(' => Ok(TokenKind::OpenParen),
            ')' => Ok(TokenKind::CloseParen),
            '[' => Ok(TokenKind::OpenBracket),
            ']' => Ok(TokenKind::CloseBracket),
            '{' => Ok(TokenKind::OpenBrace),
            '}' => Ok(TokenKind::CloseBrace),
            ':' => Ok(TokenKind::Colon),
            '<' => Ok(TokenKind::LessThan),
            '>' => Ok(TokenKind::GreaterThan),
            '=' => Ok(TokenKind::Equals),
            '!' => Ok(TokenKind::Exclamation),
            ',' => Ok(TokenKind::Comma),
            '\\' => self.lex_escape(start),
            c if c.is_ascii_digit() => Ok(TokenKind::Digit(c.to_digit(10).unwrap())),
            c => Ok(TokenKind::Literal(c)),
        }
    }

    /// Lexes a character inside a character class.
    fn lex_class_char(&mut self, c: char, start: usize) -> Result<TokenKind> {
        match c {
            ']' => Ok(TokenKind::CloseBracket),
            // `[` opens a nested class (set union), matching the Rust `regex`
            // crate / HuggingFace tokenizers — e.g. `[a[b-c]]` or `[^(\s|[.,!])]`.
            '[' => Ok(TokenKind::OpenBracket),
            '-' => Ok(TokenKind::Hyphen),
            '^' => Ok(TokenKind::Caret),
            '\\' => self.lex_escape(start),
            c => Ok(TokenKind::Literal(c)),
        }
    }

    /// Reads the remainder of an identifier (for named groups).
    ///
    /// The caller has already consumed the first character, so an empty result
    /// is the ordinary single-character-name case, not a failure.
    pub fn read_ident_rest(&mut self) -> String {
        let mut ident = String::new();

        while let Some(c) = self.peek_char() {
            if c.is_alphanumeric() || c == '_' {
                self.next_char();
                ident.push(c);
            } else {
                break;
            }
        }

        ident
    }

    /// Reads a number (for repetition bounds).
    pub fn read_number(&mut self) -> Result<Option<u32>> {
        let mut n: u32 = 0;
        let mut has_digit = false;

        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.next_char();
                has_digit = true;
                n = n.saturating_mul(10).saturating_add(c.to_digit(10).unwrap());
            } else {
                break;
            }
        }

        Ok(if has_digit { Some(n) } else { None })
    }
}
