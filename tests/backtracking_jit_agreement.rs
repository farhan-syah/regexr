//! The backtracking JIT must agree with its interpreter.
//!
//! Backreference patterns are the only ones that reach either engine, and the
//! JIT is a second implementation of the same search written in assembly, per
//! architecture. Nothing else in the suite compares the two directly: a wrong
//! instruction shows up as a missing or shifted match, and every other test that
//! would notice runs on whichever engine the build happens to select.
//!
//! The interpreter is the reference here because it is ordinary Rust, covered by
//! the rest of the suite, and identical on every target.
//!
//! Running this on a non-x86-64 host is the point — the two backends share no
//! code. `cargo build --target aarch64-unknown-linux-musl` plus a `qemu-aarch64`
//! runner is enough to exercise the ARM64 backend from an x86-64 machine.
//!
//! The engine-name assertion needs the `jit` feature; the sweep itself does not
//! and runs in every configuration.

use regexr::{Regex, RegexBuilder};

/// Backreference patterns spanning what the generated code has to get right:
/// leading classes, leading literals, leading anchors, nested and repeated
/// groups, and a group that may not participate.
const PATTERNS: &[&str] = &[
    r#"(['"])[^'"]*\1"#,
    r"(\w+)@\1",
    r"(\w+)\s+\1",
    r"(a)(b)\2\1",
    r"(a+)b\1",
    r"([a-y]+)\d\1",
    r"^(\w+)-\1",
    r"\b(\d+)\b-\1",
    r"(?:xy)+(\d)\1",
    r"(z)|(\d)\2",
    r"((a)(b))c\3\2",
    r"(.)\1",
    r"(\d{2})-\1",
    r"prefix-(\w)\1",
    r"(a|b)c\1",
    r"(\w)\1{2}",
    r"([abc])x\1y",
];

const HAYSTACKS: &[&str] = &[
    "",
    "a",
    "abab",
    "aab",
    "aabb",
    "let x = \"hello\"; let y = 'world';",
    "str = 'Python string with \"nested\" quotes';",
    "no backreference here at all",
    "word word again",
    "user@user and a@a",
    "12-12 and 34-56",
    "prefix-zz prefix-ab",
    "xy5 xy55 xyxy77",
    "zzzz",
    "éé café",
    "中中",
    "aXa bXb",
    "ab-ab cd-ef",
    "999",
    "  \t  ",
    "\n\n",
    "abcabc",
    "axayaxb bxbycxcy",
    "The quick brown fox jumps over the lazy dog",
];

fn spans(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    re.find_iter(text).map(|m| (m.start(), m.end())).collect()
}

fn groups(re: &Regex, text: &str) -> Option<Vec<Option<(usize, usize)>>> {
    re.captures(text).map(|caps| {
        (0..caps.len())
            .map(|index| caps.get(index).map(|m| (m.start(), m.end())))
            .collect()
    })
}

/// Every span and every group must come out the same on both engines.
#[test]
fn backtracking_jit_agrees_with_its_interpreter() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        let interpreted = Regex::new(pattern).expect("pattern is supported");
        let jitted = RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .expect("pattern is supported");

        for haystack in HAYSTACKS {
            let (ours, theirs) = (spans(&interpreted, haystack), spans(&jitted, haystack));
            compared += 1;
            if ours != theirs {
                divergences.push(format!(
                    "  find_iter {pattern:?} on {haystack:?}: interpreter={ours:?} jit={theirs:?}"
                ));
            }

            let (ours, theirs) = (groups(&interpreted, haystack), groups(&jitted, haystack));
            compared += 1;
            if ours != theirs {
                divergences.push(format!(
                    "  captures {pattern:?} on {haystack:?}: interpreter={ours:?} jit={theirs:?}"
                ));
            }
        }
    }

    assert!(compared > 0, "no pattern/haystack pair was compared");
    assert!(
        divergences.is_empty(),
        "{} of {compared} comparisons disagree with the interpreter, so the \
         generated code is wrong on this target:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// A long run of bytes no match can start with must not be a match.
///
/// This is the case the generated start-position scan exists for, and the case
/// where a wrong scan is most likely to run past the input or skip a real match.
#[test]
fn a_long_dead_run_is_scanned_without_finding_a_match() {
    let pattern = r"([a-y]+)\d\1";
    let jitted = RegexBuilder::new(pattern).jit(true).build().unwrap();

    let dead = "z".repeat(64 * 1024);
    assert!(jitted.find(&dead).is_none());

    // The same run with one real match buried in it, which the scan has to stop
    // at rather than skip.
    let buried = format!("{dead}abc1abc{dead}");
    let interpreted = Regex::new(pattern).unwrap();
    assert_eq!(
        jitted.find(&buried).map(|m| (m.start(), m.end())),
        interpreted.find(&buried).map(|m| (m.start(), m.end()))
    );
}

/// The patterns above must actually reach the JIT, or the sweep proves nothing.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn the_swept_patterns_reach_the_backtracking_jit() {
    let reached = PATTERNS
        .iter()
        .filter(|pattern| {
            RegexBuilder::new(pattern)
                .jit(true)
                .build()
                .unwrap()
                .engine_name()
                == "BacktrackingJit"
        })
        .count();

    assert!(
        reached * 2 >= PATTERNS.len(),
        "only {reached} of {} patterns reach BacktrackingJit, so the agreement \
         sweep is mostly comparing the interpreter with itself",
        PATTERNS.len()
    );
}
