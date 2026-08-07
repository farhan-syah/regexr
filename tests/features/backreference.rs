//! Backreference tests (\1, \2, etc.).
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.
//! Backreferences require PikeVM fallback for JIT (DFA can't handle them).

// Using local mod.rs

use super::regex;

use regexr::Regex;

#[test]
fn test_backref_basic() {
    let re = regex(r"(a)\1");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));
    assert!(!re.is_match("a"));
}

#[test]
fn test_backref_word() {
    let re = regex(r"(\w+)\s+\1");
    assert!(re.is_match("hello hello"));
    assert!(re.is_match("the the"));
    assert!(!re.is_match("hello world"));
}

#[test]
fn test_backref_multiple_groups() {
    let re = regex(r"(a)(b)\2\1");
    assert!(re.is_match("abba"));
    assert!(!re.is_match("abab"));
    assert!(!re.is_match("aabb"));
}

#[test]
fn test_backref_nested_groups() {
    let re = regex(r"((a)b)\1");
    assert!(re.is_match("abab"));
    assert!(!re.is_match("abaa"));
}

#[test]
fn test_backref_with_quantifier() {
    let re = regex(r"(a+)\1");
    assert!(re.is_match("aa"));
    assert!(re.is_match("aaaa"));
    assert!(re.is_match("aaaaaa"));
    assert!(re.is_match("aaa"));

    let m = re.find("aaa").unwrap();
    assert_eq!(m.as_str(), "aa");
}

#[test]
fn test_backref_empty_capture() {
    let re = regex(r"(a?)\1");
    assert!(re.is_match(""));
    assert!(re.is_match("aa"));
}

#[test]
fn test_backref_in_alternation() {
    let re = regex(r"(a)\1|bb");
    assert!(re.is_match("aa"));
    assert!(re.is_match("bb"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_backref_find() {
    let re = regex(r"(\w)\1");
    let m = re.find("abccde").unwrap();
    assert_eq!(m.as_str(), "cc");
    assert_eq!(m.start(), 2);
}

#[test]
fn test_backref_captures() {
    let re = regex(r"(\w+) and \1");
    let caps = re.captures("cats and cats").unwrap();
    assert_eq!(&caps[0], "cats and cats");
    assert_eq!(&caps[1], "cats");
}

#[test]
fn test_named_backref_angle_bracket() {
    let re = regex(r"(?<w>a)\k<w>");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_named_backref_brace() {
    let re = regex(r"(?<w>a)\k{w}");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_named_backref_quote() {
    let re = regex(r"(?<w>a)\k'w'");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_named_backref_python_style() {
    let re = regex(r"(?P<w>a)(?P=w)");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));
}

/// The four spellings, and the three named-group spellings, all interoperate
/// — a backreference written in one spelling can refer to a group opened
/// with any other.
#[test]
fn test_named_backref_spellings_interoperate() {
    let re = regex(r"(?'w'a)\k<w>");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));

    let re = regex(r"(?P<w>a)\k{w}");
    assert!(re.is_match("aa"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_named_backref_usable_via_capture_api() {
    let re = regex(r"(?<w>a)\k<w>");
    let caps = re.captures("aa").unwrap();
    assert_eq!(caps.name("w").unwrap().as_str(), "a");
    assert_eq!(&caps[1], "a");
}

#[test]
fn test_named_backref_unknown_name_rejected() {
    assert!(Regex::new(r"\k<nosuch>").is_err());
    assert!(Regex::new(r"(?P=nosuch)").is_err());
}

#[test]
fn test_backref_invalid_group() {
    let result = Regex::new(r"\1");
    assert!(result.is_err());

    let result = Regex::new(r"(a)\2");
    assert!(result.is_err());

    let result = Regex::new(r"(a)(b)\3");
    assert!(result.is_err());

    let result = Regex::new(r"(a)\1");
    assert!(result.is_ok());

    let result = Regex::new(r"(a)(b)\1\2");
    assert!(result.is_ok());
}
