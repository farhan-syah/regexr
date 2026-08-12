//! Shared types for PikeVM execution.
//!
//! Contains thread management structures and utilities used by the interpreter.
//!
//! # Shared capture histories
//!
//! A thread does not carry a capture vector. It carries an index into the run's
//! [`CaptureArena`], naming the most recent capture action on its path; each
//! node links to the previous one. Forking a thread copies that index, so thread
//! creation is O(1) with no allocation and no reference counting, and the full
//! capture `Vec` is built only once a match is final.

use crate::nfa::StateId;
use std::collections::{BinaryHeap, HashMap};

/// Thread scheduled for a future input position.
///
/// Threads are popped in `(pos, start, seq)` ascending order. `start` is where
/// the thread's match attempt began, which is what makes the queue leftmost-first
/// across both fixed-width (byte) and variable-width (codepoint) transitions.
/// `seq` is a monotonic counter assigned in priority order when a thread is
/// scheduled, so among threads that began together the higher-priority (lower
/// `seq`) one is processed first.
#[derive(Debug)]
pub struct PendingThread {
    pub pos: usize,
    pub seq: u64,
    pub thread: Thread,
}

impl PartialEq for PendingThread {
    fn eq(&self, other: &Self) -> bool {
        self.pos == other.pos && self.thread.start == other.thread.start && self.seq == other.seq
    }
}

impl Eq for PendingThread {}

impl PartialOrd for PendingThread {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PendingThread {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap is a max-heap but we want the smallest
        // `(pos, start, seq)` first, so every comparison is reversed: position
        // ascending, then start position ascending, then seq (priority)
        // ascending.
        //
        // `start` has to outrank `seq`, because `seq` is reassigned every time a
        // thread is rescheduled and therefore measures how recently it moved, not
        // how early it began. A thread advancing one byte at a time collects a
        // larger `seq` than one that crossed the same span in a single step — and
        // `CodepointClass` consumes up to four bytes at once — so ordering on
        // `seq` alone hands the match to whichever thread took bigger strides.
        // Comparing `start` first makes leftmost-first hold regardless of stride;
        // `seq` still resolves pattern priority between threads that began
        // together, since threads are rescheduled in priority order.
        other
            .pos
            .cmp(&self.pos)
            .then_with(|| other.thread.start.cmp(&self.thread.start))
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

/// Pre-allocated execution context for PikeVM.
/// Reusing this context across multiple captures() calls avoids repeated allocations.
#[derive(Debug)]
pub struct PikeVmContext {
    /// Thread storage for the position currently being processed.
    pub current_threads: Vec<Thread>,
    /// Threads scheduled for future positions, ordered by
    /// `(position, start, seq)`. All consuming transitions (byte and codepoint)
    /// flow through this single queue.
    pub future_threads: BinaryHeap<PendingThread>,
    /// O(1) deduplication: `visited[state_id] == generation` means state already visited
    pub visited: Vec<usize>,
    /// Current generation counter (incremented per position/step)
    pub generation: usize,
    /// Monotonic priority sequence assigned to scheduled threads. Lower = higher
    /// priority. Assigned in priority order so the queue stays leftmost-first.
    pub seq_counter: u64,
    /// Stack for iterative epsilon closure (avoids recursion stack overflow)
    pub epsilon_stack: Vec<Thread>,
    /// Memoization cache for lookaround results.
    /// Key: (state_id with lookaround instruction, position in input)
    /// Value: whether the lookaround matched
    /// This avoids re-executing the same lookaround at the same position.
    pub lookaround_cache: HashMap<(StateId, usize), bool>,
    /// Capture history for the run in progress. Threads index into this.
    pub capture_arena: CaptureArena,
}

impl PikeVmContext {
    /// Create a new context sized for an NFA with `state_count` states.
    ///
    /// Captures need no storage here: a thread carries its own history as a
    /// shared linked list of capture actions, and the slot vector is built only
    /// when a match is reconstructed.
    pub fn new(state_count: usize) -> Self {
        Self {
            current_threads: Vec::with_capacity(32),
            future_threads: BinaryHeap::new(),
            visited: vec![0; state_count],
            generation: 0,
            seq_counter: 0,
            epsilon_stack: Vec::with_capacity(32),
            lookaround_cache: HashMap::new(),
            capture_arena: Vec::with_capacity(64),
        }
    }

    /// Reset the context for a new match attempt.
    #[inline]
    pub fn reset(&mut self) {
        self.current_threads.clear();
        self.future_threads.clear();
        self.epsilon_stack.clear();
        // Don't clear visited or reset generation - the sparse set approach
        // relies on keeping generation incrementing to invalidate old entries.
        // Just increment once to ensure fresh start
        self.generation = self.generation.wrapping_add(1);
        self.seq_counter = 0;
        // Clear lookaround cache for new match attempt
        self.lookaround_cache.clear();
        // Keeps the capacity, so a repeated captures() call does not allocate.
        self.capture_arena.clear();
    }

    /// Ensure visited array is large enough for the given state count.
    #[inline]
    pub fn ensure_state_capacity(&mut self, state_count: usize) {
        if self.visited.len() < state_count {
            self.visited.resize(state_count, 0);
        }
    }
}

/// A capture action in the linked list.
/// Each action records a single capture start or end event.
#[derive(Debug, Clone)]
pub enum CaptureAction {
    /// Start of capture group at position
    Start(u32, usize),
    /// End of capture group at position
    End(u32, usize),
}

/// A node in the capture history list.
///
/// Nodes live in a per-run arena ([`CaptureArena`]) and refer to their parent by
/// index, so forking a thread copies one `u32` — no allocation, no reference
/// counting. Threads share history simply by naming the same node.
#[derive(Debug, Clone)]
pub struct CaptureNode {
    /// The capture action at this node
    pub action: CaptureAction,
    /// Index of the previous action in the arena, if any.
    pub parent: Option<u32>,
}

/// Arena holding one run's capture history.
///
/// Append-only for the length of a run, so an index stays valid once handed
/// out. Cleared by [`PikeVmContext::reset`] and its capacity reused, which is
/// what keeps a repeated `captures()` call off the allocator entirely.
pub type CaptureArena = Vec<CaptureNode>;

/// A thread in the PikeVM.
///
/// Capture history is shared structurally: a thread names the head of a list
/// held in the run's [`CaptureArena`], so forking copies a `u32` rather than a
/// capture vector. The full capture `Vec` is built only once a match is final.
#[derive(Debug, Clone, Copy)]
pub struct Thread {
    /// Current NFA state.
    pub state: StateId,
    /// Index of this thread's most recent capture action in the arena.
    /// `None` means no captures have been recorded yet.
    pub capture_head: Option<u32>,
    /// Number of capture groups (needed for reconstruction).
    pub capture_count: usize,
    /// Input position where this thread's match attempt began — capture slot 0's
    /// start. An unanchored pass keeps threads from many different start
    /// positions in flight at once, so the start cannot be a property of the run.
    pub start: usize,
    /// Bytes of a codepoint transition still to be crossed before this thread
    /// arrives in `state`. Zero means it is on a codepoint boundary and behaves
    /// normally; while non-zero the thread occupies `state` but is not expanded
    /// and cannot match.
    ///
    /// A `CodepointClass` consumes up to four bytes, and crossing them in one
    /// scheduling step let a thread overtake byte-wise threads walking the same
    /// span — `seq` measures scheduling order, so bigger strides collected
    /// smaller `seq` and won a priority contest they should have lost. Walking
    /// the codepoint one byte at a time keeps every thread on the same clock.
    ///
    /// Kept as a single byte alongside `state` rather than as a separate pending
    /// target so the struct stays its original size: threads are copied on every
    /// scheduling step, and widening them costs more than the fix saves.
    pub pending_skip: u8,
}

impl Thread {
    /// Create a new thread with no capture history, beginning at `start`.
    #[inline]
    pub fn new(state: StateId, capture_count: usize, start: usize) -> Self {
        Self {
            state,
            capture_head: None,
            capture_count,
            start,
            pending_skip: 0,
        }
    }

    /// Copies this thread with a new state. O(1): the capture history is an
    /// index, so nothing is allocated and no refcount is touched.
    #[inline]
    pub fn clone_with_state(&self, state: StateId) -> Self {
        Self {
            state,
            pending_skip: 0,
            ..*self
        }
    }

    /// Copies this thread as one part-way through a codepoint: it takes up
    /// `target` immediately but idles there for `skip` further bytes.
    #[inline]
    pub fn clone_mid_codepoint(&self, target: StateId, skip: u8) -> Self {
        Self {
            state: target,
            pending_skip: skip,
            ..*self
        }
    }

    /// Advances a mid-codepoint thread by one byte; it becomes a normal thread
    /// once the last byte is crossed.
    #[inline]
    pub fn step_mid_codepoint(&self) -> Self {
        debug_assert!(self.pending_skip > 0);
        Self {
            pending_skip: self.pending_skip - 1,
            ..*self
        }
    }

    /// Records a capture start event. O(1) amortised — one arena push.
    #[inline]
    pub fn record_capture_start(&mut self, arena: &mut CaptureArena, group_idx: u32, pos: usize) {
        self.push_action(arena, CaptureAction::Start(group_idx, pos));
    }

    /// Records a capture end event. O(1) amortised — one arena push.
    #[inline]
    pub fn record_capture_end(&mut self, arena: &mut CaptureArena, group_idx: u32, pos: usize) {
        self.push_action(arena, CaptureAction::End(group_idx, pos));
    }

    /// Appends an action to the arena and points this thread at it.
    ///
    /// An arena longer than `u32::MAX` would make the index ambiguous; the
    /// action is dropped rather than corrupting the history, which can only
    /// lose captures on an input far past any practical size.
    #[inline]
    fn push_action(&mut self, arena: &mut CaptureArena, action: CaptureAction) {
        let Ok(index) = u32::try_from(arena.len()) else {
            return;
        };
        arena.push(CaptureNode {
            action,
            parent: self.capture_head,
        });
        self.capture_head = Some(index);
    }

    /// Reconstructs the full capture Vec from the arena.
    /// Called only once a match is final. O(depth) in the number of actions.
    pub fn reconstruct_captures(&self, arena: &CaptureArena) -> Vec<Option<(usize, usize)>> {
        let mut captures = vec![None; self.capture_count + 1];

        // Walk the history backwards to collect all actions
        let mut actions = Vec::new();
        let mut current = self.capture_head;
        while let Some(index) = current {
            let Some(node) = arena.get(index as usize) else {
                break;
            };
            actions.push(&node.action);
            current = node.parent;
        }

        // Process actions in reverse order (oldest first) to build final capture state
        // For each group, we want the LAST (most recent) start and end positions
        // But we process oldest-first, so each new value overwrites the old
        for action in actions.into_iter().rev() {
            match action {
                CaptureAction::Start(idx, pos) => {
                    let idx = *idx as usize;
                    if idx < captures.len() {
                        // Start a new capture - set start position, end will be set later
                        captures[idx] = Some((*pos, *pos));
                    }
                }
                CaptureAction::End(idx, pos) => {
                    let idx = *idx as usize;
                    if idx < captures.len() {
                        if let Some((start, _)) = captures[idx] {
                            captures[idx] = Some((start, *pos));
                        }
                    }
                }
            }
        }

        captures
    }

    /// Get a capture group value by walking the linked list.
    /// Used for backref matching - more efficient than full reconstruction.
    /// Returns None if capture group is not set or incomplete.
    pub fn get_capture(&self, arena: &CaptureArena, group_idx: u32) -> Option<(usize, usize)> {
        let mut start: Option<usize> = None;
        let mut end: Option<usize> = None;

        // Walk backwards to find the most recent start and end for this group
        let mut current = self.capture_head;
        while let Some(index) = current {
            let Some(node) = arena.get(index as usize) else {
                break;
            };
            match &node.action {
                CaptureAction::Start(idx, pos) if *idx == group_idx && start.is_none() => {
                    start = Some(*pos);
                }
                CaptureAction::End(idx, pos) if *idx == group_idx && end.is_none() => {
                    end = Some(*pos);
                }
                _ => {}
            }
            // Early exit if we found both
            if start.is_some() && end.is_some() {
                break;
            }
            current = node.parent;
        }

        match (start, end) {
            (Some(s), Some(e)) => Some((s, e)),
            _ => None,
        }
    }
}

/// Result of processing an instruction during epsilon closure.
pub enum InstructionResult {
    /// Continue with epsilon transitions at the current position.
    Continue,
    /// Thread should be killed (assertion failed).
    Kill,
    /// Thread should jump to a different position (for backrefs).
    Jump(usize),
}

/// Decodes a single UTF-8 codepoint from a byte slice.
/// Returns the codepoint value and its length in bytes, or None if invalid UTF-8.
#[inline]
pub fn decode_utf8_codepoint(bytes: &[u8]) -> Option<(u32, usize)> {
    if bytes.is_empty() {
        return None;
    }

    let first = bytes[0];
    if first < 0x80 {
        // ASCII: 1 byte
        return Some((first as u32, 1));
    }

    if first < 0xC0 {
        // Invalid: continuation byte as first byte
        return None;
    }

    if first < 0xE0 {
        // 2-byte sequence: 110xxxxx 10xxxxxx
        if bytes.len() < 2 {
            return None;
        }
        let b1 = bytes[1];
        if (b1 & 0xC0) != 0x80 {
            return None;
        }
        let cp = ((first as u32 & 0x1F) << 6) | (b1 as u32 & 0x3F);
        return Some((cp, 2));
    }

    if first < 0xF0 {
        // 3-byte sequence: 1110xxxx 10xxxxxx 10xxxxxx
        if bytes.len() < 3 {
            return None;
        }
        let b1 = bytes[1];
        let b2 = bytes[2];
        if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 {
            return None;
        }
        let cp = ((first as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6) | (b2 as u32 & 0x3F);
        return Some((cp, 3));
    }

    if first < 0xF8 {
        // 4-byte sequence: 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx
        if bytes.len() < 4 {
            return None;
        }
        let b1 = bytes[1];
        let b2 = bytes[2];
        let b3 = bytes[3];
        if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 || (b3 & 0xC0) != 0x80 {
            return None;
        }
        let cp = ((first as u32 & 0x07) << 18)
            | ((b1 as u32 & 0x3F) << 12)
            | ((b2 as u32 & 0x3F) << 6)
            | (b3 as u32 & 0x3F);
        return Some((cp, 4));
    }

    // Invalid: first byte > 0xF7
    None
}
