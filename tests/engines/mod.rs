//! Integration tests for JIT compilation.
//!
//! These tests verify that the JIT compiler produces correct machine code
//! for various regex patterns and that the code executes correctly.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use regexr::dfa::LazyDfa;
use regexr::hir::translate;
use regexr::jit::{compile_dfa, is_available};
use regexr::nfa::compile as compile_nfa;
use regexr::parser::parse;

/// Helper function to compile a pattern to JIT code.
fn jit_compile(pattern: &str) -> regexr::jit::CompiledRegex {
    let ast = parse(pattern).expect("parse failed");
    let hir = translate(&ast).expect("translate failed");
    let nfa = compile_nfa(&hir).expect("nfa compile failed");
    let mut dfa = LazyDfa::new(nfa);

    compile_dfa(&mut dfa).expect("jit compile failed")
}

#[test]
fn test_jit_availability() {
    // On x86_64 with jit feature, JIT should be available
    assert!(is_available());
}

#[test]
fn test_simple_literal() {
    let jit = jit_compile("abc");

    // Exact match
    assert!(jit.is_full_match(b"abc"));

    // Prefix - should not full-match
    assert!(!jit.is_full_match(b"ab"));

    // Suffix added - should not full-match but should match (contains)
    assert!(!jit.is_full_match(b"abcd"));
    assert!(jit.is_match(b"abcd"));

    // Different string
    assert!(!jit.is_full_match(b"xyz"));
    assert!(!jit.is_match(b"xyz"));

    // Empty input
    assert!(!jit.is_full_match(b""));
}

#[test]
fn test_single_char() {
    let jit = jit_compile("a");

    assert!(jit.is_full_match(b"a"));
    assert!(!jit.is_full_match(b"b"));
    assert!(!jit.is_full_match(b"aa"));
    assert!(jit.is_match(b"aa")); // contains "a"
    assert!(!jit.is_full_match(b""));
}

#[test]
fn test_alternation() {
    let jit = jit_compile("a|b");

    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"b"));
    assert!(!jit.is_full_match(b"c"));
    assert!(!jit.is_full_match(b"ab"));
    assert!(jit.is_match(b"ab")); // contains "a" or "b"
    assert!(!jit.is_full_match(b""));
}

#[test]
fn test_star_empty_match() {
    let jit = jit_compile("a*");

    // Star should match empty string
    assert!(jit.is_full_match(b""));

    // Match single char
    assert!(jit.is_full_match(b"a"));

    // Match multiple chars
    assert!(jit.is_full_match(b"aa"));
    assert!(jit.is_full_match(b"aaaa"));

    // Should not full-match other chars (but is_match will match empty at start)
    assert!(!jit.is_full_match(b"b"));
    assert!(jit.is_match(b"b")); // matches empty string at position 0
}

#[test]
fn test_plus() {
    let jit = jit_compile("a+");

    // Plus should NOT full-match empty string
    assert!(!jit.is_full_match(b""));

    // Should full-match one or more
    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"aa"));
    assert!(jit.is_full_match(b"aaaa"));

    // Should not full-match other chars
    assert!(!jit.is_full_match(b"b"));
    assert!(!jit.is_match(b"b")); // no "a" in "b"
}

#[test]
fn test_optional() {
    let jit = jit_compile("a?");

    // Optional should full-match empty string
    assert!(jit.is_full_match(b""));

    // Should full-match single char
    assert!(jit.is_full_match(b"a"));

    // Should NOT full-match multiple chars (but is_match finds match)
    assert!(!jit.is_full_match(b"aa"));
    assert!(jit.is_match(b"aa")); // matches "a" or empty at some position
}

#[test]
fn test_concatenation() {
    let jit = jit_compile("abc");

    assert!(jit.is_full_match(b"abc"));
    assert!(!jit.is_full_match(b"ab"));
    assert!(!jit.is_full_match(b"bc"));
    assert!(!jit.is_full_match(b"ac"));
}

#[test]
fn test_complex_pattern() {
    let jit = jit_compile("ab*c");

    assert!(jit.is_full_match(b"ac")); // b*=0
    assert!(jit.is_full_match(b"abc")); // b*=1
    assert!(jit.is_full_match(b"abbc")); // b*=2
    assert!(jit.is_full_match(b"abbbc")); // b*=3
    assert!(!jit.is_full_match(b"bc")); // missing 'a'
}

#[test]
fn test_character_class() {
    let jit = jit_compile("[abc]");

    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"b"));
    assert!(jit.is_full_match(b"c"));
    assert!(!jit.is_full_match(b"d"));
    assert!(!jit.is_full_match(b"ab"));
    assert!(jit.is_match(b"ab")); // contains "a"
}

#[test]
fn test_character_range() {
    let jit = jit_compile("[a-z]");

    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"m"));
    assert!(jit.is_full_match(b"z"));
    assert!(!jit.is_full_match(b"A"));
    assert!(!jit.is_full_match(b"0"));
}

#[test]
fn test_digit_class() {
    let jit = jit_compile("[0-9]");

    assert!(jit.is_full_match(b"0"));
    assert!(jit.is_full_match(b"5"));
    assert!(jit.is_full_match(b"9"));
    assert!(!jit.is_full_match(b"a"));
}

#[test]
fn test_find() {
    let jit = jit_compile("abc");

    // Find in middle
    assert_eq!(jit.find(b"xyzabc123"), Some((3, 6)));

    // Find at start
    assert_eq!(jit.find(b"abc123"), Some((0, 3)));

    // Find at end
    assert_eq!(jit.find(b"123abc"), Some((3, 6)));

    // Not found
    assert_eq!(jit.find(b"xyz"), None);

    // Empty input
    assert_eq!(jit.find(b""), None);
}

#[test]
fn test_find_with_star() {
    let jit = jit_compile("a*");

    // Star matches empty at position 0
    assert_eq!(jit.find(b"bbb"), Some((0, 0)));

    // Star matches 'a's
    assert_eq!(jit.find(b"aaa"), Some((0, 3)));

    // Star finds first position with 'a's
    assert_eq!(jit.find(b"bbaabb"), Some((0, 0)));
}

#[test]
fn test_multiple_patterns() {
    // Test that we can compile multiple patterns independently
    let jit1 = jit_compile("abc");
    let jit2 = jit_compile("xyz");

    assert!(jit1.is_full_match(b"abc"));
    assert!(!jit1.is_full_match(b"xyz"));

    assert!(jit2.is_full_match(b"xyz"));
    assert!(!jit2.is_full_match(b"abc"));
}

#[test]
fn test_binary_input() {
    let jit = jit_compile("\\x00\\x01\\x02");

    assert!(jit.is_full_match(&[0x00, 0x01, 0x02]));
    assert!(!jit.is_full_match(&[0x00, 0x01]));
    assert!(!jit.is_full_match(&[0x01, 0x02, 0x03]));
}

#[test]
fn test_full_ascii_range_dense_transitions() {
    // The widest single-byte class the language can express: 128 transitions
    // out of one DFA state, which is what exercises the dense transition
    // table. A class reaching above U+007F compiles to UTF-8 sequences rather
    // than bytes, and engine selection sends those to the PikeVm, so they are
    // deliberately not a DFA-JIT case.
    let jit = jit_compile("[\\x00-\\x7f]");

    for byte in 0..=0x7fu8 {
        assert!(jit.is_full_match(&[byte]), "Failed for byte {byte:#x}");
    }
    assert!(!jit.is_full_match(&[0x80]));

    assert!(!jit.is_full_match(&[]));
    assert!(!jit.is_full_match(&[0, 0])); // Should only full-match one byte
    assert!(jit.is_match(&[0, 0])); // But is_match finds a match
}

#[test]
fn test_long_pattern() {
    // Test with a longer literal to verify state transitions
    let jit = jit_compile("abcdefghijklmnop");

    assert!(jit.is_full_match(b"abcdefghijklmnop"));
    assert!(!jit.is_full_match(b"abcdefghijklmno"));
    assert!(!jit.is_full_match(b"abcdefghijklmnopq"));
    assert!(jit.is_match(b"abcdefghijklmnopq")); // contains pattern
}

#[test]
fn test_alternation_chain() {
    let jit = jit_compile("a|b|c|d|e");

    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"b"));
    assert!(jit.is_full_match(b"c"));
    assert!(jit.is_full_match(b"d"));
    assert!(jit.is_full_match(b"e"));
    assert!(!jit.is_full_match(b"f"));
}

#[test]
fn test_nested_repetition() {
    let jit = jit_compile("(ab)*");

    assert!(jit.is_full_match(b""));
    assert!(jit.is_full_match(b"ab"));
    assert!(jit.is_full_match(b"abab"));
    assert!(jit.is_full_match(b"ababab"));
    assert!(!jit.is_full_match(b"a"));
    assert!(!jit.is_full_match(b"aba"));
    assert!(jit.is_match(b"aba")); // matches "ab" or empty
}

#[test]
fn test_complex_class_pattern() {
    let jit = jit_compile("[a-z]+");

    assert!(jit.is_full_match(b"hello"));
    assert!(jit.is_full_match(b"world"));
    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"zzz"));
    assert!(!jit.is_full_match(b""));
    assert!(!jit.is_full_match(b"HELLO"));
    assert!(!jit.is_full_match(b"123"));
}

#[test]
fn test_execute_returns_correct_position() {
    let jit = jit_compile("abc");

    // execute now returns (start, end) tuple
    assert_eq!(jit.execute(b"abc"), Some((0, 3)));
    assert_eq!(jit.execute(b"abcdef"), Some((0, 3)));
    assert_eq!(jit.execute(b"ab"), None);
}

#[test]
fn test_empty_pattern() {
    // Empty pattern should full-match empty string
    let jit = jit_compile("");

    assert!(jit.is_full_match(b""));
    assert!(!jit.is_full_match(b"a"));
    assert!(jit.is_match(b"a")); // matches empty at position 0
}

#[test]
fn test_special_chars() {
    // Test literal dot (not wildcard)
    let jit = jit_compile("\\.");

    assert!(jit.is_full_match(b"."));
    assert!(!jit.is_full_match(b"a"));
}

#[test]
#[ignore] // Only run with --ignored flag
fn test_large_input() {
    let jit = jit_compile("abc");

    // Test with large input to verify no buffer overflows
    let large_input = vec![b'x'; 1_000_000];
    assert!(!jit.is_match(&large_input));

    // Test with pattern at the end
    let mut input_with_pattern = vec![b'x'; 1_000_000];
    input_with_pattern.extend_from_slice(b"abc");
    assert_eq!(jit.find(&input_with_pattern), Some((1_000_000, 1_000_003)));
}

#[test]
fn test_dense_transitions() {
    // Pattern with many valid byte transitions (should trigger dense code path)
    let jit = jit_compile("[a-zA-Z0-9_]");

    assert!(jit.is_full_match(b"a"));
    assert!(jit.is_full_match(b"Z"));
    assert!(jit.is_full_match(b"5"));
    assert!(jit.is_full_match(b"_"));
    assert!(!jit.is_full_match(b"!"));
    assert!(!jit.is_full_match(b" "));
}

#[test]
fn test_word_boundary() {
    // Test a simple word pattern
    let jit = jit_compile("[a-z]+");

    assert!(jit.is_full_match(b"word"));
    assert!(jit.is_full_match(b"hello"));
    assert!(!jit.is_full_match(b""));
}

// ============================================================================
// Regressions in engine agreement
// ============================================================================

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
