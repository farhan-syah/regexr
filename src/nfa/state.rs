//! NFA state definitions.

use crate::hir::CodepointClass;
use std::collections::BTreeSet;
use std::sync::Arc;

/// A state identifier.
pub type StateId = u32;

/// An NFA (Nondeterministic Finite Automaton).
///
/// Marked `#[non_exhaustive]`: the analyses this carries grow as engines learn
/// to ask new questions of a pattern, and each new field would otherwise break
/// every caller constructing one with a struct literal. Build with [`Nfa::new`]
/// and fill the fields in, which is what the builder already does.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Nfa {
    /// All states in the NFA.
    pub states: Vec<NfaState>,
    /// The start state.
    pub start: StateId,
    /// Match states.
    pub matches: Vec<StateId>,
    /// Number of capture groups.
    pub capture_count: u32,
    /// Whether the pattern has backreferences.
    pub has_backrefs: bool,
    /// Whether the pattern has lookarounds.
    pub has_lookaround: bool,
    /// Precomputed epsilon closures for each state (optional optimization).
    /// When present, `epsilon_closure()` uses these instead of computing on-the-fly.
    pub epsilon_closures: Option<Vec<BTreeSet<StateId>>>,
    /// Whether any byte transition can match a UTF-8 continuation byte, and so
    /// leave a thread part-way through a multi-byte codepoint. See
    /// [`Nfa::compute_splits_codepoints`].
    ///
    /// Defaults to `true`: a construction path that never fills this in gets the
    /// careful behaviour, not the fast one.
    pub splits_codepoints: bool,
    /// Upper bound, in bytes, on what a match of this NFA can consume — `None`
    /// when unbounded. See [`Nfa::compute_max_match_len`].
    ///
    /// Consumers must read `None` as "no bound available" and stay correct
    /// without one, so a construction path that never fills this in only costs
    /// speed.
    pub max_match_len: Option<usize>,
}

impl Nfa {
    /// Creates a new empty NFA.
    pub fn new() -> Self {
        Self {
            states: Vec::new(),
            start: 0,
            matches: Vec::new(),
            capture_count: 0,
            has_backrefs: false,
            has_lookaround: false,
            epsilon_closures: None,
            splits_codepoints: true,
            max_match_len: None,
        }
    }

    /// The most bytes any match of this NFA can consume, or `None` if that is
    /// unbounded.
    ///
    /// A lookbehind uses this to bound its search: `(?<=X)` at position `p` only
    /// has to consider starts in `p - max_match_len ..= p`, which turns an
    /// O(p)-starts scan into O(1) for the bounded inner patterns that nearly all
    /// lookbehinds use. Being an *upper* bound is what makes it safe — a bound
    /// that is too large only costs time.
    ///
    /// This is the longest path from the start state to a match state, counting
    /// one byte per byte transition and four (the longest UTF-8 encoding) per
    /// codepoint class. Any cycle reachable from the start, and any
    /// backreference (whose length depends on captured text, not on the graph),
    /// makes the answer unbounded.
    ///
    /// Iterative rather than recursive: state counts follow pattern size, and a
    /// bounded repeat like `a{10000}` would otherwise recurse as deep.
    pub fn compute_max_match_len(&self) -> Option<usize> {
        const WHITE: u8 = 0;
        const GRAY: u8 = 1;
        const BLACK: u8 = 2;

        let n = self.states.len();
        if n == 0 {
            return Some(0);
        }
        if self
            .states
            .iter()
            .any(|s| matches!(s.instruction, Some(NfaInstruction::Backref(_))))
        {
            return None;
        }

        let mut color = vec![WHITE; n];
        // `best[s]`: the longest byte path from `s` to a match state, or `None`
        // when no match state is reachable from it.
        let mut best: Vec<Option<usize>> = vec![None; n];

        let start = self.start as usize;
        if start >= n {
            return Some(0);
        }
        color[start] = GRAY;
        // (state, index of the next outgoing edge to visit)
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];

        while let Some(&(sid, edge)) = stack.last() {
            let state = &self.states[sid];
            match Self::successor(state, edge) {
                Some((target, weight)) => {
                    let target = target as usize;
                    if target >= n {
                        stack.last_mut().unwrap().1 = edge + 1;
                        continue;
                    }
                    match color[target] {
                        // Back edge: the graph loops, so the length is unbounded.
                        GRAY => return None,
                        BLACK => {
                            if let Some(reachable) = best[target] {
                                let candidate = reachable.saturating_add(weight);
                                best[sid] = Some(best[sid].map_or(candidate, |b| b.max(candidate)));
                            }
                            stack.last_mut().unwrap().1 = edge + 1;
                        }
                        // Descend, leaving `edge` in place so this child's value
                        // is folded in when the walk returns to it.
                        _ => {
                            color[target] = GRAY;
                            stack.push((target, 0));
                        }
                    }
                }
                None => {
                    // A match state can stop here, consuming nothing more.
                    if state.is_match {
                        best[sid] = Some(best[sid].unwrap_or(0));
                    }
                    color[sid] = BLACK;
                    stack.pop();
                }
            }
        }

        best[start]
    }

    /// The `edge`-th outgoing edge of `state` as `(target, bytes consumed)`,
    /// or `None` once they are exhausted. Byte transitions come first, then
    /// epsilons, then the codepoint-class transition an instruction may carry.
    fn successor(state: &NfaState, edge: usize) -> Option<(StateId, usize)> {
        if let Some(&(_, target)) = state.transitions.get(edge) {
            return Some((target, 1));
        }
        let edge = edge - state.transitions.len();
        if let Some(&target) = state.epsilon.get(edge) {
            return Some((target, 0));
        }
        if edge == state.epsilon.len() {
            if let Some(NfaInstruction::CodepointClass(_, target)) = &state.instruction {
                // The widest UTF-8 encoding; an upper bound is all this needs.
                return Some((*target, 4));
            }
        }
        None
    }

    /// Whether matching this NFA depends on the text to the left of the start
    /// position — an anchor to the text/line start, a word boundary, or a
    /// lookbehind.
    ///
    /// An engine that searches by handing a *slice* beginning at the candidate
    /// position must not do so when this holds: the slice's first byte would
    /// look like the start of the text and like the start of a word.
    ///
    /// Lookaround interiors count. They are held behind `Arc` and so are absent
    /// from `states`, but `(?=^)x` and `(?=\b)x` read left context just as
    /// surely as a bare `^` or `\b` does.
    pub fn needs_left_context(&self) -> bool {
        self.states.iter().any(|s| match s.instruction {
            Some(
                NfaInstruction::StartOfText
                | NfaInstruction::StartOfLine
                | NfaInstruction::WordBoundary
                | NfaInstruction::NotWordBoundary
                | NfaInstruction::PositiveLookbehind(_)
                | NfaInstruction::NegativeLookbehind(_),
            ) => true,
            Some(
                NfaInstruction::PositiveLookahead(ref inner)
                | NfaInstruction::NegativeLookahead(ref inner),
            ) => inner.needs_left_context(),
            _ => false,
        })
    }

    /// Whether a byte transition in this NFA can match a UTF-8 continuation
    /// byte (`0x80..=0xBF`), which is what lets a thread sit part-way through a
    /// multi-byte codepoint.
    ///
    /// Matching only ever starts on a codepoint boundary and a codepoint-class
    /// transition lands on one, so when this is false every thread is always on
    /// a boundary — and a codepoint transition may then cross its bytes in a
    /// single step without any byte-wise thread to keep in sync with.
    pub fn compute_splits_codepoints(&self) -> bool {
        self.states.iter().any(|s| {
            s.transitions
                .iter()
                .any(|(range, _)| range.start <= 0xBF && range.end >= 0x80)
        })
    }

    /// Adds a new state and returns its ID.
    pub fn add_state(&mut self, state: NfaState) -> StateId {
        let id = self.states.len() as StateId;
        self.states.push(state);
        id
    }

    /// Returns the number of states.
    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    /// Gets a state by ID.
    pub fn get(&self, id: StateId) -> Option<&NfaState> {
        self.states.get(id as usize)
    }

    /// Gets a mutable state by ID.
    pub fn get_mut(&mut self, id: StateId) -> Option<&mut NfaState> {
        self.states.get_mut(id as usize)
    }

    /// Computes the epsilon closure of a set of states.
    pub fn epsilon_closure(&self, states: &BTreeSet<StateId>) -> BTreeSet<StateId> {
        // Fast path: if we have precomputed closures, use them
        if let Some(ref precomputed) = self.epsilon_closures {
            let mut closure = BTreeSet::new();
            for &state_id in states {
                if let Some(state_closure) = precomputed.get(state_id as usize) {
                    closure.extend(state_closure.iter().copied());
                }
            }
            return closure;
        }

        // Slow path: compute epsilon closure on the fly
        let mut closure = states.clone();
        let mut stack: Vec<StateId> = states.iter().copied().collect();

        while let Some(state_id) = stack.pop() {
            if let Some(state) = self.get(state_id) {
                for &next in &state.epsilon {
                    if closure.insert(next) {
                        stack.push(next);
                    }
                }
            }
        }

        closure
    }

    /// Precomputes epsilon closures for all states.
    /// This significantly speeds up DFA construction for NFAs with many epsilon transitions.
    pub fn precompute_epsilon_closures(&mut self) {
        // Count epsilon transitions to decide if precomputation is worthwhile
        let epsilon_count: usize = self.states.iter().map(|s| s.epsilon.len()).sum();
        if epsilon_count < 100 {
            // Not enough epsilon transitions to justify precomputation
            return;
        }

        let mut closures = Vec::with_capacity(self.states.len());

        for state_id in 0..self.states.len() {
            let mut closure = BTreeSet::new();
            closure.insert(state_id as StateId);

            let mut stack = vec![state_id as StateId];
            while let Some(sid) = stack.pop() {
                if let Some(state) = self.get(sid) {
                    for &next in &state.epsilon {
                        if closure.insert(next) {
                            stack.push(next);
                        }
                    }
                }
            }

            closures.push(closure);
        }

        self.epsilon_closures = Some(closures);
    }
}

impl Default for Nfa {
    fn default() -> Self {
        Self::new()
    }
}

/// A single NFA state.
#[derive(Debug, Clone)]
pub struct NfaState {
    /// Byte transitions: (byte_range, target_state).
    pub transitions: Vec<(ByteRange, StateId)>,
    /// Epsilon (empty) transitions.
    pub epsilon: Vec<StateId>,
    /// Whether this is a match state.
    pub is_match: bool,
    /// Optional instruction for capture groups, lookarounds, etc.
    pub instruction: Option<NfaInstruction>,
}

impl NfaState {
    /// Creates a new empty state.
    pub fn new() -> Self {
        Self {
            transitions: Vec::new(),
            epsilon: Vec::new(),
            is_match: false,
            instruction: None,
        }
    }

    /// Creates a match state.
    pub fn match_state() -> Self {
        Self {
            transitions: Vec::new(),
            epsilon: Vec::new(),
            is_match: true,
            instruction: None,
        }
    }

    /// Adds a byte transition.
    pub fn add_transition(&mut self, range: ByteRange, target: StateId) {
        self.transitions.push((range, target));
    }

    /// Adds an epsilon transition.
    pub fn add_epsilon(&mut self, target: StateId) {
        self.epsilon.push(target);
    }
}

impl Default for NfaState {
    fn default() -> Self {
        Self::new()
    }
}

/// A byte range for transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Start of range (inclusive).
    pub start: u8,
    /// End of range (inclusive).
    pub end: u8,
}

impl ByteRange {
    /// Creates a new byte range.
    pub fn new(start: u8, end: u8) -> Self {
        Self { start, end }
    }

    /// Creates a range for a single byte.
    pub fn single(byte: u8) -> Self {
        Self {
            start: byte,
            end: byte,
        }
    }

    /// Creates a range matching any byte.
    pub fn any() -> Self {
        Self { start: 0, end: 255 }
    }

    /// Returns true if this range contains the byte.
    pub fn contains(&self, byte: u8) -> bool {
        byte >= self.start && byte <= self.end
    }

    /// Returns true if this range overlaps with another.
    pub fn overlaps(&self, other: &ByteRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// A byte class with precomputed 256-bit bitmap for fast O(1) membership testing.
/// Stores both the original ranges (for debugging/serialization) and the bitmap.
#[derive(Debug, Clone)]
pub struct ByteClass {
    /// Original byte ranges (kept for reference).
    pub ranges: Vec<ByteRange>,
    /// Precomputed 256-bit bitmap for O(1) lookup.
    /// bitmap[0] = bits 0-63, bitmap[1] = bits 64-127,
    /// bitmap[2] = bits 128-191, bitmap[3] = bits 192-255
    bitmap: [u64; 4],
}

impl ByteClass {
    /// Creates a new byte class from ranges with precomputed bitmap.
    pub fn new(ranges: Vec<ByteRange>) -> Self {
        let bitmap = Self::compute_bitmap(&ranges);
        Self { ranges, bitmap }
    }

    /// Creates a byte class from a slice of ranges.
    pub fn from_slice(ranges: &[ByteRange]) -> Self {
        Self::new(ranges.to_vec())
    }

    /// Computes the 256-bit bitmap from ranges.
    fn compute_bitmap(ranges: &[ByteRange]) -> [u64; 4] {
        let mut bits = [0u64; 4];
        for range in ranges {
            for byte in range.start..=range.end {
                let idx = (byte / 64) as usize;
                let bit = byte % 64;
                bits[idx] |= 1u64 << bit;
            }
        }
        bits
    }

    /// Checks if a byte is in this class. O(1) operation.
    #[inline(always)]
    pub fn contains(&self, byte: u8) -> bool {
        let idx = (byte / 64) as usize;
        let bit = byte % 64;
        (self.bitmap[idx] & (1u64 << bit)) != 0
    }

    /// Returns the raw bitmap for JIT code generation.
    #[inline]
    pub fn bitmap(&self) -> &[u64; 4] {
        &self.bitmap
    }
}

/// Special instructions for NFA states.
#[derive(Debug, Clone)]
pub enum NfaInstruction {
    /// Start of a capture group.
    CaptureStart(u32),
    /// End of a capture group.
    CaptureEnd(u32),
    /// Backreference to a capture group.
    Backref(u32),
    /// Word boundary assertion.
    WordBoundary,
    /// Not word boundary assertion.
    NotWordBoundary,
    /// Start of text assertion.
    StartOfText,
    /// End of text assertion.
    EndOfText,
    /// Start of line assertion.
    StartOfLine,
    /// End of line assertion.
    EndOfLine,
    /// Positive lookahead.
    /// Uses Arc to avoid cloning during PikeVM execution.
    PositiveLookahead(Arc<Nfa>),
    /// Negative lookahead.
    /// Uses Arc to avoid cloning during PikeVM execution.
    NegativeLookahead(Arc<Nfa>),
    /// Positive lookbehind.
    /// Uses Arc to avoid cloning during PikeVM execution.
    PositiveLookbehind(Arc<Nfa>),
    /// Negative lookbehind.
    /// Uses Arc to avoid cloning during PikeVM execution.
    NegativeLookbehind(Arc<Nfa>),
    /// Marker for non-greedy quantifier preference.
    /// When this state is reached and leads to a match, prefer this match
    /// over longer matches from continuing the quantifier.
    NonGreedyExit,
    /// Unicode codepoint class matching.
    /// Consumes a full UTF-8 codepoint and checks membership in the class.
    /// The StateId is the next state to transition to on match.
    CodepointClass(CodepointClass, StateId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_range() {
        let range = ByteRange::new(b'a', b'z');
        assert!(range.contains(b'm'));
        assert!(!range.contains(b'A'));
    }

    #[test]
    fn test_epsilon_closure() {
        let mut nfa = Nfa::new();

        // State 0 -> epsilon -> State 1 -> epsilon -> State 2
        let mut s0 = NfaState::new();
        s0.add_epsilon(1);
        nfa.add_state(s0);

        let mut s1 = NfaState::new();
        s1.add_epsilon(2);
        nfa.add_state(s1);

        nfa.add_state(NfaState::new());

        let mut initial = BTreeSet::new();
        initial.insert(0);

        let closure = nfa.epsilon_closure(&initial);
        assert!(closure.contains(&0));
        assert!(closure.contains(&1));
        assert!(closure.contains(&2));
    }
}

#[cfg(test)]
mod max_match_len_tests {
    use crate::hir::translate;
    use crate::parser::parse;

    fn bound(pattern: &str) -> Option<usize> {
        let hir = translate(&parse(pattern).unwrap()).unwrap();
        crate::nfa::compile(&hir).unwrap().max_match_len
    }

    #[test]
    fn fixed_length_patterns_are_exact() {
        assert_eq!(bound("a"), Some(1));
        assert_eq!(bound("abc"), Some(3));
        assert_eq!(bound("[a-z][0-9]"), Some(2));
    }

    #[test]
    fn optional_and_alternation_take_the_longest_branch() {
        assert_eq!(bound("ab?"), Some(2));
        assert_eq!(bound("a|abc"), Some(3));
        assert_eq!(bound("(?:ab|c)d"), Some(3));
        assert_eq!(bound("a{2,4}"), Some(4));
    }

    #[test]
    fn multibyte_literals_are_measured_in_bytes() {
        // The bound guards a byte offset, so it counts bytes, not characters.
        assert_eq!(bound("é"), Some("é".len()));
    }

    #[test]
    fn repetition_is_unbounded() {
        assert_eq!(bound("a*"), None);
        assert_eq!(bound("a+"), None);
        assert_eq!(bound("a.*b"), None);
        assert_eq!(bound("(?:ab)+"), None);
    }

    #[test]
    fn backreferences_are_unbounded() {
        // Length depends on captured text, not on the graph.
        assert_eq!(bound(r"(a)\1"), None);
    }

    #[test]
    fn zero_width_assertions_add_nothing() {
        assert_eq!(bound(r"\ba\b"), Some(1));
        assert_eq!(bound(r"^a$"), Some(1));
    }

    #[test]
    fn bound_is_an_upper_bound_on_real_matches() {
        // Whatever the analysis reports must cover every match the engine finds.
        for pattern in [
            "a",
            "ab?",
            "a|abc",
            "[a-z]{1,3}",
            "é",
            r"\w\w?",
            "(?:ab|c)d",
        ] {
            let max = bound(pattern).expect("pattern is bounded");
            let re = crate::Regex::new(pattern).unwrap();
            for haystack in ["", "a", "ab", "abc", "abcd", "éa", "zzz", "c d"] {
                if let Some(m) = re.find(haystack) {
                    assert!(
                        m.end() - m.start() <= max,
                        "{pattern:?} matched {:?} in {haystack:?}, over the bound {max}",
                        m.as_str()
                    );
                }
            }
        }
    }
}
