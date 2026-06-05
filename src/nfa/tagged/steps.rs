//! Pattern step extraction from NFA.
//!
//! Extracts pattern steps from NFA for fast step-based matching.

use super::shared::PatternStep;
use crate::nfa::{ByteClass, ByteRange, Nfa, NfaInstruction, StateId};

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
#[derive(Default, PartialEq, Eq)]
pub(crate) struct AssertionTally {
    lookahead: usize,
    lookbehind: usize,
    anchor: usize,
    word_boundary: usize,
    backref: usize,
}

/// Tallies zero-width assertions present in an NFA's own states. Inner
/// lookaround NFAs are held behind `Arc` and are not part of `nfa.states`, so
/// each top-level assertion is counted exactly once.
pub(crate) fn count_assertions_in_nfa(nfa: &Nfa) -> AssertionTally {
    let mut t = AssertionTally::default();
    for s in &nfa.states {
        match s.instruction {
            Some(NfaInstruction::PositiveLookahead(_) | NfaInstruction::NegativeLookahead(_)) => {
                t.lookahead += 1
            }
            Some(NfaInstruction::PositiveLookbehind(_) | NfaInstruction::NegativeLookbehind(_)) => {
                t.lookbehind += 1
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
/// branches. A genuine alternation carries each assertion in exactly one branch,
/// so summing branches reproduces the NFA tally; a quantifier `Alt` duplicates
/// (and/or drops) assertions, producing a tally that differs from the NFA's.
pub(crate) fn count_assertions_in_steps(steps: &[PatternStep]) -> AssertionTally {
    let mut t = AssertionTally::default();
    for step in steps {
        match step {
            PatternStep::PositiveLookahead(_)
            | PatternStep::NegativeLookahead(_)
            | PatternStep::GreedyPlusLookahead(_, _, _)
            | PatternStep::GreedyStarLookahead(_, _, _) => t.lookahead += 1,
            PatternStep::PositiveLookbehind(_, _) | PatternStep::NegativeLookbehind(_, _) => {
                t.lookbehind += 1
            }
            PatternStep::StartOfText
            | PatternStep::EndOfText
            | PatternStep::StartOfLine
            | PatternStep::EndOfLine => t.anchor += 1,
            PatternStep::WordBoundary | PatternStep::NotWordBoundary => t.word_boundary += 1,
            PatternStep::Backref(_) => t.backref += 1,
            PatternStep::Alt(branches) => {
                for b in branches {
                    let bt = count_assertions_in_steps(b);
                    t.lookahead += bt.lookahead;
                    t.lookbehind += bt.lookbehind;
                    t.anchor += bt.anchor;
                    t.word_boundary += bt.word_boundary;
                    t.backref += bt.backref;
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
/// - a greedy quantifier together with a lookaround (the quantifier must give
///   characters back until the assertion holds, e.g. `[\r\n]+(?=\S)`), and
/// - two or more greedy quantifiers (adjacent greedy needs nested backtracking,
///   e.g. `\S+\S+\S`).
///
/// The interpreter (`TaggedNfa::find`) handles both correctly via its
/// boundary-backtracking + recursion, and it is the engine the tiktoken hot path
/// already uses (the JIT defers all Unicode-class greedy+lookaround to it), so
/// deferring these byte-class shapes costs nothing on real workloads.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn jit_must_defer(steps: &[PatternStep]) -> bool {
    let (greedy, lookaround) = count_greedy_and_lookaround(steps);
    greedy >= 2 || (greedy >= 1 && lookaround >= 1)
}

/// Returns `(greedy_quantifier_count, lookaround_count)` over a step program,
/// recursing into `Alt` branches (taking the per-branch max, since only one
/// branch executes). A combined `Greedy*Lookahead` counts as both.
#[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
fn count_greedy_and_lookaround(steps: &[PatternStep]) -> (usize, usize) {
    let mut greedy = 0;
    let mut look = 0;
    for step in steps {
        match step {
            PatternStep::GreedyPlus(_)
            | PatternStep::GreedyStar(_)
            | PatternStep::GreedyCodepointPlus(_)
            | PatternStep::NonGreedyPlus(_, _)
            | PatternStep::NonGreedyStar(_, _) => greedy += 1,
            PatternStep::GreedyPlusLookahead(_, _, _)
            | PatternStep::GreedyStarLookahead(_, _, _) => {
                greedy += 1;
                look += 1;
            }
            PatternStep::PositiveLookahead(_)
            | PatternStep::NegativeLookahead(_)
            | PatternStep::PositiveLookbehind(_, _)
            | PatternStep::NegativeLookbehind(_, _) => look += 1,
            PatternStep::Alt(branches) => {
                let (mut bg, mut bl) = (0, 0);
                for b in branches {
                    let (g, l) = count_greedy_and_lookaround(b);
                    bg = bg.max(g);
                    bl = bl.max(l);
                }
                greedy += bg;
                look += bl;
            }
            _ => {}
        }
    }
    (greedy, look)
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
                        // Lookbehind uses fixed-length matching, so don't allow GreedyStar/Plus
                        let inner_steps = self.extract_lookbehind_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return Vec::new();
                        }
                        let min_len = Self::calc_min_len(&inner_steps);
                        steps.push(PatternStep::PositiveLookbehind(inner_steps, min_len));
                    }
                    NfaInstruction::NegativeLookbehind(inner_nfa) => {
                        // Lookbehind uses fixed-length matching, so don't allow GreedyStar/Plus
                        let inner_steps = self.extract_lookbehind_steps(inner_nfa);
                        if inner_steps.is_empty() {
                            return Vec::new();
                        }
                        let min_len = Self::calc_min_len(&inner_steps);
                        steps.push(PatternStep::NegativeLookbehind(inner_steps, min_len));
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
                    let branch_steps = self.extract_branch(target, &mut branch_visited);
                    if branch_steps.is_empty() {
                        // If any branch fails to extract, fall back
                        return Vec::new();
                    }
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
    fn extract_branch(&self, start: StateId, visited: &mut [bool]) -> Vec<PatternStep> {
        let mut steps = Vec::new();
        let mut current = start;
        let mut iteration = 0;

        loop {
            {
                iteration += 1;
                if iteration > 10000 {
                    return Vec::new();
                }
            }

            if current as usize >= self.nfa.states.len() {
                return Vec::new();
            }

            if visited[current as usize] {
                return Vec::new();
            }

            let state = &self.nfa.states[current as usize];

            // Match state = end of this branch
            if state.is_match {
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
                    NfaInstruction::CodepointClass(cpclass, target) => {
                        // Check for greedy loop pattern: CodepointClass -> epsilon state -> back to current
                        if (*target as usize) >= self.nfa.states.len() {
                            return Vec::new();
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
                    let branch_steps = self.extract_branch(target, &mut branch_visited);
                    // Empty steps means either valid empty branch or extraction failure
                    // We'll accept it as valid since we're dealing with alternations
                    alternatives.push(branch_steps);
                    any_valid = true;
                }
                if !any_valid {
                    return Vec::new();
                }
                steps.push(PatternStep::Alt(alternatives));
                break;
            }

            // More than 2 epsilons - must be alternation
            if state.epsilon.len() > 2 {
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
                    let branch_steps = self.extract_branch(target, &mut branch_visited);
                    // Accept empty branches as valid for alternations
                    alternatives.push(branch_steps);
                    any_valid = true;
                }
                if !any_valid {
                    return Vec::new();
                }
                steps.push(PatternStep::Alt(alternatives));
                break;
            }

            // Dead end - no transitions, no epsilon, not a match state
            return Vec::new();
        }

        steps
    }

    fn extract_lookaround_steps(&self, inner_nfa: &Nfa) -> Vec<PatternStep> {
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
        let mut visited = vec![false; inner_nfa.states.len()];
        let mut steps = Vec::new();
        let mut current = inner_nfa.start;

        loop {
            if current as usize >= inner_nfa.states.len() {
                return Vec::new();
            }

            let state = &inner_nfa.states[current as usize];

            if state.is_match {
                break;
            }

            // Handle instructions
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
                    _ => return Vec::new(),
                }
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

    /// Calculates the minimum length (in bytes) of input that a sequence of steps can match.
    pub fn calc_min_len(steps: &[PatternStep]) -> usize {
        let mut len = 0;
        for step in steps {
            match step {
                PatternStep::Byte(_) => len += 1,
                PatternStep::ByteClass(_) => len += 1,
                PatternStep::GreedyPlus(_) => len += 1, // At least one
                PatternStep::GreedyStar(_) => {}        // Zero or more
                PatternStep::GreedyPlusLookahead(_, _, _) => len += 1,
                PatternStep::GreedyStarLookahead(_, _, _) => {}
                PatternStep::PositiveLookahead(_)
                | PatternStep::NegativeLookahead(_)
                | PatternStep::PositiveLookbehind(_, _)
                | PatternStep::NegativeLookbehind(_, _) => {} // Zero-width
                PatternStep::WordBoundary
                | PatternStep::NotWordBoundary
                | PatternStep::StartOfText
                | PatternStep::EndOfText => {} // Zero-width
                _ => {} // Other steps - conservatively assume 0
            }
        }
        len
    }
}
