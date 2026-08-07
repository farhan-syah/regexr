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
const INTENTIONAL_DIVERGENCE: &[(&str, &str)] = &[
    (
        r"a*$",
        "regexr's `$` also matches before a single trailing newline (PCRE/Python); \
         the `regex` crate's is strict end-of-haystack. On \"\\n\" that is an extra \
         empty match at 0, which PCRE2 also reports.",
    ),
    (
        r"^.$",
        "same `$` difference: on \"\\r\\n\" the `.` takes the \\r and `$` holds \
         before the final newline. PCRE2 agrees with regexr.",
    ),
];

/// Patterns whose sequence is decided by ordinary matching rather than by the
/// empty-match rule — breadth, to catch anything the curated list above misses.
///
/// Haystacks are ASCII on purpose. regexr's `\w`/`\W`/`\b`/`\B` are ASCII-only
/// by design (see the feature matrix) while the `regex` crate's are Unicode, so
/// non-ASCII subjects would report a dialect choice as a failure. Non-ASCII
/// behaviour is covered by `tests/pcre2_conformance.rs`, which runs PCRE2 with
/// UCP off and so shares regexr's definition.
const BROAD_PATTERNS: &[&str] = &[
    r"a+b",
    r"[a-z]+",
    r"[^a-z]+",
    r"\d+",
    r"\w+",
    r"\W+",
    r"(a)(b)",
    r"(a|b)+",
    r"a{2,3}",
    r"a{2,3}?",
    r"(ab)+",
    r"a+?b",
    r"cat|category",
    r"(foo|foobar)baz",
    r"^abc$",
    r"\babc\b",
    r"a$",
    r"^a",
    r"(?m)^b",
    r"(?m)^\w+$",
    r"(?i)ABC",
    r"[[:alpha:]]+",
    r"[[:digit:]]+",
    r"a.c",
    r"a.+c",
    r"(?s)a.c",
    r"X.Y",
    r"^.$",
    r"(\d{4})-(\d{2})-(\d{2})",
    r"(\w+)@(\w+)\.(\w+)",
    r"([a-z]+)([0-9]*)",
    r"\b\d+\b",
    r"\B\w",
    r"[ab]+",
    r"a*?b",
    r"(a+|b+)+",
    r"((a|b)|c)+",
    r"[^,]+",
    r"(?:ab)+",
    r"[0-9]{1,3}",
    r"\w+\s+\w+",
    r"(a)(b)?(c)",
    r"((a)(b))c",
    r"[a-c]x",
    r"x[a-c]",
];

const BROAD_HAYSTACKS: &[&str] = &[
    "",
    "a",
    "ab",
    "abc",
    "aab",
    "abab",
    "xyz",
    "a.b",
    "a b",
    "123",
    "a1b2",
    "  ",
    "\r\n",
    "hello world",
    "CAT cat",
    "foobarbaz",
    "category",
    "a\nc",
    "a\nb",
    "aXc",
    "_under_",
    "9",
    "-",
    "2024-01-15",
    "user@site.com",
    "12-34",
    "abc123",
    "ac",
    "a,b,,c",
    "one two three",
    "AAA bbb CCC",
    "x1y2z3",
    "  lead",
    "trail  ",
    "line1\nline2\nline3",
    "aaa\nbbb",
    "aXbXc",
];

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

/// The same sequence comparison across ordinary (non-nullable) patterns.
///
/// The empty-match rule was found by comparing sequences; this widens that
/// comparison so the next sequence-level difference is found the same way
/// instead of by benchmark archaeology.
#[test]
fn broad_match_sequences_agree_with_an_independent_engine() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in BROAD_PATTERNS {
        if INTENTIONAL_DIVERGENCE.iter().any(|(p, _)| p == pattern) {
            continue;
        }
        let (Ok(ours), Ok(theirs)) = (Regex::new(pattern), regex::Regex::new(pattern)) else {
            continue;
        };

        for haystack in BROAD_HAYSTACKS {
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

    assert!(
        compared > 1000,
        "expected broad coverage, compared {compared}"
    );
    assert!(
        divergences.is_empty(),
        "{} of {compared} match sequences disagree:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}
