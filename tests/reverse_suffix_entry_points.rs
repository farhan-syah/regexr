//! Every entry point of a reverse-suffix pattern must answer what `find` does.
//!
//! Patterns shaped like `\w+(?=ing\b)` are searched end-first: a `memmem` scan
//! for the lookahead's literal, a walk back over the class run, and an anchored
//! confirm. That search replaces the forward scan outright, so each public
//! entry point either goes through it or keeps a forward scan of its own — and a
//! forward scan that disagrees is exactly the failure this file exists to catch.
//! The risk is not that the search is wrong (that is covered where it is
//! defined) but that one caller of it is routed and its neighbour is not.
//!
//! The oracle is `find` itself, deliberately: these entry points are required to
//! agree with each other, whatever the answer is. `is_match` must equal
//! `find(..).is_some()`, a capture set's slot 0 must be `find`'s span, and the
//! iterators' first element must be `find`'s answer too.
//!
//! Both build configurations matter. The gate is consulted where the tagged-NFA
//! interpreter and the tagged-NFA JIT are built, and the confirm is each
//! engine's own `match_at`, so the two configurations exercise different code
//! behind the same routing. `jit(true)` falls back to the interpreter when the
//! feature is off, which is worth running as well.

use regexr::{Regex, RegexBuilder};

/// Patterns the end-first search accepts. The last one carries a group inside
/// the lookahead, so the capture path has a slot beyond the whole match to
/// report — the run itself may not contain a capture, so this is the only place
/// one can sit.
const PATTERNS: &[&str] = &[
    r"\w+(?=ing\b)",
    r"\w*(?=ing)",
    r"[a-z]+(?=xy)",
    r"\w+(?=(ing)\b)",
];

/// Haystacks chosen for where the search's answer differs from the literal it
/// scanned for: a match ending at an occurrence the scan proposed second
/// (`"singing"`), one run per match (`"singing ringing"`), a literal present
/// with no match at all (`"ing"`, `"ings"`), an occurrence with no run before it
/// followed by one that has (`"inging"`), and multi-byte text, where the reverse
/// walk must land on a character boundary rather than inside one.
const HAYSTACKS: &[&str] = &[
    "",
    "singing",
    "singing ringing",
    "sing ing",
    "ings",
    "inging",
    "ing",
    " ing",
    "xy",
    " xyxy",
    "abxyxy",
    "naïveing",
    "中ing 中",
    "中inging",
];

fn compiled(pattern: &str, jit: bool) -> Regex {
    RegexBuilder::new(pattern)
        .jit(jit)
        .build()
        .expect("pattern compiles")
}

/// `is_match` has a candidate loop of its own, and a reverse-suffix pattern's
/// candidates are not the positions that loop expects. A haystack containing the
/// literal with no match around it (`"ing"`, `"ings"`) is where a short-circuit
/// on the literal alone reports true and `find` reports nothing.
#[test]
fn is_match_agrees_with_find() {
    for pattern in PATTERNS {
        for jit in [false, true] {
            let re = compiled(pattern, jit);
            for haystack in HAYSTACKS {
                let expected = re.find(haystack).is_some();
                assert_eq!(
                    re.is_match(haystack),
                    expected,
                    "is_match disagrees: pattern={pattern:?} jit={jit} haystack={haystack:?}"
                );
                assert_eq!(
                    re.try_is_match(haystack).expect("no backtracking budget"),
                    expected,
                    "try_is_match disagrees: pattern={pattern:?} jit={jit} haystack={haystack:?}"
                );
            }
        }
    }
}

/// The capture pass is a second search, and it is handed the start the end-first
/// search found. If it ever searched from somewhere else — or scanned forward
/// from the resume point as it used to — it could land on a different match than
/// `find` reports, which no test of `find` alone would show.
#[test]
fn captures_span_agrees_with_find() {
    for pattern in PATTERNS {
        for jit in [false, true] {
            let re = compiled(pattern, jit);
            for haystack in HAYSTACKS {
                let expected = re.find(haystack).map(|m| (m.start(), m.end()));
                let context = format!("pattern={pattern:?} jit={jit} haystack={haystack:?}");

                let whole = re
                    .captures(haystack)
                    .map(|caps| caps.get(0).map(|m| (m.start(), m.end())));
                assert_eq!(whole, expected.map(Some), "captures disagrees: {context}");

                let whole = re
                    .try_captures(haystack)
                    .expect("no backtracking budget")
                    .map(|caps| caps.get(0).map(|m| (m.start(), m.end())));
                assert_eq!(
                    whole,
                    expected.map(Some),
                    "try_captures disagrees: {context}"
                );
            }
        }
    }
}

/// `find`, `try_find` and the two iterators all sit behind the same required
/// literal check, which the end-first search now performs itself. Skipping it
/// must not change any of their answers.
#[test]
fn every_search_entry_point_reports_the_same_first_match() {
    for pattern in PATTERNS {
        for jit in [false, true] {
            let re = compiled(pattern, jit);
            for haystack in HAYSTACKS {
                let expected = re.find(haystack).map(|m| (m.start(), m.end()));
                let context = format!("pattern={pattern:?} jit={jit} haystack={haystack:?}");

                assert_eq!(
                    re.try_find(haystack)
                        .expect("no backtracking budget")
                        .map(|m| (m.start(), m.end())),
                    expected,
                    "try_find disagrees: {context}"
                );
                assert_eq!(
                    re.find_iter(haystack).next().map(|m| (m.start(), m.end())),
                    expected,
                    "find_iter disagrees: {context}"
                );
                assert_eq!(
                    re.captures_iter(haystack)
                        .next()
                        .and_then(|caps| caps.get(0).map(|m| (m.start(), m.end()))),
                    expected,
                    "captures_iter disagrees: {context}"
                );
            }
        }
    }
}

/// The two haystacks a naive routing gets wrong, spelled out rather than left
/// to the agreement loops: the literal is present, so a rejection filter passes
/// it through, and no match exists, so every entry point must say so.
#[test]
fn a_literal_with_no_match_around_it_matches_nothing() {
    for jit in [false, true] {
        let re = compiled(r"\w+(?=ing\b)", jit);
        for haystack in ["ing", "ings"] {
            assert!(
                re.find(haystack).is_none(),
                "jit={jit} haystack={haystack:?}"
            );
            assert!(!re.is_match(haystack), "jit={jit} haystack={haystack:?}");
            assert!(
                re.captures(haystack).is_none(),
                "jit={jit} haystack={haystack:?}"
            );
        }

        // The counterexample the search is built around, at every entry point:
        // "ing" occurs at 1 and at 4, and the match ends at neither of the
        // positions a left-to-right literal scan would report on its own.
        let found = re.find("singing").expect("singing matches");
        assert_eq!((found.start(), found.end()), (0, 4));
        assert!(re.is_match("singing"));
        let caps = re.captures("singing").expect("singing matches");
        let whole = caps.get(0).expect("slot 0 is always set");
        assert_eq!((whole.start(), whole.end()), (0, 4));
    }
}
