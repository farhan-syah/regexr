//! Alternation, concatenation and quantifier parsing.

use super::ast::*;
use super::lexer::TokenKind;
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result};

/// Largest `{n,m}` bound accepted, matching PCRE and Perl.
///
/// The engines expand a bounded repetition literally, so this is the ceiling on
/// how much automaton one quantifier can ask for.
pub const MAX_REPETITION: u32 = 65535;

/// Deepest nesting (groups, character classes, and inline flag scopes,
/// combined) the parser will follow before refusing the pattern.
///
/// Parsing is mutually recursive with no explicit stack — each nesting level
/// costs roughly 5 stack frames across `parse_atom` / `parse_group` /
/// `parse_alternation` / `parse_concat` / `parse_repeat` — so an unbounded
/// pattern like `"(?:".repeat(50_000)` overflows the stack, which in Rust is
/// an uncatchable SIGSEGV/abort rather than an `Err`. 250 levels costs about
/// 1,250 frames, comfortably inside the 2 MiB stack Rust gives test threads
/// even in debug builds, while still matching the depth other engines accept:
/// both the `regex` crate's `nest_limit` and PCRE2's `parens_nest_limit`
/// default to 250.
pub const DEFAULT_NEST_LIMIT: u32 = 250;

impl Parser<'_> {
    /// Parses alternation (lowest precedence): a|b|c
    pub(super) fn parse_alternation(&mut self) -> Result<Expr> {
        let mut left = self.parse_concat()?;

        if matches!(self.current.kind, TokenKind::Pipe) {
            let mut alternatives = vec![left];

            while matches!(self.current.kind, TokenKind::Pipe) {
                self.advance()?;
                alternatives.push(self.parse_concat()?);
            }

            left = Expr::Alt(alternatives);
        }

        Ok(left)
    }

    /// Parses concatenation: abc
    fn parse_concat(&mut self) -> Result<Expr> {
        let mut exprs = Vec::new();

        while !self.is_at_end() && !self.is_concat_terminator() {
            let outer_flags = self.flags;
            exprs.push(self.parse_repeat()?);

            // A bare `(?flags)` sets flags for everything that follows it, so
            // the remainder of this branch is parsed under the new flags and
            // wrapped in a group carrying them. Without this the change would
            // only reach the AST's single global flag set, and would silently
            // apply to the whole pattern instead of the part after it.
            if self.flags != outer_flags {
                // Capture before recursing: a later `(?flags)` in the rest of
                // the branch moves `self.flags` on again.
                let scoped_flags = self.flags;
                // This is the parser's one self-recursion outside the
                // group/class constructs: each `(?flags)` opens a new flag
                // scope over the rest of the branch, so a run of them
                // (`(?i)(?-i)(?i)...`) recurses one level per flag change
                // with nothing bounding it otherwise. Only this call is
                // wrapped — the recursion below `parse_alternation` already
                // counts through `parse_group`/`parse_class`, and wrapping
                // every `parse_concat` call (including once per alternative)
                // would count sideways breadth as if it were depth.
                let rest = self.with_nesting(Self::parse_concat)?;
                exprs.push(Expr::Group(Box::new(Group {
                    expr: rest,
                    kind: GroupKind::Flagged(scoped_flags),
                })));
                break;
            }
        }

        Ok(match exprs.len() {
            0 => Expr::Empty,
            1 => exprs.pop().unwrap(),
            _ => Expr::Concat(exprs),
        })
    }

    /// Returns true if the current token terminates concatenation.
    fn is_concat_terminator(&self) -> bool {
        matches!(
            self.current.kind,
            TokenKind::Pipe | TokenKind::CloseParen | TokenKind::Eof
        )
    }

    /// Parses repetition: a*, a+, a?, a{n,m}
    fn parse_repeat(&mut self) -> Result<Expr> {
        let expr = self.parse_atom()?;
        self.parse_quantifier(expr)
    }

    /// Parses a quantifier if present.
    fn parse_quantifier(&mut self, expr: Expr) -> Result<Expr> {
        let (min, max) = match &self.current.kind {
            TokenKind::Star => {
                self.advance()?;
                (0, None)
            }
            TokenKind::Plus => {
                self.advance()?;
                (1, None)
            }
            TokenKind::Question => {
                self.advance()?;
                (0, Some(1))
            }
            TokenKind::OpenBrace => {
                self.advance()?;
                let (min, max) = self.parse_repetition_range()?;
                self.expect(TokenKind::CloseBrace)?;
                (min, max)
            }
            _ => return Ok(expr),
        };

        // Check for non-greedy modifier (? after quantifier)
        let greedy = if matches!(self.current.kind, TokenKind::Question) {
            self.advance()?;
            false
        } else {
            true
        };

        // Check for a possessive suffix (+ directly after a quantifier or its
        // lazy `?` modifier): a*+, a++, a?+, a{n,m}+. This must be checked
        // BEFORE the nested-quantifier check below, since a possessive `+`
        // would otherwise be misreported as a nested quantifier. Note that
        // greedy was just determined above: a leading `?` was already
        // consumed as the lazy modifier, so `a*+` and `a*?+` both land here
        // with `Plus` as the current token, while `a**` and `a*{2}` do not
        // and correctly fall through to the nested-quantifier check.
        if matches!(self.current.kind, TokenKind::Plus) {
            let span = self.current.span;
            return Err(Error::with_span(
                ErrorKind::PossessiveQuantifier,
                self.pattern,
                span,
            ));
        }

        // Check for nested quantifier (*, {n} after a quantifier). `+` is
        // handled above as the possessive suffix, so it never reaches here.
        // Note: This comes AFTER handling non-greedy ?, so *? is allowed
        if matches!(
            self.current.kind,
            TokenKind::Star | TokenKind::Question | TokenKind::OpenBrace
        ) {
            return Err(Error::with_span(
                ErrorKind::NestedQuantifier,
                self.pattern,
                self.current.span,
            ));
        }

        Ok(Expr::Repeat(Box::new(Repeat::new(expr, min, max, greedy))))
    }

    /// Rejects a repetition bound the engines would have to expand.
    ///
    /// Every engine here compiles `{n,m}` by emitting the subexpression `m`
    /// times, so the bound is a direct multiplier on compile time and on the
    /// size of the automaton. Left uncapped, `\w{200000,}` spends tens of
    /// seconds inside `Regex::new` — a pattern that is a denial of service
    /// rather than a mistake, and one a caller passing a user-supplied pattern
    /// cannot see coming. PCRE and Perl draw the same line at 65535.
    fn check_repetition_bound(&self, bound: u32) -> Result<()> {
        if bound > MAX_REPETITION {
            return Err(Error::with_span(
                ErrorKind::RepetitionTooLarge {
                    bound,
                    limit: MAX_REPETITION,
                },
                self.pattern,
                self.current.span,
            ));
        }
        Ok(())
    }

    /// Parses repetition range: {n}, {n,}, {n,m}
    fn parse_repetition_range(&mut self) -> Result<(u32, Option<u32>)> {
        // Parse first number (min)
        let mut min = 0u32;
        while let TokenKind::Digit(d) = self.current.kind {
            min = min.saturating_mul(10).saturating_add(d);
            self.advance()?;
        }

        self.check_repetition_bound(min)?;

        // Check for {n}
        if matches!(self.current.kind, TokenKind::CloseBrace) {
            return Ok((min, Some(min)));
        }

        // Expect comma
        if !matches!(self.current.kind, TokenKind::Comma) {
            return Err(Error::with_span(
                ErrorKind::InvalidRepetition,
                self.pattern,
                self.current.span,
            ));
        }
        self.advance()?; // consume comma

        // Check for {n,}
        if matches!(self.current.kind, TokenKind::CloseBrace) {
            return Ok((min, None));
        }

        // Parse second number (max)
        let mut max = 0u32;
        let mut has_max = false;
        while let TokenKind::Digit(d) = self.current.kind {
            has_max = true;
            max = max.saturating_mul(10).saturating_add(d);
            self.advance()?;
        }

        self.check_repetition_bound(max)?;

        if !has_max {
            return Err(Error::with_span(
                ErrorKind::InvalidRepetition,
                self.pattern,
                self.current.span,
            ));
        }

        if max < min {
            return Err(Error::with_span(
                ErrorKind::InvalidRepetition,
                self.pattern,
                self.current.span,
            ));
        }

        Ok((min, Some(max)))
    }
}
