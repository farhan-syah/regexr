//! The *sequence* of matches, checked against an independent implementation.
//!
//! `tests/pcre2_conformance.rs` compares one `find` at a time. Iteration adds a
//! rule no single `find` can exercise: an empty match at the position where the
//! previous, non-empty match ended is that position reported twice, and is
//! dropped. `a*` on "aa" is one match of "aa" — not that plus an empty match at
//! 2. Every span in the wrong sequence is individually correct, which is why
//! span-at-a-time comparison never caught it.
//!
//! The oracle here is the `regex` crate rather than PCRE2: the `pcre2` crate's
//! `find_iter` does not terminate on an all-nullable pattern such as `a?b?c?`.
//! `regex` agrees with PCRE2 on the rule and iterates safely, so it can cover
//! every pattern it accepts — which is all of these, since none needs lookaround
//! or backreferences.

use regexr::Regex;

/// Patterns where regexr deliberately differs from the `regex` crate.
///
/// Verified against PCRE2, which regexr follows here.
const INTENTIONAL_DIVERGENCE: &[(&str, &str)] = &[(
    r"a*$",
    "regexr's `$` also matches before a single trailing newline (PCRE/Python); \
     the `regex` crate's is strict end-of-haystack. On \"\\n\" that is an extra \
     empty match at 0, which PCRE2 also reports.",
)];

/// Patterns whose match *sequence* is decided by the empty-match rule.
const PATTERNS: &[&str] = &[
    r"a*",
    r"b*",
    r"a?",
    r"a??",
    r"\d*",
    r"\w*",
    r"\s*",
    r"a*b*",
    r"(a)*",
    r"a|",
    r"|a",
    r"\b\w*",
    r"a*\B",
    r"\B\w*",
    r"(?:)",
    r"",
    r"x*",
    r"[ab]*",
    r"a{0,2}",
    r"(a*)(b*)",
    r"\w*\d*",
    r"^a*",
    r"a*$",
];

const HAYSTACKS: &[&str] = &[
    "",
    "a",
    "aa",
    "aaa",
    "b",
    "ab",
    "ba",
    "aab",
    "abc",
    "abab",
    "a b",
    " a ",
    "  ",
    "1a2",
    "12ab34",
    "hello world",
    "yy",
    "ab cd",
    "aa bb",
    "\n",
    "a\nb",
    "_x_",
];

#[test]
fn match_sequences_agree_with_an_independent_engine() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        if INTENTIONAL_DIVERGENCE.iter().any(|(p, _)| p == pattern) {
            continue;
        }
        let (Ok(ours), Ok(theirs)) = (Regex::new(pattern), regex::Regex::new(pattern)) else {
            continue;
        };

        for haystack in HAYSTACKS {
            let a: Vec<_> = ours
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect();
            let b: Vec<_> = theirs
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect();
            compared += 1;
            if a != b {
                divergences.push(format!(
                    "  {pattern:?} on {haystack:?}: regexr={a:?} regex={b:?}"
                ));
            }
        }
    }

    assert!(compared > 0, "no pattern/haystack pair was compared");
    assert!(
        divergences.is_empty(),
        "{} of {compared} match sequences disagree:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// `captures_iter` must report the same sequence as `find_iter`.
///
/// They are separate loops with separate resume state, so the rule has to hold
/// in both or the two APIs disagree about how many matches a haystack contains.
#[test]
fn captures_iter_reports_the_same_sequence_as_find_iter() {
    let mut divergences = Vec::new();

    for pattern in PATTERNS {
        let Ok(ours) = Regex::new(pattern) else {
            continue;
        };
        for haystack in HAYSTACKS {
            let finds: Vec<_> = ours
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect();
            let captures: Vec<_> = ours
                .captures_iter(haystack)
                .filter_map(|c| c.get(0))
                .map(|m| (m.start(), m.end()))
                .collect();
            if finds != captures {
                divergences.push(format!(
                    "  {pattern:?} on {haystack:?}: find_iter={finds:?} captures_iter={captures:?}"
                ));
            }
        }
    }

    assert!(
        divergences.is_empty(),
        "{} sequences differ between the two iterators:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}
