//! A prefilter must not turn a linear search into a quadratic one.
//!
//! Several engines have no anchored primitive: their "match at this position"
//! entry point is the same unanchored scan as their ordinary search. Driving
//! those a candidate at a time makes every one of k candidates rescan the
//! remaining n bytes — `a+b` over 100 KB of prose with no match took 146 ms
//! under the DFA JIT against 0.03 ms interpreted.
//!
//! Nothing about the *results* changes when this regresses, so no correctness
//! test can see it. These measure the growth curve instead: doubling the input
//! must not quadruple the time.

use regexr::RegexBuilder;
use std::time::Instant;

/// Prose with a realistic density of the leading byte, and no match anywhere —
/// the worst case, since every candidate is examined and none succeeds.
fn haystack(size: usize) -> String {
    const WORDS: &[&str] = &[
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then", "runs",
        "through", "forest", "near", "river", "where", "birds", "sing", "their", "morning",
    ];
    let mut text = String::with_capacity(size);
    let mut index = 0usize;
    while text.len() < size {
        text.push_str(WORDS[index % WORDS.len()]);
        text.push(' ');
        index += 1;
    }
    let mut end = size;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

fn elapsed_ms(pattern: &str, text: &str) -> f64 {
    let re = RegexBuilder::new(pattern).jit(true).build().unwrap();
    // One untimed run so lazy initialisation is not charged to the measurement.
    assert_eq!(re.find_iter(text).count(), 0, "{pattern} must not match");
    let start = Instant::now();
    assert_eq!(re.find_iter(text).count(), 0);
    start.elapsed().as_secs_f64() * 1000.0
}

/// A failing search over 128 KB must finish in a budget only a linear scan can
/// meet.
///
/// The bound is absolute rather than a growth ratio because the suite runs
/// tests in parallel: under that load a wall-clock ratio between two sizes is
/// noise. Absolute is safe here only because the two regimes are ~100x apart —
/// this scan is well under a millisecond when linear and was 146 ms when
/// quadratic, so a 25 ms budget leaves both enormous slack and no ambiguity.
#[test]
fn failing_search_stays_linear_in_the_input() {
    // Each of these reaches an engine whose per-position entry point is really
    // an unanchored scan: the DFA JIT, the tagged NFA, the backtracking VM.
    const PATTERNS: &[&str] = &[r"a+b", r"a(?=b)", r"a+(?=b)", r"[ao]+(?=b)", r"(a)(b)\1"];
    const BUDGET_MS: f64 = 25.0;

    let text = haystack(128 * 1024);
    let mut over = Vec::new();

    for pattern in PATTERNS {
        let ms = elapsed_ms(pattern, &text);
        if ms > BUDGET_MS {
            over.push(format!("  {pattern}: {ms:.1} ms"));
        }
    }

    assert!(
        over.is_empty(),
        "a failing search over 128 KB took more than {BUDGET_MS} ms:\n{}\n\
         The prefilter is driving an engine that has no anchored match, so every \
         candidate rescans the rest of the input.",
        over.join("\n")
    );
}

/// The one-pass capture search must stay proportional to the plain search.
///
/// `captures()` on a one-pass pattern locates the match by trying the positions
/// the prefilter keeps, which is a win only while a failed attempt gives up
/// quickly. `(a{64})b` over a run of `a`s is the shape where it does not: every
/// position is a candidate and every attempt reads 64 bytes before failing. The
/// candidate loop has to notice and hand the rest of the input back to the
/// linear-time search.
///
/// The bound is a ratio against `find` rather than a wall-clock budget, because
/// the two are measured on the same pattern and the same haystack in the same
/// run: whatever the machine is doing to one it is doing to the other. An
/// absolute budget here would have to sit close to this pattern's own linear
/// cost — `find` alone is milliseconds — and would flip under load.
#[test]
fn failing_one_pass_capture_search_stays_proportional_to_the_search() {
    // Measured at ~1.5x when linear and ~7.6x when every candidate costs an
    // attempt, so this separates the two regimes with room on both sides.
    const MAX_RATIO: f64 = 3.0;

    let text = "a".repeat(128 * 1024);
    let re = regexr::Regex::new(r"(a{64})b").unwrap();
    // One untimed run each, so lazy initialisation is not charged to either.
    assert!(re.captures(&text).is_none());
    assert!(re.find(&text).is_none());

    let start = Instant::now();
    assert!(re.find(&text).is_none());
    let find = start.elapsed().as_secs_f64();

    let start = Instant::now();
    assert!(re.captures(&text).is_none());
    let captures = start.elapsed().as_secs_f64();

    let ratio = captures / find.max(f64::MIN_POSITIVE);
    assert!(
        ratio <= MAX_RATIO,
        "a failing capture search cost {ratio:.1}x the same failing find \
         ({:.1} ms against {:.1} ms), more than {MAX_RATIO}x. Each candidate is \
         costing a full one-pass attempt, so the search is quadratic in the input.",
        captures * 1000.0,
        find * 1000.0
    );
}
