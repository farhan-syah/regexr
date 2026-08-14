//! Tagged NFA interpreter for fast pattern matching.
//!
//! This interpreter executes pre-extracted pattern steps for fast matching.
//! It provides the same algorithm as the JIT but interpreted.

use crate::nfa::tagged::shared::PatternStep;

/// Fast step-based Tagged NFA matcher.
///
/// Executes pattern steps directly without full NFA simulation.
/// This is faster than Thompson NFA simulation for patterns that can
/// be expressed as a linear sequence of steps.
pub struct TaggedNfa;

impl TaggedNfa {
    /// Finds the first match in the input.
    pub fn find(steps: &[PatternStep], input: &[u8]) -> Option<(usize, usize)> {
        Self::find_at(steps, input, 0)
    }

    /// Finds a match starting at or after the given position.
    ///
    /// The full input is passed to every attempt, so steps that read left
    /// context (`^`, `\b`, lookbehind) see the real preceding bytes.
    pub fn find_at(
        steps: &[PatternStep],
        input: &[u8],
        start_from: usize,
    ) -> Option<(usize, usize)> {
        for start in start_from..=input.len() {
            // Only start at UTF-8 codepoint boundaries (see `is_utf8_boundary`).
            if !crate::nfa::is_utf8_boundary(input, start) {
                continue;
            }
            if let Some(end) = Self::match_at(steps, input, start) {
                return Some((start, end));
            }
        }
        None
    }

    /// Attempts to match at a specific position, returning the end position on success.
    ///
    /// Anchored: only `start` is tried, so a pattern whose match begins later
    /// reports `None`. Callers that want a search use [`TaggedNfa::find_at`],
    /// which is this method run over each candidate start in turn.
    pub(crate) fn match_at(steps: &[PatternStep], input: &[u8], start: usize) -> Option<usize> {
        Self::match_steps(steps, input, start)
    }

    /// Matches a sequence of steps starting at the given position.
    fn match_steps(steps: &[PatternStep], input: &[u8], start: usize) -> Option<usize> {
        let mut pos = start;

        for (step_idx, step) in steps.iter().enumerate() {
            match step {
                PatternStep::Byte(b) => {
                    if pos >= input.len() || input[pos] != *b {
                        return None;
                    }
                    pos += 1;
                }
                PatternStep::ByteClass(byte_class) => {
                    if pos >= input.len() {
                        return None;
                    }
                    let byte = input[pos];
                    if !byte_class.contains(byte) {
                        return None;
                    }
                    pos += 1;
                }
                PatternStep::GreedyPlus(byte_class) => {
                    // Must match at least one
                    if pos >= input.len() {
                        return None;
                    }
                    let byte = input[pos];
                    if !byte_class.contains(byte) {
                        return None;
                    }
                    let min_pos = pos + 1;
                    pos += 1;
                    // Match as many as possible
                    while pos < input.len() {
                        let byte = input[pos];
                        if !byte_class.contains(byte) {
                            break;
                        }
                        pos += 1;
                    }
                    // Try to match remaining steps, backtracking if needed
                    let remaining_steps = &steps[step_idx + 1..];
                    if !remaining_steps.is_empty() {
                        loop {
                            if let Some(end) = Self::match_steps(remaining_steps, input, pos) {
                                return Some(end);
                            }
                            if pos <= min_pos {
                                return None; // Can't backtrack more
                            }
                            pos -= 1; // Backtrack one byte
                        }
                    }
                }
                PatternStep::GreedyStar(byte_class) => {
                    let min_pos = pos; // Can backtrack to zero matches
                                       // Match as many as possible (zero or more)
                    while pos < input.len() {
                        let byte = input[pos];
                        if !byte_class.contains(byte) {
                            break;
                        }
                        pos += 1;
                    }
                    // Try to match remaining steps, backtracking if needed
                    let remaining_steps = &steps[step_idx + 1..];
                    if !remaining_steps.is_empty() {
                        loop {
                            if let Some(end) = Self::match_steps(remaining_steps, input, pos) {
                                return Some(end);
                            }
                            if pos <= min_pos {
                                return None; // Can't backtrack more
                            }
                            pos -= 1; // Backtrack one byte
                        }
                    }
                }
                PatternStep::GreedyPlusLookahead(byte_class, lookahead_steps, is_positive) => {
                    // Must match at least one
                    if pos >= input.len() {
                        return None;
                    }
                    let byte = input[pos];
                    if !byte_class.contains(byte) {
                        return None;
                    }
                    let min_pos = pos + 1;
                    pos += 1;
                    // Greedily consume all matching
                    while pos < input.len() {
                        let byte = input[pos];
                        if !byte_class.contains(byte) {
                            break;
                        }
                        pos += 1;
                    }
                    // Backtrack until lookahead succeeds
                    loop {
                        let lookahead_match = Self::check_lookahead(lookahead_steps, input, pos);
                        if *is_positive == lookahead_match {
                            break; // Lookahead succeeded
                        }
                        if pos <= min_pos {
                            return None; // Can't backtrack more
                        }
                        pos -= 1;
                    }
                }
                PatternStep::GreedyStarLookahead(byte_class, lookahead_steps, is_positive) => {
                    let min_pos = pos;
                    // Greedily consume all matching
                    while pos < input.len() {
                        let byte = input[pos];
                        if !byte_class.contains(byte) {
                            break;
                        }
                        pos += 1;
                    }
                    // Backtrack until lookahead succeeds
                    loop {
                        let lookahead_match = Self::check_lookahead(lookahead_steps, input, pos);
                        if *is_positive == lookahead_match {
                            break;
                        }
                        if pos <= min_pos {
                            return None;
                        }
                        pos -= 1;
                    }
                }
                PatternStep::PositiveLookahead(inner_steps) => {
                    if !Self::check_lookahead(inner_steps, input, pos) {
                        return None;
                    }
                    // Zero-width: don't advance pos
                }
                PatternStep::NegativeLookahead(inner_steps) => {
                    if Self::check_lookahead(inner_steps, input, pos) {
                        return None;
                    }
                    // Zero-width: don't advance pos
                }
                PatternStep::WordBoundary => {
                    if !crate::nfa::is_word_boundary(input, pos) {
                        return None;
                    }
                }
                PatternStep::NotWordBoundary => {
                    if crate::nfa::is_word_boundary(input, pos) {
                        return None;
                    }
                }
                PatternStep::StartOfText => {
                    if pos != 0 {
                        return None;
                    }
                }
                PatternStep::EndOfText => {
                    if !crate::nfa::at_end_or_before_final_newline(input, pos) {
                        return None;
                    }
                }
                PatternStep::PositiveLookbehind(inner_steps, widths) => {
                    if !Self::check_lookbehind(inner_steps, input, pos, widths) {
                        return None;
                    }
                    // Zero-width: don't advance pos
                }
                PatternStep::NegativeLookbehind(inner_steps, widths) => {
                    if Self::check_lookbehind(inner_steps, input, pos, widths) {
                        return None;
                    }
                    // Zero-width: don't advance pos
                }
                PatternStep::CaptureStart(_) | PatternStep::CaptureEnd(_) => {
                    // Capture markers don't consume input - skip them
                    // (we're only finding matches, not tracking captures)
                }
                PatternStep::CodepointClass(cpclass, _target) => {
                    // Decode one UTF-8 codepoint and check class membership
                    if let Some((cp, len)) = Self::decode_utf8(input, pos) {
                        if cpclass.contains(cp) {
                            pos += len;
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                PatternStep::GreedyCodepointPlus(cpclass) => {
                    // Must match at least one codepoint
                    if let Some((cp, len)) = Self::decode_utf8(input, pos) {
                        if !cpclass.contains(cp) {
                            return None;
                        }
                        pos += len;
                    } else {
                        return None;
                    }
                    // The first codepoint is mandatory, so it is also the
                    // shortest the run may backtrack to.
                    let min_pos = pos;
                    // Match as many as possible
                    while let Some((cp, len)) = Self::decode_utf8(input, pos) {
                        if !cpclass.contains(cp) {
                            break;
                        }
                        pos += len;
                    }
                    // Try to match remaining steps, backtracking if needed
                    let remaining_steps = &steps[step_idx + 1..];
                    if !remaining_steps.is_empty() {
                        // Backtrack from longest match to shortest, walking the
                        // run's codepoint boundaries backwards rather than
                        // recording them on the way in — see
                        // [`TaggedNfa::prev_boundary`].
                        let mut boundary = pos;
                        loop {
                            if let Some(end) = Self::match_steps(remaining_steps, input, boundary) {
                                return Some(end);
                            }
                            if boundary <= min_pos {
                                return None;
                            }
                            boundary = Self::prev_boundary(input, boundary);
                        }
                    }
                }
                PatternStep::Alt(alternatives) => {
                    // Try each alternative
                    let remaining_steps = &steps[step_idx + 1..];
                    for alt_steps in alternatives {
                        // Match the alternative
                        if let Some(alt_end) = Self::match_steps(alt_steps, input, pos) {
                            // Then match remaining steps after the Alt
                            if remaining_steps.is_empty() {
                                return Some(alt_end);
                            }
                            if let Some(final_end) =
                                Self::match_steps(remaining_steps, input, alt_end)
                            {
                                return Some(final_end);
                            }
                            // This alternative matched but remaining steps failed, try next alternative
                        }
                    }
                    return None;
                }
                _ => {
                    // Unsupported step - should have been filtered during extraction
                    return None;
                }
            }
        }

        Some(pos)
    }

    /// Checks if the lookahead pattern matches at the given position.
    /// Uses backtracking for greedy quantifiers followed by other patterns.
    fn check_lookahead(steps: &[PatternStep], input: &[u8], pos: usize) -> bool {
        // Optimize common case: `.*X` where X is a character class or byte
        // For `(?=.*\d)`, we need to check if a digit exists within the range that `.*` can match
        if steps.len() == 2 {
            if let PatternStep::GreedyStar(star_class) = &steps[0] {
                // Find the extent of `.*` - it matches characters in star_class
                // For standard `.*`, star_class excludes newline (0x0a)
                let mut star_end = pos;
                while star_end < input.len() {
                    let byte = input[star_end];
                    if !star_class.contains(byte) {
                        break;
                    }
                    star_end += 1;
                }

                // Now check if the final step matches anywhere from pos to star_end
                match &steps[1] {
                    PatternStep::ByteClass(final_class) => {
                        for p in pos..=star_end {
                            if p >= input.len() {
                                break;
                            }
                            let byte = input[p];
                            if final_class.contains(byte) {
                                return true;
                            }
                        }
                        return false;
                    }
                    PatternStep::Byte(b) => {
                        for p in pos..=star_end {
                            if p >= input.len() {
                                break;
                            }
                            if input[p] == *b {
                                return true;
                            }
                        }
                        return false;
                    }
                    _ => {}
                }
            }
        }

        // General case: use recursive backtracking
        Self::check_lookahead_recursive(steps, input, pos)
    }

    /// Recursive backtracking lookahead checker.
    fn check_lookahead_recursive(steps: &[PatternStep], input: &[u8], pos: usize) -> bool {
        if steps.is_empty() {
            return true;
        }

        let step = &steps[0];
        let rest = &steps[1..];

        match step {
            PatternStep::Byte(b) => {
                if pos >= input.len() || input[pos] != *b {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos + 1)
            }
            PatternStep::ByteClass(byte_class) => {
                if pos >= input.len() {
                    return false;
                }
                let byte = input[pos];
                if !byte_class.contains(byte) {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos + 1)
            }
            PatternStep::GreedyPlus(byte_class) => {
                // Must match at least one
                if pos >= input.len() {
                    return false;
                }
                let byte = input[pos];
                if !byte_class.contains(byte) {
                    return false;
                }
                // Greedily match as many as possible, then backtrack
                let mut end = pos + 1;
                while end < input.len() {
                    let byte = input[end];
                    if !byte_class.contains(byte) {
                        break;
                    }
                    end += 1;
                }
                // Backtrack from longest match to shortest (at least 1)
                for p in (pos + 1..=end).rev() {
                    if Self::check_lookahead_recursive(rest, input, p) {
                        return true;
                    }
                }
                false
            }
            PatternStep::GreedyStar(byte_class) => {
                // Match as many as possible (zero or more), then backtrack
                let mut end = pos;
                while end < input.len() {
                    let byte = input[end];
                    if !byte_class.contains(byte) {
                        break;
                    }
                    end += 1;
                }
                // Backtrack from longest match to shortest (including 0)
                for p in (pos..=end).rev() {
                    if Self::check_lookahead_recursive(rest, input, p) {
                        return true;
                    }
                }
                false
            }
            PatternStep::WordBoundary => {
                if !crate::nfa::is_word_boundary(input, pos) {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos)
            }
            PatternStep::NotWordBoundary => {
                if crate::nfa::is_word_boundary(input, pos) {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos)
            }
            PatternStep::StartOfText => {
                if pos != 0 {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos)
            }
            PatternStep::EndOfText => {
                if !crate::nfa::at_end_or_before_final_newline(input, pos) {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos)
            }
            PatternStep::StartOfLine => {
                if !crate::nfa::at_line_start(input, pos) {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos)
            }
            PatternStep::EndOfLine => {
                if !crate::nfa::at_line_end(input, pos) {
                    return false;
                }
                Self::check_lookahead_recursive(rest, input, pos)
            }
            PatternStep::CodepointClass(cpclass, _target) => {
                if let Some((cp, len)) = Self::decode_utf8(input, pos) {
                    if cpclass.contains(cp) {
                        return Self::check_lookahead_recursive(rest, input, pos + len);
                    }
                }
                false
            }
            PatternStep::GreedyCodepointPlus(cpclass) => {
                // Must match at least one
                if let Some((cp, len)) = Self::decode_utf8(input, pos) {
                    if !cpclass.contains(cp) {
                        return false;
                    }
                    // Greedily match as many as possible
                    let mut end = pos + len;
                    while let Some((cp2, len2)) = Self::decode_utf8(input, end) {
                        if !cpclass.contains(cp2) {
                            break;
                        }
                        end += len2;
                    }
                    // Backtrack from longest match to shortest (at least 1),
                    // walking the run's codepoint boundaries backwards rather
                    // than recording them on the way in — see
                    // [`TaggedNfa::prev_boundary`]. The first codepoint is
                    // mandatory, so `pos + len` is the shortest run allowed.
                    let min_pos = pos + len;
                    let mut boundary = end;
                    loop {
                        if Self::check_lookahead_recursive(rest, input, boundary) {
                            return true;
                        }
                        if boundary <= min_pos {
                            return false;
                        }
                        boundary = Self::prev_boundary(input, boundary);
                    }
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Checks if the lookbehind pattern matches at position `pos` looking
    /// backwards, trying each candidate total width in turn.
    ///
    /// A lookbehind is a zero-width boolean assertion that records no captures,
    /// so the candidates can be tried in any order and OR-ed: none of them can
    /// change where the surrounding match starts or ends, and leftmost/longest
    /// is unaffected. A candidate that would land mid-codepoint makes
    /// `pos - width` a continuation byte, which `check_lookbehind_at` rejects
    /// when it decodes there, so a wrong candidate cannot produce a false
    /// positive — it can only cost one wasted walk.
    fn check_lookbehind(steps: &[PatternStep], input: &[u8], pos: usize, widths: &[usize]) -> bool {
        widths
            .iter()
            .any(|&width| Self::check_lookbehind_at(steps, input, pos, width))
    }

    /// Checks if the lookbehind pattern matches at position `pos` looking
    /// backwards, assuming it consumes exactly `min_len` bytes.
    fn check_lookbehind_at(
        steps: &[PatternStep],
        input: &[u8],
        pos: usize,
        min_len: usize,
    ) -> bool {
        // Cannot match if not enough characters behind
        if pos < min_len {
            return false;
        }
        // Check pattern backwards from pos
        let start = pos - min_len;
        let mut p = start;
        for step in steps {
            match step {
                PatternStep::Byte(b) => {
                    if p >= pos || input[p] != *b {
                        return false;
                    }
                    p += 1;
                }
                PatternStep::ByteClass(byte_class) => {
                    if p >= pos {
                        return false;
                    }
                    let byte = input[p];
                    if !byte_class.contains(byte) {
                        return false;
                    }
                    p += 1;
                }
                PatternStep::WordBoundary => {
                    if !crate::nfa::is_word_boundary(input, p) {
                        return false;
                    }
                }
                PatternStep::NotWordBoundary => {
                    if crate::nfa::is_word_boundary(input, p) {
                        return false;
                    }
                }
                PatternStep::StartOfText => {
                    if p != 0 {
                        return false;
                    }
                }
                PatternStep::EndOfText => {
                    if !crate::nfa::at_end_or_before_final_newline(input, p) {
                        return false;
                    }
                }
                PatternStep::StartOfLine => {
                    if !crate::nfa::at_line_start(input, p) {
                        return false;
                    }
                }
                PatternStep::EndOfLine => {
                    if !crate::nfa::at_line_end(input, p) {
                        return false;
                    }
                }
                PatternStep::CodepointClass(cpclass, _target) => {
                    if let Some((cp, len)) = Self::decode_utf8(input, p) {
                        if p + len > pos {
                            return false; // Would go past lookbehind boundary
                        }
                        if !cpclass.contains(cp) {
                            return false;
                        }
                        p += len;
                    } else {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        // Match succeeds if we consumed exactly the required characters
        p == pos
    }

    /// The start of the UTF-8 codepoint that ends at `end`.
    ///
    /// Lets a greedy codepoint run backtrack without recording where it has
    /// been. UTF-8 is self-synchronizing — a continuation byte is `10xxxxxx`
    /// and a leading byte never is — so the previous boundary is found by
    /// stepping back over continuation bytes, in O(1) for the ≤4 bytes a
    /// codepoint can occupy.
    ///
    /// This replaced a `Vec` of boundaries collected on the way in, which grew
    /// by doubling with the length of the run: matching `\p{L}+` against a
    /// 400-character word cost nine reallocations, on every call, for a
    /// quantifier that in the common case never backtracks at all.
    ///
    /// `end` must be a codepoint boundary strictly inside `input` (every call
    /// site derives it from a decoded codepoint, and only steps back while it
    /// is above the run's mandatory first codepoint).
    #[inline]
    fn prev_boundary(input: &[u8], end: usize) -> usize {
        let mut i = end - 1;
        while i > 0 && (input[i] & 0xC0) == 0x80 {
            i -= 1;
        }
        i
    }

    /// Decodes one UTF-8 codepoint from input at the given position.
    /// Returns (codepoint, byte_length) on success, None if invalid UTF-8 or at end.
    #[inline]
    fn decode_utf8(input: &[u8], pos: usize) -> Option<(u32, usize)> {
        if pos >= input.len() {
            return None;
        }
        let b0 = input[pos];
        if b0 < 0x80 {
            // ASCII: single byte
            return Some((b0 as u32, 1));
        } else if b0 < 0xC0 {
            // Invalid: continuation byte at start
            return None;
        } else if b0 < 0xE0 {
            // 2-byte sequence
            if pos + 1 >= input.len() {
                return None;
            }
            let b1 = input[pos + 1];
            if (b1 & 0xC0) != 0x80 {
                return None;
            }
            let cp = ((b0 as u32 & 0x1F) << 6) | (b1 as u32 & 0x3F);
            return Some((cp, 2));
        } else if b0 < 0xF0 {
            // 3-byte sequence
            if pos + 2 >= input.len() {
                return None;
            }
            let b1 = input[pos + 1];
            let b2 = input[pos + 2];
            if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 {
                return None;
            }
            let cp = ((b0 as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6) | (b2 as u32 & 0x3F);
            return Some((cp, 3));
        } else if b0 < 0xF8 {
            // 4-byte sequence
            if pos + 3 >= input.len() {
                return None;
            }
            let b1 = input[pos + 1];
            let b2 = input[pos + 2];
            let b3 = input[pos + 3];
            if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 || (b3 & 0xC0) != 0x80 {
                return None;
            }
            let cp = ((b0 as u32 & 0x07) << 18)
                | ((b1 as u32 & 0x3F) << 12)
                | ((b2 as u32 & 0x3F) << 6)
                | (b3 as u32 & 0x3F);
            return Some((cp, 4));
        }
        None
    }
}

#[cfg(test)]
mod multi_width_lookbehind_tests {
    use super::*;
    use crate::nfa::tagged::steps::StepExtractor;

    /// Extracts the step program for `pattern`, failing the test if the
    /// extractor declines it.
    ///
    /// The assertion is half the point of these tests: a declined extraction is
    /// invisible from the public API — the engine silently falls back to the
    /// PikeVm and still answers correctly — so a test that only checked match
    /// results would keep passing with the step path dead. Asserting extraction
    /// here pins that `\s`-style multi-width lookbehinds stay on the step
    /// engine, and every `find` below then really runs `check_lookbehind`.
    fn steps_for(pattern: &str) -> Vec<PatternStep> {
        let ast = crate::parser::parse(pattern).unwrap();
        let hir = crate::hir::translate(&ast).unwrap();
        let nfa = crate::nfa::compile(&hir).unwrap();
        StepExtractor::new(&nfa)
            .extract()
            .unwrap_or_else(|| panic!("step extraction declined {pattern:?}"))
    }

    #[test]
    fn positive_lookbehind_accepts_every_whitespace_width() {
        // `\s` is the full Unicode White_Space set, whose members encode to 1, 2
        // or 3 UTF-8 bytes. Each width must be recognised behind the assertion.
        let steps = steps_for(r"(?<=\s)\w+");
        // ASCII space: one byte.
        assert_eq!(TaggedNfa::find(&steps, " ab".as_bytes()), Some((1, 3)));
        // U+00A0 NO-BREAK SPACE: two bytes.
        assert_eq!(TaggedNfa::find(&steps, "\u{A0}ab".as_bytes()), Some((2, 4)));
        // U+2003 EM SPACE and U+3000 IDEOGRAPHIC SPACE: three bytes.
        assert_eq!(
            TaggedNfa::find(&steps, "\u{2003}ab".as_bytes()),
            Some((3, 5))
        );
        assert_eq!(
            TaggedNfa::find(&steps, "\u{3000}ab".as_bytes()),
            Some((3, 5))
        );
        // A non-space of each width in front: no candidate may succeed, and the
        // wider candidates must not be fooled by landing on a continuation byte.
        assert_eq!(TaggedNfa::find(&steps, ",ab".as_bytes()), None);
        assert_eq!(TaggedNfa::find(&steps, "\u{E9}ab".as_bytes()), None);
        assert_eq!(TaggedNfa::find(&steps, "\u{4E2D}ab".as_bytes()), None);
    }

    #[test]
    fn a_candidate_width_wider_than_the_haystack_is_skipped() {
        // At position 1 only the one-byte candidate fits; the two- and
        // three-byte ones would run off the front of the haystack.
        let steps = steps_for(r"(?<=\s)x");
        assert_eq!(TaggedNfa::find(&steps, " x".as_bytes()), Some((1, 2)));
        // At position 0 no candidate fits at all.
        assert_eq!(TaggedNfa::find(&steps, "x".as_bytes()), None);
        assert_eq!(TaggedNfa::find(&steps, "".as_bytes()), None);
    }

    #[test]
    fn negative_lookbehind_negates_the_whole_candidate_set() {
        let steps = steps_for(r"(?<!\s)\w+");
        // Nothing behind the first position, so the assertion holds there.
        assert_eq!(TaggedNfa::find(&steps, "ab".as_bytes()), Some((0, 2)));
        // A space of any width behind must block that position — the two- and
        // three-byte cases only fail if the wider candidates are tried, since
        // the one-byte candidate lands on a continuation byte and rejects.
        assert_eq!(TaggedNfa::find(&steps, " ab".as_bytes()), Some((2, 3)));
        assert_eq!(TaggedNfa::find(&steps, "\u{A0}ab".as_bytes()), Some((3, 4)));
        assert_eq!(
            TaggedNfa::find(&steps, "\u{2003}ab".as_bytes()),
            Some((4, 5))
        );
    }

    #[test]
    fn widths_of_two_lookbehind_classes_combine() {
        // Two `\s` in the lookbehind: the totals are the sumset {2..=6}, and a
        // walk from the wrong total must not be mistaken for a match.
        let steps = steps_for(r"(?<=\s\s)x");
        assert_eq!(TaggedNfa::find(&steps, "  x".as_bytes()), Some((2, 3)));
        assert_eq!(TaggedNfa::find(&steps, " \u{A0}x".as_bytes()), Some((3, 4)));
        let two_em = "\u{2003}\u{2003}x";
        assert_eq!(TaggedNfa::find(&steps, two_em.as_bytes()), Some((6, 7)));
        // Only one space behind: no total may be satisfied.
        assert_eq!(TaggedNfa::find(&steps, " x".as_bytes()), None);
        assert_eq!(TaggedNfa::find(&steps, "\u{2003}x".as_bytes()), None);
    }
}
