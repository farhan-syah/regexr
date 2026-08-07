//! Regex syntax feature integration tests.
//!
//! Tests for alternation, character classes, quantifiers, anchors, and shorthand classes.
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.

// Using local mod.rs

use super::regex;

// =============================================================================
// Alternation
// =============================================================================

#[test]
fn test_alternation() {
    let re = regex("cat|dog");
    assert!(re.is_match("I have a cat"));
    assert!(re.is_match("I have a dog"));
    assert!(!re.is_match("I have a bird"));
}

#[test]
fn test_dot_star() {
    let re = regex("a.*b");
    assert!(re.is_match("ab"));
    assert!(re.is_match("axb"));
    assert!(re.is_match("axxxb"));
    assert!(!re.is_match("a"));
    assert!(!re.is_match("b"));
}

// =============================================================================
// Character Classes
// =============================================================================

#[test]
fn test_character_class() {
    let re = regex("[0-9]+");
    let m = re.find("abc123def").unwrap();
    assert_eq!(m.as_str(), "123");
}

/// Test caret inside character class (not at start) is literal
#[test]
fn test_caret_in_character_class() {
    // Caret not at start should match literal ^
    let re = regex("[a^b]");
    assert!(re.is_match("a"));
    assert!(re.is_match("^"));
    assert!(re.is_match("b"));
    assert!(!re.is_match("c"));

    // Caret at end
    let re2 = regex("[ab^]");
    assert!(re2.is_match("^"));

    // Multiple special chars including caret (benchmark tokenization pattern)
    let re3 = regex(r"[+\-*/=<>!&|^%]+");
    assert!(re3.is_match("^"));
    assert!(re3.is_match("+"));
    assert!(re3.is_match("^%"));
    assert!(re3.is_match("!="));
}

/// Test the full tokenization pattern from benchmarks
#[test]
fn test_tokenization_pattern() {
    let pattern = r#"[a-zA-Z_][a-zA-Z0-9_]*|[0-9]+(?:\.[0-9]+)?|[+\-*/=<>!&|^%]+|[(){}\[\];,.]|"[^"]*"|'[^']*'"#;
    let re = regex(pattern);

    // Identifiers
    assert!(re.is_match("foo"));
    assert!(re.is_match("_bar"));

    // Numbers
    assert!(re.is_match("123"));
    assert!(re.is_match("3.14"));

    // Operators
    assert!(re.is_match("+"));
    assert!(re.is_match("^"));
    assert!(re.is_match("!="));

    // Punctuation
    assert!(re.is_match("("));
    assert!(re.is_match(";"));

    // Strings
    assert!(re.is_match(r#""hello""#));
    assert!(re.is_match("'world'"));
}

// =============================================================================
// Shorthand Classes (\d, \w, \s)
// =============================================================================

#[test]
fn test_shorthand_digit() {
    let re = regex("\\d+");
    let m = re.find("abc123def").unwrap();
    assert_eq!(m.as_str(), "123");
}

#[test]
fn test_shorthand_word() {
    let re = regex("\\w+");
    let m = re.find("hello world").unwrap();
    assert_eq!(m.as_str(), "hello");
}

#[test]
fn test_shorthand_whitespace() {
    let re = regex("\\s+");
    let m = re.find("hello world").unwrap();
    assert_eq!(m.as_str(), " ");
}

#[test]
fn test_shorthand_non_digit() {
    let re = regex("\\D");
    let m = re.find("a").unwrap();
    assert_eq!(m.as_str(), "a");
}

#[test]
fn test_shorthand_non_digit_plus() {
    let re = regex("\\D+");
    let m = re.find("123abc456").unwrap();
    assert_eq!(m.as_str(), "abc");
}

#[test]
fn test_shorthand_non_digit_with_literal_prefix() {
    let re = regex("a\\Db");
    let m = re.find("axb").unwrap();
    assert_eq!(m.as_str(), "axb");
    assert!(!re.is_match("a1b"));
}

// `\W`/`\D` are negated over *characters*, not bytes, even in ASCII mode: the
// positive forms (`\w`, `\d`) stay ASCII-only, but their negations must still
// match a whole non-ASCII code point (2-, 3-, and 4-byte UTF-8) as a single
// match rather than splitting it into a continuation byte. See `[^a]`, which
// already had this property.
#[test]
fn test_shorthand_not_word_matches_whole_multibyte_codepoints() {
    let re = regex("\\W");

    // 2-byte: é (U+00E9)
    let m = re.find("é").unwrap();
    assert_eq!(m.as_str(), "é");
    assert_eq!(m.start(), 0);
    assert_eq!(m.end(), 2);

    // 3-byte: € (U+20AC)
    let m = re.find("€").unwrap();
    assert_eq!(m.as_str(), "€");
    assert_eq!(m.end(), 3);

    // 4-byte: 😀 (U+1F600)
    let m = re.find("😀").unwrap();
    assert_eq!(m.as_str(), "😀");
    assert_eq!(m.end(), 4);
}

#[test]
fn test_shorthand_not_digit_matches_whole_multibyte_codepoints() {
    let re = regex("\\D");

    // 2-byte: é
    let m = re.find("é").unwrap();
    assert_eq!(m.as_str(), "é");
    assert_eq!(m.end(), 2);

    // 3-byte: 世 (U+4E16)
    let m = re.find("世").unwrap();
    assert_eq!(m.as_str(), "世");
    assert_eq!(m.end(), 3);

    // 4-byte: 😀
    let m = re.find("😀").unwrap();
    assert_eq!(m.as_str(), "😀");
    assert_eq!(m.end(), 4);
}

#[test]
fn test_shorthand_not_word_anchored_around_multibyte() {
    let re = regex("^\\W$");
    let m = re.find("é").unwrap();
    assert_eq!(m.as_str(), "é");
}

#[test]
fn test_shorthand_not_word_between_literals_around_multibyte() {
    let re = regex("X\\WY");
    let m = re.find("XéY").unwrap();
    assert_eq!(m.as_str(), "XéY");
}

#[test]
fn test_shorthand_not_word_rejects_word_chars() {
    let re = regex("\\W");
    assert!(!re.is_match("a"));
    assert!(!re.is_match("Z"));
    assert!(!re.is_match("9"));
    assert!(!re.is_match("_"));
}

#[test]
fn test_shorthand_not_digit_rejects_digit() {
    let re = regex("\\D");
    assert!(!re.is_match("7"));
}

/// `\w`/`\d` stay ASCII-only (deliberately, unlike `\s`/`\S` which are
/// Unicode by default) — they must not match non-ASCII word characters.
#[test]
fn test_shorthand_word_is_ascii_only() {
    let re = regex("^\\w+$");
    assert!(!re.is_match("é"));
    assert!(re.is_match("abc"));
}

// =============================================================================
// Quantifiers
// =============================================================================

#[test]
fn test_plus() {
    let re = regex("a+");
    assert!(re.is_match("a"));
    assert!(re.is_match("aaa"));
    assert!(!re.is_match("b"));
}

#[test]
fn test_optional() {
    let re = regex("a?");
    assert!(re.is_match("a"));
    assert!(re.is_match(""));
}

#[test]
fn test_star() {
    let re = regex("a*");
    assert!(re.is_match(""));
    assert!(re.is_match("aaa"));
}

// Lazy quantifiers (a*?, a+?, a??, a{2,3}?): the trailing `?` after a
// quantifier selects non-greedy matching and must keep parsing successfully
// — it is not a possessive suffix and must not be confused with one.
#[test]
fn test_lazy_star() {
    let re = regex("a*?");
    let m = re.find("aaa").unwrap();
    assert_eq!(m.as_str(), "");
}

#[test]
fn test_lazy_plus() {
    let re = regex("a+?");
    let m = re.find("aaa").unwrap();
    assert_eq!(m.as_str(), "a");
}

#[test]
fn test_lazy_optional() {
    let re = regex("a??");
    let m = re.find("aaa").unwrap();
    assert_eq!(m.as_str(), "");
}

#[test]
fn test_lazy_bounded_repeat() {
    let re = regex("a{2,3}?");
    let m = re.find("aaaa").unwrap();
    assert_eq!(m.as_str(), "aa");
}

// =============================================================================
// Anchors
// =============================================================================

#[test]
fn test_start_anchor() {
    let re = regex("^hello");
    assert!(re.is_match("hello world"));
    assert!(!re.is_match("say hello")); // Not at start
    assert!(!re.is_match("  hello")); // Not at start
}

#[test]
fn test_end_anchor() {
    let re = regex("world$");
    assert!(re.is_match("hello world"));
    assert!(!re.is_match("world is big")); // Not at end
    assert!(!re.is_match("world  ")); // Not at end
}

#[test]
fn test_both_anchors() {
    let re = regex("^hello$");
    assert!(re.is_match("hello")); // Exact match
    assert!(!re.is_match("hello world")); // Has suffix
    assert!(!re.is_match("say hello")); // Has prefix
    assert!(!re.is_match(" hello ")); // Has both
}

#[test]
fn test_anchored_pattern() {
    let re = regex("^[a-z]+$");
    assert!(re.is_match("hello"));
    assert!(re.is_match("world"));
    assert!(!re.is_match("hello world")); // Has space
    assert!(!re.is_match("Hello")); // Has uppercase
    assert!(!re.is_match("123")); // Has digits
}

#[test]
fn test_multiline_start_anchor() {
    let re = regex("(?m)^hello");
    assert!(re.is_match("hello world")); // At start
    assert!(re.is_match("first\nhello")); // After newline
    assert!(re.is_match("line1\nline2\nhello")); // After multiple newlines
    assert!(!re.is_match("say hello")); // Not at start of line
}

#[test]
fn test_multiline_end_anchor() {
    let re = regex("(?m)world$");
    assert!(re.is_match("hello world")); // At end
    assert!(re.is_match("world\nnext")); // Before newline
    assert!(!re.is_match("world hello")); // Not at end of line
}

#[test]
fn test_empty_with_anchors() {
    let re = regex("^$");
    assert!(re.is_match("")); // Empty string
    assert!(!re.is_match("x")); // Non-empty
}

// =============================================================================
// Bounded Repeats {n}, {n,m}, {n,}
// =============================================================================
// These tests document correctness requirements for bounded repeat quantifiers.
// Found via rebar benchmarks - ShiftOr engine was not enforcing bounds.

#[test]
fn test_exact_repeat() {
    let re = regex("a{3}");

    // Should match exactly 3 'a's
    assert!(re.is_match("aaa"));
    assert!(re.is_match("xaaax")); // embedded

    // Should NOT match fewer than 3
    assert!(!re.is_match("a"));
    assert!(!re.is_match("aa"));

    // Find should return exactly 3 chars
    let m = re.find("aaaa").unwrap();
    assert_eq!(m.as_str(), "aaa");
    assert_eq!(m.len(), 3);
}

#[test]
fn test_exact_repeat_class() {
    let re = regex("[a-z]{3}");

    // Should match exactly 3 lowercase letters
    let m = re.find("abcd").unwrap();
    assert_eq!(m.len(), 3);

    // Should NOT match fewer than 3
    assert!(!re.is_match("ab"));
    assert!(!re.is_match("a"));
}

#[test]
fn test_bounded_repeat_range() {
    let re = regex("[A-Za-z]{8,13}");

    // Should match 8-13 letters
    assert!(re.is_match("abcdefgh")); // 8 chars - minimum
    assert!(re.is_match("abcdefghijklm")); // 13 chars - maximum

    // Should NOT match fewer than 8
    assert!(!re.is_match("hello")); // 5 chars
    assert!(!re.is_match("testing")); // 7 chars
    assert!(!re.is_match("abc")); // 3 chars

    // Find should respect bounds
    let m = re.find("abcdefghijklmnopqrstuvwxyz").unwrap();
    assert!(
        m.len() >= 8 && m.len() <= 13,
        "expected 8-13, got {}",
        m.len()
    );
}

#[test]
fn test_bounded_repeat_min_only() {
    let re = regex("a{3,}");

    // Should match 3 or more 'a's
    assert!(re.is_match("aaa"));
    assert!(re.is_match("aaaa"));
    assert!(re.is_match("aaaaaaaa"));

    // Should NOT match fewer than 3
    assert!(!re.is_match("a"));
    assert!(!re.is_match("aa"));
}

#[test]
fn test_bounded_repeat_in_find_iter() {
    let re = regex("[A-Za-z]{8,13}");
    let text = "ab hello testing abcdefghij worldtesting xy";

    let matches: Vec<_> = re.find_iter(text).collect();

    // Only "abcdefghij" (10 chars) and "worldtesting" (12 chars) should match
    for m in &matches {
        assert!(
            m.len() >= 8 && m.len() <= 13,
            "match {:?} has invalid length {}",
            m.as_str(),
            m.len()
        );
    }
}

// =============================================================================
// Multi-word Alternation
// =============================================================================
// These tests document correctness for alternations with multi-word literals.
// Found via rebar benchmarks - patterns were matching partial strings.

#[test]
fn test_multiword_alternation() {
    let re = regex("Sherlock Holmes|John Watson");

    // Should match complete phrases
    let text = "Sherlock Holmes met John Watson";

    let matches: Vec<_> = re.find_iter(text).collect();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].as_str(), "Sherlock Holmes");
    assert_eq!(matches[1].as_str(), "John Watson");
}

#[test]
fn test_multiword_alternation_partial() {
    let re = regex("Sherlock Holmes|John Watson|Irene Adler");

    // Should NOT match partial strings
    let text = "Sherlock is here"; // Only "Sherlock", not "Sherlock Holmes"
    assert!(!re.is_match(text));

    // Should match complete phrase
    let text2 = "Sherlock Holmes is here";
    assert!(re.is_match(text2));
    let m = re.find(text2).unwrap();
    assert_eq!(m.as_str(), "Sherlock Holmes");
}

#[test]
fn test_long_alternation_five_options() {
    let re = regex("Sherlock Holmes|John Watson|Irene Adler|Inspector Lestrade|Professor Moriarty");

    let test_cases = [
        ("Sherlock Holmes", true),
        ("John Watson", true),
        ("Irene Adler", true),
        ("Inspector Lestrade", true),
        ("Professor Moriarty", true),
        ("Sherlock", false), // partial
        ("Holmes", false),   // partial
        ("John", false),     // partial
    ];

    for (text, expected) in test_cases {
        assert_eq!(
            re.is_match(text),
            expected,
            "is_match({:?}) should be {}",
            text,
            expected
        );
    }
}

#[test]
fn test_alternation_match_length() {
    // Ensure alternation matches the full alternative, not truncated
    let re = regex("cat|dog|bird");

    let m1 = re.find("I have a cat").unwrap();
    assert_eq!(m1.as_str(), "cat");
    assert_eq!(m1.len(), 3);

    let m2 = re.find("I have a bird").unwrap();
    assert_eq!(m2.as_str(), "bird");
    assert_eq!(m2.len(), 4);
}

// =============================================================================
// UTF-8 Boundary Handling
// =============================================================================
// Tests for proper UTF-8 character boundary handling in iterators.
// Found via rebar benchmarks - empty matches caused panics on multi-byte chars.

#[test]
fn test_utf8_bom_handling() {
    // BOM is 3 bytes: \u{feff} = EF BB BF
    let text_with_bom = "\u{feff}Hello";
    let re = regex(".*");

    // Should not panic when iterating matches on text starting with BOM
    let matches: Vec<_> = re.find_iter(text_with_bom).collect();
    assert!(!matches.is_empty());
}

#[test]
fn test_utf8_multibyte_iteration() {
    // Test iteration over text with multi-byte UTF-8 characters
    let text = "héllo wörld";
    let re = regex(""); // Empty pattern matches at every position

    // Should not panic - must advance by char boundaries, not bytes
    let count = re.find_iter(text).count();
    // Empty pattern should match at each character position (11 chars + final)
    assert!(count > 0);
}

#[test]
fn test_utf8_emoji_iteration() {
    // Emoji can be 4 bytes
    let text = "a😀b";
    let re = regex(""); // Empty pattern

    // Should handle 4-byte emoji correctly
    let matches: Vec<_> = re.find_iter(text).collect();
    assert!(!matches.is_empty());
}

#[test]
fn test_empty_match_advancement() {
    // Empty matches should advance by one character, not one byte
    let text = "abc";
    let re = regex("");

    let matches: Vec<_> = re.find_iter(text).collect();
    // Should match at positions: before 'a', before 'b', before 'c', after 'c'
    assert_eq!(matches.len(), 4);
}

// =============================================================================
// Rebar Benchmark Patterns (Sherlock)
// =============================================================================
// These patterns come from the rebar benchmark suite's sherlock tests.

#[test]
fn test_sherlock_name_patterns() {
    // Basic literal patterns from sherlock benchmarks
    let re_sherlock = regex("Sherlock");
    let re_holmes = regex("Holmes");
    let re_both = regex("Sherlock Holmes");

    let text = "Sherlock Holmes is a detective. Holmes solved the case.";

    assert_eq!(re_sherlock.find_iter(text).count(), 1);
    assert_eq!(re_holmes.find_iter(text).count(), 2);
    assert_eq!(re_both.find_iter(text).count(), 1);
}

#[test]
fn test_sherlock_alternation_patterns() {
    // Alternation patterns from sherlock benchmarks (name-alt1 through name-alt5)
    let re_alt1 = regex("Sherlock|Street");
    let re_alt2 = regex("Sherlock|Holmes");
    let re_alt3 = regex("Sherlock|Holmes|Watson");

    let text = "Sherlock Holmes and Watson walked down Baker Street";

    assert!(re_alt1.find_iter(text).count() >= 2); // Sherlock, Street
    assert!(re_alt2.find_iter(text).count() >= 2); // Sherlock, Holmes
    assert!(re_alt3.find_iter(text).count() >= 3); // Sherlock, Holmes, Watson
}

#[test]
fn test_long_literal_over_8_chars() {
    // Literals longer than 8 characters should match completely
    // This tests the fix for truncated prefix extraction
    let re = regex("Investigating");
    let text = "Sherlock was Investigating the crime";

    let m = re.find(text).unwrap();
    assert_eq!(m.as_str(), "Investigating");
    assert_eq!(m.len(), 13);
}

#[test]
fn test_long_alternation_all_over_8_chars() {
    // All alternatives are longer than 8 characters
    let re = regex("Investigating|Encyclopedia|Understanding");

    let text = "The Encyclopedia contains Understanding of Investigating";
    let matches: Vec<_> = re.find_iter(text).collect();

    assert_eq!(matches.len(), 3);
    assert_eq!(matches[0].as_str(), "Encyclopedia");
    assert_eq!(matches[1].as_str(), "Understanding");
    assert_eq!(matches[2].as_str(), "Investigating");
}

// =============================================================================
// Case-Insensitive Tests (Known Limitations)
// =============================================================================
// These tests document case-insensitive matching behavior.
// Some may fail if case-insensitive isn't fully implemented for all engines.

#[test]
fn test_case_insensitive_basic() {
    let re = regex("(?i)sherlock");
    let text = "SHERLOCK and Sherlock and sherlock";

    let count = re.find_iter(text).count();
    assert_eq!(count, 3, "Should match all case variants");
}

#[test]
fn test_case_insensitive_alternation() {
    let re = regex("(?i)sherlock|holmes|watson");
    let text = "SHERLOCK HOLMES and Watson";

    let count = re.find_iter(text).count();
    assert_eq!(count, 3);
}

// =============================================================================
// Unicode Property Tests (Known Limitations)
// =============================================================================
// These tests document Unicode property matching behavior.

#[test]
fn test_unicode_letter_property() {
    // Note: Unicode properties require braces: \p{L} not \pL
    let re = regex(r"\p{L}+");
    let text = "Hello Wörld";

    let matches: Vec<_> = re.find_iter(text).collect();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].as_str(), "Hello");
    assert_eq!(matches[1].as_str(), "Wörld");
}

#[test]
fn test_unicode_dot_all() {
    // (?s) dotall mode: .* should match across newlines
    let re = regex("(?s).*");
    let text = "Line1\nLine2";

    let m = re.find(text).unwrap();
    assert_eq!(m.as_str(), text);
}

// =============================================================================
// Inline comments (?#...)
// =============================================================================
// `(?#...)` is a group-syntax construct (like `(?:...)` or `(?i)`), not a
// character escape, so it lives here rather than in escape_sequences.rs or
// inline_flags.rs (which is scoped specifically to the `(?flags...)` group
// syntax, not inline groups in general).

#[test]
fn inline_comment_is_discarded_and_produces_no_match_content() {
    let re = regex("a(?#note)b");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("a(?#note)b"));
}

#[test]
fn inline_comment_runs_to_the_first_close_paren() {
    // The comment body can contain arbitrary characters, including ones that
    // would otherwise be metacharacters, because it is never tokenized. The
    // comment ends at the first `)`, so the pattern effectively reduces to
    // just "ab".
    let re = regex(r"a(?#[.*+?\d)b");
    assert!(re.is_match("ab"));
    assert!(!re.is_match("a"));
}

#[test]
fn unterminated_inline_comment_is_rejected() {
    assert!(regexr::Regex::new("a(?#unterminated").is_err());
}

#[test]
fn inline_comment_works_under_extended_mode() {
    // `(?#...)` is a whole-construct comment in every mode, including
    // extended (`x`) mode — it must not be swallowed by the `x`-mode `#`
    // line-comment trivia skipper.
    let re = regex("(?x)a(?#note)b");
    assert!(re.is_match("ab"));
}

#[test]
fn inline_comment_works_under_extended_mode_with_surrounding_whitespace() {
    let re = regex("(?x) a (?#note) b");
    assert!(re.is_match("ab"));
}

// =============================================================================
// Non-capturing groups (?:...)
// =============================================================================
// `(?:` shares the `TokenKind::Question` dispatch in `parse_group` with the
// atomic-group `(?>` arm added alongside it; this pins that the two remain
// distinguished.

#[test]
fn test_non_capturing_group() {
    let re = regex("(?:ab)+c");
    assert!(re.is_match("ababc"));
    assert_eq!(re.captures("ababc").unwrap().len(), 1); // no capture groups
}

// =============================================================================
// `+` as a literal, not a quantifier
// =============================================================================
// A `+` that is not immediately preceded by a quantifier (inside a character
// class, or escaped) must remain an ordinary character — it must not be
// mistaken for the possessive-quantifier suffix.

#[test]
fn test_literal_plus_in_class() {
    let re = regex(r"[a+]+");
    assert_eq!(re.find("a++a").unwrap().as_str(), "a++a");
}

#[test]
fn test_escaped_literal_plus() {
    let re = regex(r"a\+b");
    assert!(re.is_match("a+b"));
    assert!(!re.is_match("ab"));
}

// =============================================================================
// Nested character classes (set union) — Rust `regex`-crate / HF semantics
// =============================================================================

#[test]
fn test_nested_class_union() {
    // `[a[b-c]d]` is the union {a, b, c, d}.
    let re = regex(r"[a[b-c]d]+");
    let text = "abcd x";
    assert_eq!(re.find(text).unwrap().as_str(), "abcd");
}

#[test]
fn test_negated_class_with_nested() {
    // BLOOM-style: `[^(\s|[.,!])]+` — negated union of (, \s, |, ., ,, !.
    let re = regex(r"[^(\s|[.,!])]+");
    let text = "ab.c (x)";
    let m: Vec<&str> = re.find_iter(text).map(|m| m.as_str()).collect();
    assert_eq!(m, vec!["ab", "c", "x"]);
}

#[test]
fn test_negated_nested_class_complement() {
    // A negated nested class contributes its complement to the union: `[a[^0-9]]`
    // = {a} ∪ (everything except digits) = everything except digits.
    let re = regex(r"[a[^0-9]]+");
    let text = "abc123";
    assert_eq!(re.find(text).unwrap().as_str(), "abc");
}

// =============================================================================
// Quantified alternation
//
// A literal prefilter may only report a match directly when the literals are
// the WHOLE match. Under a quantifier they are just the first iteration, so the
// engine has to run: `(a|b)+` over "abab" is "abab", not "a".
// =============================================================================

#[test]
fn test_quantified_alternation_consumes_every_iteration() {
    assert_eq!(
        regex(r"(a|b)+").find("abab").map(|m| m.as_str()),
        Some("abab")
    );
    assert_eq!(
        regex(r"(?:a|b)+").find("abab").map(|m| m.as_str()),
        Some("abab")
    );
    assert_eq!(
        regex(r"(ab|cd)+").find("abab").map(|m| m.as_str()),
        Some("abab")
    );
    assert_eq!(
        regex(r"(foo|bar)+").find("foobarfoo").map(|m| m.as_str()),
        Some("foobarfoo")
    );
}

#[test]
fn test_bounded_alternation_repeat_respects_its_minimum() {
    let re = regex(r"(a|b){2,}");
    assert_eq!(re.find("abab").map(|m| m.as_str()), Some("abab"));
    // One iteration is below the minimum, so a single character must not match.
    assert!(!re.is_match("a"));
}

#[test]
fn test_unquantified_alternation_still_matches_one_branch() {
    assert_eq!(
        regex(r"foo|bar").find("bar").map(|m| m.as_str()),
        Some("bar")
    );
    assert_eq!(
        regex(r"(cat|dog)").find("dog").map(|m| m.as_str()),
        Some("dog")
    );
    assert_eq!(regex(r"(a|b)").find("ab").map(|m| m.as_str()), Some("a"));
}

#[test]
fn test_nested_sibling_groups_followed_by_a_literal() {
    // The literal prefilter must not splice a later literal onto a prefix it
    // only extracted part of: `((a)(b))c` starts with "ab", never "ac", and
    // claiming otherwise made the prefilter find no candidates and report no
    // match at all.
    let re = regex(r"((a)(b))c");
    assert_eq!(re.find("abc").map(|m| m.as_str()), Some("abc"));
    assert_eq!(re.find("xabc").map(|m| m.as_str()), Some("abc"));
    assert!(!re.is_match("acc"));

    let caps = regex(r"((a)(b))c").captures("abc").expect("should match");
    assert_eq!(caps.get(1).map(|m| m.as_str()), Some("ab"));
    assert_eq!(caps.get(2).map(|m| m.as_str()), Some("a"));
    assert_eq!(caps.get(3).map(|m| m.as_str()), Some("b"));

    // Deeper nesting and a longer tail exercise the same extension path.
    assert_eq!(
        regex(r"((a)(b)(e))cd").find("abecd").map(|m| m.as_str()),
        Some("abecd")
    );
}
