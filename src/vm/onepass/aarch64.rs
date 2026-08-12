//! AArch64 code generation for the one-pass capture engine.
//!
//! The mirror of [`super::x86_64`]; see that module for what the generated shape
//! is and why. Only the encoding differs.
//!
//! # Register allocation (AAPCS64)
//!
//! | Register | Purpose |
//! |----------|---------|
//! | x19 | Current position |
//! | x20 | Input base pointer |
//! | x21 | Input length |
//! | x22 | Live capture slots (stack) |
//! | x23 | Slots snapshotted at the last match (stack) |
//! | x24 | Caller's output pointer |
//! | x25 | End of the last match, or -1 |
//! | x26 | Deferred snapshot's stub address, or 0 |
//! | x27 | Position the deferred snapshot was recorded at |
//! | x28 | The live priority limit |
//! | x0-x9 | Scratch |
//!
//! The locals live in callee-saved registers rather than a frame, because
//! AArch64 has enough of them and it keeps every access a single instruction.

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};

use super::{Action, Guard, GuardSpan, OnePass, NO_TRANSITION};

/// Compiles `one_pass` to native code, or returns `None` when it is not a shape
/// this emitter handles.
pub(super) fn compile(one_pass: &OnePass) -> Option<super::jit::Compiled> {
    let mut asm = dynasmrt::aarch64::Assembler::new().ok()?;

    let slot_len = one_pass.slot_count.checked_mul(2)?;
    // One slot array each for the live slots and the snapshot, then the start
    // position, 16-byte aligned.
    let live_bytes = u32::try_from(slot_len * 8).ok()?;
    let start_slot = live_bytes * 2;
    let frame = (start_slot + 16).div_ceil(16) * 16;

    let closures: Vec<_> = (0..one_pass.closures.len())
        .map(|_| asm.new_dynamic_label())
        .collect();
    let done = asm.new_dynamic_label();
    let entry = asm.offset();

    dynasm!(asm
        ; .arch aarch64
        ; stp x29, x30, [sp, #-16]!
        ; stp x19, x20, [sp, #-16]!
        ; stp x21, x22, [sp, #-16]!
        ; stp x23, x24, [sp, #-16]!
        ; stp x25, x26, [sp, #-16]!
        ; stp x27, x28, [sp, #-16]!
        ; mov x29, sp
        ; sub sp, sp, frame
        ; mov x20, x0               // input
        ; mov x21, x1               // length
        ; mov x19, x2               // position
        ; mov x24, x3               // caller's slot buffer
        ; mov x22, sp
        ; add x23, x22, live_bytes
        ; movn x25, 0               // match end = -1
        ; mov x26, xzr              // no deferred snapshot
        ; movn x0, 0
    );
    for slot in 0..slot_len {
        let offset = u32::try_from(slot * 8).ok()?;
        dynasm!(asm ; .arch aarch64 ; str x0, [x22, offset]);
    }
    // The start position is needed once, at the end, for slot 0.
    dynasm!(asm ; .arch aarch64 ; str x19, [x22, start_slot]);
    dynasm!(asm ; .arch aarch64 ; b =>*closures.first()?);

    let mut tables: Vec<(dynasmrt::DynamicLabel, [u8; 256])> = Vec::new();
    let mut match_stubs: Vec<(dynasmrt::DynamicLabel, super::ActionSpan)> = Vec::new();
    let mut word_table: Option<dynasmrt::DynamicLabel> = None;

    for (index, closure) in one_pass.closures.iter().enumerate() {
        dynasm!(asm ; .arch aarch64 ; =>*closures.get(index)?);

        dynasm!(asm ; .arch aarch64 ; movn x28, 0);
        if !closure.matches.is_empty() {
            let recorded = asm.new_dynamic_label();
            for item in &closure.matches {
                let next = asm.new_dynamic_label();
                emit_guards(&mut asm, one_pass, item.guards, next, &mut word_table)?;

                let stub = asm.new_dynamic_label();
                match_stubs.push((stub, item.actions));
                let order = u64::from(item.order);
                dynasm!(asm
                    ; .arch aarch64
                    ; mov x25, x19
                    ; mov x27, x19
                    ; adr x26, =>stub
                    ; mov x28, order
                    ; b =>recorded
                    ; =>next
                );
            }
            dynasm!(asm ; .arch aarch64 ; =>recorded);
        }

        let table = asm.new_dynamic_label();
        tables.push((table, closure.table));
        dynasm!(asm
            ; .arch aarch64
            ; cmp x19, x21
            ; b.hs =>done
            ; adr x1, =>table
            ; ldrb w0, [x20, x19]
            ; ldrb w0, [x1, x0]
        );

        let stubs: Vec<_> = closure
            .transitions
            .iter()
            .map(|_| asm.new_dynamic_label())
            .collect();
        for (slot, stub) in stubs.iter().enumerate() {
            let slot = u32::try_from(slot).ok()?;
            dynasm!(asm ; .arch aarch64 ; cmp w0, #slot ; b.eq =>*stub);
        }
        dynasm!(asm ; .arch aarch64 ; b =>done);

        for (transition, stub) in closure.transitions.iter().zip(&stubs) {
            dynasm!(asm ; .arch aarch64 ; =>*stub);
            // See the x86-64 emitter: the limit only bites for a guarded match
            // ahead of a non-greedy loop-back, and mirrors the interpreter.
            let order = u64::from(transition.order);
            dynasm!(asm
                ; .arch aarch64
                ; mov x1, order
                ; cmp x28, x1
                ; b.lo =>done
            );
            emit_guards(&mut asm, one_pass, transition.guards, done, &mut word_table)?;
            if transition.actions.len != 0 {
                emit_flush(&mut asm, slot_len)?;
                emit_actions(&mut asm, one_pass, transition.actions, false)?;
            }
            let target = closures.get(transition.target as usize)?;
            dynasm!(asm ; .arch aarch64 ; add x19, x19, #1 ; b =>*target);
        }
    }

    dynasm!(asm ; .arch aarch64 ; =>done);
    emit_flush(&mut asm, slot_len)?;
    dynasm!(asm
        ; .arch aarch64
        ; mov x0, x25
        ; movn x1, 0
        ; cmp x0, x1
        ; b.eq >no_match
    );
    for slot in 0..slot_len {
        let offset = u32::try_from(slot * 8).ok()?;
        dynasm!(asm ; .arch aarch64 ; ldr x1, [x23, offset] ; str x1, [x24, offset]);
    }
    dynasm!(asm
        ; .arch aarch64
        ; ldr x1, [x22, start_slot]
        ; str x1, [x24]
        ; str x25, [x24, #8]
        ; b >epilogue
        ; no_match:
        ; movn x0, 0
        ; epilogue:
        ; mov sp, x29
        ; ldp x27, x28, [sp], #16
        ; ldp x25, x26, [sp], #16
        ; ldp x23, x24, [sp], #16
        ; ldp x21, x22, [sp], #16
        ; ldp x19, x20, [sp], #16
        ; ldp x29, x30, [sp], #16
        ; ret
    );

    for (stub, actions) in &match_stubs {
        dynasm!(asm ; .arch aarch64 ; =>*stub);
        emit_actions(&mut asm, one_pass, *actions, true)?;
        dynasm!(asm ; .arch aarch64 ; ret x30);
    }

    for (label, table) in &tables {
        dynasm!(asm ; .arch aarch64 ; =>*label ; .bytes table);
    }
    if let Some(label) = word_table {
        let mut members = [0u8; 256];
        for (byte, entry) in members.iter_mut().enumerate() {
            *entry = u8::from(crate::hir::unicode::is_word_byte(byte as u8));
        }
        dynasm!(asm ; .arch aarch64 ; =>label ; .bytes members);
    }

    // `finalize` panics on a failed final commit; commit first so an
    // out-of-range branch declines the JIT instead.
    asm.commit().ok()?;
    let code = asm.finalize().ok()?;
    let run = unsafe { std::mem::transmute::<*const u8, super::jit::MatchFn>(code.ptr(entry)) };
    Some(super::jit::Compiled { code, run })
}

/// Emits every assertion on a path, branching to `fail` if one does not hold.
///
/// Mirrors `Guard::holds` instruction for instruction with the x86-64 emitter;
/// a divergence between the two would be a divergence between targets.
fn emit_guards(
    asm: &mut dynasmrt::aarch64::Assembler,
    one_pass: &OnePass,
    span: GuardSpan,
    fail: dynasmrt::DynamicLabel,
    word_table: &mut Option<dynasmrt::DynamicLabel>,
) -> Option<()> {
    if span.is_empty() {
        return Some(());
    }
    let range = span.start as usize..span.start as usize + span.len as usize;
    for guard in one_pass.guards.get(range)? {
        match *guard {
            Guard::StartOfText => dynasm!(asm
                ; .arch aarch64
                ; cbnz x19, =>fail
            ),
            Guard::EndOfText => dynasm!(asm
                ; .arch aarch64
                ; cmp x19, x21
                ; b.eq >held
                ; add x0, x19, #1
                ; cmp x0, x21
                ; b.ne =>fail
                ; ldrb w0, [x20, x19]
                ; cmp w0, #0x0a
                ; b.ne =>fail
                ; held:
            ),
            Guard::StartOfLine => dynasm!(asm
                ; .arch aarch64
                ; cbz x19, >held
                ; sub x0, x19, #1
                ; ldrb w0, [x20, x0]
                ; cmp w0, #0x0a
                ; b.ne =>fail
                ; held:
            ),
            Guard::EndOfLine => dynasm!(asm
                ; .arch aarch64
                ; cmp x19, x21
                ; b.eq >held
                ; ldrb w0, [x20, x19]
                ; cmp w0, #0x0a
                ; b.ne =>fail
                ; held:
            ),
            Guard::WordBoundary | Guard::NotWordBoundary => {
                let table = *word_table.get_or_insert_with(|| asm.new_dynamic_label());
                let boundary = matches!(*guard, Guard::WordBoundary);
                dynasm!(asm
                    ; .arch aarch64
                    ; adr x2, =>table
                    ; mov x0, xzr
                    ; cbz x19, >no_before
                    ; sub x1, x19, #1
                    ; ldrb w1, [x20, x1]
                    ; ldrb w0, [x2, x1]
                    ; no_before:
                    ; mov x1, xzr
                    ; cmp x19, x21
                    ; b.hs >no_after
                    ; ldrb w3, [x20, x19]
                    ; ldrb w1, [x2, x3]
                    ; no_after:
                    ; cmp w0, w1
                );
                if boundary {
                    dynasm!(asm ; .arch aarch64 ; b.eq =>fail);
                } else {
                    dynasm!(asm ; .arch aarch64 ; b.ne =>fail);
                }
            }
        }
    }
    Some(())
}

/// Takes the deferred snapshot, if one is outstanding.
fn emit_flush(asm: &mut dynasmrt::aarch64::Assembler, slot_len: usize) -> Option<()> {
    dynasm!(asm ; .arch aarch64 ; cbz x26, >skip);
    for slot in 0..slot_len {
        let offset = u32::try_from(slot * 8).ok()?;
        dynasm!(asm ; .arch aarch64 ; ldr x0, [x22, offset] ; str x0, [x23, offset]);
    }
    dynasm!(asm
        ; .arch aarch64
        ; mov x9, x27
        ; blr x26
        ; mov x26, xzr
        ; skip:
    );
    Some(())
}

/// Emits one action span.
///
/// A match's actions land on the snapshot at the recorded position (in x9), a
/// transition's on the live slots at the current one (in x19).
fn emit_actions(
    asm: &mut dynasmrt::aarch64::Assembler,
    one_pass: &OnePass,
    span: super::ActionSpan,
    snapshot: bool,
) -> Option<()> {
    let range = span.start as usize..span.start as usize + span.len as usize;
    for action in one_pass.actions.get(range)? {
        let (group, start) = match *action {
            Action::Start(group) => (group, true),
            Action::End(group) => (group, false),
        };
        let slot = group.checked_mul(16)?;
        let end_slot = slot.checked_add(8)?;
        if snapshot {
            if start {
                dynasm!(asm ; .arch aarch64 ; str x9, [x23, slot] ; str x9, [x23, end_slot]);
            } else {
                dynasm!(asm
                    ; .arch aarch64
                    ; ldr x0, [x23, slot]
                    ; tbnz x0, #63, >unset
                    ; str x9, [x23, end_slot]
                    ; unset:
                );
            }
        } else if start {
            dynasm!(asm ; .arch aarch64 ; str x19, [x22, slot] ; str x19, [x22, end_slot]);
        } else {
            dynasm!(asm
                ; .arch aarch64
                ; ldr x0, [x22, slot]
                ; tbnz x0, #63, >unset
                ; str x19, [x22, end_slot]
                ; unset:
            );
        }
    }
    Some(())
}

/// Whether this emitter is willing to compile `one_pass`.
pub(super) fn is_supported(one_pass: &OnePass) -> bool {
    one_pass.closures.len() <= MAX_CLOSURES
        && one_pass.slot_count <= MAX_SLOTS
        && one_pass
            .closures
            .iter()
            .all(|closure| closure.transitions.len() < NO_TRANSITION as usize)
}

/// Each closure emits a 256-byte table and a block, and `adr` reaches +-1 MB.
const MAX_CLOSURES: usize = 96;

/// Two slot arrays live in the stack frame.
const MAX_SLOTS: usize = 32;
