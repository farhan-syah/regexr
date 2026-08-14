//! Group, lookaround and inline-flag parsing.

use super::ast::*;
use super::lexer::TokenKind;
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result, Span};

impl Parser<'_> {
    /// Parses a group: (...), (?:...), (?=...), etc.
    ///
    /// Wrapped in [`Parser::with_nesting`] since every `(` is one nesting
    /// level, regardless of which group flavor it turns out to be — that
    /// covers the mutual recursion back through `parse_alternation` at every
    /// one of this function's call sites into it.
    pub(super) fn parse_group(&mut self) -> Result<Expr> {
        self.with_nesting(Self::parse_group_inner)
    }

    /// Dispatches on the group's flavor. Each branch lives in its own
    /// function so the frame held across the recursion carries only this
    /// dispatch — see [`DEFAULT_NEST_LIMIT`](super::DEFAULT_NEST_LIMIT).
    fn parse_group_inner(&mut self) -> Result<Expr> {
        let start_span = self.current.span;
        self.advance()?; // consume '('

        // Check for special group syntax
        if matches!(self.current.kind, TokenKind::Question) {
            self.advance()?;

            match &self.current.kind {
                // Non-capturing group (?:...)
                TokenKind::Colon => self.parse_non_capturing_group(start_span),

                // Named group (?<name>...) or lookbehind (?<=...) (?<!...)
                TokenKind::LessThan => self.parse_angle_group(start_span),

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
                    self.parse_lookaround(start_span, LookaroundKind::PositiveLookahead)
                }
                TokenKind::Exclamation => {
                    self.parse_lookaround(start_span, LookaroundKind::NegativeLookahead)
                }

                // Note: `(?#...)` inline comments never reach the parser as
                // tokens — the lexer recognizes `(?#` as a unit at the point
                // `(` is scanned (see `Lexer::next_token`) and consumes the
                // whole construct itself, so it behaves identically in every
                // mode, including extended (`x`) mode.

                // Python-style named group (?P<name>...), or a named
                // backreference to one, (?P=name).
                TokenKind::Literal('P') => self.parse_python_group(start_span),

                // Quote-delimited named group (?'name'...), equivalent to
                // (?<name>...) and (?P<name>...).
                TokenKind::Literal('\'') => self.parse_quoted_named_group(start_span),

                // Flags (?imsx-imsx) or (?imsx:...)
                TokenKind::Literal(c) if starts_flag_group(*c) => self.parse_flag_group(start_span),

                _ => Err(Error::with_span(
                    ErrorKind::InvalidGroup,
                    self.pattern,
                    self.current.span,
                )),
            }
        } else {
            self.parse_capturing_group(start_span)
        }
    }

    /// `(?:...)`, positioned on the `:`.
    #[inline(never)]
    fn parse_non_capturing_group(&mut self, start_span: Span) -> Result<Expr> {
        self.advance()?;
        let expr = self.parse_alternation()?;
        self.expect_close_paren(start_span)?;
        Ok(group(expr, GroupKind::NonCapturing))
    }

    /// A lookahead or lookbehind body, positioned on the `=`/`!` that opens
    /// it. Both directions differ only in the node they build.
    #[inline(never)]
    fn parse_lookaround(&mut self, start_span: Span, kind: LookaroundKind) -> Result<Expr> {
        self.advance()?;
        let expr = self.parse_alternation()?;
        self.expect_close_paren(start_span)?;
        Ok(Expr::Lookaround(Box::new(Lookaround { expr, kind })))
    }

    /// `(?<...`: a lookbehind (`(?<=`, `(?<!`) or a named group `(?<name>`,
    /// positioned on the `<`.
    #[inline(never)]
    fn parse_angle_group(&mut self, start_span: Span) -> Result<Expr> {
        self.advance()?;

        // Check if it's lookbehind or named group
        match &self.current.kind {
            TokenKind::Equals => {
                self.parse_lookaround(start_span, LookaroundKind::PositiveLookbehind)
            }
            TokenKind::Exclamation => {
                self.parse_lookaround(start_span, LookaroundKind::NegativeLookbehind)
            }
            // (?<name>...) named group.
            // The first character of the name is in self.current
            _ => self.parse_named_group_body(start_span, TokenKind::GreaterThan),
        }
    }

    /// The `name>...)` tail shared by `(?<name>...)` and `(?P<name>...)`,
    /// positioned on the name's first character. `terminator` is the token
    /// that closes the name.
    #[inline(never)]
    fn parse_named_group_body(&mut self, start_span: Span, terminator: TokenKind) -> Result<Expr> {
        let name = self.parse_group_name()?;
        self.expect(terminator)?;
        let expr = self.parse_alternation()?;
        self.expect_close_paren(start_span)?;
        Ok(self.finish_named_group(name, expr))
    }

    /// `(?P<name>...)` or the `(?P=name)` backreference, positioned on the `P`.
    #[inline(never)]
    fn parse_python_group(&mut self, start_span: Span) -> Result<Expr> {
        self.advance()?;
        match &self.current.kind {
            TokenKind::LessThan => {
                self.advance()?;
                // The first character of the name is now in self.current
                self.parse_named_group_body(start_span, TokenKind::GreaterThan)
            }
            // (?P=name) - named backreference, Python style.
            TokenKind::Equals => {
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

    /// `(?'name'...)`, positioned on the opening quote.
    #[inline(never)]
    fn parse_quoted_named_group(&mut self, start_span: Span) -> Result<Expr> {
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
        Ok(self.finish_named_group(name, expr))
    }

    /// Registers a parsed named group and builds its node. Shared by every
    /// named-group spelling so they cannot drift in capture numbering.
    fn finish_named_group(&mut self, name: String, expr: Expr) -> Expr {
        let index = self.next_capture;
        self.next_capture += 1;
        self.capture_count += 1;
        self.named_groups.insert(name.clone(), index);
        group(expr, GroupKind::NamedCapturing { name, index })
    }

    /// `(?flags)` or `(?flags:...)`, positioned on the first flag character.
    #[inline(never)]
    fn parse_flag_group(&mut self, start_span: Span) -> Result<Expr> {
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
            Ok(group(expr, GroupKind::Flagged(group_flags)))
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

    /// A plain `(...)` capturing group, positioned on its first body token.
    #[inline(never)]
    fn parse_capturing_group(&mut self, start_span: Span) -> Result<Expr> {
        let index = self.next_capture;
        self.next_capture += 1;
        self.capture_count += 1;

        let expr = self.parse_alternation()?;
        self.expect_close_paren(start_span)?;

        Ok(group(expr, GroupKind::Capturing(index)))
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

/// Builds a group node. Kept out of line so the boxing adds no slots to the
/// caller's frame, which is held across the nesting recursion.
#[inline(never)]
fn group(expr: Expr, kind: GroupKind) -> Expr {
    Expr::Group(Box::new(Group { expr, kind }))
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
