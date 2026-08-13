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
