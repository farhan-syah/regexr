//! Shared types for backtracking engine.
//!
//! Contains the bytecode instructions and helper functions used by both
//! interpreter and JIT backends.

/// Steps a backtracking search may take before it gives up.
///
/// Only patterns with backreferences reach a backtracking engine, and matching
/// a backreference is NP-hard in general: there is no polynomial bound to fall
/// back on, so a budget is what makes the search terminate. It is set high
/// enough that ordinary patterns never approach it — a linear scan of a 1 MiB
/// input costs a few million steps — and a pattern that does exhaust it is
/// reported through [`crate::error::ErrorKind::MatchLimitExceeded`] rather than
/// being quietly answered "no match".
///
/// The value bounds the worst case at a few hundred milliseconds. A unit is one
/// choice point — the quantity that actually explodes — in both the interpreter
/// and the generated code; the forward scanning between two choice points is
/// already bounded by the input and the program.
pub const DEFAULT_BACKTRACK_LIMIT: u64 = 100_000_000;

/// Spans of every capture group of a match, indexed by group number; index 0 is
/// the overall match. `None` means the group did not participate.
pub type CaptureSlots = Vec<Option<(usize, usize)>>;

/// A backtracking search stopped because it ran out of steps.
///
/// Distinct from "no match": the search never finished, so the answer is
/// unknown. Callers turn this into
/// [`crate::error::ErrorKind::MatchLimitExceeded`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetExhausted;

/// Bytecode instructions for the backtracking VM.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum Op {
    /// Match a single byte.
    Byte(u8),
    /// Match a byte range [lo, hi].
    ByteRange(u8, u8),
    /// Match any byte except these (negated class with up to 4 ranges inline).
    NotByteRanges { count: u8, ranges: [(u8, u8); 4] },
    /// Match any byte in these ranges (up to 4 ranges inline).
    ByteRanges { count: u8, ranges: [(u8, u8); 4] },
    /// Match any byte in a large class (index into byte_classes table).
    ByteClassRef { index: u16, negated: bool },
    /// Match a Unicode codepoint range.
    CpRange(u32, u32),
    /// Negated Unicode codepoint range.
    NotCpRange(u32, u32),
    /// Match a Unicode codepoint class (index into cp_classes table).
    CpClassRef { index: u16, negated: bool },
    /// Match any byte.
    Any,
    /// Split: try pc+1 first, on backtrack try target.
    Split(u32),
    /// Jump to target.
    Jump(u32),
    /// Save position to capture slot.
    Save(u16),
    /// Match (success).
    Match,
    /// Start of text (`^` without `(?m)`, `\A`).
    StartAnchor,
    /// Start of a line (`^` under `(?m)`): the start of the text, or just after
    /// a newline.
    StartLineAnchor,
    /// End of text (`$` without `(?m)`): the end of the text, or immediately
    /// before a trailing newline — the PCRE/Python rule the rest of the crate
    /// follows, so `a$` matches "a" in "a\n".
    EndAnchor,
    /// End of a line (`$` under `(?m)`): the end of the text, or just before any
    /// newline.
    EndLineAnchor,
    /// Word boundary.
    WordBoundary,
    /// Not word boundary.
    NotWordBoundary,
    /// Backreference to group N.
    Backref(u16),
    /// Record the current position in progress register N, on entry to a
    /// repetition body.
    MarkPos(u16),
    /// Reject a repetition iteration that consumed nothing.
    ///
    /// Backtracks if the position still equals the one [`Op::MarkPos`] wrote to
    /// register `reg`, so the loop falls back to its exit branch. Without it a
    /// body that can match empty re-enters at a position it can never advance
    /// past and the search never terminates.
    ///
    /// Rejecting the iteration — rather than keeping it and then leaving the
    /// loop — is what makes the empty-body case agree with the executable spec
    /// in `crate::reference` and with the PikeVM.
    AssertProgress(u16),
}

/// Decode UTF-8 codepoint from bytes.
/// Returns (codepoint, byte_length) if valid.
#[inline]
pub fn decode_utf8(bytes: &[u8]) -> Option<(u32, usize)> {
    if bytes.is_empty() {
        return None;
    }
    let b0 = bytes[0];
    if b0 < 0x80 {
        return Some((b0 as u32, 1));
    }
    if bytes.len() < 2 {
        return None;
    }
    let b1 = bytes[1];
    if (b0 & 0xE0) == 0xC0 {
        return Some((((b0 as u32 & 0x1F) << 6) | (b1 as u32 & 0x3F), 2));
    }
    if bytes.len() < 3 {
        return None;
    }
    let b2 = bytes[2];
    if (b0 & 0xF0) == 0xE0 {
        return Some((
            ((b0 as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6) | (b2 as u32 & 0x3F),
            3,
        ));
    }
    if bytes.len() < 4 {
        return None;
    }
    let b3 = bytes[3];
    if (b0 & 0xF8) == 0xF0 {
        return Some((
            ((b0 as u32 & 0x07) << 18)
                | ((b1 as u32 & 0x3F) << 12)
                | ((b2 as u32 & 0x3F) << 6)
                | (b3 as u32 & 0x3F),
            4,
        ));
    }
    None
}

/// Returns true if the byte is a word character (alphanumeric or underscore).
#[inline]
pub fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
