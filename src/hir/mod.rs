//! High-Level Intermediate Representation (HIR) module.
//!
//! Translates the AST into a byte-oriented representation suitable for NFA construction.

pub mod builder;
mod grapheme;
mod prefix_opt;
pub mod unicode;
pub mod unicode_data;

pub use builder::*;
pub use prefix_opt::optimize_prefixes;

use crate::error::Result;
use crate::parser::Ast;

/// High-level IR for a regex pattern.
#[derive(Debug, Clone)]
pub struct Hir {
    /// The root expression.
    pub expr: HirExpr,
    /// Properties of the pattern.
    pub props: HirProps,
}

/// Properties derived from analyzing the HIR.
#[derive(Debug, Clone, Default)]
pub struct HirProps {
    /// Whether the pattern contains backreferences.
    pub has_backrefs: bool,
    /// Whether the pattern contains lookarounds.
    pub has_lookaround: bool,
    /// Whether the pattern contains positional anchors (^, $).
    /// These require matching at specific input positions.
    pub has_anchors: bool,
    /// Whether the pattern has a start anchor (^).
    pub has_start_anchor: bool,
    /// Whether the pattern has an end anchor ($).
    pub has_end_anchor: bool,
    /// Whether the pattern has multiline anchors ((?m)^ or (?m)$).
    pub has_multiline_anchors: bool,
    /// Whether the pattern contains word boundaries (\b, \B).
    /// These can be handled by DFA with position tracking.
    pub has_word_boundary: bool,
    /// Whether the pattern contains non-greedy quantifiers (*?, +?, ??, {n,m}?).
    pub has_non_greedy: bool,
    /// Whether the pattern contains bounded repeats with explicit min/max ({n}, {n,m}).
    /// ShiftOr cannot handle these correctly - they need unrolling or a counting engine.
    pub has_bounded_repeat: bool,
    /// Whether the pattern contains large Unicode character classes.
    /// These cause DFA state explosion and should use PikeVM instead of JIT.
    pub has_large_unicode_class: bool,
    /// Number of capture groups.
    pub capture_count: u32,
    /// Minimum match length in bytes.
    pub min_len: usize,
    /// Maximum match length in bytes (None = unbounded).
    pub max_len: Option<usize>,
    /// Named capture groups: maps name to index.
    pub named_groups: std::collections::HashMap<String, u32>,
    /// If the pattern is a single codepoint class, store the ranges here
    /// for fast codepoint-level matching. This avoids byte-level expansion.
    pub codepoint_class: Option<CodepointClass>,
}

/// A codepoint-level character class (Unicode scalar values).
/// Used for fast matching of patterns like `[α-ω]` or `\p{Greek}`.
#[derive(Debug, Clone)]
pub struct CodepointClass {
    /// Codepoint ranges (sorted, non-overlapping). Each range is (start, end) inclusive.
    pub ranges: Vec<(u32, u32)>,
    /// Whether this class is negated.
    pub negated: bool,
    /// Precomputed ASCII bitmap for fast lookup of codepoints 0-127.
    /// `ascii_bitmap[0]` covers bits 0-63, `ascii_bitmap[1]` covers bits 64-127.
    /// A set bit means the codepoint is IN the class (before negation is applied).
    pub ascii_bitmap: [u64; 2],
}

impl CodepointClass {
    /// Creates a new codepoint class with precomputed ASCII bitmap.
    pub fn new(ranges: Vec<(u32, u32)>, negated: bool) -> Self {
        let ascii_bitmap = Self::compute_ascii_bitmap(&ranges);
        Self {
            ranges,
            negated,
            ascii_bitmap,
        }
    }

    /// Computes the ASCII bitmap from ranges.
    /// Sets bit i if codepoint i is in any of the ranges (for i in 0..128).
    fn compute_ascii_bitmap(ranges: &[(u32, u32)]) -> [u64; 2] {
        let mut bitmap = [0u64; 2];
        for &(start, end) in ranges {
            // Only process ranges that overlap with ASCII (0-127)
            if start > 127 {
                continue;
            }
            let range_start = start as usize;
            let range_end = (end.min(127)) as usize;

            for cp in range_start..=range_end {
                if cp < 64 {
                    bitmap[0] |= 1u64 << cp;
                } else {
                    bitmap[1] |= 1u64 << (cp - 64);
                }
            }
        }
        bitmap
    }

    /// Checks if a codepoint is in the ranges (ignoring negation flag).
    /// Uses fast bitmap lookup for ASCII (< 128), binary search for others.
    /// This is useful when negation is handled separately by the caller.
    #[inline]
    pub fn contains_raw(&self, cp: u32) -> bool {
        // Fast path for ASCII codepoints using precomputed bitmap
        if cp < 128 {
            return if cp < 64 {
                (self.ascii_bitmap[0] & (1u64 << cp)) != 0
            } else {
                (self.ascii_bitmap[1] & (1u64 << (cp - 64))) != 0
            };
        }

        // Slow path for non-ASCII: binary search over ranges
        self.ranges
            .binary_search_by(|&(start, end)| {
                if cp < start {
                    std::cmp::Ordering::Greater
                } else if cp > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok()
    }

    /// Checks if a codepoint is in this class.
    /// Uses fast bitmap lookup for ASCII (< 128), binary search for others.
    #[inline]
    pub fn contains(&self, cp: u32) -> bool {
        // Fast path for ASCII codepoints using precomputed bitmap
        if cp < 128 {
            let in_bitmap = if cp < 64 {
                (self.ascii_bitmap[0] & (1u64 << cp)) != 0
            } else {
                (self.ascii_bitmap[1] & (1u64 << (cp - 64))) != 0
            };
            return if self.negated { !in_bitmap } else { in_bitmap };
        }

        // Slow path for non-ASCII: binary search over ranges
        let in_ranges = self
            .ranges
            .binary_search_by(|&(start, end)| {
                if cp < start {
                    std::cmp::Ordering::Greater
                } else if cp > end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .is_ok();

        if self.negated {
            !in_ranges
        } else {
            in_ranges
        }
    }
}

/// An HIR expression node.
#[derive(Debug, Clone)]
pub enum HirExpr {
    /// Empty expression.
    Empty,
    /// A literal byte sequence.
    Literal(Vec<u8>),
    /// A byte class (set of byte ranges).
    Class(HirClass),
    /// A Unicode codepoint class (used for efficient matching of Unicode patterns).
    /// This matches a single UTF-8 encoded codepoint and checks membership.
    UnicodeCpClass(CodepointClass),
    /// Concatenation.
    Concat(Vec<HirExpr>),
    /// Alternation.
    Alt(Vec<HirExpr>),
    /// Repetition.
    Repeat(Box<HirRepeat>),
    /// Capture group.
    Capture(Box<HirCapture>),
    /// Anchor.
    Anchor(HirAnchor),
    /// Lookaround.
    Lookaround(Box<HirLookaround>),
    /// Backreference.
    Backref(u32),
}

/// A byte class - set of byte ranges.
#[derive(Debug, Clone)]
pub struct HirClass {
    /// Byte ranges (sorted, non-overlapping).
    pub ranges: Vec<(u8, u8)>,
    /// Whether this class is negated.
    pub negated: bool,
}

impl HirClass {
    /// Creates a new class.
    pub fn new(ranges: Vec<(u8, u8)>, negated: bool) -> Self {
        Self { ranges, negated }
    }

    /// Creates a class matching any byte.
    ///
    /// This is a raw byte class, so it is only meaningful for a single step of
    /// a UTF-8 sequence or inside a zero-width assertion. `.` is not built from
    /// it — see `build_dot_expr`, which spells `.` as an alternation covering a
    /// whole code point.
    pub fn any() -> Self {
        Self {
            ranges: vec![(0, 255)],
            negated: false,
        }
    }
}

/// A repetition in HIR.
#[derive(Debug, Clone)]
pub struct HirRepeat {
    /// The expression being repeated.
    pub expr: HirExpr,
    /// Minimum repetitions.
    pub min: u32,
    /// Maximum repetitions.
    pub max: Option<u32>,
    /// Whether greedy.
    pub greedy: bool,
}

/// A capture group in HIR.
#[derive(Debug, Clone)]
pub struct HirCapture {
    /// Capture group index.
    pub index: u32,
    /// Optional name.
    pub name: Option<String>,
    /// The captured expression.
    pub expr: HirExpr,
}

/// An anchor in HIR.
#[derive(Debug, Clone, Copy)]
pub enum HirAnchor {
    /// Start of text.
    Start,
    /// End of text.
    End,
    /// Start of line.
    StartLine,
    /// End of line.
    EndLine,
    /// Word boundary.
    WordBoundary,
    /// Not word boundary.
    NotWordBoundary,
}

/// A lookaround in HIR.
#[derive(Debug, Clone)]
pub struct HirLookaround {
    /// The lookaround expression.
    pub expr: HirExpr,
    /// The kind of lookaround.
    pub kind: HirLookaroundKind,
}

/// The kind of lookaround.
#[derive(Debug, Clone, Copy)]
pub enum HirLookaroundKind {
    /// Positive lookahead.
    PositiveLookahead,
    /// Negative lookahead.
    NegativeLookahead,
    /// Positive lookbehind.
    PositiveLookbehind,
    /// Negative lookbehind.
    NegativeLookbehind,
}

/// Translates an AST to HIR, under the default expansion limit.
pub fn translate(ast: &Ast) -> Result<Hir> {
    translate_with_limit(ast, builder::DEFAULT_EXPANDED_SIZE)
}

/// [`translate`] under a caller-chosen ceiling on the expanded pattern.
///
/// See [`crate::RegexBuilder::size_limit`] for what the number means and when
/// raising it is the right call.
pub fn translate_with_limit(ast: &Ast, limit: u32) -> Result<Hir> {
    let mut translator = HirTranslator::new();
    translator.translate_with_limit(ast, limit)
}

/// Whether an expression can match the empty string.
///
/// Zero-width constructs (anchors, lookaround, and a backreference to a group
/// that captured nothing) consume no input and therefore match empty; a class or
/// a non-empty literal never does. Repetition matches empty when it may run zero
/// times or when its body can.
///
/// This is what decides whether a repetition needs a progress guard: a loop over
/// a body that matches empty can re-enter at a position it never advances past.
pub fn matches_empty(expr: &HirExpr) -> bool {
    match expr {
        HirExpr::Empty | HirExpr::Anchor(_) | HirExpr::Lookaround(_) | HirExpr::Backref(_) => true,
        HirExpr::Literal(bytes) => bytes.is_empty(),
        HirExpr::Class(_) | HirExpr::UnicodeCpClass(_) => false,
        HirExpr::Concat(exprs) => exprs.iter().all(matches_empty),
        HirExpr::Alt(branches) => branches.iter().any(matches_empty),
        HirExpr::Repeat(r) => r.min == 0 || matches_empty(&r.expr),
        HirExpr::Capture(c) => matches_empty(&c.expr),
    }
}

/// Whether the expression contains an unbounded repetition over a body that can
/// match the empty string (`(a*)*`, `()+`, `(a?)*`, …).
///
/// Such a loop needs a progress guard: without one it re-enters its body at a
/// position it can never advance past. Bounded repeats are excluded because they
/// unroll into a finite number of copies and cannot loop.
pub fn has_unbounded_nullable_repeat(expr: &HirExpr) -> bool {
    match expr {
        HirExpr::Empty
        | HirExpr::Literal(_)
        | HirExpr::Class(_)
        | HirExpr::UnicodeCpClass(_)
        | HirExpr::Anchor(_)
        | HirExpr::Backref(_) => false,
        HirExpr::Concat(exprs) | HirExpr::Alt(exprs) => {
            exprs.iter().any(has_unbounded_nullable_repeat)
        }
        HirExpr::Repeat(r) => {
            (r.max.is_none() && matches_empty(&r.expr)) || has_unbounded_nullable_repeat(&r.expr)
        }
        HirExpr::Capture(c) => has_unbounded_nullable_repeat(&c.expr),
        HirExpr::Lookaround(l) => has_unbounded_nullable_repeat(&l.expr),
    }
}

/// Computes the maximum capture group index from an HIR expression.
/// Returns 0 if there are no capture groups.
pub fn compute_capture_count(expr: &HirExpr) -> u32 {
    match expr {
        HirExpr::Empty
        | HirExpr::Literal(_)
        | HirExpr::Class(_)
        | HirExpr::UnicodeCpClass(_)
        | HirExpr::Anchor(_)
        | HirExpr::Backref(_) => 0,
        HirExpr::Concat(exprs) | HirExpr::Alt(exprs) => {
            exprs.iter().map(compute_capture_count).max().unwrap_or(0)
        }
        HirExpr::Repeat(rep) => compute_capture_count(&rep.expr),
        HirExpr::Capture(cap) => cap.index.max(compute_capture_count(&cap.expr)),
        HirExpr::Lookaround(la) => compute_capture_count(&la.expr),
    }
}
