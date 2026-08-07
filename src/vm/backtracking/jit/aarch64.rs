//! AArch64 (ARM64) code generation for backtracking JIT.
//!
//! This module implements a PCRE-style backtracking JIT that generates native AArch64
//! code for patterns containing backreferences.
//!
//! # Register Allocation (AAPCS64)
//!
//! | Register | Purpose |
//! |----------|---------|
//! | x19 | Input base pointer (callee-saved) |
//! | x20 | Input length (callee-saved) |
//! | x21 | Current position in input (callee-saved) |
//! | x22 | Captures base pointer (callee-saved) |
//! | x23 | Start position for current match attempt (callee-saved) |
//! | x24 | Scratch for comparisons (callee-saved) |
//! | x25 | Loop counter (callee-saved) |
//! | x26 | Backtrack stack pointer (callee-saved) |
//! | x29 | Frame pointer |
//! | x30 | Link register |
//! | x0-x15 | Scratch / arguments / return |

use crate::error::{Error, ErrorKind, Result};
use crate::hir::{Hir, HirAnchor, HirClass, HirExpr};

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};

use super::jit::{BacktrackingJit, BUDGET_EXHAUSTED, STACK_EXHAUSTED};

// ARM64 backtracking JIT enabled
const ARM64_BACKTRACKING_JIT_ENABLED: bool = true;

// The exhaustion sentinels are emitted as `movn` literals below (`movn xN, k`
// yields `!k`), so they cannot reference the constants directly. Pin them.
const _: () = assert!(STACK_EXHAUSTED == -2 && BUDGET_EXHAUSTED == -3);

/// The backtracking JIT compiler for ARM64.
pub(super) struct BacktrackingCompiler {
    /// The assembler.
    asm: dynasmrt::aarch64::Assembler,
    /// The HIR to compile.
    hir: Hir,
    /// Label for the backtrack handler.
    backtrack_label: dynasmrt::DynamicLabel,
    /// Label for successful match.
    match_success_label: dynasmrt::DynamicLabel,
    /// Label for no match found.
    no_match_label: dynasmrt::DynamicLabel,
    /// Label for trying next start position.
    next_start_label: dynasmrt::DynamicLabel,
    /// Label for "the choice-point stack is full".
    ///
    /// The stack is a fixed frame, so a search that needs more choice points
    /// than it holds has to stop rather than write past the end of it. Jumping
    /// here returns `STACK_EXHAUSTED`, and the caller re-runs the search on the
    /// interpreter, whose stack grows.
    stack_exhausted_label: dynasmrt::DynamicLabel,
    /// Label for "the step budget is spent".
    ///
    /// Every choice point costs a step, so a search that explores exponentially
    /// many of them runs the budget down and stops here, returning
    /// `BUDGET_EXHAUSTED`. Unlike a full stack this is not worth retrying on the
    /// interpreter — it is the caller's limit, and the caller is told.
    budget_exhausted_label: dynasmrt::DynamicLabel,
    /// Number of capture groups.
    capture_count: u32,
    /// Current capture index being filled.
    current_capture: Option<u32>,
    /// Byte-set tables to emit as data once the code is complete.
    byte_set_tables: Vec<(dynasmrt::DynamicLabel, crate::literal::ByteSet)>,
}

impl BacktrackingCompiler {
    pub(super) fn new(hir: &Hir) -> Result<Self> {
        let mut asm = dynasmrt::aarch64::Assembler::new().map_err(|e| {
            Error::new(
                ErrorKind::Jit(format!("Failed to create assembler: {:?}", e)),
                "",
            )
        })?;

        let backtrack_label = asm.new_dynamic_label();
        let match_success_label = asm.new_dynamic_label();
        let no_match_label = asm.new_dynamic_label();
        let next_start_label = asm.new_dynamic_label();
        let stack_exhausted_label = asm.new_dynamic_label();
        let budget_exhausted_label = asm.new_dynamic_label();

        Ok(Self {
            asm,
            hir: hir.clone(),
            backtrack_label,
            match_success_label,
            no_match_label,
            next_start_label,
            stack_exhausted_label,
            budget_exhausted_label,
            capture_count: hir.props.capture_count,
            current_capture: None,
            byte_set_tables: Vec::new(),
        })
    }

    pub(super) fn compile(mut self) -> Result<BacktrackingJit> {
        // ARM64 backtracking JIT is disabled until assembly is fully debugged
        if !ARM64_BACKTRACKING_JIT_ENABLED {
            return Err(Error::new(
                ErrorKind::Jit("ARM64 backtracking JIT temporarily disabled".to_string()),
                "",
            ));
        }

        let entry_offset = self.asm.offset();

        self.emit_prologue();
        self.emit_main_loop()?;
        self.emit_pattern(&self.hir.expr.clone())?;

        // After pattern matches, jump to success
        dynasm!(self.asm
            ; .arch aarch64
            ; b =>self.match_success_label
        );

        self.emit_backtrack_handler();
        self.emit_success_handler();

        // Data, so it follows every reachable instruction.
        self.emit_byte_set_tables();

        let code = self
            .asm
            .finalize()
            .map_err(|e| Error::new(ErrorKind::Jit(format!("Failed to finalize: {:?}", e)), ""))?;

        // ARM64 uses AAPCS64 calling convention (extern "C")
        let match_fn: unsafe extern "C" fn(*const u8, usize, *mut i64, u64) -> i64 =
            unsafe { std::mem::transmute(code.ptr(entry_offset)) };

        Ok(BacktrackingJit {
            code,
            match_fn,
            capture_count: self.capture_count,
            vm: crate::vm::backtracking::BacktrackingVm::new(&self.hir),
            // Set by `compile_backtracking` when the pattern reads left context.
            needs_left_context: false,
        })
    }

    /// Advances the start position past every byte no match can begin with.
    ///
    /// Without this the outer loop pays a full attempt — the capture reset, the
    /// stack reset and the first element's own test — at every position in the
    /// input. A pattern whose first byte is constrained can reject most of them
    /// with one table lookup instead.
    ///
    /// Emitted only when the byte set is known and is not every byte; otherwise
    /// nothing is emitted and the loop is unchanged.
    fn emit_start_byte_skip(&mut self) {
        let Some(set) = crate::literal::first_byte_set(&self.hir) else {
            return;
        };

        let table = self.asm.new_dynamic_label();
        dynasm!(self.asm
            ; .arch aarch64
            ; adr x9, =>table
            ; scan:
            // A start at the end of the input reads no byte; leave it to the
            // attempt, which is where an empty match is decided.
            ; cmp x23, x20
            ; b.hs >scanned
            ; ldrb w10, [x19, x23]
            ; ldrb w11, [x9, x10]
            ; cbnz w11, >scanned
            ; add x23, x23, #1
            ; b <scan
            ; scanned:
        );
        self.byte_set_tables.push((table, set));
    }

    /// Emits the byte-set tables read by [`Self::emit_start_byte_skip`].
    ///
    /// They are data, so they go after every instruction the function can reach.
    fn emit_byte_set_tables(&mut self) {
        for (label, set) in std::mem::take(&mut self.byte_set_tables) {
            dynasm!(self.asm
                ; .arch aarch64
                ; =>label
                ; .bytes set
            );
        }
    }

    /// Emits the function prologue.
    fn emit_prologue(&mut self) {
        // Function signature: fn(input_ptr: *const u8, input_len: usize, captures: *mut i64) -> i64
        // AAPCS64: x0 = input_ptr, x1 = input_len, x2 = captures_ptr

        dynasm!(self.asm
            ; .arch aarch64
            // Save frame pointer and link register
            ; stp x29, x30, [sp, #-16]!
            ; mov x29, sp

            // Save callee-saved registers
            ; stp x19, x20, [sp, #-16]!
            ; stp x21, x22, [sp, #-16]!
            ; stp x23, x24, [sp, #-16]!
            ; stp x25, x26, [sp, #-16]!
            ; stp x27, x28, [sp, #-16]!

            // Allocate backtrack stack (4KB) - use mov+sub since 0x1000 > 4095
            ; mov x9, 0x1000
            ; sub sp, sp, x9

            // Move arguments to callee-saved registers
            ; mov x19, x0              // x19 = input_ptr
            ; mov x20, x1              // x20 = input_len
            ; mov x22, x2              // x22 = captures_ptr
            ; sub x9, x29, #0x58       // x9 = &budget slot
            ; str x3, [x9]             // Step budget
            ; mov x23, #0              // x23 = start_pos = 0
            ; mov x26, sp              // x26 = backtrack stack pointer (bottom)

            // Initialize captures to -1 using x0 as scratch
            ; movn x0, 0
        );

        // Initialize all capture slots to -1
        let num_slots = (self.capture_count as usize + 1) * 2;
        for slot in 0..num_slots {
            let offset = (slot * 8) as u32;
            if offset < 4096 {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; str x0, [x22, offset]
                );
            } else {
                let offset64 = offset as u64;
                dynasm!(self.asm
                    ; .arch aarch64
                    ; mov x1, offset64
                    ; str x0, [x22, x1]
                );
            }
        }
    }

    /// Emits the main loop that tries each start position.
    fn emit_main_loop(&mut self) -> Result<()> {
        dynasm!(self.asm
            ; .arch aarch64
            ; =>self.next_start_label
        );

        self.emit_start_byte_skip();

        dynasm!(self.asm
            ; .arch aarch64
            // Reset captures for new attempt
            ; movn x0, 0
        );

        // Reset capture slots to -1
        let num_slots = (self.capture_count as usize + 1) * 2;
        for slot in 0..num_slots {
            let offset = (slot * 8) as u32;
            if offset < 4096 {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; str x0, [x22, offset]
                );
            } else {
                let offset64 = offset as u64;
                dynasm!(self.asm
                    ; .arch aarch64
                    ; mov x1, offset64
                    ; str x0, [x22, x1]
                );
            }
        }

        dynasm!(self.asm
            ; .arch aarch64
            // x21 = current position = start_pos
            ; mov x21, x23

            // Set group 0 start = current position
            ; str x21, [x22]

            // Reset backtrack stack to bottom
            ; mov x9, 0x1000
            ; sub x26, x29, x9
            ; sub x26, x26, #0x50     // Account for saved registers
        );

        Ok(())
    }

    /// Emits code to match the pattern.
    fn emit_pattern(&mut self, expr: &HirExpr) -> Result<()> {
        match expr {
            HirExpr::Empty => Ok(()),
            HirExpr::Literal(bytes) => self.emit_literal(bytes),
            HirExpr::Class(class) => self.emit_class(class),
            HirExpr::UnicodeCpClass(_) => Err(Error::new(
                ErrorKind::Jit(
                    "Unicode codepoint classes not supported in backtracking JIT".to_string(),
                ),
                "",
            )),
            HirExpr::Concat(parts) => {
                for part in parts {
                    self.emit_pattern(part)?;
                }
                Ok(())
            }
            HirExpr::Alt(alternatives) => self.emit_alternation(alternatives),
            HirExpr::Repeat(repeat) => {
                self.emit_repetition(&repeat.expr, repeat.min, repeat.max, repeat.greedy)
            }
            HirExpr::Capture(capture) => self.emit_capture(capture.index, &capture.expr),
            HirExpr::Backref(group) => self.emit_backref(*group),
            HirExpr::Anchor(anchor) => self.emit_anchor(*anchor),
            HirExpr::Lookaround(_) => Err(Error::new(
                ErrorKind::Jit("Lookarounds not supported in backtracking JIT".to_string()),
                "",
            )),
        }
    }

    /// Emits code to match a literal string.
    fn emit_literal(&mut self, bytes: &[u8]) -> Result<()> {
        for &byte in bytes {
            dynasm!(self.asm
                ; .arch aarch64
                // Check if we're at end of input
                ; cmp x21, x20
                ; b.hs =>self.backtrack_label

                // Load byte at current position
                ; ldrb w0, [x19, x21]

                // Compare with expected byte
                ; cmp w0, #byte as u32
                ; b.ne =>self.backtrack_label

                // Advance position
                ; add x21, x21, #1
            );
        }
        Ok(())
    }

    /// Emits code to match a character class.
    fn emit_class(&mut self, class: &HirClass) -> Result<()> {
        let match_ok = self.asm.new_dynamic_label();
        let no_match = self.asm.new_dynamic_label();

        dynasm!(self.asm
            ; .arch aarch64
            // Check end of input
            ; cmp x21, x20
            ; b.hs =>self.backtrack_label

            // Load current byte
            ; ldrb w0, [x19, x21]
        );

        // Generate range checks
        for &(start, end) in &class.ranges {
            if start == end {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cmp w0, #start as u32
                    ; b.eq =>match_ok
                );
            } else {
                let next_range = self.asm.new_dynamic_label();
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cmp w0, #start as u32
                    ; b.lo =>next_range
                    ; cmp w0, #end as u32
                    ; b.ls =>match_ok
                    ; =>next_range
                );
            }
        }

        // No range matched
        dynasm!(self.asm
            ; .arch aarch64
            ; b =>no_match
        );

        dynasm!(self.asm
            ; .arch aarch64
            ; =>match_ok
        );

        // Handle negation
        if class.negated {
            let done = self.asm.new_dynamic_label();
            dynasm!(self.asm
                ; .arch aarch64
                ; b =>self.backtrack_label
                ; =>no_match
                ; add x21, x21, #1
                ; b =>done
                ; =>done
            );
        } else {
            let done = self.asm.new_dynamic_label();
            dynasm!(self.asm
                ; .arch aarch64
                ; add x21, x21, #1
                ; b =>done
                ; =>no_match
                ; b =>self.backtrack_label
                ; =>done
            );
        }

        Ok(())
    }

    /// Emits code for alternation with backtracking.
    fn emit_alternation(&mut self, alternatives: &[HirExpr]) -> Result<()> {
        if alternatives.is_empty() {
            return Ok(());
        }

        let after_alt = self.asm.new_dynamic_label();

        for (i, alt) in alternatives.iter().enumerate() {
            let is_last = i == alternatives.len() - 1;

            if !is_last {
                let try_next = self.asm.new_dynamic_label();

                // Save state for backtracking (32-byte entry)
                dynasm!(self.asm
                    ; .arch aarch64
                    // Refuse to push past the top of the frame (see
                    // `stack_exhausted_label`). The frame ends 0x58 below x29:
                    // 0x50 of spilled callee-saved registers, then the budget.
                    ; sub x9, x29, #0x58
                    ; ldr x10, [x9]
                    ; subs x10, x10, #1
                    ; str x10, [x9]
                    ; b.ls =>self.budget_exhausted_label
                    ; add x9, x26, #32
                    ; sub x10, x29, #0x58
                    ; cmp x9, x10
                    ; b.hi =>self.stack_exhausted_label
                    ; str x21, [x26]             // Save position
                    ; adr x0, =>try_next
                    ; str x0, [x26, #8]          // Save resume address
                    ; str x23, [x26, #16]        // Save start_pos
                    ; str xzr, [x26, #24]        // Unused slot
                    ; add x26, x26, #32          // Push
                );

                self.emit_pattern(alt)?;

                // Success - pop choice point and jump past alternatives
                dynasm!(self.asm
                    ; .arch aarch64
                    ; sub x26, x26, #32
                    ; b =>after_alt
                );

                dynasm!(self.asm
                    ; .arch aarch64
                    ; =>try_next
                );
            } else {
                self.emit_pattern(alt)?;
            }
        }

        dynasm!(self.asm
            ; .arch aarch64
            ; =>after_alt
        );

        Ok(())
    }

    /// Emits an unbounded greedy repetition of a single byte class as one run.
    ///
    /// The general greedy loop pushes a 32-byte choice point *per iteration*, so
    /// `[^'"]*` over a 30-byte string costs 30 pushes and 30 stack-limit checks.
    /// A single byte class consumes exactly one byte, captures nothing, and can
    /// never fail in a way that needs an inner choice point, so the whole run is
    /// decided by a scan and recorded as ONE choice point that gives a byte back
    /// at a time.
    fn try_emit_greedy_class_run(
        &mut self,
        expr: &HirExpr,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    ) -> Result<bool> {
        if !greedy || max.is_some() {
            return Ok(false);
        }
        let set = match expr {
            HirExpr::Class(class) => crate::literal::byte_class_set(&class.ranges, class.negated),
            HirExpr::Literal(bytes) if bytes.len() == 1 => {
                crate::literal::byte_class_set(&[(bytes[0], bytes[0])], false)
            }
            _ => return Ok(false),
        };

        let table = self.asm.new_dynamic_label();
        let retry = self.asm.new_dynamic_label();
        let push = self.asm.new_dynamic_label();

        dynasm!(self.asm
            ; .arch aarch64
            ; mov x24, x21              // x24 = where the run began
            ; adr x9, =>table
            ; scan:
            ; cmp x21, x20
            ; b.hs >scanned
            ; ldrb w10, [x19, x21]
            ; ldrb w11, [x9, x10]
            ; cbz w11, >scanned
            ; add x21, x21, #1
            ; b <scan
            ; scanned:
        );
        self.byte_set_tables.push((table, set));

        if min > 0 {
            dynasm!(self.asm
                ; .arch aarch64
                ; sub x9, x21, x24
                ; cmp x9, #min
                ; b.lo =>self.backtrack_label
            );
        }

        dynasm!(self.asm
            ; .arch aarch64
            ; b =>push

            ; =>retry
            // The handler restored x21 = the length last tried, x25 = the run
            // start. Below the run start there is nothing left to try.
            ; cmp x21, x25
            ; b.ls =>self.backtrack_label
            ; sub x21, x21, #1
            ; mov x24, x25
        );

        if min > 0 {
            dynasm!(self.asm
                ; .arch aarch64
                ; sub x9, x21, x24
                ; cmp x9, #min
                ; b.lo =>self.backtrack_label
            );
        }

        if let Some(cap_idx) = self.current_capture {
            let end_offset = cap_idx * 16 + 8;
            if end_offset < 4096 {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; str x21, [x22, #end_offset]
                );
            }
        }

        dynasm!(self.asm
            ; .arch aarch64
            ; =>push
            // One choice point for the whole run.
            ; sub x9, x29, #0x58
            ; ldr x10, [x9]
            ; subs x10, x10, #1
            ; str x10, [x9]
            ; b.ls =>self.budget_exhausted_label
            ; add x9, x26, #32
            ; sub x10, x29, #0x58
            ; cmp x9, x10
            ; b.hi =>self.stack_exhausted_label
            ; str x21, [x26]
            ; adr x0, =>retry
            ; str x0, [x26, #8]
            ; str x23, [x26, #16]
            ; str x24, [x26, #24]
            ; add x26, x26, #32
        );

        Ok(true)
    }

    /// Consumes a leading run of single-byte matches before the general greedy
    /// loop starts, recording the whole run as one choice point.
    ///
    /// `[^\'"]` lowers to an ASCII class beside a UTF-8 trie, so it is not a bare
    /// class and cannot take [`Self::try_emit_greedy_class_run`] — yet on ASCII
    /// text every iteration matches the class half. This scans that half and
    /// leaves the general loop to start wherever the scan stopped, so the
    /// multi-byte half still works.
    ///
    /// The run\'s choice point is pushed *before* the general loop\'s, so
    /// backtracking gives back the general loop\'s iterations first and only then
    /// the run\'s bytes — the same order, longest first, that the per-iteration
    /// loop produced on its own.
    fn emit_run_prologue(&mut self, expr: &HirExpr) -> Result<Option<dynasmrt::DynamicLabel>> {
        let Some(set) = crate::literal::single_byte_run_set(expr) else {
            return Ok(None);
        };

        let table = self.asm.new_dynamic_label();
        let retry = self.asm.new_dynamic_label();

        dynasm!(self.asm
            ; .arch aarch64
            ; mov x24, x21              // x24 = where the run began
            ; adr x9, =>table
            ; scan:
            ; cmp x21, x20
            ; b.hs >scanned
            ; ldrb w10, [x19, x21]
            ; ldrb w11, [x9, x10]
            ; cbz w11, >scanned
            ; add x21, x21, #1
            ; b <scan
            ; scanned:
            ; sub x9, x29, #0x58
            ; ldr x10, [x9]
            ; subs x10, x10, #1
            ; str x10, [x9]
            ; b.ls =>self.budget_exhausted_label
            ; add x9, x26, #32
            ; sub x10, x29, #0x58
            ; cmp x9, x10
            ; b.hi =>self.stack_exhausted_label
            ; str x21, [x26]
            ; adr x0, =>retry
            ; str x0, [x26, #8]
            ; str x23, [x26, #16]
            ; str x24, [x26, #24]
            ; add x26, x26, #32
        );
        self.byte_set_tables.push((table, set));

        Ok(Some(retry))
    }

    /// Emits the resume path for [`Self::emit_run_prologue`]: give one byte of
    /// the run back and carry on from where the repetition ends.
    fn emit_run_retry(&mut self, retry: dynasmrt::DynamicLabel, loop_done: dynasmrt::DynamicLabel) {
        dynasm!(self.asm
            ; .arch aarch64
            ; b >past
            ; =>retry
            ; cmp x21, x25
            ; b.ls =>self.backtrack_label
            ; sub x21, x21, #1
        );

        if let Some(cap_idx) = self.current_capture {
            let end_offset = cap_idx * 16 + 8;
            if end_offset < 4096 {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; str x21, [x22, #end_offset]
                );
            }
        }

        dynasm!(self.asm
            ; .arch aarch64
            ; sub x9, x29, #0x58
            ; ldr x10, [x9]
            ; subs x10, x10, #1
            ; str x10, [x9]
            ; b.ls =>self.budget_exhausted_label
            ; add x9, x26, #32
            ; sub x10, x29, #0x58
            ; cmp x9, x10
            ; b.hi =>self.stack_exhausted_label
            ; str x21, [x26]
            ; adr x0, =>retry
            ; str x0, [x26, #8]
            ; str x23, [x26, #16]
            ; str x25, [x26, #24]
            ; add x26, x26, #32
            ; b =>loop_done
            ; past:
        );
    }

    /// Emits code for repetition.
    fn emit_repetition(
        &mut self,
        expr: &HirExpr,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    ) -> Result<()> {
        let loop_done = self.asm.new_dynamic_label();

        // Exact repetitions {n,n} optimization
        if let Some(max_val) = max {
            if min == max_val && min > 0 {
                return self.emit_exact_repetition(expr, min);
            }
        }

        if self.try_emit_greedy_class_run(expr, min, max, greedy)? {
            return Ok(());
        }

        // A run of single-byte matches consumed up front, so the general loop
        // below starts wherever the scan stopped. `None` when the body has no
        // such bytes, or when the general loop's own minimum count would be
        // wrong about a run it did not make.
        let run_retry = if greedy && min == 0 && max.is_none() {
            self.emit_run_prologue(expr)?
        } else {
            None
        };

        // x25 = iteration counter
        dynasm!(self.asm
            ; .arch aarch64
            ; mov x25, #0
        );

        if greedy {
            let loop_start = self.asm.new_dynamic_label();
            let try_backtrack = self.asm.new_dynamic_label();

            dynasm!(self.asm
                ; .arch aarch64
                ; =>loop_start
            );

            if let Some(max_val) = max {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cmp x25, #max_val
                    ; b.hs =>loop_done
                );
            }

            // Push choice point
            dynasm!(self.asm
                ; .arch aarch64
                // Refuse to push past the top of the frame; see
                // `stack_exhausted_label`.
                ; sub x9, x29, #0x58
                ; ldr x10, [x9]
                ; subs x10, x10, #1
                ; str x10, [x9]
                ; b.ls =>self.budget_exhausted_label
                ; add x9, x26, #32
                ; sub x10, x29, #0x58
                ; cmp x9, x10
                ; b.hi =>self.stack_exhausted_label
                ; str x21, [x26]
                ; adr x0, =>try_backtrack
                ; str x0, [x26, #8]
                ; str x23, [x26, #16]
                ; str x25, [x26, #24]
                ; add x26, x26, #32
            );

            let iteration_matched = self.asm.new_dynamic_label();
            let iteration_backtrack = self.asm.new_dynamic_label();
            let old_backtrack = self.backtrack_label;
            self.backtrack_label = iteration_backtrack;

            self.emit_pattern(expr)?;

            self.backtrack_label = old_backtrack;

            dynasm!(self.asm
                ; .arch aarch64
                ; b =>iteration_matched

                ; =>iteration_backtrack
                // Calculate stack bottom
                ; mov x0, 0x1000
                ; sub x0, x29, x0
                ; sub x0, x0, #0x50
                ; cmp x26, x0
                ; b.ls >empty_stack

                // Pop and check entry
                ; sub x26, x26, #32
                ; ldr x0, [x26, #8]
                ; adr x1, =>try_backtrack
                ; cmp x0, x1
                ; b.ne >not_our_entry

                // Our entry - restore and exit loop
                ; ldr x21, [x26]
                ; ldr x23, [x26, #16]
                ; ldr x25, [x26, #24]
                ; b =>loop_done

                ; not_our_entry:
                ; ldr x21, [x26]
                ; ldr x23, [x26, #16]
                ; ldr x25, [x26, #24]
                ; br x0

                ; empty_stack:
                ; b =>loop_done

                ; =>iteration_matched
                ; add x25, x25, #1
                ; b =>loop_start

                ; =>try_backtrack
            );

            // Update capture end if inside a capture
            if let Some(cap_idx) = self.current_capture {
                let end_offset = cap_idx * 16 + 8;
                if end_offset < 4096 {
                    dynasm!(self.asm
                        ; .arch aarch64
                        ; str x21, [x22, #end_offset]
                    );
                }
            }

            dynasm!(self.asm
                ; .arch aarch64
                ; cmp x25, #min
                ; b.lo =>self.backtrack_label
                ; b =>loop_done
            );
        } else {
            // Non-greedy: match minimum first
            for _ in 0..min {
                self.emit_pattern(expr)?;
                dynasm!(self.asm
                    ; .arch aarch64
                    ; add x25, x25, #1
                );
            }

            if max.is_none_or(|m| m > min) {
                let loop_start = self.asm.new_dynamic_label();
                let try_more = self.asm.new_dynamic_label();

                dynasm!(self.asm
                    ; .arch aarch64
                    ; =>loop_start
                );

                if let Some(max_val) = max {
                    dynasm!(self.asm
                        ; .arch aarch64
                        ; cmp x25, #max_val
                        ; b.hs =>loop_done
                    );
                }

                // Push choice point to try more later
                dynasm!(self.asm
                    ; .arch aarch64
                    // Refuse to push past the top of the frame; see
                    // `stack_exhausted_label`.
                    ; sub x9, x29, #0x58
                    ; ldr x10, [x9]
                    ; subs x10, x10, #1
                    ; str x10, [x9]
                    ; b.ls =>self.budget_exhausted_label
                    ; add x9, x26, #32
                    ; sub x10, x29, #0x58
                    ; cmp x9, x10
                    ; b.hi =>self.stack_exhausted_label
                    ; str x21, [x26]
                    ; adr x0, =>try_more
                    ; str x0, [x26, #8]
                    ; str x23, [x26, #16]
                    ; str x25, [x26, #24]
                    ; add x26, x26, #32
                    ; b =>loop_done

                    ; =>try_more
                );

                self.emit_pattern(expr)?;
                dynasm!(self.asm
                    ; .arch aarch64
                    ; add x25, x25, #1
                    ; b =>loop_start
                );
            }
        }

        // Reached once the general loop has given back everything it matched;
        // the run below it hands its bytes back one at a time from here.
        if let Some(retry) = run_retry {
            self.emit_run_retry(retry, loop_done);
        }

        dynasm!(self.asm
            ; .arch aarch64
            ; =>loop_done
            ; cmp x25, #min
            ; b.lo =>self.backtrack_label
        );

        Ok(())
    }

    /// Emits optimized code for exact repetitions.
    fn emit_exact_repetition(&mut self, expr: &HirExpr, count: u32) -> Result<()> {
        let loop_start = self.asm.new_dynamic_label();
        let count64 = count as u64;

        dynasm!(self.asm
            ; .arch aarch64
            ; mov x25, count64
            ; =>loop_start
        );

        self.emit_pattern(expr)?;

        dynasm!(self.asm
            ; .arch aarch64
            ; subs w25, w25, #1
            ; b.ne =>loop_start
        );

        Ok(())
    }

    /// Emits code for a capture group.
    fn emit_capture(&mut self, index: u32, expr: &HirExpr) -> Result<()> {
        let start_offset = index * 16;
        let end_offset = start_offset + 8;

        // Record start position
        if start_offset < 4096 {
            dynasm!(self.asm
                ; .arch aarch64
                ; str x21, [x22, #start_offset]
            );
        }

        let old_capture = self.current_capture;
        self.current_capture = Some(index);

        self.emit_pattern(expr)?;

        self.current_capture = old_capture;

        // Record end position
        if end_offset < 4096 {
            dynasm!(self.asm
                ; .arch aarch64
                ; str x21, [x22, #end_offset]
            );
        }

        Ok(())
    }

    /// Emits code for a backreference.
    fn emit_backref(&mut self, group: u32) -> Result<()> {
        let start_offset = group * 16;
        let end_offset = start_offset + 8;
        let backref_ok = self.asm.new_dynamic_label();

        dynasm!(self.asm
            ; .arch aarch64
            // Load captured text bounds
            ; ldr x8, [x22, #start_offset]   // x8 = capture_start
            ; ldr x9, [x22, #end_offset]     // x9 = capture_end

            // Check if capture is valid (not -1)
            ; cmn x8, #1
            ; b.eq =>self.backtrack_label

            // Calculate capture length
            ; sub x10, x9, x8                // x10 = capture_len

            // Empty capture always matches
            ; cbz x10, =>backref_ok

            // Check if enough input remains
            ; sub x11, x20, x21              // x11 = remaining
            ; cmp x10, x11
            ; b.hi =>self.backtrack_label

            // Set up pointers for comparison
            ; add x8, x8, x19               // x8 = input + capture_start
            ; add x9, x19, x21              // x9 = input + current_pos

            // Compare bytes
            ; mov x24, #0                    // x24 = comparison index
            ; cmp_loop:
            ; cmp x24, x10
            ; b.hs =>backref_ok

            ; ldrb w0, [x8, x24]
            ; ldrb w1, [x9, x24]
            ; cmp w0, w1
            ; b.ne =>self.backtrack_label

            ; add x24, x24, #1
            ; b <cmp_loop

            ; =>backref_ok
            ; add x21, x21, x10
        );

        Ok(())
    }

    /// Emits code for anchors.
    fn emit_anchor(&mut self, anchor: HirAnchor) -> Result<()> {
        match anchor {
            HirAnchor::Start => {
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cbnz x21, =>self.backtrack_label
                );
            }
            HirAnchor::End => {
                // End of text, or immediately before a trailing newline — the
                // PCRE/Python rule, so `a$` matches "a" in "a\n".
                let ok = self.asm.new_dynamic_label();
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cmp x21, x20
                    ; b.eq =>ok
                    ; add x0, x21, #1
                    ; cmp x0, x20
                    ; b.ne =>self.backtrack_label
                    ; ldrb w0, [x19, x21]
                    ; cmp w0, #0x0a
                    ; b.ne =>self.backtrack_label
                    ; =>ok
                );
            }
            HirAnchor::StartLine => {
                let ok = self.asm.new_dynamic_label();
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cbz x21, =>ok
                    ; sub x0, x21, #1
                    ; ldrb w0, [x19, x0]
                    ; cmp w0, #0x0a
                    ; b.ne =>self.backtrack_label
                    ; =>ok
                );
            }
            HirAnchor::EndLine => {
                let ok = self.asm.new_dynamic_label();
                dynasm!(self.asm
                    ; .arch aarch64
                    ; cmp x21, x20
                    ; b.eq =>ok
                    ; ldrb w0, [x19, x21]
                    ; cmp w0, #0x0a
                    ; b.ne =>self.backtrack_label
                    ; =>ok
                );
            }
            HirAnchor::WordBoundary | HirAnchor::NotWordBoundary => {
                return Err(Error::new(
                    ErrorKind::Jit(
                        "Word boundaries not yet supported in backtracking JIT".to_string(),
                    ),
                    "",
                ));
            }
        }
        Ok(())
    }

    /// Emits the backtrack handler.
    fn emit_backtrack_handler(&mut self) {
        dynasm!(self.asm
            ; .arch aarch64
            ; =>self.backtrack_label

            // Calculate stack bottom
            ; mov x0, 0x1000
            ; sub x0, x29, x0
            ; sub x0, x0, #0x50
            ; cmp x26, x0
            ; b.ls >try_next_pos

            // Pop backtrack entry
            ; sub x26, x26, #32
            ; ldr x21, [x26]           // Restore position
            ; ldr x0, [x26, #8]        // Get resume address
            ; ldr x23, [x26, #16]      // Restore start_pos
            ; ldr x25, [x26, #24]      // Restore extra data

            // Jump to resume address
            ; br x0

            ; try_next_pos:
            ; add x23, x23, #1
            ; cmp x23, x20
            ; b.hi =>self.no_match_label
            ; b =>self.next_start_label
        );
    }

    /// Emits the success handler.
    fn emit_success_handler(&mut self) {
        dynasm!(self.asm
            ; .arch aarch64
            ; =>self.match_success_label
            // Set group 0 end = current position
            ; str x21, [x22, #8]

            // Return the end position (positive = success)
            ; mov x0, x21
            ; b >epilogue

            ; =>self.no_match_label
            ; movn x0, 0
            ; b >epilogue

            // Choice-point stack full: the answer is unknown, not "no match".
            ; =>self.stack_exhausted_label
            ; movn x0, 1                    // -2
            ; b >epilogue

            // Step budget spent: the caller's limit, reported as such.
            ; =>self.budget_exhausted_label
            ; movn x0, 2                    // -3

            ; epilogue:
            // Deallocate backtrack stack
            ; mov x9, 0x1000
            ; add sp, sp, x9

            // Restore callee-saved registers
            ; ldp x27, x28, [sp], #16
            ; ldp x25, x26, [sp], #16
            ; ldp x23, x24, [sp], #16
            ; ldp x21, x22, [sp], #16
            ; ldp x19, x20, [sp], #16
            ; ldp x29, x30, [sp], #16
            ; ret
        );
    }
}
