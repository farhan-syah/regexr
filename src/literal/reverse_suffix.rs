//! Reverse-suffix search: recognising the shape, and walking a class run.
//!
//! A pattern like `\w+(?=ing\b)` is expensive to search forwards: every word
//! position is a plausible start, and the assertion is re-evaluated at every
//! candidate end. Searching it backwards costs a fraction — seek the
//! lookahead's literal with `memmem`, walk *back* over the preceding class run
//! to find where the match can start, then confirm with an anchored engine run.
//!
//! This module holds the applicability gate ([`reverse_suffix_plan`]), the run
//! scan ([`run_start`] / [`run_end`]) and the search that drives them
//! ([`ReverseSuffixSearch`]). The executor consults the search from
//! `find_from_inner`; the anchored confirm is the engine's own.

use memchr::memmem::Finder;

use crate::hir::{
    compute_capture_count, CodepointClass, Hir, HirClass, HirExpr, HirLookaroundKind, HirProps,
    HirRepeat,
};
use crate::literal::{byte_class_set, extract_literals};
use crate::nfa::{ByteClass, ByteRange};
use crate::vm::pike::shared::decode_utf8_codepoint;

/// The class a reverse-suffix run is made of.
///
/// One element per iteration of the repetition, which is one byte for a byte
/// class and one whole UTF-8 encoded code point for a code-point class. The
/// distinction is what the reverse walk steps by.
#[derive(Debug, Clone)]
pub(crate) enum RunClass {
    /// One byte per element.
    Bytes(ByteClass),
    /// One UTF-8 encoded code point per element.
    Codepoints(CodepointClass),
}

/// Everything the reverse-suffix search needs about a pattern it accepted.
#[derive(Debug, Clone)]
pub(crate) struct ReverseSuffixPlan {
    /// The lookahead's leading literal — the text to seek with `memmem`. Every
    /// match ends where an occurrence of this literal begins.
    pub(crate) literal: Vec<u8>,
    /// The class the repetition before the lookahead runs over.
    pub(crate) class: RunClass,
    /// Whether the repetition's minimum was zero (`*`) rather than one (`+`),
    /// i.e. whether an empty run at a literal occurrence is still a match.
    pub(crate) allows_empty_run: bool,
}

/// A [`ReverseSuffixPlan`] plus the `memmem` finder for its literal, ready to
/// search with.
///
/// Built once at compile time — the finder owns its needle, so it outlives the
/// HIR the plan was derived from.
#[derive(Debug)]
pub(crate) struct ReverseSuffixSearch {
    plan: ReverseSuffixPlan,
    finder: Finder<'static>,
}

impl ReverseSuffixSearch {
    /// The search for `hir`, or `None` when the gate refuses its shape.
    pub(crate) fn new(hir: &Hir) -> Option<Self> {
        let plan = reverse_suffix_plan(hir)?;
        let finder = Finder::new(&plan.literal).into_owned();
        Some(Self { plan, finder })
    }

    /// The leftmost match starting at or after `from`, found end-first.
    ///
    /// `confirm` is the engine's *anchored* match at a given start — it must
    /// answer about that position alone, never search forward from it. The end
    /// comes from `confirm`, never from the literal occurrence: on `"singing"`
    /// the gate's `\w+(?=ing\b)` sees `ing` at 1 and at 4, yet the leftmost
    /// match is `(0, 4)`, whose end the scan never proposed.
    ///
    /// Each iteration seeks the next literal occurrence `p` (every match ends at
    /// one, since the lookahead is zero-width and its inner begins with the
    /// literal), walks back over the class run to the leftmost start `s` a match
    /// ending there can have, and confirms at `s`. A failed confirm kills the
    /// whole run — every start inside it reaches only ends inside it, all of
    /// which the confirm at `s` already explored — so the search resumes at the
    /// run's end rather than at the next position, which is what keeps it
    /// linear.
    pub(crate) fn find(
        &self,
        input: &[u8],
        from: usize,
        mut confirm: impl FnMut(usize) -> Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        let mut pos = from;
        loop {
            let p = pos + self.finder.find(input.get(pos..)?)?;
            let mut start = run_start(input, p, &self.plan.class, from);
            // A match never begins inside a codepoint, and the engines refuse
            // such a start outright. The walk can still land on one — a byte
            // class whose members include continuation bytes has runs that begin
            // mid-character — and confirming there would fail for the wrong
            // reason, taking the whole run with it, since the skip below reads a
            // failed confirm as proof that no start in the run can match. The
            // first boundary at or after the walk's answer is the leftmost start
            // that is viable at all, and it is never past `p`, whose byte the
            // literal's own first byte matched.
            while start < p && !crate::nfa::is_utf8_boundary(input, start) {
                start += 1;
            }
            if start == p && !self.plan.allows_empty_run {
                // A `C+` run cannot end at `p`, so `p` is not a match end. `p`
                // itself may still be the start of a match ending later in the
                // run ahead, so only this occurrence is skipped.
                pos = p + 1;
                continue;
            }
            if let Some(found) = confirm(start) {
                return Some(found);
            }
            pos = (p + 1).max(run_end(input, p, &self.plan.class));
        }
    }
}

/// Recognises the one pattern shape a reverse-suffix search is sound for:
///
/// ```text
/// Concat([ Repeat { <one class>, min: 0|1, max: None, greedy }, (?= inner ) ])
/// ```
///
/// where `inner` begins with a literal of at least two bytes.
///
/// # Why the gate is this narrow
///
/// The search visits literal occurrences left to right and stops at the first
/// one whose reverse walk yields a viable start. That is leftmost-first only if
/// `min S(p)` — the leftmost start of a match ending at `p` — is non-decreasing
/// in `p`. In general it is not. `(?:abcde|c)(?=d)` on `"abcded"` has `d` at 3
/// and at 5, with `S(3) = {2}` (via the `c` branch) and `S(5) = {0}` (via the
/// `abcde` branch): the first occurrence answers `(2,3)` where the leftmost
/// match is `(0,5)`.
///
/// Monotonicity holds here *only* because the runs of a single class are
/// disjoint and ordered, so a later end point can never reach back past an
/// earlier one's start. Every construct that could break that is refused: an
/// alternation, a concatenation of two or more consuming elements, a bounded
/// repeat (whose start is capped independently of the run), a non-greedy one
/// (which prefers a different start), a capture in the run (whose recorded span
/// the walk cannot reproduce), a backreference, or a second lookaround.
pub(crate) fn reverse_suffix_plan(hir: &Hir) -> Option<ReverseSuffixPlan> {
    // A backreference needs the backtracker, and a non-greedy quantifier picks
    // a start the reverse walk does not compute. Both are pattern-wide
    // properties, so they are checked before the shape.
    if hir.props.has_backrefs || hir.props.has_non_greedy {
        return None;
    }

    let HirExpr::Concat(parts) = &hir.expr else {
        return None;
    };
    // Exactly two elements: a run and the lookahead, nothing else consuming.
    let [head, tail] = parts.as_slice() else {
        return None;
    };

    let HirExpr::Repeat(repeat) = head else {
        return None;
    };
    let class = run_class(repeat)?;

    let HirExpr::Lookaround(look) = tail else {
        return None;
    };
    if !matches!(look.kind, HirLookaroundKind::PositiveLookahead) {
        return None;
    }
    // Exactly one lookaround in the whole pattern: a second one (including one
    // the translator synthesised, as `\z` does) is another assertion the
    // reverse walk knows nothing about.
    if count_lookarounds(&hir.expr) != 1 {
        return None;
    }

    let literal = leading_literal(&look.expr)?;
    // A one-byte literal is not selective enough to pay for the walk, and the
    // shorter the literal the more often the confirming run is wasted.
    if literal.len() < 2 {
        return None;
    }

    Some(ReverseSuffixPlan {
        literal,
        class,
        allows_empty_run: repeat.min == 0,
    })
}

/// The class of an accepted repetition, or `None` for any other repetition.
///
/// Greedy, unbounded, at most one mandatory iteration, no capture inside. A
/// capture would have to be filled with the span the walk never enumerates.
fn run_class(repeat: &HirRepeat) -> Option<RunClass> {
    if !repeat.greedy || repeat.max.is_some() || repeat.min > 1 {
        return None;
    }
    if compute_capture_count(&repeat.expr) > 0 {
        return None;
    }
    match &repeat.expr {
        HirExpr::Class(class) => Some(RunClass::Bytes(byte_class(class))),
        HirExpr::UnicodeCpClass(cpclass) => Some(RunClass::Codepoints(cpclass.clone())),
        _ => None,
    }
}

/// The membership table of an HIR byte class, as a [`ByteClass`].
///
/// Negation is resolved by [`byte_class_set`], the same helper the Shift-Or and
/// backtracking emitters use, so this agrees with the engines byte for byte
/// rather than re-deriving the complement.
fn byte_class(class: &HirClass) -> ByteClass {
    let members = byte_class_set(&class.ranges, class.negated);
    let is_member = |byte: usize| members.get(byte).is_some_and(|&flag| flag != 0);

    let mut ranges = Vec::new();
    let mut byte = 0usize;
    while byte < 256 {
        if !is_member(byte) {
            byte += 1;
            continue;
        }
        let start = byte as u8;
        while byte < 256 && is_member(byte) {
            byte += 1;
        }
        // `byte` advanced at least once since `start`, so `byte - 1` is that
        // range's last member and is still below 256.
        ranges.push(ByteRange::new(start, (byte - 1) as u8));
    }
    ByteClass::new(ranges)
}

/// The literal every match of `inner` must *begin* with, when there is exactly
/// one such literal.
///
/// Deliberately not [`crate::literal::required_literal`], which is a rejection
/// filter and reports a literal from anywhere in the pattern — `\w+\s+error\s+\w+`
/// yields `"error"`, text that sits in the middle of a match and would anchor
/// the search at the wrong place entirely.
///
/// The guarantee relied on instead is `LiteralExtractor`'s own: its `Concat` arm
/// extracts from the first element that is not zero-width, and every arm that
/// cannot produce a literal (a class, a `min == 0` repetition) returns an
/// *incomplete* result, which stops the extension loop before any later literal
/// is spliced on. So a non-empty `prefixes` is always text at offset zero of
/// `inner` — modulo zero-width elements, which consume nothing and so do not
/// move the offset. `prefix_offset` is the one exception (a leading positive
/// lookbehind's literal sits to the *left* of the match) and is refused here.
///
/// Case-insensitive patterns exclude themselves: under `(?i)` the translator
/// emits a folded *class* per letter rather than `HirExpr::Literal` (see
/// `HirTranslator::translate_literal`), so `(?i)\w+(?=ing)` has no literal to
/// extract at all. Only characters without case variants stay literals, and for
/// those the fold is a no-op, so the literal search remains exact.
fn leading_literal(inner: &HirExpr) -> Option<Vec<u8>> {
    // `extract_literals` takes a whole `Hir`; the props it reads (`has_backrefs`
    // and friends) only ever demote `prefix_complete`, which is not consulted
    // here, so a default-props probe answers the same question for the
    // subexpression.
    let probe = Hir {
        expr: inner.clone(),
        props: HirProps::default(),
    };
    let literals = extract_literals(&probe);
    if literals.prefix_offset != 0 {
        return None;
    }
    literals.single_prefix().map(|prefix| prefix.to_vec())
}

/// How many lookarounds `expr` contains, counting nested ones.
fn count_lookarounds(expr: &HirExpr) -> usize {
    match expr {
        HirExpr::Empty
        | HirExpr::Literal(_)
        | HirExpr::Class(_)
        | HirExpr::UnicodeCpClass(_)
        | HirExpr::Anchor(_)
        | HirExpr::Backref(_) => 0,
        HirExpr::Concat(exprs) | HirExpr::Alt(exprs) => exprs.iter().map(count_lookarounds).sum(),
        HirExpr::Repeat(rep) => count_lookarounds(&rep.expr),
        HirExpr::Capture(cap) => count_lookarounds(&cap.expr),
        HirExpr::Lookaround(look) => 1 + count_lookarounds(&look.expr),
    }
}

/// The smallest `s >= floor` such that every element of `input[s..p]` is in
/// `class`.
///
/// This is the reverse half of the search: `p` is where a literal occurrence
/// begins, and `s` is the leftmost start a match ending there can have.
///
/// The `floor` clamp is what keeps iteration correct. A run may extend to the
/// left of the caller's resume point, and walking back past it would report a
/// start inside text an earlier match already consumed. Positions outside
/// `input` are clamped rather than trusted, since both offsets come from a
/// caller that may have derived them from an earlier match.
pub(crate) fn run_start(input: &[u8], p: usize, class: &RunClass, floor: usize) -> usize {
    let mut start = p.min(input.len());
    let floor = floor.min(start);

    match class {
        RunClass::Bytes(bytes) => {
            while start > floor {
                let Some(&byte) = input.get(start - 1) else {
                    break;
                };
                if !bytes.contains(byte) {
                    break;
                }
                start -= 1;
            }
        }
        RunClass::Codepoints(cpclass) => {
            while start > floor {
                let boundary = prev_boundary(input, start);
                if boundary < floor {
                    // The run reaches past the resume point mid-character.
                    break;
                }
                let Some(bytes) = input.get(boundary..start) else {
                    break;
                };
                // A decode that does not fill `bytes` exactly means `boundary`
                // was not a character start (malformed input), so the walk
                // stops rather than stepping into the middle of a sequence.
                match decode_utf8_codepoint(bytes) {
                    Some((cp, len)) if len == start - boundary && cpclass.contains(cp) => {
                        start = boundary;
                    }
                    _ => break,
                }
            }
        }
    }

    start
}

/// The end of the maximal `class` run containing position `p`, i.e. the largest
/// `e` such that every element of `input[p..e]` is in `class`.
///
/// `p` past the end of the input is clamped to it, and answers `input.len()`.
pub(crate) fn run_end(input: &[u8], p: usize, class: &RunClass) -> usize {
    let mut end = p.min(input.len());

    match class {
        RunClass::Bytes(bytes) => {
            while input.get(end).is_some_and(|&byte| bytes.contains(byte)) {
                end += 1;
            }
        }
        RunClass::Codepoints(cpclass) => {
            while let Some((cp, len)) = input.get(end..).and_then(decode_utf8_codepoint) {
                if len == 0 || !cpclass.contains(cp) {
                    break;
                }
                end += len;
            }
        }
    }

    end
}

/// The start of the UTF-8 sequence that ends at `end`.
///
/// UTF-8 is self-synchronising — a continuation byte is `10xxxxxx` and a lead
/// byte never is — so the previous boundary is found by stepping back over
/// continuation bytes, in O(1) for the at most four bytes a character occupies.
///
/// `TaggedNfa` has the same walk as a private associated function
/// (`nfa::tagged::interpreter::tagged_nfa`). It is duplicated rather than
/// shared because publishing it would widen that engine's surface for four
/// lines, and because this copy has to answer for arbitrary caller-supplied
/// offsets over possibly malformed input: it never indexes unchecked, and it
/// stops after four bytes so a long run of continuation bytes cannot make the
/// walk linear. The caller rejects whatever it lands on if that is not a real
/// character start.
fn prev_boundary(input: &[u8], end: usize) -> usize {
    let mut i = end.saturating_sub(1);
    while i > 0 && end - i < 4 && input.get(i).is_some_and(|&byte| (byte & 0xC0) == 0x80) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(pattern: &str) -> Option<ReverseSuffixPlan> {
        let ast = crate::parser::parse(pattern).unwrap();
        let hir = crate::hir::translate(&ast).unwrap();
        reverse_suffix_plan(&hir)
    }

    fn search(pattern: &str) -> ReverseSuffixSearch {
        let ast = crate::parser::parse(pattern).unwrap();
        let hir = crate::hir::translate(&ast).unwrap();
        ReverseSuffixSearch::new(&hir).expect("gate accepts this shape")
    }

    /// Records every start the search asks about, and answers `matches`.
    fn confirms(
        search: &ReverseSuffixSearch,
        input: &[u8],
        from: usize,
        matches: Option<(usize, usize)>,
    ) -> (Option<(usize, usize)>, Vec<usize>) {
        let mut asked = Vec::new();
        let found = search.find(input, from, |start| {
            asked.push(start);
            matches.filter(|&(match_start, _)| match_start == start)
        });
        (found, asked)
    }

    /// The search's two load-bearing properties, neither visible in the answer
    /// alone: it confirms *one* start per class run (the dead-run skip, which is
    /// what makes it linear), and the span it reports is the confirm's, not the
    /// literal occurrence it walked back from.
    #[test]
    fn test_search_confirms_one_start_per_run() {
        let ing = search(r"\w+(?=ing\b)");

        // "ing" occurs at 1 and at 4, both inside one `\w` run. The confirm at
        // that run's start answers (0, 4) — an end the literal scan proposed
        // only as a candidate, and a run the second occurrence must not re-ask.
        let (found, asked) = confirms(&ing, b"singing", 0, Some((0, 4)));
        assert_eq!(found, Some((0, 4)));
        assert_eq!(asked, vec![0]);

        // The same haystack with nothing to find: still one confirm.
        let (found, asked) = confirms(&ing, b"singing", 0, None);
        assert_eq!(found, None);
        assert_eq!(asked, vec![0]);

        // Two runs, two confirms — one per run, in order.
        let (found, asked) = confirms(&ing, b"singing ringing", 0, Some((8, 12)));
        assert_eq!(found, Some((8, 12)));
        assert_eq!(asked, vec![0, 8]);
    }

    /// An occurrence with no run before it is not a match end for a `+` run, so
    /// it is never confirmed — but it is skipped by one position rather than by
    /// the run ahead of it, which may still contain a match that starts there.
    #[test]
    fn test_search_skips_an_empty_run_only_when_the_repeat_needs_one() {
        let ing = search(r"\w+(?=ing\b)");
        assert_eq!(confirms(&ing, b"ings", 0, None), (None, vec![]));
        assert_eq!(confirms(&ing, b" ing", 0, None), (None, vec![]));

        // `[a-z]+(?=xy)`: the first `xy` is not a match end, yet position 1 is
        // where the match ending at the second one begins.
        let xy = search(r"[a-z]+(?=xy)");
        assert_eq!(
            confirms(&xy, b" xyxy", 0, Some((1, 3))),
            (Some((1, 3)), vec![1])
        );

        // A `*` run may be empty, so the same occurrence is confirmed.
        let star = search(r"\w*(?=ing)");
        assert_eq!(
            confirms(&star, b"ings", 0, Some((0, 0))),
            (Some((0, 0)), vec![0])
        );
    }

    /// The resume point is a floor on the reverse walk: iteration must never be
    /// handed a start inside the text an earlier match already covered.
    #[test]
    fn test_search_never_confirms_a_start_before_the_resume_point() {
        let ing = search(r"\w+(?=ing\b)");
        // Resuming at 4 leaves no run before the occurrence there at all.
        assert_eq!(confirms(&ing, b"singing", 4, None), (None, vec![]));
        // Resuming mid-run clamps the start to the resume point.
        assert_eq!(
            confirms(&ing, b"singing", 2, Some((2, 4))),
            (Some((2, 4)), vec![2])
        );
    }

    fn bytes_class(ranges: &[(u8, u8)]) -> RunClass {
        RunClass::Bytes(byte_class(&HirClass::new(ranges.to_vec(), false)))
    }

    fn codepoint_class(ranges: &[(u32, u32)]) -> RunClass {
        RunClass::Codepoints(CodepointClass::new(ranges.to_vec(), false))
    }

    /// The shapes the search is for.
    #[test]
    fn test_plan_accepts_a_class_run_before_a_literal_lookahead() {
        let accepted = plan(r"\w+(?=ing\b)").expect("class run before a literal lookahead");
        assert_eq!(accepted.literal, b"ing".to_vec());
        assert!(!accepted.allows_empty_run);
        let RunClass::Bytes(class) = &accepted.class else {
            panic!("ASCII \\w is a byte class");
        };
        assert!(class.contains(b'a'));
        assert!(class.contains(b'_'));
        assert!(!class.contains(b' '));

        // `*` differs only in whether an empty run still matches.
        let accepted = plan(r"\w*(?=ing)").expect("a zero-or-more run is still one run");
        assert!(accepted.allows_empty_run);

        let accepted = plan(r"[a-z]+(?=xy)").expect("an explicit class is the same shape");
        assert_eq!(accepted.literal, b"xy".to_vec());
    }

    /// A Unicode `\w` lowers to a code-point class, whose run is walked one
    /// character at a time rather than one byte.
    ///
    /// Spelled as a scoped `(?u:…)` group because a bare `(?u)` directive parses
    /// into an `Empty` beside a flagged group wrapping the rest, which is no
    /// longer the two-element concatenation the gate accepts.
    #[test]
    fn test_plan_accepts_a_codepoint_class_run() {
        let accepted = plan(r"(?u:\w+(?=ing))").expect("a codepoint run is still one run");
        assert!(matches!(accepted.class, RunClass::Codepoints(_)));
        assert_eq!(accepted.literal, b"ing".to_vec());
    }

    /// Everything the gate must refuse. Each of these breaks the monotonicity
    /// the left-to-right literal scan rests on, or leaves the walk unable to
    /// reproduce what the engine would have matched — and none of it is visible
    /// as a *slower* search, only as a wrong one.
    #[test]
    fn test_plan_refuses_everything_that_is_not_one_class_run() {
        // The counterexample itself: an alternation makes the leftmost start of
        // a match ending at `p` jump backwards as `p` advances. On "abcded" the
        // first `d` answers (2,3) where the leftmost match is (0,5).
        assert!(plan(r"(?:abcde|c)(?=d)").is_none());
        // Several literals to seek, so "the first occurrence" is not defined by
        // a single memmem scan.
        assert!(plan(r"\w+(?=ing|ed)").is_none());
        // The literal is not the lookahead's leading text, so an occurrence of
        // it does not mark a match end.
        assert!(plan(r"\w+(?=\w*ing)").is_none());
        // A literal in the middle of a concatenation - what `required_literal`
        // would have handed back - anchors nothing.
        assert!(plan(r"\w+\s+error\s+\w+").is_none());
        // Non-greedy prefers a shorter run than the walk finds.
        assert!(plan(r"\w+?(?=ing)").is_none());
        // A bounded repeat caps the start independently of the run.
        assert!(plan(r"\w{2,5}(?=ing)").is_none());
        // A capture inside the run has a span the walk never enumerates.
        assert!(plan(r"(\w)+(?=ing)").is_none());
        // Case folding leaves classes, not literals, so there is nothing to
        // seek. The scoped spelling is the one that reaches the literal check —
        // a bare `(?i)` directive already fails the shape check, since it parses
        // into an `Empty` beside a flagged group wrapping the rest.
        assert!(plan(r"(?i:\w+(?=ing))").is_none());
        assert!(plan(r"(?i)\w+(?=ing)").is_none());
        // A backreference needs the backtracker. The shape is otherwise exactly
        // right - the capture is inside the lookahead, and "ing" is extracted -
        // so only the property check refuses it.
        assert!(plan(r"\w+(?=(i)ng\1)").is_none());
        // A single-byte literal is below the length floor.
        assert!(plan(r"\w+(?=x)").is_none());
        // A negative lookahead marks text that must be ABSENT.
        assert!(plan(r"\w+(?!ing)").is_none());
        // A second assertion the walk knows nothing about.
        assert!(plan(r"\w+(?=ing(?=s))").is_none());
        // No run at all, and no lookahead at all.
        assert!(plan(r"abc(?=ing)").is_none());
        assert!(plan(r"\w+ing").is_none());
    }

    #[test]
    fn test_run_scan_over_a_byte_class() {
        let class = bytes_class(&[(b'a', b'z')]);
        let input = b"foo bar";

        // A run bounded by a non-member on both sides.
        assert_eq!(run_start(input, 3, &class, 0), 0);
        assert_eq!(run_end(input, 0, &class), 3);
        // `p` at the very end of the input.
        assert_eq!(run_start(input, input.len(), &class, 0), 4);
        assert_eq!(run_end(input, input.len(), &class), input.len());
        // `p` at zero, and `p` whose preceding byte is not a member: both are
        // empty runs, and the answer is `p` itself.
        assert_eq!(run_start(input, 0, &class, 0), 0);
        assert_eq!(run_start(input, 4, &class, 0), 4);
        assert_eq!(run_end(input, 3, &class), 3);

        // A run spanning the whole input.
        let whole = b"foobar";
        assert_eq!(run_start(whole, whole.len(), &class, 0), 0);
        assert_eq!(run_end(whole, 0, &class), whole.len());

        // Offsets past the end are clamped, not trusted.
        assert_eq!(run_start(whole, 99, &class, 0), 0);
        assert_eq!(run_end(whole, 99, &class), whole.len());
    }

    /// The clamp that makes iteration resume correct: a run must never be
    /// walked back past the caller's resume point, even when the class matches
    /// all the way.
    #[test]
    fn test_run_start_never_walks_past_the_floor() {
        let class = bytes_class(&[(b'a', b'z')]);
        let whole = b"foobar";
        assert_eq!(run_start(whole, whole.len(), &class, 3), 3);
        // A floor at or past `p` leaves nothing to walk.
        assert_eq!(run_start(whole, 3, &class, 3), 3);
        assert_eq!(run_start(whole, 3, &class, 99), 3);

        // The same clamp on a codepoint run, including a floor that falls in
        // the middle of a character: the walk stops at the character after it
        // rather than stepping into the sequence.
        let letters = codepoint_class(&[(0x61, 0x7A), (0xE9, 0xE9), (0x4E16, 0x4E16)]);
        let input = "aé世".as_bytes(); // a = [0], é = [1,3), 世 = [3,6)
        assert_eq!(input.len(), 6);
        assert_eq!(run_start(input, 6, &letters, 0), 0);
        assert_eq!(run_start(input, 6, &letters, 3), 3);
        assert_eq!(run_start(input, 6, &letters, 2), 3);
    }

    #[test]
    fn test_run_scan_over_a_codepoint_class() {
        let letters = codepoint_class(&[(0x61, 0x7A), (0xE9, 0xE9), (0x4E16, 0x4E16)]);
        // a = [0], é = [1,3), 世 = [3,6), ' ' = [6], b = [7]
        let input = "aé世 b".as_bytes();
        assert_eq!(input.len(), 8);

        // Multi-byte members are stepped one character at a time.
        assert_eq!(run_start(input, 6, &letters, 0), 0);
        assert_eq!(run_end(input, 0, &letters), 6);
        // `p` at the end of the input, and at zero.
        assert_eq!(run_start(input, input.len(), &letters, 0), 7);
        assert_eq!(run_end(input, input.len(), &letters), input.len());
        assert_eq!(run_start(input, 0, &letters, 0), 0);
        // The character before `p` is not a member: an empty run.
        assert_eq!(run_start(input, 7, &letters, 0), 7);
        assert_eq!(run_end(input, 6, &letters), 6);

        // A run spanning the whole input.
        let whole = "aé世".as_bytes();
        assert_eq!(run_start(whole, whole.len(), &letters, 0), 0);
        assert_eq!(run_end(whole, 0, &letters), whole.len());

        // A character outside the class stops the walk even though its bytes
        // would pass a byte-wise test.
        // a = [0], 漢 = [1,4), é = [4,6), b = [6]
        let other = "a漢éb".as_bytes();
        assert_eq!(other.len(), 7);
        assert_eq!(run_start(other, other.len(), &letters, 0), 4);
        assert_eq!(run_end(other, 4, &letters), other.len());
        assert_eq!(run_end(other, 0, &letters), 1);

        // Offsets past the end are clamped, not trusted.
        assert_eq!(run_start(whole, 99, &letters, 0), 0);
        assert_eq!(run_end(whole, 99, &letters), whole.len());
    }
}
