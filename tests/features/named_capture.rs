//! Named capture group tests.
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.

// Using local mod.rs

use super::regex;

use regexr::Regex;

#[test]
fn test_named_capture_angle_bracket() {
    let re = regex(r"(?<word>\w+)");
    let caps = re.captures("hello world").unwrap();
    assert_eq!(&caps["word"], "hello");
    assert_eq!(caps.name("word").unwrap().as_str(), "hello");
}

#[test]
fn test_named_capture_python_style() {
    let re = regex(r"(?P<word>\w+)");
    let caps = re.captures("hello world").unwrap();
    assert_eq!(&caps["word"], "hello");
    assert_eq!(caps.name("word").unwrap().as_str(), "hello");
}

#[test]
fn test_named_capture_multiple() {
    let re = regex(r"(?<first>\w+)\s+(?<second>\w+)");
    let caps = re.captures("hello world").unwrap();
    assert_eq!(&caps["first"], "hello");
    assert_eq!(&caps["second"], "world");
    assert_eq!(&caps[1], "hello");
    assert_eq!(&caps[2], "world");
}

#[test]
fn test_named_capture_mixed() {
    let re = regex(r"(\d+)-(?<name>\w+)-(\d+)");
    let caps = re.captures("123-foo-456").unwrap();
    assert_eq!(&caps[0], "123-foo-456");
    assert_eq!(&caps[1], "123");
    assert_eq!(&caps["name"], "foo");
    assert_eq!(&caps[2], "foo");
    assert_eq!(&caps[3], "456");
}

#[test]
fn test_named_capture_quote_delimited() {
    let re = regex(r"(?'word'\w+)");
    let caps = re.captures("hello world").unwrap();
    assert_eq!(&caps["word"], "hello");
    assert_eq!(caps.name("word").unwrap().as_str(), "hello");
}

#[test]
fn test_named_capture_quote_delimited_same_index_as_angle_bracket() {
    // All three spellings must be fully equivalent: same capture index, same
    // entry in the named-group map, usable by `Captures::name`.
    let angle = Regex::new(r"(a)(?<w>b)").unwrap();
    let quote = Regex::new(r"(a)(?'w'b)").unwrap();
    let python = Regex::new(r"(a)(?P<w>b)").unwrap();

    for re in [&angle, &quote, &python] {
        let caps = re.captures("ab").unwrap();
        assert_eq!(&caps["w"], "b");
        assert_eq!(&caps[2], "b");
    }
}

#[test]
fn test_capture_names() {
    let re = Regex::new(r"(?<year>\d{4})-(?<month>\d{2})-(?<day>\d{2})").unwrap();
    let names: Vec<_> = re.capture_names().collect();
    assert!(names.contains(&"year"));
    assert!(names.contains(&"month"));
    assert!(names.contains(&"day"));
    assert_eq!(names.len(), 3);
}
