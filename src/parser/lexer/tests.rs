//! Lexer unit tests.

use super::*;
use crate::error::{ErrorKind, Result};

fn lex_all(src: &str) -> Result<Vec<TokenKind>> {
    let mut lexer = Lexer::new(src);
    let mut tokens = Vec::new();
    loop {
        let tok = lexer.next_token()?;
        if tok.kind == TokenKind::Eof {
            break;
        }
        tokens.push(tok.kind);
    }
    Ok(tokens)
}

#[test]
fn test_simple_tokens() {
    let tokens = lex_all("a.b*c+d?").unwrap();
    assert_eq!(
        tokens,
        vec![
            TokenKind::Literal('a'),
            TokenKind::Dot,
            TokenKind::Literal('b'),
            TokenKind::Star,
            TokenKind::Literal('c'),
            TokenKind::Plus,
            TokenKind::Literal('d'),
            TokenKind::Question,
        ]
    );
}

#[test]
fn test_escapes() {
    let tokens = lex_all(r"\d\w\s\n\t").unwrap();
    assert_eq!(
        tokens,
        vec![
            TokenKind::Escape(EscapeKind::Digit),
            TokenKind::Escape(EscapeKind::Word),
            TokenKind::Escape(EscapeKind::Whitespace),
            TokenKind::Escape(EscapeKind::Newline),
            TokenKind::Escape(EscapeKind::Tab),
        ]
    );
}

#[test]
fn test_hex_escape() {
    let tokens = lex_all(r"\x41").unwrap();
    assert_eq!(tokens, vec![TokenKind::Escape(EscapeKind::Hex('A'))]);
}

#[test]
fn test_unicode_escape() {
    let tokens = lex_all(r"\u{1F600}").unwrap();
    assert_eq!(tokens, vec![TokenKind::Escape(EscapeKind::Unicode('😀'))]);
}

#[test]
fn test_backref() {
    let tokens = lex_all(r"\1\12").unwrap();
    assert_eq!(
        tokens,
        vec![
            TokenKind::Escape(EscapeKind::Backref(1)),
            TokenKind::Escape(EscapeKind::Backref(12)),
        ]
    );
}

#[test]
fn test_invalid_escape() {
    let err = lex_all(r"\q").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidEscape('q')));
}

#[test]
fn test_unicode_property() {
    let tokens = lex_all(r"\p{Letter}").unwrap();
    assert_eq!(
        tokens,
        vec![TokenKind::Escape(EscapeKind::UnicodeProperty(
            "Letter".to_string()
        ))]
    );
}

#[test]
fn test_negated_unicode_property() {
    let tokens = lex_all(r"\P{Number}").unwrap();
    assert_eq!(
        tokens,
        vec![TokenKind::Escape(EscapeKind::NotUnicodeProperty(
            "Number".to_string()
        ))]
    );
}

#[test]
fn test_unicode_property_short() {
    let tokens = lex_all(r"\p{L}").unwrap();
    assert_eq!(
        tokens,
        vec![TokenKind::Escape(EscapeKind::UnicodeProperty(
            "L".to_string()
        ))]
    );
}

#[test]
fn test_unicode_property_shorthand() {
    // Single-letter shorthand syntax
    let tokens = lex_all(r"\pL").unwrap();
    assert_eq!(
        tokens,
        vec![TokenKind::Escape(EscapeKind::UnicodeProperty(
            "L".to_string()
        ))]
    );

    // Negated shorthand
    let tokens = lex_all(r"\PN").unwrap();
    assert_eq!(
        tokens,
        vec![TokenKind::Escape(EscapeKind::NotUnicodeProperty(
            "N".to_string()
        ))]
    );

    // Lowercase also works
    let tokens = lex_all(r"\ps").unwrap();
    assert_eq!(
        tokens,
        vec![TokenKind::Escape(EscapeKind::UnicodeProperty(
            "s".to_string()
        ))]
    );
}

#[test]
fn test_invalid_unicode_property_no_char() {
    // \p followed by non-alphabetic, non-brace should fail
    let err = lex_all(r"\p1").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidUnicodeProperty));
}

#[test]
fn test_invalid_unicode_property_empty() {
    let err = lex_all(r"\p{}").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidUnicodeProperty));
}

#[test]
fn test_alert_and_escape_character_escapes() {
    let tokens = lex_all(r"\a\e").unwrap();
    assert_eq!(
        tokens,
        vec![
            TokenKind::Escape(EscapeKind::Literal('\u{07}')),
            TokenKind::Escape(EscapeKind::Literal('\u{1b}')),
        ]
    );
}

#[test]
fn test_escaped_ascii_punctuation_is_literal() {
    for c in ' '..='~' {
        if c.is_ascii_alphanumeric() {
            continue;
        }
        let src = format!("\\{c}");
        let tokens = lex_all(&src).unwrap_or_else(|e| panic!("{src:?} should lex: {e}"));
        assert_eq!(tokens, vec![TokenKind::Escape(EscapeKind::Literal(c))]);
    }
}

#[test]
fn test_unassigned_letter_escape_is_invalid() {
    let err = lex_all(r"\j").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidEscape('j')));
}

#[test]
fn test_escaped_non_ascii_is_invalid() {
    // Non-ASCII needs no escaping, and accepting `\é` would make a typo look
    // deliberate.
    let err = lex_all(r"\é").unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidEscape('é')));
}

#[test]
fn test_extended_mode_strips_whitespace_and_comments() {
    let mut lexer = Lexer::new("a b # comment\nc");
    lexer.set_extended(true);
    let mut kinds = Vec::new();
    loop {
        let tok = lexer.next_token().unwrap();
        if tok.kind == TokenKind::Eof {
            break;
        }
        kinds.push(tok.kind);
    }
    assert_eq!(
        kinds,
        vec![
            TokenKind::Literal('a'),
            TokenKind::Literal('b'),
            TokenKind::Literal('c'),
        ]
    );
}
