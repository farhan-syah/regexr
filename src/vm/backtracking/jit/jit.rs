//! Backtracking JIT public API.
//!
//! This module contains the BacktrackingJit struct and the public compile function.

use crate::error::{Error, ErrorKind, Result};
use crate::hir::Hir;

use super::super::interpreter::BacktrackingVm;
use super::super::shared::{BudgetExhausted, DEFAULT_BACKTRACK_LIMIT};

use dynasmrt::ExecutableBuffer;

/// Why the generated code stopped without an answer.
enum Halt {
    /// Its choice-point stack filled; the interpreter can carry on.
    Retry,
    /// The caller's step budget ran out; nothing else to try.
    Budget,
}

#[cfg(target_arch = "x86_64")]
use super::x86_64::BacktrackingCompiler;

#[cfg(target_arch = "aarch64")]
use super::aarch64::BacktrackingCompiler;

// Platform-specific function pointer type
#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
type MatchFn = unsafe extern "win64" fn(*const u8, usize, *mut i64, u64) -> i64;
#[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
type MatchFn = unsafe extern "sysv64" fn(*const u8, usize, *mut i64, u64) -> i64;
// ARM64 uses AAPCS64 on all platforms (extern "C")
#[cfg(target_arch = "aarch64")]
type MatchFn = unsafe extern "C" fn(*const u8, usize, *mut i64, u64) -> i64;

/// Returned by the generated code when its choice-point stack filled up.
///
/// The generated code keeps choice points in a fixed stack frame, which a deep
/// enough backtrack will fill. That is not "no match" — the search never
/// finished — so it is reported distinctly from the `-1` no-match result and
/// the caller re-runs on the interpreter, whose stack grows with demand.
pub(super) const STACK_EXHAUSTED: i64 = -2;

/// Returned by the generated code when the caller's step budget ran out.
///
/// Every choice point costs one step, so a pattern that explores exponentially
/// many of them stops here. Unlike [`STACK_EXHAUSTED`] there is nothing to
/// retry: the interpreter would spend the same budget on the same search, so
/// this is reported straight to the caller as [`BudgetExhausted`].
pub(super) const BUDGET_EXHAUSTED: i64 = -3;

/// A compiled backtracking regex.
pub struct BacktrackingJit {
    /// Executable code buffer (kept alive for the function pointer).
    #[allow(dead_code)]
    pub(super) code: ExecutableBuffer,
    /// Entry point for matching.
    pub(super) match_fn: MatchFn,
    /// Number of capture groups.
    pub(super) capture_count: u32,
    /// The same pattern on the interpreter, which every search can fall back to.
    ///
    /// It answers two cases the generated code cannot. First, a search that
    /// starts past position 0 when the pattern depends on what precedes the
    /// match — a start anchor (`^` / `\A`) or `\b`/`\B` — because the generated
    /// code has no start-offset parameter, so such a search would have to run on
    /// a slice, which makes the slice's first byte look like the start of text.
    /// Second, a search that fills the generated code's fixed choice-point stack
    /// (see [`STACK_EXHAUSTED`]); the interpreter's stack grows with demand.
    ///
    /// Lookbehind is not in the first list because it cannot reach this engine:
    /// `compile_with_jit` only routes a pattern here when it has backreferences
    /// and *no* lookaround.
    pub(super) vm: BacktrackingVm,
    /// Whether a resumed search has to go to [`Self::vm`] to see its left context.
    pub(super) needs_left_context: bool,
}

impl BacktrackingJit {
    /// Returns whether the pattern matches anywhere in the input.
    pub fn is_match(&self, input: &[u8]) -> bool {
        self.find(input).is_some()
    }

    /// Runs the generated code over the whole of `input`.
    ///
    /// `Ok(Some(..))`/`Ok(None)` is a finished search; `Err(Retry)` means it ran
    /// out of choice-point stack and the caller must ask the interpreter, and
    /// `Err(Budget)` means the caller's own limit stopped it.
    fn run(
        &self,
        input: &[u8],
        limit: u64,
    ) -> std::result::Result<Option<Vec<Option<(usize, usize)>>>, Halt> {
        let num_slots = (self.capture_count as usize + 1) * 2;
        let mut buf: Vec<i64> = vec![-1; num_slots];

        let result =
            unsafe { (self.match_fn)(input.as_ptr(), input.len(), buf.as_mut_ptr(), limit) };

        if result == STACK_EXHAUSTED {
            return Err(Halt::Retry);
        }
        if result == BUDGET_EXHAUSTED {
            return Err(Halt::Budget);
        }
        if result < 0 {
            return Ok(None);
        }
        let mut captures = Vec::with_capacity(self.capture_count as usize + 1);
        for i in 0..=self.capture_count as usize {
            let (start, end) = (buf[i * 2], buf[i * 2 + 1]);
            if start >= 0 && end >= 0 {
                captures.push(Some((start as usize, end as usize)));
            } else {
                captures.push(None);
            }
        }
        Ok(Some(captures))
    }

    /// Finds the first match, returning (start, end).
    pub fn find(&self, input: &[u8]) -> Option<(usize, usize)> {
        self.captures(input).and_then(|caps| caps[0])
    }

    /// Returns capture groups for the first match.
    pub fn captures(&self, input: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
        self.try_captures_from(input, 0, DEFAULT_BACKTRACK_LIMIT)
            .unwrap_or(None)
    }

    /// [`Self::captures_from`] under an explicit step budget.
    ///
    /// The generated code spends one step per choice point and stops when the
    /// budget runs out. A search that instead fills its fixed choice-point stack
    /// is not out of budget, so it continues on the interpreter, which grows its
    /// stack rather than capping it, under what remains of the same budget.
    pub fn try_captures_from(
        &self,
        input: &[u8],
        from: usize,
        limit: u64,
    ) -> std::result::Result<Option<Vec<Option<(usize, usize)>>>, BudgetExhausted> {
        if from > input.len() {
            return Ok(None);
        }
        if self.needs_left_context && from > 0 {
            return self.vm.try_captures_from(input, from, limit);
        }
        let (haystack, offset) = if from == 0 {
            (input, 0)
        } else {
            (&input[from..], from)
        };
        let caps = match self.run(haystack, limit) {
            Ok(caps) => caps,
            // A full stack is not an answer, so the search continues on the
            // interpreter, which grows its stack instead of capping it.
            Err(Halt::Retry) => self.vm.try_captures_from(haystack, 0, limit)?,
            Err(Halt::Budget) => return Err(BudgetExhausted),
        };
        Ok(caps.map(|caps| {
            caps.into_iter()
                .map(|slot| slot.map(|(s, e)| (s + offset, e + offset)))
                .collect()
        }))
    }

    /// Finds a match starting at or after the given position.
    pub fn find_at(&self, input: &[u8], start: usize) -> Option<(usize, usize)> {
        self.find_from(input, start)
    }

    /// Finds the leftmost match starting at or after `from`.
    ///
    /// Patterns that read left context go to the interpreter, which matches at an
    /// absolute position with the full input visible (see [`Self::vm`]);
    /// everything else can safely run on the suffix slice, whose end — and so
    /// `$`/`\Z` — is the end of the input.
    pub fn find_from(&self, input: &[u8], from: usize) -> Option<(usize, usize)> {
        self.captures_from(input, from).and_then(|caps| caps[0])
    }

    /// Returns capture groups for the leftmost match starting at or after `from`.
    pub fn captures_from(&self, input: &[u8], from: usize) -> Option<Vec<Option<(usize, usize)>>> {
        self.try_captures_from(input, from, DEFAULT_BACKTRACK_LIMIT)
            .unwrap_or(None)
    }

    /// Debug method to see raw results
    #[cfg(test)]
    pub fn debug_match(&self, input: &[u8]) -> (i64, Vec<i64>) {
        let num_slots = (self.capture_count as usize + 1) * 2;
        let mut captures: Vec<i64> = vec![-1; num_slots];

        let result = unsafe {
            (self.match_fn)(
                input.as_ptr(),
                input.len(),
                captures.as_mut_ptr(),
                DEFAULT_BACKTRACK_LIMIT,
            )
        };

        (result, captures)
    }
}

/// Compiles a HIR pattern to a backtracking JIT.
///
/// Returns an error for patterns that require complex backtracking within captures,
/// such as `(a+)\1` where the backref refers to a capture containing an unbounded
/// repetition. These patterns should fall back to PikeVM.
pub fn compile_backtracking(hir: &Hir) -> Result<BacktrackingJit> {
    // Note: We now support backrefs to captures with unbounded repetitions like (\w+)\1.
    // The greedy repetition code properly saves choice points and updates capture ends
    // during backtracking.

    // An unbounded loop over a body that can match empty needs a progress guard,
    // and the guard needs per-loop state that survives backtracking — which the
    // generated code has no room for, since a choice point only restores the
    // position, the start offset and the iteration count. The interpreter keeps
    // that state properly (`Op::MarkPos` / `Op::ExitIfNoProgress`), so these
    // patterns compile there instead of looping forever here.
    if crate::hir::has_unbounded_nullable_repeat(&hir.expr) {
        return Err(Error::new(
            ErrorKind::Jit("unbounded repetition over a nullable body".to_string()),
            "",
        ));
    }

    let compiler = BacktrackingCompiler::new(hir)?;
    let mut jit = compiler.compile()?;

    // A resumed search can only slice the input when nothing in the pattern
    // depends on what precedes the match start.
    let props = &hir.props;
    jit.needs_left_context =
        props.has_start_anchor || props.has_multiline_anchors || props.has_word_boundary;

    Ok(jit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::translate;
    use crate::parser::parse;

    fn compile_pattern(pattern: &str) -> Result<BacktrackingJit> {
        let ast = parse(pattern)?;
        let hir = translate(&ast)?;
        compile_backtracking(&hir)
    }

    #[test]
    fn test_literal_debug() {
        let jit = compile_pattern("hello").unwrap();
        let (result, caps) = jit.debug_match(b"hello");
        println!("result: {}, caps: {:?}", result, caps);
        assert!(result >= 0, "Expected match, got result={}", result);
    }

    #[test]
    fn test_literal() {
        let jit = compile_pattern("hello").unwrap();
        assert!(jit.is_match(b"hello"));
        assert!(jit.is_match(b"say hello world"));
        assert!(!jit.is_match(b"helo"));
    }

    #[test]
    fn test_simple_backref() {
        let jit = compile_pattern(r"(a)\1").unwrap();

        // Debug: check what we match
        let (result_aa, caps_aa) = jit.debug_match(b"aa");
        println!("(a)\\1 on 'aa': result={}, caps={:?}", result_aa, caps_aa);

        let (result_ab, caps_ab) = jit.debug_match(b"ab");
        println!("(a)\\1 on 'ab': result={}, caps={:?}", result_ab, caps_ab);

        let (result_a, caps_a) = jit.debug_match(b"a");
        println!("(a)\\1 on 'a': result={}, caps={:?}", result_a, caps_a);

        assert!(jit.is_match(b"aa"), "Should match 'aa'");
        assert!(!jit.is_match(b"ab"), "Should NOT match 'ab'");
        assert!(!jit.is_match(b"a"), "Should NOT match 'a'");
    }

    #[test]
    fn test_quoted_string() {
        let jit = compile_pattern(r#"(['"])[^'"]*\1"#).unwrap();

        let (r1, c1) = jit.debug_match(br#""hello""#);
        println!(r#"['"][^'"]*\1 on "hello": result={}, caps={:?}"#, r1, c1);

        let (r2, c2) = jit.debug_match(b"'world'");
        println!(r#"['"][^'"]*\1 on 'world': result={}, caps={:?}"#, r2, c2);

        let (r3, c3) = jit.debug_match(br#""mixed'"#);
        println!(r#"['"][^'"]*\1 on "mixed': result={}, caps={:?}"#, r3, c3);

        let (r4, c4) = jit.debug_match(b"'mixed\"");
        println!(r#"['"][^'"]*\1 on 'mixed": result={}, caps={:?}"#, r4, c4);

        assert!(jit.is_match(br#""hello""#), "Should match \"hello\"");
        assert!(jit.is_match(b"'world'"), "Should match 'world'");
        assert!(!jit.is_match(br#""mixed'"#), "Should NOT match \"mixed'");
        assert!(!jit.is_match(b"'mixed\""), "Should NOT match 'mixed\"");
    }

    #[test]
    fn test_alternation_backref() {
        let jit = compile_pattern(r"(a|b)\1").unwrap();

        let (result_aa, caps_aa) = jit.debug_match(b"aa");
        println!("(a|b)\\1 on 'aa': result={}, caps={:?}", result_aa, caps_aa);

        let (result_bb, caps_bb) = jit.debug_match(b"bb");
        println!("(a|b)\\1 on 'bb': result={}, caps={:?}", result_bb, caps_bb);

        let (result_ab, caps_ab) = jit.debug_match(b"ab");
        println!("(a|b)\\1 on 'ab': result={}, caps={:?}", result_ab, caps_ab);

        let (result_ba, caps_ba) = jit.debug_match(b"ba");
        println!("(a|b)\\1 on 'ba': result={}, caps={:?}", result_ba, caps_ba);

        assert!(jit.is_match(b"aa"), "Should match 'aa'");
        assert!(jit.is_match(b"bb"), "Should match 'bb'");
        assert!(!jit.is_match(b"ab"), "Should NOT match 'ab'");
        assert!(!jit.is_match(b"ba"), "Should NOT match 'ba'");
    }

    #[test]
    fn test_captures() {
        let jit = compile_pattern(r"(a)(b)\2\1").unwrap();
        let caps = jit.captures(b"abba").unwrap();
        assert_eq!(caps[0], Some((0, 4))); // Full match
        assert_eq!(caps[1], Some((0, 1))); // Group 1: "a"
        assert_eq!(caps[2], Some((1, 2))); // Group 2: "b"
    }
}
