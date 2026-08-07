//! Malformed patterns must be rejected by the compiler, not silently accepted.
//!
//! The dangerous failure mode for a regex engine is not a bad error message —
//! it is a malformed pattern that compiles into an *empty* pattern, because an
//! empty pattern matches at every position of every input, including the empty
//! string. A caller that trusts `Regex::new` then silently matches everything.
//!
//! These tests therefore assert two things everywhere: that invalid syntax is
//! rejected, and that rejection does not depend on *where* in the pattern the
//! invalid syntax appears.

use regexr::{Regex, Result};

/// Compiles a pattern through the same builder path the rest of the suite uses,
/// returning the error instead of panicking.
fn compile(pattern: &str) -> Result<Regex> {
    #[cfg(feature = "jit")]
    {
        regexr::RegexBuilder::new(pattern).jit(true).build()
    }
    #[cfg(not(feature = "jit"))]
    {
        Regex::new(pattern)
    }
}

/// Patterns that no escape-table extension can ever make valid: the escape is
/// truncated or its argument is malformed.
const MALFORMED_ESCAPES: &[&str] = &[
    r"\",
    r"\x",
    r"\xZ",
    r"\xZZ",
    r"\x{",
    r"\u",
    r"\uZZ",
    r"\u{",
    r"\u{ZZZZ}",
    r"\p",
    r"\p{",
    r"\p{L",
    r"\P{",
    r"[[:bogus:]]",
    r"[[:alpha]",
    r"[[::]]",
    r"\x{}",
    r"\x{ZZ}",
    r"\x{110000}",
    r"\x{263A",
    r"\c",
    r"\c1",
    // `\R` and `\N` are multi-/single-character escapes that cannot be a
    // class member; `\N{NAME}` (named Unicode characters) is unsupported.
    r"[\R]",
    r"[\N]",
    r"\N{LATIN SMALL LETTER A}",
    // Named backreferences: undefined name, empty name, unterminated,
    // missing/unrecognized delimiter, and mismatched delimiter pairs.
    r"\k<nosuch>",
    r"\k<>",
    r"\k<w",
    r"\k",
    r"\kX",
    r"\k<w}",
    r"\k{w>",
    // (?P=name): undefined name, empty name, unterminated.
    r"(?P=nosuch)",
    r"(?P=)",
    r"(?P=w",
];

#[test]
fn malformed_escape_at_pattern_start_is_rejected() {
    for pattern in MALFORMED_ESCAPES {
        assert!(
            compile(pattern).is_err(),
            "{pattern:?} must not compile: the escape is malformed"
        );
    }
}

/// Regression guard on the failure *mode*, not just the return value: if a
/// malformed pattern ever compiles again, it must at least not have degenerated
/// into the empty pattern, which matches every input everywhere.
#[test]
fn malformed_escape_never_compiles_to_a_match_everything_pattern() {
    for pattern in MALFORMED_ESCAPES {
        if let Ok(re) = compile(pattern) {
            assert!(
                !(re.is_match("") && re.is_match("zzz") && re.is_match("\u{1b}")),
                "{pattern:?} compiled into a pattern that matches every input"
            );
        }
    }
}

/// The core invariant this file exists to pin: whether an escape is accepted is
/// a property of the escape, never of its offset in the pattern. A leading
/// escape is lexed by a different code path than every later token, and that
/// path must report failures identically.
#[test]
fn escape_acceptance_does_not_depend_on_position() {
    for c in ' '..='~' {
        let leading = format!("\\{c}");
        let trailing = format!("a\\{c}");

        assert_eq!(
            compile(&leading).is_ok(),
            compile(&trailing).is_ok(),
            "\\{c} is accepted at one position and rejected at another \
             ({leading:?} vs {trailing:?})"
        );
    }
}

/// Same invariant for the escapes whose argument is malformed rather than
/// whose leading character is unknown.
#[test]
fn malformed_escape_acceptance_does_not_depend_on_position() {
    for pattern in MALFORMED_ESCAPES {
        let trailing = format!("a{pattern}");
        assert_eq!(
            compile(pattern).is_ok(),
            compile(&trailing).is_ok(),
            "{pattern:?} is accepted at one position and rejected at another"
        );
    }
}

/// A rejected leading escape must point at the start of the pattern. A span
/// pointing elsewhere means the error was manufactured downstream of the site
/// that actually failed.
#[test]
fn leading_invalid_escape_reports_position_zero() {
    let err = compile(r"\q")
        .expect_err(r"\q must be rejected")
        .to_string();
    assert!(
        err.contains("position 0"),
        "error should locate the escape at position 0: {err}"
    );
}

/// Anchors are not character-class members. Rejecting them is correct; naming
/// them in the diagnostic is what makes the rejection actionable.
///
/// `\b` is deliberately excluded here: inside a class it denotes backspace
/// (U+0008), matching PCRE/Perl, rather than being rejected like the other
/// assertions. See `escape_sequences::backspace_inside_class_denotes_u0008`.
#[test]
fn class_escape_error_names_the_offending_escape() {
    for (pattern, escape) in [(r"[\B]", r"\B"), (r"[\z]", r"\z")] {
        let err = compile(pattern)
            .expect_err(&format!("{pattern} must be rejected"))
            .to_string();
        assert!(
            err.contains(escape),
            "diagnostic for {pattern} should name {escape}: {err}"
        );
        assert!(
            !err.contains(r"\?"),
            "diagnostic for {pattern} reports a placeholder instead of the escape: {err}"
        );
    }
}

/// The empty pattern is legitimately match-everything. It is the only pattern
/// that may behave this way, which is why every test above treats
/// match-everything as the signature of a swallowed compile error.
#[test]
fn empty_pattern_is_the_only_legitimate_match_everything() {
    let re = compile("").expect("the empty pattern is valid");
    assert!(re.is_match(""));
    assert!(re.is_match("anything"));
}
