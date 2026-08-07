//! Alternation, concatenation and quantifier parsing.

use super::ast::*;
use super::lexer::TokenKind;
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result};

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
                let rest = self.parse_concat()?;
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

    /// Parses repetition range: {n}, {n,}, {n,m}
    fn parse_repetition_range(&mut self) -> Result<(u32, Option<u32>)> {
        // Parse first number (min)
        let mut min = 0u32;
        while let TokenKind::Digit(d) = self.current.kind {
            min = min.saturating_mul(10).saturating_add(d);
            self.advance()?;
        }

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
