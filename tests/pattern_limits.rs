//! A pattern may not ask for an unbounded amount of work at compile time.
//!
//! Every engine compiles `{n,m}` by emitting the subexpression `m` times, so the
//! cost of `Regex::new` is set by the pattern's *expanded* size rather than its
//! text length. Without a ceiling, `\w{200000,}` spends tens of seconds inside
//! the constructor — which turns a caller that compiles a user-supplied pattern
//! into a denial of service, with nothing in the pattern's length to warn them.

use std::time::Instant;

/// A bound past what the engines will expand is refused, with the count in the
/// message so the author can see what they asked for.
#[test]
fn an_oversized_repetition_bound_is_refused() {
    for pattern in [
        r"\w{200000,}",
        r"a{70000}",
        r"(?:ab){65536}",
        r"[a-z]{0,99999}",
    ] {
        let error = regexr::Regex::new(pattern)
            .expect_err(&format!("{pattern} must not compile"))
            .to_string();
        assert!(
            error.contains("exceed"),
            "{pattern}: expected a limit error, got {error:?}"
        );
    }
}

/// Nested repetitions multiply, so bounding each one alone is not enough.
///
/// Every bound in `(?:a{1000}){1000}` is legal on its own; together they spell
/// out a million elements.
#[test]
fn nested_repetitions_are_bounded_by_their_product() {
    for pattern in [
        r"(?:a{1000}){1000}",
        r"(?:(?:a{100}){100}){100}",
        r"(?:\w{500}){500}",
    ] {
        let error = regexr::Regex::new(pattern)
            .expect_err(&format!("{pattern} must not compile"))
            .to_string();
        assert!(
            error.contains("expands to"),
            "{pattern}: expected an expansion error, got {error:?}"
        );
    }
}

/// The ceiling has to leave ordinary patterns alone.
#[test]
fn ordinary_repetitions_still_compile() {
    const PATTERNS: &[&str] = &[
        r"\w+",
        r"a{2,4}",
        r"(?:ab){100}",
        r"\d{1,3}(\.\d{1,3}){3}",
        r"[a-z]{500}",
        r"(?:\w{50}){50}",
        r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$",
    ];

    for pattern in PATTERNS {
        assert!(
            regexr::Regex::new(pattern).is_ok(),
            "{pattern} must still compile"
        );
    }
}

/// The ceiling is the caller's to raise.
///
/// A default that cannot be overridden decides for every user of the crate that
/// their pattern is not worth the compile time, which is not the library's call
/// to make permanently — `backtrack_limit` is the same bargain. The error names
/// the size the pattern reached, so it also says what to raise the limit to.
#[test]
fn the_expansion_limit_can_be_raised() {
    let pattern = r"a{50000}";
    assert!(
        regexr::Regex::new(pattern).is_err(),
        "{pattern} is past the default"
    );

    let raised = regexr::RegexBuilder::new(pattern)
        .size_limit(100_000)
        .build()
        .expect("a caller who owns the pattern may pay for it");
    assert!(raised.is_match(&"a".repeat(50_000)));
    assert!(!raised.is_match(&"a".repeat(49_999)));

    // Lowering it works the same way, so a caller taking untrusted patterns can
    // be stricter than the default rather than only more permissive.
    assert!(regexr::RegexBuilder::new(r"(?:ab){100}")
        .size_limit(10)
        .build()
        .is_err());
}

/// Whatever the ceiling admits must compile in a time a caller can absorb.
///
/// This is the property the number was chosen for; the number itself is free to
/// move as long as this holds.
///
/// The claim is that *the expansion count* bounds compile time, so the check is
/// against the same pattern at a tenth the expansion rather than against a wall
/// clock: an absolute millisecond ceiling measures the machine as much as the
/// engine, and an unoptimized test build on a loaded CI runner is an order of
/// magnitude off a release build on an idle one. A ratio cancels that out. Near
/// the limit, compilation is linear in expanded elements, so the honest number
/// here is 10; the margin covers timing noise and the fixed per-compile cost the
/// smaller pattern pays proportionally more of.
#[test]
fn the_largest_accepted_pattern_compiles_promptly() {
    /// Best of several runs: a scheduler preempting one run inflates that
    /// sample, and only the minimum is a measurement of the compiler.
    fn fastest_compile_ms(pattern: &str) -> f64 {
        (0..5)
            .map(|_| {
                let start = Instant::now();
                let regex = regexr::Regex::new(pattern);
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                assert!(regex.is_ok(), "{pattern} should be within the limit");
                ms
            })
            .fold(f64::INFINITY, f64::min)
    }

    // Just inside the limit: 10 x 999 elements, against a tenth of it.
    let full = fastest_compile_ms(r"(?:[a-z]{999}){10}");
    let tenth = fastest_compile_ms(r"(?:[a-z]{999}){1}");

    let ratio = full / tenth;
    assert!(
        ratio <= 40.0,
        "compiling the largest accepted pattern took {full:.0} ms against \
         {tenth:.0} ms for a tenth of the expansion ({ratio:.1}x for 10x the \
         elements); the expansion limit is no longer bounding compile time"
    );
}
