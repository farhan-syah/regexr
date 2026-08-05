//! Shared types for Shift-Or engine.
//!
//! Contains the ShiftOr data structure used by both interpreter and JIT.

use crate::hir::Hir;
use crate::nfa::{
    compile_glushkov, compile_glushkov_wide, BitSet256, GlushkovNfa, GlushkovWideNfa,
    MAX_POSITIONS, MAX_POSITIONS_WIDE,
};

/// A compiled Shift-Or pattern.
///
/// This is a data structure that holds the precomputed masks and follow sets
/// for the Shift-Or (Bitap) algorithm. The actual matching is performed by
/// either the interpreter or JIT.
///
/// **CRITICAL**: This implementation uses Glushkov NFA (epsilon-free), NOT Thompson NFA.
/// Thompson's epsilon-transitions break the 1-shift = 1-byte invariant.
///
/// Unlike classic Shift-Or which assumes linear position progression (i -> i+1),
/// this implementation uses explicit follow sets from Glushkov construction to
/// handle patterns like `a.*b` where nullable subexpressions create non-linear
/// transitions.
///
/// ## Limitations
///
/// ShiftOr does NOT support:
/// - Anchors (`^`, `$`)
/// - Word boundaries (`\b`, `\B`) - use LazyDFA instead
/// - Backreferences - use PikeVM or BacktrackingVM instead
/// - Lookaround - use PikeVM instead
/// - Non-greedy quantifiers (`.*?`, `.+?`) - Glushkov doesn't preserve match preference
/// - Patterns with more than 64 positions
#[derive(Debug)]
pub struct ShiftOr {
    /// Bit masks for each byte value.
    /// mask[b] has bit i cleared (0) if position i can transition on byte b.
    /// (Shift-Or uses inverted logic: 0 = "can be in this state")
    pub(crate) masks: [u64; 256],
    /// Accept state mask (inverted: 0 bits = accepting positions).
    pub(crate) accept: u64,
    /// First set: positions that can start a match.
    pub(crate) first: u64,
    /// Follow sets: follow[i] indicates which positions can follow position i.
    pub(crate) follow: Vec<u64>,
    /// Whether the pattern can match empty string.
    pub(crate) nullable: bool,
    /// Number of positions.
    pub(crate) position_count: usize,
    /// Whether the pattern has a leading word boundary (\b at start).
    pub(crate) has_leading_word_boundary: bool,
    /// Whether the pattern has a trailing word boundary (\b at end).
    pub(crate) has_trailing_word_boundary: bool,
    /// Whether the pattern has a start anchor (^).
    pub(crate) has_start_anchor: bool,
    /// Whether the pattern has an end anchor ($).
    pub(crate) has_end_anchor: bool,
}

impl ShiftOr {
    /// Tries to compile an HIR into a Shift-Or matcher.
    /// Returns None if the pattern is not suitable for Shift-Or.
    pub fn from_hir(hir: &Hir) -> Option<Self> {
        // Skip patterns with special features that can't be handled
        // Anchors (^, $), backrefs, lookarounds, and word boundaries require different engines.
        // Word boundaries (\b, \B) are complex to handle correctly in shift-or;
        // LazyDFA handles them properly with character-class augmented states.
        // Non-greedy quantifiers (.*?, .+?) require tracking match preference which
        // Glushkov NFA doesn't preserve - use TaggedNFA or PikeVM instead.
        if hir.props.has_backrefs
            || hir.props.has_lookaround
            || hir.props.has_anchors
            || hir.props.has_word_boundary
            || hir.props.has_non_greedy
        {
            return None;
        }

        // Build Glushkov NFA (epsilon-free)
        let glushkov = compile_glushkov(hir)?;

        Self::from_glushkov_with_boundaries(&glushkov, false, false)
    }

    /// Tries to compile an HIR with anchors into a Shift-Or matcher.
    /// Supports non-multiline ^ and $ anchors.
    /// Returns None if the pattern is not suitable for Shift-Or.
    pub fn from_hir_with_anchors(hir: &Hir) -> Option<Self> {
        // Skip patterns with unsupported features
        if hir.props.has_backrefs
            || hir.props.has_lookaround
            || hir.props.has_word_boundary
            || hir.props.has_non_greedy
        {
            return None;
        }

        // Build Glushkov NFA (epsilon-free) - anchors are treated as empty
        let glushkov = compile_glushkov(hir)?;

        // Detect anchor types from HIR properties
        let has_start_anchor = hir.props.has_start_anchor;
        let has_end_anchor = hir.props.has_end_anchor;

        Self::from_glushkov_with_options(&glushkov, false, false, has_start_anchor, has_end_anchor)
    }

    /// Creates a Shift-Or matcher from a Glushkov NFA.
    pub fn from_glushkov(nfa: &GlushkovNfa) -> Option<Self> {
        Self::from_glushkov_with_options(nfa, false, false, false, false)
    }

    /// Creates a Shift-Or matcher from a Glushkov NFA with word boundary info.
    fn from_glushkov_with_boundaries(
        nfa: &GlushkovNfa,
        has_leading_word_boundary: bool,
        has_trailing_word_boundary: bool,
    ) -> Option<Self> {
        Self::from_glushkov_with_options(
            nfa,
            has_leading_word_boundary,
            has_trailing_word_boundary,
            false,
            false,
        )
    }

    /// Creates a Shift-Or matcher from a Glushkov NFA with all options.
    fn from_glushkov_with_options(
        nfa: &GlushkovNfa,
        has_leading_word_boundary: bool,
        has_trailing_word_boundary: bool,
        has_start_anchor: bool,
        has_end_anchor: bool,
    ) -> Option<Self> {
        if nfa.position_count > MAX_POSITIONS || nfa.position_count == 0 {
            return None;
        }

        let masks = nfa.build_shift_or_masks();
        let accept = nfa.build_accept_mask();

        Some(Self {
            masks,
            accept,
            first: nfa.first,
            follow: nfa.follow.clone(),
            nullable: nfa.nullable,
            position_count: nfa.position_count,
            has_leading_word_boundary,
            has_trailing_word_boundary,
            has_start_anchor,
            has_end_anchor,
        })
    }

    /// Returns true if this pattern has word boundaries.
    /// Note: ShiftOr no longer accepts patterns with word boundaries,
    /// so this always returns false for valid ShiftOr instances.
    #[inline]
    pub fn has_word_boundary(&self) -> bool {
        self.has_leading_word_boundary || self.has_trailing_word_boundary
    }

    /// Returns the number of positions.
    pub fn state_count(&self) -> usize {
        self.position_count
    }

    /// Returns the masks table.
    pub fn masks(&self) -> &[u64; 256] {
        &self.masks
    }

    /// Returns the accept mask.
    pub fn accept(&self) -> u64 {
        self.accept
    }

    /// Returns the first set.
    pub fn first(&self) -> u64 {
        self.first
    }

    /// Returns the follow sets.
    pub fn follow(&self) -> &[u64] {
        &self.follow
    }

    /// Returns whether the pattern is nullable.
    pub fn is_nullable(&self) -> bool {
        self.nullable
    }

    /// Returns whether there's a leading word boundary.
    pub fn has_leading_word_boundary(&self) -> bool {
        self.has_leading_word_boundary
    }

    /// Returns whether there's a trailing word boundary.
    pub fn has_trailing_word_boundary(&self) -> bool {
        self.has_trailing_word_boundary
    }

    // ========================================================================
    // Convenience matching methods (delegate to interpreter)
    // ========================================================================

    /// Returns true if the pattern matches anywhere in the input.
    pub fn is_match(&self, input: &[u8]) -> bool {
        self.find(input).is_some()
    }

    /// Finds the first match, returning (start, end).
    pub fn find(&self, input: &[u8]) -> Option<(usize, usize)> {
        // For start anchor: only try matching at position 0
        if self.has_start_anchor {
            if let Some(end) = self.match_at(input, 0) {
                // For end anchor: only accept if match ends at input end
                if self.has_end_anchor && !crate::nfa::at_end_or_before_final_newline(input, end) {
                    return None;
                }
                return Some((0, end));
            }
            // If pattern is nullable and has both anchors, empty match at 0 only if input is empty
            if self.nullable && (!self.has_end_anchor || input.is_empty()) {
                return Some((0, 0));
            }
            return None;
        }

        // Try matching at each position, preferring longest match (greedy).
        // `scan_limit` first rules out "no match anywhere" in a single pass and
        // otherwise caps how far this scan has to walk.
        let scan_end = self.scan_limit(input, 0)?;
        for start in 0..=scan_end {
            if let Some(end) = self.match_at(input, start) {
                // For end anchor: only accept if match ends at input end
                if self.has_end_anchor && !crate::nfa::at_end_or_before_final_newline(input, end) {
                    continue;
                }
                return Some((start, end));
            }
        }

        // If pattern is nullable and no non-empty match found, return empty match at 0
        if self.nullable && !self.has_end_anchor {
            return Some((0, 0));
        }

        None
    }

    /// Finds a match starting at or after the given position.
    /// Returns (start, end) if found.
    pub fn find_at(&self, input: &[u8], pos: usize) -> Option<(usize, usize)> {
        if pos > input.len() {
            return None;
        }

        // For start anchor: can only match at position 0
        if self.has_start_anchor && pos > 0 {
            return None;
        }

        let search_start = if self.has_start_anchor { 0 } else { pos };

        // Try matching at each position from pos. `scan_limit` first rules out
        // "no match anywhere" in a single pass and otherwise caps how far this
        // scan has to walk.
        let scan_end = self.scan_limit(input, search_start)?;
        for start in search_start..=scan_end {
            if let Some(end) = self.match_at(input, start) {
                // For end anchor: only accept if match ends at input end
                if self.has_end_anchor && !crate::nfa::at_end_or_before_final_newline(input, end) {
                    if self.has_start_anchor {
                        // Can't match anywhere else
                        return None;
                    }
                    continue;
                }
                return Some((start, end));
            }
            // For start anchor: only one position to try
            if self.has_start_anchor {
                break;
            }
        }
        None
    }

    /// One bit-parallel pass over `input[from..]` keeping a fresh start live at
    /// every position; returns the earliest position at which *some* non-empty
    /// match ends, or `None` if no match begins at or after `from`.
    ///
    /// This is the classic unanchored Shift-Or scan. Because every start is live
    /// at once it answers "is there a match at all?" in a single pass, where the
    /// anchored `match_at` needs one pass per start position.
    ///
    /// It deliberately reports only the earliest **end**. The state is a bitmask
    /// of reachable positions and records nothing about *which* start reached
    /// them, so it cannot express the leftmost-first preference between two
    /// starts — `abc|b` on "abc" ends a match at 2 via the `b` branch, while the
    /// leftmost match is `abc` spanning 0..3. What it does give is a bound: the
    /// match ending at `e` starts at or before `e`, and the leftmost start is no
    /// later than that start, so `s* <= e`. A caller can therefore stop its
    /// anchored scan at `e` instead of walking to the end of the input.
    pub(crate) fn earliest_match_end(&self, input: &[u8], from: usize) -> Option<usize> {
        // Inverted logic throughout, as in `match_at`: bit i == 0 means position
        // i is active. All 1s = nothing reached yet.
        let mut state = !0u64;

        for (i, &byte) in input[from..].iter().enumerate() {
            // Positions reachable from the active set, unioned with First — that
            // unconditional injection is what keeps a match starting *here* live
            // alongside every earlier one, and is the whole difference from the
            // anchored scan.
            let mut reachable = self.first;
            let mut active = !state;
            while active != 0 {
                let pos = active.trailing_zeros() as usize;
                reachable |= self.follow[pos];
                active &= active - 1;
            }

            state = (!reachable) | self.masks[byte as usize];

            if (state | self.accept) != !0u64 {
                return Some(from + i + 1);
            }
        }

        None
    }

    /// How far an anchored scan starting at `search_start` has to walk.
    ///
    /// Returns `None` when a single unanchored pass proves there is no match to
    /// find, letting the caller skip the scan entirely — that is what turns the
    /// no-match case from one pass per position into one pass overall.
    ///
    /// The pass is skipped where it cannot pay off: a start anchor means the scan
    /// tries a single position anyway, and a nullable pattern matches empty at
    /// the first position tried.
    pub(crate) fn scan_limit(&self, input: &[u8], search_start: usize) -> Option<usize> {
        if self.has_start_anchor || self.nullable {
            return Some(input.len());
        }
        match self.earliest_match_end(input, search_start) {
            None => None,
            // An end-anchored pattern may reject this match and keep looking, so
            // only the existence check carries over — the bound does not.
            Some(_) if self.has_end_anchor => Some(input.len()),
            Some(end) => Some(end),
        }
    }

    /// Tries to match at exactly the given position.
    /// Returns (start, end) if matched, None otherwise.
    /// Use this when you know the match should start at exactly `pos` (e.g., from a prefilter).
    pub fn try_match_at(&self, input: &[u8], pos: usize) -> Option<(usize, usize)> {
        // For start anchor: only position 0 can match
        if self.has_start_anchor && pos != 0 {
            return None;
        }
        match self.match_at(input, pos) {
            Some(end) => {
                // For end anchor: must match to end of input
                if self.has_end_anchor && !crate::nfa::at_end_or_before_final_newline(input, end) {
                    return None;
                }
                Some((pos, end))
            }
            None => None,
        }
    }

    /// Attempts to match at a specific position.
    fn match_at(&self, input: &[u8], start: usize) -> Option<usize> {
        if start > input.len() {
            return None;
        }

        // Track the last match position found
        let mut last_match = None;

        // Check if nullable (empty match)
        if self.nullable {
            last_match = Some(start);
        }

        // State tracking using inverted logic:
        // - bit i = 0 means we've reached position i (active)
        // - bit i = 1 means we haven't reached position i (inactive)
        //
        // Initial state: all 1s (no positions reached yet)
        let mut state = !0u64;

        for (i, &byte) in input[start..].iter().enumerate() {
            let byte_mask = self.masks[byte as usize];

            if i == 0 {
                // First byte: can only start at positions in First set
                // ~first gives us 0s at First positions, 1s elsewhere
                // Then apply byte mask to filter positions that don't accept this byte
                state = (!self.first) | byte_mask;
            } else {
                // Subsequent bytes: use follow sets for transitions
                let mut active = !state; // Flip: 1 = active, 0 = inactive

                // Compute union of follow sets for all active positions
                let mut reachable = 0u64;
                while active != 0 {
                    let pos = active.trailing_zeros() as usize;
                    reachable |= self.follow[pos];
                    active &= active - 1; // Clear lowest set bit
                }

                // Invert back to Shift-Or convention (0 = active)
                // Then apply byte mask (positions that don't accept byte become 1)
                state = (!reachable) | byte_mask;
            }

            // Check for match: if any accepting position is reached (bit is 0)
            if (state | self.accept) != !0u64 {
                last_match = Some(start + i + 1);
            }

            // If all bits are 1, no possible match from this starting point
            if state == !0u64 {
                break;
            }
        }

        last_match
    }
}

/// Checks if an HIR is suitable for Shift-Or.
/// Whether `expr` can match the empty string. Such patterns have a leftmost
/// zero-width match that bit-parallel Shift-Or can't represent.
fn hir_is_nullable(expr: &crate::hir::HirExpr) -> bool {
    use crate::hir::HirExpr;
    match expr {
        HirExpr::Empty => true,
        HirExpr::Literal(b) => b.is_empty(),
        HirExpr::Class(_) | HirExpr::UnicodeCpClass(_) | HirExpr::Backref(_) => false,
        HirExpr::Concat(es) => es.iter().all(hir_is_nullable),
        HirExpr::Alt(es) => es.iter().any(hir_is_nullable),
        HirExpr::Repeat(r) => r.min == 0 || hir_is_nullable(&r.expr),
        HirExpr::Capture(c) => hir_is_nullable(&c.expr),
        // Anchors and lookarounds are zero-width assertions.
        HirExpr::Anchor(_) | HirExpr::Lookaround(_) => true,
    }
}

/// Whether `hir` can be matched by the (narrow) Shift-Or engine.
pub fn is_shift_or_compatible(hir: &Hir) -> bool {
    // Backrefs, lookarounds, and word boundaries require different engines.
    // Word boundaries (\b, \B) are complex to handle correctly in shift-or;
    // LazyDFA handles them properly with character-class augmented states.
    // Non-greedy quantifiers (.*?, .+?) require tracking match preference which
    // Glushkov NFA doesn't preserve - use TaggedNFA or PikeVM instead.
    // Multiline anchors ((?m)^, (?m)$) need position-aware matching via LazyDFA.
    if hir.props.has_backrefs
        || hir.props.has_lookaround
        || hir.props.has_multiline_anchors
        || hir.props.has_word_boundary
        || hir.props.has_non_greedy
    {
        return false;
    }

    // Nullable patterns (those that can match the empty string, e.g. `a*`, ` *`,
    // `a*|b`) are not Shift-Or compatible: the Glushkov automaton has no empty-match
    // representation, so Shift-Or skips to the first literal occurrence and misses
    // the leftmost zero-width match. Route them to LazyDFA, which matches empty.
    if hir_is_nullable(&hir.expr) {
        return false;
    }

    // Try to build Glushkov NFA to check position count
    compile_glushkov(hir)
        .map(|nfa| nfa.position_count <= MAX_POSITIONS && nfa.position_count > 0)
        .unwrap_or(false)
}

// ============================================================================
// Wide Shift-Or (supports up to 256 positions)
// ============================================================================

/// A compiled Wide Shift-Or pattern supporting up to 256 positions.
///
/// Uses `[u64; 4]` (BitSet256) for state vectors instead of `u64`,
/// allowing patterns with 65-256 character positions to use the efficient
/// bit-parallel Shift-Or algorithm instead of falling back to PikeVM.
///
/// Performance notes:
/// - For patterns with ≤64 positions, use `ShiftOr` (faster due to single u64)
/// - For patterns with 65-256 positions, use `ShiftOrWide`
/// - For patterns with >256 positions, use LazyDFA or PikeVM
#[derive(Debug)]
pub struct ShiftOrWide {
    /// Bit masks for each byte value (256-bit wide).
    pub(crate) masks: Box<[BitSet256; 256]>,
    /// Accept state mask (inverted: 0 bits = accepting positions).
    pub(crate) accept: BitSet256,
    /// First set: positions that can start a match.
    pub(crate) first: BitSet256,
    /// Follow sets: follow[i] indicates which positions can follow position i.
    pub(crate) follow: Vec<BitSet256>,
    /// Whether the pattern can match empty string.
    pub(crate) nullable: bool,
    /// Number of positions.
    pub(crate) position_count: usize,
}

impl ShiftOrWide {
    /// Tries to compile an HIR into a Wide Shift-Or matcher.
    /// Returns None if the pattern is not suitable.
    pub fn from_hir(hir: &Hir) -> Option<Self> {
        // Skip patterns with special features that can't be handled
        if hir.props.has_backrefs
            || hir.props.has_lookaround
            || hir.props.has_anchors
            || hir.props.has_word_boundary
            || hir.props.has_non_greedy
        {
            return None;
        }

        // Build Wide Glushkov NFA
        let glushkov = compile_glushkov_wide(hir)?;

        Self::from_glushkov(&glushkov)
    }

    /// Creates a Wide Shift-Or matcher from a Wide Glushkov NFA.
    pub fn from_glushkov(nfa: &GlushkovWideNfa) -> Option<Self> {
        if nfa.position_count > MAX_POSITIONS_WIDE || nfa.position_count == 0 {
            return None;
        }

        let masks = Box::new(nfa.build_shift_or_masks());
        let accept = nfa.build_accept_mask();

        Some(Self {
            masks,
            accept,
            first: nfa.first,
            follow: nfa.follow.clone(),
            nullable: nfa.nullable,
            position_count: nfa.position_count,
        })
    }

    /// Returns the number of positions.
    pub fn state_count(&self) -> usize {
        self.position_count
    }

    /// Returns whether the pattern is nullable.
    pub fn is_nullable(&self) -> bool {
        self.nullable
    }

    // ========================================================================
    // Convenience matching methods (delegate to interpreter)
    // ========================================================================

    /// Returns true if the pattern matches anywhere in the input.
    pub fn is_match(&self, input: &[u8]) -> bool {
        self.find(input).is_some()
    }

    /// Finds the first match, returning (start, end).
    pub fn find(&self, input: &[u8]) -> Option<(usize, usize)> {
        // Try matching at each position, preferring longest match (greedy).
        // `scan_limit` first rules out "no match anywhere" in a single pass and
        // otherwise caps how far this scan has to walk.
        // No match to find when this is `None`. A nullable pattern still
        // matches empty, and `scan_limit` never reports `None` for one.
        let scan_end = self.scan_limit(input, 0)?;
        for start in 0..=scan_end {
            if let Some(end) = self.match_at(input, start) {
                return Some((start, end));
            }
        }

        // If pattern is nullable and no non-empty match found, return empty match at 0
        if self.nullable {
            return Some((0, 0));
        }

        None
    }

    /// Finds a match starting at or after the given position.
    pub fn find_at(&self, input: &[u8], pos: usize) -> Option<(usize, usize)> {
        if pos > input.len() {
            return None;
        }

        let scan_end = self.scan_limit(input, pos)?;
        for start in pos..=scan_end {
            if let Some(end) = self.match_at(input, start) {
                return Some((start, end));
            }
        }
        None
    }

    /// One bit-parallel pass over `input[from..]` keeping a fresh start live at
    /// every position; returns the earliest position at which *some* non-empty
    /// match ends, or `None` if no match begins at or after `from`.
    ///
    /// The 256-bit counterpart of `ShiftOr::earliest_match_end` — see there for
    /// why this reports the earliest *end* and how that bounds the anchored scan
    /// (`s* <= e`).
    fn earliest_match_end(&self, input: &[u8], from: usize) -> Option<usize> {
        let mut state = BitSet256::all_ones();

        for (i, &byte) in input[from..].iter().enumerate() {
            // Reachable from the active set, unioned with First — the
            // unconditional injection keeps a match starting *here* live
            // alongside every earlier one.
            let mut reachable = self.first;
            let active = state.complement();
            for word_idx in 0..4 {
                let mut word = active.parts[word_idx];
                while word != 0 {
                    let pos = word_idx * 64 + word.trailing_zeros() as usize;
                    if pos < self.follow.len() {
                        reachable.union_assign(self.follow[pos]);
                    }
                    word &= word - 1;
                }
            }

            state = reachable.complement().union(self.masks[byte as usize]);

            if !state.union(self.accept).is_all_ones() {
                return Some(from + i + 1);
            }
        }

        None
    }

    /// How far an anchored scan starting at `search_start` has to walk, or `None`
    /// when a single unanchored pass proves there is nothing to find. Skipped for
    /// a nullable pattern, which matches empty at the first position tried.
    fn scan_limit(&self, input: &[u8], search_start: usize) -> Option<usize> {
        if self.nullable {
            return Some(input.len());
        }
        self.earliest_match_end(input, search_start)
    }

    /// Tries to match at exactly the given position.
    pub fn try_match_at(&self, input: &[u8], pos: usize) -> Option<(usize, usize)> {
        self.match_at(input, pos).map(|end| (pos, end))
    }

    /// Core matching logic using 256-bit state vectors.
    fn match_at(&self, input: &[u8], start: usize) -> Option<usize> {
        if start > input.len() {
            return None;
        }

        let mut last_match = None;

        if self.nullable {
            last_match = Some(start);
        }

        // State tracking using inverted logic (same as u64 version):
        // - bit i = 0 means we've reached position i (active)
        // - bit i = 1 means we haven't reached position i (inactive)
        let mut state = BitSet256::all_ones();

        for (i, &byte) in input[start..].iter().enumerate() {
            let byte_mask = self.masks[byte as usize];

            if i == 0 {
                // First byte: can only start at positions in First set
                state = self.first.complement().union(byte_mask);
            } else {
                // Subsequent bytes: use follow sets for transitions
                // Flip state: 1 = active, 0 = inactive
                let active = state.complement();

                // Compute union of follow sets for all active positions
                let mut reachable = BitSet256::empty();

                // Iterate over all 4 words to find active positions
                for word_idx in 0..4 {
                    let mut word = active.parts[word_idx];
                    while word != 0 {
                        let bit_idx = word.trailing_zeros() as usize;
                        let pos = word_idx * 64 + bit_idx;
                        if pos < self.follow.len() {
                            reachable.union_assign(self.follow[pos]);
                        }
                        word &= word - 1; // Clear lowest set bit
                    }
                }

                // Invert back to Shift-Or convention (0 = active)
                // Then apply byte mask
                state = reachable.complement().union(byte_mask);
            }

            // Check for match: if any accepting position is reached (bit is 0)
            if !state.union(self.accept).is_all_ones() {
                last_match = Some(start + i + 1);
            }

            // If all bits are 1, no possible match from this starting point
            if state.is_all_ones() {
                break;
            }
        }

        last_match
    }
}

/// Checks if an HIR is suitable for Wide Shift-Or (65-256 positions).
pub fn is_shift_or_wide_compatible(hir: &Hir) -> bool {
    if hir.props.has_backrefs
        || hir.props.has_lookaround
        || hir.props.has_anchors
        || hir.props.has_word_boundary
        || hir.props.has_non_greedy
    {
        return false;
    }

    // Nullable patterns can't be represented (see `is_shift_or_compatible`).
    if hir_is_nullable(&hir.expr) {
        return false;
    }

    // Try to build Wide Glushkov NFA to check position count
    compile_glushkov_wide(hir)
        .map(|nfa| {
            nfa.position_count > MAX_POSITIONS
                && nfa.position_count <= MAX_POSITIONS_WIDE
                && nfa.position_count > 0
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod scan_bound_tests {
    use super::*;
    use crate::hir::translate;
    use crate::parser::parse;

    fn compile(pattern: &str) -> Option<ShiftOr> {
        let hir = parse(pattern).and_then(|ast| translate(&ast)).ok()?;
        if hir.props.has_anchors {
            ShiftOr::from_hir_with_anchors(&hir)
        } else {
            ShiftOr::from_hir(&hir)
        }
    }

    /// The unbounded scan the `scan_limit` bound replaced: try every start.
    ///
    /// `match_at` knows nothing about anchors — they are stripped during Glushkov
    /// construction and enforced by the callers — so this oracle applies them the
    /// same way, otherwise it would happily accept `^a` at position 1.
    fn brute_force(so: &ShiftOr, input: &[u8], from: usize) -> Option<(usize, usize)> {
        if so.has_start_anchor && from > 0 {
            return None;
        }
        let last = if so.has_start_anchor { 0 } else { input.len() };
        for start in from..=last {
            if let Some(end) = so.match_at(input, start) {
                if so.has_end_anchor && !crate::nfa::at_end_or_before_final_newline(input, end) {
                    continue;
                }
                return Some((start, end));
            }
        }
        None
    }

    const PATTERNS: &[&str] = &[
        "a", "ab", "abc", "[ab]", "a+", "a*", "a?", "a{2}", "a{1,3}", "\\w", "\\w+", "\\w*", "\\d",
        "\\w*\\d", "[a-z]*9", "a*b", "a.*b", "a.c", "(?:ab)+", "[^a]", "[^a]+", ".", ".*", ".+",
        "\\s*", "\\s+", "ab*c", "a[bc]d", "^a", "a$", "^ab$", "^a*", "a*$",
    ];

    const TEXTS: &[&str] = &[
        "",
        "a",
        "aa",
        "ab",
        "ba",
        "abc",
        "abab",
        "aaab",
        "aaa9",
        "9aaa",
        "aaa",
        "a b c",
        "  ",
        "xyz",
        "aaaaaaaaab",
        "baaaaaaaaa",
        "abcabcabc",
        "a\nb",
        "aaa\n",
        "9",
        "z",
    ];

    /// Bounding the anchored scan by the earliest match end must never change an
    /// answer: the leftmost start `s*` always satisfies `s* <= e`, because the
    /// match ending at `e` itself starts at or before `e`.
    #[test]
    fn bounded_scan_matches_brute_force() {
        let mut failures = Vec::new();
        for pattern in PATTERNS {
            let Some(so) = compile(pattern) else {
                continue;
            };
            for text in TEXTS {
                let bytes = text.as_bytes();
                for from in 0..=bytes.len() {
                    let expected = brute_force(&so, bytes, from);
                    let got = so.find_at(bytes, from);
                    if got != expected {
                        failures.push(format!(
                            "{pattern:?} on {text:?} from {from}: brute={expected:?} got={got:?}"
                        ));
                    }
                }
                // `find` is the same search anchored at zero.
                let got = so.find(bytes);
                let expected = brute_force(&so, bytes, 0);
                if got != expected && !(so.nullable && expected.is_none()) {
                    failures.push(format!(
                        "find {pattern:?} on {text:?}: brute={expected:?} got={got:?}"
                    ));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} divergences:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// The one-pass rejector must agree with the scan about whether anything
    /// matches at all — that equivalence is what makes the early return sound.
    #[test]
    fn earliest_match_end_agrees_with_scan() {
        for pattern in PATTERNS {
            let Some(so) = compile(pattern) else {
                continue;
            };
            if so.nullable || so.has_start_anchor {
                continue;
            }
            for text in TEXTS {
                let bytes = text.as_bytes();
                for from in 0..=bytes.len() {
                    let any_match = (from..=bytes.len())
                        .filter_map(|s| so.match_at(bytes, s).map(|e| (s, e)))
                        .find(|(s, e)| e > s);
                    let end = so.earliest_match_end(bytes, from);
                    assert_eq!(
                        end.is_some(),
                        any_match.is_some(),
                        "{pattern:?} on {text:?} from {from}: pass={end:?} scan={any_match:?}"
                    );
                    if let (Some(e), Some((s, _))) = (end, any_match) {
                        assert!(
                            s <= e,
                            "{pattern:?} on {text:?} from {from}: leftmost start {s} > bound {e}"
                        );
                    }
                }
            }
        }
    }
}
