//! Differential sweep of resumed iteration against the executable spec.
//!
//! Every pattern is run through `find_iter` on both builds and compared to
//! `reference::find_from` resumed with the iterator's own rule.

use regexr::hir::translate;
use regexr::parser::parse;
use regexr::RegexBuilder;

fn ceil_char_boundary(text: &str, i: usize) -> usize {
    let mut j = i;
    while j < text.len() && !text.is_char_boundary(j) {
        j += 1;
    }
    j
}

fn reference_ranges(pattern: &str, text: &str) -> Option<Vec<(usize, usize)>> {
    let hir = parse(pattern).and_then(|ast| translate(&ast)).ok()?;
    let ncaps = hir.props.capture_count as usize;
    let bytes = text.as_bytes();

    let mut out = Vec::new();
    let mut last_end = 0usize;
    while last_end <= bytes.len() {
        // Running out of matches ends the iteration; it does not discard the
        // ones already found. Propagating the `None` out of this function
        // instead would make every terminating case look like "no expectation
        // available", and the caller skips those — so the sweep would compare
        // nothing at all except patterns that match more than 64 times.
        let Some((start, end)) = regexr::reference::find_from(&hir.expr, ncaps, bytes, last_end)
        else {
            break;
        };
        out.push((start, end));
        last_end = if start == end {
            ceil_char_boundary(text, end + 1)
        } else {
            ceil_char_boundary(text, end)
        };
        if out.len() > 64 {
            break;
        }
    }
    Some(out)
}

const FRAGMENTS: &[&str] = &[
    "a",
    "ab",
    "[ab]",
    "a+",
    "a*",
    "a?",
    "a{2}",
    "a{1,2}",
    ".",
    "\\w",
    "\\w+",
    "\\s*",
    "(a)",
    "(a)(b)",
    "(a|b)",
    "a|b",
    "(a)\\1",
    "(?:ab)+",
    "a+?",
    "[^a]",
    "(?=a)a",
    "(?!b)a",
    "(?<=a)b",
    "(?<!a)b",
    // Loops over a body that can match the empty string. These decide how many
    // times the loop runs at a position it cannot advance past, which is both a
    // termination question and a capture-value question.
    "(a*)*",
    "(a?)*",
    "()+",
    "(|a)*",
    "(a|)*",
    "(a*)+",
    "((a)*)*",
    "(a{0,2})*",
    "(a*)*b",
    "(a*?)*",
    // Mid-pattern flag changes wrap the rest of the branch in a scoped group,
    // so the AST shape here differs from the same pattern with the flag set up
    // front. Every engine must still agree with the spec.
    "a(?i)b",
    "(?i)a(?-i)b",
    "(?i:a)b",
    "(?i)a|b",
    // Extended mode removes tokens before the parser sees them; the resulting
    // pattern must behave exactly like the whitespace-free spelling.
    "(?x) a b",
    "(?x)a#c\nb",
    "(?x)[a b]",
    // Escapes that resolve to a plain character, including inside a class.
    "\\ a",
    "\\@|a",
    "[\\e\\a]",
    // `\X` expands to a large alternation with assertions inside it; every
    // engine must agree with the spec on where its clusters end.
    "\\X",
    "\\X+",
    "a\\X",
    "\\Xb",
    "(\\X)",
    // Quoting is resolved in the lexer, so these must behave exactly like the
    // escaped spellings next to them.
    "\\Qa.b\\E",
    "\\Qa\\Eb",
    "x\\Q.\\Ey",
];

const PREFIXES: &[&str] = &["", "^", "\\A", "(?m)^", "\\b", "\\B", "(?i)"];
const SUFFIXES: &[&str] = &["", "$", "\\z", "\\b", "\\B", "(?m)$"];

const TEXTS: &[&str] = &[
    "", "a", "aa", "ab", "ba", "abab", "aabb", "a b", " ab ", "a\na", "a\nb\na", "cab", "xaby",
    "aaa\n", "b", "bb", "abc abc", "héllo", "中a中", "AaBb", "\n", "  ", "abababab", "a@b",
    "a\u{1b}b", "a\u{07}b", "a#c\nb",
];

#[test]
fn resumed_iteration_matches_reference() {
    let mut patterns: Vec<String> = Vec::new();
    for p in PREFIXES {
        for f in FRAGMENTS {
            for s in SUFFIXES {
                patterns.push(format!("{p}{f}{s}"));
            }
        }
    }
    // Alternations where only one branch carries the anchor: the property flags
    // are pattern-wide, so an engine that treats "has a start anchor" as
    // "anchored at 0" gets these wrong.
    for p in [
        "^a|b", "a|^b", "(?m)^a|b", "\\ba|b", "^a$|b", "^a|b$", "\\Aa|b",
        // Disjoint first bytes, but one branch carries an anchor. These must
        // keep the ordered engine: the anchor is a condition the DFA family
        // resolves for the pattern as a whole, so "only one branch can match
        // here" is not on its own enough to decide the match (`a|b$` on "aa"
        // reports only 1..2 without it).
        "a|b$", "a$|b", "a|b\\b", "a\\b|b",
    ] {
        patterns.push(p.to_string());
    }

    let mut failures = Vec::new();
    for pattern in &patterns {
        for text in TEXTS {
            let Some(expected) = reference_ranges(pattern, text) else {
                continue;
            };
            for (label, jit) in [("jit", true), ("interp", false)] {
                let Ok(re) = RegexBuilder::new(pattern).jit(jit).build() else {
                    continue;
                };
                let got: Vec<(usize, usize)> =
                    re.find_iter(text).map(|m| (m.start(), m.end())).collect();
                if got != expected {
                    // A first match that already disagrees is a plain `find`
                    // bug; only a divergence that appears later in the sequence
                    // is caused by resuming.
                    let kind = if got.first() == expected.first() {
                        "RESUME"
                    } else {
                        "FIND  "
                    };
                    failures.push(format!(
                        "{kind} [{label}] {pattern:?} on {text:?}: ref={expected:?} got={got:?}"
                    ));
                }
                let caps: Vec<(usize, usize)> = re
                    .captures_iter(text)
                    .filter_map(|c| c.get(0).map(|m| (m.start(), m.end())))
                    .collect();
                if caps != expected {
                    failures.push(format!(
                        "[{label}] captures {pattern:?} on {text:?}: ref={expected:?} got={caps:?}"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} divergences (of {} patterns):\n{}",
        failures.len(),
        patterns.len(),
        failures.join("\n")
    );
}

/// Every group's span, not just the overall match.
///
/// `find` and `captures` run on different engines — an automaton for the bounds,
/// a second pass for the slots — so agreeing on where a match starts and ends
/// says nothing about agreeing on what each group captured. A loop over a body
/// that can match empty is where the two most easily part company: the overall
/// span is the same whether or not the loop takes a final zero-width iteration,
/// but group 1 is not.
#[test]
fn capture_groups_match_reference() {
    let mut patterns: Vec<String> = Vec::new();
    for p in PREFIXES {
        for f in FRAGMENTS {
            for s in SUFFIXES {
                patterns.push(format!("{p}{f}{s}"));
            }
        }
    }

    let mut failures = Vec::new();
    for pattern in &patterns {
        let Ok(hir) = parse(pattern).and_then(|ast| translate(&ast)) else {
            continue;
        };
        let ncaps = hir.props.capture_count as usize;
        if ncaps == 0 {
            continue;
        }
        for text in TEXTS {
            let expected =
                regexr::reference::captures(&hir.expr, ncaps, text.as_bytes()).map(|caps| {
                    // The spec leaves slots for groups it never entered as
                    // `None`, which is what the engines report too.
                    caps.into_iter().collect::<Vec<_>>()
                });
            for (label, jit) in [("jit", true), ("interp", false)] {
                let Ok(re) = RegexBuilder::new(pattern).jit(jit).build() else {
                    continue;
                };
                let got = re.captures(text).map(|caps| {
                    (0..caps.len())
                        .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
                        .collect::<Vec<_>>()
                });
                if got != expected {
                    failures.push(format!(
                        "[{label}] {pattern:?} on {text:?}: ref={expected:?} got={got:?}"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} divergences (of {} patterns):\n{}",
        failures.len(),
        patterns.len(),
        failures.join("\n")
    );
}

/// `find` and `find_iter`'s first element must agree — a resumed-search bug that
/// also broke position 0 would otherwise hide behind matching wrong answers.
#[test]
fn first_iteration_match_equals_find() {
    let mut failures = Vec::new();
    for p in PREFIXES {
        for f in FRAGMENTS {
            for s in SUFFIXES {
                let pattern = format!("{p}{f}{s}");
                for text in TEXTS {
                    for (label, jit) in [("jit", true), ("interp", false)] {
                        let Ok(re) = RegexBuilder::new(&pattern).jit(jit).build() else {
                            continue;
                        };
                        let single = re.find(text).map(|m| (m.start(), m.end()));
                        let first = re.find_iter(text).next().map(|m| (m.start(), m.end()));
                        if single != first {
                            failures.push(format!(
                                "[{label}] {pattern:?} on {text:?}: find={single:?} iter.next={first:?}"
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} divergences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
