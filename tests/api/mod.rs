//! Public API integration tests.
//!
//! Tests for the core `Regex` type and its methods: `new`, `is_match`, `find`,
//! `find_iter`, `captures`, `captures_iter`, `replace`, `replace_all`.
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.

use regexr::Regex;
#[cfg(feature = "jit")]
use regexr::RegexBuilder;

/// Creates a Regex with JIT enabled when the `jit` feature is available.
#[allow(dead_code)]
fn regex(pattern: &str) -> Regex {
    #[cfg(feature = "jit")]
    {
        RegexBuilder::new(pattern)
            .jit(true)
            .build()
            .expect("failed to compile pattern")
    }
    #[cfg(not(feature = "jit"))]
    {
        Regex::new(pattern).expect("failed to compile pattern")
    }
}

// =============================================================================
// Regex::new and as_str
// =============================================================================

#[test]
fn test_regex_new() {
    let re = Regex::new("hello").unwrap();
    assert_eq!(re.as_str(), "hello");
}

// =============================================================================
// is_match
// =============================================================================

#[test]
fn test_is_match() {
    let re = regex("hello");
    assert!(re.is_match("hello world"));
    assert!(re.is_match("say hello"));
    assert!(!re.is_match("goodbye"));
}

// =============================================================================
// find
// =============================================================================

#[test]
fn test_find() {
    let re = regex("world");
    let m = re.find("hello world").unwrap();
    assert_eq!(m.start(), 6);
    assert_eq!(m.end(), 11);
    assert_eq!(m.as_str(), "world");
    assert_eq!(m.len(), 5);
    assert!(!m.is_empty());
}

#[test]
fn test_find_none() {
    let re = regex("xyz");
    assert!(re.find("hello world").is_none());
}

#[test]
fn test_match_range() {
    let re = regex("test");
    let m = re.find("this is a test").unwrap();
    assert_eq!(m.range(), 10..14);
}

#[test]
fn test_empty_match() {
    let re = regex("a*");
    let m = re.find("bbb").unwrap();
    assert!(m.is_empty());
    assert_eq!(m.len(), 0);
}

// =============================================================================
// find_iter
// =============================================================================

#[test]
fn test_find_iter() {
    let re = regex("a");
    let matches: Vec<_> = re.find_iter("abracadabra").collect();
    assert_eq!(matches.len(), 5);
    assert_eq!(matches[0].start(), 0);
    assert_eq!(matches[1].start(), 3);
    assert_eq!(matches[2].start(), 5);
    assert_eq!(matches[3].start(), 7);
    assert_eq!(matches[4].start(), 10);
}

#[test]
fn test_find_iter_empty() {
    let re = regex("xyz");
    let matches: Vec<_> = re.find_iter("hello world").collect();
    assert!(matches.is_empty());
}

// =============================================================================
// find_iter over multi-byte UTF-8
// =============================================================================
//
// `.` and byte classes match a single byte (this is the engine's canonical
// semantics, see `regexr::reference`), so a match can end inside a multi-byte
// codepoint. The iterator reports that span as-is but always resumes at the
// next codepoint boundary, so no match ever starts inside a codepoint and no
// `&str` is ever sliced at a non-boundary.

/// Collects `(start, end)` for every match, driving the iterator to completion.
fn ranges(re: &Regex, text: &str) -> Vec<(usize, usize)> {
    re.find_iter(text).map(|m| (m.start(), m.end())).collect()
}

#[test]
fn test_find_iter_dot_over_multibyte_text() {
    // The reported panic: `.` over text containing a 3-byte codepoint.
    // "Hello " is bytes 0..6, '世' is 6..9, '界' is 9..12, '!' is 12..13.
    let re = regex(".");
    let text = "Hello 世界!";

    assert_eq!(
        ranges(&re, text),
        vec![
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 4),
            (4, 5),
            (5, 6),
            (6, 7),
            (9, 10),
            (12, 13),
        ]
    );

    // The two spans that split a codepoint have no `&str` representation.
    let strs: Vec<_> = re.find_iter(text).map(|m| m.as_str()).collect();
    assert_eq!(strs, vec!["H", "e", "l", "l", "o", " ", "", "", "!"]);

    // ...but the bytes are always available and exact.
    let bytes: Vec<_> = re.find_iter(text).map(|m| m.as_bytes().to_vec()).collect();
    assert_eq!(bytes[6], vec![0xE4]); // lead byte of '世'
    assert_eq!(bytes[7], vec![0xE7]); // lead byte of '界'
}

#[test]
fn test_find_iter_dot_over_two_byte_codepoint() {
    // 'a' is 0..1, 'é' is 1..3, 'b' is 3..4.
    let re = regex(".");
    assert_eq!(ranges(&re, "aéb"), vec![(0, 1), (1, 2), (3, 4)]);
}

#[test]
fn test_find_iter_dot_over_three_byte_codepoint() {
    // '世' is 0..3, '界' is 3..6.
    let re = regex(".");
    assert_eq!(ranges(&re, "世界"), vec![(0, 1), (3, 4)]);
}

#[test]
fn test_find_iter_dot_over_four_byte_codepoint() {
    // 'x' is 0..1, '🎉' is 1..5, 'y' is 5..6.
    let re = regex(".");
    assert_eq!(ranges(&re, "x🎉y"), vec![(0, 1), (1, 2), (5, 6)]);
}

#[test]
fn test_find_iter_never_starts_inside_a_codepoint() {
    // The invariant the resume rule maintains, over mixed 1/2/3/4-byte text.
    let re = regex(".");
    let text = "a é 世 🎉 z";
    for m in re.find_iter(text) {
        assert!(
            text.is_char_boundary(m.start()),
            "match at {} starts inside a codepoint",
            m.start()
        );
    }
}

#[test]
fn test_find_iter_empty_matches_over_multibyte_text() {
    // The pre-existing empty-match guard: one empty match per codepoint plus
    // one at the end, and the iterator terminates.
    // 'é' is 0..2, '世' is 2..5.
    let re = regex("x*");
    assert_eq!(ranges(&re, "é世"), vec![(0, 0), (2, 2), (5, 5)]);
}

#[test]
fn test_find_iter_ascii_behaviour_unchanged() {
    // Every ASCII index is a codepoint boundary, so the resume rule is a no-op.
    let re = regex(".");
    assert_eq!(ranges(&re, "abc"), vec![(0, 1), (1, 2), (2, 3)]);
    let strs: Vec<_> = re.find_iter("abc").map(|m| m.as_str()).collect();
    assert_eq!(strs, vec!["a", "b", "c"]);

    let re = regex("x*");
    assert_eq!(ranges(&re, "abc"), vec![(0, 0), (1, 1), (2, 2), (3, 3)]);
}

#[test]
fn test_captures_iter_over_multibyte_text() {
    // `CapturesIter` resumes the same way as `find_iter`.
    let re = regex("(.)");
    let spans: Vec<_> = re
        .captures_iter("世界")
        .map(|c| {
            let m = c.get(0).unwrap();
            (m.start(), m.end())
        })
        .collect();
    assert_eq!(spans, vec![(0, 1), (3, 4)]);
}

#[test]
fn test_replace_over_multibyte_text() {
    // A byte-level match removes part of a codepoint; the orphaned bytes have
    // no `str` form and become U+FFFD rather than panicking.
    let re = regex(".");
    let replaced = re.replace_all("世", "-");
    assert!(replaced.starts_with('-'));
    assert!(replaced.contains('\u{FFFD}'));

    // Codepoint-aligned matches are unaffected.
    let re = regex("世");
    assert_eq!(re.replace_all("a世b", "-"), "a-b");
    assert_eq!(re.replace("a世b世", "-"), "a-b世");
}

// =============================================================================
// captures
// =============================================================================

#[test]
fn test_captures() {
    let re = regex("(\\d+)-(\\d+)");
    let caps = re.captures("phone: 123-456").unwrap();
    assert!(caps.len() >= 3);
    assert_eq!(&caps[0], "123-456");
    assert_eq!(&caps[1], "123");
    assert_eq!(&caps[2], "456");
}

// =============================================================================
// captures_iter
// =============================================================================

#[test]
fn test_captures_iter_basic() {
    let re = regex(r"(\w+)");
    let text = "hello world foo";
    let caps: Vec<_> = re.captures_iter(text).collect();
    assert_eq!(caps.len(), 3);
    assert_eq!(&caps[0][0], "hello");
    assert_eq!(&caps[1][0], "world");
    assert_eq!(&caps[2][0], "foo");
}

#[test]
fn test_captures_iter_with_groups() {
    let re = regex(r"(\w+)=(\d+)");
    let text = "a=1 b=2 c=3";
    let caps: Vec<_> = re.captures_iter(text).collect();
    assert_eq!(caps.len(), 3);
    assert_eq!(&caps[0][1], "a");
    assert_eq!(&caps[0][2], "1");
    assert_eq!(&caps[1][1], "b");
    assert_eq!(&caps[1][2], "2");
    assert_eq!(&caps[2][1], "c");
    assert_eq!(&caps[2][2], "3");
}

#[test]
fn test_captures_iter_named() {
    let re = regex(r"(?<key>\w+)=(?<value>\d+)");
    let text = "x=10 y=20";
    let caps: Vec<_> = re.captures_iter(text).collect();
    assert_eq!(caps.len(), 2);
    assert_eq!(&caps[0]["key"], "x");
    assert_eq!(&caps[0]["value"], "10");
    assert_eq!(&caps[1]["key"], "y");
    assert_eq!(&caps[1]["value"], "20");
}

#[test]
fn test_captures_iter_positions() {
    let re = regex(r"(\d+)");
    let text = "a1b22c333";
    let caps: Vec<_> = re.captures_iter(text).collect();
    assert_eq!(caps.len(), 3);
    assert_eq!(caps[0].get(0).unwrap().start(), 1);
    assert_eq!(caps[0].get(0).unwrap().end(), 2);
    assert_eq!(caps[1].get(0).unwrap().start(), 3);
    assert_eq!(caps[1].get(0).unwrap().end(), 5);
    assert_eq!(caps[2].get(0).unwrap().start(), 6);
    assert_eq!(caps[2].get(0).unwrap().end(), 9);
}

// =============================================================================
// replace
// =============================================================================

#[test]
fn test_replace() {
    let re = regex("world");
    let result = re.replace("hello world", "rust");
    assert_eq!(result, "hello rust");
}

#[test]
fn test_replace_no_match() {
    let re = regex("xyz");
    let result = re.replace("hello world", "rust");
    assert_eq!(result, "hello world");
}

// =============================================================================
// replace_all
// =============================================================================

#[test]
fn test_replace_all() {
    let re = regex("o");
    let result = re.replace_all("hello world", "0");
    assert_eq!(result, "hell0 w0rld");
}

#[test]
fn test_replace_all_no_match() {
    let re = regex("xyz");
    let result = re.replace_all("hello world", "!");
    assert_eq!(result, "hello world");
}

// =============================================================================
// Prefix Optimization
// =============================================================================

#[cfg(feature = "jit")]
mod prefix_opt {
    use regexr::RegexBuilder;

    #[test]
    fn test_prefix_optimized_basic() {
        // Pattern with many tokens sharing common prefixes
        let re = RegexBuilder::new(r"the|that|them|they|this")
            .optimize_prefixes(true)
            .build()
            .unwrap();

        assert!(re.is_match("the"));
        assert!(re.is_match("that"));
        assert!(re.is_match("them"));
        assert!(re.is_match("they"));
        assert!(re.is_match("this"));
        assert!(!re.is_match("those"));
    }

    #[test]
    fn test_prefix_optimized_find() {
        let re = RegexBuilder::new(r"apple|application|apply|apt")
            .optimize_prefixes(true)
            .build()
            .unwrap();

        let m = re.find("the application was running").unwrap();
        assert_eq!(m.as_str(), "application");
    }

    #[test]
    fn test_prefix_optimized_multiple_branches() {
        // Words that share some prefixes but not all
        let re = RegexBuilder::new(r"test|testing|tested|tester|apple|application")
            .optimize_prefixes(true)
            .build()
            .unwrap();

        assert!(re.is_match("test"));
        assert!(re.is_match("testing"));
        assert!(re.is_match("tested"));
        assert!(re.is_match("tester"));
        assert!(re.is_match("apple"));
        assert!(re.is_match("application"));
        // "tests" matches because it contains "test"
        assert!(re.is_match("tests"));
        // But "xyz" doesn't match
        assert!(!re.is_match("xyz"));
    }

    #[test]
    fn test_prefix_optimized_with_jit() {
        // Combine prefix optimization with JIT
        let re = RegexBuilder::new(r"the|that|them|they|this")
            .optimize_prefixes(true)
            .jit(true)
            .build()
            .unwrap();

        assert!(re.is_match("the"));
        assert!(re.is_match("that"));
        assert!(re.is_match("them"));
        assert!(re.is_match("they"));
        assert!(re.is_match("this"));
        assert!(!re.is_match("those"));
    }

    #[test]
    fn test_prefix_optimized_find_iter() {
        let re = RegexBuilder::new(r"the|that|them|they")
            .optimize_prefixes(true)
            .build()
            .unwrap();

        let text = "the cat that sat on them made they jump";
        let matches: Vec<_> = re.find_iter(text).map(|m| m.as_str()).collect();
        // Leftmost-first/PCRE semantics: the first alternative `the` wins wherever
        // it matches, so "them" and "they" both match as their prefix "the".
        // (Python: re.findall(r"the|that|them|they", text) == ['the','that','the','the'].)
        assert_eq!(matches, vec!["the", "that", "the", "the"]);
    }
}

// =============================================================================
// JIT Alternation Tests
// =============================================================================

#[cfg(feature = "jit")]
mod jit_alternation {
    use regexr::RegexBuilder;

    #[test]
    fn test_jit_simple_alternation() {
        let re = RegexBuilder::new(r"foo|bar").jit(true).build().unwrap();

        assert!(re.is_match("foo"));
        assert!(re.is_match("bar"));
        assert!(!re.is_match("baz"));
    }

    #[test]
    fn test_jit_alternation_find() {
        let re = RegexBuilder::new(r"foo|bar").jit(true).build().unwrap();

        let m = re.find("xyzfoo123").unwrap();
        assert_eq!(m.start(), 3);
        assert_eq!(m.end(), 6);
        assert_eq!(m.as_str(), "foo");

        let m = re.find("xyzbar123").unwrap();
        assert_eq!(m.start(), 3);
        assert_eq!(m.end(), 6);
        assert_eq!(m.as_str(), "bar");
    }

    #[test]
    fn test_jit_alternation_multi() {
        let re = RegexBuilder::new(r"hello|world|test")
            .jit(true)
            .build()
            .unwrap();

        assert!(re.is_match("hello"));
        assert!(re.is_match("world"));
        assert!(re.is_match("test"));
        assert!(!re.is_match("other"));

        let m = re.find("say hello there").unwrap();
        assert_eq!(m.as_str(), "hello");
    }

    #[test]
    fn test_jit_alternation_with_char_class() {
        let re = RegexBuilder::new(r"[a-z]+|[0-9]+")
            .jit(true)
            .build()
            .unwrap();

        assert!(re.is_match("abc"));
        assert!(re.is_match("123"));

        let m = re.find("...abc...").unwrap();
        assert_eq!(m.as_str(), "abc");

        let m = re.find("...123...").unwrap();
        assert_eq!(m.as_str(), "123");
    }

    #[test]
    fn test_jit_alternation_find_iter() {
        let re = RegexBuilder::new(r"foo|bar").jit(true).build().unwrap();

        let text = "foo bar foo bar baz";
        let matches: Vec<_> = re.find_iter(text).map(|m| m.as_str()).collect();
        assert_eq!(matches, vec!["foo", "bar", "foo", "bar"]);
    }

    #[test]
    fn test_jit_alternation_different_lengths() {
        let re = RegexBuilder::new(r"a|bb|ccc").jit(true).build().unwrap();

        assert!(re.is_match("a"));
        assert!(re.is_match("bb"));
        assert!(re.is_match("ccc"));

        let m = re.find("xxaxx").unwrap();
        assert_eq!(m.as_str(), "a");

        let m = re.find("xxbbxx").unwrap();
        assert_eq!(m.as_str(), "bb");

        let m = re.find("xxcccxx").unwrap();
        assert_eq!(m.as_str(), "ccc");
    }

    #[test]
    fn test_jit_captures_basic() {
        // Test that JIT captures work correctly
        let re = RegexBuilder::new(r"([a-z]+)=([0-9]+)")
            .jit(true)
            .build()
            .unwrap();

        let caps = re.captures("key=123").unwrap();
        assert_eq!(&caps[0], "key=123");
        assert_eq!(&caps[1], "key");
        assert_eq!(&caps[2], "123");
    }

    #[test]
    fn test_jit_captures_find_in_text() {
        let re = RegexBuilder::new(r"([a-z]+):([0-9]+)")
            .jit(true)
            .build()
            .unwrap();

        let caps = re.captures("data is foo:42 and bar:99").unwrap();
        assert_eq!(&caps[0], "foo:42");
        assert_eq!(&caps[1], "foo");
        assert_eq!(&caps[2], "42");
    }

    #[test]
    fn test_jit_captures_iter() {
        let re = RegexBuilder::new(r"([a-z]+)=([0-9]+)")
            .jit(true)
            .build()
            .unwrap();

        let text = "a=1 b=2 c=3";
        let caps: Vec<_> = re.captures_iter(text).collect();
        assert_eq!(caps.len(), 3);
        assert_eq!(&caps[0][1], "a");
        assert_eq!(&caps[0][2], "1");
        assert_eq!(&caps[1][1], "b");
        assert_eq!(&caps[1][2], "2");
        assert_eq!(&caps[2][1], "c");
        assert_eq!(&caps[2][2], "3");
    }
}

// =============================================================================
// escape()
// =============================================================================

mod escape_fn {
    use regexr::{escape, Regex};

    /// Builds a real `Regex` from `escape(s)` and asserts it matches `s`
    /// literally at that position: the escaped pattern must match the whole
    /// string, and anchoring it with `^...$` must match nothing else.
    fn assert_round_trips(s: &str) {
        let pattern = escape(s);
        let re = Regex::new(&pattern)
            .unwrap_or_else(|e| panic!("escape({s:?}) = {pattern:?} failed to compile: {e}"));
        let m = re
            .find(s)
            .unwrap_or_else(|| panic!("escape({s:?}) = {pattern:?} did not match {s:?} literally"));
        assert_eq!(m.as_str(), s, "escape({s:?}) matched the wrong substring");

        // Anchored, it must match `s` exactly and nothing longer or shorter.
        let anchored = format!("^(?:{pattern})$");
        let anchored_re = Regex::new(&anchored)
            .unwrap_or_else(|e| panic!("anchored pattern {anchored:?} failed to compile: {e}"));
        assert!(
            anchored_re.is_match(s),
            "anchored escape({s:?}) = {anchored:?} did not fully match {s:?}"
        );
    }

    #[test]
    fn test_round_trip_empty_string() {
        assert_round_trips("");
    }

    #[test]
    fn test_round_trip_all_metacharacters() {
        // Every character regexr's parser treats specially at the top level.
        assert_round_trips(r"\.*+?|^$(){}[]");
    }

    #[test]
    fn test_round_trip_no_metacharacters() {
        assert_round_trips("hello world 123");
    }

    #[test]
    fn test_round_trip_mixed_literal_and_meta() {
        assert_round_trips("a.b*c(d)[e]{f}g|h^i$j\\k");
    }

    #[test]
    fn test_round_trip_embedded_whitespace_and_newlines() {
        assert_round_trips("line one\nline two\ttabbed  double-spaced\r\n");
    }

    #[test]
    fn test_round_trip_non_ascii() {
        // Multi-byte UTF-8: accented Latin, CJK, and a 4-byte emoji. None of
        // these should be split or corrupted by byte-wise escaping.
        assert_round_trips("héllo wörld");
        assert_round_trips("こんにちは世界");
        assert_round_trips("emoji: 😀🎉👍");
    }

    #[test]
    fn test_round_trip_context_dependent_characters() {
        // These are only special *inside* other constructs in regexr's own
        // parser (`-` inside `[...]`, `:` `<` `>` `=` `!` `,` inside `(?...)`
        // and `{n,m}`, digits inside backreferences) but parse as plain
        // literals at the top level, which is the only context `escape`'s
        // output is used in. They must still round-trip correctly.
        assert_round_trips("a-b:c<d>e=f!g,h123");
        assert_round_trips("range: 1-100, ratio: 3:2");
    }

    #[test]
    fn test_round_trip_unmatched_brackets_and_parens() {
        // Individually these would be parse errors (or change meaning) if
        // left unescaped: unmatched `)`, `]`, `}`, or an unescaped `(`.
        assert_round_trips(")");
        assert_round_trips("]");
        assert_round_trips("}");
        assert_round_trips("(");
        assert_round_trips("[");
        assert_round_trips("{");
    }

    #[test]
    fn test_round_trip_backslash_sequences() {
        // Text that looks like escape sequences (\d, \n, \\) must be matched
        // as those literal characters, not interpreted as regexr escapes.
        assert_round_trips(r"\d\w\s\n\t\\");
    }

    #[test]
    fn test_round_trip_repetition_like_text() {
        // Looks like a quantified atom (`a{2,4}`) but must match only the
        // literal text `a{2,4}`, not "a repeated 2-4 times".
        assert_round_trips("a{2,4}");
        assert!(!Regex::new(&escape("a{2,4}")).unwrap().is_match("aa"));
    }

    #[test]
    fn test_escape_used_as_literal_delimiter() {
        // The motivating use case: splitting/matching on a literal delimiter
        // that happens to be a regex metacharacter.
        let re = Regex::new(&escape(".")).unwrap();
        assert!(re.is_match("a.b"));
        assert!(!re.is_match("axb"));

        let re = Regex::new(&escape("|")).unwrap();
        assert!(re.is_match("a|b"));
        assert!(!re.is_match("ab"));
    }

    #[test]
    fn test_escape_does_not_over_escape_safe_characters() {
        // Characters that are already safe unescaped should pass through
        // byte-for-byte, so escape() output stays minimal and readable.
        for s in ["-", ":", "<", ">", "=", "!", ",", "&", "~", "/", "0"] {
            assert_eq!(escape(s), s);
        }
    }

    #[test]
    fn test_escape_output_survives_extended_mode() {
        // Extended mode strips unescaped whitespace and `#` comments, so
        // escaped text spliced into an `(?x)` pattern must keep them.
        for s in ["a b", "a#b", "a # not a comment\nb", "a\tb"] {
            let re = Regex::new(&format!("(?x){}", escape(s)))
                .unwrap_or_else(|e| panic!("escape({s:?}) failed to compile under (?x): {e}"));
            let m = re
                .find(s)
                .unwrap_or_else(|| panic!("escape({s:?}) did not match under (?x)"));
            assert_eq!(m.as_str(), s);
        }
    }
}
