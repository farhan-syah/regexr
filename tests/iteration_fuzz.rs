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
        let (start, end) = regexr::reference::find_from(&hir.expr, ncaps, bytes, last_end)?;
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
    "a", "ab", "[ab]", "a+", "a*", "a?", "a{2}", "a{1,2}", ".", "\\w", "\\w+", "\\s*", "(a)",
    "(a)(b)", "(a|b)", "a|b", "(a)\\1", "(?:ab)+", "a+?", "[^a]", "(?=a)a", "(?!b)a", "(?<=a)b",
    "(?<!a)b",
];

const PREFIXES: &[&str] = &["", "^", "\\A", "(?m)^", "\\b", "\\B", "(?i)"];
const SUFFIXES: &[&str] = &["", "$", "\\z", "\\b", "\\B", "(?m)$"];

const TEXTS: &[&str] = &[
    "", "a", "aa", "ab", "ba", "abab", "aabb", "a b", " ab ", "a\na", "a\nb\na", "cab", "xaby",
    "aaa\n", "b", "bb", "abc abc", "héllo", "中a中", "AaBb", "\n", "  ", "abababab",
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
