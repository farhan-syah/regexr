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
/// plus the adjacent-greedy shapes that must stay deferred, plus lookbehind over
/// classes with more than one candidate width — the JIT emits one attempt per
/// candidate there, and picking the wrong one returns wrong spans rather than
/// failing loudly.
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
    // Lookbehind, both polarities, over classes whose members encode to more
    // than one UTF-8 width. The JIT emits one attempt per candidate total, so
    // these are the shapes where picking the wrong width — or ordering the
    // attempts wrongly, or short-circuiting the negative case on the first
    // failing width — returns wrong spans rather than crashing.
    r"(?<=\s)\w+",
    r"(?<!\s)\w+",
    r"(?<=\p{L})x",
    r"(?<!\p{L})x",
    r"(?<=\s)x",
    r"(?<=\p{L})\w",
    // A class spanning all four UTF-8 widths: 'a', Cyrillic 'а', CJK '中',
    // emoji '😀' — four candidate widths, so four attempts.
    "(?<=[a\u{430}\u{4e2d}\u{1F600}])x",
    "(?<![a\u{430}\u{4e2d}\u{1F600}])x",
    // Two candidate widths (1 for 'a', 2 for 'é') that can succeed against
    // *different* bytes: on "aXb" the 2-byte attempt starts on the 'a', matches
    // it as a 1-byte codepoint and stops one byte short of the assertion
    // position. Only requiring the walk to land exactly on that position
    // rejects it; an implementation that just runs out of steps reports a match
    // the interpreter does not.
    "(?<=[a\u{e9}])b",
    "(?<![a\u{e9}])b",
    // A multi-width class followed by a fixed byte, so the candidate totals
    // (2 and 3) both sit above zero and the wrong one lands off by one.
    "(?<=[a\u{e9}]x)y",
    "(?<![a\u{e9}]x)y",
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
    // A lookbehind is checked by walking forwards from `pos - width`, so a wrong
    // width shows up as reading from the middle of the character *immediately
    // before* the assertion position. Every input above puts its multi-byte
    // characters at the end or next to each other, which never exercises that;
    // these put one directly before the ASCII the assertion is anchored on, at
    // each of the four UTF-8 widths.
    "\u{430}x",
    "\u{4e2d}x",
    "\u{1F600}x",
    "a\u{4e2d}x",
    "\u{4e2d}\u{4e2d}x",
    "x\u{4e2d}x\u{4e2d}",
    // Multi-byte whitespace (ideographic space, NBSP) directly before word
    // characters, which is the `\s` lookbehind case: widths 1, 2 and 3 for one
    // class.
    "\u{3000}word",
    "a\u{a0}word b",
    "\u{3000}x \u{a0}x x",
    // 'é' (2 bytes) and 'a' (1 byte) before the assertion point, plus the
    // one-byte-short case where a wrong candidate width can match unrelated
    // bytes and stop early.
    "\u{e9}b",
    "ab",
    "aXb",
    "a\u{e9}b",
    "\u{e9}\u{e9}b",
    "\u{e9}xy",
    "axy",
    "aXxy",
    "1x x \u{4e2d}x",
    // The wrong-width trap for a class that really is compiled as a codepoint
    // class (`\p{L}`, `\s`): the character before the assertion point does NOT
    // satisfy it, but the one before *that* does. A wide candidate starts on the
    // satisfying character, matches it at its own (narrower) length and stops a
    // byte short of the assertion position, so only requiring the walk to land
    // exactly on that position keeps these non-matches non-matching.
    "a.x",
    "ab.x",
    "a x",
    "1.x",
    " .x",
    "\u{4e2d}.x",
    "\u{4e2d} x",
    "\u{1F600}.x",
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

/// A lookaround whose inner pattern ends in a zero-width assertion must reach
/// the JIT.
///
/// The inner extractors walk to the inner NFA's match state, and an assertion
/// compiled onto that state has to be read before they stop. If one is missed,
/// the assertion tally no longer matches the NFA and the JIT correctly refuses —
/// so these patterns silently drop to a slower engine rather than returning
/// wrong answers. Assert the engine to catch that.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn lookaround_with_trailing_assertion_is_jit_compiled() {
    // Word boundaries and the start anchor are emitted by both backends. The
    // end/line anchors inside a *lookbehind* are x86-64 only: the AArch64
    // lookbehind refuses them, because emitting them there produced code that
    // faulted at run time (`(?<=a\Z)x?`). Refusing keeps those patterns on the
    // interpreter, which is correct — so this asserts the coverage each backend
    // actually has rather than the coverage we would like it to have.
    let mut patterns = vec![
        r"(?<=a\b)x",
        r"(?<=a\B)x",
        r"(?<!a\b)x",
        r"(?<=^a)x",
        r"x(?=a\b)",
        r"x(?=a\B)",
        r"x(?!a\b)",
        r"x(?=a$)",
        r"\w(?=\w\b)",
    ];
    if cfg!(target_arch = "x86_64") {
        patterns.extend([r"(?<=a$)b", r"(?<!a$)x"]);
    }
    for pattern in patterns {
        let jitted = RegexBuilder::new(pattern).jit(true).build().unwrap();
        assert_eq!(
            jitted.engine_name(),
            "TaggedNfaJit",
            "{pattern} is expressible as steps and should be JIT-compiled"
        );
    }
}
