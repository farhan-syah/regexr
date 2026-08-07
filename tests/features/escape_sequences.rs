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
