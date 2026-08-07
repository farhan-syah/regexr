//! x86-64 code generation for the one-pass capture engine.
//!
//! [`super::OnePass`] is a DFA whose transitions carry capture writes, and its
//! interpreter pays for that generality at every byte: it loads the closure, its
//! match list and its transition list, then re-decides at run time what is
//! constant about them. Generated code folds all of it away — a closure becomes
//! a block, its match becomes two stores, and a transition becomes the stores it
//! actually performs followed by a jump to the next block.
//!
//! # Register allocation
//!
//! | Register | Purpose |
//! |----------|---------|
//! | rbx | Current position |
//! | r12 | Input base pointer |
//! | r13 | Input length |
//! | r14 | Live capture slots (stack) |
//! | r15 | Slots snapshotted at the last match (stack) |
//! | rax, rdx, rdi | Scratch |
//!
//! Locals sit just under `rbp`: the end of the last match, the deferred
//! snapshot's stub and position, the caller's output pointer, and the start
//! position.

use dynasmrt::{dynasm, DynasmApi, DynasmLabelApi};

use super::{Action, OnePass, NO_TRANSITION};

/// Offsets of the locals, in bytes below `rbp`.
const MATCH_END: i32 = 8;
const PENDING_STUB: i32 = 16;
const PENDING_POS: i32 = 24;
const OUT_PTR: i32 = 32;
const START_POS: i32 = 40;
const LOCALS: i32 = 48;

/// Compiles `one_pass` to native code, or returns `None` when it is not a shape
/// this emitter handles.
pub(super) fn compile(one_pass: &OnePass) -> Option<super::jit::Compiled> {
    let mut asm = dynasmrt::x64::Assembler::new().ok()?;

    let slot_len = one_pass.slot_count.checked_mul(2)?;
    let live_bytes = i32::try_from(slot_len * 8).ok()?;
    // Two slot arrays plus the locals, rounded up to keep the frame 16-byte
    // aligned for the `call` the flush path uses.
    let live_base = LOCALS + live_bytes;
    let match_base = LOCALS + live_bytes * 2;
    let frame = (match_base + 15) & !15;

    let closures: Vec<_> = (0..one_pass.closures.len())
        .map(|_| asm.new_dynamic_label())
        .collect();
    let done = asm.new_dynamic_label();
    let entry = asm.offset();

    // The callee-saved registers are pushed *before* `rbp` is established, so
    // the locals below it cannot land on top of them.
    dynasm!(asm
        ; .arch x64
        ; push rbx
        ; push r12
        ; push r13
        ; push r14
        ; push r15
        ; push rbp
        ; mov rbp, rsp
        ; sub rsp, frame
        ; mov r12, rdi              // input
        ; mov r13, rsi              // length
        ; mov rbx, rdx              // position
        ; mov [rbp - OUT_PTR], rcx  // caller's slot buffer
        ; mov [rbp - START_POS], rdx
        ; lea r14, [rbp - live_base]
        ; lea r15, [rbp - match_base]
        ; mov QWORD [rbp - MATCH_END], -1
        ; mov QWORD [rbp - PENDING_STUB], 0
        ; mov rax, -1
    );
    for slot in 0..slot_len {
        let offset = i32::try_from(slot * 8).ok()?;
        dynasm!(asm ; .arch x64 ; mov [r14 + offset], rax);
    }
    dynasm!(asm ; .arch x64 ; jmp =>*closures.first()?);

    // Emitted after every block, so the tables and stubs are data and code the
    // fall-through never reaches.
    let mut tables: Vec<(dynasmrt::DynamicLabel, [u8; 256])> = Vec::new();
    let mut match_stubs: Vec<(dynasmrt::DynamicLabel, super::ActionSpan)> = Vec::new();

    for (index, closure) in one_pass.closures.iter().enumerate() {
        dynasm!(asm ; .arch x64 ; =>*closures.get(index)?);

        // Every match here is unconditional (the driver refuses guards), so the
        // first one always fires: record it and defer its snapshot.
        if let Some(item) = closure.matches.first() {
            let stub = asm.new_dynamic_label();
            match_stubs.push((stub, item.actions));
            dynasm!(asm
                ; .arch x64
                ; mov [rbp - MATCH_END], rbx
                ; mov [rbp - PENDING_POS], rbx
                ; lea rax, [=>stub]
                ; mov [rbp - PENDING_STUB], rax
            );
        }

        let table = asm.new_dynamic_label();
        tables.push((table, closure.table));
        dynasm!(asm
            ; .arch x64
            ; cmp rbx, r13
            ; jae =>done
            ; lea rdx, [=>table]
            ; movzx eax, BYTE [r12 + rbx]
            ; movzx eax, BYTE [rdx + rax]
        );

        // One stub per transition, reached by its index in the byte table.
        let stubs: Vec<_> = closure
            .transitions
            .iter()
            .map(|_| asm.new_dynamic_label())
            .collect();
        for (slot, stub) in stubs.iter().enumerate() {
            let slot = i32::try_from(slot).ok()?;
            dynasm!(asm ; .arch x64 ; cmp eax, slot ; je =>*stub);
        }
        dynasm!(asm ; .arch x64 ; jmp =>done);

        for (transition, stub) in closure.transitions.iter().zip(&stubs) {
            dynasm!(asm ; .arch x64 ; =>*stub);
            if transition.actions.len != 0 {
                emit_flush(&mut asm, slot_len)?;
                emit_actions(&mut asm, one_pass, transition.actions, false)?;
            }
            let target = closures.get(transition.target as usize)?;
            dynasm!(asm ; .arch x64 ; inc rbx ; jmp =>*target);
        }
    }

    // The scan stopped. Take any deferred snapshot, then report.
    dynasm!(asm ; .arch x64 ; =>done);
    emit_flush(&mut asm, slot_len)?;
    dynasm!(asm
        ; .arch x64
        ; mov rax, [rbp - MATCH_END]
        ; cmp rax, -1
        ; je >no_match
        ; mov rdi, [rbp - OUT_PTR]
    );
    for slot in 0..slot_len {
        let offset = i32::try_from(slot * 8).ok()?;
        dynasm!(asm ; .arch x64 ; mov rdx, [r15 + offset] ; mov [rdi + offset], rdx);
    }
    // Slot 0 is the whole match, which the scan never writes.
    dynasm!(asm
        ; .arch x64
        ; mov rdx, [rbp - START_POS]
        ; mov [rdi], rdx
        ; mov rdx, [rbp - MATCH_END]
        ; mov [rdi + 8], rdx
        ; jmp >epilogue
        ; no_match:
        ; mov rax, -1
        ; epilogue:
        ; add rsp, frame
        ; pop rbp
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rbx
        ; ret
    );

    // The match stubs: apply one match's actions to the snapshot at the position
    // the match was recorded at, which the flush path passes in rdi.
    for (stub, actions) in &match_stubs {
        dynasm!(asm ; .arch x64 ; =>*stub);
        emit_actions(&mut asm, one_pass, *actions, true)?;
        dynasm!(asm ; .arch x64 ; ret);
    }

    for (label, table) in &tables {
        dynasm!(asm ; .arch x64 ; =>*label ; .bytes table);
    }

    let code = asm.finalize().ok()?;
    let run = unsafe { std::mem::transmute::<*const u8, super::jit::MatchFn>(code.ptr(entry)) };
    Some(super::jit::Compiled { code, run })
}

/// Takes the deferred snapshot, if one is outstanding.
///
/// A match records only *that* it happened; copying the slots is put off until
/// they are about to change, because a greedy tail re-reaches its match at every
/// byte while writing nothing.
fn emit_flush(asm: &mut dynasmrt::x64::Assembler, slot_len: usize) -> Option<()> {
    dynasm!(asm
        ; .arch x64
        ; mov rax, [rbp - PENDING_STUB]
        ; test rax, rax
        ; je >skip
    );
    for slot in 0..slot_len {
        let offset = i32::try_from(slot * 8).ok()?;
        dynasm!(asm ; .arch x64 ; mov rdx, [r14 + offset] ; mov [r15 + offset], rdx);
    }
    dynasm!(asm
        ; .arch x64
        ; mov rdi, [rbp - PENDING_POS]
        ; call rax
        ; mov QWORD [rbp - PENDING_STUB], 0
        ; skip:
    );
    Some(())
}

/// Emits one action span.
///
/// `snapshot` selects which array is written and where the position comes from:
/// a match's actions land on the snapshot at the recorded position (in rdi), a
/// transition's on the live slots at the current one (in rbx).
fn emit_actions(
    asm: &mut dynasmrt::x64::Assembler,
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
        let slot = i32::try_from(group).ok()?.checked_mul(16)?;
        if snapshot {
            if start {
                dynasm!(asm ; .arch x64 ; mov [r15 + slot], rdi ; mov [r15 + slot + 8], rdi);
            } else {
                // An end extends a group that was started, and does nothing to
                // one that was not.
                dynasm!(asm
                    ; .arch x64
                    ; cmp QWORD [r15 + slot], 0
                    ; jl >unset
                    ; mov [r15 + slot + 8], rdi
                    ; unset:
                );
            }
        } else if start {
            dynasm!(asm ; .arch x64 ; mov [r14 + slot], rbx ; mov [r14 + slot + 8], rbx);
        } else {
            dynasm!(asm
                ; .arch x64
                ; cmp QWORD [r14 + slot], 0
                ; jl >unset
                ; mov [r14 + slot + 8], rbx
                ; unset:
            );
        }
    }
    Some(())
}

/// Whether this emitter is willing to compile `one_pass`.
///
/// Guards are the exclusion that matters: an assertion has to be evaluated at
/// the position a transition fires, which would make the live priority limit a
/// run-time value and every dead-transition decision dynamic. Without them each
/// closure's limit is a constant, so the tables below are decided here rather
/// than re-derived per byte.
pub(super) fn is_supported(one_pass: &OnePass) -> bool {
    one_pass.guards.is_empty()
        && one_pass.closures.len() <= MAX_CLOSURES
        && one_pass.slot_count <= MAX_SLOTS
        && one_pass
            .closures
            .iter()
            .all(|closure| closure.transitions.len() < NO_TRANSITION as usize)
}

/// Each closure emits a 256-byte table and a block, so this bounds the code and
/// the data the tables occupy.
const MAX_CLOSURES: usize = 96;

/// Two slot arrays live in the stack frame.
const MAX_SLOTS: usize = 32;
