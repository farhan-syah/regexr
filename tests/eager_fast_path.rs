//! The specialized eager-DFA scan must be invisible from the outside.
//!
//! Iteration takes a shortcut for the commonest configuration — no prefilter,
//! an eager DFA whose search is the plain unanchored loop — skipping the
//! engine dispatch and the per-attempt anchor checks that are invariant for the
//! whole search. That is a pure dispatch change, so every span it produces must
//! be the span the general path produces.
//!
//! Two kinds of test here. The first pins spans for patterns the shortcut is
//! meant to serve: a wrong loop (a skipped start position, a dropped end
//! anchor) changes them. The second pins spans for patterns it must decline —
//! assertions the shortcut's inner loop does not evaluate — so a gate that
//! widened to admit them would report matches at positions the assertions
//! forbid.

use regexr::Regex;

/// Every match span, as byte offsets.
fn spans(pattern: &str, text: &str) -> Vec<(usize, usize)> {
    let re = Regex::new(pattern).expect("pattern should compile");
    re.find_iter(text).map(|m| (m.start(), m.end())).collect()
}

/// The tokenization pattern the shortcut was measured on.
const TOKENIZER: &str =
    r#"[a-zA-Z_][a-zA-Z0-9_]*|[0-9]+(?:\.[0-9]+)?|[+\-*/=<>!&|^%]+|[(){}\[\];,.]|"[^"]*"|'[^']*'"#;

#[test]
fn class_alternation_spans_are_unchanged() {
    // Leftmost-longest, one run at a time: "x" stops at the digit, which then
    // starts its own match.
    assert_eq!(
        spans(r"[a-z]+|[0-9]+", "abc 123 x9"),
        vec![(0, 3), (4, 7), (8, 9), (9, 10)]
    );
}

#[test]
fn character_class_spans_are_unchanged() {
    let text = "abc 123 45 6789 x";
    let expected = vec![(4, 7), (8, 10), (11, 15)];
    assert_eq!(spans(r"[0-9]+", text), expected);

    // Same answer through an engine the shortcut declines: every digit run in
    // this text is word-delimited, so the boundaries add no constraint and the
    // two patterns must agree span for span.
    assert_eq!(spans(r"\b[0-9]+\b", text), expected);
}

#[test]
fn tokenizer_spans_are_unchanged() {
    // `1.5` is one number, not `1` `.` `5`: the longest match at that position
    // wins over the shorter branches that also start there.
    assert_eq!(
        spans(TOKENIZER, "x = 1.5 + foo(y);"),
        vec![
            (0, 1),
            (2, 3),
            (4, 7),
            (8, 9),
            (10, 13),
            (13, 14),
            (14, 15),
            (15, 16),
            (16, 17),
        ]
    );
}

#[test]
fn tokenizer_spans_are_unchanged_with_strings() {
    // The quoted branches, which are the ones that can run far past their
    // start position before the DFA settles.
    assert_eq!(
        spans(TOKENIZER, r#"s = "a b" + 'c';"#),
        vec![(0, 1), (2, 3), (4, 9), (10, 11), (12, 15), (15, 16)]
    );
}

#[test]
fn word_boundaries_still_hold() {
    // The discriminating case for the gate: `\b\w` is the *first* character of
    // each word, not every word character. The shortcut's inner loop does not
    // evaluate boundary assertions, so admitting this pattern would report all
    // four positions.
    assert_eq!(spans(r"\b\w", "ab cd"), vec![(0, 1), (3, 4)]);
}

#[test]
fn word_boundary_negation_still_holds() {
    // The mirror image: `\B\w` is every word character *except* the first.
    assert_eq!(spans(r"\B\w", "ab cd"), vec![(1, 2), (4, 5)]);
}

#[test]
fn start_anchor_still_holds() {
    // One match, at 0 — not one per occurrence of the literal.
    assert_eq!(spans(r"^foo", "foo foo"), vec![(0, 3)]);
}

#[test]
fn multiline_anchors_still_hold() {
    // Line starts only: the `x` at offset 3 is mid-line and must not match.
    assert_eq!(spans(r"(?m)^x", "x\nyx\nx"), vec![(0, 1), (5, 6)]);
}

#[test]
fn literal_prefilter_spans_are_unchanged() {
    // A literal pattern has a prefilter, so it keeps the general path; the
    // spans are here to catch a gate that ignored the prefilter.
    assert_eq!(spans(r"hello", "say hello, hello"), vec![(4, 9), (11, 16)]);
}

#[test]
fn end_anchor_spans_are_unchanged() {
    // `$` is checked inside the attempt rather than at the start position, so
    // the trailing run is the only match — the leading one is not reported and
    // does not suppress it either.
    assert_eq!(spans(r"[0-9]+$", "12 345"), vec![(3, 6)]);
}

#[test]
fn multibyte_haystack_spans_stay_on_codepoint_boundaries() {
    // No span may begin or end inside a codepoint. Each accented letter is two
    // bytes, so the offsets only line up if the scan respects them.
    let text = "héllo wörld café";
    assert_eq!(
        spans(r"[a-z]+", text),
        vec![(0, 1), (3, 6), (7, 8), (10, 13), (14, 17)]
    );

    // Same haystack, with a class that does reach past ASCII.
    assert_eq!(spans(r"[^ ]+", text), vec![(0, 6), (7, 13), (14, 19)]);
}

#[test]
fn empty_matches_are_unchanged() {
    // The empty match at 2 is dropped — it is where the previous non-empty
    // match ended — and the one at the end of the text is kept. Losing a start
    // position in the scan loop would drop the `a` instead.
    assert_eq!(spans(r"a*", "bab"), vec![(0, 0), (1, 2), (3, 3)]);
}
