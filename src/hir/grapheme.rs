//! `\X` — one extended grapheme cluster, per [UAX #29].
//!
//! A grapheme cluster is what a reader calls "a character": a base codepoint
//! plus everything that renders as part of it — combining marks, a Hangul
//! syllable's jamo, the codepoints joined into one emoji, an Indic consonant
//! conjunct. The boundary rules are stated in UAX #29 as constraints between
//! adjacent codepoints (GB1–GB999); here they are turned inside out into an
//! expression matching exactly one cluster starting at the current position.
//!
//! The translation, with the rule each part implements:
//!
//! ```text
//! \X := CR LF                                                    (GB3)
//!     | (CR | LF | Control)                                      (GB4, GB5)
//!     | Prepend* core (Extend | ZWJ | SpacingMark)*              (GB9, GB9a, GB9b)
//!
//! core := hangul                                                 (GB6, GB7, GB8)
//!       | RI RI                                                  (GB12, GB13)
//!       | conjunct                                               (GB9c)
//!       | pictographic                                           (GB11)
//!       | [^ CR LF Control ]                                     (GB999)
//!
//! hangul      := L* (V+ | LV V* | LVT) T* | L+ | T+
//! conjunct    := Consonant (LinkerExtend* Linker LinkerExtend* Consonant)+
//! pictographic := ExtPict (Extend* ZWJ ExtPict)*
//! ```
//!
//! Alternatives are ordered longest-first because the engines are
//! leftmost-first: the cluster at a position is unique, so the first
//! alternative that can match the whole cluster is the right one.
//!
//! # Atomicity
//!
//! PCRE's `\X` is atomic: once a cluster is consumed it is never given back.
//! regexr has no atomic groups, so every branch here instead asserts that it
//! consumed as much as its rule allows — `(?!…)` after each greedy run. Without
//! those assertions a `\X` followed by a pattern that fails could backtrack
//! into the cluster and match half of it, so that e.g. `^\X{3}$` would match a
//! two-cluster string.
//!
//! `tests/unicode/grapheme_break_conformance.rs` checks the result against the
//! full Unicode grapheme-break test suite.
//!
//! [UAX #29]: https://www.unicode.org/reports/tr29/

use super::unicode_data as ud;
use super::{CodepointClass, HirExpr, HirLookaround, HirLookaroundKind, HirRepeat};

/// Builds the expression matching one extended grapheme cluster.
pub fn grapheme_cluster() -> HirExpr {
    HirExpr::Alt(vec![
        // GB3: CR LF is one cluster, and must be tried before the single
        // Control-like branch that would otherwise take the CR alone.
        concat(vec![class(ud::GCB_CR), class(ud::GCB_LF)]),
        // GB4/GB5: a control character never joins anything, in either
        // direction, so it is always a cluster by itself. The CR here is
        // guarded so it cannot take the first half of a CRLF pair.
        concat(vec![class(ud::GCB_CR), not_followed_by(class(ud::GCB_LF))]),
        class_of(&[ud::GCB_LF, ud::GCB_CONTROL]),
        concat(vec![
            // GB9b: Prepend attaches to what follows it.
            star(class(ud::GCB_PREPEND)),
            core(),
            // GB9/GB9a: Extend, ZWJ and SpacingMark attach to what precedes.
            star(postcore()),
            // The repeat above is greedy but not possessive, so a following
            // pattern that fails could otherwise make it give a combining mark
            // back and leave `\X` matching half a cluster. Asserting that
            // nothing attachable follows makes `\X` behave like PCRE's atomic
            // version even under backtracking.
            //
            // This branch only; GB4/GB5 clusters are controls, which never
            // absorb a following mark.
            not_followed_by(postcore()),
        ]),
    ])
}

/// The codepoints that attach to the end of a cluster (GB9, GB9a). A virama is
/// an Extend and attaches here like any other; it is GB9c, applied in
/// `conjunct_cluster`, that additionally keeps the *following* consonant from
/// splitting off.
fn postcore() -> HirExpr {
    class_of(&[ud::GCB_EXTEND, ud::GCB_ZWJ, ud::GCB_SPACINGMARK])
}

/// The part of a cluster that carries its identity, before the trailing
/// combining marks are attached.
fn core() -> HirExpr {
    HirExpr::Alt(vec![
        hangul_syllable(),
        // GB12/GB13: regional indicators pair up into flags. Exactly two, so a
        // run of four is two flags rather than one cluster.
        concat(vec![
            class(ud::GCB_REGIONAL_INDICATOR),
            class(ud::GCB_REGIONAL_INDICATOR),
        ]),
        conjunct_cluster(),
        pictographic_sequence(),
        // GB999: anything else is a cluster of its own. Controls are excluded
        // because they were handled above and must not be absorbed here.
        //
        // Hangul jamo and regional indicators are excluded because the branches
        // above already match every case of them, including a lone one. Leaving
        // them here would let backtracking take a single jamo or half a flag as
        // a cluster and split the syllable or the flag.
        HirExpr::UnicodeCpClass(CodepointClass::new(
            union(&[&fallback_excluded()[..], ud::INCB_CONSONANT]),
            true,
        )),
        // A consonant is held back only when it opens a conjunct, which the
        // branch above already matches in full; taking it here would split the
        // conjunct at its virama. A non-consonant base is unaffected, so
        // `a` + virama + consonant still breaks after the virama.
        concat(vec![
            class(ud::INCB_CONSONANT),
            not_followed_by(conjunct_link()),
        ]),
        concat(vec![
            class(ud::GCB_REGIONAL_INDICATOR),
            not_followed_by(class(ud::GCB_REGIONAL_INDICATOR)),
        ]),
        // GB9b makes Prepend attach rightward, so a Prepend is a cluster of its
        // own only when nothing follows it or a control does — which is also
        // the only case the `Prepend* core` form above cannot cover.
        concat(vec![
            plus(class(ud::GCB_PREPEND)),
            not_followed_by(not_class_of(&[ud::GCB_CR, ud::GCB_LF, ud::GCB_CONTROL])),
        ]),
    ])
}

/// GB6–GB8: a Hangul syllable spelled out in jamo, or a run of jamo that does
/// not form one.
fn hangul_syllable() -> HirExpr {
    let l = || class(ud::GCB_L);
    let v = || class(ud::GCB_V);
    let t = || class(ud::GCB_T);

    // Each jamo run is followed by an assertion that it consumed everything it
    // could. The repeats are greedy, but greedy is not possessive: a following
    // pattern that fails would otherwise make a syllable give back a jamo and
    // leave `\X` matching part of it.
    HirExpr::Alt(vec![
        concat(vec![
            star(l()),
            HirExpr::Alt(vec![
                concat(vec![plus(v()), not_followed_by(v())]),
                concat(vec![class(ud::GCB_LV), star(v()), not_followed_by(v())]),
                class(ud::GCB_LVT),
            ]),
            star(t()),
            not_followed_by(t()),
        ]),
        // A run of leading jamo with no vowel. Anything that GB6 would have
        // joined must already have been consumed.
        concat(vec![
            plus(l()),
            not_followed_by(class_of(&[ud::GCB_L, ud::GCB_V, ud::GCB_LV, ud::GCB_LVT])),
        ]),
        concat(vec![plus(t()), not_followed_by(t())]),
    ])
}

/// GB9c: an Indic conjunct — consonants joined by a linker (virama) — is one
/// cluster, so that e.g. Devanagari क्ष does not split at the virama.
fn conjunct_cluster() -> HirExpr {
    concat(vec![
        class(ud::INCB_CONSONANT),
        plus(conjunct_link()),
        // The repeat is greedy but not possessive; asserting that no further
        // link follows stops backtracking from ending the cluster part-way
        // through a conjunct.
        not_followed_by(conjunct_link()),
    ])
}

/// The classes GB999 must never match: each is handled by a branch above, and
/// letting the fallback take one would split the sequence that branch matches.
fn fallback_excluded() -> Vec<(u32, u32)> {
    union(&[
        ud::GCB_CR,
        ud::GCB_LF,
        ud::GCB_CONTROL,
        ud::GCB_REGIONAL_INDICATOR,
        ud::GCB_PREPEND,
        ud::GCB_L,
        ud::GCB_V,
        ud::GCB_T,
        ud::GCB_LV,
        ud::GCB_LVT,
        ud::PROP_EXTENDED_PICTOGRAPHIC,
    ])
}

/// One `virama + consonant` step of a conjunct, with the marks that may sit
/// around the virama.
fn conjunct_link() -> HirExpr {
    let linker_extend = || class_of(&[ud::INCB_EXTEND, ud::INCB_LINKER]);
    concat(vec![
        star(linker_extend()),
        class(ud::INCB_LINKER),
        star(linker_extend()),
        class(ud::INCB_CONSONANT),
    ])
}

/// GB11: an emoji ZWJ sequence — the codepoints joined by zero-width joiners
/// render as a single glyph (a family emoji, a profession, a flag variant).
fn pictographic_sequence() -> HirExpr {
    let pict = || class(ud::PROP_EXTENDED_PICTOGRAPHIC);
    let join = || {
        concat(vec![
            star(class(ud::GCB_EXTEND)),
            class(ud::GCB_ZWJ),
            pict(),
        ])
    };
    concat(vec![
        pict(),
        star(join()),
        // As above: without this, backtracking could end the cluster on one
        // pictograph of a joined sequence and start the next cluster inside it.
        not_followed_by(join()),
    ])
}

// ── expression helpers ──────────────────────────────────────────────────────

fn class(ranges: &'static [(u32, u32)]) -> HirExpr {
    HirExpr::UnicodeCpClass(CodepointClass::new(ranges.to_vec(), false))
}

/// The union of several property tables as one class. Building the union here
/// rather than as an alternation keeps the codepoint test to a single binary
/// search.
fn class_of(tables: &[&[(u32, u32)]]) -> HirExpr {
    HirExpr::UnicodeCpClass(CodepointClass::new(union(tables), false))
}

fn not_class_of(tables: &[&[(u32, u32)]]) -> HirExpr {
    HirExpr::UnicodeCpClass(CodepointClass::new(union(tables), true))
}

/// Merges tables into one sorted, non-overlapping range list, as
/// `CodepointClass`'s binary search requires.
fn union(tables: &[&[(u32, u32)]]) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = tables.iter().flat_map(|t| t.iter().copied()).collect();
    ranges.sort_unstable();

    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1.saturating_add(1) => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn not_followed_by(expr: HirExpr) -> HirExpr {
    HirExpr::Lookaround(Box::new(HirLookaround {
        expr,
        kind: HirLookaroundKind::NegativeLookahead,
    }))
}

fn concat(exprs: Vec<HirExpr>) -> HirExpr {
    HirExpr::Concat(exprs)
}

fn star(expr: HirExpr) -> HirExpr {
    repeat(expr, 0)
}

fn plus(expr: HirExpr) -> HirExpr {
    repeat(expr, 1)
}

fn repeat(expr: HirExpr, min: u32) -> HirExpr {
    HirExpr::Repeat(Box::new(HirRepeat {
        expr,
        min,
        max: None,
        greedy: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The union helper underpins every "one of these classes" branch; an
    /// unsorted or overlapping result would silently break the binary search
    /// `CodepointClass::contains` relies on.
    #[test]
    fn union_is_sorted_and_disjoint() {
        let merged = union(&[ud::GCB_EXTEND, ud::GCB_ZWJ, ud::GCB_SPACINGMARK]);
        for pair in merged.windows(2) {
            assert!(
                pair[0].1 < pair[1].0,
                "ranges {:?} and {:?} overlap or touch",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn union_covers_every_input_codepoint() {
        let tables: &[&[(u32, u32)]] = &[ud::GCB_EXTEND, ud::GCB_ZWJ];
        let merged = union(tables);
        for table in tables {
            for &(start, end) in *table {
                for cp in [start, end] {
                    assert!(
                        merged.iter().any(|&(s, e)| cp >= s && cp <= e),
                        "U+{cp:04X} lost from the union"
                    );
                }
            }
        }
    }
}
