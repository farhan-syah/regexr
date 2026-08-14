//! Lookahead and lookbehind tests.
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.
//! Lookarounds require PikeVM fallback for JIT (DFA can't handle them).

// Using local mod.rs

use super::regex;

use regexr::Regex;

// =============================================================================
// Positive Lookahead (?=...)
// =============================================================================

#[test]
fn test_positive_lookahead_basic() {
    let re = regex(r"a(?=b)");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("ac"));
    assert!(!re.is_match("a"));
}

#[test]
fn test_positive_lookahead_find() {
    let re = regex(r"a(?=b)");
    let m = re.find("ab").unwrap();
    assert_eq!(m.as_str(), "a");
    assert_eq!(m.start(), 0);
    assert_eq!(m.end(), 1);
}

#[test]
fn test_positive_lookahead_complex() {
    let re = regex(r"\w+(?=\.)");
    let m = re.find("hello. world").unwrap();
    assert_eq!(m.as_str(), "hello");
}

// =============================================================================
// Negative Lookahead (?!...)
// =============================================================================

#[test]
fn test_negative_lookahead_basic() {
    let re = regex(r"a(?!b)");
    assert!(re.is_match("ac"));
    assert!(re.is_match("ad"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_negative_lookahead_end() {
    let re = regex(r"a(?!b)");
    assert!(re.is_match("a"));
}

#[test]
fn test_negative_lookahead_find() {
    let re = regex(r"a(?!b)");
    let m = re.find("ab ac").unwrap();
    assert_eq!(m.as_str(), "a");
    assert_eq!(m.start(), 3);
}

#[test]
fn test_lookahead_in_pattern() {
    let re = regex(r"(?=a)ab");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("cb"));
}

#[test]
fn test_multiple_lookaheads() {
    let re = regex(r"(?=a)(?=.b)..");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("ac"));
}

// =============================================================================
// Positive Lookbehind (?<=...)
// =============================================================================

#[test]
fn test_positive_lookbehind_basic() {
    let re = regex(r"(?<=a)b");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("cb"));
    assert!(!re.is_match("b"));
}

#[test]
fn test_positive_lookbehind_find() {
    let re = regex(r"(?<=a)b");
    let m = re.find("ab").unwrap();
    assert_eq!(m.as_str(), "b");
    assert_eq!(m.start(), 1);
    assert_eq!(m.end(), 2);
}

#[test]
fn test_positive_lookbehind_complex() {
    let re = regex(r"(?<=hello )\w+");
    let m = re.find("hello world").unwrap();
    assert_eq!(m.as_str(), "world");
}

// =============================================================================
// Negative Lookbehind (?<!...)
// =============================================================================

#[test]
fn test_negative_lookbehind_basic() {
    let re = regex(r"(?<!a)b");
    assert!(re.is_match("cb"));
    assert!(re.is_match("xb"));
    assert!(!re.is_match("ab"));
}

#[test]
fn test_negative_lookbehind_start() {
    let re = regex(r"(?<!a)b");
    assert!(re.is_match("b"));
}

#[test]
fn test_negative_lookbehind_find() {
    let re = regex(r"(?<!a)b");
    let m = re.find("ab cb").unwrap();
    assert_eq!(m.as_str(), "b");
    assert_eq!(m.start(), 4);
}

// =============================================================================
// Variable-Length Lookbehind
// =============================================================================
//
// A lookbehind holds when *some* path through it ends at the current position,
// not just the path the inner pattern prefers.

#[test]
fn test_negative_lookbehind_optional_dot() {
    let re = regex(r"(?<!..?)b");
    // "b" at 1 is preceded by one character, which `..?` matches.
    assert!(!re.is_match("ab"));
    // Nothing precedes "b", so neither branch can match.
    assert!(re.is_match("b"));
    // Two preceding characters: matched by the two-character branch.
    assert!(!re.is_match("xxb"));
}

#[test]
fn test_negative_lookbehind_optional_word() {
    let re = regex(r"(?<!\w\w?)b");
    assert!(!re.is_match("ab"));
    assert!(!re.is_match("aab"));
    assert!(re.is_match("b"));
    // A non-word character cannot start either branch.
    assert!(re.is_match(" b"));
}

#[test]
fn test_positive_lookbehind_optional_dot() {
    let re = regex(r"(?<=..?)b");
    assert!(re.is_match("ab"));
    assert!(re.is_match("xxb"));
    assert!(!re.is_match("b"));
}

#[test]
fn test_lookbehind_alternation_unequal_lengths() {
    // The longer branch is preferred, so the shorter one must still be tried.
    let re = regex(r"(?<=xa|a)b");
    assert!(re.is_match("ab"));
    assert!(re.is_match("xab"));
    assert!(!re.is_match("yb"));

    let re = regex(r"(?<!xa|a)b");
    assert!(!re.is_match("ab"));
    assert!(!re.is_match("xab"));
    assert!(re.is_match("yb"));
}

#[test]
fn test_lookbehind_bounded_repetition() {
    let re = regex(r"(?<=\d{1,3})b");
    assert!(re.is_match("1b"));
    assert!(re.is_match("123b"));
    assert!(!re.is_match("xb"));

    let re = regex(r"(?<!\d{1,3})b");
    assert!(!re.is_match("1b"));
    assert!(!re.is_match("12b"));
    assert!(re.is_match("xb"));
}

#[test]
fn test_lookbehind_unbounded_repetition() {
    let re = regex(r"(?<=a.*)b");
    assert!(re.is_match("ab"));
    assert!(re.is_match("axxxb"));
    assert!(!re.is_match("xb"));

    // `x{0,2}` can match empty, so it always ends at the current position.
    let re = regex(r"(?<!x{0,2})y");
    assert!(!re.is_match("y"));
    assert!(!re.is_match("xxy"));
}

#[test]
fn test_lookbehind_variable_length_keeps_left_context() {
    // The inner `^` must see the real start of input, not a detached slice.
    let re = regex(r"(?<=^a?)b");
    assert!(re.is_match("ab"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("xab"));

    // The inner `\b` must see the byte before the lookbehind's start.
    let re = regex(r"(?<=\bfo?)o");
    assert!(re.is_match("foo"));
    assert!(!re.is_match("xfoo"));
}

#[test]
fn test_lookbehind_variable_length_multibyte() {
    let re = regex(r"(?<=[αβ]?)γ");
    assert!(re.is_match("αγ"));
    assert!(re.is_match("γ"));

    let re = regex(r"(?<![αβ]?)γ");
    assert!(!re.is_match("αγ"));
    assert!(!re.is_match("γ"));
}

#[test]
fn test_lookbehind_variable_length_find_position() {
    let re = regex(r"(?<!..?)b");
    // Only the "b" with no preceding characters qualifies.
    assert_eq!(re.find("b ab").map(|m| m.start()), Some(0));
    assert!(re.find("ab b").is_none());
}

// =============================================================================
// Combined Lookahead and Lookbehind
// =============================================================================

#[test]
fn test_lookbehind_and_lookahead() {
    let re = regex(r"(?<=a).(?=c)");
    assert!(re.is_match("abc"));
    assert!(re.is_match("adc"));
    assert!(!re.is_match("axyz"));
    assert!(!re.is_match("xbc"));
}

#[test]
fn test_lookbehind_variable_length() {
    let re = Regex::new(r"(?<=a+)b");
    if let Ok(re) = re {
        assert!(re.is_match("ab"));
        assert!(re.is_match("aab"));
        assert!(re.is_match("aaab"));
    }
}

// =============================================================================
// Nested lookarounds
//
// An assertion inside another assertion. The inner one must be evaluated, not
// dropped: a dropped assertion reads as trivially satisfied, which makes the
// outer assertion match text it has to reject. Expectations here were checked
// against PCRE2.
// =============================================================================

#[test]
fn negative_lookahead_nested_in_positive_lookahead_is_honoured() {
    // `(?=a(?!b))` demands an `a` that is NOT followed by `b`.
    let re = regex(r"(?=a(?!b))a.");
    assert!(
        !re.is_match("ab"),
        "the nested (?!b) must reject an `a` followed by `b`"
    );
    assert_eq!(re.find("ac").map(|m| m.as_str()), Some("ac"));
}

#[test]
fn positive_lookahead_nests_in_positive_lookahead() {
    let re = regex(r"a(?=b(?=c))bc");
    assert_eq!(re.find("abc").map(|m| m.as_str()), Some("abc"));
    assert!(!re.is_match("abd"));
}

#[test]
fn lookahead_nested_at_the_start_of_a_lookahead() {
    let re = regex(r"(?=(?=a)a)a");
    assert_eq!(re.find("a").map(|m| m.as_str()), Some("a"));
    assert!(!re.is_match("b"));
}

#[test]
fn negative_lookahead_nested_in_negative_lookahead_double_negates() {
    // `(?!(?!y))` succeeds where `(?=y)` would.
    let re = regex(r"x(?!(?!y))");
    assert_eq!(re.find("xy").map(|m| m.as_str()), Some("x"));
    assert!(!re.is_match("xz"));
}

#[test]
fn lookbehind_sees_the_bytes_before_its_own_start() {
    // The inner `(?<=x)` has to look further left than the outer `(?<=ya)`
    // begins. Matching the outer against a detached slice would hide the `x`.
    let re = regex(r"a(?<=(?<=x)ya)");
    assert_eq!(re.find("xya").map(|m| m.as_str()), Some("a"));
    assert!(
        !re.is_match("zya"),
        "the inner (?<=x) must still be checked"
    );
}

#[test]
fn stacked_lookbehinds_are_all_checked() {
    let re = regex(r"(?<=a)(?<=xa)b");
    assert_eq!(re.find("xab").map(|m| m.as_str()), Some("b"));
    assert!(!re.is_match("yab"));
}

#[test]
fn word_boundary_inside_a_lookbehind_sees_real_context() {
    let re = regex(r"(?<=\bfoo)bar");
    assert_eq!(re.find("foobar").map(|m| m.as_str()), Some("bar"));
}

// =============================================================================
// Assertions at the Tail of a Lookaround
// =============================================================================
//
// A zero-width assertion is part of the lookaround's inner pattern wherever it
// sits, including as its last element. `(?<=a\b)` and `(?<=a)` are different
// assertions, as are `(?=a$)` and `(?=a)`.

#[test]
fn test_lookbehind_trailing_word_boundary() {
    let re = regex(r"(?<=a\b)x");
    // No boundary between "a" and "x" — both are word characters.
    assert!(!re.is_match("ax"));

    let re = regex(r"(?<=a\b)-");
    assert_eq!(re.find("a-").map(|m| m.start()), Some(1));
}

#[test]
fn test_lookbehind_trailing_not_word_boundary() {
    // `\b` and `\B` are opposites and must not compile to the same assertion.
    let word_boundary = regex(r"(?<=a\b)x");
    let not_word_boundary = regex(r"(?<=a\B)x");
    assert!(!word_boundary.is_match("ax"));
    assert!(not_word_boundary.is_match("ax"));
}

#[test]
fn test_negative_lookbehind_trailing_word_boundary() {
    let re = regex(r"(?<!a\b)x");
    // `(?<=a\b)` does not hold here, so its negation does.
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));

    let re = regex(r"(?<!a\b)-");
    assert!(!re.is_match("a-"));
}

#[test]
fn test_lookbehind_trailing_end_anchor() {
    let re = regex(r"(?<=a$)b");
    // "a" is not at the end of the text, so the lookbehind fails.
    assert!(!re.is_match("ab"));

    let re = regex(r"(?<=a$)");
    assert!(!re.is_match("ab"));
    assert_eq!(re.find("a").map(|m| m.start()), Some(1));

    let re = regex(r"(?<!a$)x");
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
}

#[test]
fn test_lookbehind_trailing_absolute_end_anchor() {
    let re = regex(r"(?<=a\Z)");
    assert!(!re.is_match("ab"));
    assert_eq!(re.find("a").map(|m| m.start()), Some(1));
}

#[test]
fn test_lookbehind_trailing_line_anchor() {
    let re = regex(r"(?m)(?<=a$)x");
    assert!(!re.is_match("ax"));

    let re = regex(r"(?m)(?<=a$)\nx");
    assert_eq!(re.find("a\nx").map(|m| m.start()), Some(1));
}

#[test]
fn test_lookahead_trailing_word_boundary() {
    let re = regex(r"x(?=a\b)");
    assert_eq!(re.find("xa").map(|m| m.start()), Some(0));
    assert_eq!(re.find("xa-").map(|m| m.start()), Some(0));
    // "a" is followed by another word character, so there is no boundary.
    assert!(!re.is_match("xab"));
}

#[test]
fn test_lookahead_trailing_not_word_boundary() {
    let re = regex(r"x(?=a\B)");
    assert!(re.is_match("xab"));
    assert!(!re.is_match("xa"));
}

#[test]
fn test_negative_lookahead_trailing_assertion() {
    let re = regex(r"x(?!a\b)");
    assert_eq!(re.find("xab").map(|m| m.start()), Some(0));
    assert!(!re.is_match("xa"));

    let re = regex(r"x(?!a$)");
    assert_eq!(re.find("xab").map(|m| m.start()), Some(0));
}

#[test]
fn test_lookahead_trailing_end_anchor() {
    let re = regex(r"x(?=a$)");
    assert_eq!(re.find("xa").map(|m| m.start()), Some(0));
    assert!(!re.is_match("xab"));
}

#[test]
fn test_lookahead_trailing_boundary_after_class() {
    // The last word character of a run, found via a trailing `\b` in a lookahead.
    let re = regex(r"\w(?=\w\b)");
    assert_eq!(re.find("  111").map(|m| m.start()), Some(3));
}

#[test]
fn test_find_and_captures_agree_on_tail_assertions() {
    // `find` and `captures` must answer the same question. They are served by
    // different engines internally, so a lookaround they disagree on means one
    // of those engines is not evaluating the pattern.
    for pattern in [
        r"(?<=a\b)x",
        r"(?<!a\b)x",
        r"(?<=a$)b",
        r"x(?=a\b)",
        r"x(?!a\b)",
        r"x(?=a$)",
        r"x(?=a\B)",
    ] {
        let re = regex(pattern);
        for haystack in ["ax", "abx", "xa", "xab", "a-", "ab"] {
            let found = re.find(haystack).map(|m| (m.start(), m.end()));
            let captured = re
                .captures(haystack)
                .map(|c| c.get(0).map(|m| (m.start(), m.end())).unwrap());
            assert_eq!(
                found, captured,
                "find/captures disagree for {pattern:?} on {haystack:?}"
            );
        }
    }
}

#[test]
fn test_lookbehind_fixed_width_codepoint_class() {
    // Every codepoint in the class encodes to the same number of UTF-8 bytes,
    // so the lookbehind knows exactly how far back to look.
    let re = regex(r"(?<=[α-ω])x");
    assert_eq!(re.find("αx").map(|m| m.start()), Some(2));
    assert_eq!(re.find("αβx").map(|m| m.start()), Some(4));
    assert!(!re.is_match("ax"));
    assert!(!re.is_match("x"));

    let re = regex(r"(?<![α-ω])x");
    assert!(!re.is_match("αx"));
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert_eq!(re.find("x").map(|m| m.start()), Some(0));

    // Three-byte encodings.
    let re = regex(r"(?<=[\u{4E00}-\u{9FFF}])x");
    assert_eq!(re.find("中x").map(|m| m.start()), Some(3));
    assert!(!re.is_match("ax"));
}

#[test]
fn test_lookbehind_variable_width_codepoint_class() {
    // `\p{L}` spans one- through four-byte encodings, and a class mixing widths
    // or a negated one does too. All must still be evaluated correctly.
    let re = regex(r"(?<=\p{L})x");
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert_eq!(re.find("éx").map(|m| m.start()), Some(2));
    assert_eq!(re.find("中x").map(|m| m.start()), Some(3));
    assert!(!re.is_match("1x"));

    let re = regex(r"(?<=[a-zα])x");
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert_eq!(re.find("αx").map(|m| m.start()), Some(2));

    let re = regex(r"(?<![^α])x");
    assert_eq!(re.find("αx").map(|m| m.start()), Some(2));
    assert!(!re.is_match("ax"));
}

// =============================================================================
// Lookaround After a Quantifier, and Anchors Inside a Lookaround
// =============================================================================

#[test]
fn test_quantifier_followed_by_two_lookaheads() {
    // A `*` splits into "took some"/"took none", and both arms must still carry
    // every assertion that follows the quantifier.
    let re = regex(r"a*(?=b)(?=c)");
    assert!(!re.is_match("ab"));
    assert!(!re.is_match("ac"));

    let re = regex(r"x*(?=a)(?=b)");
    assert!(!re.is_match("ab"));

    let re = regex(r"[^\s]*(?!\S)(?=a.*)");
    assert!(!re.is_match(""));
    assert!(!re.is_match(" \u{4E2D}\u{A0}a,"));

    let re = regex(r"\p{L}*(?=\S)(?=\p{L})");
    assert_eq!(re.find("Z0").map(|m| (m.start(), m.end())), Some((0, 0)));
}

#[test]
fn test_quantifier_followed_by_two_word_boundaries() {
    // Same shape with `\b`: `\b` and `\b` are one assertion each, and neither
    // may be lost to the quantifier split.
    let re = regex(r"a*\b\b");
    assert_eq!(re.find("ab").map(|m| (m.start(), m.end())), Some((0, 0)));
}

#[test]
fn test_start_anchor_inside_a_lookaround() {
    // `^` inside a lookaround reads absolute position, so a search that resumes
    // at a later candidate must still see it as false there.
    let re = regex(r"(?=^)x");
    assert!(!re.is_match("ax"));
    assert_eq!(re.find("x").map(|m| m.start()), Some(0));

    let re = regex(r"(?!^)x");
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert!(!re.is_match("x"));

    let re = regex(r"(?=\A)x");
    assert!(!re.is_match("ax"));

    let re = regex(r"(?=^[a-z])x");
    assert!(!re.is_match("ax"));
    assert!(!re.is_match("zzzzx"));

    let re = regex(r"(?!^[a-z])x");
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert_eq!(re.find("abx").map(|m| m.start()), Some(2));
}

#[test]
fn test_word_boundary_inside_a_lookahead_reads_left_context() {
    // `\b` at the candidate position depends on the byte before it, which a
    // search resuming there must not hide.
    let re = regex(r"(?=\b)x");
    assert!(!re.is_match("ax"));
    assert_eq!(re.find("a x").map(|m| m.start()), Some(2));

    let re = regex(r"(?=\B)x");
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert!(!re.is_match("a x"));
}

#[test]
fn test_alternation_arms_each_keep_their_lookaround() {
    // Two top-level arms, each ending in a lookaround whose inner pattern is
    // itself an alternation with anchors.
    let re = regex(r"\S+\p{N}?[^\s\p{L}\p{N}]*(?=$a|xa\Z)|[^\s]* +(?!\Z\p{L}^)");
    assert!(!re.is_match("\u{3000}a,\u{B2}\t.\u{E9}\u{4E2D}\u{E9}"));
}

// =============================================================================
// Multi-Width Lookbehind
// =============================================================================
//
// `\s` is the full Unicode `White_Space` set, whose members encode to one, two
// or three UTF-8 bytes, so a lookbehind on it has no single width to look back
// by — it has a *set* of candidate widths. These pin the observable behaviour
// for each width, at the start of the haystack where the wider candidates do
// not fit, and for the negated form.

#[test]
fn test_lookbehind_on_a_multi_width_class() {
    let re = regex(r"(?<=\s)x");
    // One-byte space.
    assert_eq!(re.find(" x").map(|m| m.start()), Some(1));
    // Two-byte space: U+00A0 NO-BREAK SPACE.
    assert_eq!(re.find("\u{A0}x").map(|m| m.start()), Some(2));
    // Three-byte spaces: U+2003 EM SPACE, U+3000 IDEOGRAPHIC SPACE.
    assert_eq!(re.find("\u{2003}x").map(|m| m.start()), Some(3));
    assert_eq!(re.find("\u{3000}x").map(|m| m.start()), Some(3));

    // Non-space of each width behind: no candidate may match, and a candidate
    // that lands mid-codepoint must be rejected rather than read as a space.
    assert!(!re.is_match("ax"));
    assert!(!re.is_match("\u{E9}x"));
    assert!(!re.is_match("\u{4E2D}x"));
}

#[test]
fn test_lookbehind_on_a_multi_width_class_at_haystack_start() {
    // At position 0 nothing is behind, and at position 1 only the one-byte
    // candidate fits; the wider ones must be skipped, not underflow the index.
    let re = regex(r"(?<=\s)x");
    assert!(!re.is_match("x"));
    assert_eq!(re.find(" x").map(|m| m.start()), Some(1));

    // Same at the start of a *word* match rather than a single byte.
    let re = regex(r"(?<=\s)\w+");
    assert!(!re.is_match("ab"));
    assert_eq!(re.find(" ab").map(|m| (m.start(), m.end())), Some((1, 3)));
    assert_eq!(
        re.find("\u{A0}ab").map(|m| (m.start(), m.end())),
        Some((2, 4))
    );
    assert_eq!(
        re.find("\u{2003}ab").map(|m| (m.start(), m.end())),
        Some((3, 5))
    );
}

#[test]
fn test_negative_lookbehind_on_a_multi_width_class() {
    // The negation must cover *every* candidate width: a two- or three-byte
    // space behind is only found if the wider candidates are tried, since the
    // one-byte candidate lands on a continuation byte and rejects on its own.
    let re = regex(r"(?<!\s)x");
    assert_eq!(re.find("x").map(|m| m.start()), Some(0));
    assert_eq!(re.find("ax").map(|m| m.start()), Some(1));
    assert!(!re.is_match(" x"));
    assert!(!re.is_match("\u{A0}x"));
    assert!(!re.is_match("\u{2003}x"));
    assert!(!re.is_match("\u{3000}x"));

    let re = regex(r"(?<!\s)\w+");
    assert_eq!(re.find("ab").map(|m| (m.start(), m.end())), Some((0, 2)));
    assert_eq!(re.find(" ab").map(|m| (m.start(), m.end())), Some((2, 3)));
    assert_eq!(
        re.find("\u{A0}ab").map(|m| (m.start(), m.end())),
        Some((3, 4))
    );
}

#[test]
fn test_lookbehind_widths_of_two_classes_combine() {
    // Two multi-width classes in one lookbehind: the totals are every sum of
    // their widths, and a mixed pair must match as readily as a matching pair.
    let re = regex(r"(?<=\s\s)x");
    assert_eq!(re.find("  x").map(|m| m.start()), Some(2));
    assert_eq!(re.find(" \u{A0}x").map(|m| m.start()), Some(3));
    assert_eq!(re.find("\u{A0} x").map(|m| m.start()), Some(3));
    assert_eq!(re.find("\u{2003}\u{2003}x").map(|m| m.start()), Some(6));
    // One space is not two, at any width.
    assert!(!re.is_match(" x"));
    assert!(!re.is_match("\u{2003}x"));
}

// =============================================================================
// Leading lookbehind literal as an offset prefilter
// =============================================================================

/// All spans of `pattern` over `text`, as (start, end) byte offsets.
fn spans(pattern: &str, text: &str) -> Vec<(usize, usize)> {
    let re = regex(pattern);
    re.find_iter(text).map(|m| (m.start(), m.end())).collect()
}

/// A pattern opening with `(?<=@)` is searched by its lookbehind's literal,
/// which sits one byte to the LEFT of every match. Every span below is one the
/// offset arithmetic has to reproduce exactly.
#[test]
fn test_lookbehind_literal_prefix_finds_every_match() {
    let text = "@abc x@def @ghi @@jkl no@at";
    assert_eq!(
        spans(r"(?<=@)\w+", text),
        vec![(1, 4), (7, 10), (12, 15), (18, 21), (25, 27)]
    );
    assert_eq!(
        spans(r"(?<=\$)\d+", "$12 x34 $5 6$78"),
        vec![(1, 3), (9, 10), (13, 15)]
    );

    // A haystack without the literal has no match, and the first byte of the
    // haystack can never be a match start when a byte is required behind it.
    assert!(!regex(r"(?<=@)\w+").is_match("abc def"));
    assert_eq!(spans(r"(?<=@)\w+", "@a"), vec![(1, 2)]);
}

/// The resume path: the second match starts at exactly the byte the previous
/// iteration step resumed from, so its `a` sits STRICTLY BEFORE the resume
/// point. A scan that starts looking for the literal at the resume position
/// instead of `resume - offset` never sees it and silently drops the match.
#[test]
fn test_lookbehind_literal_prefix_survives_the_resume_point() {
    assert_eq!(spans(r"(?<=a)\w", "aab"), vec![(1, 2), (2, 3)]);
    assert_eq!(spans(r"(?<=a)\w", "aaaa"), vec![(1, 2), (2, 3), (3, 4)]);
    // The same shape with a two-byte literal, so the scan has to reach two
    // bytes back from the resume point rather than one.
    assert_eq!(spans(r"(?<=ab)\w\w", "abcdcd"), vec![(2, 4)]);
    assert_eq!(spans(r"(?<=ab)\w\w", "abcdabef"), vec![(2, 4), (6, 8)]);
}

/// A multi-byte lookbehind literal offsets by its full byte length.
#[test]
fn test_multi_byte_lookbehind_literal_prefix() {
    assert_eq!(
        spans(r"(?<=foo@)\w+", "foo@bar x@baz foo@qux"),
        vec![(4, 7), (18, 21)]
    );
    // Only the whole literal qualifies: a partial one must not offset.
    assert!(!regex(r"(?<=foo@)\w+").is_match("fo@bar"));
    assert!(!regex(r"(?<=foo@)\w+").is_match("xoo@bar"));
}

/// A UTF-8 lookbehind literal is offset by BYTES, not characters: `é` is two
/// bytes, so a match sits two bytes past the literal's start.
#[test]
fn test_utf8_lookbehind_literal_prefix() {
    assert_eq!(spans(r"(?<=é)\w+", "xéab yéc"), vec![(3, 5), (9, 10)]);
    assert!(!regex(r"(?<=é)\w+").is_match("xeab"));
}

/// A nullable body can match at end of input, where the translated candidate
/// lands exactly on `input.len()` — one past the last byte the prefilter can
/// ever report.
#[test]
fn test_lookbehind_literal_prefix_at_end_of_input() {
    let re = regex(r"(?<=@)\w*");
    assert_eq!(re.find("x@").map(|m| (m.start(), m.end())), Some((2, 2)));
    assert!(re.is_match("x@"));
    assert_eq!(re.find("@ab").map(|m| (m.start(), m.end())), Some((1, 3)));
}

/// The lookbehind's literal makes `@` a prefilter candidate, but the body must
/// still match at the position it translates to. The first `@` here is followed
/// by letters, not digits, so that candidate is rejected and the search
/// continues to the real match much further along.
#[test]
fn test_lookbehind_prefix_candidate_rejected_before_real_match() {
    let re = regex(r"(?<=@)\d+");
    assert_eq!(
        re.find("a@bcdefghijkl x@42").map(|m| (m.start(), m.end())),
        Some((16, 18))
    );
    assert_eq!(spans(r"(?<=@)\d+", "a@bcdefghijkl x@42"), vec![(16, 18)]);
}

/// Every match of a leading-lookbehind pattern over a multi-match haystack,
/// which is the path a literal prefilter drives candidate by candidate.
#[test]
fn test_lookbehind_prefix_multi_match_spans() {
    assert_eq!(
        spans(r"(?<=@)\w+", "a@one b@two c@three"),
        vec![(2, 5), (8, 11), (14, 19)]
    );
}

/// A NEGATIVE lookbehind's literal is the text that must be absent, so it must
/// never become a prefilter: searching for `@` would skip to precisely the
/// positions that cannot match.
#[test]
fn test_negative_lookbehind_literal_is_not_a_prefix() {
    assert_eq!(
        spans(r"(?<!@)\w+", "@abc x@def"),
        vec![(2, 4), (5, 6), (8, 10)]
    );
    // Only the first `a` qualifies: every later position has an `a` behind it.
    assert_eq!(spans(r"(?<!a)\w", "aab"), vec![(0, 1)]);
    assert!(regex(r"(?<!@)\w+").is_match("abc"));
}

// =============================================================================
// End-first search (`\w+(?=ing\b)`)
// =============================================================================

/// The end of a match is NOT the literal occurrence that led to it.
///
/// `ing` occurs at 1 and at 4 in `"singing"`, and `\b` only holds after the one
/// at 4. The answer's end is 4 — a position the literal scan proposes only as a
/// *candidate end*, never as the start it walks back to, and which the anchored
/// engine run at the discovered start is what actually produces.
#[test]
fn test_end_first_search_reports_the_start_not_the_literal() {
    let re = regex(r"\w+(?=ing\b)");
    assert_eq!(
        re.find("singing").map(|m| (m.start(), m.end())),
        Some((0, 4))
    );
    assert_eq!(spans(r"\w+(?=ing\b)", "singing"), vec![(0, 4)]);
}

/// Every span of an end-first search over a multi-match haystack. Iteration
/// resumes at the previous match's end, which the reverse walk must never step
/// back past.
#[test]
fn test_end_first_search_iteration_spans() {
    assert_eq!(
        spans(r"\w+(?=ing\b)", "singing ringing"),
        vec![(0, 4), (8, 12)]
    );
    // The run before the *second* `ing` stops at the space, so only the first
    // word matches — and it matches at length one, since `ing` follows `s`.
    assert_eq!(spans(r"\w+(?=ing\b)", "sing ing"), vec![(0, 1)]);
    // A leading occurrence has no run before it, so it is not a match end; the
    // one at 3 is, and its run reaches back to 0.
    assert_eq!(spans(r"\w+(?=ing\b)", "inging"), vec![(0, 3)]);
    assert_eq!(spans(r"\w+(?=ing\b)", "ings"), vec![]);
    assert_eq!(spans(r"\w+(?=ing\b)", ""), vec![]);
}

/// The literal is present and every occurrence still fails: `\w+` needs at
/// least one character before it, and there is none.
#[test]
fn test_end_first_search_literal_without_a_match() {
    let re = regex(r"\w+(?=ing\b)");
    assert!(re.find("ing").is_none());
    assert!(re.find(" ing").is_none());
    assert!(re.find("ings").is_none());
}

/// A match starting in the *interior* of a class run. The first `xy` is not a
/// match end (nothing precedes it in the run), but it is inside the run that
/// the second `xy` ends, so the position it sits at is still a viable start.
#[test]
fn test_end_first_search_start_inside_a_run() {
    assert_eq!(spans(r"[a-z]+(?=xy)", " xyxy"), vec![(1, 3)]);
    assert_eq!(spans(r"[a-z]+(?=xy)", "abxy"), vec![(0, 2)]);
    assert_eq!(spans(r"[a-z]+(?=xy)", " xy"), vec![]);
}

/// A `*` run may be empty, so a literal occurrence with nothing before it is
/// still a match — of width zero.
#[test]
fn test_end_first_search_allows_an_empty_run() {
    let re = regex(r"\w*(?=ing)");
    assert_eq!(re.find("ings").map(|m| (m.start(), m.end())), Some((0, 0)));
    assert_eq!(
        re.find("singing").map(|m| (m.start(), m.end())),
        Some((0, 4))
    );
    assert!(re.find("abc").is_none());
}

/// A run of multi-byte characters is walked one whole character at a time.
#[test]
fn test_end_first_search_over_a_codepoint_run() {
    let re = regex(r"(?u:\w+(?=ing))");
    // n a ï v e i n g — `ï` is two bytes, so `ing` begins at byte 6.
    assert_eq!(
        re.find("naïveing").map(|m| (m.start(), m.end())),
        Some((0, 6))
    );
    assert!(re.find("ï ing").is_none());
}
