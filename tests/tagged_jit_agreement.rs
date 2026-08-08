//! The tagged-NFA JIT must agree with its interpreter, step for step.
//!
//! `jit_must_defer` decides which step programs the JIT may emit. Loosening it
//! trades a guaranteed-correct interpreter for generated code, so every shape it
//! now admits — greedy quantifiers carrying their own lookahead — is compared
//! against the interpreter here. A divergence means the guard was loosened too
//! far, not that a test is too strict.
//!
//! The engine-name assertions need the `jit` feature; the agreement sweep does
//! not and runs in every configuration.

use regexr::{Regex, RegexBuilder};

/// Shapes that reach `PatternStep::GreedyPlusLookahead` / `GreedyStarLookahead`,
/// plus the adjacent-greedy shapes that must stay deferred.
const PATTERNS: &[&str] = &[
    r"[\r\n]+(?=\S)",
    r"\S+\S+\S",
    r"\w+(?=ing\b)",
    r"\s+(?!\S)",
    r"\S+(?=\s)",
    r"a+(?=b)",
    r"a*(?=b)",
    r"a+(?!b)",
    r"a*(?!b)",
    r"[a-z]+(?=[0-9])",
    r"[a-z]*(?=[0-9])",
    r"\w+\w+",
    r"\s+\s+",
    r"\S+\s+\S+",
    r"a+b+",
    r"a+b+c",
    r"[\r\n]+(?=\S)x?",
    r"\w+(?=\d)\d",
    r"a+(?=ab)ab",
    r"\s*(?=\S)\S+",
    r"[ab]+(?=c)",
    r"[ab]*(?!c)",
    r"\d+(?=px\b)",
    r"\w+(?=\.)",
    r"[^,]+(?=,)",
    // Lookaheads whose body is a Unicode class, so the assertion needs a
    // codepoint check rather than a byte one. On a non-ASCII follower that check
    // is a call, and the greedy loop keeps its backtracking floor in a
    // caller-saved register across it.
    r"a+(?!\S)",
    r"a+(?=\S)",
    r"\w+(?!\p{L})",
    // The tokenizer pattern this engine was built for.
    r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+",
];

const INPUTS: &[&str] = &[
    "",
    "a",
    "aa",
    "ab",
    "aab",
    "abb",
    "aabb",
    "abc",
    "aaa",
    "b",
    "c",
    "abcabc",
    "\r\n\r\nx",
    "\r\n ",
    "\n\nabc",
    "  \t x",
    "hello world",
    "running sing",
    "singing in the rain",
    "a1b2",
    "x  ",
    "  ",
    "\n",
    "foo bar baz",
    "'s don't  won't",
    "abc123 def",
    "  \n\n  x",
    "aXbXc",
    "test(ing)",
    "12px 34px",
    "a,b,,c",
    "file.name.ext",
    "\u{4e2d}\u{6587} \u{3042}",
    // A greedy run ending against a multi-byte character, which is what sends
    // the lookahead's codepoint check down its calling path.
    "aa\u{4e2d}",
    "e.aae00 a\u{4e2d}\u{4e2d}\n",
    "a\u{3000}",
    "MiXeD CaSe 42",
];

#[test]
fn tagged_jit_agrees_with_its_interpreter() {
    let mut divergences = Vec::new();
    let mut compared = 0usize;

    for pattern in PATTERNS {
        let (Ok(interpreted), Ok(jitted)) = (
            Regex::new(pattern),
            RegexBuilder::new(pattern).jit(true).build(),
        ) else {
            continue;
        };

        for input in INPUTS {
            let a: Vec<_> = interpreted
                .find_iter(input)
                .map(|m| (m.start(), m.end()))
                .collect();
            let b: Vec<_> = jitted
                .find_iter(input)
                .map(|m| (m.start(), m.end()))
                .collect();
            compared += 1;
            if a != b {
                divergences.push(format!(
                    "  {pattern:?} on {input:?}: interpreter={a:?} jit={b:?} (jit engine {})",
                    jitted.engine_name()
                ));
                continue;
            }

            // Captures too: a span can be right while a group inside it is not.
            let ca = interpreted.captures(input).map(|c| {
                (0..c.len())
                    .map(|i| c.get(i).map(|m| (m.start(), m.end())))
                    .collect::<Vec<_>>()
            });
            let cb = jitted.captures(input).map(|c| {
                (0..c.len())
                    .map(|i| c.get(i).map(|m| (m.start(), m.end())))
                    .collect::<Vec<_>>()
            });
            if ca != cb {
                divergences.push(format!(
                    "  {pattern:?} on {input:?} captures: interpreter={ca:?} jit={cb:?}"
                ));
            }
        }
    }

    assert!(compared > 0, "no pattern/input pair was compared");
    assert!(
        divergences.is_empty(),
        "{} of {compared} comparisons disagree between the tagged-NFA JIT and \
         its interpreter:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// The shapes that stay deferred must actually stay deferred.
///
/// Without this the guard could be loosened to nothing and the test above would
/// still pass by comparing the interpreter against itself.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn generic_backtracking_shapes_stay_on_the_interpreter() {
    // Two quantifiers, or a quantifier plus a lookaround the generic step
    // sequencing has to satisfy by giving characters back.
    for pattern in [
        r"a+(?=b)c+",
        r"a+(?=b)b+(?=c)",
        r"[ab]+(?=c)[de]+",
        r"(?=abc)a+x",
        r"a+b(?=c)",
        r"x(?=y)a+",
        r"a+(?=b)(?=bc)",
    ] {
        let jitted = RegexBuilder::new(pattern).jit(true).build().unwrap();
        assert_eq!(
            jitted.engine_name(),
            "TaggedNfa",
            "{pattern} needs generic backtracking and must not be JIT-compiled"
        );
    }
}

/// A greedy quantifier carrying its own lookahead must reach the JIT.
///
/// This is the point of the loosened guard: without it `\w+(?=ing\b)` silently
/// ran on the interpreter even with the JIT requested.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn greedy_with_attached_lookahead_is_jit_compiled() {
    for pattern in [r"\w+(?=ing\b)", r"a+(?=b)", r"[ab]+(?=c)", r"[0-9]+(?=px)"] {
        let jitted = RegexBuilder::new(pattern).jit(true).build().unwrap();
        assert_eq!(
            jitted.engine_name(),
            "TaggedNfaJit",
            "{pattern} carries its lookahead in one step and should be JIT-compiled"
        );
    }
}
