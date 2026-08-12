//! Pattern step extraction from NFA.
//!
//! Extracts pattern steps from NFA for fast step-based matching.

use super::shared::PatternStep;
use crate::nfa::{ByteClass, ByteRange, Nfa, NfaInstruction, NfaState, StateId};

/// Combines greedy quantifiers followed by lookahead into combined variants.
/// This is needed for both JIT and interpreter to handle backtracking correctly.
pub fn combine_greedy_with_lookahead(steps: Vec<PatternStep>) -> Vec<PatternStep> {
    let mut result = Vec::with_capacity(steps.len());
    let mut i = 0;

    while i < steps.len() {
        match &steps[i] {
            PatternStep::GreedyPlus(ranges) if i + 1 < steps.len() => match &steps[i + 1] {
                PatternStep::PositiveLookahead(inner) => {
                    result.push(PatternStep::GreedyPlusLookahead(
                        ranges.clone(),
                        inner.clone(),
                        true,
                    ));
                    i += 2;
                    continue;
                }
                PatternStep::NegativeLookahead(inner) => {
                    result.push(PatternStep::GreedyPlusLookahead(
                        ranges.clone(),
                        inner.clone(),
                        false,
                    ));
                    i += 2;
                    continue;
                }
                _ => {}
            },
            PatternStep::GreedyStar(ranges) if i + 1 < steps.len() => match &steps[i + 1] {
                PatternStep::PositiveLookahead(inner) => {
                    result.push(PatternStep::GreedyStarLookahead(
                        ranges.clone(),
                        inner.clone(),
                        true,
                    ));
                    i += 2;
                    continue;
                }
                PatternStep::NegativeLookahead(inner) => {
                    result.push(PatternStep::GreedyStarLookahead(
                        ranges.clone(),
                        inner.clone(),
                        false,
                    ));
                    i += 2;
                    continue;
                }
                _ => {}
            },
            _ => {}
        }
        result.push(steps[i].clone());
        i += 1;
    }

    result
}

/// Per-kind tally of zero-width assertions. Comparing tallies kind-by-kind (not
/// just a single total) is essential: a quantifier `Alt` can simultaneously
/// DUPLICATE one assertion and DROP another (`a*(?=\S)$` keeps the lookahead in
/// both branches but loses the `$`), and the two errors would cancel in a single
/// total (2 == 1+1) yet differ per kind (lookahead 2≠1, anchor 0≠1).
///
/// The tally covers the *whole* assertion tree, descending into the interior of
/// a lookaround as well as into `Alt` branches. Counting a lookaround as one
/// opaque unit would make the comparison blind exactly where it is needed: an
/// assertion dropped inside a lookaround's inner pattern (`(?<=a\b)` extracting
/// as `(?<=a)`) cancels out on both sides and the guard passes a step program
/// that no longer means what the pattern means.
#[derive(Default, PartialEq, Eq)]
pub(crate) struct AssertionTally {
    lookahead: usize,
    lookbehind: usize,
    anchor: usize,
    word_boundary: usize,
    backref: usize,
}

impl AssertionTally {
    fn add(&mut self, other: Self) {
        self.lookahead += other.lookahead;
        self.lookbehind += other.lookbehind;
        self.anchor += other.anchor;
        self.word_boundary += other.word_boundary;
        self.backref += other.backref;
    }
}

/// The exact number of bytes a step program consumes, or `None` when that is
/// not the same for every path through it.
///
/// A lookbehind is checked by walking forwards from `pos - len`, so `len` has to
/// be the *exact* width, not a lower bound: start too early and the walk ends
/// past `pos`, start too late and it ends short. Using a minimum here would read
/// as exact and silently mislocate the walk — a variable-width step must refuse
/// extraction instead, which is what `None` makes the caller do.
///
/// The match is deliberately exhaustive. A new [`PatternStep`] variant then
/// fails to compile here rather than falling into a default arm that guesses a
/// width, which is the only way this stays honest as the step model grows.
pub(crate) fn fixed_byte_len(steps: &[PatternStep]) -> Option<usize> {
    let mut len = 0usize;
    for step in steps {
        let step_len = match step {
            PatternStep::Byte(_) | PatternStep::ByteClass(_) => 1,
            // A codepoint class is fixed-width only when every codepoint it
            // admits encodes to the same number of UTF-8 bytes.
            PatternStep::CodepointClass(cpclass, _) => fixed_utf8_width(cpclass)?,
            // Zero-width: assertions and capture markers.
            PatternStep::WordBoundary
            | PatternStep::NotWordBoundary
            | PatternStep::StartOfText
            | PatternStep::EndOfText
            | PatternStep::StartOfLine
            | PatternStep::EndOfLine
            | PatternStep::CaptureStart(_)
            | PatternStep::CaptureEnd(_)
            | PatternStep::PositiveLookahead(_)
            | PatternStep::NegativeLookahead(_)
            | PatternStep::PositiveLookbehind(_, _)
            | PatternStep::NegativeLookbehind(_, _) => 0,
            // Repetition, alternation and backreferences all admit more than one
            // width.
            PatternStep::GreedyPlus(_)
            | PatternStep::GreedyStar(_)
            | PatternStep::GreedyPlusLookahead(_, _, _)
            | PatternStep::GreedyStarLookahead(_, _, _)
            | PatternStep::NonGreedyPlus(_, _)
            | PatternStep::NonGreedyStar(_, _)
            | PatternStep::GreedyCodepointPlus(_)
            | PatternStep::Alt(_)
            | PatternStep::Backref(_) => return None,
        };
        len += step_len;
    }
    Some(len)
}

/// The fewest bytes a step program can consume.
///
/// Unlike [`fixed_byte_len`] this always answers, because a lower bound exists
/// even for variable-width steps. It is used as a "can there be enough input
/// left?" precheck, where under-counting only costs a wasted attempt — but
/// over-counting would skip real matches, so every arm errs downwards.
///
/// Exhaustive for the same reason [`fixed_byte_len`] is: a new [`PatternStep`]
/// must be classified here deliberately rather than defaulting to zero.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn min_byte_len(steps: &[PatternStep]) -> usize {
    steps
        .iter()
        .map(|s| match s {
            PatternStep::Byte(_) | PatternStep::ByteClass(_) => 1,
            // A codepoint is 1..=4 bytes, so at least one.
            PatternStep::CodepointClass(_, _) | PatternStep::GreedyCodepointPlus(_) => 1,
            // A `+` runs at least once, a `*` may run not at all.
            PatternStep::GreedyPlus(_) | PatternStep::GreedyPlusLookahead(_, _, _) => 1,
            PatternStep::GreedyStar(_) | PatternStep::GreedyStarLookahead(_, _, _) => 0,
            // Non-greedy quantifiers carry their continuation inside them.
            PatternStep::NonGreedyPlus(_, suffix) => 1 + min_byte_len(std::slice::from_ref(suffix)),
            PatternStep::NonGreedyStar(_, suffix) => min_byte_len(std::slice::from_ref(suffix)),
            // The shortest branch bounds the alternation.
            PatternStep::Alt(branches) => {
                branches.iter().map(|b| min_byte_len(b)).min().unwrap_or(0)
            }
            // Zero-width: assertions, capture markers, and a backreference,
            // whose length is unknown here and may be empty.
            PatternStep::CaptureStart(_)
            | PatternStep::CaptureEnd(_)
            | PatternStep::WordBoundary
            | PatternStep::NotWordBoundary
            | PatternStep::StartOfText
            | PatternStep::EndOfText
            | PatternStep::StartOfLine
            | PatternStep::EndOfLine
            | PatternStep::PositiveLookahead(_)
            | PatternStep::NegativeLookahead(_)
            | PatternStep::PositiveLookbehind(_, _)
            | PatternStep::NegativeLookbehind(_, _)
            | PatternStep::Backref(_) => 0,
        })
        .sum()
}

/// The UTF-8 encoded width shared by every codepoint in `cpclass`, or `None` if
/// they differ.
fn fixed_utf8_width(cpclass: &crate::hir::CodepointClass) -> Option<usize> {
    // A negated class admits codepoints from across the whole range, so it spans
    // every encoded width.
    if cpclass.negated || cpclass.ranges.is_empty() {
        return None;
    }
    let width = crate::nfa::utf8_automata::utf8_width;
    let mut common = None;
    for &(start, end) in &cpclass.ranges {
        // A range spanning an encoding boundary is itself variable-width.
        if width(start) != width(end) {
            return None;
        }
        match common {
            None => common = Some(width(start)),
            Some(w) if w == width(start) => {}
            Some(_) => return None,
        }
    }
    common
}

/// What an inner lookaround NFA's match state carries, for the linear walks
/// that stop there.
pub(crate) enum TerminalAssertion {
    /// Nothing that affects whether the inner pattern matches.
    Nothing,
    /// A zero-width assertion that is still part of the inner pattern and must
    /// be appended to its step list.
    Step(PatternStep),
    /// Something the linear step model cannot represent; extraction must refuse
    /// so the caller falls back to the PikeVm.
    Unsupported,
}

/// Reads the assertion an inner NFA's match state carries.
///
/// The inner extractors walk to the match state and stop, so without this the
/// state's instruction is never read and a trailing assertion silently
/// disappears — `(?<=a\b)` extracting as `(?<=a)`, `(?=a$)` as `(?=a)`. The two
/// are different assertions, and dropping one turns an unsupported pattern into
/// a wrong match rather than a fallback.
pub(crate) fn terminal_assertion(state: &NfaState) -> TerminalAssertion {
    match state.instruction {
        None => TerminalAssertion::Nothing,
        // Captures are not tracked while evaluating a lookaround.
        Some(NfaInstruction::CaptureStart(_) | NfaInstruction::CaptureEnd(_)) => {
            TerminalAssertion::Nothing
        }
        Some(NfaInstruction::WordBoundary) => TerminalAssertion::Step(PatternStep::WordBoundary),
        Some(NfaInstruction::NotWordBoundary) => {
            TerminalAssertion::Step(PatternStep::NotWordBoundary)
        }
        Some(NfaInstruction::StartOfText) => TerminalAssertion::Step(PatternStep::StartOfText),
        Some(NfaInstruction::EndOfText) => TerminalAssertion::Step(PatternStep::EndOfText),
        Some(NfaInstruction::StartOfLine) => TerminalAssertion::Step(PatternStep::StartOfLine),
        Some(NfaInstruction::EndOfLine) => TerminalAssertion::Step(PatternStep::EndOfLine),
        Some(_) => TerminalAssertion::Unsupported,
    }
}

/// Whether an NFA contains any lookaround assertion.
///
/// Used to keep the linear step extractor away from nested assertions, which it
/// cannot model.
fn nfa_has_lookaround(nfa: &Nfa) -> bool {
    nfa.states.iter().any(|s| {
        matches!(
            s.instruction,
            Some(
                NfaInstruction::PositiveLookahead(_)
                    | NfaInstruction::NegativeLookahead(_)
                    | NfaInstruction::PositiveLookbehind(_)
                    | NfaInstruction::NegativeLookbehind(_)
            )
        )
    })
}

/// Tallies zero-width assertions reachable from an NFA, including those inside
/// lookaround inner NFAs (held behind `Arc`, so not part of `nfa.states`).
pub(crate) fn count_assertions_in_nfa(nfa: &Nfa) -> AssertionTally {
    let mut t = AssertionTally::default();
    for s in &nfa.states {
        match s.instruction {
            Some(
                NfaInstruction::PositiveLookahead(ref inner)
                | NfaInstruction::NegativeLookahead(ref inner),
            ) => {
                t.lookahead += 1;
                t.add(count_assertions_in_nfa(inner));
            }
            Some(
                NfaInstruction::PositiveLookbehind(ref inner)
                | NfaInstruction::NegativeLookbehind(ref inner),
            ) => {
                t.lookbehind += 1;
                t.add(count_assertions_in_nfa(inner));
            }
            Some(
                NfaInstruction::StartOfText
                | NfaInstruction::EndOfText
                | NfaInstruction::StartOfLine
                | NfaInstruction::EndOfLine,
            ) => t.anchor += 1,
            Some(NfaInstruction::WordBoundary | NfaInstruction::NotWordBoundary) => {
                t.word_boundary += 1
            }
            Some(NfaInstruction::Backref(_)) => t.backref += 1,
            _ => {}
        }
    }
    t
}

/// Tallies zero-width assertions in a step program, recursing into `Alt`
/// branches and into lookaround interiors.
///
/// A genuine alternation carries each assertion in exactly one branch, so
/// summing branches reproduces the NFA tally; a quantifier `Alt` duplicates
/// (and/or drops) assertions, producing a tally that differs from the NFA's.
///
/// Lookaround interiors are summed for the same reason one level down. The
/// inner extractors walk to the inner NFA's match state and stop there, so an
/// assertion compiled onto that state — the `\b` in `(?<=a\b)`, the `$` in
/// `(?=a$)` — never reaches the step list. Descending here is what turns that
/// loss into a tally mismatch, and so into a fallback to the PikeVm.
pub(crate) fn count_assertions_in_steps(steps: &[PatternStep]) -> AssertionTally {
    let mut t = AssertionTally::default();
    for step in steps {
        match step {
            PatternStep::PositiveLookahead(inner) | PatternStep::NegativeLookahead(inner) => {
                t.lookahead += 1;
                t.add(count_assertions_in_steps(inner));
            }
            PatternStep::GreedyPlusLookahead(_, inner, _)
            | PatternStep::GreedyStarLookahead(_, inner, _) => {
                t.lookahead += 1;
                t.add(count_assertions_in_steps(inner));
            }
            PatternStep::PositiveLookbehind(inner, _)
            | PatternStep::NegativeLookbehind(inner, _) => {
                t.lookbehind += 1;
                t.add(count_assertions_in_steps(inner));
            }
            PatternStep::StartOfText
            | PatternStep::EndOfText
            | PatternStep::StartOfLine
            | PatternStep::EndOfLine => t.anchor += 1,
            PatternStep::WordBoundary | PatternStep::NotWordBoundary => t.word_boundary += 1,
            PatternStep::Backref(_) => t.backref += 1,
            PatternStep::Alt(branches) => {
                for b in branches {
                    t.add(count_assertions_in_steps(b));
                }
            }
            _ => {}
        }
    }
    t
}

/// Whether a step program must be executed by the TaggedNfa **interpreter**
/// rather than the JIT, because the JIT's emitted backtracking is unreliable for
/// the shape. Two shapes are deferred:
///
/// - a greedy quantifier together with a *separate* lookaround step (the
///   quantifier must give characters back until the assertion holds), and
/// - two or more quantifiers (adjacent greedy needs nested backtracking, e.g.
///   `\S+\S+\S`).
///
/// A `Greedy*Lookahead` step is neither: the quantifier and its assertion are
/// emitted as one unit that backtracks the one against the other, so `\w+(?=x)`
/// is JIT-compiled rather than deferred. `tests/tagged_jit_agreement.rs` holds
/// the differential coverage for that.
///
/// The interpreter (`TaggedNfa::find`) handles every shape correctly via its
/// boundary-backtracking + recursion, so deferring costs correctness nothing.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn jit_must_defer(steps: &[PatternStep]) -> bool {
    let counts = count_step_kinds(steps);
    let quantifiers = counts.greedy + counts.combined;
    // Nested backtracking across two quantifiers, of any kind.
    if quantifiers >= 2 {
        return true;
    }
    // A quantifier and a lookaround the generic step sequencing has to satisfy
    // by giving characters back — the shape the emitted backtracking gets wrong.
    // A `Greedy*Lookahead` step on its own is not this case: its emitter owns
    // both halves and backtracks the quantifier against its own assertion, so
    // `\w+(?=x)` is compiled rather than deferred.
    quantifiers >= 1 && counts.lookaround >= 1
}

/// Step kinds that decide whether the JIT can emit a program (see
/// [`jit_must_defer`]).
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
#[derive(Default, Clone, Copy)]
struct StepKinds {
    /// Greedy quantifiers with no attached assertion.
    greedy: usize,
    /// `Greedy*Lookahead`: quantifier and assertion emitted as one unit.
    combined: usize,
    /// Lookarounds that stand alone as their own step.
    lookaround: usize,
}

/// Returns `(greedy_quantifier_count, lookaround_count)` over a step program,
/// recursing into `Alt` branches (taking the per-branch max, since only one
/// branch executes). A combined `Greedy*Lookahead` counts as both.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn count_step_kinds(steps: &[PatternStep]) -> StepKinds {
    let mut counts = StepKinds::default();
    for step in steps {
        match step {
            PatternStep::GreedyPlus(_)
            | PatternStep::GreedyStar(_)
            | PatternStep::GreedyCodepointPlus(_)
            | PatternStep::NonGreedyPlus(_, _)
            | PatternStep::NonGreedyStar(_, _) => counts.greedy += 1,
            PatternStep::GreedyPlusLookahead(_, _, _)
            | PatternStep::GreedyStarLookahead(_, _, _) => counts.combined += 1,
            PatternStep::PositiveLookahead(_)
            | PatternStep::NegativeLookahead(_)
            | PatternStep::PositiveLookbehind(_, _)
            | PatternStep::NegativeLookbehind(_, _) => counts.lookaround += 1,
            PatternStep::Alt(branches) => {
                let mut worst = StepKinds::default();
                for branch in branches {
                    let branch_counts = count_step_kinds(branch);
                    worst.greedy = worst.greedy.max(branch_counts.greedy);
                    worst.combined = worst.combined.max(branch_counts.combined);
                    worst.lookaround = worst.lookaround.max(branch_counts.lookaround);
                }
                counts.greedy += worst.greedy;
                counts.combined += worst.combined;
                counts.lookaround += worst.lookaround;
            }
            _ => {}
        }
    }
    counts
}

/// Extracts pattern steps from an NFA for fast matching.
pub struct StepExtractor<'a> {
    nfa: &'a Nfa,
}

impl<'a> StepExtractor<'a> {
    /// Creates a new step extractor for the given NFA.
    pub fn new(nfa: &'a Nfa) -> Self {
        Self { nfa }
    }

    /// Extracts pattern steps, returning None if pattern is too complex.
    pub fn extract(&self) -> Option<Vec<PatternStep>> {
        let mut visited = vec![false; self.nfa.states.len()];
        let steps = self.extract_from_state(self.nfa.start, &mut visited);
        if steps.is_empty() {
            return None;
        }
        // Soundness guard: the linear step extractor mis-models a quantifier split
        // (`X?`, `X*`, `X{n,m}`) as an `Alt`, which corrupts any zero-width
        // assertion attached to the quantifier in one of two ways:
        //   * it DROPS the trailing assertion (`a{1,3}(?=x)` loses the lookahead,
        //     `a{1,3}$` loses the `$`) — the step count is then LESS than the NFA's;
        //   * it DUPLICATES the assertion into each branch (`a*(?=\S)$` puts the
        //     `(?=\S)$` in both the "took some" and "took none" branches) — the
        //     step count is then GREATER than the NFA's.
        // Either way the step program no longer faithfully represents the pattern
        // and the interpreter's non-backtracking `Alt` handler diverges. A genuine
        // alternation instead carries each assertion in exactly one branch, so its
        // count matches the NFA. Bail on any mismatch to the PikeVM, which is
        // correct; tiktoken's `\s+(?!\S)` and the full cl100k/o200k patterns match
        // exactly and stay on the fast path.
        if count_assertions_in_steps(&steps) != count_assertions_in_nfa(self.nfa) {
            return None;
        }
        // Combine greedy quantifiers with following lookahead
        Some(combine_greedy_with_lookahead(steps))
    }

    /// Whether an alternation at `state` can re-enter itself.
    ///
    /// The extractor emits `Alt` as the *last* step and stops, which is only
    /// faithful when the branches run to the end of the pattern. A quantified
    /// alternation (`(?:a|b)+`, and now `\s+`, whose class expands to an
    /// alternation of UTF-8 shapes) loops back instead, so everything after the
    /// alternation — including the repetition itself — would be silently
    /// dropped, leaving a program that matches one iteration and stops. Refusing
    /// hands the pattern to the PikeVM, which models the loop.
    fn alternation_loops_back(&self, state: StateId) -> bool {
        let mut seen = vec![false; self.nfa.states.len()];
        let mut stack: Vec<StateId> = Vec::new();

        let Some(root) = self.nfa.get(state) else {
            return true;
        };
        stack.extend(root.epsilon.iter().copied());
        stack.extend(root.transitions.iter().map(|&(_, target)| target));

        while let Some(id) = stack.pop() {
            if id == state {
                return true;
            }
            match seen.get_mut(id as usize) {
                Some(flag) if !*flag => *flag = true,
                Some(_) => continue,
                // Out of range: treat as unknown, which must read as unsafe.
                None => return true,
            }
            let Some(next) = self.nfa.get(id) else {
                return true;
            };
            stack.extend(next.epsilon.iter().copied());
            stack.extend(next.transitions.iter().map(|&(_, target)| target));
        }
        false
    }

    fn extract_from_state(&self, start: StateId, visited: &mut [bool]) -> Vec<PatternStep> {
        let mut steps = Vec::new();
        let mut current = start;
        let mut iteration = 0;

        loop {
            {
                iteration += 1;
                if iteration > 1000 {
                    return Vec::new();
                }
            }

            if current as usize >= self.nfa.states.len() {
                return Vec::new();
            }

            let state = &self.nfa.states[current as usize];

            // Handle instructions BEFORE checking match state
            // (lookahead instructions can be on the match state itself)
            if let Some(ref instr) = state.instruction {
                match instr {
                    NfaInstruction::CaptureStart(_) | NfaInstruction::CaptureEnd(_) => {
                        // Skip capture markers for find (they don't affect matching)
                    }
                    NfaInstruction::WordBoundary => {
                        steps.push(PatternStep::WordBoundary);
                    }
                    NfaInstruction::NotWordBoundary => {
                        steps.push(PatternStep::NotWordBoundary);
                    }
                    NfaInstruction::StartOfText => {
                        steps.push(PatternStep::StartOfText);
                    }
                    NfaInstruction::EndOfText => {
                        steps.push(PatternStep::EndOfText);
                    }
                    NfaInstruction::PositiveLookahead(inner_nfa) => {
                        let inner_steps = self.extract_lookaround_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return Vec::new();
                        }
                        steps.push(PatternStep::PositiveLookahead(inner_steps));
                    }
                    NfaInstruction::NegativeLookahead(inner_nfa) => {
                        let inner_steps = self.extract_lookaround_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return Vec::new();
                        }
                        steps.push(PatternStep::NegativeLookahead(inner_steps));
                    }
                    NfaInstruction::PositiveLookbehind(inner_nfa) => {
                        // The lookbehind walk needs an exact width, not a
                        // minimum — see `fixed_byte_len`.
                        let inner_steps = self.extract_lookbehind_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return Vec::new();
                        }
                        let Some(width) = fixed_byte_len(&inner_steps) else {
                            return Vec::new();
                        };
                        steps.push(PatternStep::PositiveLookbehind(inner_steps, width));
                    }
                    NfaInstruction::NegativeLookbehind(inner_nfa) => {
                        let inner_steps = self.extract_lookbehind_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return Vec::new();
                        }
                        let Some(width) = fixed_byte_len(&inner_steps) else {
                            return Vec::new();
                        };
                        steps.push(PatternStep::NegativeLookbehind(inner_steps, width));
                    }
                    NfaInstruction::CodepointClass(cpclass, target) => {
                        // Unicode codepoint class - check for greedy loop pattern
                        if (*target as usize) < self.nfa.states.len() {
                            let target_state = &self.nfa.states[*target as usize];
                            if target_state.epsilon.len() == 2
                                && target_state.transitions.is_empty()
                            {
                                let eps0 = target_state.epsilon[0];
                                let eps1 = target_state.epsilon[1];

                                // Check if this is a greedy plus (X+) pattern
                                if eps0 == current {
                                    // Found greedy plus: emit GreedyCodepointPlus and continue to eps1
                                    steps.push(PatternStep::GreedyCodepointPlus(cpclass.clone()));
                                    visited[current as usize] = true;
                                    visited[*target as usize] = true;
                                    current = eps1;
                                    continue;
                                } else if eps1 == current {
                                    // Found greedy plus: emit GreedyCodepointPlus and continue to eps0
                                    steps.push(PatternStep::GreedyCodepointPlus(cpclass.clone()));
                                    visited[current as usize] = true;
                                    visited[*target as usize] = true;
                                    current = eps0;
                                    continue;
                                }
                            }
                        }
                        // Not a greedy loop - emit single CodepointClass and continue
                        steps.push(PatternStep::CodepointClass(cpclass.clone(), *target));
                        if visited[current as usize] {
                            return Vec::new();
                        }
                        visited[current as usize] = true;
                        current = *target;
                        continue;
                    }
                    _ => {
                        // Unsupported instruction
                        return Vec::new();
                    }
                }
            }

            // Handle byte transitions
            if !state.transitions.is_empty() {
                let target = state.transitions[0].1;
                if !state.transitions.iter().all(|(_, t)| *t == target) {
                    return Vec::new();
                }

                let ranges: Vec<ByteRange> = state.transitions.iter().map(|(r, _)| *r).collect();

                // Check for greedy loop
                let target_state = &self.nfa.states[target as usize];
                if target_state.epsilon.len() == 2 && target_state.transitions.is_empty() {
                    let eps0 = target_state.epsilon[0];
                    let eps1 = target_state.epsilon[1];

                    if eps0 == current {
                        // Greedy plus: loop back
                        steps.push(PatternStep::GreedyPlus(ByteClass::new(ranges)));
                        if visited[target as usize] {
                            return Vec::new();
                        }
                        visited[target as usize] = true;
                        current = eps1;
                        continue;
                    }
                }

                // Regular transition
                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;

                if ranges.len() == 1 && ranges[0].start == ranges[0].end {
                    steps.push(PatternStep::Byte(ranges[0].start));
                } else {
                    steps.push(PatternStep::ByteClass(ByteClass::new(ranges)));
                }
                current = target;
                continue;
            }

            // Handle epsilon transitions
            if state.epsilon.len() == 1 {
                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;
                current = state.epsilon[0];
                continue;
            }

            // Multiple epsilon = alternation
            if state.epsilon.len() >= 2 {
                // Extract each alternative branch
                let mut alternatives: Vec<Vec<PatternStep>> = Vec::new();
                for &target in state.epsilon.iter() {
                    let mut branch_visited = visited.to_vec();
                    branch_visited[current as usize] = true;
                    let Some(branch_steps) = self.extract_branch(target, &mut branch_visited)
                    else {
                        // A branch this walk cannot represent: fall back.
                        return Vec::new();
                    };
                    alternatives.push(branch_steps);
                }
                steps.push(PatternStep::Alt(alternatives));
                break; // Alternation consumes the rest of the pattern
            }

            break;
        }

        steps
    }

    /// Extracts steps from a single branch of an alternation.
    fn extract_branch(&self, start: StateId, visited: &mut [bool]) -> Option<Vec<PatternStep>> {
        let mut steps = Vec::new();
        let mut current = start;
        let mut iteration = 0;

        loop {
            {
                iteration += 1;
                if iteration > 10000 {
                    return None;
                }
            }

            if current as usize >= self.nfa.states.len() {
                return None;
            }

            if visited[current as usize] {
                return None;
            }

            let state = &self.nfa.states[current as usize];

            // Match state = end of this branch. An assertion compiled onto it is
            // still part of the branch — `a*(?=b)(?=c)` puts the `(?=c)` here —
            // so read it before stopping. See `terminal_assertion`.
            if state.is_match {
                match terminal_assertion(state) {
                    TerminalAssertion::Nothing => {}
                    TerminalAssertion::Step(step) => steps.push(step),
                    TerminalAssertion::Unsupported => return None,
                }
                break;
            }

            // Handle instructions
            if let Some(ref instr) = state.instruction {
                match instr {
                    NfaInstruction::CaptureStart(_) | NfaInstruction::CaptureEnd(_) => {
                        // Skip capture markers
                    }
                    NfaInstruction::WordBoundary => {
                        steps.push(PatternStep::WordBoundary);
                    }
                    NfaInstruction::NotWordBoundary => {
                        steps.push(PatternStep::NotWordBoundary);
                    }
                    NfaInstruction::StartOfText => {
                        steps.push(PatternStep::StartOfText);
                    }
                    NfaInstruction::EndOfText => {
                        steps.push(PatternStep::EndOfText);
                    }
                    NfaInstruction::PositiveLookahead(inner_nfa) => {
                        let inner_steps = self.extract_lookaround_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return None;
                        }
                        steps.push(PatternStep::PositiveLookahead(inner_steps));
                    }
                    NfaInstruction::NegativeLookahead(inner_nfa) => {
                        let inner_steps = self.extract_lookaround_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return None;
                        }
                        steps.push(PatternStep::NegativeLookahead(inner_steps));
                    }
                    NfaInstruction::CodepointClass(cpclass, target) => {
                        // Check for greedy loop pattern: CodepointClass -> epsilon state -> back to current
                        if (*target as usize) >= self.nfa.states.len() {
                            return None;
                        }

                        let target_state = &self.nfa.states[*target as usize];
                        if target_state.epsilon.len() == 2 && target_state.transitions.is_empty() {
                            let eps0 = target_state.epsilon[0];
                            let eps1 = target_state.epsilon[1];

                            // Check if this is a greedy plus (X+) pattern
                            if eps0 == current {
                                // Found greedy plus: CodepointClass+ - emit GreedyCodepointPlus and continue to eps1
                                steps.push(PatternStep::GreedyCodepointPlus(cpclass.clone()));
                                visited[current as usize] = true;
                                visited[*target as usize] = true;
                                current = eps1;
                                continue;
                            } else if eps1 == current {
                                // Found greedy plus: CodepointClass+ - emit GreedyCodepointPlus and continue to eps0
                                steps.push(PatternStep::GreedyCodepointPlus(cpclass.clone()));
                                visited[current as usize] = true;
                                visited[*target as usize] = true;
                                current = eps0;
                                continue;
                            }
                        }

                        // Not a greedy loop - just emit and continue
                        steps.push(PatternStep::CodepointClass(cpclass.clone(), *target));
                        visited[current as usize] = true;
                        current = *target;
                        continue;
                    }
                    _ => {
                        return None;
                    }
                }
            }

            // Handle byte transitions
            if !state.transitions.is_empty() {
                let target = state.transitions[0].1;
                if !state.transitions.iter().all(|(_, t)| *t == target) {
                    return None;
                }

                let ranges: Vec<ByteRange> = state.transitions.iter().map(|(r, _)| *r).collect();

                // Check for greedy loop
                let target_state = &self.nfa.states[target as usize];
                if target_state.epsilon.len() == 2 && target_state.transitions.is_empty() {
                    let eps0 = target_state.epsilon[0];
                    let eps1 = target_state.epsilon[1];

                    if eps0 == current {
                        steps.push(PatternStep::GreedyPlus(ByteClass::new(ranges)));
                        visited[current as usize] = true;
                        visited[target as usize] = true;
                        current = eps1;
                        continue;
                    } else if eps1 == current {
                        steps.push(PatternStep::GreedyPlus(ByteClass::new(ranges)));
                        visited[current as usize] = true;
                        visited[target as usize] = true;
                        current = eps0;
                        continue;
                    }
                }

                // Simple transition
                steps.push(PatternStep::ByteClass(ByteClass::new(ranges)));
                visited[current as usize] = true;
                current = target;
                continue;
            }

            // Handle epsilon transitions
            if state.epsilon.len() == 1 {
                visited[current as usize] = true;
                current = state.epsilon[0];
                continue;
            }

            // Two epsilons - could be greedy star or alternation
            if state.epsilon.len() == 2 {
                let eps0 = state.epsilon[0];
                let eps1 = state.epsilon[1];

                // Check for greedy loop: if one epsilon leads to an already-visited state,
                // and we have a CodepointClass step, this is a greedy plus/star pattern
                let eps0_visited = visited[eps0 as usize];
                let eps1_visited = visited[eps1 as usize];

                if eps0_visited && !eps1_visited {
                    // eps0 is the back-edge of a greedy loop, eps1 is the exit
                    // Continue with the exit branch
                    visited[current as usize] = true;
                    current = eps1;
                    continue;
                }
                if eps1_visited && !eps0_visited {
                    // eps1 is the back-edge of a greedy loop, eps0 is the exit
                    visited[current as usize] = true;
                    current = eps0;
                    continue;
                }

                // Not a greedy star, treat as alternation
                if self.alternation_loops_back(current) {
                    return None;
                }
                let mut alternatives: Vec<Vec<PatternStep>> = Vec::new();
                let mut any_valid = false;
                for &target in state.epsilon.iter() {
                    let mut branch_visited = visited.to_vec();
                    branch_visited[current as usize] = true;
                    // Check if this branch can reach the match state
                    let target_state = &self.nfa.states[target as usize];
                    if target_state.is_match {
                        // Empty branch directly to match (like in X? patterns)
                        alternatives.push(Vec::new());
                        any_valid = true;
                        continue;
                    }
                    // A branch that cannot be represented is not an empty branch.
                    // Treating the two alike lets the rest of the pattern vanish
                    // from that arm while the step program still looks complete.
                    let branch_steps = self.extract_branch(target, &mut branch_visited)?;
                    alternatives.push(branch_steps);
                    any_valid = true;
                }
                if !any_valid {
                    return None;
                }
                steps.push(PatternStep::Alt(alternatives));
                break;
            }

            // More than 2 epsilons - must be alternation
            if state.epsilon.len() > 2 {
                if self.alternation_loops_back(current) {
                    return None;
                }
                let mut alternatives: Vec<Vec<PatternStep>> = Vec::new();
                let mut any_valid = false;
                for &target in state.epsilon.iter() {
                    let mut branch_visited = visited.to_vec();
                    branch_visited[current as usize] = true;
                    // Check if this branch directly reaches match state
                    let target_state = &self.nfa.states[target as usize];
                    if target_state.is_match {
                        alternatives.push(Vec::new());
                        any_valid = true;
                        continue;
                    }
                    let branch_steps = self.extract_branch(target, &mut branch_visited)?;
                    alternatives.push(branch_steps);
                    any_valid = true;
                }
                if !any_valid {
                    return None;
                }
                steps.push(PatternStep::Alt(alternatives));
                break;
            }

            // Dead end - no transitions, no epsilon, not a match state
            return None;
        }

        Some(steps)
    }

    fn extract_lookaround_steps(&self, inner_nfa: &Nfa) -> Vec<PatternStep> {
        // A nested assertion cannot be modelled by this linear walk: the loop
        // breaks on the match state before reading that state's instruction, so
        // a trailing inner assertion would be dropped and the resulting empty
        // step list read as "assertion satisfied" — turning an unsupported
        // pattern into a wrong match. Refuse instead, so the caller falls back
        // to the PikeVm, which evaluates nested lookaround correctly.
        if nfa_has_lookaround(inner_nfa) {
            return Vec::new();
        }

        let mut visited = vec![false; inner_nfa.states.len()];
        let mut steps = Vec::new();
        let mut current = inner_nfa.start;
        let mut iteration = 0;

        loop {
            {
                iteration += 1;
                if iteration > 10000 {
                    return Vec::new();
                }
            }

            if current as usize >= inner_nfa.states.len() {
                return Vec::new();
            }

            let state = &inner_nfa.states[current as usize];

            if state.is_match {
                match terminal_assertion(state) {
                    TerminalAssertion::Nothing => {}
                    TerminalAssertion::Step(step) => steps.push(step),
                    TerminalAssertion::Unsupported => return Vec::new(),
                }
                break;
            }

            // Handle instructions in lookaround
            if let Some(ref instr) = state.instruction {
                match instr {
                    NfaInstruction::WordBoundary => {
                        steps.push(PatternStep::WordBoundary);
                    }
                    NfaInstruction::EndOfText => {
                        steps.push(PatternStep::EndOfText);
                    }
                    NfaInstruction::StartOfText => {
                        steps.push(PatternStep::StartOfText);
                    }
                    NfaInstruction::CaptureStart(_) | NfaInstruction::CaptureEnd(_) => {
                        // Skip capture markers in lookaround
                    }
                    NfaInstruction::CodepointClass(cpclass, target) => {
                        // Unicode codepoint class in lookaround
                        steps.push(PatternStep::CodepointClass(cpclass.clone(), *target));
                        if visited[current as usize] {
                            return Vec::new();
                        }
                        visited[current as usize] = true;
                        current = *target;
                        continue;
                    }
                    _ => {
                        return Vec::new();
                    }
                }
            }

            if !state.transitions.is_empty() {
                let target = state.transitions[0].1;
                if !state.transitions.iter().all(|(_, t)| *t == target) {
                    return Vec::new();
                }

                let ranges: Vec<ByteRange> = state.transitions.iter().map(|(r, _)| *r).collect();

                // Check for greedy star/plus pattern: state has transitions to target,
                // and target has epsilon transitions where one leads back to current state
                let target_state = &inner_nfa.states[target as usize];

                // Pattern for greedy plus: current -[byte]-> target -[eps]-> current (loop back)
                //                                              |-> next (exit)
                if target_state.transitions.is_empty() && target_state.epsilon.len() == 2 {
                    let eps0 = target_state.epsilon[0];
                    let eps1 = target_state.epsilon[1];

                    // Check if one epsilon leads back to current (greedy loop)
                    if eps0 == current {
                        // Greedy plus: must match at least one
                        steps.push(PatternStep::GreedyPlus(ByteClass::new(ranges)));
                        if visited[target as usize] {
                            return Vec::new();
                        }
                        visited[target as usize] = true;
                        current = eps1; // Continue from exit path
                        continue;
                    } else if eps1 == current {
                        // Greedy plus: loop back is second epsilon
                        steps.push(PatternStep::GreedyPlus(ByteClass::new(ranges)));
                        if visited[target as usize] {
                            return Vec::new();
                        }
                        visited[target as usize] = true;
                        current = eps0; // Continue from exit path
                        continue;
                    }
                }

                // Check for greedy star pattern: current has epsilon to both:
                // - a state with byte transitions (the loop body)
                // - a state that continues (the exit)
                // This is handled below in epsilon section

                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;

                if ranges.len() == 1 && ranges[0].start == ranges[0].end {
                    steps.push(PatternStep::Byte(ranges[0].start));
                } else {
                    steps.push(PatternStep::ByteClass(ByteClass::new(ranges)));
                }
                current = target;
                continue;
            }

            if state.epsilon.len() == 1 && state.transitions.is_empty() {
                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;
                current = state.epsilon[0];
                continue;
            }

            // Handle greedy star: two epsilons where one leads to byte transitions
            // that loop back, and one leads to exit
            if state.epsilon.len() == 2 && state.transitions.is_empty() {
                let eps0 = state.epsilon[0];
                let eps1 = state.epsilon[1];

                // Try to detect: eps0 has transitions that loop, eps1 exits
                // or vice versa
                if let Some((ranges, exit_state)) =
                    self.detect_greedy_star_in_lookaround(inner_nfa, current, eps0, eps1, &visited)
                {
                    steps.push(PatternStep::GreedyStar(ByteClass::new(ranges)));
                    visited[current as usize] = true;
                    current = exit_state;
                    continue;
                }
                if let Some((ranges, exit_state)) =
                    self.detect_greedy_star_in_lookaround(inner_nfa, current, eps1, eps0, &visited)
                {
                    steps.push(PatternStep::GreedyStar(ByteClass::new(ranges)));
                    visited[current as usize] = true;
                    current = exit_state;
                    continue;
                }

                // Not a recognized greedy star pattern
                return Vec::new();
            }

            if !state.epsilon.is_empty() {
                return Vec::new();
            }

            break;
        }

        steps
    }

    /// Extracts pattern steps from a lookbehind inner NFA.
    /// This is simpler than lookahead extraction - it doesn't recognize GreedyStar/Plus
    /// because check_lookbehind uses fixed-length matching.
    /// Patterns with repetitions in lookbehind will return empty, causing fallback to PikeVM.
    fn extract_lookbehind_steps(&self, inner_nfa: &Nfa) -> Vec<PatternStep> {
        // Same reason as `extract_lookaround_steps`: refuse a nested assertion
        // rather than silently dropping it.
        if nfa_has_lookaround(inner_nfa) {
            return Vec::new();
        }

        let mut visited = vec![false; inner_nfa.states.len()];
        let mut steps = Vec::new();
        let mut current = inner_nfa.start;

        loop {
            if current as usize >= inner_nfa.states.len() {
                return Vec::new();
            }

            let state = &inner_nfa.states[current as usize];

            if state.is_match {
                match terminal_assertion(state) {
                    TerminalAssertion::Nothing => {}
                    TerminalAssertion::Step(step) => steps.push(step),
                    TerminalAssertion::Unsupported => return Vec::new(),
                }
                break;
            }

            // A codepoint class consumes input, so it advances the walk rather
            // than being appended as a zero-width step. `fixed_byte_len` refuses
            // the whole lookbehind if the class is not fixed-width, so the
            // interpreter's fixed-offset walk stays valid.
            if let Some(NfaInstruction::CodepointClass(cpclass, target)) = &state.instruction {
                steps.push(PatternStep::CodepointClass(cpclass.clone(), *target));
                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;
                current = *target;
                continue;
            }

            // Interior assertions translate the same way as a terminal one — the
            // position in the walk does not change what the assertion means.
            match terminal_assertion(state) {
                TerminalAssertion::Nothing => {}
                TerminalAssertion::Step(step) => steps.push(step),
                TerminalAssertion::Unsupported => return Vec::new(),
            }

            if !state.transitions.is_empty() {
                let target = state.transitions[0].1;
                if !state.transitions.iter().all(|(_, t)| *t == target) {
                    return Vec::new();
                }

                let ranges: Vec<ByteRange> = state.transitions.iter().map(|(r, _)| *r).collect();

                // Check for repetition patterns - we can't handle these in lookbehind
                let target_state = &inner_nfa.states[target as usize];
                if target_state.transitions.is_empty() && target_state.epsilon.len() == 2 {
                    let eps0 = target_state.epsilon[0];
                    let eps1 = target_state.epsilon[1];
                    // If any epsilon leads back to current, it's a loop - reject
                    if eps0 == current || eps1 == current {
                        return Vec::new();
                    }
                }

                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;

                if ranges.len() == 1 && ranges[0].start == ranges[0].end {
                    steps.push(PatternStep::Byte(ranges[0].start));
                } else {
                    steps.push(PatternStep::ByteClass(ByteClass::new(ranges)));
                }
                current = target;
                continue;
            }

            if state.epsilon.len() == 1 && state.transitions.is_empty() {
                if visited[current as usize] {
                    return Vec::new();
                }
                visited[current as usize] = true;
                current = state.epsilon[0];
                continue;
            }

            // Any other epsilon patterns (including repetitions) - reject
            if !state.epsilon.is_empty() {
                return Vec::new();
            }

            break;
        }

        steps
    }

    /// Detects a greedy star pattern where `loop_start` has transitions that loop back
    /// to `loop_start` itself (or to a state that leads back), and `exit_state` is the continuation.
    /// Returns (ranges, exit_state) if detected, None otherwise.
    fn detect_greedy_star_in_lookaround(
        &self,
        inner_nfa: &Nfa,
        _branch_state: StateId,
        loop_start: StateId,
        exit_state: StateId,
        visited: &[bool],
    ) -> Option<(Vec<ByteRange>, StateId)> {
        if loop_start as usize >= inner_nfa.states.len() {
            return None;
        }

        let loop_state = &inner_nfa.states[loop_start as usize];

        // The loop state should have byte transitions
        if loop_state.transitions.is_empty() {
            return None;
        }

        // All transitions should go to the same target
        let target = loop_state.transitions[0].1;
        if !loop_state.transitions.iter().all(|(_, t)| *t == target) {
            return None;
        }

        let ranges: Vec<ByteRange> = loop_state.transitions.iter().map(|(r, _)| *r).collect();

        // The target should have epsilon back to loop_start (completing the loop)
        let target_state = &inner_nfa.states[target as usize];

        // Check if target has two epsilons where one leads back to loop_start
        if target_state.epsilon.len() == 2 {
            let eps0 = target_state.epsilon[0];
            let eps1 = target_state.epsilon[1];

            // Check if one epsilon leads back to loop_start
            if eps0 == loop_start {
                // eps0 loops back, eps1 exits
                if !visited[loop_start as usize] {
                    // The exit should eventually lead to exit_state or be it
                    return Some((ranges, exit_state));
                }
            } else if eps1 == loop_start {
                // eps1 loops back, eps0 exits
                if !visited[loop_start as usize] {
                    return Some((ranges, exit_state));
                }
            }
        }

        // Simple case: target has single epsilon back to loop_start
        if target_state.epsilon.len() == 1
            && target_state.epsilon[0] == loop_start
            && !visited[loop_start as usize]
        {
            return Some((ranges, exit_state));
        }

        None
    }
}

#[cfg(test)]
mod fixed_byte_len_tests {
    use super::*;
    use crate::hir::CodepointClass;

    fn class(ranges: &[(u32, u32)], negated: bool) -> PatternStep {
        PatternStep::CodepointClass(CodepointClass::new(ranges.to_vec(), negated), 0)
    }

    #[test]
    fn consuming_steps_add_their_width() {
        assert_eq!(fixed_byte_len(&[]), Some(0));
        assert_eq!(
            fixed_byte_len(&[PatternStep::Byte(b'a'), PatternStep::Byte(b'b')]),
            Some(2)
        );
    }

    #[test]
    fn assertions_and_capture_markers_are_zero_width() {
        assert_eq!(
            fixed_byte_len(&[
                PatternStep::StartOfText,
                PatternStep::Byte(b'a'),
                PatternStep::WordBoundary,
                PatternStep::NotWordBoundary,
                PatternStep::EndOfText,
                PatternStep::StartOfLine,
                PatternStep::EndOfLine,
                PatternStep::CaptureStart(1),
                PatternStep::CaptureEnd(1),
            ]),
            Some(1)
        );
    }

    #[test]
    fn codepoint_class_counts_its_utf8_width() {
        // Greek: every codepoint encodes to two bytes.
        assert_eq!(fixed_byte_len(&[class(&[(0x3B1, 0x3C9)], false)]), Some(2));
        // CJK: three bytes.
        assert_eq!(
            fixed_byte_len(&[class(&[(0x4E00, 0x9FFF)], false)]),
            Some(3)
        );
        // ASCII: one byte.
        assert_eq!(
            fixed_byte_len(&[class(&[(b'a' as u32, b'z' as u32)], false)]),
            Some(1)
        );
        // Astral: four bytes.
        assert_eq!(
            fixed_byte_len(&[class(&[(0x1F600, 0x1F64F)], false)]),
            Some(4)
        );
    }

    #[test]
    fn variable_width_codepoint_classes_are_refused() {
        // A range straddling the 1/2-byte boundary.
        assert_eq!(fixed_byte_len(&[class(&[(0x7F, 0x80)], false)]), None);
        // Two ranges of differing width.
        assert_eq!(
            fixed_byte_len(&[class(&[(b'a' as u32, b'z' as u32), (0x3B1, 0x3C9)], false)]),
            None
        );
        // A negated class admits codepoints of every width.
        assert_eq!(fixed_byte_len(&[class(&[(0x3B1, 0x3C9)], true)]), None);
        // An empty class matches nothing, so it has no width to report.
        assert_eq!(fixed_byte_len(&[class(&[], false)]), None);
    }

    #[test]
    fn variable_width_steps_are_refused() {
        let bytes = ByteClass::new(vec![ByteRange::new(b'a', b'z')]);
        assert_eq!(
            fixed_byte_len(&[PatternStep::GreedyPlus(bytes.clone())]),
            None
        );
        assert_eq!(
            fixed_byte_len(&[PatternStep::GreedyStar(bytes.clone())]),
            None
        );
        assert_eq!(fixed_byte_len(&[PatternStep::Backref(1)]), None);
        assert_eq!(
            fixed_byte_len(&[PatternStep::Alt(vec![
                vec![PatternStep::Byte(b'a')],
                vec![PatternStep::Byte(b'a'), PatternStep::Byte(b'b')],
            ])]),
            None
        );
    }

    #[test]
    fn one_refusal_refuses_the_whole_program() {
        assert_eq!(
            fixed_byte_len(&[PatternStep::Byte(b'a'), PatternStep::Backref(1)]),
            None
        );
    }
}
