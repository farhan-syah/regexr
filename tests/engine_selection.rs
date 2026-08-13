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

    // Nothing about the *results* changes when any of the next three regresses,
    // so all three assertions are on the selection itself.

    // A pattern that is one repeated byte class is answered by a scan, not by
    // the bit-parallel automaton — so JIT-compiling that automaton, or reaching
    // for the DFA JIT, compiles the very thing the scan exists to replace.
    // `\w+`, the tokenizer pattern, ran ~2x slower under `jit(true)` both ways.
    // A *negated* class is not this shape: it lowers to an ASCII class beside a
    // UTF-8 trie, which is not one byte per iteration.
    const RUN_PATTERNS: &[&str] = &[r"\w+", r"\d+", r"[a-z]+", r"[0-9a-f]{2,}", r"\w{2,8}"];

    // An alternation belongs on the DFA, whose step is one table lookup however
    // many branches there are, rather than Shift-Or, which walks the live
    // positions. Both selection paths have to agree: the rule landed in
    // `select_engine_from_hir` first, and until `compile_with_jit` matched it the
    // tokenizer's alternation ran ~20% slower under `jit(true)`. Negated classes
    // lower to an alternation, so `[^>]+` is one of these too.
    const ALTERNATION_PATTERNS: &[&str] = &[
        r#"[a-zA-Z_][a-zA-Z0-9_]*|[0-9]+(?:\.[0-9]+)?|[+\-*/=<>!&|^%]+|[(){}\[\];,.]|"[^"]*"|'[^']*'"#,
        r"[^>]+",
        r"<[^>]+>",
        r"https?://[^\s<>]+",
        r"error|warning|critical|fatal",
    ];

    // A word boundary next to an effective prefilter is the DFA JIT's worst
    // shape: the prefilter finds a literal, the boundary is what makes those
    // candidates fail ("the" inside "their"), and the DFA JIT has no anchored
    // entry, so a failed candidate becomes a scan of the rest of the input
    // instead of a rejection. `\bthe\b` ran ~2.5x slower under `jit(true)`.
    const BOUNDARY_PATTERNS: &[&str] = &[r"\bthe\b", r"\bword\b", r"\bfoo\b\s"];

    let selection: &[(&str, &[&str])] = &[
        ("a repeated byte class", RUN_PATTERNS),
        ("an alternation", ALTERNATION_PATTERNS),
        ("a literal guarded by a word boundary", BOUNDARY_PATTERNS),
    ];
    for (shape, patterns) in selection {
        for pattern in *patterns {
            let plain = regexr::Regex::new(pattern).unwrap();
            let jitted = regexr::RegexBuilder::new(pattern)
                .jit(true)
                .build()
                .unwrap();
            assert_eq!(
                plain.engine_name(),
                jitted.engine_name(),
                "{pattern}: {shape} must reach the same engine either way"
            );
        }
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

/// A codepoint class must reach the tagged NFA in a plain, non-`jit` build.
///
/// `\p{L}` and friends lower to a `CodepointClass` instruction, which the DFA
/// and Shift-Or families cannot execute at all — so the choice is between the
/// PikeVM and the tagged NFA, and `select_engine_from_hir` used to return the
/// PikeVM for every one of them. `\p{L}+` extracts to a single greedy codepoint
/// step on the tagged path against ~481 instructions per byte of PikeVM thread
/// bookkeeping, and `compile_with_jit` had been taking that path all along.
///
/// Asserted on the selection, not on the output: the PikeVM answers all of these
/// correctly too, so no `find()` result distinguishes the two engines.
#[test]
fn codepoint_class_patterns_select_the_tagged_nfa() {
    const CODEPOINT_CLASS_PATTERNS: &[&str] = &[
        r"\p{L}+",
        r"\p{N}{1,3}",
        r"\P{L}+",
        // The shape the cl100k tokenizer pattern is built from.
        r"[^\r\n\p{L}\p{N}]?\p{L}+",
    ];

    for pattern in CODEPOINT_CLASS_PATTERNS {
        let re = regexr::Regex::new(pattern).unwrap();
        assert_eq!(
            re.engine_name(),
            "TaggedNfa",
            "{pattern}: a codepoint class belongs on the tagged NFA, not the PikeVM"
        );
    }

    // The engine change must not move the answers.
    let re = regexr::Regex::new(r"\p{L}+").unwrap();
    let spans: Vec<_> = re
        .find_iter("ab \u{4e2d}\u{6587}1 \u{3b1}")
        .map(|m| (m.start(), m.end()))
        .collect();
    assert_eq!(spans, vec![(0, 2), (3, 9), (11, 13)]);
}

/// The tagged route keeps the PikeVM as its own fallback.
///
/// Step extraction is budgeted (`MAX_EXTRACTED_STEPS`): an `Alt` copies every
/// step after it into both branches, so k sequential alternation groups would
/// emit ~2^k steps. Past the budget the extractor returns `None` and
/// `TaggedNfaEngine` runs the PikeVM it constructs regardless — which is why
/// routing codepoint classes here does not give up the linear bound. This pins
/// both halves: that the pattern really does decline extraction, and that it
/// still selects the tagged engine and answers correctly through the fallback.
#[test]
fn codepoint_class_pattern_declining_extraction_falls_back_to_the_pikevm() {
    use regexr::hir::translate;
    use regexr::nfa::tagged::StepExtractor;
    use regexr::parser::parse;

    // A codepoint class followed by 24 sequential alternation groups.
    let groups = ["(?:ab|cd)", "(?:ef|gh)", "(?:ij|kl)", "(?:mn|op)"];
    let mut pattern = String::from(r"\p{L}");
    let mut haystack = String::from("x");
    for i in 0..24 {
        let group = groups[i % groups.len()];
        pattern.push_str(group);
        // The group's first branch, e.g. "ab" out of "(?:ab|cd)".
        haystack.push_str(&group[3..5]);
    }

    let hir = parse(&pattern)
        .and_then(|ast| translate(&ast))
        .expect("pattern should compile");
    let nfa = regexr::nfa::compile(&hir).expect("NFA should build");
    assert!(
        StepExtractor::new(&nfa).extract().is_none(),
        "the fallback under test is only exercised while this shape declines extraction"
    );

    let re = regexr::Regex::new(&pattern).expect("pattern should compile");
    assert_eq!(
        re.engine_name(),
        "TaggedNfa",
        "declining extraction is the tagged engine's internal fallback, not a different selection"
    );
    assert_eq!(
        re.find(&haystack).map(|m| (m.start(), m.end())),
        Some((0, haystack.len())),
        "the PikeVM fallback must still find the match"
    );
    assert!(!re.is_match("x ab cd ef gh"));
}

// =============================================================================
// Byte-Wise Quantifiers Crossing Multi-Byte Codepoints
// =============================================================================
//
// A `CodepointClass` transition consumes up to four bytes at once while a byte
// class walks the same span one byte at a time. Both threads must stay
// comparable on pattern priority, or the one taking bigger strides wins a
// contest it should lose and the match comes out short.

#[test]
fn byte_class_star_outranks_a_codepoint_class_it_could_consume() {
    // `[^\s]*` should take the first `½`, leaving the second for `\p{N}`.
    let re = regexr::Regex::new(r"[^\s]*\p{N}").unwrap();
    assert_eq!(re.find("\u{BD}\u{BD}").map(|m| m.end()), Some(4));
    assert_eq!(re.find("\u{BD}\u{BD}\u{BD}").map(|m| m.end()), Some(6));
    assert_eq!(re.find("\u{BD}1").map(|m| m.end()), Some(3));
    // Single-byte codepoints were never affected; keep them covered.
    assert_eq!(re.find("12").map(|m| m.end()), Some(2));
}

#[test]
fn byte_class_star_crossing_three_byte_codepoints() {
    let re = regexr::Regex::new(r"[^\s]*\p{L}").unwrap();
    assert_eq!(re.find("\u{E9}\u{E9}").map(|m| m.end()), Some(4));
    assert_eq!(re.find("\u{4E2D}\u{4E2D}").map(|m| m.end()), Some(6));
}

#[test]
fn plus_quantifier_before_a_codepoint_class_is_unaffected() {
    // `+` consumes before the codepoint class can start, so no contest arises.
    let re = regexr::Regex::new(r"[^\s]+\p{N}").unwrap();
    assert_eq!(re.find("\u{BD}\u{BD}").map(|m| m.end()), Some(4));
}

// =============================================================================
// `EagerDfa::from_lazy` declining its materialization budget must not change
// what the pattern matches.
// =============================================================================
// `(?:a?){n}` selects `EngineType::LazyDfa`, which without anchors or a large
// Unicode class used to always materialize into an `EagerDfa` eagerly. Past
// n≈1000 that BFS is too expensive to run at all (see
// `src/dfa/eager/shared.rs::MATERIALIZATION_WORK_BUDGET`), so it now declines
// partway through and the engine falls back to `LazyDfa` instead. `LazyDfa`
// computes the exact same states on demand, so the two must agree on every
// match — this locks that in at a small `n`, where the eager path still
// finishes, so both engines are exercised for real (not just the fallback).
#[test]
fn nullable_repetition_matches_agree_with_the_reference() {
    use regexr::hir::translate;
    use regexr::parser::parse;

    let pattern = "(?:a?){20}";
    let hir = parse(pattern)
        .and_then(|ast| translate(&ast))
        .expect("pattern should compile");
    let ncaps = hir.props.capture_count as usize;

    let re = regexr::Regex::new(pattern).expect("pattern should compile");

    let haystacks = [
        String::new(),
        "a".to_string(),
        "aaaa".to_string(),
        "a".repeat(20),
        "b".to_string(),
        // A mismatch in the middle of an otherwise-matching run.
        "aaaabaaaa".to_string(),
    ];

    for haystack in &haystacks {
        let bytes = haystack.as_bytes();
        let expected_find = regexr::reference::find(&hir.expr, ncaps, bytes);
        let expected_captures = regexr::reference::captures(&hir.expr, ncaps, bytes);

        let found = re.find(haystack).map(|m| (m.start(), m.end()));
        assert_eq!(
            found, expected_find,
            "{pattern:?} on {haystack:?}: find() disagrees with reference"
        );

        let captured = re
            .captures(haystack)
            .and_then(|caps| caps.get(0))
            .map(|m| (m.start(), m.end()));
        let expected_group0 = expected_captures.and_then(|caps| caps[0]);
        assert_eq!(
            captured, expected_group0,
            "{pattern:?} on {haystack:?}: captures() disagrees with reference"
        );
    }
}

// =============================================================================
// `Nfa::precompute_epsilon_closures` declining its own budget must not change
// what the pattern matches either.
// =============================================================================
// `(?:a?){n}` is also the pattern that makes `precompute_epsilon_closures`
// (`src/nfa/state.rs`) blow up: because every `a?` can be skipped,
// closure(start_i) reaches every downstream skip state, so Σ|closure| is
// Θ(n²) — about 8·n² measured. `EPSILON_CLOSURE_BUDGET` caps that sum and
// aborts the precomputation once n grows past roughly 354, leaving
// `epsilon_closures` as `None` for this pattern. `Nfa::epsilon_closure` then
// takes its on-the-fly DFS fallback instead of the cache — this locks in that
// the fallback computes the exact same closures, at an n picked well past
// that cutoff. The haystacks stay short: `reference::find`/`captures` recurse
// once per repetition of the pattern, not per haystack byte, so `n` (not
// haystack length) is what has to stay stack-safe here.
#[test]
fn nullable_repetition_matches_agree_with_the_reference_past_the_closure_budget() {
    use regexr::hir::translate;
    use regexr::parser::parse;

    let pattern = "(?:a?){500}";
    let hir = parse(pattern)
        .and_then(|ast| translate(&ast))
        .expect("pattern should compile");
    let ncaps = hir.props.capture_count as usize;

    let re = regexr::Regex::new(pattern).expect("pattern should compile");

    let haystacks = [
        String::new(),
        "a".to_string(),
        "aaaa".to_string(),
        "a".repeat(500),
        "b".to_string(),
        // A mismatch in the middle of an otherwise-matching run.
        "aaaabaaaa".to_string(),
    ];

    for haystack in &haystacks {
        let bytes = haystack.as_bytes();
        let expected_find = regexr::reference::find(&hir.expr, ncaps, bytes);
        let expected_captures = regexr::reference::captures(&hir.expr, ncaps, bytes);

        let found = re.find(haystack).map(|m| (m.start(), m.end()));
        assert_eq!(
            found, expected_find,
            "{pattern:?} on {haystack:?}: find() disagrees with reference"
        );

        let captured = re
            .captures(haystack)
            .and_then(|caps| caps.get(0))
            .map(|m| (m.start(), m.end()));
        let expected_group0 = expected_captures.and_then(|caps| caps[0]);
        assert_eq!(
            captured, expected_group0,
            "{pattern:?} on {haystack:?}: captures() disagrees with reference"
        );
    }
}
