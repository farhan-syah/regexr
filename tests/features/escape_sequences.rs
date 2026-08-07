//! Character escapes: `\n`, `\t`, `\e`, `\a`, and escaped punctuation.
//!
//! Each escape is exercised at the start of a pattern, inside a pattern, and
//! inside a character class, because those are three separate code paths.

use regexr::Regex;

/// Compiles a pattern through the same builder path the rest of the suite uses.
fn compile(pattern: &str) -> Result<Regex, regexr::Error> {
    #[cfg(feature = "jit")]
    {
        regexr::RegexBuilder::new(pattern).jit(true).build()
    }
    #[cfg(not(feature = "jit"))]
    {
        Regex::new(pattern)
    }
}

/// Asserts that `escape` denotes exactly `expected` — matching it, rejecting a
/// neighbouring character, and behaving the same in all three positions.
fn assert_escape_denotes(escape: &str, expected: char) {
    let subject = expected.to_string();
    let other = if expected == 'x' { 'y' } else { 'x' }.to_string();

    for pattern in [
        escape.to_string(),
        format!("a{escape}"),
        format!("[{escape}]"),
    ] {
        let re = compile(&pattern).unwrap_or_else(|e| panic!("{pattern:?} should compile: {e}"));

        let haystack = if pattern.starts_with('a') {
            format!("a{subject}")
        } else {
            subject.clone()
        };
        assert!(
            re.is_match(&haystack),
            "{pattern:?} should match {haystack:?} (U+{:04X})",
            expected as u32
        );
        assert!(
            !re.is_match(&other),
            "{pattern:?} matched {other:?}, so it is not anchored to U+{:04X}",
            expected as u32
        );
    }
}

#[test]
fn control_character_escapes_denote_their_control_characters() {
    assert_escape_denotes(r"\n", '\n');
    assert_escape_denotes(r"\r", '\r');
    assert_escape_denotes(r"\t", '\t');
    assert_escape_denotes(r"\f", '\u{0c}');
    assert_escape_denotes(r"\v", '\u{0b}');
    assert_escape_denotes(r"\0", '\0');
}

/// `\e` is the escape character (U+001B) in PCRE, Perl, Java and the `regex`
/// crate. It is a character escape, not an assertion, so it is valid inside a
/// character class too.
#[test]
fn escape_character_escape_denotes_u001b() {
    assert_escape_denotes(r"\e", '\u{1b}');
}

/// `\a` is the alert/bell character (U+0007) in PCRE, Perl, Java and the
/// `regex` crate.
#[test]
fn alert_escape_denotes_u0007() {
    assert_escape_denotes(r"\a", '\u{07}');
}

/// Escaping a non-alphanumeric ASCII character always yields that character.
/// Patterns written for other engines lean on this heavily — an escaped quote,
/// space, or `@` is far more common than the metacharacters that strictly need
/// escaping.
#[test]
fn escaped_ascii_punctuation_denotes_itself() {
    for c in ' '..='~' {
        if c.is_ascii_alphanumeric() {
            continue;
        }
        assert_escape_denotes(&format!("\\{c}"), c);
    }
}

/// `\x{...}` is the braced hex escape: 1-6 hex digits denoting that code
/// point, identical to `\u{...}`. The two-digit `\xHH` form is unchanged.
#[test]
fn braced_hex_escape_denotes_the_code_point() {
    assert_escape_denotes(r"\x{41}", 'A');
    assert_escape_denotes(r"\x{263A}", '\u{263A}');
    assert_escape_denotes(r"\x{1F600}", '\u{1F600}');
}

/// The pre-existing two-digit form keeps working unchanged.
#[test]
fn two_digit_hex_escape_still_works() {
    assert_escape_denotes(r"\x41", 'A');
}

/// `\cX` is the control escape: the letter's ASCII code with bit 0x40
/// cleared. Both cases are accepted and agree with each other.
#[test]
fn control_escape_denotes_the_control_character() {
    assert_escape_denotes(r"\cA", '\u{01}');
    assert_escape_denotes(r"\ca", '\u{01}');
    assert_escape_denotes(r"\cZ", '\u{1A}');
    assert_escape_denotes(r"\cz", '\u{1A}');
}

/// Inside a character class only, `\b` denotes backspace (U+0008) rather
/// than the word-boundary assertion it is everywhere else.
#[test]
fn backspace_inside_class_denotes_u0008() {
    let re = compile(r"[\b]").unwrap_or_else(|e| panic!(r"[\b] should compile: {e}"));
    assert!(re.is_match("\u{8}"));
    assert!(!re.is_match("b"));
}

/// Outside a character class, `\b` is still the word-boundary assertion —
/// the class-only backspace meaning must not leak out.
#[test]
fn bare_word_boundary_is_unaffected_by_class_backspace() {
    let re = compile(r"\bcat\b").unwrap();
    assert!(re.is_match("a cat sat"));
    assert!(!re.is_match("concatenate"));
}

/// Escaping an unassigned ASCII *letter* stays an error rather than silently
/// decaying to the letter itself, so that adding a meaning to it later is not a
/// breaking change.
#[test]
fn unassigned_letter_escapes_remain_errors() {
    for pattern in [r"\j", r"\J", r"\m", r"\T", r"\Y", r"\g", r"\l"] {
        assert!(
            compile(pattern).is_err(),
            "{pattern:?} has no assigned meaning and must not compile"
        );
    }
}

/// `\h` matches horizontal whitespace: Tab, Space, and the Unicode horizontal
/// space separators (e.g. U+00A0, U+3000) — but rejects an ordinary letter
/// and the vertical whitespace character `\n`.
#[test]
fn horizontal_whitespace_matches_tab_space_and_unicode_separators() {
    for pattern in [r"\h", r"a\h", r"[\h]"] {
        let re = compile(pattern).unwrap_or_else(|e| panic!("{pattern:?} should compile: {e}"));
        for c in ['\t', ' ', '\u{A0}', '\u{3000}'] {
            let haystack = if pattern.starts_with('a') {
                format!("a{c}")
            } else {
                c.to_string()
            };
            assert!(
                re.is_match(&haystack),
                "{pattern:?} should match {haystack:?} (U+{:04X})",
                c as u32
            );
        }
        let rejected = if pattern.starts_with('a') { "ab" } else { "a" };
        assert!(!re.is_match(rejected), "{pattern:?} matched {rejected:?}");
        assert!(!re.is_match("\n"), "{pattern:?} matched a newline");
    }
}

/// `\H` is the complement of `\h`: it matches an ordinary letter and `\n`,
/// but rejects a tab and U+00A0.
#[test]
fn not_horizontal_whitespace_matches_everything_else() {
    for pattern in [r"\H", r"[\H]"] {
        let re = compile(pattern).unwrap_or_else(|e| panic!("{pattern:?} should compile: {e}"));
        assert!(re.is_match("a"), "{pattern:?} should match 'a'");
        assert!(re.is_match("\n"), "{pattern:?} should match a newline");
        assert!(!re.is_match("\t"), "{pattern:?} should not match a tab");
        assert!(
            !re.is_match("\u{A0}"),
            "{pattern:?} should not match U+00A0"
        );
    }
}

/// `\H` matches a whole code point, never a single byte of a multi-byte
/// character — the same UTF-8 correctness `\S`/`.`/negated classes already
/// guarantee.
#[test]
fn not_horizontal_whitespace_matches_whole_multibyte_character() {
    let re = compile(r"\H").unwrap();
    let m = re.find("é").expect(r"\H should match 'é'");
    assert_eq!(m.as_str(), "é");
    assert_eq!(m.end() - m.start(), 2, "match should span both UTF-8 bytes");
}

/// `[\h\d]` unions the two Perl-class shorthands: it matches a space (from
/// `\h`) and a digit (from `\d`).
#[test]
fn horizontal_whitespace_unions_with_other_perl_classes_in_a_class() {
    let re = compile(r"[\h\d]").unwrap();
    assert!(re.is_match(" "));
    assert!(re.is_match("5"));
    assert!(!re.is_match("a"));
}

/// `\R` matches any Unicode line-break sequence as a single unit. Against
/// `"\r\n"` it must consume *both* characters — the two-character `\r\n`
/// branch is tried before the single-character `\r` branch — not just the
/// leading `\r`.
#[test]
fn line_break_consumes_crlf_as_one_match() {
    let re = compile(r"\R").unwrap();
    let m = re.find("\r\n").expect(r"\R should match \r\n");
    assert_eq!(m.as_str(), "\r\n");
    assert_eq!(m.start(), 0);
    assert_eq!(m.end(), 2);
}

/// `\R` also matches each single-character line break on its own: LF, VT,
/// FF, CR (without a following LF), NEL, LINE SEPARATOR, PARAGRAPH SEPARATOR.
#[test]
fn line_break_matches_each_single_character_break() {
    let re = compile(r"\R").unwrap();
    for c in [
        '\n', '\u{0B}', '\u{0C}', '\r', '\u{85}', '\u{2028}', '\u{2029}',
    ] {
        let haystack = c.to_string();
        let m = re
            .find(&haystack)
            .unwrap_or_else(|| panic!(r"\R should match U+{:04X}", c as u32));
        assert_eq!(m.as_str(), haystack);
    }
}

/// `\R` composes with surrounding literals: `a\Rb` matches `"a\r\nb"`, with
/// the line break consuming both `\r` and `\n`.
#[test]
fn line_break_composes_with_surrounding_literals() {
    let re = compile(r"a\Rb").unwrap();
    assert!(re.is_match("a\r\nb"));
    let m = re.find("a\r\nb").unwrap();
    assert_eq!(m.as_str(), "a\r\nb");
}

/// `\N` matches any code point except line feed — including a whole
/// multi-byte character — but never `\n`.
#[test]
fn any_except_newline_matches_any_char_but_newline() {
    let re = compile(r"\N").unwrap();
    assert!(re.is_match("a"));
    let m = re.find("é").expect(r"\N should match 'é'");
    assert_eq!(m.as_str(), "é");
    assert!(!re.is_match("\n"));
}

/// `\N` is unaffected by the `s` (dot-all) flag: unlike `.`, `(?s)\N` must
/// still reject line feed. `.` under `(?s)` is unchanged and still matches
/// `\n`, pinning that `\N`'s dot-all immunity did not leak onto `.`.
#[test]
fn any_except_newline_ignores_dot_all_flag() {
    let re = compile(r"(?s)\N").unwrap();
    assert!(!re.is_match("\n"), "(?s)\\N must still reject a newline");

    let dot = compile(r"(?s).").unwrap();
    assert!(
        dot.is_match("\n"),
        "(?s). must still match a newline, unaffected by \\N"
    );
}

/// `\N{NAME}` (the named-Unicode-character form) is rejected with a clear
/// error rather than being misparsed as bare `\N` followed by a literal `{`.
#[test]
fn named_unicode_character_escape_is_rejected() {
    let err = compile(r"\N{LATIN SMALL LETTER A}")
        .expect_err(r"\N{...} should be rejected")
        .to_string();
    assert!(
        err.to_lowercase().contains("named"),
        "error should explain the named form is unsupported: {err}"
    );
}

/// `\R` and `\N` are each a hard error inside a character class: `\R` can
/// span two characters and `\N` is not a fixed set of code points, so
/// neither can be a class member.
#[test]
fn line_break_and_any_except_newline_are_rejected_inside_a_class() {
    for pattern in [r"[\R]", r"[\N]"] {
        assert!(
            compile(pattern).is_err(),
            "{pattern:?} must be rejected inside a character class"
        );
    }
}
