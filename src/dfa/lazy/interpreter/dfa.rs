//! Lazy DFA interpreter implementation.
//!
//! Builds DFA states on-demand using subset construction,
//! caching them for future use.
//!
//! ## Performance Optimizations
//!
//! This implementation uses several optimizations from the regex crate:
//!
//! 1. **Premultiplied State IDs**: State IDs are pre-multiplied by stride (256),
//!    so `transitions[state + byte]` is a simple addition instead of
//!    `transitions[state * 256 + byte]` which requires multiplication.
//!
//! 2. **Tagged State IDs**: High bits encode status (match/dead/unknown), allowing
//!    status checks without memory dereference.
//!
//! 3. **Dense Transition Table**: A flat array of all transitions for cache efficiency.
//!
//! ## Cache Eviction Strategy
//!
//! We use a **full flush** strategy instead of LRU. When the cache reaches
//! its limit, we clear all states and rebuild from the start state.

use std::collections::BTreeSet;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use crate::nfa::{Nfa, NfaInstruction, StateId as NfaStateId};

use super::super::shared::{
    epsilon_closure_with_context, flush_cache, get_or_create_state_with_class, is_dead_state,
    is_tagged_match, is_unknown_state, match_reachable_without_end_assertion, state_index,
    tag_state, untag_state, CacheCeilingExceeded, CharClass, DfaStateId, LazyDfaContext,
    PositionContext, DEAD_STATE, UNKNOWN_STATE,
};

/// How much input the per-start attempts in [`LazyDfa::find_from`] may walk in
/// total before the single-pass search takes over. A few passes' worth: high
/// enough that attempts which fail near their start never reach it, low enough
/// that ones which scan to the end trip it within a handful of tries.
const SCAN_BUDGET_FACTOR: usize = 4;

/// A lazy DFA that builds states on demand.
#[derive(Debug, Clone)]
pub struct LazyDfa {
    /// Internal context containing state and transition data.
    ctx: LazyDfaContext,
}

/// Holds a [`LazyDfa`] for the duration of one search.
///
/// State IDs are premultiplied indices into the state and transition tables, so
/// a cache flush turns every ID a running search is holding into garbage. While
/// this guard lives, flushing is suppressed and the cache grows instead; the
/// deferred flush happens on the outermost drop, so the growth does not carry
/// into the next search. Being a `Drop` guard, it also restores the depth when a
/// search unwinds, which is what keeps a panic from suppressing flushes forever.
///
/// Searches may nest, hence a depth count rather than a flag.
struct SearchGuard<'a> {
    dfa: &'a mut LazyDfa,
}

impl<'a> SearchGuard<'a> {
    fn new(dfa: &'a mut LazyDfa) -> Self {
        dfa.ctx.search_depth += 1;
        Self { dfa }
    }

    /// Reports the search's result, unless it gave up on the cache ceiling.
    fn finish<T>(self, outcome: T) -> Result<T, CacheCeilingExceeded> {
        if std::mem::take(&mut self.dfa.ctx.ceiling_exceeded) {
            Err(CacheCeilingExceeded)
        } else {
            Ok(outcome)
        }
    }
}

impl Drop for SearchGuard<'_> {
    fn drop(&mut self) {
        self.dfa.ctx.search_depth = self.dfa.ctx.search_depth.saturating_sub(1);
        if self.dfa.ctx.search_depth == 0 && self.dfa.ctx.states.len() >= self.dfa.ctx.cache_limit {
            flush_cache(&mut self.dfa.ctx);
        }
    }
}

impl Deref for SearchGuard<'_> {
    type Target = LazyDfa;

    fn deref(&self) -> &LazyDfa {
        &*self.dfa
    }
}

impl DerefMut for SearchGuard<'_> {
    fn deref_mut(&mut self) -> &mut LazyDfa {
        &mut *self.dfa
    }
}

impl LazyDfa {
    /// Creates a new lazy DFA from an NFA.
    pub fn new(nfa: Nfa) -> Self {
        Self {
            ctx: LazyDfaContext::new(nfa),
        }
    }

    /// Sets the cache size limit.
    pub fn set_cache_limit(&mut self, limit: usize) {
        self.ctx.set_cache_limit(limit);
    }

    /// Returns the number of times the cache has been flushed.
    pub fn flush_count(&self) -> usize {
        self.ctx.flush_count()
    }

    /// Returns the start state.
    pub fn start(&self) -> DfaStateId {
        self.ctx.start()
    }

    /// Returns true if this DFA has word boundary assertions.
    pub fn has_word_boundary(&self) -> bool {
        self.ctx.has_word_boundary()
    }

    /// Returns true if this DFA has anchor assertions (^, $).
    pub fn has_anchors(&self) -> bool {
        self.ctx.has_anchors()
    }

    /// Returns true if this DFA has a start anchor (^).
    pub fn has_start_anchor(&self) -> bool {
        self.ctx.has_start_anchor()
    }

    /// Returns true if this DFA has an end anchor ($).
    pub fn has_end_anchor(&self) -> bool {
        self.ctx.has_end_anchor()
    }

    /// Returns true if any accepting state is reachable without an end assertion.
    pub fn has_clean_accept(&self) -> bool {
        self.ctx.has_clean_accept()
    }

    /// Returns true if this DFA has multiline anchors.
    pub fn has_multiline_anchors(&self) -> bool {
        self.ctx.has_multiline_anchors()
    }

    /// Returns true if the start anchor specifically is line-mode.
    pub fn has_multiline_start_anchor(&self) -> bool {
        self.ctx.has_multiline_start_anchor()
    }

    /// Gets the transition for a state and byte, computing it if needed.
    #[inline]
    pub fn transition(&mut self, state: DfaStateId, byte: u8) -> Option<DfaStateId> {
        let idx = (state + byte as u32) as usize;
        if idx < self.ctx.transitions.len() {
            let tagged = unsafe { *self.ctx.transitions.get_unchecked(idx) };
            if !is_unknown_state(tagged) {
                if is_dead_state(tagged) {
                    return None;
                }
                return Some(untag_state(tagged));
            }
        }

        self.compute_transition(state, byte)
    }

    /// Fast transition lookup returning tagged state ID.
    #[inline(always)]
    pub fn transition_tagged(&self, state: DfaStateId, byte: u8) -> u32 {
        let idx = (state + byte as u32) as usize;
        if idx < self.ctx.transitions.len() {
            unsafe { *self.ctx.transitions.get_unchecked(idx) }
        } else {
            UNKNOWN_STATE
        }
    }

    /// Fast transition lookup for cached states only (immutable).
    #[inline(always)]
    pub fn transition_cached(&self, state: DfaStateId, byte: u8) -> Option<DfaStateId> {
        let idx = (state + byte as u32) as usize;
        if idx < self.ctx.transitions.len() {
            let tagged = unsafe { *self.ctx.transitions.get_unchecked(idx) };
            if is_unknown_state(tagged) || is_dead_state(tagged) {
                None
            } else {
                Some(untag_state(tagged))
            }
        } else {
            None
        }
    }

    /// Computes a transition, handling word boundaries and anchors.
    fn compute_transition(&mut self, state: DfaStateId, byte: u8) -> Option<DfaStateId> {
        let state_idx = state_index(state);
        let dfa_state = self.ctx.states.get(state_idx)?;
        let nfa_states = dfa_state.nfa_states.clone();
        let prev_class = dfa_state.prev_class;

        let curr_class = CharClass::from_byte(byte);

        let is_at_boundary = if self.ctx.has_word_boundary {
            Some(prev_class != curr_class)
        } else {
            None
        };

        let pos_ctx = if self.ctx.has_anchors {
            Some(PositionContext::middle())
        } else {
            None
        };

        let expanded_nfa_states = if self.ctx.has_word_boundary || self.ctx.has_anchors {
            epsilon_closure_with_context(&self.ctx.nfa, &nfa_states, is_at_boundary, pos_ctx)
        } else {
            nfa_states
        };

        let mut next_states = BTreeSet::new();

        for &nfa_state in &expanded_nfa_states {
            if let Some(nfa_s) = self.ctx.nfa.get(nfa_state) {
                for (range, target) in &nfa_s.transitions {
                    if range.contains(byte) {
                        next_states.insert(*target);
                    }
                }
            }
        }

        let cache_idx = (state + byte as u32) as usize;

        if next_states.is_empty() {
            if cache_idx < self.ctx.transitions.len() {
                self.ctx.transitions[cache_idx] = DEAD_STATE;
            }
            return None;
        }

        let target_pos_ctx = if self.ctx.has_anchors {
            if self.ctx.has_multiline_anchors && byte == b'\n' {
                Some(PositionContext::after_newline())
            } else {
                None
            }
        } else {
            None
        };

        let next_closure = if self.ctx.has_word_boundary || self.ctx.has_anchors {
            epsilon_closure_with_context(&self.ctx.nfa, &next_states, None, target_pos_ctx)
        } else {
            self.ctx.nfa.epsilon_closure(&next_states)
        };

        if next_closure.is_empty() {
            if cache_idx < self.ctx.transitions.len() {
                self.ctx.transitions[cache_idx] = DEAD_STATE;
            }
            return None;
        }

        let next_clean = match_reachable_without_end_assertion(
            &self.ctx.nfa,
            &next_states,
            None,
            target_pos_ctx,
        );
        let flushes_before = self.ctx.flush_count;
        let next_id =
            get_or_create_state_with_class(&mut self.ctx, next_closure, curr_class, next_clean);

        let next_idx = state_index(next_id);
        let is_match = self.ctx.states.get(next_idx).is_some_and(|s| s.is_match);

        // `state` is a premultiplied index into the table. Creating the target
        // may have flushed the cache, which renumbers every state, so the row
        // this index names is no longer this state's row — writing there would
        // cache the transition against an unrelated state.
        if self.ctx.flush_count == flushes_before {
            let cache_idx = (state + byte as u32) as usize;
            if cache_idx < self.ctx.transitions.len() {
                self.ctx.transitions[cache_idx] = tag_state(next_id, is_match);
            }
        }

        Some(next_id)
    }

    /// Returns true if the state is a match state.
    pub fn is_match(&self, state: DfaStateId) -> bool {
        let idx = state_index(state);
        self.ctx
            .states
            .get(idx)
            .map(|s| s.is_match)
            .unwrap_or(false)
    }

    /// Returns the prev_class of a state (for JIT compilation).
    pub fn get_state_prev_class(&self, state: DfaStateId) -> CharClass {
        let idx = state_index(state);
        self.ctx
            .states
            .get(idx)
            .map(|s| s.prev_class)
            .unwrap_or(CharClass::NonWord)
    }

    /// Gets or creates a start state with a specific previous character class.
    pub fn get_start_state_for_class(&mut self, prev_class: CharClass) -> DfaStateId {
        self.get_start_state_with_prev_class(prev_class)
    }

    /// Computes all 256 transitions for a state at once.
    pub fn compute_all_transitions(&mut self, state: DfaStateId) -> [Option<DfaStateId>; 256] {
        let mut result = [None; 256];

        let base_idx = state as usize;
        if base_idx + 255 < self.ctx.transitions.len() {
            let mut all_cached = true;
            for byte in 0..=255u8 {
                let tagged = self.ctx.transitions[base_idx + byte as usize];
                if is_unknown_state(tagged) {
                    all_cached = false;
                } else if !is_dead_state(tagged) {
                    result[byte as usize] = Some(untag_state(tagged));
                }
            }
            if all_cached {
                return result;
            }
        }

        if !self.ctx.has_word_boundary && !self.ctx.has_anchors {
            self.compute_all_transitions_simple(state, &mut result);
        } else {
            self.compute_all_transitions_with_context(state, &mut result);
        }

        result
    }

    /// Computes all transitions for patterns without word boundaries or anchors.
    fn compute_all_transitions_simple(
        &mut self,
        state: DfaStateId,
        result: &mut [Option<DfaStateId>; 256],
    ) {
        let state_idx = state_index(state);
        let nfa_states = match self.ctx.states.get(state_idx) {
            Some(s) => s.nfa_states.clone(),
            None => return,
        };

        let mut byte_targets: [Option<BTreeSet<u32>>; 256] = std::array::from_fn(|_| None);

        for &nfa_state in &nfa_states {
            if let Some(nfa_s) = self.ctx.nfa.get(nfa_state) {
                for (range, target) in &nfa_s.transitions {
                    for byte in range.start..=range.end {
                        byte_targets[byte as usize]
                            .get_or_insert_with(BTreeSet::new)
                            .insert(*target);
                    }
                }
            }
        }

        for byte in 0..=255u8 {
            let cache_idx = (state + byte as u32) as usize;

            if cache_idx < self.ctx.transitions.len() {
                let tagged = self.ctx.transitions[cache_idx];
                if !is_unknown_state(tagged) {
                    if !is_dead_state(tagged) {
                        result[byte as usize] = Some(untag_state(tagged));
                    }
                    continue;
                }
            }

            if let Some(ref targets) = byte_targets[byte as usize] {
                if targets.is_empty() {
                    if cache_idx < self.ctx.transitions.len() {
                        self.ctx.transitions[cache_idx] = DEAD_STATE;
                    }
                    continue;
                }

                let next_closure = self.ctx.nfa.epsilon_closure(targets);
                if next_closure.is_empty() {
                    if cache_idx < self.ctx.transitions.len() {
                        self.ctx.transitions[cache_idx] = DEAD_STATE;
                    }
                    continue;
                }

                let next_clean =
                    match_reachable_without_end_assertion(&self.ctx.nfa, targets, None, None);
                let flushes_before = self.ctx.flush_count;
                let next_id = get_or_create_state_with_class(
                    &mut self.ctx,
                    next_closure,
                    CharClass::NonWord,
                    next_clean,
                );
                result[byte as usize] = Some(next_id);

                let next_idx = state_index(next_id);
                let is_match = self.ctx.states.get(next_idx).is_some_and(|s| s.is_match);
                // `state` is fixed across this 256-byte loop, but creating a
                // target may flush and renumber the cache underneath it, so the
                // row this index names is only this state's row while no flush
                // has happened.
                if self.ctx.flush_count == flushes_before {
                    let cache_idx = (state + byte as u32) as usize;
                    if cache_idx < self.ctx.transitions.len() {
                        self.ctx.transitions[cache_idx] = tag_state(next_id, is_match);
                    }
                }
            } else if cache_idx < self.ctx.transitions.len() {
                self.ctx.transitions[cache_idx] = DEAD_STATE;
            }
        }
    }

    /// Computes all transitions for patterns with word boundaries or anchors.
    fn compute_all_transitions_with_context(
        &mut self,
        state: DfaStateId,
        result: &mut [Option<DfaStateId>; 256],
    ) {
        for byte in 0..=255u8 {
            let cache_idx = (state + byte as u32) as usize;
            if cache_idx < self.ctx.transitions.len() {
                let tagged = self.ctx.transitions[cache_idx];
                if !is_unknown_state(tagged) {
                    if !is_dead_state(tagged) {
                        result[byte as usize] = Some(untag_state(tagged));
                    }
                    continue;
                }
            }
            if let Some(target) = self.transition(state, byte) {
                result[byte as usize] = Some(target);
            }
        }
    }

    /// Returns the number of cached states.
    pub fn state_count(&self) -> usize {
        self.ctx.state_count()
    }

    /// Clears the DFA cache (except start state).
    pub fn clear_cache(&mut self) {
        flush_cache(&mut self.ctx);
    }

    /// Executes the DFA on input, returning true if it matches the whole input.
    ///
    /// `Err` means the state cache could not grow far enough to finish the scan;
    /// the answer is unknown, not `false`.
    pub fn is_match_bytes(&mut self, input: &[u8]) -> Result<bool, CacheCeilingExceeded> {
        let mut guard = SearchGuard::new(self);
        let outcome = guard.is_match_bytes_inner(input);
        guard.finish(outcome)
    }

    /// [`LazyDfa::is_match_bytes`] without the search guard.
    fn is_match_bytes_inner(&mut self, input: &[u8]) -> bool {
        let mut state = self.ctx.start;

        for &byte in input {
            match self.transition(state, byte) {
                Some(next) => state = next,
                None => return false,
            }
        }

        self.is_match(state)
    }

    /// Finds the first match in the input.
    ///
    /// `Err` means the search gave up on the cache ceiling; see
    /// [`LazyDfa::find_from`].
    pub fn find(&mut self, input: &[u8]) -> Result<Option<(usize, usize)>, CacheCeilingExceeded> {
        self.find_from(input, 0)
    }

    /// Finds the leftmost match starting at or after `from`.
    ///
    /// Every attempt gets the whole input (see [`LazyDfa::find_at`]), so the
    /// start state carries the real preceding byte for `^` and `\b`.
    ///
    /// `Err` means the search needed more DFA states than the cache is allowed
    /// to hold, so the transition table is incomplete and the scan's result
    /// would be a false negative. Callers re-run such a search on an engine that
    /// does not cache states — the PikeVM — rather than reporting "no match".
    pub fn find_from(
        &mut self,
        input: &[u8],
        from: usize,
    ) -> Result<Option<(usize, usize)>, CacheCeilingExceeded> {
        let mut guard = SearchGuard::new(self);
        let outcome = guard.find_from_inner(input, from);
        guard.finish(outcome)
    }

    /// [`LazyDfa::find_from`] without the search guard.
    fn find_from_inner(&mut self, input: &[u8], from: usize) -> Option<(usize, usize)> {
        if from > input.len() {
            return None;
        }

        // If pattern has both start and end anchors, they may be on different branches
        // of an alternation (e.g., ^X|Y$), so we need to do an unanchored search.
        // Only optimize with line-boundary-only search if we have ONLY a start anchor.
        let start_only = self.ctx.has_start_anchor && !self.ctx.has_end_anchor;

        if start_only {
            if self.ctx.has_multiline_anchors {
                if from == 0 {
                    if let Some(end) = self.find_at_inner(input, 0) {
                        return Some((0, end));
                    }
                }
                // Line starts at or after `from`: the newline itself may sit just
                // before it, hence `from - 1`.
                for (i, &byte) in input.iter().enumerate().skip(from.saturating_sub(1)) {
                    if byte == b'\n' {
                        if let Some(end) = self.find_at_inner(input, i + 1) {
                            return Some((i + 1, end));
                        }
                    }
                }
                None
            } else if from == 0 {
                self.find_at_inner(input, 0).map(|end| (0, end))
            } else {
                None
            }
        } else {
            // Trying one start at a time is right while the attempts are cheap:
            // most give up within a byte or two of where they began, and the
            // first one that matches ends the search. It collapses when they are
            // not — a pattern that consumes a long run before failing costs a
            // full scan per start, and the search turns quadratic.
            //
            // So the attempts are metered. Once they have collectively walked
            // several times the input, the pattern is one of those, and the
            // single-pass search takes over from where the loop stopped.
            let budget = input.len().saturating_mul(SCAN_BUDGET_FACTOR);
            let mut walked = 0usize;

            for start_pos in from..=input.len() {
                // The cache ran out under an earlier start; the remaining ones
                // would scan an incomplete table, so stop and let the entry
                // point report the give-up.
                if self.ctx.ceiling_exceeded {
                    return None;
                }
                self.ctx.last_reach = start_pos;
                if let Some(end) = self.find_at_inner(input, start_pos) {
                    return Some((start_pos, end));
                }
                walked += self.ctx.last_reach.saturating_sub(start_pos);
                if walked > budget {
                    let start = self.leftmost_start(input, start_pos + 1)?;
                    return self.find_at_inner(input, start).map(|end| (start, end));
                }
            }
            None
        }
    }

    /// Finds a match starting at the given position, returning its end.
    ///
    /// `Err` means the search gave up on the cache ceiling; see
    /// [`LazyDfa::find_from`].
    pub fn find_at(
        &mut self,
        input: &[u8],
        start: usize,
    ) -> Result<Option<usize>, CacheCeilingExceeded> {
        let mut guard = SearchGuard::new(self);
        let outcome = guard.find_at_inner(input, start);
        guard.finish(outcome)
    }

    /// [`LazyDfa::find_at`] without the search guard.
    fn find_at_inner(&mut self, input: &[u8], start: usize) -> Option<usize> {
        // A previous attempt in this search already ran out of cache; every
        // further one would scan an incomplete table, so unwind instead.
        if self.ctx.ceiling_exceeded {
            return None;
        }

        // If pattern has ONLY a start anchor (no end anchor), we can skip invalid positions.
        // But if it has both anchors (possibly on different alternation branches), we need
        // to try all positions and let the NFA handle anchor checking per branch.
        let start_only = self.ctx.has_start_anchor && !self.ctx.has_end_anchor;

        if start_only {
            let valid_start = if self.ctx.has_multiline_anchors {
                crate::nfa::at_line_start(input, start)
            } else {
                start == 0
            };
            if !valid_start {
                return None;
            }
        }

        let state = self.get_start_state_for_position(input, start);
        self.find_at_with_state(input, start, state)
    }

    /// Gets the appropriate start state for a position.
    fn get_start_state_for_position(&mut self, input: &[u8], start: usize) -> DfaStateId {
        let prev_class = if start > 0 {
            CharClass::from_byte(input[start - 1])
        } else {
            CharClass::NonWord
        };

        if self.ctx.has_anchors {
            let pos_ctx = if start == 0 {
                PositionContext::start_of_input()
            } else if self.ctx.has_multiline_anchors && input[start - 1] == b'\n' {
                PositionContext::after_newline()
            } else {
                PositionContext::middle()
            };

            let mut start_set = BTreeSet::new();
            start_set.insert(self.ctx.nfa.start);

            let is_at_boundary: Option<bool> = None;

            let start_closure = epsilon_closure_with_context(
                &self.ctx.nfa,
                &start_set,
                is_at_boundary,
                Some(pos_ctx),
            );
            let start_clean = match_reachable_without_end_assertion(
                &self.ctx.nfa,
                &start_set,
                is_at_boundary,
                Some(pos_ctx),
            );
            get_or_create_state_with_class(&mut self.ctx, start_closure, prev_class, start_clean)
        } else if self.ctx.has_word_boundary && start > 0 {
            self.get_start_state_with_prev_class(prev_class)
        } else {
            self.ctx.start
        }
    }

    /// Gets a start state with a specific previous character class.
    fn get_start_state_with_prev_class(&mut self, prev_class: CharClass) -> DfaStateId {
        if prev_class == CharClass::NonWord {
            return self.ctx.start;
        }

        let mut start_set = BTreeSet::new();
        start_set.insert(self.ctx.nfa.start);

        let start_closure = epsilon_closure_with_context(&self.ctx.nfa, &start_set, None, None);
        let start_clean =
            match_reachable_without_end_assertion(&self.ctx.nfa, &start_set, None, None);
        get_or_create_state_with_class(&mut self.ctx, start_closure, prev_class, start_clean)
    }

    /// Internal find implementation with explicit start state.
    fn find_at_with_state(
        &mut self,
        input: &[u8],
        start: usize,
        state: DfaStateId,
    ) -> Option<usize> {
        if !self.ctx.has_word_boundary && !self.ctx.has_anchors {
            return self.find_at_with_state_fast(input, start, state);
        }

        if !self.ctx.has_word_boundary && !self.ctx.has_multiline_anchors {
            return self.find_at_with_state_anchored(input, start, state);
        }

        self.find_at_with_state_checked(input, start, state)
    }

    /// Find that re-checks every candidate end against that state's assertions.
    fn find_at_with_state_checked(
        &mut self,
        input: &[u8],
        start: usize,
        state: DfaStateId,
    ) -> Option<usize> {
        let mut last_match = if self.is_match(state) {
            if self.check_end_assertions(input, start, state) {
                Some(start)
            } else {
                None
            }
        } else {
            None
        };

        let mut current_state = state;
        for (i, &byte) in input[start..].iter().enumerate() {
            match self.transition(current_state, byte) {
                Some(next) => {
                    current_state = next;
                    if self.is_match(current_state) {
                        let match_end = start + i + 1;
                        if self.check_end_assertions(input, match_end, current_state) {
                            last_match = Some(match_end);
                        }
                    }
                }
                None => {
                    self.ctx.last_reach = start + i;
                    return last_match;
                }
            }
        }

        self.ctx.last_reach = input.len();
        last_match
    }

    /// Fast find for patterns with only simple anchors.
    #[inline(never)]
    fn find_at_with_state_anchored(
        &mut self,
        input: &[u8],
        start: usize,
        state: DfaStateId,
    ) -> Option<usize> {
        let potential_match = self.find_at_with_state_fast(input, start, state);

        if let Some(end_pos) = potential_match {
            if !self.ctx.has_end_anchor
                || crate::nfa::at_end_or_before_final_newline(input, end_pos)
            {
                return potential_match;
            }
            // The greedy end fails `$`. If every accepting state requires it,
            // no shorter end can satisfy it either — `$` holds at one position,
            // and a shorter end is further from it — so the start is rejected.
            if !self.ctx.has_clean_accept {
                return None;
            }
            // Otherwise `$` belongs to the branch carrying it: `a|b$` on "aa"
            // reaches a match through `a`, which asks for nothing at the end.
            return self.find_at_with_state_checked(input, start, state);
        }

        None
    }

    /// Fast find implementation for patterns without assertions.
    #[inline(never)]
    fn find_at_with_state_fast(
        &mut self,
        input: &[u8],
        start: usize,
        mut state: DfaStateId,
    ) -> Option<usize> {
        let mut last_match = if self.is_match(state) {
            Some(start)
        } else {
            None
        };

        let bytes = &input[start..];
        let len = bytes.len();
        let mut i = 0;

        // Every exit records how far the scan walked. `find_from` uses it to
        // notice when start attempts are scanning the whole input each time.
        macro_rules! done {
            () => {{
                self.ctx.last_reach = start + i;
                return last_match;
            }};
        }

        while i + 4 <= len {
            let b0 = unsafe { *bytes.get_unchecked(i) };
            let b1 = unsafe { *bytes.get_unchecked(i + 1) };
            let b2 = unsafe { *bytes.get_unchecked(i + 2) };
            let b3 = unsafe { *bytes.get_unchecked(i + 3) };

            let tagged0 = self.transition_or_compute(state, b0);
            if is_dead_state(tagged0) {
                done!();
            }
            state = untag_state(tagged0);
            if is_tagged_match(tagged0) {
                last_match = Some(start + i + 1);
            }

            let tagged1 = self.transition_or_compute(state, b1);
            if is_dead_state(tagged1) {
                done!();
            }
            state = untag_state(tagged1);
            if is_tagged_match(tagged1) {
                last_match = Some(start + i + 2);
            }

            let tagged2 = self.transition_or_compute(state, b2);
            if is_dead_state(tagged2) {
                done!();
            }
            state = untag_state(tagged2);
            if is_tagged_match(tagged2) {
                last_match = Some(start + i + 3);
            }

            let tagged3 = self.transition_or_compute(state, b3);
            if is_dead_state(tagged3) {
                done!();
            }
            state = untag_state(tagged3);
            if is_tagged_match(tagged3) {
                last_match = Some(start + i + 4);
            }

            i += 4;
        }

        while i < len {
            let byte = unsafe { *bytes.get_unchecked(i) };
            let tagged = self.transition_or_compute(state, byte);
            if is_dead_state(tagged) {
                break;
            }
            state = untag_state(tagged);
            if is_tagged_match(tagged) {
                last_match = Some(start + i + 1);
            }
            i += 1;
        }

        self.ctx.last_reach = start + i;
        last_match
    }

    /// Transition in the unanchored automaton: like [`LazyDfa::transition`], but
    /// the target set also contains a match beginning at the position reached.
    ///
    /// Never dead — the start is always live — so this returns a plain state.
    fn transition_unanchored(&mut self, state: DfaStateId, byte: u8) -> DfaStateId {
        let idx = (state + byte as u32) as usize;
        if idx < self.ctx.transitions_unanchored.len() {
            let tagged = self.ctx.transitions_unanchored[idx];
            if !is_unknown_state(tagged) {
                return untag_state(tagged);
            }
        }
        self.compute_transition_unanchored(state, byte)
    }

    /// Subset construction for one unanchored transition.
    ///
    /// Identical to [`LazyDfa::compute_transition`] except that the NFA start is
    /// added to the target set before its closure is taken. Seeding through the
    /// closure — rather than by hand — is what makes `^` behave: the closure runs
    /// in mid-input context, which kills a start-anchored branch at every
    /// position but the first.
    fn compute_transition_unanchored(&mut self, state: DfaStateId, byte: u8) -> DfaStateId {
        let state_idx = state_index(state);
        let (nfa_states, prev_class) = match self.ctx.states.get(state_idx) {
            Some(st) => (st.nfa_states.clone(), st.prev_class),
            None => return self.ctx.start,
        };

        let curr_class = CharClass::from_byte(byte);

        let is_at_boundary = if self.ctx.has_word_boundary {
            Some(prev_class != curr_class)
        } else {
            None
        };
        let pos_ctx = if self.ctx.has_anchors {
            Some(PositionContext::middle())
        } else {
            None
        };

        let expanded = if self.ctx.has_word_boundary || self.ctx.has_anchors {
            epsilon_closure_with_context(&self.ctx.nfa, &nfa_states, is_at_boundary, pos_ctx)
        } else {
            nfa_states
        };

        let mut next_states = BTreeSet::new();
        for &nfa_state in &expanded {
            if let Some(nfa_s) = self.ctx.nfa.get(nfa_state) {
                for (range, target) in &nfa_s.transitions {
                    if range.contains(byte) {
                        next_states.insert(*target);
                    }
                }
            }
        }
        next_states.insert(self.ctx.nfa.start);

        let target_pos_ctx = if self.ctx.has_anchors {
            if self.ctx.has_multiline_anchors && byte == b'\n' {
                Some(PositionContext::after_newline())
            } else {
                Some(PositionContext::middle())
            }
        } else {
            None
        };

        let next_closure = if self.ctx.has_word_boundary || self.ctx.has_anchors {
            epsilon_closure_with_context(&self.ctx.nfa, &next_states, None, target_pos_ctx)
        } else {
            self.ctx.nfa.epsilon_closure(&next_states)
        };
        let next_clean = match_reachable_without_end_assertion(
            &self.ctx.nfa,
            &next_states,
            None,
            target_pos_ctx,
        );
        let next_id =
            get_or_create_state_with_class(&mut self.ctx, next_closure, curr_class, next_clean);

        let is_match = self
            .ctx
            .states
            .get(state_index(next_id))
            .is_some_and(|st| st.is_match);

        if self.ctx.transitions_unanchored.len() < self.ctx.transitions.len() {
            self.ctx
                .transitions_unanchored
                .resize(self.ctx.transitions.len(), UNKNOWN_STATE);
        }
        let idx = (state + byte as u32) as usize;
        if idx < self.ctx.transitions_unanchored.len() {
            self.ctx.transitions_unanchored[idx] = tag_state(next_id, is_match);
        }

        next_id
    }

    /// One pass over `input[from..]`, reporting whether any match starting at or
    /// before `seed_until` exists.
    ///
    /// Positions up to `seed_until` transition in the unanchored automaton, so
    /// each is considered as a start; past it the anchored automaton takes over
    /// and no new start is introduced. `seed_until >= input.len()` therefore asks
    /// "does this pattern match at all", and a smaller value asks "does it match
    /// starting no later than here" — which is monotone in `seed_until`, and so
    /// binary-searchable for the leftmost start.
    fn matches_starting_by(&mut self, input: &[u8], from: usize, seed_until: usize) -> bool {
        if self.ctx.ceiling_exceeded {
            return false;
        }

        let mut state = self.get_start_state_for_position(input, from);
        if self.is_match(state) && self.check_end_assertions(input, from, state) {
            return true;
        }

        for (i, &byte) in input[from..].iter().enumerate() {
            let pos = from + i;
            state = if pos < seed_until {
                self.transition_unanchored(state, byte)
            } else {
                match self.transition(state, byte) {
                    Some(next) => next,
                    None => return false,
                }
            };
            if self.is_match(state) && self.check_end_assertions(input, pos + 1, state) {
                return true;
            }
        }

        false
    }

    /// The leftmost start position at or after `from` that begins a match.
    ///
    /// Binary search over [`LazyDfa::matches_starting_by`]: each probe is one
    /// pass, so this costs O(n log n) where trying every start costs O(n²).
    fn leftmost_start(&mut self, input: &[u8], from: usize) -> Option<usize> {
        if !self.matches_starting_by(input, from, input.len() + 1) {
            return None;
        }
        let (mut lo, mut hi) = (from, input.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.matches_starting_by(input, from, mid) {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Some(lo)
    }

    /// Get transition, computing if needed, returning tagged state.
    ///
    /// The computed state is returned directly. Re-reading the table at
    /// `state + byte` instead would be reading through an ID that computing the
    /// transition may have invalidated (a cache flush renumbers every state),
    /// and would report the live target as dead.
    #[inline(always)]
    fn transition_or_compute(&mut self, state: DfaStateId, byte: u8) -> u32 {
        let idx = (state + byte as u32) as usize;
        if idx < self.ctx.transitions.len() {
            let tagged = unsafe { *self.ctx.transitions.get_unchecked(idx) };
            if !is_unknown_state(tagged) {
                return tagged;
            }
        }
        match self.compute_transition(state, byte) {
            Some(next_id) => tag_state(next_id, self.is_match(next_id)),
            None => DEAD_STATE,
        }
    }

    /// Checks if the end position satisfies any trailing assertions.
    ///
    /// For patterns like `^A|B$`, we need to check if there's ANY valid path to a match.
    /// - If there's a match state that doesn't require an assertion, the match is valid.
    /// - Only require an assertion if ALL match paths require it.
    fn check_end_assertions(&self, input: &[u8], pos: usize, state: DfaStateId) -> bool {
        if !self.ctx.has_word_boundary && !self.ctx.has_anchors {
            return true;
        }

        let state_idx = state_index(state);
        let dfa_state = match self.ctx.states.get(state_idx) {
            Some(s) => s,
            None => return true,
        };

        // For end anchors: If there's a match path that doesn't require end anchors,
        // the match is valid without satisfying them. This handles patterns like `^A|B$`.
        // Note: This only applies to end anchors, not word boundaries.
        if self.ctx.has_anchors
            && !self.ctx.has_word_boundary
            && dfa_state.match_without_end_assertion
        {
            return true;
        }

        // No clean match path - check if assertions are satisfied
        if self.ctx.has_word_boundary {
            let prev_class = dfa_state.prev_class;

            let next_class = if pos < input.len() {
                CharClass::from_byte(input[pos])
            } else {
                CharClass::NonWord
            };

            let is_at_boundary = prev_class != next_class;

            let needs_word_boundary = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
                matches!(instr, NfaInstruction::WordBoundary)
            });

            let needs_not_word_boundary = self
                .state_needs_assertion(&dfa_state.nfa_states, |instr| {
                    matches!(instr, NfaInstruction::NotWordBoundary)
                });

            if needs_word_boundary && !is_at_boundary {
                return false;
            }
            if needs_not_word_boundary && is_at_boundary {
                return false;
            }
        }

        if self.ctx.has_anchors {
            let needs_end_of_text = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
                matches!(instr, NfaInstruction::EndOfText)
            });

            if needs_end_of_text {
                // End of text, or immediately before a trailing newline — the
                // same rule `crate::reference` and the other engines follow, so
                // `a$` matches "a" in "a\n". Patterns without a word boundary
                // take the clean-match-path shortcut above and never get here,
                // which is why this only ever showed up alongside `\b`/`\B`.
                let at_end = pos == input.len()
                    || (pos + 1 == input.len() && input.get(pos) == Some(&b'\n'));
                if !at_end {
                    return false;
                }
            }

            let needs_end_of_line = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
                matches!(instr, NfaInstruction::EndOfLine)
            });

            if needs_end_of_line && !crate::nfa::at_line_end(input, pos) {
                return false;
            }
        }

        true
    }

    /// Helper to check if any NFA state in the set has a pending assertion.
    ///
    /// Important: We only check states that are actually IN the nfa_states set,
    /// NOT their epsilon targets. The epsilon closure already filtered out assertion
    /// states that don't match the current position context. Checking epsilon targets
    /// would incorrectly require assertions for paths that were already blocked.
    ///
    /// For example, in pattern `(?m)^A|B$`:
    /// - Branch 1 reaches match through ^A (no end anchor)
    /// - Branch 2 reaches match through B$ (EndOfLine)
    ///
    /// After matching, the DFA state may include states from both branches.
    /// If EndOfLine was filtered out during epsilon closure (because we're not at EOL),
    /// we shouldn't require it - branch 1's path is still valid.
    fn state_needs_assertion<F>(&self, nfa_states: &BTreeSet<NfaStateId>, pred: F) -> bool
    where
        F: Fn(&NfaInstruction) -> bool,
    {
        nfa_states.iter().any(|&nfa_id| {
            self.ctx
                .nfa
                .get(nfa_id)
                .is_some_and(|nfa_state| nfa_state.instruction.as_ref().is_some_and(&pred))
        })
    }

    /// Returns the boundary requirements for a state.
    pub fn get_state_boundary_requirements(&self, state: DfaStateId) -> (bool, bool) {
        let state_idx = state_index(state);
        let dfa_state = match self.ctx.states.get(state_idx) {
            Some(s) => s,
            None => return (false, false),
        };

        let needs_word_boundary = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
            matches!(instr, NfaInstruction::WordBoundary)
        });

        let needs_not_word_boundary = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
            matches!(instr, NfaInstruction::NotWordBoundary)
        });

        (needs_word_boundary, needs_not_word_boundary)
    }

    /// Whether a match is reachable in `state` without passing an end assertion.
    pub fn get_state_match_without_end_assertion(&self, state: DfaStateId) -> bool {
        self.ctx
            .states
            .get(state_index(state))
            .is_some_and(|s| s.match_without_end_assertion)
    }

    /// Returns the anchor requirements for a state.
    pub fn get_state_anchor_requirements(&self, state: DfaStateId) -> (bool, bool) {
        let state_idx = state_index(state);
        let dfa_state = match self.ctx.states.get(state_idx) {
            Some(s) => s,
            None => return (false, false),
        };

        let needs_end_of_text = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
            matches!(instr, NfaInstruction::EndOfText)
        });

        let needs_end_of_line = self.state_needs_assertion(&dfa_state.nfa_states, |instr| {
            matches!(instr, NfaInstruction::EndOfLine)
        });

        (needs_end_of_text, needs_end_of_line)
    }

    /// Provides access to the internal NFA reference.
    pub fn nfa(&self) -> &Nfa {
        &self.ctx.nfa
    }

    /// The NFA this DFA was built from, shareable without copying it.
    ///
    /// Lets a caller that catches [`CacheCeilingExceeded`] build the fallback
    /// engine for the same pattern.
    pub fn nfa_arc(&self) -> Arc<Nfa> {
        self.ctx.nfa_arc()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dfa::EagerDfa;
    use crate::hir::translate;
    use crate::parser::parse;

    fn dfa(pattern: &str) -> LazyDfa {
        let hir = parse(pattern).and_then(|ast| translate(&ast)).unwrap();
        LazyDfa::new(crate::nfa::compile(&hir).unwrap())
    }

    /// An end anchor belongs to the branch that carries it, not to the pattern.
    ///
    /// `a|b$` can match through `a`, which asks for nothing at the end, so 0..1
    /// of "aa" stands even though `$` does not hold there. `a$|b` must not get
    /// the same treatment, and `world$` — where the anchor sits on the accepting
    /// state itself rather than a predecessor — must still enforce it.
    ///
    /// Driven against the engines directly: engine selection sends these to the
    /// PikeVM today, which would hide a regression here behind the selector.
    /// Every expectation was taken from `regexr::reference` and cross-checked
    /// against the `regex` crate.
    #[test]
    fn end_anchor_in_one_branch_does_not_gate_the_others() {
        for (pattern, input, expected) in [
            ("a|b$", "aa", Some((0, 1))),
            ("a|b$", "ab", Some((0, 1))),
            ("a|b$", "ba", Some((1, 2))),
            ("a|a$", "aa", Some((0, 1))),
            ("a$|b", "ab", Some((1, 2))),
            ("a$|b", "aa", Some((1, 2))),
            ("world$", "world hello", None),
            ("world$", "hello world", Some((6, 11))),
        ] {
            let mut lazy = dfa(pattern);
            assert_eq!(
                lazy.find_from(input.as_bytes(), 0),
                Ok(expected),
                "LazyDfa {pattern:?} on {input:?}"
            );

            let eager = EagerDfa::from_lazy(&mut lazy);
            assert_eq!(
                eager.find_from(input.as_bytes(), 0),
                expected,
                "EagerDfa {pattern:?} on {input:?}"
            );
        }
    }
}
