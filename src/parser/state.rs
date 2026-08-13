//! Parser state and token-stream primitives.

use super::alternation::DEFAULT_NEST_LIMIT;
use super::ast::{Ast, Flags};
use super::lexer::{Lexer, Token, TokenKind};
use crate::error::{Error, ErrorKind, Result};

/// Parses a regex pattern into an AST, under the default nesting limit.
pub fn parse(pattern: &str) -> Result<Ast> {
    parse_with_nest_limit(pattern, DEFAULT_NEST_LIMIT)
}

/// [`parse`] under a caller-chosen ceiling on how deeply groups, character
/// classes, and inline flag scopes may nest.
///
/// See [`crate::RegexBuilder::nest_limit`] for what the number means and when
/// raising it is the right call.
pub fn parse_with_nest_limit(pattern: &str, nest_limit: u32) -> Result<Ast> {
    let mut parser = Parser::with_nest_limit(pattern, nest_limit)?;
    parser.parse()
}

/// The regex parser.
pub struct Parser<'a> {
    pub(super) lexer: Lexer<'a>,
    /// Current token.
    pub(super) current: Token,
    /// Pattern being parsed.
    pub(super) pattern: &'a str,
    /// Next capture group index.
    pub(super) next_capture: u32,
    /// Total number of capture groups.
    pub(super) capture_count: u32,
    /// Current flags.
    pub(super) flags: Flags,
    /// Name -> capture index, populated as named groups are parsed. Backed
    /// by a single left-to-right pass, so it only ever holds names already
    /// seen: a named backreference (`\k<name>`, `(?P=name)`, ...) resolves
    /// against whatever this map holds at the point it's encountered — i.e.
    /// only a name defined earlier in the pattern is visible. A duplicate
    /// name simply overwrites its earlier entry, matching how
    /// `HirProps::named_groups` (built later from the same left-to-right AST
    /// walk) resolves duplicates too.
    pub(super) named_groups: std::collections::HashMap<String, u32>,
    /// Current nesting depth: groups, character classes, and inline flag
    /// scopes all share this one counter, since what it bounds is combined
    /// stack usage, not any one construct. Maintained exclusively through
    /// [`Self::with_nesting`], which increments on entry and decrements on
    /// every exit path (`Ok` or `Err`), so it can never be left over-counted
    /// by an early `?` return.
    pub(super) depth: u32,
    /// Deepest nesting [`Self::depth`] may reach before parsing refuses the
    /// pattern; see [`super::alternation::DEFAULT_NEST_LIMIT`].
    pub(super) nest_limit: u32,
}

impl<'a> Parser<'a> {
    /// Creates a new parser, lexing the first token, under the default
    /// nesting limit.
    ///
    /// Lexing the first token can fail exactly like lexing any other token, so
    /// this is fallible. Substituting a benign token for a lexer error here
    /// would turn a malformed pattern into an empty one, and an empty pattern
    /// matches at every position of every input.
    pub fn new(pattern: &'a str) -> Result<Self> {
        Self::with_nest_limit(pattern, DEFAULT_NEST_LIMIT)
    }

    /// [`Self::new`] under a caller-chosen nesting limit.
    pub fn with_nest_limit(pattern: &'a str, nest_limit: u32) -> Result<Self> {
        let mut lexer = Lexer::new(pattern);
        let current = lexer.next_token()?;

        Ok(Self {
            lexer,
            current,
            pattern,
            next_capture: 1,
            capture_count: 0,
            flags: Flags::default(),
            named_groups: std::collections::HashMap::new(),
            depth: 0,
            nest_limit,
        })
    }

    /// Runs `f` with the nesting-depth counter incremented by one level,
    /// decrementing it again once `f` returns — whether it returned `Ok` or
    /// `Err`. This is the only way [`Self::depth`] is ever mutated, which is
    /// what makes it decrement-safe: every early `?` return inside `f` still
    /// unwinds through this function's own decrement before propagating, so a
    /// rejected branch can never leave a later sibling at the same depth
    /// wrongly counted as deeper than it is.
    ///
    /// Every call site is a genuine nesting boundary — entering a group,
    /// entering a character class, or entering a new inline-flag scope — so
    /// one call here is one level, matching what a user means by "nesting
    /// depth" rather than counting call-stack frames.
    pub(super) fn with_nesting<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.depth += 1;
        if self.depth > self.nest_limit {
            let depth = self.depth;
            let limit = self.nest_limit;
            let span = self.current.span;
            self.depth -= 1;
            return Err(Error::with_span(
                ErrorKind::NestingTooDeep { depth, limit },
                self.pattern,
                span,
            ));
        }
        let result = f(self);
        self.depth -= 1;
        result
    }

    /// Parses the pattern.
    pub fn parse(&mut self) -> Result<Ast> {
        let expr = self.parse_alternation()?;

        if !self.is_at_end() {
            return Err(Error::with_span(
                ErrorKind::UnexpectedChar(self.current_char().unwrap_or('?')),
                self.pattern,
                self.current.span,
            ));
        }

        Ok(Ast {
            expr,
            flags: self.flags,
        })
    }

    /// Advances to the next token.
    pub(super) fn advance(&mut self) -> Result<Token> {
        let prev = std::mem::replace(&mut self.current, self.lexer.next_token()?);
        Ok(prev)
    }

    /// Returns true if we're at the end of input.
    pub(super) fn is_at_end(&self) -> bool {
        matches!(self.current.kind, TokenKind::Eof)
    }

    /// Returns the current character if it's a literal.
    pub(super) fn current_char(&self) -> Option<char> {
        match &self.current.kind {
            TokenKind::Literal(c) => Some(*c),
            _ => None,
        }
    }

    /// Checks if the current token matches the given kind.
    pub(super) fn check(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.current.kind) == std::mem::discriminant(kind)
    }

    /// Consumes the current token if it matches, otherwise returns an error.
    pub(super) fn expect(&mut self, kind: TokenKind) -> Result<Token> {
        if self.check(&kind) {
            self.advance()
        } else {
            Err(Error::with_span(
                ErrorKind::UnexpectedChar(self.current_char().unwrap_or('?')),
                self.pattern,
                self.current.span,
            ))
        }
    }

    /// Restores a saved flag set, keeping the lexer's extended-mode state in
    /// sync. Extended mode is consumed by the lexer rather than the HIR builder
    /// — whitespace and comments are removed before a token exists — so the two
    /// must never drift apart.
    pub(super) fn restore_flags(&mut self, flags: Flags) {
        self.flags = flags;
        self.lexer.set_extended(flags.extended);
    }

    /// Returns the source text of the current token.
    pub(super) fn current_text(&self) -> &str {
        self.pattern
            .get(self.current.span.start..self.current.span.end)
            .unwrap_or_default()
    }
}
