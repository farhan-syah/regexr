//! Unicode mode tests ((?u) flag for \w, \d, \s).
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.

use super::regex;

#[test]
fn test_unicode_mode_word_class() {
    let re_ascii = regex(r"\w+");
    assert!(re_ascii.is_match("hello"));

    let re_unicode = regex(r"(?u)\w+");
    assert!(re_unicode.is_match("hello"));
    assert!(re_unicode.is_match("αβγ"));
    assert!(re_unicode.is_match("中文"));
}

#[test]
fn test_unicode_mode_digit_class() {
    let re_ascii = regex(r"\d+");
    assert!(re_ascii.is_match("123"));

    let re_unicode = regex(r"(?u)\d+");
    assert!(re_unicode.is_match("123"));
    assert!(re_unicode.is_match("٠١٢"));
}

#[test]
fn test_unicode_mode_whitespace_class() {
    let re_ascii = regex(r"\s+");
    assert!(re_ascii.is_match(" \t\n"));

    let re_unicode = regex(r"(?u)\s+");
    assert!(re_unicode.is_match(" \t\n"));
    assert!(re_unicode.is_match("\u{00A0}"));
    assert!(re_unicode.is_match("\u{3000}"));
}

#[test]
fn test_unicode_mode_negated_classes() {
    let re_unicode = regex(r"(?u)\D+");
    assert!(re_unicode.is_match("abc"));
    assert!(re_unicode.is_match("αβγ"));
    assert!(!re_unicode.is_match("123"));
    assert!(!re_unicode.is_match("٠١٢"));

    let re_unicode_w = regex(r"(?u)\W+");
    assert!(re_unicode_w.is_match("!@#"));
    assert!(re_unicode_w.is_match(" \t\n"));
    assert!(!re_unicode_w.is_match("abc"));
    assert!(!re_unicode_w.is_match("αβγ"));
}

// Bracketed forms: `(?u)` must reach `\w`/`\d` (and negations) nested inside
// a character class exactly like it reaches the standalone escape above.

#[test]
fn test_unicode_mode_bracketed_word_class() {
    let re_unicode = regex(r"(?u)[\w]");
    assert!(re_unicode.is_match("中"));
    assert!(re_unicode.is_match("é"));
    assert!(re_unicode.is_match("a"));
    assert!(!re_unicode.is_match(" "));

    let re_ascii = regex(r"[\w]");
    assert!(re_ascii.is_match("a"));
    assert!(!re_ascii.is_match("中"));
    assert!(!re_ascii.is_match("é"));
}

#[test]
fn test_unicode_mode_bracketed_digit_class() {
    let re_unicode = regex(r"(?u)[\d]");
    assert!(re_unicode.is_match("٠")); // U+0660 Arabic-Indic zero
    assert!(re_unicode.is_match("5"));
    assert!(!re_unicode.is_match("a"));

    let re_ascii = regex(r"[\d]");
    assert!(re_ascii.is_match("5"));
    assert!(!re_ascii.is_match("٠"));
}

#[test]
fn test_unicode_mode_bracketed_negated_word_class() {
    // [^\w]: non-word chars in unicode mode; everything but ASCII
    // [A-Za-z0-9_] in non-unicode mode (so non-ASCII code points like 中
    // count as "not an ASCII word char" and match).
    let re_unicode = regex(r"(?u)[^\w]");
    assert!(!re_unicode.is_match("中"));
    assert!(re_unicode.is_match(" "));

    let re_ascii = regex(r"[^\w]");
    assert!(re_ascii.is_match("中"));
    assert!(re_ascii.is_match(" "));
    assert!(!re_ascii.is_match("a"));
}

#[test]
fn test_unicode_mode_bracketed_not_w_class() {
    // \W nested inside a class carries the same semantics as [^\w] above.
    let re_unicode = regex(r"(?u)[\W]");
    assert!(!re_unicode.is_match("中"));
    assert!(re_unicode.is_match(" "));

    let re_ascii = regex(r"[\W]");
    assert!(re_ascii.is_match("中"));
    assert!(re_ascii.is_match(" "));
    assert!(!re_ascii.is_match("a"));
}

#[test]
fn test_unicode_mode_bracketed_negated_not_w_class() {
    // [^\W]: word chars in unicode mode; ASCII word chars in non-unicode.
    let re_unicode = regex(r"(?u)[^\W]");
    assert!(re_unicode.is_match("中"));
    assert!(re_unicode.is_match("a"));
    assert!(!re_unicode.is_match(" "));

    let re_ascii = regex(r"[^\W]");
    assert!(re_ascii.is_match("a"));
    assert!(!re_ascii.is_match("中"));
    assert!(!re_ascii.is_match(" "));
}

#[test]
fn test_unicode_mode_bracketed_w_and_not_w_matches_everything() {
    // [\w\W] unions a class with its own negation, so it must match any
    // code point regardless of unicode mode.
    let re_unicode = regex(r"(?u)[\w\W]");
    assert!(re_unicode.is_match("中"));
    assert!(re_unicode.is_match("a"));
    assert!(re_unicode.is_match(" "));
    assert!(re_unicode.is_match("!"));

    let re_ascii = regex(r"[\w\W]");
    assert!(re_ascii.is_match("中"));
    assert!(re_ascii.is_match("a"));
    assert!(re_ascii.is_match(" "));
}

#[test]
fn test_unicode_mode_scoped_bracketed_word_class() {
    // `a(?u:[\w])b`: unicode mode must apply only inside the scoped group,
    // not leak past it.
    let re = regex(r"a(?u:[\w])b");
    assert!(re.is_match("a中b"));
    assert!(re.is_match("aéb"));
    assert!(re.is_match("aab"));
    assert!(!re.is_match("a b"));
}

#[test]
fn test_unicode_mode_flag_flip_bracketed_word_class() {
    // `(?u)(?-u)[\w]`: the later `(?-u)` must turn unicode mode back off
    // before `[\w]` is parsed, so the class stays ASCII-only despite the
    // leading `(?u)`.
    let re = regex(r"(?u)(?-u)[\w]");
    assert!(re.is_match("a"));
    assert!(!re.is_match("中"));
    assert!(!re.is_match("é"));
}
