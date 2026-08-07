//! The one-pass capture engine must report the same groups however it is run.
//!
//! `OnePass` answers captures in a single deterministic scan, and on x86-64 with
//! the `jit` feature that scan is compiled to machine code rather than
//! interpreted. The two are separate implementations of the same machine, and
//! the generated one has its own stack frame, its own slot representation and
//! its own deferred-snapshot bookkeeping — none of which any other test targets.
//!
//! The oracle is the `regex` crate, so this checks both implementations against
//! something outside the crate rather than against each other. It runs in every
//! configuration; without the feature it is checking the interpreter, which is
//! also worth doing.
//!
//! Haystacks are ASCII on purpose: regexr's `\w`/`\b` are ASCII-only by design
//! and the `regex` crate's are Unicode, so non-ASCII subjects would report a
//! deliberate dialect choice as a failure. `pcre2_conformance` covers non-ASCII.

/// Capture patterns that reach the one-pass engine, chosen for the paths the
/// generated code has that the interpreter's shape hides: groups that close on a
/// transition, a group that may not participate, a greedy tail that re-reaches
/// its match at every byte, and enough groups to exercise the slot frame.
const PATTERNS: &[&str] = &[
    r"(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2}) \[(\w+)\] (.+)",
    r"(\d{4})-(\d{2})-(\d{2})",
    r"(\w+)@(\w+)\.(\w+)",
    r"(\d+)-(\d+)",
    r"([a-z]+)([0-9]*)",
    r"(a)(b)?(c)",
    r"((a)(b))c",
    r"(a+)(b+)(c+)",
    r"(\w+):(\w+):(\w+):(\w+):(\w+)",
    r"x(\d*)y",
    r"(.+)",
    r"(.+)!",
    r"([^,]+),([^,]+)",
    r"(\d)(\d)(\d)(\d)(\d)(\d)",
    r"a(\d+)?b",
    r"(ab)+c",
];

const HAYSTACKS: &[&str] = &[
    "",
    "abc",
    "2024-01-15 09:30:45 [INFO] Request processed successfully",
    "2024-12-31 23:59:59 [CRITICAL] Database query failed while committing",
    "user@site.com",
    "12-34",
    "abc123",
    "ac",
    "xy",
    "x123y",
    "one,two",
    "123456",
    "ab",
    "a9b",
    "ababc",
    "aaabbbccc",
    "a:b:c:d:e",
    "hello world!",
    "no match here at all",
    "   ",
    "9",
    "a",
];

fn groups(text: &str, caps: Option<regexr::Captures<'_>>) -> Option<Vec<Option<(usize, usize)>>> {
    let _ = text;
    caps.map(|caps| {
        (0..caps.len())
            .map(|index| caps.get(index).map(|m| (m.start(), m.end())))
            .collect()
    })
}

fn their_groups(caps: Option<regex::Captures<'_>>) -> Option<Vec<Option<(usize, usize)>>> {
    caps.map(|caps| {
        (0..caps.len())
            .map(|index| caps.get(index).map(|m| (m.start(), m.end())))
            .collect()
    })
}

/// Every group of the first match must agree with an independent engine.
#[test]
fn one_pass_capture_groups_agree_with_an_independent_engine() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        let ours = regexr::Regex::new(pattern).expect("pattern is supported");
        let theirs = regex::Regex::new(pattern).expect("pattern is supported");

        for haystack in HAYSTACKS {
            let a = groups(haystack, ours.captures(haystack));
            let b = their_groups(theirs.captures(haystack));
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
        "{} of {compared} capture comparisons disagree:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// The same across a whole sequence, where the engine is re-entered at every
/// resume position and the slot state has to start clean each time.
#[test]
fn one_pass_capture_sequences_agree_with_an_independent_engine() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        let ours = regexr::Regex::new(pattern).expect("pattern is supported");
        let theirs = regex::Regex::new(pattern).expect("pattern is supported");

        for haystack in HAYSTACKS {
            let a: Vec<_> = ours
                .captures_iter(haystack)
                .map(|caps| {
                    (0..caps.len())
                        .map(|index| caps.get(index).map(|m| (m.start(), m.end())))
                        .collect::<Vec<_>>()
                })
                .collect();
            let b: Vec<_> = theirs
                .captures_iter(haystack)
                .map(|caps| {
                    (0..caps.len())
                        .map(|index| caps.get(index).map(|m| (m.start(), m.end())))
                        .collect::<Vec<_>>()
                })
                .collect();
            compared += 1;
            if a != b {
                divergences.push(format!(
                    "  {pattern:?} on {haystack:?}: regexr={a:?} regex={b:?}"
                ));
            }
        }
    }

    assert!(compared > 0, "no sequence was compared");
    assert!(
        divergences.is_empty(),
        "{} of {compared} sequence comparisons disagree:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// A long greedy tail: the match is re-reached at every byte, which is the path
/// the deferred snapshot exists for and the one most likely to leave the slots
/// holding a position from the wrong iteration.
#[test]
fn a_long_greedy_tail_reports_the_last_position() {
    let text = format!("head {}", "z".repeat(64 * 1024));
    let ours = regexr::Regex::new(r"(head) (z+)").unwrap();
    let theirs = regex::Regex::new(r"(head) (z+)").unwrap();

    let a = groups(&text, ours.captures(&text));
    let b = their_groups(theirs.captures(&text));
    assert_eq!(a, b, "a 64 KB greedy tail must report the same groups");
}
