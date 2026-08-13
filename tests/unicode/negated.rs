//! Negated Unicode character class tests ([^...]).
//!
//! When the `jit` feature is enabled, these tests use JIT compilation.

use super::regex;

#[test]
fn test_negated_unicode_class_greek() {
    let re = regex("[^α-ω]+");

    assert!(re.is_match("abc"));
    assert!(re.is_match("XYZ"));
    assert!(re.is_match("123"));
    assert!(re.is_match("中文"));
    assert!(re.is_match("😀"));

    assert!(!re.is_match("αβγ"));
    assert!(!re.is_match("ωψχ"));

    let m = re.find("αβγabcδεζ").unwrap();
    assert_eq!(m.as_str(), "abc");
}

#[test]
fn test_negated_unicode_class_cjk() {
    let re = regex("[^一-龥]+");

    assert!(re.is_match("hello"));
    assert!(re.is_match("world"));
    assert!(re.is_match("αβγ"));

    assert!(!re.is_match("中文"));
    assert!(!re.is_match("汉字"));

    let m = re.find("中文hello世界").unwrap();
    assert_eq!(m.as_str(), "hello");
}

#[test]
fn test_negated_unicode_class_emoji() {
    let re = regex("[^😀-😂]+");

    assert!(re.is_match("hello"));
    assert!(re.is_match("αβγ"));
    assert!(re.is_match("中文"));
    assert!(re.is_match("🎉"));

    assert!(!re.is_match("😀"));
    assert!(!re.is_match("😁"));
    assert!(!re.is_match("😂"));
}

#[test]
fn test_negated_unicode_class_ascii() {
    let re = regex("[^a-z]+");

    assert!(re.is_match("ABC"));
    assert!(re.is_match("123"));
    assert!(re.is_match("!@#"));
    assert!(re.is_match("αβγ"));
    assert!(re.is_match("中文"));
    assert!(re.is_match("😀"));

    assert!(!re.is_match("abc"));
    assert!(!re.is_match("xyz"));

    let m = re.find("abc123xyz").unwrap();
    assert_eq!(m.as_str(), "123");
}

#[test]
fn test_negated_unicode_class_single_char() {
    let re = regex("[^α]+");

    assert!(re.is_match("abc"));
    assert!(re.is_match("βγδ"));
    assert!(re.is_match("中文"));

    assert!(!re.is_match("α"));

    let m = re.find("ααβγδαα").unwrap();
    assert_eq!(m.as_str(), "βγδ");
}

#[test]
fn test_negated_unicode_class_multiple_ranges() {
    let re = regex("[^a-zA-Z]+");

    assert!(re.is_match("123"));
    assert!(re.is_match("!@#$"));
    assert!(re.is_match("αβγ"));
    assert!(re.is_match("中文"));

    assert!(!re.is_match("abc"));
    assert!(!re.is_match("XYZ"));
    assert!(!re.is_match("Hello"));

    let m = re.find("hello123world").unwrap();
    assert_eq!(m.as_str(), "123");
}

#[test]
fn test_negated_unicode_class_find_iter() {
    let re = regex("[^α-ω]+");

    let matches: Vec<_> = re.find_iter("αβγ123δεζabc").collect();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].as_str(), "123");
    assert_eq!(matches[1].as_str(), "abc");
}

#[test]
fn test_negated_unicode_mixed_with_quantifiers() {
    let re = regex("[^α-ω]*");

    assert!(re.is_match(""));
    assert!(re.is_match("abc"));
    assert!(re.is_match("123"));

    let m = re.find("abc αβγ").unwrap();
    assert_eq!(m.as_str(), "abc ");
}

/// A negated class must exclude exactly the code points it names — not the
/// hull of the UTF-8 sequences they compile to.
///
/// U+0810-U+083F and U+0850-U+087F encode to the single sequences
/// `E0 A0 90-BF` and `E0 A1 90-BF`. They differ in one byte position whose
/// values are adjacent, so sequence optimization merges them into
/// `E0 A0-A1 90-BF` — a shape that also encodes U+0840-U+084F, which the class
/// never named. Reading the excluded set back out of the merged sequence
/// therefore over-excludes, and the negated class rejected U+0840. Building it
/// from the class's own code-point ranges keeps the gap open.
#[test]
fn test_negated_class_excludes_only_the_codepoints_it_names() {
    let re = regex(r"^[^\x{810}-\x{83F}\x{850}-\x{87F}]$");

    // In the gap between the two excluded ranges: must match.
    assert!(
        re.is_match("\u{840}"),
        "U+0840 lies between the excluded ranges and must not be excluded"
    );
    assert!(
        re.is_match("\u{84F}"),
        "U+084F lies between the excluded ranges and must not be excluded"
    );

    // The named ranges themselves, including both edges: must not match.
    for cp in [
        '\u{810}', '\u{820}', '\u{83F}', '\u{850}', '\u{86F}', '\u{87F}',
    ] {
        assert!(
            !re.is_match(&cp.to_string()),
            "U+{:04X} is named by the class and must be excluded",
            cp as u32
        );
    }

    // Either side of the whole span, and an unrelated character.
    assert!(re.is_match("\u{80F}"));
    assert!(re.is_match("\u{880}"));
    assert!(re.is_match("a"));
}

/// The `[^\s\p{L}\p{N}]`-shaped class from the tiktoken split patterns still
/// admits and rejects the right characters after being built from its exact
/// code-point ranges.
#[test]
fn test_negated_multi_property_class() {
    let re = regex(r"^[^\s\p{L}\p{N}]+$");

    // Punctuation and symbols: neither space, letter nor number.
    assert!(re.is_match("!@#"));
    assert!(re.is_match("\u{2014}")); // em dash
    assert!(re.is_match("\u{2018}\u{2019}")); // curly quotes
    assert!(re.is_match("\u{1F600}")); // emoji

    // Letters, in several scripts and widths.
    assert!(!re.is_match("a"));
    assert!(!re.is_match("\u{E9}")); // é
    assert!(!re.is_match("\u{3B1}")); // α
    assert!(!re.is_match("\u{4E2D}")); // 中

    // Numbers, including a non-ASCII one.
    assert!(!re.is_match("7"));
    assert!(!re.is_match("\u{B2}")); // superscript two

    // Whitespace, in several widths.
    assert!(!re.is_match(" "));
    assert!(!re.is_match("\u{A0}"));
    assert!(!re.is_match("\u{2003}"));
    assert!(!re.is_match("\u{3000}"));
}

#[test]
fn test_negated_unicode_class_complex() {
    let re = regex("[^α-ω]+[0-9]+");

    assert!(re.is_match("abc123"));
    assert!(re.is_match("中文456"));
    assert!(re.is_match("αβγ123"));
    assert!(re.is_match("αβγabc123"));

    let re2 = regex("^[^α-ω]+$");
    assert!(!re2.is_match("αβγ"));
    assert!(re2.is_match("abc"));
    assert!(re2.is_match("123"));
}
