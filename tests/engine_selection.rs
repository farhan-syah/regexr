//! Cross-engine agreement and engine-selection regressions.
//!
//! Deliberately NOT gated on the `jit` feature. Each bug below was only visible
//! in one build configuration — the `jit(true)`-without-the-feature downgrade
//! cannot be observed at all in a JIT build — so these have to run everywhere.

/// Leftmost-first must not depend on how many bytes a thread consumes per step.
///
/// The PikeVM's unanchored pass keeps threads from many start positions in
/// flight and orders them by a sequence number — but that number is reassigned
/// every time a thread is rescheduled, so it records how recently a thread moved
/// rather than how early it began. A `CodepointClass` consumes up to four bytes
/// in one step while a byte transition consumes one, so a later-starting thread
/// taking big strides could overtake an earlier-starting one. Here the match at
/// 0 ends before the match at 2 even begins to complete, so nothing but the
/// ordering can explain preferring the latter.
#[test]
fn leftmost_match_wins_regardless_of_step_size() {
    let re = regexr::Regex::new(r"\p{L}\s\S").unwrap();
    let haystack = "x\r\u{4e2d}\te";
    let found = re.find(haystack).map(|m| (m.start(), m.end()));
    assert_eq!(found, Some((0, 5)), "leftmost match must win");

    // The same haystack without the trailing competitor already worked; the bug
    // was that appending it changed the *earlier* answer.
    let re = regexr::Regex::new(r"\p{L}\s\S").unwrap();
    assert_eq!(
        re.find("x\r\u{4e2d}\t").map(|m| (m.start(), m.end())),
        Some((0, 5))
    );
}

/// A quantified alternation must keep its loop.
///
/// `\s` expands to an alternation of UTF-8 shapes, and the tagged-NFA step
/// extractor emits an alternation as its final step and stops — so `\s+` lost
/// the repetition entirely and matched a single byte.
#[test]
fn quantified_alternation_keeps_its_loop() {
    for pattern in [r"\s+", r"\s+\s+", r"(?:a|b)+"] {
        let interpreted = regexr::Regex::new(pattern).unwrap();
        let jitted = regexr::RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .unwrap();
        for haystack in ["  \n", "aab", "  ", "ab"] {
            assert_eq!(
                interpreted.find(haystack).map(|m| (m.start(), m.end())),
                jitted.find(haystack).map(|m| (m.start(), m.end())),
                "pattern {pattern:?} on {haystack:?}: JIT and interpreter disagree"
            );
        }
    }
}

/// Small Unicode classes stay on the byte engines and still match whole
/// characters.
#[test]
fn small_unicode_classes_match_whole_characters() {
    let re = regexr::Regex::new(r"^[^\s<>]+$").unwrap();
    assert!(re.is_match("https://example.com/\u{4e2d}"));
    assert!(!re.is_match("has space"));
    assert!(!re.is_match("a\u{a0}b"), "U+00A0 is Unicode whitespace");

    // Every reported span must sit on character boundaries, which is what the
    // code-point node used to be needed for.
    let re = regexr::Regex::new(r"\s+").unwrap();
    let haystack = "\u{4e2d}\u{2003}\u{3000}\u{4e2d}";
    let m = re.find(haystack).expect("should match the separators");
    assert_eq!(m.as_str(), "\u{2003}\u{3000}");
}

/// Requesting the JIT must never select a *worse* engine than not requesting it.
///
/// `RegexBuilder::jit(true)` runs a separate selection path from `Regex::new`.
/// On a build without the `jit` feature — which is what `cargo bench` produces,
/// since `jit` is not a default feature — that path used to send backreference
/// patterns to the PikeVM. The PikeVM does handle backreferences, but a
/// backreference defeats its per-position state deduplication, so it restarts at
/// every start position, well behind the backtracking engine `Regex::new`
/// picks. Asking for more speed produced less.
#[test]
fn requesting_jit_never_downgrades_the_engine() {
    const BACKREF_PATTERNS: &[&str] = &[r#"(['"])[^'"]*\1"#, r"(\w+)\s+\1", r"(a)\1"];

    for pattern in BACKREF_PATTERNS {
        let jitted = regexr::RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .unwrap();
        assert_ne!(
            jitted.engine_name(),
            "PikeVm",
            "{pattern}: backreferences must reach a backtracking engine"
        );
    }

    // Nothing about the *results* changes when either of the next two
    // regresses, so both assertions are on the selection itself.
    //
    // A pattern that is one repeated byte class is answered by a scan, not by
    // the bit-parallel automaton — so JIT-compiling that automaton, or reaching
    // for the DFA JIT, compiles the very thing the scan exists to replace.
    // `\w+`, the tokenizer pattern, ran ~2x slower under `jit(true)` both ways.
    // A *negated* class is not this shape: it lowers to an ASCII class beside a
    // UTF-8 trie, which is not one byte per iteration.
    const RUN_PATTERNS: &[&str] = &[r"\w+", r"\d+", r"[a-z]+", r"[0-9a-f]{2,}", r"\w{2,8}"];

    for pattern in RUN_PATTERNS {
        let plain = regexr::Regex::new(pattern).unwrap();
        let jitted = regexr::RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .unwrap();
        assert_eq!(
            plain.engine_name(),
            jitted.engine_name(),
            "{pattern}: a repeated byte class must reach the same engine either way"
        );
    }

    // A word boundary next to an effective prefilter is the DFA JIT's worst
    // shape: the prefilter finds a literal, the boundary is what makes those
    // candidates fail ("the" inside "their"), and the DFA JIT has no anchored
    // entry, so a failed candidate becomes a scan of the rest of the input
    // instead of a rejection. `\bthe\b` ran ~2.5x slower under `jit(true)` than
    // under `Regex::new` until both paths agreed here.
    const BOUNDARY_PATTERNS: &[&str] = &[r"\bthe\b", r"\bword\b", r"\bfoo\b\s"];

    for pattern in BOUNDARY_PATTERNS {
        let plain = regexr::Regex::new(pattern).unwrap();
        let jitted = regexr::RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .unwrap();
        assert_eq!(
            plain.engine_name(),
            jitted.engine_name(),
            "{pattern}: a literal guarded by a word boundary must reach the same \
             engine either way"
        );
    }

    // Whatever each path selects, they must agree on what they find.
    const PATTERNS: &[&str] = &[
        r#"(['"])[^'"]*\1"#,
        r"(\w+)\s+\1",
        r"\s+",
        r"\S+",
        r"[^\s<>]+",
        r"\w+(?=ing\b)",
        r"(cat|dog)+",
        r"\bword\b",
    ];
    const HAYSTACKS: &[&str] = &[
        r#"let x = "hello"; y = 'z';"#,
        "the the  word word",
        "running and singing",
        "cat dog catdog",
        "  \t\u{a0}  ",
        "https://example.com/a<b>",
        "",
    ];

    for pattern in PATTERNS {
        let plain = regexr::Regex::new(pattern).unwrap();
        let jitted = regexr::RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .unwrap();
        for haystack in HAYSTACKS {
            let a: Vec<_> = plain
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect();
            let b: Vec<_> = jitted
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect();
            assert_eq!(
                a,
                b,
                "{pattern:?} on {haystack:?}: plain ({}) and jit(true) ({}) disagree",
                plain.engine_name(),
                jitted.engine_name()
            );
        }
    }
}
