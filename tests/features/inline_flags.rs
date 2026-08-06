//! Inline flag groups: `(?i)`, `(?-i)`, `(?im-sx)`, `(?i:…)`.
//!
//! Setting a flag is covered per-flag elsewhere; this file covers the *group
//! syntax* itself — negation and scoping — which applies uniformly to every
//! flag letter.

use super::regex;

#[test]
fn flags_can_be_negated() {
    let re = regex(r"(?i)ab(?-i)CD");
    assert!(re.is_match("ABCD"));
    assert!(re.is_match("abCD"));
    assert!(!re.is_match("abcd"));
}

#[test]
fn a_leading_hyphen_negates_every_following_flag() {
    let re = regex(r"(?i)AB(?-is).C");
    assert!(re.is_match("abxC"));
    assert!(!re.is_match("abxc"));
    // `s` was turned off with `i`, so `.` no longer crosses a newline.
    assert!(!re.is_match("ab\nC"));
}

#[test]
fn negation_applies_only_to_flags_after_the_hyphen() {
    let re = regex(r"(?m-i)^ab$");
    assert!(re.is_match("x\nab\ny"));
    assert!(!re.is_match("x\nAB\ny"));
}

#[test]
fn scoped_flags_do_not_leak_past_their_group() {
    let re = regex(r"(?i:ab)cd");
    assert!(re.is_match("ABcd"));
    assert!(!re.is_match("ABCD"));
}

#[test]
fn scoped_negation_restores_the_outer_flag() {
    let re = regex(r"(?i)ab(?-i:cd)ef");
    assert!(re.is_match("ABcdEF"));
    assert!(!re.is_match("ABCDEF"));
}
