//! Escape-sequence lexing: `\d`, `\n`, `\xHH`, `\u{…}`, `\p{…}`, backrefs, and
//! escaped literals.

use super::scanner::Lexer;
use super::token::{EscapeKind, TokenKind};
use crate::error::{Error, ErrorKind, Result, Span};

impl Lexer<'_> {
    /// Lexes an escape sequence.
    pub(super) fn lex_escape(&mut self, start: usize) -> Result<TokenKind> {
        let c = match self.next_char() {
            Some((_, c)) => c,
            None => {
                return Err(Error::with_span(
                    ErrorKind::UnexpectedEof,
                    self.src,
                    Span::point(start),
                ));
            }
        };

        let escape = match c {
            // Character classes
            'd' => EscapeKind::Digit,
            'D' => EscapeKind::NotDigit,
            'w' => EscapeKind::Word,
            'W' => EscapeKind::NotWord,
            's' => EscapeKind::Whitespace,
            'S' => EscapeKind::NotWhitespace,

            // Anchors
            'b' => EscapeKind::WordBoundary,
            'B' => EscapeKind::NotWordBoundary,
            'A' => EscapeKind::StartOfInput,
            'z' => EscapeKind::EndOfInput,
            'Z' => EscapeKind::EndOfInputBeforeNewline,

            // Special characters
            'n' => EscapeKind::Newline,
            'r' => EscapeKind::CarriageReturn,
            't' => EscapeKind::Tab,
            'f' => EscapeKind::FormFeed,
            'v' => EscapeKind::VerticalTab,
            '0' => EscapeKind::Null,
            // Alert/bell and escape, as in PCRE, Perl, Java and the `regex`
            // crate. Both are plain characters, so they are also valid inside a
            // character class.
            'a' => EscapeKind::Literal('\u{07}'),
            'e' => EscapeKind::Literal('\u{1b}'),

            // One extended grapheme cluster. Expanded by the HIR builder into
            // the UAX #29 boundary rules.
            'X' => EscapeKind::GraphemeCluster,

            // Hex escape
            'x' => {
                let ch = self.lex_hex_escape(start)?;
                EscapeKind::Hex(ch)
            }

            // Unicode escape
            'u' => {
                let ch = self.lex_unicode_escape(start)?;
                EscapeKind::Unicode(ch)
            }

            // Unicode property
            'p' => {
                let name = self.lex_unicode_property(start)?;
                EscapeKind::UnicodeProperty(name)
            }

            // Negated Unicode property
            'P' => {
                let name = self.lex_unicode_property(start)?;
                EscapeKind::NotUnicodeProperty(name)
            }

            // Backreference
            c if c.is_ascii_digit() && c != '0' => {
                let n = self.lex_backref(c)?;
                EscapeKind::Backref(n)
            }

            // Escaping any non-alphanumeric ASCII character yields that
            // character. Patterns written for other engines rely on this far
            // beyond the metacharacters that strictly need it — `\ `, `\"`,
            // `\@`, `\#` and friends are all common in the wild.
            c if c.is_ascii() && !c.is_ascii_alphanumeric() => EscapeKind::Literal(c),

            // An unassigned ASCII letter stays an error rather than decaying to
            // the letter itself, so that giving it a meaning later is not a
            // silent behavior change. Non-ASCII needs no escaping to begin with.
            _ => {
                return Err(Error::with_span(
                    ErrorKind::InvalidEscape(c),
                    self.src,
                    Span::new(start, self.pos),
                ));
            }
        };

        Ok(TokenKind::Escape(escape))
    }

    /// Lexes a hex escape (\xHH).
    fn lex_hex_escape(&mut self, start: usize) -> Result<char> {
        let mut value = 0u32;

        for _ in 0..2 {
            let (_, c) = self.next_char().ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidHexEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })?;

            let digit = c.to_digit(16).ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidHexEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })?;

            value = value * 16 + digit;
        }

        char::from_u32(value).ok_or_else(|| {
            Error::with_span(
                ErrorKind::InvalidHexEscape,
                self.src,
                Span::new(start, self.pos),
            )
        })
    }

    /// Lexes a unicode escape (\u{HHHH} or \uHHHH).
    fn lex_unicode_escape(&mut self, start: usize) -> Result<char> {
        let braced = self.peek_char() == Some('{');

        if braced {
            self.next_char(); // consume '{'

            let mut value = 0u32;
            let mut count = 0;

            loop {
                match self.peek_char() {
                    Some('}') => {
                        self.next_char();
                        break;
                    }
                    Some(c) if c.is_ascii_hexdigit() => {
                        self.next_char();
                        let digit = c.to_digit(16).unwrap();
                        value = value * 16 + digit;
                        count += 1;
                        if count > 6 {
                            return Err(Error::with_span(
                                ErrorKind::InvalidUnicodeEscape,
                                self.src,
                                Span::new(start, self.pos),
                            ));
                        }
                    }
                    _ => {
                        return Err(Error::with_span(
                            ErrorKind::InvalidUnicodeEscape,
                            self.src,
                            Span::new(start, self.pos),
                        ));
                    }
                }
            }

            if count == 0 {
                return Err(Error::with_span(
                    ErrorKind::InvalidUnicodeEscape,
                    self.src,
                    Span::new(start, self.pos),
                ));
            }

            char::from_u32(value).ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidUnicodeEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })
        } else {
            // \uHHHH format
            let mut value = 0u32;

            for _ in 0..4 {
                let (_, c) = self.next_char().ok_or_else(|| {
                    Error::with_span(
                        ErrorKind::InvalidUnicodeEscape,
                        self.src,
                        Span::new(start, self.pos),
                    )
                })?;

                let digit = c.to_digit(16).ok_or_else(|| {
                    Error::with_span(
                        ErrorKind::InvalidUnicodeEscape,
                        self.src,
                        Span::new(start, self.pos),
                    )
                })?;

                value = value * 16 + digit;
            }

            char::from_u32(value).ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidUnicodeEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })
        }
    }

    /// Lexes a Unicode property escape.
    ///
    /// Supports two syntaxes:
    /// - `\p{Name}` or `\P{Name}` - full property name in braces
    /// - `\pL` or `\PL` - single-letter shorthand for general categories
    fn lex_unicode_property(&mut self, start: usize) -> Result<String> {
        match self.peek_char() {
            Some('{') => {
                // Brace syntax: \p{Name}
                self.next_char(); // consume '{'

                let mut name = String::new();

                // Read property name until closing brace
                loop {
                    match self.next_char() {
                        Some((_, '}')) => break,
                        Some((_, c)) if c.is_alphanumeric() || c == '_' || c == '-' => {
                            name.push(c);
                        }
                        _ => {
                            return Err(Error::with_span(
                                ErrorKind::InvalidUnicodeProperty,
                                self.src,
                                Span::new(start, self.pos),
                            ));
                        }
                    }
                }

                if name.is_empty() {
                    return Err(Error::with_span(
                        ErrorKind::InvalidUnicodeProperty,
                        self.src,
                        Span::new(start, self.pos),
                    ));
                }

                Ok(name)
            }
            Some(c) if c.is_ascii_alphabetic() => {
                // Shorthand syntax: \pL (single letter)
                self.next_char(); // consume the letter
                Ok(c.to_string())
            }
            _ => Err(Error::with_span(
                ErrorKind::InvalidUnicodeProperty,
                self.src,
                Span::new(start, self.pos),
            )),
        }
    }

    /// Lexes a POSIX bracket-expression class (`[:alpha:]`, `[:^alpha:]`),
    /// already past the opening `[` — the caller peeked the following `:`
    /// before dispatching here. Once `[:` has been seen the syntax is
    /// committed: anything short of a well-formed `[:name:]` (or `[:^name:]`)
    /// is an error, never a silent fallback to a literal `[`.
    pub(super) fn lex_posix_class(&mut self, start: usize) -> Result<TokenKind> {
        self.next_char(); // consume ':'

        let negated = if self.peek_char() == Some('^') {
            self.next_char();
            true
        } else {
            false
        };

        let mut name = String::new();
        while let Some(c) = self.peek_char() {
            if c.is_ascii_alphabetic() {
                self.next_char();
                name.push(c);
            } else {
                break;
            }
        }

        if name.is_empty() {
            return Err(Error::with_span(
                ErrorKind::InvalidPosixClass,
                self.src,
                Span::new(start, self.pos),
            ));
        }

        match (self.next_char(), self.next_char()) {
            (Some((_, ':')), Some((_, ']'))) => Ok(TokenKind::PosixClass { name, negated }),
            _ => Err(Error::with_span(
                ErrorKind::InvalidPosixClass,
                self.src,
                Span::new(start, self.pos),
            )),
        }
    }

    /// Lexes a backreference (\1, \12, etc.).
    fn lex_backref(&mut self, first: char) -> Result<u32> {
        let mut n = first.to_digit(10).unwrap();

        // Consume additional digits
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.next_char();
                n = n * 10 + c.to_digit(10).unwrap();
            } else {
                break;
            }
        }

        Ok(n)
    }
}
