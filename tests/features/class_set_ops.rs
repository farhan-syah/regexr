//! Character-class set operations: `&&` (intersection), `--` (difference),
//! and `~~` (symmetric difference) inside a bracket expression, e.g.
//! `[a-z&&[^aeiou]]`.
//!
//! These are the `regex` crate's bracket-expression set operators. All three
//! share one precedence level and associate left to right. A single `&`,
//! `-`, or `~` keeps its ordinary meaning — only the *doubled* form is an
//! operator — so this file also pins the single-character behavior that must
//! not regress.

use super::regex;

#[test]
fn intersection_matches_consonants_of_a_to_z() {
    let re = regex("[a-z&&[^aeiou]]");
    assert!(re.is_match("b"));
    assert!(!re.is_match("a"));
}

#[test]
fn difference_matches_consonants_of_a_to_z() {
    let re = regex("[a-z--[aeiou]]");
    assert!(re.is_match("b"));
    assert!(!re.is_match("a"));
}

#[test]
fn symmetric_difference_matches_code_points_in_exactly_one_side() {
    let re = regex("[a-g~~[d-z]]");
    assert!(re.is_match("b")); // in a-g only
    assert!(!re.is_match("d")); // in both
    assert!(re.is_match("z")); // in d-z only
}

#[test]
fn operators_are_left_associative_and_chain() {
    // ((a-z) && (^aeiou)) -- (xyz) = consonants of a-z, minus x, y, z.
    let re = regex("[a-z&&[^aeiou]--[xyz]]");
    assert!(re.is_match("b"));
    assert!(!re.is_match("a")); // excluded by &&
    assert!(!re.is_match("x")); // excluded by --
}

#[test]
fn operand_may_be_a_bare_run_of_members_on_either_side() {
    // Bare run on the right of &&.
    let re = regex("[a-z&&aeiou]");
    assert!(re.is_match("a"));
    assert!(!re.is_match("b"));

    // Bare run on the left of --.
    let re = regex("[aeiou--a]");
    assert!(re.is_match("e"));
    assert!(!re.is_match("a"));
}

#[test]
fn leading_negation_applies_to_the_whole_computed_set_last() {
    // [^a-z&&[^aeiou]] = complement of (consonants of a-z), i.e. everything
    // except those consonants — including the vowels and everything outside
    // a-z, but not the consonants themselves.
    let re = regex("[^a-z&&[^aeiou]]");
    assert!(!re.is_match("b")); // a consonant: excluded by the negation
    assert!(re.is_match("a")); // a vowel: not in the consonant set, so included
    assert!(re.is_match("9")); // outside a-z entirely: included
}

#[test]
fn single_ampersand_is_still_a_literal_member() {
    let re = regex("[a&b]");
    assert!(re.is_match("a"));
    assert!(re.is_match("&"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("c"));
}

#[test]
fn single_hyphen_is_still_a_range_operator() {
    let re = regex("[a-z]");
    assert!(re.is_match("m"));
    assert!(!re.is_match("A"));
}

#[test]
fn single_hyphen_at_class_edges_is_still_a_literal() {
    let leading = regex("[-a]");
    assert!(leading.is_match("-"));
    assert!(leading.is_match("a"));
    assert!(!leading.is_match("b"));

    let trailing = regex("[a-]");
    assert!(trailing.is_match("-"));
    assert!(trailing.is_match("a"));
    assert!(!trailing.is_match("b"));
}

#[test]
fn single_tilde_is_still_a_literal_member() {
    let re = regex("[a~b]");
    assert!(re.is_match("a"));
    assert!(re.is_match("~"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("c"));
}

#[test]
fn nested_class_union_still_works() {
    let re = regex("[[ab]|[cd]]");
    assert!(re.is_match("a"));
    assert!(re.is_match("|"));
    assert!(re.is_match("c"));
}

#[test]
fn nested_class_composition_still_works() {
    let re = regex("[a[b-c]]");
    assert!(re.is_match("a"));
    assert!(re.is_match("b"));
    assert!(re.is_match("c"));
    assert!(!re.is_match("d"));
}

#[test]
fn set_op_not_confused_with_posix_class_syntax() {
    // `[[:alpha:]]` must still parse as a POSIX class, not a nested class
    // whose members happen to include `:`.
    let re = regex("[[:alpha:]]");
    assert!(re.is_match("a"));
    assert!(!re.is_match("9"));

    // And a genuine set operation with a nested class operand must still
    // work alongside POSIX class syntax existing in the codebase.
    let re = regex("[a-z&&[^aeiou]]");
    assert!(re.is_match("b"));
    assert!(!re.is_match("a"));
}
