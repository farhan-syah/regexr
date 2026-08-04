//! Iteration must preserve the left context of the resume position.
//!
//! `find_iter` / `captures_iter` / `replace_all` resume a search at a byte
//! offset into the *original* haystack. They must not hand the engines a fresh,
//! shorter string starting at that offset: doing so makes the resume point look
//! like the start of the text, so `^` matches there, `\b` is computed against a
//! missing neighbour, and lookbehind sees nothing.
//!
//! Every case is checked on both builds (JIT and interpreter), and the whole
//! iteration is also compared byte-for-byte against the executable spec in
//! `regexr::reference`, resumed with the same rule the iterators use.

use regexr::hir::translate;
use regexr::parser::parse;
use regexr::{Regex, RegexBuilder};

/// The two builds under test, labelled for failure messages.
fn builds(pattern: &str) -> Vec<(&'static str, Regex)> {
    vec![
        (
            "jit",
            RegexBuilder::new(pattern)
                .jit(true)
                .build()
                .expect("pattern should compile"),
        ),
        (
            "interp",
            RegexBuilder::new(pattern)
                .jit(false)
                .build()
                .expect("pattern should compile"),
        ),
    ]
}

fn ranges(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    re.find_iter(text).map(|m| (m.start(), m.end())).collect()
}

fn capture_ranges(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    re.captures_iter(text)
        .filter_map(|c| c.get(0).map(|m| (m.start(), m.end())))
        .collect()
}

/// Smallest index `>= i` that is a UTF-8 boundary — the resume rule documented
/// on `Regex::find_iter` (byte-level constructs can end inside a codepoint, so
/// the next search must start at the following codepoint).
fn ceil_char_boundary(text: &str, i: usize) -> usize {
    let mut j = i;
    while j < text.len() && !text.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// What the iterators must produce, straight from the spec: leftmost-first
/// matches over the *whole* input, resumed at an offset.
fn reference_ranges(pattern: &str, text: &str) -> Vec<(usize, usize)> {
    let hir = parse(pattern)
        .and_then(|ast| translate(&ast))
        .expect("pattern should compile");
    let ncaps = hir.props.capture_count as usize;
    let bytes = text.as_bytes();

    let mut out = Vec::new();
    let mut last_end = 0usize;
    while last_end <= bytes.len() {
        let (start, end) = match regexr::reference::find_from(&hir.expr, ncaps, bytes, last_end) {
            Some(m) => m,
            None => break,
        };
        out.push((start, end));
        last_end = if start == end {
            ceil_char_boundary(text, end + 1)
        } else {
            ceil_char_boundary(text, end)
        };
    }
    out
}

/// Splices `rep` over each span — what `replace_all` must produce for the
/// reference's match list. Assembled as bytes and decoded lossily, exactly like
/// `Regex::replace_all`, since a span can split a codepoint.
fn replace_spans(text: &str, spans: &[(usize, usize)], rep: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut last_end = 0;
    for &(start, end) in spans {
        out.extend_from_slice(&bytes[last_end..start]);
        out.extend_from_slice(rep.as_bytes());
        last_end = end;
    }
    out.extend_from_slice(&bytes[last_end..]);
    String::from_utf8_lossy(&out).into_owned()
}

// =============================================================================
// Start anchors
// =============================================================================

/// The reported bug: `^` matched at the resume point, so `^a` found two matches
/// in "aa" instead of one.
#[test]
fn start_anchor_matches_only_at_text_start() {
    for (label, re) in builds(r"^a") {
        assert_eq!(ranges(&re, "aa"), vec![(0, 1)], "{label}");
        assert_eq!(capture_ranges(&re, "aa"), vec![(0, 1)], "{label}");
        assert_eq!(re.replace_all("aa", "X"), "Xa", "{label}");
    }
}

#[test]
fn start_anchor_word_run_matches_once() {
    for (label, re) in builds(r"^\w+") {
        assert_eq!(ranges(&re, "abc abc"), vec![(0, 3)], "{label}");
    }
}

/// A literal alternation is normally answered by the literal prefilter alone,
/// which knows nothing about anchors — so an anchored one must not take that
/// short-cut, at position 0 or at any resume position.
#[test]
fn start_anchor_with_complete_literal_prefilter() {
    for (label, re) in builds(r"^[ab]") {
        assert_eq!(ranges(&re, "ab"), vec![(0, 1)], "{label}");
        assert_eq!(ranges(&re, "ba"), vec![(0, 1)], "{label}");
        assert!(!re.is_match("cab"), "{label}");
    }
    // The same alternation without the anchor still matches everywhere.
    for (label, re) in builds(r"[ab]") {
        assert_eq!(ranges(&re, "ab"), vec![(0, 1), (1, 2)], "{label}");
    }
}

/// `\A` is the strict start of text and can never hold at a resume point.
#[test]
fn text_start_anchor_matches_only_once() {
    for (label, re) in builds(r"\Aab") {
        assert_eq!(ranges(&re, "abab"), vec![(0, 2)], "{label}");
    }
}

/// Multiline `^` still matches at every line start — the fix must not turn the
/// resumed search into an anchored-at-0 search.
#[test]
fn multiline_start_anchor_matches_every_line() {
    for (label, re) in builds(r"(?m)^a") {
        assert_eq!(
            ranges(&re, "a\na\na"),
            vec![(0, 1), (2, 3), (4, 5)],
            "{label}"
        );
    }
}

// =============================================================================
// End anchors
// =============================================================================

/// An end-anchored pattern must not match at an interior resume point: with the
/// old re-slicing every resumed search ended where the original did, but the
/// leftmost-first choice was taken over a truncated string.
#[test]
fn end_anchor_matches_only_at_text_end() {
    for (label, re) in builds(r"a$") {
        assert_eq!(ranges(&re, "aaa"), vec![(2, 3)], "{label}");
    }
    for (label, re) in builds(r"\w+$") {
        assert_eq!(ranges(&re, "ab cd"), vec![(3, 5)], "{label}");
    }
}

#[test]
fn strict_end_anchor_matches_only_at_text_end() {
    for (label, re) in builds(r"a\z") {
        assert_eq!(ranges(&re, "aaa"), vec![(2, 3)], "{label}");
    }
}

// =============================================================================
// Word boundaries
// =============================================================================

/// A match followed immediately by a word character must not see a boundary at
/// the resume position: "foofoo" contains exactly one `\bfoo`.
#[test]
fn word_boundary_across_resume_boundary() {
    for (label, re) in builds(r"\bfoo") {
        assert_eq!(ranges(&re, "foofoo"), vec![(0, 3)], "{label}");
        assert_eq!(re.replace_all("foofoo", "X"), "Xfoo", "{label}");
    }
}

/// The mirror case: `\B` must see the *word* character before the resume point.
#[test]
fn not_word_boundary_across_resume_boundary() {
    for (label, re) in builds(r"\Bo") {
        // "foo": the `o` at 1 and the `o` at 2 are both preceded by a word
        // character, so both are non-boundaries.
        assert_eq!(ranges(&re, "foo"), vec![(1, 2), (2, 3)], "{label}");
    }
}

#[test]
fn word_boundary_pairs_still_found() {
    for (label, re) in builds(r"\bab\b") {
        assert_eq!(
            ranges(&re, "ab ab ab"),
            vec![(0, 2), (3, 5), (6, 8)],
            "{label}"
        );
    }
}

// =============================================================================
// Lookbehind
// =============================================================================

/// Lookbehind must see the bytes before the resume position. With re-slicing
/// the second "b" looked like it had nothing behind it.
#[test]
fn positive_lookbehind_across_resume_boundary() {
    for (label, re) in builds(r"(?<=a)b") {
        assert_eq!(ranges(&re, "abb"), vec![(1, 2)], "{label}");
        assert_eq!(ranges(&re, "abab"), vec![(1, 2), (3, 4)], "{label}");
    }
}

/// The negative form must not become vacuously true at the resume point.
#[test]
fn negative_lookbehind_across_resume_boundary() {
    for (label, re) in builds(r"(?<!a)b") {
        // "abb": the b at 1 is preceded by 'a' (rejected), the b at 2 by 'b'.
        assert_eq!(ranges(&re, "abb"), vec![(2, 3)], "{label}");
    }
}

// =============================================================================
// Unanchored patterns and multi-byte input are unaffected
// =============================================================================

#[test]
fn plain_ascii_iteration_unchanged() {
    for (label, re) in builds(r"\w+") {
        assert_eq!(
            ranges(&re, "ab cd  ef"),
            vec![(0, 2), (3, 5), (7, 9)],
            "{label}"
        );
        assert_eq!(re.replace_all("ab cd  ef", "X"), "X X  X", "{label}");
    }
}

#[test]
fn empty_matches_still_make_progress() {
    for (label, re) in builds(r"a*") {
        // One empty match per position, plus the non-empty run at 0.
        assert_eq!(ranges(&re, "aab"), vec![(0, 2), (2, 2), (3, 3)], "{label}");
    }
}

/// The resume position is snapped to the next codepoint boundary, so no match
/// ever starts inside a multi-byte codepoint even for byte-level constructs.
#[test]
fn multibyte_resume_stays_on_codepoint_boundaries() {
    let text = "aé世🎉";
    for (label, re) in builds(r".") {
        for (start, _) in ranges(&re, text) {
            assert!(
                text.is_char_boundary(start),
                "match started inside a codepoint ({label}): {start}"
            );
        }
    }
}

/// Left context that is itself multi-byte: "éxéx" is `é`(0..2) `x`(2..3)
/// `é`(3..5) `x`(5..6), so both `x`es are preceded by `é`.
#[test]
fn lookbehind_over_multibyte_left_context() {
    for (label, re) in builds(r"(?<=é)x") {
        assert_eq!(ranges(&re, "éxéx"), vec![(2, 3), (5, 6)], "{label}");
    }
}

// =============================================================================
// Differential check against the executable spec
// =============================================================================

#[test]
fn iteration_agrees_with_reference() {
    const CASES: &[(&str, &str)] = &[
        (r"^a", "aa"),
        (r"^[ab]", "ab"),
        (r"[ab]", "abcab"),
        (r"^\w+", "abc abc"),
        (r"\Aab", "abab"),
        (r"(?m)^a", "a\na\na"),
        (r"a$", "aaa"),
        (r"\w+$", "ab cd"),
        (r"\bfoo", "foofoo"),
        (r"\bab\b", "ab ab ab"),
        (r"\Bo", "foo"),
        (r"(?<=a)b", "abab"),
        (r"(?<!a)b", "abb"),
        (r"(?<=a)(b)", "abab"),
        (r"(\w)\1", "aabb"),
        (r"\w+", "ab cd  ef"),
        (r"\w+", "héllo wörld"),
        (r"[a-z]+", "one two three"),
        (r" ?[^\s]+", "hello   world"),
        (r"\s+", "a  b\n\nc"),
        (r"\p{L}+", "中文 test"),
    ];

    // Collect every divergence instead of stopping at the first, so one run
    // reports the complete picture.
    let mut failures = Vec::new();

    for &(pattern, text) in CASES {
        let expected = reference_ranges(pattern, text);
        let expected_replacement = replace_spans(text, &expected, "X");

        for (label, re) in builds(pattern) {
            let case = format!("[{label}] {pattern:?} on {text:?}");

            let found = ranges(&re, text);
            if found != expected {
                failures.push(format!("find_iter {case}: ref={expected:?} got={found:?}"));
            }

            let captured = capture_ranges(&re, text);
            if captured != expected {
                failures.push(format!(
                    "captures_iter {case}: ref={expected:?} got={captured:?}"
                ));
            }

            let replaced = re.replace_all(text, "X").into_owned();
            if replaced != expected_replacement {
                failures.push(format!(
                    "replace_all {case}: ref={expected_replacement:?} got={replaced:?}"
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "iteration/reference divergences ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
