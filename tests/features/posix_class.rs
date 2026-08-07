//! POSIX bracket-expression classes: `[:alpha:]`, `[:digit:]`, etc.
//!
//! These are ASCII-only, matching PCRE, grep and the `regex` crate, and this
//! codebase's own precedent that `\d`/`\w` are ASCII-only rather than wired
//! to Unicode property tables. `[[:alpha:]]` must never fall back to being
//! parsed as a nested class containing the literal members `:`, `a`, `l`,
//! `p`, `h`.

use super::regex;

/// All 14 POSIX class names, each with the characters it must match and the
/// characters it must reject.
const CLASSES: &[(&str, &[char], &[char])] = &[
    ("alpha", &['a', 'X'], &['9', '_']),
    ("digit", &['9'], &['a']),
    ("alnum", &['a', '9'], &['_']),
    ("upper", &['X'], &['x']),
    ("lower", &['x'], &['X']),
    ("space", &[' ', '\t'], &['a']),
    ("blank", &[' ', '\t'], &['a']),
    ("cntrl", &['\u{01}'], &['a']),
    ("print", &['a'], &['\u{01}']),
    ("graph", &['a'], &[' ']),
    ("punct", &['.'], &['a']),
    ("xdigit", &['f'], &['g']),
    ("word", &['_'], &[' ']),
    ("ascii", &['a'], &['\u{100}']),
];

#[test]
fn every_posix_class_matches_its_members_and_rejects_its_non_members() {
    for &(name, members, non_members) in CLASSES {
        let pattern = format!("[[:{name}:]]");
        let re = regex(&pattern);
        for member in members {
            assert!(
                re.is_match(&member.to_string()),
                "[[:{name}:]] should match {member:?}"
            );
        }
        for non_member in non_members {
            assert!(
                !re.is_match(&non_member.to_string()),
                "[[:{name}:]] should reject {non_member:?}"
            );
        }
    }
}

#[test]
fn negated_posix_class_inside_class_composes() {
    let re = regex("[[:^alpha:]]");
    assert!(re.is_match("9"));
    assert!(!re.is_match("a"));
}

#[test]
fn class_level_negation_composes_with_posix_class() {
    let re = regex("[^[:alpha:]]");
    assert!(re.is_match("9"));
    assert!(!re.is_match("a"));
}

#[test]
fn multiple_posix_classes_in_one_bracket_expression_union() {
    let re = regex("[[:alpha:][:digit:]]");
    assert!(re.is_match("a"));
    assert!(re.is_match("9"));
    assert!(!re.is_match("_"));
}

#[test]
fn posix_class_mixed_with_ordinary_members() {
    let re = regex("[a[:digit:]]");
    assert!(re.is_match("a"));
    assert!(re.is_match("9"));
    assert!(!re.is_match("b"));
}

#[test]
fn plain_nested_class_union_still_works() {
    // `[` is only treated as a POSIX class opener when immediately followed
    // by `:`; otherwise it is the pre-existing nested-class union syntax.
    let re = regex("[a[b-c]]");
    assert!(re.is_match("a"));
    assert!(re.is_match("b"));
    assert!(re.is_match("c"));
    assert!(!re.is_match("d"));
}

#[test]
fn malformed_posix_class_syntax_is_rejected() {
    for pattern in ["[[:bogus:]]", "[[:alpha]", "[[::]]"] {
        assert!(
            regexr::Regex::new(pattern).is_err(),
            "{pattern:?} must not compile"
        );
    }
}
