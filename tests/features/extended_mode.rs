//! Extended mode (`x`): unescaped whitespace and `#` comments are not part of
//! the pattern.
//!
//! Whitespace is stripped before a token exists, so these tests cover the
//! places where that is easy to get wrong: inside character classes (where
//! whitespace stays literal), around escapes, and at the boundary of a scoped
//! `(?x:…)` group.

use super::regex;
use regexr::Regex;

#[test]
fn extended_mode_ignores_whitespace() {
    let re = regex(r"(?x) a b c ");
    assert!(re.is_match("abc"));
    assert!(!re.is_match("a b c"));
}

#[test]
fn extended_mode_ignores_comments() {
    let re = regex("(?x) a # trailing comment\n b");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("a # trailing comment b"));
}

#[test]
fn extended_mode_comment_runs_to_end_of_pattern() {
    let re = regex("(?x) ab # no newline after this");
    assert!(re.is_match("ab"));
}

#[test]
fn extended_mode_line_comment_and_inline_comment_coexist() {
    // `(?#...)` is a whole-construct comment recognized independently of
    // extended mode, while a bare `#` is still the `x`-mode line-comment
    // starter. Both forms can appear in the same pattern without either one
    // interfering with the other.
    let re = regex("(?x)a(?#note)b # trailing line comment");
    assert!(re.is_match("ab"));
}

#[test]
fn extended_mode_escaped_whitespace_is_literal() {
    let re = regex(r"(?x) a \  b");
    assert!(re.is_match("a b"));
    assert!(!re.is_match("ab"));
}

#[test]
fn extended_mode_escaped_hash_is_literal() {
    let re = regex(r"(?x) a \# b");
    assert!(re.is_match("a#b"));
}

/// Every engine that implements this mode keeps whitespace literal inside a
/// character class, because a class is a set of characters rather than a
/// sequence of pattern elements.
#[test]
fn extended_mode_keeps_whitespace_literal_in_a_class() {
    let re = regex(r"(?x) [a b] +");
    assert!(re.is_match(" "));
    assert!(re.is_match("a"));
    assert!(!re.is_match("c"));
}

#[test]
fn extended_mode_applies_to_quantifiers_and_groups() {
    let re = regex(
        r"(?x)
        ^ (\d{4})   # year
          - (\d{2}) # month
          - (\d{2}) # day
        $",
    );
    let caps = re.captures("2026-08-07").expect("should match");
    assert_eq!(caps.get(1).map(|m| m.as_str()), Some("2026"));
    assert_eq!(caps.get(2).map(|m| m.as_str()), Some("08"));
    assert_eq!(caps.get(3).map(|m| m.as_str()), Some("07"));
    assert!(!re.is_match("2026 - 08 - 07"));
}

/// A scoped `(?x:…)` must not leak past its closing paren — including onto the
/// whitespace immediately after it, which the lexer sees while consuming `)`.
#[test]
fn scoped_extended_mode_does_not_leak_past_the_group() {
    let re = regex(r"(?x:a b) c");
    assert!(re.is_match("ab c"));
    assert!(!re.is_match("abc"));
}

#[test]
fn extended_mode_can_be_turned_off_again() {
    let re = regex(r"(?x)a b(?-x) c d");
    assert!(re.is_match("ab c d"));
    assert!(!re.is_match("abcd"));
}

#[test]
fn whitespace_is_literal_without_extended_mode() {
    let re = regex(r"a b");
    assert!(re.is_match("a b"));
    assert!(!re.is_match("ab"));
}

#[test]
fn extended_mode_pattern_of_only_trivia_is_the_empty_pattern() {
    let re = Regex::new("(?x)   # nothing but a comment\n   ").expect("valid pattern");
    assert!(re.is_match(""));
}
