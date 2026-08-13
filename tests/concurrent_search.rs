//! One `Regex` shared across threads must give every thread the same answer.
//!
//! The lazy DFA builds its states on demand, so a search mutates its state
//! cache. Each concurrent search therefore runs on its own instance handed out
//! by a pool rather than on one shared instance. The cache is only a cache —
//! every instance runs the same subset construction over the same NFA — so
//! which instance computes a state must never change what is matched.
//!
//! This asserts the results, not the speed: a wall-clock scaling assertion
//! would be flaky on a loaded machine.

use regexr::{Regex, RegexBuilder};
use std::sync::Arc;

/// Threads used by each case. Deliberately above the pool's idle cap, so the
/// path that clones a fresh instance for an over-cap search runs too.
const THREADS: usize = 16;

/// Passes per thread, so the threads keep overlapping rather than each
/// finishing before the next is spawned.
const PASSES: usize = 5;

const HAYSTACK_BYTES: usize = 200 * 1024;

/// Prose broken into lines, so a multiline-anchored pattern has many matches.
fn haystack() -> String {
    const WORDS: &[&str] = &[
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then", "runs",
        "through", "forest", "near", "river", "where", "birds", "sing", "their", "morning",
    ];
    let mut text = String::with_capacity(HAYSTACK_BYTES);
    let mut index = 0usize;
    while text.len() < HAYSTACK_BYTES {
        text.push_str(WORDS[index % WORDS.len()]);
        text.push(if index % 7 == 0 { '\n' } else { ' ' });
        index += 1;
    }
    text
}

fn spans(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    re.find_iter(text).map(|m| (m.start(), m.end())).collect()
}

/// The same spans by way of `captures_iter`, which holds its own instance for
/// the length of the iteration just as `find_iter` does.
fn capture_spans(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    re.captures_iter(text)
        .map(|c| {
            let m = c.get(0).expect("group 0 is always set on a match");
            (m.start(), m.end())
        })
        .collect()
}

/// Compiles `pattern` and checks the test is actually exercising the lazy DFA.
fn lazy_dfa_regex(pattern: &str) -> Regex {
    let re = RegexBuilder::new(pattern)
        .jit(false)
        .build()
        .expect("pattern should compile");
    assert_eq!(
        re.engine_name(),
        "LazyDfa",
        "{pattern}: this test is only meaningful on the lazy DFA"
    );
    re
}

/// Runs `find_iter` on `THREADS` threads sharing one `Regex`, and asserts every
/// thread saw exactly what a single-threaded run saw.
fn assert_agrees_across_threads(pattern: &'static str) {
    let re = lazy_dfa_regex(pattern);

    let text = Arc::new(haystack());
    let expected = Arc::new(spans(&re, &text));
    let expected_captures = Arc::new(capture_spans(&re, &text));
    assert!(
        !expected.is_empty(),
        "{pattern}: expected the pattern to match the haystack"
    );

    let re = Arc::new(re);
    let handles: Vec<_> = (0..THREADS)
        .map(|thread| {
            let re = Arc::clone(&re);
            let text = Arc::clone(&text);
            let expected = Arc::clone(&expected);
            let expected_captures = Arc::clone(&expected_captures);
            std::thread::spawn(move || {
                for pass in 0..PASSES {
                    assert_eq!(
                        spans(&re, &text),
                        *expected,
                        "{pattern}: thread {thread} pass {pass} disagreed with the \
                         single-threaded run"
                    );
                    assert_eq!(
                        capture_spans(&re, &text),
                        *expected_captures,
                        "{pattern}: thread {thread} pass {pass} disagreed with the \
                         single-threaded captures_iter run"
                    );
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread panicked");
    }
}

#[test]
fn a_shared_lazy_dfa_agrees_across_threads() {
    assert_agrees_across_threads(r"(?m)^\w+");
}

#[test]
fn a_shared_lazy_dfa_agrees_across_threads_on_a_longer_pattern() {
    assert_agrees_across_threads(r"(?m)^\w+\s\w+");
}

/// An iterator holds an instance for its whole lifetime, so one abandoned
/// part-way through must still hand it back — and a reused instance must give
/// the same answers as a fresh one.
///
/// Enough abandoned iterators are created to refill the pool several times
/// over, so later searches are running on instances that a previous, partial
/// iteration left mid-cache rather than on freshly cloned ones.
#[test]
fn an_abandoned_iterator_returns_its_instance() {
    const ABANDONED: usize = 32;

    let re = lazy_dfa_regex(r"(?m)^\w+");
    let text = haystack();
    let expected = spans(&re, &text);
    let expected_captures = capture_spans(&re, &text);
    assert!(
        expected.len() > 3 && expected_captures.len() > 3,
        "expected the pattern to match the haystack more than three times"
    );

    for _ in 0..ABANDONED {
        let partial: Vec<_> = re
            .find_iter(&text)
            .take(3)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(partial, expected[..3].to_vec(), "a partial run diverged");

        // Abandoned before its first `next`, so the instance is returned
        // untouched.
        drop(re.find_iter(&text));

        let partial: Vec<_> = re
            .captures_iter(&text)
            .take(3)
            .map(|c| {
                let m = c.get(0).expect("group 0 is always set on a match");
                (m.start(), m.end())
            })
            .collect();
        assert_eq!(
            partial,
            expected_captures[..3].to_vec(),
            "a partial capture run diverged"
        );

        // Full runs in between, so a reused instance is exercised by a search
        // that walks the whole haystack.
        assert_eq!(spans(&re, &text), expected, "a full run diverged");
    }

    assert_eq!(capture_spans(&re, &text), expected_captures);
}

/// The same, across threads: abandoned iterators return instances that other
/// threads then pick up, and every thread must still see the full match list.
#[test]
fn abandoned_iterators_do_not_disturb_other_threads() {
    let re = Arc::new(lazy_dfa_regex(r"(?m)^\w+\s\w+"));
    let text = Arc::new(haystack());
    let expected = Arc::new(spans(&re, &text));
    assert!(
        expected.len() >= 4,
        "expected the pattern to match the haystack at least four times"
    );

    let handles: Vec<_> = (0..THREADS)
        .map(|thread| {
            let re = Arc::clone(&re);
            let text = Arc::clone(&text);
            let expected = Arc::clone(&expected);
            std::thread::spawn(move || {
                for pass in 0..PASSES {
                    for take in 0..4 {
                        let partial: Vec<_> = re
                            .find_iter(&text)
                            .take(take)
                            .map(|m| (m.start(), m.end()))
                            .collect();
                        assert_eq!(
                            partial,
                            expected[..take].to_vec(),
                            "thread {thread} pass {pass} take {take} diverged"
                        );
                    }
                    assert_eq!(
                        spans(&re, &text),
                        *expected,
                        "thread {thread} pass {pass} diverged after abandoning iterators"
                    );
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread panicked");
    }
}
