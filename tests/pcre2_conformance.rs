//! Differential conformance against PCRE2.
//!
//! `tests/reference_conformance.rs` and `tests/iteration_fuzz.rs` check the
//! engines against `src/reference.rs`, an oracle written against the same
//! understanding of the semantics as the engines themselves. That catches
//! engine-to-engine disagreement but not a misconception shared by all of them.
//!
//! PCRE2 is an independent implementation, so it catches the shared kind. Every
//! pattern below is run through both and the match spans compared. Where regexr
//! deliberately differs, the pattern is listed in [`INTENTIONAL_DIVERGENCE`]
//! with the reason — that list is the specification of where regexr is not
//! PCRE2, and it is meant to stay short.
//!
//! PCRE2 runs with UTF on but UCP off, which gives `\w`, `\d` and the POSIX
//! classes their ASCII meanings — the same choice regexr makes.

use pcre2::bytes::RegexBuilder as Pcre2Builder;
use regexr::Regex;

/// Patterns where regexr intentionally disagrees with PCRE2, and why.
///
/// A pattern here is skipped entirely. Keep the reason specific: this list is
/// the difference between "we chose this" and "we have a bug".
const INTENTIONAL_DIVERGENCE: &[(&str, &str)] = &[
    (
        r"\s+",
        "regexr's \\s is Unicode White_Space; PCRE2 without UCP is ASCII-only",
    ),
    (r"\S+", "negation of the above, same reason"),
    (r"[\d\s]+", "contains \\s; see above"),
    (r"[^\d\s]+", "contains \\s; see above"),
    (
        r"[a-z&&[^aeiou]]+",
        "class set operators are regex-crate syntax; PCRE2 reads && as literal members",
    ),
    (
        r"(?<=a+)b",
        "regexr allows variable-length lookbehind; PCRE2 rejects the pattern",
    ),
];

/// Patterns spanning the syntax regexr claims to support.
const PATTERNS: &[&str] = &[
    // Quantifiers, greedy and lazy
    r"a*b",
    r"a+?b",
    r"a{2,3}",
    r"a{2,3}?",
    r"(ab)+",
    r"a?b?c?",
    r"a*?b",
    // Character classes
    r"[a-z]+",
    r"[^a-z]+",
    r"\d+",
    r"\D+",
    r"\w+",
    r"\W+",
    r"\s+",
    r"\S+",
    r"[[:alpha:]]+",
    r"[[:^alpha:]]+",
    r"[[:digit:]]+",
    r"[a-z&&[^aeiou]]+",
    r"\h+",
    r"\R",
    r"\N+",
    r"[\d\s]+",
    r"[^\d\s]+",
    r"\p{L}+",
    r"\P{L}+",
    r"[\p{L}\d]+",
    r"[à-ÿ]+",
    // Anchors and boundaries
    r"^abc$",
    r"\Aabc\z",
    r"\babc\b",
    r"\Babc",
    r"a$",
    r"^a",
    r"a\Z",
    r"(?m)^b",
    // Groups, captures, backreferences
    r"(a)(b)\2\1",
    r"(?<n>a)\k<n>",
    r"(?P<n>a)(?P=n)",
    r"(?:ab)+",
    r"(a|b)+",
    r"(foo|bar)+",
    r"(ab|cd)+",
    r"(a|b){2,}",
    r"((a|b)|c)+",
    r"(a+|b+)+",
    // Deterministic capture patterns — these take the one-pass capture engine,
    // so they must agree group-for-group with an engine that does not have one.
    r"(\d{4})-(\d{2})-(\d{2})",
    r"(\w+)@(\w+)\.(\w+)",
    r"(\d+)-(\d+)",
    r"([a-z]+)([0-9]*)",
    r"(a)(b)?(c)",
    r"((a)(b))c",
    // The same, gated by assertions: these take the one-pass engine's guard
    // path, where a wrongly-ordered assertion changes the groups but not the
    // full match.
    r"^(\d+)-(\d+)$",
    r"\b(\w+)\b",
    r"^(\w+)",
    r"(\w+)$",
    r"\B(\w)",
    r"(?m)^(\w+)$",
    r"\b(\d+)\b-(\w+)",
    r"^(a+)$",
    r"^(a+?)$",
    // Lookaround
    r"a(?=b)",
    r"a(?!b)",
    r"(?<=a)b",
    r"(?<!a)b",
    r"(?=a(?!b))a.",
    r"a(?=b(?=c))bc",
    r"(?<=a)(?<=xa)b",
    r"(?<=\bfoo)bar",
    // A positive lookahead carrying a literal drives the required-literal
    // rejection filter; it must not change which match is reported.
    r"\w+(?=ing\b)",
    r"(\w+)(?=ing\b)",
    r"a(?=bc)\w",
    r"\w+(?=ing\b)|zzz",
    r"(?<=a+)b",
    // A leading zero-width element must not hide the prefilter behind it: these
    // still start with a digit / literal, and the wrong answer is silently
    // reporting no match.
    r"(?<!\$)\d+",
    r"(?<=\$)\d+",
    r"(?!x)\d+",
    r"(?<!a)bcd",
    r"a(?=b)bc",
    // A small leading class drives the byte-set prefilter; a wrong set silently
    // skips real matches.
    r#"['"][^'"]*['"]"#,
    r#"(['"])[^'"]*\1"#,
    r"[abc]+",
    r"[ab]x",
    r"[^ab]x",
    // Nullable patterns, where the iteration rule for empty matches decides the
    // whole sequence rather than any single span.
    r"a*",
    r"b*",
    r"a?",
    r"\d*",
    r"\w*",
    r"a*b*",
    r"(a)*",
    r"a|",
    r"\b\w*",
    r"a*\B",
    // Escapes
    r"\x41",
    r"\x{263A}",
    r"\cA",
    r"\Qa.b\E",
    r"\t\n\r",
    r"a\-b",
    // Dot and Unicode
    r"a.c",
    r"a.+c",
    r"(?s)a.c",
    r"X.Y",
    r"[é]",
    r"[α-ω]+",
    r"^.$",
    // Alternation and flags
    r"cat|category",
    r"(foo|foobar)baz",
    r"(?i)ABC",
    r"(?i)straße",
    r"(?x) a b  # comment",
];

/// Subjects chosen to straddle the interesting boundaries: empty, ASCII,
/// multi-byte, line terminators, and text that nearly matches.
const HAYSTACKS: &[&str] = &[
    "",
    "a",
    "b",
    "ab",
    "abc",
    "aab",
    "aaab",
    "abab",
    "xyz",
    "a.b",
    "a b",
    "123",
    "a1b2",
    "  ",
    "\t \u{a0}",
    "\r\n",
    "hello world",
    "CAT cat",
    "é",
    "XéY",
    "αβγ",
    "☺",
    "A",
    "\u{1}",
    "foobarbaz",
    "running and singing",
    "sing",
    "ing",
    "zzz nothing",
    "zzz",
    "let x = \"hi\"; y = 'z';",
    "'a' and \"b\"",
    "abcabc",
    "ax bx cx",
    "$100 and 200",
    "price 42",
    "a1 $2 b3",
    "abcd",
    "xbcd",
    "no matches at all here",
    "category",
    "a\nc",
    "a\nb",
    "aXc",
    "_under_",
    "9",
    "-",
    "café",
    "straße",
    "foobar",
    "xab",
    "2024-01-15",
    "user@site.com",
    "12-34",
    "abc123",
    "ac",
    "abc",
];

/// Every supported pattern must report the same match span as PCRE2.
#[test]
fn match_spans_agree_with_pcre2() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        if INTENTIONAL_DIVERGENCE.iter().any(|(p, _)| p == pattern) {
            continue;
        }

        let ours = match Regex::new(pattern) {
            Ok(re) => re,
            // A pattern regexr rejects is covered by the feature matrix, not here.
            Err(_) => continue,
        };
        let theirs = match Pcre2Builder::new().utf(true).ucp(false).build(pattern) {
            Ok(re) => re,
            // PCRE2 lacking the syntax says nothing about regexr's correctness.
            Err(_) => continue,
        };

        for haystack in HAYSTACKS {
            let a = ours.find(haystack).map(|m| (m.start(), m.end()));
            let b = theirs
                .find(haystack.as_bytes())
                .ok()
                .flatten()
                .map(|m| (m.start(), m.end()));
            compared += 1;
            if a != b {
                divergences.push(format!(
                    "  {pattern:?} on {haystack:?}: regexr={a:?} pcre2={b:?}"
                ));
            }
        }
    }

    assert!(compared > 0, "no pattern/haystack pair was compared");
    assert!(
        divergences.is_empty(),
        "{} of {compared} comparisons disagree with PCRE2. Either regexr is \
         wrong, or the difference is deliberate and belongs in \
         INTENTIONAL_DIVERGENCE with a reason:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// Every capture group must report the same span as PCRE2, not just group 0.
///
/// The full-match comparison above misses a whole class of bug: a match can end
/// in the right place while a group inside it is wrong.
#[test]
fn capture_groups_agree_with_pcre2() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        if INTENTIONAL_DIVERGENCE.iter().any(|(p, _)| p == pattern) {
            continue;
        }
        let (Ok(ours), Ok(theirs)) = (
            Regex::new(pattern),
            Pcre2Builder::new().utf(true).ucp(false).build(pattern),
        ) else {
            continue;
        };

        for haystack in HAYSTACKS {
            let Some(ours_caps) = ours.captures(haystack) else {
                continue;
            };
            let Ok(Some(their_caps)) = theirs.captures(haystack.as_bytes()) else {
                continue;
            };

            for group in 0..ours_caps.len() {
                let a = ours_caps.get(group).map(|m| (m.start(), m.end()));
                let b = their_caps.get(group).map(|m| (m.start(), m.end()));
                compared += 1;
                if a != b {
                    divergences.push(format!(
                        "  {pattern:?} on {haystack:?} group {group}: regexr={a:?} pcre2={b:?}"
                    ));
                }
            }
        }
    }

    assert!(compared > 0, "no capture group was compared");
    assert!(
        divergences.is_empty(),
        "{} of {compared} capture-group comparisons disagree with PCRE2:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// Every entry in the exclusion list must still be a real divergence.
///
/// Without this, a fixed difference would sit in the list forever, quietly
/// exempting a pattern that no longer needs it.
#[test]
fn intentional_divergences_are_still_divergent() {
    let mut stale = Vec::new();

    for (pattern, reason) in INTENTIONAL_DIVERGENCE {
        let ours = Regex::new(pattern).ok();
        let theirs = Pcre2Builder::new().utf(true).ucp(false).build(pattern).ok();

        let (Some(ours), Some(theirs)) = (ours, theirs) else {
            // One side rejects the pattern outright, which is itself the
            // divergence the entry documents.
            continue;
        };

        let differs = HAYSTACKS.iter().any(|haystack| {
            let a = ours.find(haystack).map(|m| (m.start(), m.end()));
            let b = theirs
                .find(haystack.as_bytes())
                .ok()
                .flatten()
                .map(|m| (m.start(), m.end()));
            a != b
        });

        if !differs {
            stale.push(format!("  {pattern:?} — listed because: {reason}"));
        }
    }

    assert!(
        stale.is_empty(),
        "{} INTENTIONAL_DIVERGENCE entries now agree with PCRE2 and should be \
         removed from the list:\n{}",
        stale.len(),
        stale.join("\n")
    );
}
