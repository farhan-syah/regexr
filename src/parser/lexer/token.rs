//! Token kinds produced by the lexer.

use crate::error::Span;

/// A token in the regex pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The kind of token.
    pub kind: TokenKind,
    /// Position in the source pattern.
    pub span: Span,
}

/// The kind of token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    /// A literal character.
    Literal(char),
    /// Dot (.) - any character.
    Dot,
    /// Star (*) - zero or more.
    Star,
    /// Plus (+) - one or more.
    Plus,
    /// Question (?) - zero or one, or non-greedy modifier.
    Question,
    /// Pipe (|) - alternation.
    Pipe,
    /// Caret (^) - start anchor or class negation.
    Caret,
    /// Dollar ($) - end anchor.
    Dollar,
    /// Opening parenthesis.
    OpenParen,
    /// Closing parenthesis.
    CloseParen,
    /// Opening bracket.
    OpenBracket,
    /// Closing bracket.
    CloseBracket,
    /// Opening brace.
    OpenBrace,
    /// Closing brace.
    CloseBrace,
    /// Hyphen (-) in character class.
    Hyphen,
    /// Comma (,) in repetition.
    Comma,
    /// A digit (for repetition bounds and backrefs).
    Digit(u32),
    /// Escaped character.
    Escape(EscapeKind),
    /// Colon (:).
    Colon,
    /// Less than (<).
    LessThan,
    /// Greater than (>).
    GreaterThan,
    /// Equals (=).
    Equals,
    /// Exclamation (!).
    Exclamation,
    /// An identifier (for named groups).
    Ident(String),
    /// End of input.
    Eof,
}

/// An escape sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscapeKind {
    /// Literal escaped character (e.g., \., \\).
    Literal(char),
    /// \d - digit.
    Digit,
    /// \D - non-digit.
    NotDigit,
    /// \w - word character.
    Word,
    /// \W - non-word character.
    NotWord,
    /// \s - whitespace.
    Whitespace,
    /// \S - non-whitespace.
    NotWhitespace,
    /// \b - word boundary.
    WordBoundary,
    /// \B - non-word boundary.
    NotWordBoundary,
    /// \A - start of input.
    StartOfInput,
    /// \z - end of input.
    EndOfInput,
    /// \Z - end of input before newline.
    EndOfInputBeforeNewline,
    /// \n - newline.
    Newline,
    /// \r - carriage return.
    CarriageReturn,
    /// \t - tab.
    Tab,
    /// \f - form feed.
    FormFeed,
    /// \v - vertical tab.
    VerticalTab,
    /// \0 - null.
    Null,
    /// \xHH - hex escape.
    Hex(char),
    /// \u{HHHH} - unicode escape.
    Unicode(char),
    /// \1, \2, etc. - backreference.
    Backref(u32),
    /// \X - one extended grapheme cluster (UAX #29).
    GraphemeCluster,
    /// \p{PropertyName} - Unicode property.
    UnicodeProperty(String),
    /// \P{PropertyName} - negated Unicode property.
    NotUnicodeProperty(String),
}
