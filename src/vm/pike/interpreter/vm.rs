//! PikeVM implementation.
//!
//! A thread-based NFA simulator that supports capture groups,
//! backreferences, and lookarounds.
//!
//! Non-greedy quantifiers are supported through thread priority:
//! - Threads have a priority that increases when taking "exit" paths
//! - For non-greedy quantifiers, the exit path has higher priority
//! - The first match from the highest-priority thread wins
//!
//! # Optimizations
//!
//! This implementation includes several key optimizations:
//! 1. **Sparse Set Deduplication**: O(1) state deduplication using generation counters
//! 2. **BinaryHeap Scheduling**: Efficient backref handling with min-heap instead of BTreeMap
//! 3. **`Arc<Nfa>` for Lookarounds**: Avoids expensive NFA cloning during lookaround checks

use crate::hir::unicode::is_word_byte;
use crate::nfa::{Nfa, NfaInstruction, StateId};
use std::sync::Arc;

use crate::vm::pike::shared::{
    decode_utf8_codepoint, InstructionResult, PendingThread, PikeVmContext, Thread,
};

/// The PikeVM executor.
pub struct PikeVm {
    nfa: Arc<Nfa>,
}

impl PikeVm {
    /// Creates a new PikeVM from an NFA.
    pub fn new(nfa: Nfa) -> Self {
        Self { nfa: Arc::new(nfa) }
    }

    /// Creates a new PikeVM from an `Arc<Nfa>` (avoids cloning).
    pub fn from_arc(nfa: Arc<Nfa>) -> Self {
        Self { nfa }
    }

    /// Returns true if the pattern matches the input.
    pub fn is_match(&self, input: &[u8]) -> bool {
        self.find(input).is_some()
    }

    /// Finds the first match, returning (start, end).
    pub fn find(&self, input: &[u8]) -> Option<(usize, usize)> {
        for start in 0..=input.len() {
            // Only start at UTF-8 codepoint boundaries (see `is_utf8_boundary`).
            if !crate::nfa::is_utf8_boundary(input, start) {
                continue;
            }
            if let Some(end) = self.match_at(input, start) {
                return Some((start, end));
            }
        }
        None
    }

    /// Returns capture groups for the first match.
    pub fn captures(&self, input: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
        for start in 0..=input.len() {
            if !crate::nfa::is_utf8_boundary(input, start) {
                continue;
            }
            if let Some(captures) = self.captures_at(input, start) {
                return Some(captures);
            }
        }
        None
    }

    /// Finds a match starting at the given position.
    /// Returns (start, end) if found.
    ///
    /// This method correctly handles word boundaries by using the full input
    /// to determine the character class before the start position.
    pub fn find_at(&self, input: &[u8], start: usize) -> Option<(usize, usize)> {
        self.match_at(input, start).map(|end| (start, end))
    }

    /// Attempts to match at a specific position.
    fn match_at(&self, input: &[u8], start: usize) -> Option<usize> {
        self.captures_at(input, start)
            .and_then(|caps| caps.first().and_then(|c| c.map(|(_, end)| end)))
    }

    /// Returns capture groups for a match known to start at position 0.
    ///
    /// This is more efficient than `captures()` when match bounds are already
    /// known (e.g., from a DFA match). Skips the loop that tries every position.
    pub fn captures_from_start(&self, input: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
        self.captures_at(input, 0)
    }

    /// Creates a reusable context for this VM.
    pub fn create_context(&self) -> PikeVmContext {
        PikeVmContext::new(self.nfa.capture_count as usize, self.nfa.states.len())
    }

    /// Returns capture groups using a pre-allocated context.
    /// This is the fastest method for repeated captures on different inputs.
    ///
    /// The context should be created once via `create_context()` and reused.
    pub fn captures_from_start_with_context(
        &self,
        input: &[u8],
        ctx: &mut PikeVmContext,
    ) -> Option<Vec<Option<(usize, usize)>>> {
        self.captures_with_context(input, ctx, 0)
    }

    /// Returns capture groups using a pre-allocated context, starting from a given position.
    pub fn captures_with_context(
        &self,
        input: &[u8],
        ctx: &mut PikeVmContext,
        start_pos: usize,
    ) -> Option<Vec<Option<(usize, usize)>>> {
        ctx.reset();
        ctx.ensure_state_capacity(self.nfa.states.len());

        let capture_count = self.nfa.capture_count as usize;

        // Schedule the initial thread. All threads — whether they advance by a byte
        // or by a multi-byte codepoint — flow through a single priority-ordered
        // queue keyed on (position, seq). `seq` is assigned in priority order as
        // threads are scheduled, so at any position the highest-priority thread is
        // processed first. Greedy vs non-greedy is encoded entirely in the NFA's
        // epsilon ordering (preserved by the closure), so no special non-greedy
        // bookkeeping is needed here.
        let seq = ctx.seq_counter;
        ctx.seq_counter += 1;
        ctx.future_threads.push(PendingThread {
            pos: start_pos,
            seq,
            thread: Thread::new(self.nfa.start, capture_count),
        });

        let mut matched: Option<Vec<Option<(usize, usize)>>> = None;

        // Buffer for the consuming successors scheduled at each position.
        let mut sched: Vec<(usize, Thread)> = Vec::new();

        // Copy `pos` out of the peeked thread so the borrow ends before the body
        // pops from / pushes to `future_threads` below.
        while let Some(pos) = ctx.future_threads.peek().map(|pt| pt.pos) {
            if pos > input.len() {
                break;
            }

            // Fresh generation for per-position state deduplication.
            ctx.generation = ctx.generation.wrapping_add(1);
            ctx.current_threads.clear();

            // Drain every thread scheduled for this position. The heap yields them
            // in (seq) priority order; the epsilon closure expands each into
            // `current_threads`, preserving that order.
            while matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                let pt = ctx.future_threads.pop().unwrap();
                self.add_thread(ctx, pt.thread, input, pos);
            }

            // The first (highest-priority) thread to reach a match wins at this
            // position and preempts every lower-priority thread. A higher-priority
            // thread that is still consuming may overwrite this match at a later
            // position (it comes from a more-preferred path).
            let match_idx = ctx
                .current_threads
                .iter()
                .position(|t| self.nfa.get(t.state).map(|s| s.is_match).unwrap_or(false));
            if let Some(i) = match_idx {
                let mut caps = ctx.current_threads[i].reconstruct_captures();
                caps[0] = Some((start_pos, pos));
                matched = Some(caps);
            }
            // Only threads strictly higher priority than the match advance.
            let limit = match_idx.unwrap_or(ctx.current_threads.len());

            // Schedule consuming successors in priority order so `seq` keeps
            // reflecting pattern priority. A state may consume a byte (byte-range
            // transitions) or a codepoint (`CodepointClass` instruction).
            let byte = input.get(pos).copied();
            sched.clear();
            for thread in &ctx.current_threads[..limit] {
                let state = match self.nfa.get(thread.state) {
                    Some(s) => s,
                    None => continue,
                };
                if let Some(b) = byte {
                    for (range, target) in &state.transitions {
                        if range.contains(b) {
                            sched.push((pos + 1, thread.clone_with_state(*target)));
                        }
                    }
                }
                if let Some(NfaInstruction::CodepointClass(cpclass, target)) = &state.instruction {
                    if let Some((cp, len)) = decode_utf8_codepoint(&input[pos..]) {
                        if cpclass.contains(cp) {
                            sched.push((pos + len, thread.clone_with_state(*target)));
                        }
                    }
                }
            }
            for (npos, th) in sched.drain(..) {
                let seq = ctx.seq_counter;
                ctx.seq_counter += 1;
                ctx.future_threads.push(PendingThread {
                    pos: npos,
                    seq,
                    thread: th,
                });
            }
        }

        matched
    }

    /// Epsilon closure for one seed thread at `pos`, adding all reachable states
    /// to `ctx.current_threads` in priority order.
    ///
    /// Epsilons are pushed onto the LIFO stack in REVERSE order so the first
    /// (highest-priority) epsilon is popped first — preserving the leftmost-first
    /// ordering that encodes greedy vs non-greedy. A `CodepointClass` state is a
    /// *consuming* state (like a byte-transition state): it rests in
    /// `current_threads` and the caller performs the consume, so the closure does
    /// not follow it. Backreferences (`Jump`) schedule their continuations into
    /// the shared priority queue.
    fn add_thread(&self, ctx: &mut PikeVmContext, thread: Thread, input: &[u8], pos: usize) {
        ctx.epsilon_stack.push(thread);
        while let Some(mut thread) = ctx.epsilon_stack.pop() {
            let state_id = thread.state as usize;

            // O(1) per-generation deduplication: highest-priority arrival wins.
            if ctx.visited.get(state_id).copied() == Some(ctx.generation) {
                continue;
            }
            if state_id < ctx.visited.len() {
                ctx.visited[state_id] = ctx.generation;
            }

            let state = match self.nfa.get(thread.state) {
                Some(s) => s,
                None => continue,
            };

            // A codepoint class consumes input; rest here and let the caller do the
            // consume (so byte and codepoint consumption share one priority queue).
            if matches!(
                state.instruction,
                Some(NfaInstruction::CodepointClass(_, _))
            ) {
                ctx.current_threads.push(thread);
                continue;
            }

            if let Some(ref instruction) = state.instruction {
                let current_state_id = thread.state;
                match self.process_instruction(
                    instruction,
                    &mut thread,
                    input,
                    pos,
                    current_state_id,
                    &mut ctx.lookaround_cache,
                ) {
                    InstructionResult::Continue => {}
                    InstructionResult::Kill => continue,
                    InstructionResult::Jump(new_pos) => {
                        // Backref consumed text: schedule continuations at new_pos.
                        for &next_id in &state.epsilon {
                            let seq = ctx.seq_counter;
                            ctx.seq_counter += 1;
                            ctx.future_threads.push(PendingThread {
                                pos: new_pos,
                                seq,
                                thread: thread.clone_with_state(next_id),
                            });
                        }
                        continue;
                    }
                }
            }

            ctx.current_threads.push(thread.clone());

            // Reverse-order push so epsilon[0] (highest priority) is popped first.
            for &next_id in state.epsilon.iter().rev() {
                ctx.epsilon_stack.push(thread.clone_with_state(next_id));
            }
        }
    }

    /// Attempts to capture at a specific position (non-context version).
    fn captures_at(&self, input: &[u8], start: usize) -> Option<Vec<Option<(usize, usize)>>> {
        let mut ctx = self.create_context();
        self.captures_with_context(input, &mut ctx, start)
    }

    /// Process an NFA instruction and determine what to do with the thread.
    ///
    /// For lookaround instructions, uses memoization via the cache to avoid
    /// re-executing the same lookaround at the same position.
    fn process_instruction(
        &self,
        instruction: &NfaInstruction,
        thread: &mut Thread,
        input: &[u8],
        pos: usize,
        state_id: StateId,
        lookaround_cache: &mut std::collections::HashMap<(StateId, usize), bool>,
    ) -> InstructionResult {
        match instruction {
            NfaInstruction::CaptureStart(idx) => {
                thread.record_capture_start(*idx, pos);
                InstructionResult::Continue
            }
            NfaInstruction::CaptureEnd(idx) => {
                thread.record_capture_end(*idx, pos);
                InstructionResult::Continue
            }
            NfaInstruction::StartOfText => {
                if pos != 0 {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::EndOfText => {
                if crate::nfa::at_end_or_before_final_newline(input, pos) {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::StartOfLine => {
                if pos != 0 && input.get(pos - 1) != Some(&b'\n') {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::EndOfLine => {
                if pos != input.len() && input.get(pos) != Some(&b'\n') {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::WordBoundary => {
                if !self.is_word_boundary(input, pos) {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::NotWordBoundary => {
                if self.is_word_boundary(input, pos) {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::Backref(idx) => {
                if let Some((cap_start, cap_end)) = thread.get_capture(*idx) {
                    let cap_len = cap_end - cap_start;

                    // Empty capture - just continue (no text to match)
                    if cap_len == 0 {
                        return InstructionResult::Continue;
                    }

                    if pos + cap_len > input.len() {
                        return InstructionResult::Kill;
                    }
                    let captured = &input[cap_start..cap_end];
                    let current = &input[pos..pos + cap_len];
                    if captured != current {
                        return InstructionResult::Kill;
                    }
                    // Backref matched - jump to position after the matched text
                    InstructionResult::Jump(pos + cap_len)
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::PositiveLookahead(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = lookaround_cache.get(&(state_id, pos)) {
                    return if cached_result {
                        InstructionResult::Continue
                    } else {
                        InstructionResult::Kill
                    };
                }

                // A lookahead `(?=X)` requires X to match *anchored at* `pos`, not
                // merely somewhere ahead. Run the inner anchored at `pos` against the
                // full input so inner anchors (`\z`, `\b`) see the real context.
                let inner_vm = PikeVm::from_arc(Arc::clone(inner_nfa));
                let matched = inner_vm.match_at(input, pos).is_some();

                // Cache the result
                lookaround_cache.insert((state_id, pos), matched);

                if matched {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::NegativeLookahead(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = lookaround_cache.get(&(state_id, pos)) {
                    return if cached_result {
                        InstructionResult::Continue
                    } else {
                        InstructionResult::Kill
                    };
                }

                // `(?!X)` succeeds iff X does NOT match anchored at `pos`.
                let inner_vm = PikeVm::from_arc(Arc::clone(inner_nfa));
                let matched = inner_vm.match_at(input, pos).is_none();

                // Cache the result (true = lookaround succeeded, i.e., inner did NOT match)
                lookaround_cache.insert((state_id, pos), matched);

                if matched {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::PositiveLookbehind(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = lookaround_cache.get(&(state_id, pos)) {
                    return if cached_result {
                        InstructionResult::Continue
                    } else {
                        InstructionResult::Kill
                    };
                }

                // Arc::clone is O(1) - just increments reference count
                let inner_vm = PikeVm::from_arc(Arc::clone(inner_nfa));
                let mut found = false;
                for lookback_start in 0..=pos {
                    let slice = &input[lookback_start..pos];
                    // Check if inner pattern matches the entire slice (anchored)
                    if let Some((s, e)) = inner_vm.find(slice) {
                        if s == 0 && e == slice.len() {
                            found = true;
                            break;
                        }
                    }
                }

                // Cache the result
                lookaround_cache.insert((state_id, pos), found);

                if found {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::NegativeLookbehind(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = lookaround_cache.get(&(state_id, pos)) {
                    return if cached_result {
                        InstructionResult::Continue
                    } else {
                        InstructionResult::Kill
                    };
                }

                // Arc::clone is O(1) - just increments reference count
                let inner_vm = PikeVm::from_arc(Arc::clone(inner_nfa));
                let mut found = false;
                for lookback_start in 0..=pos {
                    let slice = &input[lookback_start..pos];
                    if let Some((s, e)) = inner_vm.find(slice) {
                        if s == 0 && e == slice.len() {
                            found = true;
                            break;
                        }
                    }
                }

                // Cache the result (true = lookaround succeeded, i.e., inner did NOT match)
                let result = !found;
                lookaround_cache.insert((state_id, pos), result);

                if result {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::NonGreedyExit => {
                // The non-greedy preference is encoded in the epsilon ordering, so
                // this marker is just a pass-through.
                InstructionResult::Continue
            }
            // A `CodepointClass` is a consuming state, handled directly in the
            // closure (`add_thread`) before `process_instruction` is ever called.
            NfaInstruction::CodepointClass(_, _) => {
                unreachable!("CodepointClass is consumed in the closure, not here")
            }
        }
    }

    /// Returns true if position is at a word boundary.
    fn is_word_boundary(&self, input: &[u8], pos: usize) -> bool {
        let prev_word = pos > 0 && is_word_byte(input[pos - 1]);
        let curr_word = pos < input.len() && is_word_byte(input[pos]);
        prev_word != curr_word
    }
}
