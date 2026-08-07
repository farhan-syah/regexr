//! Driver for the JIT-compiled one-pass capture engine.
//!
//! [`super::OnePass`] already answers captures in a single scan, but as an
//! interpreter: every byte re-reads the closure, its match list and its
//! transition list, and re-decides what was fixed when the pattern was compiled.
//! Compiling the same machine turns each closure into a block whose match is two
//! stores and whose transitions are direct jumps.
//!
//! The generated code writes raw slot pairs — `[2n]` and `[2n + 1]` for group
//! `n`, with `-1` for a group the match never entered — which is the same shape
//! the backtracking JIT uses and avoids building an `Option` per group in
//! generated code.
//!
//! Nothing here decides *whether* a pattern is one-pass; that is
//! [`super::OnePass::compile`]'s job, and this compiles what it produced or
//! declines and leaves the interpreter in place.

use dynasmrt::ExecutableBuffer;

use super::OnePass;

/// The generated entry point: `(input, len, start, slots) -> match end or -1`.
pub(super) type MatchFn = unsafe extern "sysv64" fn(*const u8, usize, usize, *mut i64) -> i64;

/// A finalized code buffer and its entry point.
pub(super) struct Compiled {
    /// Kept alive for the function pointer.
    #[allow(dead_code)]
    pub(super) code: ExecutableBuffer,
    pub(super) run: MatchFn,
}

/// A one-pass capture engine compiled to native code.
pub struct OnePassJit {
    compiled: Compiled,
    slot_count: usize,
}

impl std::fmt::Debug for OnePassJit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnePassJit")
            .field("slot_count", &self.slot_count)
            .finish()
    }
}

// The generated code is position-independent and holds no interior mutability;
// running it only reads the input and writes the caller's slot buffer.
unsafe impl Send for OnePassJit {}
unsafe impl Sync for OnePassJit {}

impl OnePassJit {
    /// Compiles `one_pass`, or returns `None` when this target or this pattern
    /// is not one the emitter handles.
    pub fn compile(one_pass: &OnePass) -> Option<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            if !super::x86_64::is_supported(one_pass) {
                return None;
            }
            Some(Self {
                compiled: super::x86_64::compile(one_pass)?,
                slot_count: one_pass.slot_count,
            })
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = one_pass;
            None
        }
    }

    /// Number of slots the buffer passed to [`Self::captures_at_into`] must hold.
    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    /// Capture slots for a match beginning exactly at `start`.
    ///
    /// Mirrors [`OnePass::captures_at_into`]: `slots` is filled only when this
    /// returns true, and a group the match never entered stays `None`.
    pub fn captures_at_into(
        &self,
        input: &[u8],
        start: usize,
        slots: &mut [Option<(usize, usize)>],
    ) -> bool {
        if start > input.len() || slots.len() != self.slot_count {
            return false;
        }
        let mut raw = [-1i64; RAW_SLOTS];
        let Some(raw) = raw.get_mut(..self.slot_count * 2) else {
            return false;
        };

        let end =
            unsafe { (self.compiled.run)(input.as_ptr(), input.len(), start, raw.as_mut_ptr()) };
        if end < 0 {
            return false;
        }

        for (slot, pair) in slots.iter_mut().zip(raw.chunks_exact(2)) {
            *slot = match (pair.first(), pair.get(1)) {
                (Some(&group_start), Some(&group_end)) if group_start >= 0 && group_end >= 0 => {
                    Some((group_start as usize, group_end as usize))
                }
                _ => None,
            };
        }
        true
    }
}

/// Raw slot entries held on the stack, twice [`MAX_SLOTS`] because each group
/// occupies a start and an end.
const RAW_SLOTS: usize = 64;
