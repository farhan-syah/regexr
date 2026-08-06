//! Backtracking out of a greedy Unicode codepoint run.
//!
//! A greedy codepoint quantifier (`\p{L}+`, `\p{N}+`, …) matches as far as it
//! can and then gives characters back one at a time until the rest of the
//! pattern fits. Giving one back means moving to the previous **codepoint**
//! boundary, not the previous byte, so every case below deliberately mixes
//! character widths: 1-byte ASCII, 2-byte Latin/Greek/Cyrillic, 3-byte CJK, and
//! 4-byte astral characters.
//!
//! The interpreter used to record each boundary in a `Vec` on the way in and
//! walk it in reverse. It now steps backwards through UTF-8 directly, which is
//! only correct because continuation bytes are distinguishable from leading
//! ones — so a run of multi-byte characters is exactly where a mistake would
//! show up, and exactly what these tests pin.
//!
//! Each case is checked twice: once in its plain form, which the engine
//! selector is free to route anywhere, and once with a trailing `(?=$)`. The
//! lookahead is not there for what it asserts — it is there because a pattern
//! carrying a lookaround is routed to the tagged-NFA interpreter in every
//! feature configuration, which is the code the backwards walk lives in. The
//! plain form alone lands on the PikeVM and would leave the walk untested.

use super::regex;

/// Backtracking one character out of a run of 3-byte characters.
#[test]
fn greedy_letters_give_back_one_multibyte_character() {
    // The run must give back the final 好 so the literal can match it.
    for pattern in [r"\p{L}+好", r"\p{L}+好(?=$)"] {
        let m = regex(pattern).find("你我好").expect("matches");
        assert_eq!(m.as_str(), "你我好", "pattern {pattern}");
    }
}

/// Backtracking across characters of DIFFERENT widths in one run.
#[test]
fn greedy_letters_backtrack_across_mixed_widths() {
    // `a` (1) + `é` (2) + `中` (3) + `𝔸` (4), then a required trailing letter.
    for pattern in [r"(\p{L}+)(\p{L})", r"(\p{L}+)(\p{L})(?=$)"] {
        let re = regex(pattern);
        let caps = re.captures("aé中𝔸z").expect("matches");
        assert_eq!(
            caps.get(1).map(|m| m.as_str()),
            Some("aé中𝔸"),
            "pattern {pattern}"
        );
        assert_eq!(
            caps.get(2).map(|m| m.as_str()),
            Some("z"),
            "pattern {pattern}"
        );
    }
}

/// Backtracking all the way down to the single mandatory character.
#[test]
fn greedy_letters_backtrack_to_the_mandatory_first_character() {
    // `+` must keep at least one character; the rest of the run is given back.
    for pattern in [r"(\p{L}+)(\p{L}+)$", r"(\p{L}+)(\p{L}+)(?=$)"] {
        let re = regex(pattern);
        let caps = re.captures("中文字").expect("matches");
        let (first, second) = (
            caps.get(1).map(|m| m.as_str()).expect("group 1"),
            caps.get(2).map(|m| m.as_str()).expect("group 2"),
        );
        assert_eq!(format!("{first}{second}"), "中文字", "pattern {pattern}");
        assert!(
            !first.is_empty() && !second.is_empty(),
            "pattern {pattern}: both groups must keep their mandatory character"
        );
    }
}

/// A run that cannot give back enough must fail, not match a partial codepoint.
#[test]
fn greedy_letters_fail_rather_than_split_a_codepoint() {
    // Nothing here can satisfy a trailing digit, so the whole match fails.
    for pattern in [r"\p{L}+\p{N}", r"\p{L}+\p{N}(?=$)"] {
        assert!(regex(pattern).find("中文字").is_none(), "pattern {pattern}");
    }
}

/// The same backtracking inside a lookahead, which has its own greedy path.
#[test]
fn greedy_letters_backtrack_inside_a_lookahead() {
    let re = regex(r"(?=\p{L}+好)\p{L}+");
    let m = re.find("你我好").expect("matches");
    assert_eq!(m.as_str(), "你我好");
}

/// A negative lookahead over a multi-byte run: the pre-tokenizer shape that
/// first surfaced this path (`\s+(?!\S)` alongside `\p{L}+` alternatives).
#[test]
fn greedy_letters_under_a_negative_lookahead() {
    let re = regex(r"\p{L}+(?!\p{N})");
    let m = re.find("中文字").expect("matches");
    assert_eq!(m.as_str(), "中文字");

    // With a digit attached, the run gives characters back until the negative
    // lookahead is satisfied.
    let m = re.find("中文字7").expect("matches");
    assert_eq!(m.as_str(), "中文");
}

/// Long runs, so the backwards walk is exercised well past any small-buffer
/// threshold a previous implementation might have had.
#[test]
fn greedy_letters_backtrack_over_a_long_multibyte_run() {
    let text: String = "中".repeat(500) + "字";
    for pattern in [r"\p{L}+字", r"\p{L}+字(?=$)"] {
        let m = regex(pattern).find(&text).expect("matches");
        assert_eq!(m.as_str(), text, "pattern {pattern}");
    }
}

/// Astral (4-byte) characters, the widest step the backwards walk can take.
#[test]
fn greedy_letters_backtrack_over_astral_characters() {
    let text: String = "𝔹".repeat(50) + "𝔸";
    for pattern in [r"\p{L}+𝔸", r"\p{L}+𝔸(?=$)"] {
        let m = regex(pattern).find(&text).expect("matches");
        assert_eq!(m.as_str(), text, "pattern {pattern}");
    }
}
