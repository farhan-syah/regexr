//! `\Q…\E` — literal quoting.
//!
//! Inside the span every character stands for itself. The interesting cases are
//! the three places that normally rewrite characters before the parser sees
//! them: escapes, extended-mode trivia, and character-class syntax.

use super::regex;
use regexr::Regex;

#[test]
fn quoted_metacharacters_are_literal() {
    let re = regex(r"\Qa.b\E");
    assert!(re.is_match("a.b"));
    assert!(!re.is_match("axb"));

    for (pattern, subject) in [
        (r"\Qa+b\E", "a+b"),
        (r"\Q(x)\E", "(x)"),
        (r"\Qa|b\E", "a|b"),
        (r"\Q[a]\E", "[a]"),
        (r"\Qa*\E", "a*"),
        (r"\Q^$\E", "^$"),
        (r"\Qa{2}\E", "a{2}"),
    ] {
        assert!(
            regex(pattern).is_match(subject),
            "{pattern} should match {subject:?} literally"
        );
    }
}

#[test]
fn quoting_ends_at_the_terminator_and_normal_syntax_resumes() {
    let re = regex(r"x\Q.\Ey+");
    assert!(re.is_match("x.yyy"));
    assert!(!re.is_match("xzy"));
}

/// PCRE scans a quoted span for the two-character sequence `\E`, taking
/// backslashes literally otherwise — so this pattern is two backslashes, not
/// one escaped backslash.
#[test]
fn a_backslash_inside_a_quoted_span_is_literal() {
    let re = regex(r"\Q\\\E");
    assert!(re.is_match("\\\\"));
    assert!(!re.is_match("x"));
    assert_eq!(re.find("a\\\\b").map(|m| m.as_str()), Some("\\\\"));
}

#[test]
fn an_unterminated_quote_runs_to_the_end_of_the_pattern() {
    let re = regex(r"\Qa.b");
    assert!(re.is_match("a.b"));
    assert!(!re.is_match("axb"));
}

/// A stray `\E` is a no-op, so `\Q…\E` can be spliced in unconditionally
/// without the caller tracking whether a span is open.
#[test]
fn a_terminator_without_an_opener_is_ignored() {
    let re = regex(r"\Eabc");
    assert!(re.is_match("abc"));

    let re = regex(r"a\E\Eb");
    assert!(re.is_match("ab"));

    // A pattern of nothing but terminators is the empty pattern.
    assert!(Regex::new(r"\E").expect("valid pattern").is_match(""));
}

#[test]
fn quoting_survives_extended_mode() {
    // Extended mode strips whitespace and `#` comments — but not inside a
    // quoted span, which is the point of quoting.
    let re = regex(r"(?x) \Qa b\E ");
    assert!(re.is_match("a b"));
    assert!(!re.is_match("ab"));

    let re = regex(r"(?x) \Qa#b\E ");
    assert!(re.is_match("a#b"));
}

#[test]
fn quoting_works_inside_a_character_class() {
    // `]` and `-` lose their class meaning inside the span.
    let re = regex(r"[\Qa]b\E]");
    assert!(re.is_match("]"));
    assert!(re.is_match("a"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("c"));

    // `-` is a member, not a range operator, so `c` is not matched.
    let re = regex(r"[\Qa-c\E]");
    assert!(re.is_match("-"));
    assert!(re.is_match("a"));
    assert!(!re.is_match("b"));
}

#[test]
fn a_quantifier_after_a_quoted_span_applies_to_its_last_character() {
    let re = regex(r"^\Qab\E+$");
    assert!(re.is_match("abbb"));
    assert!(!re.is_match("abab"));
}

#[test]
fn escaped_q_is_a_literal_q_not_an_opener() {
    // `\\Q` is an escaped backslash followed by `Q`, and must not open a span.
    let re = regex(r"\\Q.");
    assert!(re.is_match("\\Qx"));
    assert_eq!(re.find("a\\Qzb").map(|m| m.as_str()), Some("\\Qz"));

    // Had `\\Q` opened a quoted span, the trailing `.` would be literal.
    assert!(!re.is_match("Q."));
}
