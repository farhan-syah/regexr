//! Capture extraction for deterministic (one-pass) patterns.
//!
//! These patterns are one-pass: from any point at most one transition can
//! consume a given byte, so capture slots are written during a single scan
//! instead of a second PikeVM pass. The slots must be indistinguishable from
//! what the general path produces, so every case here is asserted through the
//! public API.

use super::regex;

#[test]
fn test_log_line_captures() {
    let re = regex(r"(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2}) \[(\w+)\] (.+)");
    let line = "2024-05-17 08:30:00 [ERROR] disk quota exceeded on /dev/sda1";
    let caps = re.captures(line).expect("log line should match");

    assert_eq!(caps.len(), 5);
    assert_eq!(caps.get(0).unwrap().as_str(), line);
    assert_eq!(caps.get(1).unwrap().as_str(), "2024-05-17");
    assert_eq!(caps.get(2).unwrap().as_str(), "08:30:00");
    assert_eq!(caps.get(3).unwrap().as_str(), "ERROR");
    assert_eq!(
        caps.get(4).unwrap().as_str(),
        "disk quota exceeded on /dev/sda1"
    );
}

#[test]
fn test_log_line_captures_offsets() {
    let re = regex(r"(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2}) \[(\w+)\] (.+)");
    let line = "2024-05-17 08:30:00 [WARN] retrying";
    let caps = re.captures(line).expect("log line should match");

    let date = caps.get(1).unwrap();
    assert_eq!((date.start(), date.end()), (0, 10));
    let time = caps.get(2).unwrap();
    assert_eq!((time.start(), time.end()), (11, 19));
    let level = caps.get(3).unwrap();
    assert_eq!((level.start(), level.end()), (21, 25));
}

#[test]
fn test_captures_agree_with_find() {
    let re = regex(r"(\d+)-(\d+)");
    let text = "prefix 12-345 suffix";
    let found = re.find(text).expect("should match");
    let caps = re.captures(text).expect("should match");

    assert_eq!(caps.get(0).unwrap().as_str(), found.as_str());
    assert_eq!(caps.get(0).unwrap().start(), found.start());
    assert_eq!(caps.get(0).unwrap().end(), found.end());
    assert_eq!(caps.get(1).unwrap().as_str(), "12");
    assert_eq!(caps.get(2).unwrap().as_str(), "345");
}

#[test]
fn test_nested_groups() {
    let re = regex(r"((\w+)-(\w+))");
    let caps = re.captures("left-right").expect("should match");

    assert_eq!(caps.len(), 4);
    assert_eq!(caps.get(1).unwrap().as_str(), "left-right");
    assert_eq!(caps.get(2).unwrap().as_str(), "left");
    assert_eq!(caps.get(3).unwrap().as_str(), "right");
}

#[test]
fn test_group_inside_repetition_reports_last_iteration() {
    let re = regex(r"(?:(\w)-)+");
    let caps = re.captures("a-b-c-").expect("should match");

    assert_eq!(caps.get(0).unwrap().as_str(), "a-b-c-");
    assert_eq!(caps.get(1).unwrap().as_str(), "c");
    assert_eq!(caps.get(1).unwrap().start(), 4);
}

#[test]
fn test_nested_group_inside_repetition() {
    let re = regex(r"(?:\[(\d+):(\w+)\])+");
    let caps = re.captures("[1:one][22:two]").expect("should match");

    assert_eq!(caps.get(0).unwrap().as_str(), "[1:one][22:two]");
    assert_eq!(caps.get(1).unwrap().as_str(), "22");
    assert_eq!(caps.get(2).unwrap().as_str(), "two");
}

#[test]
fn test_optional_group_that_does_not_participate() {
    let re = regex(r"(-)?(\d+)");
    let caps = re.captures("42").expect("should match");

    assert_eq!(caps.len(), 3);
    assert!(
        caps.get(1).is_none(),
        "a group the match never entered stays unset"
    );
    assert_eq!(caps.get(2).unwrap().as_str(), "42");
}

#[test]
fn test_optional_group_that_does_participate() {
    let re = regex(r"(-)?(\d+)");
    let caps = re.captures("-42").expect("should match");

    assert_eq!(caps.get(1).unwrap().as_str(), "-");
    assert_eq!(caps.get(2).unwrap().as_str(), "42");
}

#[test]
fn test_leading_group_matches_empty() {
    let re = regex(r"(\d*)([a-z]+)");
    let caps = re.captures("abc").expect("should match");

    let empty = caps.get(1).expect("an empty match still participates");
    assert_eq!(empty.as_str(), "");
    assert_eq!((empty.start(), empty.end()), (0, 0));
    assert_eq!(caps.get(2).unwrap().as_str(), "abc");
}

#[test]
fn test_leading_group_matches_empty_unanchored() {
    let re = regex(r"(\d*)([a-z]+)");
    let caps = re.captures("!!abc").expect("should match");

    let empty = caps.get(1).expect("an empty match still participates");
    assert_eq!((empty.start(), empty.end()), (2, 2));
    assert_eq!(caps.get(2).unwrap().as_str(), "abc");
}

#[test]
fn test_greedy_trailing_group() {
    let re = regex(r"(\w+)=(.+)");
    let caps = re.captures("key=a=b=c").expect("should match");

    assert_eq!(caps.get(1).unwrap().as_str(), "key");
    assert_eq!(caps.get(2).unwrap().as_str(), "a=b=c");
}

#[test]
fn test_no_match_yields_no_captures() {
    let re = regex(r"(\d{4}-\d{2}-\d{2}) (\w+)");
    assert!(re.captures("no timestamp here").is_none());
}

#[test]
fn test_repeated_captures_over_iterator() {
    let re = regex(r"(\w+)@(\w+)");
    let text = "ann@one bob@two";
    let pairs: Vec<(String, String)> = re
        .captures_iter(text)
        .map(|caps| {
            (
                caps.get(1).unwrap().as_str().to_string(),
                caps.get(2).unwrap().as_str().to_string(),
            )
        })
        .collect();

    assert_eq!(
        pairs,
        vec![
            ("ann".to_string(), "one".to_string()),
            ("bob".to_string(), "two".to_string()),
        ]
    );
}
