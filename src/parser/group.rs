//! Group, lookaround and inline-flag parsing.

use super::ast::*;
use super::lexer::TokenKind;
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result, Span};

impl Parser<'_> {
    /// Parses a group: (...), (?:...), (?=...), etc.
    pub(super) fn parse_group(&mut self) -> Result<Expr> {
        let start_span = self.current.span;
        self.advance()?; // consume '('

        // Check for special group syntax
        if matches!(self.current.kind, TokenKind::Question) {
            self.advance()?;

            match &self.current.kind {
                // Non-capturing group (?:...)
                TokenKind::Colon => {
                    self.advance()?;
                    let expr = self.parse_alternation()?;
                    self.expect_close_paren(start_span)?;
                    Ok(Expr::Group(Box::new(Group {
                        expr,
                        kind: GroupKind::NonCapturing,
                    })))
                }

                // Named group (?<name>...) or lookbehind (?<=...) (?<!...)
                TokenKind::LessThan => {
                    self.advance()?;

                    // Check if it's lookbehind or named group
                    match &self.current.kind {
                        TokenKind::Equals => {
                            // (?<=...) positive lookbehind
                            self.advance()?;
                            let expr = self.parse_alternation()?;
                            self.expect_close_paren(start_span)?;
                            Ok(Expr::Lookaround(Box::new(Lookaround {
                                expr,
                                kind: LookaroundKind::PositiveLookbehind,
                            })))
                        }
                        TokenKind::Exclamation => {
                            // (?<!...) negative lookbehind
                            self.advance()?;
                            let expr = self.parse_alternation()?;
                            self.expect_close_paren(start_span)?;
                            Ok(Expr::Lookaround(Box::new(Lookaround {
                                expr,
                                kind: LookaroundKind::NegativeLookbehind,
                            })))
                        }
                        _ => {
                            // (?<name>...) named group
                            // The first character of the name is in self.current
                            let name = self.parse_group_name()?;
                            self.expect(TokenKind::GreaterThan)?;
                            let expr = self.parse_alternation()?;
                            self.expect_close_paren(start_span)?;
                            let index = self.next_capture;
                            self.next_capture += 1;
                            self.capture_count += 1;
                            self.named_groups.insert(name.clone(), index);
                            Ok(Expr::Group(Box::new(Group {
                                expr,
                                kind: GroupKind::NamedCapturing { name, index },
                            })))
                        }
                    }
                }

                // Atomic group (?>...): deliberately unsupported, since
                // regexr's engines are linear-time and never backtrack, so
                // there is nothing for an atomic group to bound.
                TokenKind::GreaterThan => Err(Error::with_span(
                    ErrorKind::AtomicGroup,
                    self.pattern,
                    start_span,
                )),

                // Lookahead (?=...) or (?!...)
                TokenKind::Equals => {
                    self.advance()?;
                    let expr = self.parse_alternation()?;
                    self.expect_close_paren(start_span)?;
                    Ok(Expr::Lookaround(Box::new(Lookaround {
                        expr,
                        kind: LookaroundKind::PositiveLookahead,
                    })))
                }
                TokenKind::Exclamation => {
                    self.advance()?;
                    let expr = self.parse_alternation()?;
                    self.expect_close_paren(start_span)?;
                    Ok(Expr::Lookaround(Box::new(Lookaround {
                        expr,
                        kind: LookaroundKind::NegativeLookahead,
                    })))
                }

                // Note: `(?#...)` inline comments never reach the parser as
                // tokens — the lexer recognizes `(?#` as a unit at the point
                // `(` is scanned (see `Lexer::next_token`) and consumes the
                // whole construct itself, so it behaves identically in every
                // mode, including extended (`x`) mode.

                // Python-style named group (?P<name>...), or a named
                // backreference to one, (?P=name).
                TokenKind::Literal('P') => {
                    self.advance()?;
                    match &self.current.kind {
                        TokenKind::LessThan => {
                            self.advance()?;
                            // The first character of the name is now in self.current
                            let name = self.parse_group_name()?;
                            self.expect(TokenKind::GreaterThan)?;
                            let expr = self.parse_alternation()?;
                            self.expect_close_paren(start_span)?;
                            let index = self.next_capture;
                            self.next_capture += 1;
                            self.capture_count += 1;
                            self.named_groups.insert(name.clone(), index);
                            Ok(Expr::Group(Box::new(Group {
                                expr,
                                kind: GroupKind::NamedCapturing { name, index },
                            })))
                        }
                        TokenKind::Equals => {
                            // (?P=name) - named backreference, Python style.
                            self.advance()?;
                            let name_span = self.current.span;
                            let name = self.parse_group_name()?;
                            self.expect(TokenKind::CloseParen)?;
                            match self.named_groups.get(&name) {
                                Some(&index) => Ok(Expr::Backref(index)),
                                None => Err(Error::with_span(
                                    ErrorKind::UnknownGroupName(name),
                                    self.pattern,
                                    name_span,
                                )),
                            }
                        }
                        _ => Err(Error::with_span(
                            ErrorKind::InvalidGroup,
                            self.pattern,
                            self.current.span,
                        )),
                    }
                }

                // Quote-delimited named group (?'name'...), equivalent to
                // (?<name>...) and (?P<name>...).
                TokenKind::Literal('\'') => {
                    self.advance()?;
                    // An immediate closing quote means an empty name, which
                    // `parse_group_name` cannot detect on its own: `'` lexes
                    // to the same `Literal` token kind as any other name
                    // character, so it would otherwise be read as content.
                    if matches!(self.current.kind, TokenKind::Literal('\'')) {
                        return Err(Error::with_span(
                            ErrorKind::InvalidGroup,
                            self.pattern,
                            self.current.span,
                        ));
                    }
                    let name = self.parse_group_name()?;
                    match self.current.kind {
                        TokenKind::Literal('\'') => {
                            self.advance()?;
                        }
                        _ => {
                            return Err(Error::with_span(
                                ErrorKind::InvalidGroup,
                                self.pattern,
                                self.current.span,
                            ));
                        }
                    }
                    let expr = self.parse_alternation()?;
                    self.expect_close_paren(start_span)?;
                    let index = self.next_capture;
                    self.next_capture += 1;
                    self.capture_count += 1;
                    self.named_groups.insert(name.clone(), index);
                    Ok(Expr::Group(Box::new(Group {
                        expr,
                        kind: GroupKind::NamedCapturing { name, index },
                    })))
                }

                // Flags (?imsx-imsx) or (?imsx:...)
                TokenKind::Literal(c) if starts_flag_group(*c) => {
                    // Flags set by `(?flags:...)` are scoped to the group only, so
                    // remember the outer flags to restore them afterward. Without
                    // this, e.g. `'(?i:[sdmt])\p{L}` would leak case-insensitivity
                    // onto the trailing `\p{L}`.
                    let saved_flags = self.flags;
                    self.parse_flags()?;

                    if matches!(self.current.kind, TokenKind::Colon) {
                        // (?flags:...) — flags apply only within this group. Record
                        // the effective flags on the group so the HIR builder can
                        // scope case-folding/unicode to the body, then restore the
                        // outer flags for the rest of the pattern.
                        let group_flags = self.flags;
                        self.advance()?;
                        let expr = self.parse_alternation()?;
                        // Restore before consuming `)`, because consuming it
                        // lexes the token that follows the group — and under
                        // extended mode the lexer decides there whether the
                        // whitespace after `)` is part of the pattern.
                        self.restore_flags(saved_flags);
                        self.expect_close_paren(start_span)?;
                        Ok(Expr::Group(Box::new(Group {
                            expr,
                            kind: GroupKind::Flagged(group_flags),
                        })))
                    } else if matches!(self.current.kind, TokenKind::CloseParen) {
                        // (?flags) - just set flags
                        self.advance()?;
                        Ok(Expr::Empty)
                    } else {
                        Err(Error::with_span(
                            ErrorKind::InvalidGroup,
                            self.pattern,
                            self.current.span,
                        ))
                    }
                }

                _ => Err(Error::with_span(
                    ErrorKind::InvalidGroup,
                    self.pattern,
                    self.current.span,
                )),
            }
        } else {
            // Regular capturing group
            let index = self.next_capture;
            self.next_capture += 1;
            self.capture_count += 1;

            let expr = self.parse_alternation()?;
            self.expect_close_paren(start_span)?;

            Ok(Expr::Group(Box::new(Group {
                expr,
                kind: GroupKind::Capturing(index),
            })))
        }
    }

    /// Parses a group name from the token stream: `self.current` must
    /// already hold the name's first character (lexed as a `Literal`), and
    /// the rest is read straight off the lexer's raw character stream via
    /// `read_ident_rest`. Shared by every named-group spelling —
    /// `(?<name>...)`, `(?P<name>...)`, `(?'name'...)` — and by the
    /// `(?P=name)` backreference, so the name syntax (and its malformed-name
    /// error) is identical across all of them. Does not consume the
    /// construct's closing delimiter; callers check that themselves, since
    /// it differs per spelling (`>`, `'`, or `)`).
    fn parse_group_name(&mut self) -> Result<String> {
        let first_char = match &self.current.kind {
            TokenKind::Literal(c) => *c,
            _ => {
                return Err(Error::with_span(
                    ErrorKind::InvalidGroup,
                    self.pattern,
                    self.current.span,
                ));
            }
        };
        let rest = self.lexer.read_ident_rest();
        let name = format!("{}{}", first_char, rest);
        self.current = self.lexer.next_token()?;
        Ok(name)
    }

    /// Expects a closing parenthesis.
    fn expect_close_paren(&mut self, open_span: Span) -> Result<()> {
        if matches!(self.current.kind, TokenKind::CloseParen) {
            self.advance()?;
            Ok(())
        } else {
            Err(Error::with_span(
                ErrorKind::UnmatchedOpenParen,
                self.pattern,
                open_span,
            ))
        }
    }

    /// Parses inline flags.
    fn parse_flags(&mut self) -> Result<()> {
        let mut negating = false;

        loop {
            match &self.current.kind {
                TokenKind::Literal(c) => match c {
                    'i' => {
                        self.flags.case_insensitive = !negating;
                        self.advance()?;
                    }
                    'm' => {
                        self.flags.multi_line = !negating;
                        self.advance()?;
                    }
                    's' => {
                        self.flags.dot_all = !negating;
                        self.advance()?;
                    }
                    'x' => {
                        self.flags.extended = !negating;
                        // The lexer, not the HIR builder, implements extended
                        // mode: whitespace and `#` comments are dropped before
                        // a token exists.
                        self.lexer.set_extended(self.flags.extended);
                        self.advance()?;
                    }
                    'u' => {
                        self.flags.unicode = !negating;
                        self.advance()?;
                    }
                    // Outside a character class `-` lexes as a literal, not as
                    // `Hyphen`, so `(?i-s)` reaches us in this arm.
                    '-' => {
                        negating = true;
                        self.advance()?;
                    }
                    _ => break,
                },
                TokenKind::Hyphen => {
                    negating = true;
                    self.advance()?;
                }
                _ => break,
            }
        }

        Ok(())
    }
}

/// Returns true if the character can open a `(?…)` flag group. A leading `-`
/// negates every flag that follows it, as in `(?-i)` or `(?-x:…)`.
fn starts_flag_group(c: char) -> bool {
    is_flag_char(c) || c == '-'
}

/// Returns true if the character is a valid flag.
fn is_flag_char(c: char) -> bool {
    matches!(c, 'i' | 'm' | 's' | 'x' | 'u')
}
