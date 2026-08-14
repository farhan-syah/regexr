//! A nullable greedy run (`X*` over a byte class) is recognised structurally by
//! the tagged-NFA step extractor and emitted as one `GreedyStar`, instead of
//! being modelled as an `Alt` of "took some"/"took none" with every trailing
//! step duplicated into both branches.
//!
//! Two things have to hold, and both are checked here:
//!
//! * the recognition fires where it should — `\w*(?=ing)` now extracts and
//!   reaches the combined `GreedyStarLookahead` step instead of falling back to
//!   the PikeVm;
//! * it fires *only* on that shape — a genuine alternation such as `(?:a+|b)`
//!   reaches the same split-with-two-epsilons NFA shape, and recognising it as
//!   a star would silently compile `a*b`.
//!
//! The oracle is `regexr::reference`, the executable spec the engines are
//! defined against, driven with the same empty-match iteration rule as
//! `Regex::find_iter` (see `MatchesInner::Generic`). Using the spec rather than
//! another crate keeps ASCII-vs-Unicode `\w` and `$`-before-newline dialect
//! differences out of the comparison.

use regexr::hir::translate;
use regexr::nfa::tagged::{PatternStep, StepExtractor, TaggedNfa};
use regexr::parser::parse;

/// Patterns whose match sequence must be unchanged by the recognition.
///
/// Greedy stars that now take the new path, the non-greedy spellings that must
/// not, and the alternations the exit-convergence check protects.
const PATTERNS: &[&str] = &[
    // Nullable greedy runs, bare and with leading/trailing context.
    r"\w*",
    r"\w*a",
    r"a\w*",
    r"[a-z]*x",
    r"\w*$",
    r"\w*\b",
    r"\w*(?=ing)",
    r"\w*(?!ing)",
    r"[a-z]*(?=ing)",
    // Chained and prefixed runs: two quantifiers in one program.
    r"\w*\d*a",
    r"\w+\w*a",
    r"(?:ab)*x",
    // Non-greedy: the split prefers the exit, and must keep its old treatment.
    r"\w*?a",
    r"\w*?(?=ing)",
    // Genuine alternations reaching the same NFA shape.
    r"(?:a+|b)",
    r"(?:a+|b)c",
    r"(?:\w+|x)(?=ing)",
    // Optional and codepoint-class nullables, which the recognition declines.
    r"\w?(?=ing)",
    r"\p{L}*(?=ing)",
    r"\p{L}*a",
];

/// Empty, all-match, no-match, and multi-byte UTF-8 subjects.
///
/// Free of newlines on purpose: `$` before a trailing newline is regexr's
/// documented divergence from other engines, and it is not what these patterns
/// are here to check.
const HAYSTACKS: &[&str] = &[
    "",
    "a",
    "aa",
    "aaa",
    "b",
    "bc",
    "aac",
    "abc",
    "ab cd",
    "  ",
    "a1a",
    "bca",
    "ing",
    "inging",
    "singing",
    "sing ing",
    "singing ringing",
    "xyz",
    "héllo x",
    "naïveing",
    "中文 abc",
    "abcded",
];

/// The next codepoint boundary at or after `at`, clamped to the end of `text`.
fn ceil_boundary(text: &str, at: usize) -> usize {
    let mut at = at;
    while at < text.len() && !text.is_char_boundary(at) {
        at += 1;
    }
    at.min(text.len() + 1)
}

/// Every span `find_iter` must report, according to the reference matcher.
///
/// Mirrors `Matches::next`: resume the search at the previous match's end, step
/// one codepoint past an empty match so iteration progresses, and drop an empty
/// match falling exactly where the previous non-empty one ended (that position
/// reported twice).
fn reference_spans(pattern: &str, text: &str) -> Vec<(usize, usize)> {
    let hir = parse(pattern)
        .and_then(|ast| translate(&ast))
        .expect("pattern should compile");
    let ncaps = hir.props.capture_count as usize;
    let bytes = text.as_bytes();

    let mut spans = Vec::new();
    let mut last_end = 0usize;
    let mut skip_empty_at: Option<usize> = None;
    while last_end <= bytes.len() {
        let Some((start, end)) = regexr::reference::find_from(&hir.expr, ncaps, bytes, last_end)
        else {
            break;
        };
        let empty = start == end;
        last_end = if empty {
            ceil_boundary(text, end + 1)
        } else {
            ceil_boundary(text, end)
        };
        if empty && skip_empty_at == Some(start) {
            skip_empty_at = None;
            continue;
        }
        skip_empty_at = (!empty).then_some(end);
        spans.push((start, end));
    }
    spans
}

fn engine_spans(regex: &regexr::Regex, text: &str) -> Vec<(usize, usize)> {
    regex
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect()
}

/// The extracted step program, or `None` when extraction declines it.
fn extract(pattern: &str) -> Option<Vec<PatternStep>> {
    let hir = parse(pattern)
        .and_then(|ast| translate(&ast))
        .expect("pattern should compile");
    let nfa = regexr::nfa::compile(&hir).expect("NFA should compile");
    StepExtractor::new(&nfa).extract()
}

#[test]
fn star_patterns_iterate_like_the_reference() {
    let mut failures = Vec::new();

    for &pattern in PATTERNS {
        let jit = regexr::RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .unwrap();
        let interp = regexr::RegexBuilder::new(pattern)
            .jit(false)
            .build()
            .unwrap();

        for &text in HAYSTACKS {
            let expected = reference_spans(pattern, text);
            for (label, regex) in [("jit", &jit), ("interp", &interp)] {
                let got = engine_spans(regex, text);
                if got != expected {
                    failures.push(format!(
                        "{pattern:?} on {text:?} ({label}): got {got:?}, reference {expected:?}"
                    ));
                }
            }
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Absolute spans for the headline cases, independent of the oracle: a greedy
/// star takes as much as it can and gives characters back, a non-greedy one
/// takes as little as it can, and neither is the other.
#[test]
fn greedy_and_non_greedy_stars_report_different_spans() {
    /// Pattern, haystack, and the exact spans `find_iter` must yield.
    type Case = (&'static str, &'static str, &'static [(usize, usize)]);

    let cases: &[Case] = &[
        (r"\w*a", "aa", &[(0, 2)]),
        (r"\w*?a", "aa", &[(0, 1), (1, 2)]),
        (r"\w*", "ab cd", &[(0, 2), (3, 5)]),
        (r"\w*", "", &[(0, 0)]),
        (r"\w*a", "", &[]),
        (r"\w*(?=ing)", "singing", &[(0, 4)]),
        (r"\w*(?=ing)", "ing", &[(0, 0)]),
        (r"\w*(?=ing)", "xyz", &[]),
        (r"\w*(?!ing)", "singing", &[(0, 7)]),
        // The star stops at the first byte of `ï`, so the run that satisfies
        // the assertion starts after it.
        (r"[a-z]*(?=ing)", "naïveing", &[(4, 6)]),
    ];

    for &(pattern, text, expected) in cases {
        for jit in [true, false] {
            let regex = regexr::RegexBuilder::new(pattern).jit(jit).build().unwrap();
            assert_eq!(
                engine_spans(&regex, text),
                expected,
                "{pattern:?} on {text:?} (jit={jit})"
            );
        }
    }
}

/// The point of the change: the nullable run and its lookahead become one
/// combined step, so the pattern stays on the tagged-NFA fast path instead of
/// extracting nothing and falling back to the PikeVm.
#[test]
fn a_nullable_run_beside_a_lookahead_reaches_the_combined_step() {
    for pattern in [r"\w*(?=ing)", r"[a-z]*(?=xy)", r"a\w*(?!ing)"] {
        let steps =
            extract(pattern).unwrap_or_else(|| panic!("{pattern:?} extracted no step program"));
        assert!(
            steps
                .iter()
                .any(|s| matches!(s, PatternStep::GreedyStarLookahead(_, _, _))),
            "{pattern:?} extracted no combined greedy-star+lookahead step: {steps:?}"
        );
    }
}

/// An alternation whose first branch is a `+` loop reaches the same
/// split-with-two-epsilons shape as `X*`. What separates them is where the loop
/// exits: a quantifier's loop rejoins the split's own exit, an alternation's
/// does not.
///
/// The check bites here rather than through `Regex`, because a pattern without
/// lookaround or non-greedy parts is not routed to this engine at all — the
/// step program is built and run directly.
#[test]
fn a_genuine_alternation_is_not_compiled_as_a_star() {
    for pattern in [r"(?:a+|b)", r"(?:a+|b)c"] {
        let steps = extract(pattern).unwrap_or_else(|| panic!("{pattern:?} no longer extracts"));
        assert!(
            !steps
                .iter()
                .any(|s| matches!(s, PatternStep::GreedyStar(_))),
            "{pattern:?} was compiled as a star: {steps:?}"
        );

        let hir = parse(pattern)
            .and_then(|ast| translate(&ast))
            .expect("pattern should compile");
        let ncaps = hir.props.capture_count as usize;
        for &text in HAYSTACKS {
            let bytes = text.as_bytes();
            assert_eq!(
                TaggedNfa::find(&steps, bytes),
                regexr::reference::find(&hir.expr, ncaps, bytes),
                "{pattern:?} on {text:?}: step program disagrees with the reference"
            );
        }
    }

    // Same shape, plus the trailing lookahead that makes the branch walk give
    // up. It must stay declined rather than be rescued by a star recognition
    // that ignores where the loop exits — which would compile it as `\w*x`.
    assert!(
        extract(r"(?:\w+|x)(?=ing)").is_none(),
        "an alternation was recognised as a star: the exit-convergence check is \
         too loose"
    );
}
