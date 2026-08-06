//! `\X` — one extended grapheme cluster (UAX #29).
//!
//! Each test names the boundary rule it pins, because the failure mode is
//! subtle: a wrong rule still matches *something*, just the wrong number of
//! codepoints.

use super::regex;
use regexr::Regex;

/// Splits `text` into the clusters `\X` finds, as codepoint counts.
fn cluster_sizes(text: &str) -> Vec<usize> {
    regex(r"\X")
        .find_iter(text)
        .map(|m| m.as_str().chars().count())
        .collect()
}

/// Every cluster together must reconstruct the input — a rule that drops or
/// duplicates a codepoint would otherwise pass the per-cluster counts.
fn assert_partitions(text: &str) {
    let joined: String = regex(r"\X").find_iter(text).map(|m| m.as_str()).collect();
    assert_eq!(joined, text, "clusters do not partition {text:?}");
}

#[test]
fn combining_marks_attach_to_their_base() {
    // GB9: Extend never starts a cluster.
    assert_eq!(cluster_sizes("e\u{301}"), vec![2]);
    assert_eq!(cluster_sizes("n\u{303}"), vec![2]);
    assert_eq!(cluster_sizes("a\u{301}b"), vec![2, 1]);
    assert_eq!(cluster_sizes("e\u{301}\u{327}"), vec![3]);
    assert_partitions("a\u{301}b\u{303}c");
}

#[test]
fn crlf_is_one_cluster_but_cr_and_lf_alone_are_not() {
    // GB3 keeps CRLF together; GB4/GB5 keep every other control apart.
    assert_eq!(cluster_sizes("\r\n"), vec![2]);
    assert_eq!(cluster_sizes("\n\r"), vec![1, 1]);
    assert_eq!(cluster_sizes("\r"), vec![1]);
    assert_eq!(cluster_sizes("a\r\nb"), vec![1, 2, 1]);
}

#[test]
fn a_control_never_absorbs_a_following_mark() {
    // GB4: even though Extend normally attaches leftward, a control refuses it.
    assert_eq!(cluster_sizes("\u{7}\u{301}"), vec![1, 1]);
}

#[test]
fn hangul_jamo_form_one_syllable() {
    // GB6-GB8: L V T is one syllable.
    assert_eq!(cluster_sizes("\u{1100}\u{1161}\u{11A8}"), vec![3]);
    // LV followed by T.
    assert_eq!(cluster_sizes("\u{AC00}\u{11A8}"), vec![2]);
    // Two complete syllables stay apart.
    assert_eq!(
        cluster_sizes("\u{1100}\u{1161}\u{1100}\u{1161}"),
        vec![2, 2]
    );
    assert_partitions("\u{1100}\u{1161}\u{11A8}\u{1100}\u{1161}");
}

#[test]
fn emoji_zwj_sequences_are_one_cluster() {
    // GB11: the joined codepoints render as a single glyph.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(cluster_sizes(family), vec![5]);

    // A ZWJ sequence carrying a skin-tone modifier (Extend) before the joiner.
    let couple = "\u{1F468}\u{1F3FB}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F468}";
    assert_eq!(cluster_sizes(couple), vec![7]);

    // Adjacent emoji with no joiner stay separate.
    assert_eq!(cluster_sizes("\u{1F600}\u{1F600}"), vec![1, 1]);
    assert_partitions(family);
}

#[test]
fn regional_indicators_pair_into_flags() {
    // GB12/GB13: exactly two, so four indicators are two flags rather than one
    // long cluster.
    assert_eq!(cluster_sizes("\u{1F1FA}\u{1F1F8}"), vec![2]);
    assert_eq!(
        cluster_sizes("\u{1F1FA}\u{1F1F8}\u{1F1EF}\u{1F1F5}"),
        vec![2, 2]
    );
    // An odd indicator is its own cluster.
    assert_eq!(cluster_sizes("\u{1F1FA}\u{1F1F8}\u{1F1EF}"), vec![2, 1]);
    assert_partitions("\u{1F1FA}\u{1F1F8}\u{1F1EF}\u{1F1F5}");
}

#[test]
fn indic_conjuncts_are_not_split_at_the_linker() {
    // GB9c: consonant + virama + consonant renders as one conjunct glyph, so
    // क्ष is one cluster and not three.
    assert_eq!(cluster_sizes("\u{915}\u{94D}\u{937}"), vec![3]);
    // Two conjuncts in a row.
    assert_eq!(
        cluster_sizes("\u{915}\u{94D}\u{937}\u{915}\u{94D}\u{937}"),
        vec![3, 3]
    );
    assert_partitions("\u{915}\u{94D}\u{937}\u{928}");
}

#[test]
fn spacing_marks_attach_to_their_base() {
    // GB9a: a spacing mark is part of the preceding cluster.
    assert_eq!(cluster_sizes("\u{0E01}\u{0E33}"), vec![2]);
    assert_eq!(cluster_sizes("\u{0915}\u{093E}"), vec![2]);
}

#[test]
fn ascii_text_is_one_cluster_per_character() {
    assert_eq!(cluster_sizes("abc"), vec![1, 1, 1]);
    assert_partitions("abc");
}

#[test]
fn grapheme_cluster_composes_with_quantifiers_and_anchors() {
    let re = regex(r"^\X{3}$");
    assert!(re.is_match("a\u{301}b\u{303}c"));
    assert!(!re.is_match("a\u{301}b\u{303}"));

    let re = regex(r"\X+");
    assert_eq!(re.find("e\u{301}x").map(|m| m.as_str()), Some("e\u{301}x"));
}

#[test]
fn grapheme_cluster_escape_is_rejected_inside_a_character_class() {
    // `\X` matches a sequence, so it cannot be a member of a set.
    let err = Regex::new(r"[\X]")
        .expect_err(r"[\X] must be rejected")
        .to_string();
    assert!(
        err.contains(r"\X"),
        "diagnostic should name the escape: {err}"
    );
}

#[test]
fn grapheme_cluster_never_matches_empty() {
    // A cluster is at least one codepoint; matching empty would make `\X*`
    // loop forever at a position it cannot advance past.
    let re = regex(r"\X");
    assert!(!re.is_match(""));
    assert_eq!(re.find("a").map(|m| m.as_str()), Some("a"));
}

/// `\X` behaves as if it were atomic: a following pattern that fails must not
/// be able to make it match part of a cluster. Each case is a single cluster,
/// so only `\X{1}` may match it.
#[test]
fn a_cluster_is_never_split_by_backtracking() {
    let single_cluster = [
        "e\u{301}",                                    // base + mark
        "\r\n",                                        // CRLF
        "\u{1100}\u{1161}\u{11A8}",                    // Hangul jamo
        "\u{1F1FA}\u{1F1F8}",                          // flag
        "\u{915}\u{94D}\u{937}",                       // Indic conjunct
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", // emoji ZWJ sequence
        "\u{600}",                                     // lone Prepend
    ];

    for text in single_cluster {
        assert!(
            regex(r"^\X$").is_match(text),
            "{text:?} should be exactly one cluster"
        );
        for n in 2..=6 {
            let re = regex(&format!(r"^\X{{{n}}}$"));
            assert!(
                !re.is_match(text),
                r"\X{{{n}}} matched {text:?}, which is one cluster — \X gave part of it back"
            );
        }
    }
}
