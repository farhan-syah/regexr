//! Atom and escape-sequence parsing.

use super::ast::*;
use super::lexer::{EscapeKind, TokenKind};
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result};

impl Parser<'_> {
    /// Parses an atom (the smallest unit).
    pub(super) fn parse_atom(&mut self) -> Result<Expr> {
        match &self.current.kind {
            TokenKind::Literal(c) => {
                let c = *c;
                self.advance()?;
                Ok(Expr::Literal(c))
            }
            TokenKind::Dot => {
                self.advance()?;
                Ok(Expr::Dot)
            }
            TokenKind::Caret => {
                self.advance()?;
                let anchor = if self.flags.multi_line {
                    Anchor::StartOfLine
                } else {
                    Anchor::StartOfString
                };
                Ok(Expr::Anchor(anchor))
            }
            TokenKind::Dollar => {
                self.advance()?;
                let anchor = if self.flags.multi_line {
                    Anchor::EndOfLine
                } else {
                    Anchor::EndOfString
                };
                Ok(Expr::Anchor(anchor))
            }
            TokenKind::OpenParen => self.parse_group(),
            TokenKind::OpenBracket => self.parse_class(),
            TokenKind::Escape(esc) => self.parse_escape(esc.clone()),
            TokenKind::Digit(d) => {
                let c = char::from_digit(*d, 10).unwrap();
                self.advance()?;
                Ok(Expr::Literal(c))
            }
            // These tokens are only special inside (?...) constructs,
            // but as regular atoms they are literal characters
            TokenKind::Equals => {
                self.advance()?;
                Ok(Expr::Literal('='))
            }
            TokenKind::Exclamation => {
                self.advance()?;
                Ok(Expr::Literal('!'))
            }
            TokenKind::LessThan => {
                self.advance()?;
                Ok(Expr::Literal('<'))
            }
            TokenKind::GreaterThan => {
                self.advance()?;
                Ok(Expr::Literal('>'))
            }
            TokenKind::Colon => {
                self.advance()?;
                Ok(Expr::Literal(':'))
            }
            TokenKind::Comma => {
                self.advance()?;
                Ok(Expr::Literal(','))
            }
            TokenKind::CloseParen => Err(Error::with_span(
                ErrorKind::UnmatchedCloseParen,
                self.pattern,
                self.current.span,
            )),
            TokenKind::Star | TokenKind::Plus | TokenKind::Question => Err(Error::with_span(
                ErrorKind::RepetitionOnNothing,
                self.pattern,
                self.current.span,
            )),
            _ => Err(Error::with_span(
                ErrorKind::UnexpectedChar(self.current_char().unwrap_or('?')),
                self.pattern,
                self.current.span,
            )),
        }
    }

    /// Parses an escape sequence.
    fn parse_escape(&mut self, esc: EscapeKind) -> Result<Expr> {
        self.advance()?;

        match esc {
            EscapeKind::Literal(c) => Ok(Expr::Literal(c)),
            EscapeKind::Newline => Ok(Expr::Literal('\n')),
            EscapeKind::CarriageReturn => Ok(Expr::Literal('\r')),
            EscapeKind::Tab => Ok(Expr::Literal('\t')),
            EscapeKind::FormFeed => Ok(Expr::Literal('\x0C')),
            EscapeKind::VerticalTab => Ok(Expr::Literal('\x0B')),
            EscapeKind::Null => Ok(Expr::Literal('\0')),
            EscapeKind::Hex(c) | EscapeKind::Unicode(c) => Ok(Expr::Literal(c)),

            EscapeKind::Digit => Ok(Expr::PerlClass(PerlClassKind::Digit)),
            EscapeKind::NotDigit => Ok(Expr::PerlClass(PerlClassKind::NotDigit)),
            EscapeKind::Word => Ok(Expr::PerlClass(PerlClassKind::Word)),
            EscapeKind::NotWord => Ok(Expr::PerlClass(PerlClassKind::NotWord)),
            EscapeKind::Whitespace => Ok(Expr::PerlClass(PerlClassKind::Whitespace)),
            EscapeKind::NotWhitespace => Ok(Expr::PerlClass(PerlClassKind::NotWhitespace)),

            EscapeKind::WordBoundary => Ok(Expr::Anchor(Anchor::WordBoundary)),
            EscapeKind::NotWordBoundary => Ok(Expr::Anchor(Anchor::NotWordBoundary)),
            EscapeKind::StartOfInput => Ok(Expr::Anchor(Anchor::StartOfInput)),
            EscapeKind::EndOfInput => Ok(Expr::Anchor(Anchor::EndOfInput)),
            EscapeKind::EndOfInputBeforeNewline => {
                Ok(Expr::Anchor(Anchor::EndOfInputBeforeNewline))
            }

            EscapeKind::Backref(n) => Ok(Expr::Backref(n)),

            EscapeKind::GraphemeCluster => Ok(Expr::GraphemeCluster),

            EscapeKind::UnicodeProperty(name) => Ok(Expr::UnicodeProperty {
                name,
                negated: false,
            }),
            EscapeKind::NotUnicodeProperty(name) => Ok(Expr::UnicodeProperty {
                name,
                negated: true,
            }),
        }
    }
}
