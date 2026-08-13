//! Capture extraction must honour the same execution bound as `find`.
//!
//! `find` runs on the automaton engines and is linear in the input. Capture
//! extraction owes the caller the same guarantee: every search terminates, in
//! time and memory bounded by the input, for every pattern the parser accepts.
//!
//! Three bounds are checked here:
//!
//! 1. A repetition whose body matches the empty string runs it once and then
//!    leaves the loop. Without that, `(a*)*` re-enters its body at a position it
//!    can never advance past and the search never returns.
//! 2. Alternative-heavy repetition does not cost exponential time. `(a+)+c` over
//!    a run of `a` has exponentially many ways to split the run, and an engine
//!    that explores them one at a time will not finish.
//! 3. The choice-point stack stays inside the memory it owns. A search that
//!    needs more choice points than the stack was sized for must still answer
//!    correctly, not write past the end of its frame.
//!
//! Expected spans are the leftmost-first, greedy semantics of the executable
//! spec in `regexr::reference`, cross-checked against the `regex` crate.
//!
//! Every case runs in a child process. The failure modes under test are an
//! unbounded allocation (abort) and a choice-point stack that runs past its
//! frame (SIGSEGV); both take down the whole test binary, so isolating each
//! case keeps one broken bound from hiding the state of the others.

use std::env;
use std::process::Command;
use std::time::{Duration, Instant};

use regexr::{Regex, RegexBuilder};

/// Set by the parent on the child it spawns, naming the single case to run.
const CASE_VAR: &str = "REGEXR_BOUNDED_EXECUTION_CASE";

/// How long a bounded search is allowed to take. Correct engines answer every
/// case below in microseconds; this only has to be short enough that an
/// unbounded one is reported as a failure rather than left running.
const DEADLINE: Duration = Duration::from_secs(20);

/// Address-space ceiling for the child, so an unbounded search aborts on a
/// failed allocation instead of pushing the machine into swap.
const CHILD_ADDRESS_SPACE_KIB: u64 = 4_000_000;

/// Runs `body` in a child process dedicated to `case`, and fails if that child
/// crashes, is killed, or has not finished within [`DEADLINE`].
///
/// `case` must be the name of the calling test, because that is the filter the
/// child is invoked with.
fn bounded(case: &str, body: impl FnOnce()) {
    bounded_within(case, DEADLINE, body)
}

/// [`bounded`] with an explicit deadline, for cases whose correct behaviour is
/// still measured in seconds rather than microseconds.
fn bounded_within(case: &str, deadline: Duration, body: impl FnOnce()) {
    if env::var(CASE_VAR).as_deref() == Ok(case) {
        body();
        return;
    }

    let exe = env::current_exe().expect("test binary path");
    let mut command = if cfg!(unix) {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(format!(
                "ulimit -v {CHILD_ADDRESS_SPACE_KIB} 2>/dev/null; exec \"$@\""
            ))
            .arg("sh")
            .arg(&exe);
        c
    } else {
        Command::new(&exe)
    };
    let mut child = command
        .arg(case)
        .arg("--exact")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(CASE_VAR, case)
        .spawn()
        .expect("spawn isolated case");

    let started = Instant::now();
    loop {
        match child.try_wait().expect("poll isolated case") {
            Some(status) if status.success() => return,
            Some(status) => panic!("{case}: isolated run failed ({status})"),
            None if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{case}: search did not terminate within {deadline:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// The two builds under test, labelled for failure messages.
fn builds(pattern: &str) -> Vec<(&'static str, Regex)> {
    vec![
        (
            "jit",
            RegexBuilder::new(pattern)
                .jit(true)
                .build()
                .expect("pattern should compile"),
        ),
        (
            "interp",
            RegexBuilder::new(pattern)
                .jit(false)
                .build()
                .expect("pattern should compile"),
        ),
    ]
}

type Spans = Vec<Option<(usize, usize)>>;

/// Spans of every group of the first match, group 0 first.
fn capture_spans(re: &Regex, text: &str) -> Option<Spans> {
    re.captures(text).map(|caps| {
        (0..caps.len())
            .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
            .collect()
    })
}

/// Spans of every group of every match produced by iteration.
fn iterated_spans(re: &Regex, text: &str) -> Vec<Spans> {
    re.captures_iter(text)
        .map(|caps| {
            (0..caps.len())
                .map(|i| caps.get(i).map(|m| (m.start(), m.end())))
                .collect()
        })
        .collect()
}

fn span(start: usize, end: usize) -> Option<(usize, usize)> {
    Some((start, end))
}

// =============================================================================
// A repetition whose body matches empty runs it once, then leaves the loop
// =============================================================================
// The loop takes a zero-width iteration while it still owes iterations — the
// first one, plus any `min` demands — and refuses it after that. So `(a*)*` on
// "b" reports group 1 as the empty span at 0, while on "a" it reports 0..1 and
// not the empty span that would follow. Every expectation below was cross-checked
// against `regexr::reference` and the `regex` crate.

#[test]
fn nested_star_over_nullable_body_terminates() {
    bounded("nested_star_over_nullable_body_terminates", || {
        for (label, re) in builds(r"(a*)*") {
            assert_eq!(
                capture_spans(&re, "a"),
                Some(vec![span(0, 1), span(0, 1)]),
                "{label}"
            );
            assert_eq!(
                capture_spans(&re, "aaa"),
                Some(vec![span(0, 3), span(0, 3)]),
                "{label}"
            );
        }
    });
}

#[test]
fn nested_star_over_nullable_body_matches_empty_at_start() {
    bounded(
        "nested_star_over_nullable_body_matches_empty_at_start",
        || {
            for (label, re) in builds(r"(a*)*") {
                assert_eq!(
                    capture_spans(&re, "b"),
                    Some(vec![span(0, 0), span(0, 0)]),
                    "{label}"
                );
            }
        },
    );
}

#[test]
fn star_over_optional_body_terminates() {
    bounded("star_over_optional_body_terminates", || {
        for (label, re) in builds(r"(a?)*") {
            assert_eq!(
                capture_spans(&re, "a"),
                Some(vec![span(0, 1), span(0, 1)]),
                "{label}"
            );
        }
    });
}

#[test]
fn plus_over_empty_group_terminates() {
    bounded("plus_over_empty_group_terminates", || {
        for (label, re) in builds(r"()+") {
            assert_eq!(
                capture_spans(&re, "a"),
                Some(vec![span(0, 0), span(0, 0)]),
                "{label}"
            );
        }
    });
}

#[test]
fn plus_over_nullable_body_terminates() {
    bounded("plus_over_nullable_body_terminates", || {
        for (label, re) in builds(r"(a*)+") {
            assert_eq!(
                capture_spans(&re, "a"),
                Some(vec![span(0, 1), span(0, 1)]),
                "{label}"
            );
        }
    });
}

#[test]
fn star_over_bounded_nullable_body_terminates() {
    bounded("star_over_bounded_nullable_body_terminates", || {
        for (label, re) in builds(r"(a{0,2})*") {
            assert_eq!(
                capture_spans(&re, "aaa"),
                Some(vec![span(0, 3), span(2, 3)]),
                "{label}"
            );
        }
    });
}

#[test]
fn nested_capture_inside_nullable_loop_terminates() {
    bounded("nested_capture_inside_nullable_loop_terminates", || {
        for (label, re) in builds(r"((a)*)*") {
            assert_eq!(
                capture_spans(&re, "a"),
                Some(vec![span(0, 1), span(0, 1), span(0, 1)]),
                "{label}"
            );
        }
    });
}

#[test]
fn nullable_loop_followed_by_literal_terminates() {
    bounded("nullable_loop_followed_by_literal_terminates", || {
        for (label, re) in builds(r"(a*)*b") {
            assert_eq!(
                capture_spans(&re, "ab"),
                Some(vec![span(0, 2), span(0, 1)]),
                "{label}"
            );
            assert_eq!(capture_spans(&re, "a"), None, "{label}");
        }
    });
}

#[test]
fn iteration_over_nullable_loop_terminates() {
    bounded("iteration_over_nullable_loop_terminates", || {
        for (label, re) in builds(r"(a*)*") {
            // The empty match at 1 is where the non-empty match ended, so
            // iteration drops it; the one at 2 follows an empty match and stays.
            assert_eq!(
                iterated_spans(&re, "ab"),
                vec![vec![span(0, 1), span(0, 1)], vec![span(2, 2), span(2, 2)],],
                "{label}"
            );
        }
    });
}

#[test]
fn iteration_over_empty_group_loop_terminates() {
    bounded("iteration_over_empty_group_loop_terminates", || {
        for (label, re) in builds(r"()+") {
            assert_eq!(
                iterated_spans(&re, "ab"),
                vec![
                    vec![span(0, 0), span(0, 0)],
                    vec![span(1, 1), span(1, 1)],
                    vec![span(2, 2), span(2, 2)],
                ],
                "{label}"
            );
        }
    });
}

/// The capture path must agree with the automaton path, which already reports
/// these spans correctly — the two must not disagree about the same pattern.
#[test]
fn nullable_loop_captures_agree_with_find() {
    bounded("nullable_loop_captures_agree_with_find", || {
        for pattern in [r"(a*)*", r"(a?)*", r"()+", r"(a*)+"] {
            for (label, re) in builds(pattern) {
                for text in ["", "a", "aaa", "ab", "b"] {
                    let found = re.find(text).map(|m| (m.start(), m.end()));
                    let captured = capture_spans(&re, text).and_then(|s| s[0]);
                    assert_eq!(found, captured, "{label} {pattern:?} {text:?}");
                }
            }
        }
    });
}

// =============================================================================
// Repetition with many ways to split the input costs bounded time
// =============================================================================

#[test]
fn nested_plus_does_not_backtrack_exponentially() {
    bounded("nested_plus_does_not_backtrack_exponentially", || {
        let text = "a".repeat(30);
        for (label, re) in builds(r"(a+)+c") {
            assert_eq!(capture_spans(&re, &text), None, "{label}");
        }
    });
}

#[test]
fn nested_star_does_not_backtrack_exponentially() {
    bounded("nested_star_does_not_backtrack_exponentially", || {
        let text = "a".repeat(30);
        for (label, re) in builds(r"(a*)*c") {
            assert_eq!(capture_spans(&re, &text), None, "{label}");
        }
    });
}

#[test]
fn nested_plus_reports_the_match_it_finds() {
    bounded("nested_plus_reports_the_match_it_finds", || {
        let text = format!("{}c", "a".repeat(30));
        for (label, re) in builds(r"(a+)+c") {
            assert_eq!(
                capture_spans(&re, &text),
                Some(vec![span(0, 31), span(0, 30)]),
                "{label}"
            );
        }
    });
}

// =============================================================================
// The choice-point stack stays inside the memory it owns
// =============================================================================
// A greedy repetition records one choice point per iteration. The JIT engines
// hold those in a fixed frame, so a run longer than that frame is the case that
// distinguishes "grew the stack" from "wrote past the end of it".

#[test]
fn long_greedy_run_before_a_literal_stays_in_bounds() {
    bounded("long_greedy_run_before_a_literal_stays_in_bounds", || {
        let matching = format!("{}@", "a".repeat(300));
        let non_matching = "a".repeat(300);
        for (label, re) in builds(r"([a-z]+)@") {
            assert_eq!(
                capture_spans(&re, &matching),
                Some(vec![span(0, 301), span(0, 300)]),
                "{label}"
            );
            assert_eq!(capture_spans(&re, &non_matching), None, "{label}");
        }
    });
}

#[test]
fn very_long_greedy_run_before_a_literal_stays_in_bounds() {
    bounded(
        "very_long_greedy_run_before_a_literal_stays_in_bounds",
        || {
            let text = format!("{}!", "a".repeat(4000));
            for (label, re) in builds(r"(\w+)!") {
                assert_eq!(
                    capture_spans(&re, &text),
                    Some(vec![span(0, 4001), span(0, 4000)]),
                    "{label}"
                );
            }
        },
    );
}

#[test]
fn long_greedy_run_captures_agree_with_find() {
    bounded("long_greedy_run_captures_agree_with_find", || {
        let text = format!("{}@", "a".repeat(1000));
        for (label, re) in builds(r"([a-z]+)@") {
            let found = re.find(&text).map(|m| (m.start(), m.end()));
            let captured = capture_spans(&re, &text).and_then(|s| s[0]);
            assert_eq!(found, captured, "{label}");
        }
    });
}

// =============================================================================
// A search over a long non-matching input stays linear in its length
// =============================================================================
// Every engine here searches unanchored input by trying start positions. That is
// right while a failed attempt gives up near where it began, and quadratic when
// it does not: `(a+)+$` over a run of `a` consumes the whole run from every
// start and rejects it at `$`, so the cost is one full scan per byte. The
// engines answer that with a single pass that covers every start at once.
//
// The input below is long enough that the difference is not a matter of
// constants: linear finishes in milliseconds, and one scan per byte would need
// far longer than [`DEADLINE`] allows.

/// Input length for the scaling cases.
const LONG_INPUT: usize = 20_000;

/// Deadline for the scaling cases.
///
/// Unlike the termination cases, these do real work even when correct — tens of
/// thousands of bytes through every engine, in a debug build. The deadline is
/// not a performance target and is deliberately far above what linear costs on
/// any machine, emulated ones included; it only has to sit below what one scan
/// per byte would take at [`LONG_INPUT`], which is minutes.
const SCALING_DEADLINE: Duration = Duration::from_secs(120);

/// Patterns whose failed attempts each consume the whole run before rejecting
/// it, spread across the engines that search by start position: Shift-Or, the
/// lazy DFA and its JIT, and the PikeVM.
const LONG_RUN_NON_MATCHING: &[&str] = &[
    r"(a|a)+$",
    r"(?:a|a)+$",
    r"(a|aa)+$",
    r"(a+)+$",
    r"(a|b)+$",
    r"(x+x+)+y",
    r"([a-zA-Z]+)*b$",
];

#[test]
fn long_run_rejection_stays_linear() {
    bounded_within("long_run_rejection_stays_linear", SCALING_DEADLINE, || {
        let text = format!("{}!", "a".repeat(LONG_INPUT));
        for pattern in LONG_RUN_NON_MATCHING {
            for (label, re) in builds(pattern) {
                assert!(!re.is_match(&text), "{label} {pattern}");
                assert_eq!(
                    re.find(&text).map(|m| (m.start(), m.end())),
                    None,
                    "{label} {pattern}"
                );
                assert_eq!(capture_spans(&re, &text), None, "{label} {pattern}");
            }
        }
    });
}

#[test]
fn long_run_iteration_stays_linear() {
    bounded_within("long_run_iteration_stays_linear", SCALING_DEADLINE, || {
        let text = format!("{}!", "a".repeat(LONG_INPUT));
        for pattern in LONG_RUN_NON_MATCHING {
            for (label, re) in builds(pattern) {
                assert_eq!(re.find_iter(&text).count(), 0, "{label} {pattern}");
                assert_eq!(iterated_spans(&re, &text).len(), 0, "{label} {pattern}");
            }
        }
    });
}

// =============================================================================
// Parsing does not overflow the stack on deeply nested patterns
// =============================================================================
// The parser is mutually recursive with no explicit stack, so a pattern that
// nests deeply enough overflows the real call stack — which in Rust is an
// uncatchable SIGSEGV/abort, not a `Result::Err` a caller can handle. That
// makes `assert!(Regex::new(deep).is_err())` unsafe as a standalone test: if
// the depth guard ever regresses, the process crashes instead of the assertion
// failing, taking down the rest of the `cargo test` binary with it. Each case
// below is isolated in a child process for exactly that reason (see the module
// doc comment above).
//
// Patterns are held well past `parser::DEFAULT_NEST_LIMIT` (250) so the cases
// stay meaningful even if the limit is later raised somewhat.

/// A run of unmatched `(` is malformed — parsing would eventually reach an
/// unmatched-paren error — but the depth guard must reject it long before
/// that, on the way down through the nesting, not on the way back up.
#[test]
fn many_unmatched_open_parens_is_rejected_not_crashed() {
    bounded("many_unmatched_open_parens_is_rejected_not_crashed", || {
        let pattern = "(".repeat(50_000);
        let err = Regex::new(&pattern)
            .expect_err("unbounded nesting must not compile")
            .to_string();
        assert!(
            err.contains("nest"),
            "expected a nesting error, got {err:?}"
        );
    });
}

/// A well-formed pattern nested via `(?:...)` recurses just as deeply while
/// parsing the opening half, so the guard must catch it there rather than
/// relying on the (never reached) closing half to bound anything.
#[test]
fn many_well_formed_non_capturing_groups_is_rejected_not_crashed() {
    bounded(
        "many_well_formed_non_capturing_groups_is_rejected_not_crashed",
        || {
            let pattern = format!("{}{}", "(?:".repeat(50_000), ")".repeat(50_000));
            let err = Regex::new(&pattern)
                .expect_err("unbounded nesting must not compile")
                .to_string();
            assert!(
                err.contains("nest"),
                "expected a nesting error, got {err:?}"
            );
        },
    );
}

/// Character classes recurse on their own path (`parse_class` <->
/// `parse_class_term`), separate from the group/alternation cycle, and must be
/// bounded independently: `[a[a[a...` opens a fresh nested class after every
/// literal `a`.
#[test]
fn many_nested_classes_is_rejected_not_crashed() {
    bounded("many_nested_classes_is_rejected_not_crashed", || {
        let pattern = "[a".repeat(50_000);
        let err = Regex::new(&pattern)
            .expect_err("unbounded class nesting must not compile")
            .to_string();
        assert!(
            err.contains("nest"),
            "expected a nesting error, got {err:?}"
        );
    });
}

/// A bare `(?flags)` opens a new flag scope over the rest of its branch by
/// recursing into `parse_concat` again — a third recursion path, distinct from
/// both the group cycle and the class path, that a long run of flag changes
/// can walk arbitrarily deep.
#[test]
fn many_inline_flag_changes_is_rejected_not_crashed() {
    bounded("many_inline_flag_changes_is_rejected_not_crashed", || {
        let pattern = "(?i)(?-i)".repeat(50_000);
        let err = Regex::new(&pattern)
            .expect_err("unbounded flag-scope nesting must not compile")
            .to_string();
        assert!(
            err.contains("nest"),
            "expected a nesting error, got {err:?}"
        );
    });
}

/// The other side of the same guard: nesting one level under the default
/// limit must still compile cleanly. This is what catches an off-by-one that
/// rejects legitimate patterns, which the crash-only cases above cannot show.
#[test]
fn nesting_just_under_the_limit_still_compiles() {
    bounded("nesting_just_under_the_limit_still_compiles", || {
        let depth = (regexr::parser::DEFAULT_NEST_LIMIT - 1) as usize;
        let pattern = format!("{}a{}", "(?:".repeat(depth), ")".repeat(depth));
        Regex::new(&pattern).expect("nesting one under the limit must compile");
    });
}

// =============================================================================
// Literal extraction does not cost exponential time on deeply nested patterns
// =============================================================================
// `LiteralExtractor::extract` walks the HIR once per `Regex::new`. Two sites
// used to walk the same subtree twice: the `Concat` arm's extend-loop and its
// trailing-element check can land on the same node, and the `Alt` arm's
// bail-out into `extract_common_prefix` used to re-walk every branch from
// scratch, including the ones its own loop had already extracted. Either one
// gives `T(depth) = 2*T(depth-1)`, which crosses into "does not return"
// around depth 40-60 - well under the parser's 250-level nesting cap, so a
// pattern the parser happily accepts could still hang `Regex::new` itself,
// before any matching happens.
//
// Each case nests to depth 60: comfortably past the exponential crossover,
// comfortably under `DEFAULT_NEST_LIMIT`. Exponential behaviour hangs and
// trips the harness deadline; linear behaviour returns instantly.

/// Nesting depth for the extractor-doubling cases: past the exponential
/// crossover, under the parser's nesting cap.
const EXTRACTOR_DOUBLING_DEPTH: usize = 60;

/// `a(?:a(?:a(?:...(?:ab)...)))` - the `Concat` shape that doubles: every
/// level is a two-element `Concat[Literal, Tail]`, exactly the shape where
/// the extend-loop's break node and the trailing-element check's
/// `actual_last` are the same node.
fn nested_concat_doubling_pattern(depth: usize) -> String {
    format!("{}ab{}", "a(?:".repeat(depth), ")".repeat(depth))
}

/// Nested `(?:a...|\d)` alternations - the `Alt` shape that doubles: at
/// every level the `\d` branch has no literal prefix and forces a bail into
/// `extract_common_prefix`, which used to re-walk every branch - including
/// the nested alternation in the other branch - from scratch.
fn nested_alt_doubling_pattern(depth: usize) -> String {
    let mut pattern = String::from(r"\d");
    for _ in 0..depth {
        pattern = format!(r"(?:a{pattern}|\d)");
    }
    pattern
}

#[test]
fn nested_concat_literal_extraction_terminates() {
    bounded("nested_concat_literal_extraction_terminates", || {
        let pattern = nested_concat_doubling_pattern(EXTRACTOR_DOUBLING_DEPTH);
        Regex::new(&pattern).expect("well under the nesting cap, must compile");
    });
}

#[test]
fn nested_alt_literal_extraction_terminates() {
    bounded("nested_alt_literal_extraction_terminates", || {
        let pattern = nested_alt_doubling_pattern(EXTRACTOR_DOUBLING_DEPTH);
        Regex::new(&pattern).expect("well under the nesting cap, must compile");
    });
}

/// `required_literal`'s `Lookaround` arm (`src/literal/extractor.rs:601-606`)
/// reaches `LiteralExtractor::extract` on the lookahead's inner expression
/// directly, independent of the top-level prefix extraction - the outer
/// `x(?=...)` concat never doubles on its own (a lookaround is zero-width, so
/// the extend-loop `continue`s past it rather than breaking on it), so this
/// isolates that call path from the one the two cases above already cover.
#[test]
fn nested_concat_in_lookahead_literal_extraction_terminates() {
    bounded(
        "nested_concat_in_lookahead_literal_extraction_terminates",
        || {
            let inner = nested_concat_doubling_pattern(EXTRACTOR_DOUBLING_DEPTH);
            let pattern = format!("x(?={inner})");
            Regex::new(&pattern).expect("well under the nesting cap, must compile");
        },
    );
}

// =============================================================================
// Tagged-NFA step extraction does not cost exponential time on sequential
// alternation groups
// =============================================================================
// `StepExtractor` (src/nfa/tagged/steps.rs) emits an `Alt` step whose branches
// each carry a full copy of everything after the alternation. That is not just
// explored exponentially, the *emitted program itself* is exponentially sized:
// `k` sequential alternation groups produce ~2^k steps. Without a cap on the
// extraction budget, `Regex::new` on a pattern with a lookaround (which forces
// the tagged-NFA path) and enough sequential groups does not return in any
// reasonable time - 24 groups measured at ~18s and ~16.7M emitted steps before
// the fix. `MAX_EXTRACTED_STEPS` bounds that work by bailing out of extraction
// early, which sends the pattern to the PikeVm instead: `Regex::new` returns
// immediately, matching just costs more per search.

#[test]
fn pathological_sequential_alternations_do_not_blow_up_compile_time() {
    bounded(
        "pathological_sequential_alternations_do_not_blow_up_compile_time",
        || {
            let groups = ["(?:ab|cd)", "(?:ef|gh)", "(?:ij|kl)", "(?:mn|op)"];
            let mut pattern = String::from("(?=a)");
            for i in 0..32 {
                pattern.push_str(groups[i % groups.len()]);
            }
            Regex::new(&pattern).expect("pattern is well-formed and must compile");
        },
    );
}
