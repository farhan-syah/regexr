//! Case-insensitive matching tests.
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.

// Using local mod.rs

use super::regex;

#[test]
fn test_case_insensitive_ascii() {
    let re = regex(r"(?i)hello");
    assert!(re.is_match("hello"));
    assert!(re.is_match("HELLO"));
    assert!(re.is_match("Hello"));
    assert!(re.is_match("HeLLo"));
    assert!(!re.is_match("helo"));
}

#[test]
fn test_case_insensitive_single_char() {
    let re = regex(r"(?i)a");
    assert!(re.is_match("a"));
    assert!(re.is_match("A"));
    assert!(!re.is_match("b"));
}

#[test]
fn test_case_insensitive_mixed() {
    let re = regex(r"(?i)test123");
    assert!(re.is_match("test123"));
    assert!(re.is_match("TEST123"));
    assert!(re.is_match("Test123"));
    assert!(!re.is_match("test12"));
}

#[test]
fn test_case_insensitive_unicode() {
    let re = regex(r"(?i)α");
    assert!(re.is_match("α"));
    assert!(re.is_match("Α"));
}

#[test]
fn test_case_insensitive_german() {
    let re = regex(r"(?i)straße");
    assert!(re.is_match("Straße"));
    assert!(!re.is_match("STRASSE"));
}

#[test]
fn test_case_sensitive_default() {
    let re = regex(r"Hello");
    assert!(re.is_match("Hello"));
    assert!(!re.is_match("hello"));
    assert!(!re.is_match("HELLO"));
}

/// `(?i)` over a *bracketed* class has to fold every member of every range,
/// which is the path that enumerates the case-folding tables. A `\p{L}`-style
/// test proves nothing here — Α and α are both letters, so it passes even with
/// folding entirely broken. These use ranges that only match the opposite case
/// if folding actually happened.
#[test]
fn test_case_insensitive_class_folds_greek_range() {
    let re = regex(r"(?i)[Α-Ω]+");
    assert!(re.is_match("αβγ"), "uppercase range must fold to lowercase");
    assert!(re.is_match("ΑΒΓ"));
    assert!(!re.is_match("123"));

    // Without the flag the same range must NOT match lowercase.
    assert!(!regex(r"[Α-Ω]+").is_match("αβγ"));
}

#[test]
fn test_case_insensitive_class_folds_cyrillic_range() {
    let re = regex(r"(?i)[А-Я]+");
    assert!(re.is_match("абв"));
    assert!(!regex(r"[А-Я]+").is_match("абв"));
}

/// The exact shape whose compile cost drove the case-folding rewrite.
#[test]
fn test_case_insensitive_unicode_property_class() {
    let re = regex(r"(?i)[\p{L}]+");
    assert!(re.is_match("ΑΒΓ"));
    assert!(re.is_match("αβγ"));
    assert!(re.is_match("ΑβΓ"));
    assert!(!re.is_match("123"));
}
