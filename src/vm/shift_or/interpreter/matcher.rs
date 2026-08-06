//! Shift-Or interpreter matching implementation.

use super::super::ShiftOr;

/// Interpreter for Shift-Or matching.
///
/// A borrowing view over a compiled [`ShiftOr`], which owns the matching itself.
/// This type exists so callers can name "the interpreter backend" alongside the
/// JIT one in [`super::super::ShiftOrEngine`]; it deliberately holds no matching
/// logic of its own, because a second copy of the bit-parallel scan is a second
/// place for a fix to have to land — and to be forgotten.
pub struct ShiftOrInterpreter<'a> {
    shift_or: &'a ShiftOr,
}

impl<'a> ShiftOrInterpreter<'a> {
    /// Creates a new interpreter for the given ShiftOr.
    pub fn new(shift_or: &'a ShiftOr) -> Self {
        Self { shift_or }
    }

    /// Returns true if the pattern matches anywhere in the input.
    #[inline]
    pub fn is_match(&self, input: &[u8]) -> bool {
        self.shift_or.is_match(input)
    }

    /// Finds the first match, returning (start, end).
    #[inline]
    pub fn find(&self, input: &[u8]) -> Option<(usize, usize)> {
        self.shift_or.find(input)
    }

    /// Finds a match starting at or after the given position.
    /// Returns (start, end) if found.
    #[inline]
    pub fn find_at(&self, input: &[u8], pos: usize) -> Option<(usize, usize)> {
        self.shift_or.find_at(input, pos)
    }

    /// Tries to match at exactly the given position.
    /// Returns (start, end) if matched, None otherwise.
    /// Use this when you know the match should start at exactly `pos` (e.g., from a prefilter).
    #[inline]
    pub fn try_match_at(&self, input: &[u8], pos: usize) -> Option<(usize, usize)> {
        self.shift_or.try_match_at(input, pos)
    }
}
