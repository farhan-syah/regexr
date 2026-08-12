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

use crate::nfa::{is_word_boundary, Nfa, NfaInstruction, StateId};
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
        self.find_from(input, 0)
    }

    /// Finds the leftmost match starting at or after `from`, returning (start, end).
    ///
    /// The whole input is passed to every attempt, so anchors, `\b`/`\B` and
    /// lookbehind see the bytes before `from` — unlike searching a slice that
    /// begins there.
    pub fn find_from(&self, input: &[u8], from: usize) -> Option<(usize, usize)> {
        let mut ctx = self.create_context();
        self.captures_unanchored_with_context(input, &mut ctx, from)
            .and_then(|caps| caps.first().copied().flatten())
    }

    /// Returns capture groups for the first match.
    pub fn captures(&self, input: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
        self.captures_from(input, 0)
    }

    /// Returns capture groups for the leftmost match starting at or after `from`.
    ///
    /// Keeps the full left context visible, like [`PikeVm::find_from`].
    pub fn captures_from(&self, input: &[u8], from: usize) -> Option<Vec<Option<(usize, usize)>>> {
        let mut ctx = self.create_context();
        self.captures_unanchored_with_context(input, &mut ctx, from)
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

    /// Returns true if *any* match of this NFA ends exactly at `end`.
    ///
    /// This is the question a lookbehind asks, and it is not the question
    /// [`PikeVm::match_at`] answers. `match_at` reports the single
    /// leftmost-greedy end for one start, so a pattern that could also stop
    /// earlier — `..?`, `\w\w?`, `a|ab` — hides every alternative end behind its
    /// preferred one.
    ///
    /// Starts are tried nearest-first against the FULL input, so an inner `\b`,
    /// `^` or nested lookbehind still sees the bytes to the left of that start —
    /// matching a detached slice would hide them.
    ///
    /// Only starts within [`Nfa::compute_max_match_len`] bytes of `end` can
    /// reach it, so a bounded inner pattern examines a constant number of them
    /// rather than every position in the input. Without that bound — an inner
    /// `a*`, `a.*b` or backreference — the scan is the whole prefix, which is
    /// what the assertion actually requires.
    pub fn matches_ending_at(&self, input: &[u8], end: usize) -> bool {
        if end > input.len() {
            return false;
        }
        let earliest = self
            .nfa
            .max_match_len
            .map_or(0, |max| end.saturating_sub(max));
        let mut ctx = self.create_context();
        (earliest..=end).rev().any(|start| {
            crate::nfa::is_utf8_boundary(input, start)
                && self.run_ending_at(input, &mut ctx, start, end)
        })
    }

    /// Anchored simulation that reports whether some path from `start` reaches a
    /// match state exactly at `end`.
    ///
    /// Unlike [`PikeVm::run`] this keeps every thread alive past a match: `run`
    /// drops threads of lower priority than a match because leftmost-greedy has
    /// already been decided there, but an existence question needs the ends that
    /// preference would have discarded. Threads scheduled beyond `end` are
    /// simply never visited, which also bounds the work to the span.
    fn run_ending_at(
        &self,
        input: &[u8],
        ctx: &mut PikeVmContext,
        start: usize,
        end: usize,
    ) -> bool {
        ctx.reset();
        ctx.ensure_state_capacity(self.nfa.states.len());

        self.seed_start_thread(ctx, start, self.nfa.capture_count as usize);

        let mut sched: Vec<(usize, Thread)> = Vec::new();
        let mut pos = start;

        while pos <= end {
            if matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                // Fresh generation for per-position state deduplication.
                ctx.generation = ctx.generation.wrapping_add(1);
                ctx.current_threads.clear();

                while matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                    let pt = ctx.future_threads.pop().unwrap();
                    self.add_thread(ctx, pt.thread, input, pos);
                }

                if pos == end
                    && ctx
                        .current_threads
                        .iter()
                        .any(|t| self.nfa.get(t.state).map(|s| s.is_match).unwrap_or(false))
                {
                    return true;
                }

                self.schedule_consuming_successors(&ctx.current_threads, input, pos, &mut sched);
                for (npos, th) in sched.drain(..) {
                    // A successor past `end` can never end at `end`.
                    if npos > end {
                        continue;
                    }
                    let seq = ctx.seq_counter;
                    ctx.seq_counter += 1;
                    ctx.future_threads.push(PendingThread {
                        pos: npos,
                        seq,
                        thread: th,
                    });
                }
            }

            // Threads scheduled back onto `pos` (zero-width backref jump) get
            // another round before the position advances.
            if matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                continue;
            }

            match ctx.future_threads.peek().map(|pt| pt.pos) {
                Some(next) => pos = next,
                None => break,
            }
        }

        false
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
        PikeVmContext::new(self.nfa.states.len())
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

    /// Returns capture groups using a pre-allocated context, for a match that
    /// must begin exactly at `start_pos`.
    ///
    /// This is the *anchored* entry point: no other start position is tried, so
    /// a pattern whose match begins later reports no match. Use
    /// [`PikeVm::captures_unanchored_with_context`] to search.
    pub fn captures_with_context(
        &self,
        input: &[u8],
        ctx: &mut PikeVmContext,
        start_pos: usize,
    ) -> Option<Vec<Option<(usize, usize)>>> {
        self.run(input, ctx, start_pos, false)
    }

    /// Returns capture groups for the leftmost match starting at or after `from`,
    /// in a **single pass** over the input.
    ///
    /// Rather than restarting the whole simulation at every candidate position —
    /// which costs O(n) passes over an n-byte input — this seeds one fresh start
    /// thread per position into the same priority queue. Threads seeded earlier
    /// carry a smaller `seq` and therefore outrank later ones, which is exactly
    /// leftmost-first: a match from an earlier start always preempts a later one.
    ///
    /// A match beginning at `from` short-circuits that sweep entirely, since no
    /// match can start earlier. A backreference pattern is the one case that
    /// cannot use the sweep at all: see below.
    pub fn captures_unanchored_with_context(
        &self,
        input: &[u8],
        ctx: &mut PikeVmContext,
        from: usize,
    ) -> Option<Vec<Option<(usize, usize)>>> {
        if from > input.len() {
            return None;
        }

        // The single pass relies on per-position deduplication by NFA state:
        // when two threads reach the same state, the lower-priority one is
        // dropped as redundant. A backreference breaks that premise — what a
        // thread matches next depends on the text it captured, not just on its
        // state — so two threads in the same state with different capture
        // histories are *not* interchangeable. With one start position in flight
        // that rarely bites, but seeding every position puts threads from many
        // starts in the same generation and the wrong one wins. Restart per
        // position for these. (Backreference patterns normally go to the
        // backtracking engine; the PikeVM only sees them as a fallback.)
        if self.nfa.has_backrefs {
            for start in from..=input.len() {
                if !crate::nfa::is_utf8_boundary(input, start) {
                    continue;
                }
                if let Some(caps) = self.run(input, ctx, start, false) {
                    return Some(caps);
                }
            }
            return None;
        }

        // A match that starts exactly at `from` is the leftmost one there can
        // be, so settle that with a single anchored run before paying for the
        // sweep. The sweep seeds a start thread at every position it advances
        // over until a match registers, and a greedy pattern only registers one
        // after consuming its whole run — so on input that matches at nearly
        // every position, which is what a tokenizer's split pattern does, those
        // seeds are all redundant and all simulated. Failing here costs one
        // anchored attempt that the sweep would have made as its first thread
        // anyway.
        if crate::nfa::is_utf8_boundary(input, from) {
            if let Some(caps) = self.run(input, ctx, from, false) {
                return Some(caps);
            }
        }
        if from == input.len() {
            return None;
        }
        self.run(input, ctx, from + 1, true)
    }

    /// Schedules a fresh start thread at `pos`.
    ///
    /// Called when the main loop has reached `pos`, so every thread already
    /// queued for `pos` was scheduled on an earlier iteration and holds a smaller
    /// `seq`. The new thread is therefore the lowest-priority one at `pos`.
    fn seed_start_thread(&self, ctx: &mut PikeVmContext, pos: usize, capture_count: usize) {
        let seq = ctx.seq_counter;
        ctx.seq_counter += 1;
        ctx.future_threads.push(PendingThread {
            pos,
            seq,
            thread: Thread::new(self.nfa.start, capture_count, pos),
        });
    }

    /// The thread simulation shared by the anchored and unanchored entry points.
    ///
    /// All threads — whether they advance by a byte or by a multi-byte codepoint —
    /// flow through a single priority-ordered queue keyed on (position, seq).
    /// `seq` is assigned in priority order as threads are scheduled, so at any
    /// position the highest-priority thread is processed first. Greedy vs
    /// non-greedy is encoded entirely in the NFA's epsilon ordering (preserved by
    /// the closure), so no special non-greedy bookkeeping is needed here.
    ///
    /// When `unanchored`, a start thread is seeded at every codepoint boundary
    /// from `from` onwards until a match exists; after that no later start could
    /// win, so seeding stops and the loop only visits positions that still hold
    /// threads.
    fn run(
        &self,
        input: &[u8],
        ctx: &mut PikeVmContext,
        from: usize,
        unanchored: bool,
    ) -> Option<Vec<Option<(usize, usize)>>> {
        ctx.reset();
        ctx.ensure_state_capacity(self.nfa.states.len());

        let capture_count = self.nfa.capture_count as usize;

        if !unanchored {
            self.seed_start_thread(ctx, from, capture_count);
        }

        // The winning thread and the position it matched to, rather than its
        // reconstructed captures. A match is re-established at every position a
        // greedy run extends over, so reconstructing here would walk the capture
        // list and allocate once per byte of the match; cloning the thread is an
        // `Arc` bump. Reconstruction happens once, after the loop.
        let mut matched: Option<(Thread, usize)> = None;

        // Buffer for the consuming successors scheduled at each position.
        let mut sched: Vec<(usize, Thread)> = Vec::new();

        let mut pos = from;
        // Guards against re-seeding a position the loop revisits because threads
        // were scheduled back onto it (a zero-width backreference jump).
        let mut seeded_at: Option<usize> = None;

        while pos <= input.len() {
            if unanchored
                && matched.is_none()
                && seeded_at != Some(pos)
                // Only start at UTF-8 codepoint boundaries (see `is_utf8_boundary`).
                && crate::nfa::is_utf8_boundary(input, pos)
            {
                self.seed_start_thread(ctx, pos, capture_count);
                seeded_at = Some(pos);
            }

            if matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                // Fresh generation for per-position state deduplication.
                ctx.generation = ctx.generation.wrapping_add(1);
                ctx.current_threads.clear();

                // Drain every thread scheduled for this position. The heap yields
                // them in (seq) priority order; the epsilon closure expands each
                // into `current_threads`, preserving that order.
                while matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                    let pt = ctx.future_threads.pop().unwrap();
                    self.add_thread(ctx, pt.thread, input, pos);
                }

                // The first (highest-priority) thread to reach a match wins at
                // this position and preempts every lower-priority thread. A
                // higher-priority thread that is still consuming may overwrite
                // this match at a later position (it comes from a more-preferred
                // path — and, unanchored, from a start at or before this one).
                let match_idx = ctx
                    .current_threads
                    .iter()
                    .position(|t| self.nfa.get(t.state).map(|s| s.is_match).unwrap_or(false));
                if let Some(i) = match_idx {
                    matched = Some((ctx.current_threads[i], pos));
                }
                // Only threads strictly higher priority than the match advance.
                let limit = match_idx.unwrap_or(ctx.current_threads.len());

                self.schedule_consuming_successors(
                    &ctx.current_threads[..limit],
                    input,
                    pos,
                    &mut sched,
                );
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

            // Threads scheduled back onto `pos` (zero-width backref jump) get
            // another round before the position advances.
            if matches!(ctx.future_threads.peek(), Some(pt) if pt.pos == pos) {
                continue;
            }

            // While still seeding, every position must be visited. Otherwise only
            // positions that actually hold threads matter, so skip ahead.
            if unanchored && matched.is_none() {
                pos += 1;
            } else {
                match ctx.future_threads.peek().map(|pt| pt.pos) {
                    Some(next) => pos = next,
                    None => break,
                }
            }
        }

        matched.map(|(thread, end)| {
            let mut caps = thread.reconstruct_captures(&ctx.capture_arena);
            caps[0] = Some((thread.start, end));
            caps
        })
    }

    /// Collects the consuming successors of `threads` at `pos` into `sched`.
    ///
    /// A state consumes either a byte (its byte-range transitions) or a whole
    /// codepoint (`CodepointClass`); both land in the same buffer so the caller
    /// can assign `seq` to them in one pass and keep the queue ordered by
    /// pattern priority. `threads` is a slice rather than the whole thread list
    /// because the leftmost-greedy search passes only the prefix that outranks
    /// the match it just found, while an existence search passes all of them.
    #[inline]
    fn schedule_consuming_successors(
        &self,
        threads: &[Thread],
        input: &[u8],
        pos: usize,
        sched: &mut Vec<(usize, Thread)>,
    ) {
        let byte = input.get(pos).copied();
        sched.clear();
        for thread in threads {
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
                    ctx,
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

            ctx.current_threads.push(thread);

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
        ctx: &mut PikeVmContext,
    ) -> InstructionResult {
        match instruction {
            NfaInstruction::CaptureStart(idx) => {
                thread.record_capture_start(&mut ctx.capture_arena, *idx, pos);
                InstructionResult::Continue
            }
            NfaInstruction::CaptureEnd(idx) => {
                thread.record_capture_end(&mut ctx.capture_arena, *idx, pos);
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
                if crate::nfa::at_line_start(input, pos) {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::EndOfLine => {
                if crate::nfa::at_line_end(input, pos) {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::WordBoundary => {
                if !is_word_boundary(input, pos) {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::NotWordBoundary => {
                if is_word_boundary(input, pos) {
                    InstructionResult::Kill
                } else {
                    InstructionResult::Continue
                }
            }
            NfaInstruction::Backref(idx) => {
                if let Some((cap_start, cap_end)) = thread.get_capture(&ctx.capture_arena, *idx) {
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
                if let Some(&cached_result) = ctx.lookaround_cache.get(&(state_id, pos)) {
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
                ctx.lookaround_cache.insert((state_id, pos), matched);

                if matched {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::NegativeLookahead(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = ctx.lookaround_cache.get(&(state_id, pos)) {
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
                ctx.lookaround_cache.insert((state_id, pos), matched);

                if matched {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::PositiveLookbehind(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = ctx.lookaround_cache.get(&(state_id, pos)) {
                    return if cached_result {
                        InstructionResult::Continue
                    } else {
                        InstructionResult::Kill
                    };
                }

                // Arc::clone is O(1) - just increments reference count
                let inner_vm = PikeVm::from_arc(Arc::clone(inner_nfa));
                // A lookbehind `(?<=X)` requires X to end exactly at `pos` — for
                // *some* path through X, not just X's preferred one.
                let found = inner_vm.matches_ending_at(input, pos);

                // Cache the result
                ctx.lookaround_cache.insert((state_id, pos), found);

                if found {
                    InstructionResult::Continue
                } else {
                    InstructionResult::Kill
                }
            }
            NfaInstruction::NegativeLookbehind(inner_nfa) => {
                // Check memoization cache first
                if let Some(&cached_result) = ctx.lookaround_cache.get(&(state_id, pos)) {
                    return if cached_result {
                        InstructionResult::Continue
                    } else {
                        InstructionResult::Kill
                    };
                }

                // Arc::clone is O(1) - just increments reference count
                let inner_vm = PikeVm::from_arc(Arc::clone(inner_nfa));
                // Same as the positive form: `(?<!X)` fails iff some path through
                // X ends exactly at `pos`.
                let found = inner_vm.matches_ending_at(input, pos);

                // Cache the result (true = lookaround succeeded, i.e., inner did NOT match)
                let result = !found;
                ctx.lookaround_cache.insert((state_id, pos), result);

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
}
