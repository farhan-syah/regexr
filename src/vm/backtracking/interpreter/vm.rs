//! Backtracking regex engine for fast capture extraction.
//!
//! This is a PCRE-style backtracking interpreter that uses bytecode compilation
//! for efficient execution. Unlike tree-walking interpreters, this compiles HIR
//! to a flat bytecode representation first, then executes with minimal overhead.

use crate::hir::{CodepointClass, Hir, HirAnchor, HirExpr};
use crate::literal::ByteSet;
use crate::nfa::{ByteClass, ByteRange};

use super::super::shared::{
    decode_utf8, is_word_byte, BudgetExhausted, CaptureSlots, Op, DEFAULT_BACKTRACK_LIMIT,
};

/// A compiled backtracking regex.
pub struct BacktrackingVm {
    /// Bytecode program.
    code: Vec<Op>,
    /// Number of capture groups (not slots).
    capture_count: u32,
    /// Large byte classes (for classes with >4 ranges).
    /// Uses ByteClass for fast O(1) bitmap lookup.
    byte_classes: Vec<ByteClass>,
    /// Large codepoint classes (for Unicode classes with multiple ranges).
    /// Uses CodepointClass for fast ASCII bitmap lookup.
    cp_classes: Vec<CodepointClass>,
    /// How many progress registers [`Op::MarkPos`] can address.
    progress_regs: usize,
    /// Bytes a match can begin with, when the pattern pins them down.
    ///
    /// Every start position costs a full attempt — the slot reset plus entering
    /// the bytecode — so a search over text that mostly does not match spends
    /// nearly all of it on positions the first element rejects outright. This
    /// skips them with one indexed load.
    first_bytes: Option<Box<ByteSet>>,
}

impl BacktrackingVm {
    /// Creates a new backtracking VM from HIR.
    pub fn new(hir: &Hir) -> Self {
        let mut compiler = Compiler::new();
        compiler.compile(&hir.expr);
        compiler.emit(Op::Match);

        Self {
            code: compiler.code,
            capture_count: hir.props.capture_count,
            byte_classes: compiler.byte_classes,
            cp_classes: compiler.cp_classes,
            progress_regs: compiler.progress_regs as usize,
            first_bytes: crate::literal::first_byte_set(hir).map(Box::new),
        }
    }

    /// Returns the number of capture groups.
    pub fn capture_count(&self) -> u32 {
        self.capture_count
    }

    /// Finds the first match in the input.
    pub fn find(&self, input: &[u8]) -> Option<(usize, usize)> {
        self.find_at(input, 0)
    }

    /// Finds a match starting at the given position.
    pub fn find_at(&self, input: &[u8], start: usize) -> Option<(usize, usize)> {
        self.try_find_at(input, start, DEFAULT_BACKTRACK_LIMIT)
            .unwrap_or(None)
    }

    /// [`Self::find_at`] under an explicit step budget.
    ///
    /// [`BudgetExhausted`] means the budget ran out before the search
    /// finished; the caller decides what that means.
    pub fn try_find_at(
        &self,
        input: &[u8],
        start: usize,
        limit: u64,
    ) -> Result<Option<(usize, usize)>, BudgetExhausted> {
        let num_slots = (self.capture_count as usize + 1) * 2;
        let mut slots = vec![-1i32; num_slots];
        let mut budget = limit;
        let mut scratch = Scratch::new(self.progress_regs);

        let mut pos = start;
        while pos <= input.len() {
            pos = self.skip_to_viable_start(input, pos);
            if pos > input.len() {
                break;
            }
            slots.fill(-1);
            if self.exec(input, pos, &mut slots, &mut budget, &mut scratch)? {
                let s = slots[0];
                let e = slots[1];
                if s >= 0 && e >= 0 {
                    return Ok(Some((s as usize, e as usize)));
                }
            }
            pos += 1;
        }
        Ok(None)
    }

    /// Returns captures for the first match.
    pub fn captures(&self, input: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
        self.captures_from(input, 0)
    }

    /// Returns captures for the first match starting at or after `from`.
    ///
    /// The whole input is handed to every attempt, so `^`, `\b` and lookbehind
    /// still see the bytes before `from`.
    pub fn captures_from(&self, input: &[u8], from: usize) -> Option<Vec<Option<(usize, usize)>>> {
        self.try_captures_from(input, from, DEFAULT_BACKTRACK_LIMIT)
            .unwrap_or(None)
    }

    /// [`Self::captures_from`] under an explicit step budget.
    pub fn try_captures_from(
        &self,
        input: &[u8],
        from: usize,
        limit: u64,
    ) -> Result<Option<CaptureSlots>, BudgetExhausted> {
        let num_slots = (self.capture_count as usize + 1) * 2;
        let mut slots = vec![-1i32; num_slots];
        let mut budget = limit;
        let mut scratch = Scratch::new(self.progress_regs);

        let mut start = from;
        while start <= input.len() {
            start = self.skip_to_viable_start(input, start);
            if start > input.len() {
                break;
            }
            slots.fill(-1);
            if self.exec(input, start, &mut slots, &mut budget, &mut scratch)? {
                return Ok(Some(self.extract_captures(&slots)));
            }
            start += 1;
        }
        Ok(None)
    }

    /// Advances past every position whose byte no match can begin with.
    ///
    /// The end of the input is always viable: it reads no byte, and whether an
    /// empty match belongs there is the attempt's decision.
    #[inline]
    fn skip_to_viable_start(&self, input: &[u8], from: usize) -> usize {
        let Some(ref first_bytes) = self.first_bytes else {
            return from;
        };
        let mut pos = from;
        while let Some(&byte) = input.get(pos) {
            if first_bytes
                .get(byte as usize)
                .is_none_or(|member| *member != 0)
            {
                break;
            }
            pos += 1;
        }
        pos
    }

    /// Extract captures from slots.
    fn extract_captures(&self, slots: &[i32]) -> Vec<Option<(usize, usize)>> {
        let mut result = Vec::with_capacity(self.capture_count as usize + 1);
        for i in 0..=self.capture_count as usize {
            let s = slots[i * 2];
            let e = slots[i * 2 + 1];
            if s >= 0 && e >= 0 {
                result.push(Some((s as usize, e as usize)));
            } else {
                result.push(None);
            }
        }
        result
    }

    /// Executes the bytecode from `start`, reporting whether it matched.
    ///
    /// Returns [`BudgetExhausted`] when the search exceeds `budget` steps. Only patterns
    /// with backreferences reach this engine, and backreference matching has no
    /// polynomial bound, so the budget is the guarantee that a search ends.
    #[inline(never)]
    fn exec(
        &self,
        input: &[u8],
        start: usize,
        slots: &mut [i32],
        budget: &mut u64,
        scratch: &mut Scratch,
    ) -> Result<bool, BudgetExhausted> {
        let Scratch { trail, progress } = scratch;
        trail.clear();
        progress.fill(usize::MAX);

        let mut pc = 0u32;
        let mut pos = start;
        // Where the innermost `RunScan` run began, restored by `Trail::pop`.
        let mut run_start = 0usize;

        // Set group 0 start
        slots[0] = start as i32;

        loop {
            if pc as usize >= self.code.len() {
                return Ok(false);
            }
            match self.code[pc as usize] {
                Op::Byte(b) => {
                    if pos < input.len() && input[pos] == b {
                        pos += 1;
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::ByteRange(lo, hi) => {
                    if pos < input.len() && input[pos] >= lo && input[pos] <= hi {
                        pos += 1;
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::ByteRanges { count, ranges } => {
                    if pos < input.len() {
                        let b = input[pos];
                        let mut matched = false;
                        for &(lo, hi) in ranges.iter().take(count as usize) {
                            if b >= lo && b <= hi {
                                matched = true;
                                break;
                            }
                        }
                        if matched {
                            pos += 1;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::NotByteRanges { count, ranges } => {
                    if pos < input.len() {
                        let b = input[pos];
                        let mut in_range = false;
                        for &(lo, hi) in ranges.iter().take(count as usize) {
                            if b >= lo && b <= hi {
                                in_range = true;
                                break;
                            }
                        }
                        if !in_range {
                            pos += 1;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::CpRange(lo, hi) => {
                    if let Some((cp, len)) = decode_utf8(&input[pos..]) {
                        if cp >= lo && cp <= hi {
                            pos += len;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::NotCpRange(lo, hi) => {
                    if let Some((cp, len)) = decode_utf8(&input[pos..]) {
                        if cp < lo || cp > hi {
                            pos += len;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::ByteClassRef { index, negated } => {
                    if pos < input.len() {
                        let b = input[pos];
                        let byte_class = &self.byte_classes[index as usize];
                        // Use ByteClass::contains() for O(1) bitmap lookup
                        let in_class = byte_class.contains(b);
                        let matched = if negated { !in_class } else { in_class };
                        if matched {
                            pos += 1;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::CpClassRef { index, negated } => {
                    if let Some((cp, len)) = decode_utf8(&input[pos..]) {
                        let cpclass = &self.cp_classes[index as usize];
                        // Use CodepointClass::contains() which has ASCII bitmap fast path
                        let in_class = cpclass.contains_raw(cp);
                        let matched = if negated { !in_class } else { in_class };
                        if matched {
                            pos += len;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::Any => {
                    if pos < input.len() {
                        pos += 1;
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::RunScan { class, retry } => {
                    let Some(set) = self.byte_classes.get(class as usize) else {
                        return Ok(false);
                    };
                    let begin = pos;
                    while input.get(pos).is_some_and(|&byte| set.contains(byte)) {
                        pos += 1;
                    }
                    // One choice point for the run, charged as one step: the
                    // scan itself is a forward walk whose cost the input already
                    // bounds.
                    if *budget == 0 {
                        return Err(BudgetExhausted);
                    }
                    *budget -= 1;
                    trail.push_run(retry, pos, begin, self.progress_regs);
                    pc += 1;
                }

                Op::RunRetry => {
                    // Resumed with `pos` at the length last tried and
                    // `run_start` where the run began.
                    if pos <= run_start {
                        if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                            return Ok(false);
                        }
                        continue;
                    }
                    pos -= 1;
                    if *budget == 0 {
                        return Err(BudgetExhausted);
                    }
                    *budget -= 1;
                    trail.push_run(pc, pos, run_start, self.progress_regs);
                    pc += 1;
                }

                Op::Split(target) => {
                    // The budget is spent here rather than on every
                    // instruction. A choice point is what multiplies the search
                    // — everything between two of them is a forward scan, whose
                    // cost is already bounded by the input and the program — so
                    // bounding choice points bounds the whole search, and it is
                    // the same quantity the generated code charges for.
                    if *budget == 0 {
                        return Err(BudgetExhausted);
                    }
                    *budget -= 1;
                    trail.push(target, pos, self.progress_regs);
                    pc += 1;
                }

                Op::Jump(target) => {
                    pc = target;
                }

                Op::Save(slot) => {
                    let slot = slot as usize;
                    if slot < slots.len() {
                        trail.record_slot(slot as u16, slots[slot]);
                        slots[slot] = pos as i32;
                    }
                    pc += 1;
                }

                Op::MarkPos(reg) => {
                    let reg = reg as usize;
                    trail.record_reg(reg as u16, progress[reg]);
                    progress[reg] = pos;
                    pc += 1;
                }

                Op::AssertProgress(reg) => {
                    // The body consumed nothing this turn, so this iteration is
                    // rejected and the loop falls back to its exit branch —
                    // which is where the choice point pushed at the loop head
                    // resumes, with the slots this iteration wrote put back.
                    if pos == progress[reg as usize] {
                        if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                            return Ok(false);
                        }
                    } else {
                        pc += 1;
                    }
                }

                Op::Match => {
                    slots[1] = pos as i32;
                    return Ok(true);
                }

                Op::StartAnchor => {
                    if pos == 0 {
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::StartLineAnchor => {
                    if pos == 0 || input[pos - 1] == b'\n' {
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::EndAnchor => {
                    // End of text, or just before a trailing newline.
                    if pos == input.len() || (pos + 1 == input.len() && input[pos] == b'\n') {
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::EndLineAnchor => {
                    if pos == input.len() || input[pos] == b'\n' {
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::WordBoundary => {
                    let before = pos > 0 && is_word_byte(input[pos - 1]);
                    let after = pos < input.len() && is_word_byte(input[pos]);
                    if before != after {
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::NotWordBoundary => {
                    let before = pos > 0 && is_word_byte(input[pos - 1]);
                    let after = pos < input.len() && is_word_byte(input[pos]);
                    if before == after {
                        pc += 1;
                    } else if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }

                Op::Backref(group) => {
                    let idx = group as usize;
                    let s = slots[idx * 2];
                    let e = slots[idx * 2 + 1];
                    if s >= 0 && e >= 0 {
                        let captured = &input[s as usize..e as usize];
                        let len = captured.len();
                        if pos + len <= input.len() && &input[pos..pos + len] == captured {
                            pos += len;
                            pc += 1;
                            continue;
                        }
                    }
                    if !trail.pop(&mut pc, &mut pos, &mut run_start, slots, progress) {
                        return Ok(false);
                    }
                }
            }
        }
    }
}

/// Working storage for [`BacktrackingVm::exec`], reused across start positions.
///
/// A search tries every start offset in turn, so anything allocated per call is
/// allocated per byte of input. Hoisting it here keeps that to one allocation
/// per search.
struct Scratch {
    trail: Trail,
    progress: Vec<usize>,
}

impl Scratch {
    fn new(progress_regs: usize) -> Self {
        Self {
            trail: Trail::new(progress_regs),
            progress: vec![usize::MAX; progress_regs],
        }
    }
}

/// Choice points, and the values that have to be put back when one is resumed.
///
/// Everything a choice point has to undo is recorded against it: the capture
/// slots and the progress registers overwritten since it was pushed. Each is a
/// flat stack plus a frame index per choice point, so resuming is a pop and a
/// short unwind rather than a copy of the whole slot array.
struct Trail {
    /// `(pc, pos, aux)` to resume from, innermost last. `aux` is where a
    /// [`Op::RunScan`] run began, and unused by every other entry.
    stack: Vec<(u32, usize, usize)>,
    /// Capture slots overwritten since a choice point, with their old values.
    slots: Vec<(u16, i32)>,
    /// Where each choice point's run of `slots` begins.
    slot_frames: Vec<usize>,
    /// Progress registers overwritten since a choice point, with old values.
    regs: Vec<(u16, usize)>,
    /// Where each choice point's run of `regs` begins.
    reg_frames: Vec<usize>,
}

impl Trail {
    fn new(progress_regs: usize) -> Self {
        // The register side stays empty for the patterns that have no nullable
        // loop, which is most of them.
        let (regs, reg_frames) = if progress_regs > 0 {
            (Vec::with_capacity(16), Vec::with_capacity(32))
        } else {
            (Vec::new(), Vec::new())
        };
        Self {
            stack: Vec::with_capacity(32),
            slots: Vec::with_capacity(64),
            slot_frames: Vec::with_capacity(32),
            regs,
            reg_frames,
        }
    }

    fn clear(&mut self) {
        self.stack.clear();
        self.slots.clear();
        self.slot_frames.clear();
        self.regs.clear();
        self.reg_frames.clear();
    }

    /// Records a choice point resuming at `pc` with the input at `pos`.
    #[inline]
    fn push(&mut self, pc: u32, pos: usize, progress_regs: usize) {
        self.push_run(pc, pos, 0, progress_regs);
    }

    /// [`Self::push`] for a [`Op::RunScan`] choice point, which also has to
    /// remember where its run began.
    #[inline]
    fn push_run(&mut self, pc: u32, pos: usize, start: usize, progress_regs: usize) {
        self.slot_frames.push(self.slots.len());
        if progress_regs > 0 {
            self.reg_frames.push(self.regs.len());
        }
        self.stack.push((pc, pos, start));
    }

    /// Notes that `slot` is about to be overwritten, so the innermost choice
    /// point can put `old` back.
    #[inline]
    fn record_slot(&mut self, slot: u16, old: i32) {
        if let Some(&frame) = self.slot_frames.last() {
            // One entry per slot per frame: the first is the value to restore,
            // later writes in the same frame are undone by that same entry.
            if !self.slots[frame..].iter().any(|&(s, _)| s == slot) {
                self.slots.push((slot, old));
            }
        }
    }

    /// Notes that progress register `reg` is about to be overwritten.
    #[inline]
    fn record_reg(&mut self, reg: u16, old: usize) {
        if let Some(&frame) = self.reg_frames.last() {
            if !self.regs[frame..].iter().any(|&(r, _)| r == reg) {
                self.regs.push((reg, old));
            }
        }
    }

    /// Resumes the innermost choice point, restoring everything it recorded.
    /// Returns false when there is none left and the search has failed.
    #[inline]
    fn pop(
        &mut self,
        pc: &mut u32,
        pos: &mut usize,
        run_start: &mut usize,
        slots: &mut [i32],
        progress: &mut [usize],
    ) -> bool {
        let Some((saved_pc, saved_pos, saved_start)) = self.stack.pop() else {
            return false;
        };
        *run_start = saved_start;
        if let Some(frame) = self.slot_frames.pop() {
            for (slot, val) in self.slots.drain(frame..).rev() {
                slots[slot as usize] = val;
            }
        }
        if let Some(frame) = self.reg_frames.pop() {
            for (reg, val) in self.regs.drain(frame..).rev() {
                progress[reg as usize] = val;
            }
        }
        *pc = saved_pc;
        *pos = saved_pos;
        true
    }
}

/// Compiler from HIR to bytecode.
struct Compiler {
    code: Vec<Op>,
    byte_classes: Vec<ByteClass>,
    cp_classes: Vec<CodepointClass>,
    /// Progress registers handed out to nullable-body loops so far.
    progress_regs: u16,
}

impl Compiler {
    fn new() -> Self {
        Self {
            code: Vec::with_capacity(64),
            byte_classes: Vec::new(),
            cp_classes: Vec::new(),
            progress_regs: 0,
        }
    }

    fn emit(&mut self, op: Op) {
        self.code.push(op);
    }

    fn pc(&self) -> u32 {
        self.code.len() as u32
    }

    /// Interns a byte class and returns its index in `byte_classes`.
    fn byte_class(&mut self, ranges: Vec<ByteRange>) -> u16 {
        let index = self.byte_classes.len() as u16;
        self.byte_classes.push(ByteClass::new(ranges));
        index
    }

    /// Reserves a progress register for one repetition.
    ///
    /// Registers are per loop *node*, never shared, so a loop only ever reads
    /// back a position its own [`Op::MarkPos`] wrote.
    fn progress_reg(&mut self) -> u16 {
        let reg = self.progress_regs;
        self.progress_regs += 1;
        reg
    }

    fn compile(&mut self, expr: &HirExpr) {
        match expr {
            HirExpr::Empty => {}

            HirExpr::Literal(bytes) => {
                for &b in bytes {
                    self.emit(Op::Byte(b));
                }
            }

            HirExpr::Class(class) => {
                if class.ranges.len() <= 4 {
                    let mut ranges = [(0u8, 0u8); 4];
                    for (i, &(lo, hi)) in class.ranges.iter().enumerate() {
                        ranges[i] = (lo, hi);
                    }
                    if class.negated {
                        self.emit(Op::NotByteRanges {
                            count: class.ranges.len() as u8,
                            ranges,
                        });
                    } else {
                        self.emit(Op::ByteRanges {
                            count: class.ranges.len() as u8,
                            ranges,
                        });
                    }
                } else {
                    // Too many ranges - store in byte_classes table and use ByteClassRef
                    let index = self.byte_classes.len() as u16;
                    // Convert (u8, u8) tuples to ByteRange and create ByteClass with bitmap
                    let byte_ranges: Vec<ByteRange> = class
                        .ranges
                        .iter()
                        .map(|&(lo, hi)| ByteRange::new(lo, hi))
                        .collect();
                    self.byte_classes.push(ByteClass::new(byte_ranges));
                    self.emit(Op::ByteClassRef {
                        index,
                        negated: class.negated,
                    });
                }
            }

            HirExpr::UnicodeCpClass(class) => {
                if class.ranges.len() == 1 {
                    // Single range - use inline op
                    let (lo, hi) = class.ranges[0];
                    if class.negated {
                        self.emit(Op::NotCpRange(lo, hi));
                    } else {
                        self.emit(Op::CpRange(lo, hi));
                    }
                } else if !class.ranges.is_empty() {
                    // Multiple ranges - store full CodepointClass for ASCII bitmap optimization
                    let index = self.cp_classes.len() as u16;
                    self.cp_classes.push(class.clone());
                    self.emit(Op::CpClassRef {
                        index,
                        negated: class.negated,
                    });
                }
            }

            HirExpr::Concat(parts) => {
                for part in parts {
                    self.compile(part);
                }
            }

            HirExpr::Alt(alts) => {
                if alts.is_empty() {
                    return;
                }
                if alts.len() == 1 {
                    self.compile(&alts[0]);
                    return;
                }

                // For each alternative except the last, emit Split
                let mut jump_patches = Vec::new();

                for (i, alt) in alts.iter().enumerate() {
                    if i + 1 < alts.len() {
                        let split_pc = self.pc();
                        self.emit(Op::Split(0)); // Placeholder, will patch

                        self.compile(alt);

                        let jump_pc = self.pc();
                        self.emit(Op::Jump(0)); // Placeholder
                        jump_patches.push(jump_pc);

                        // Patch split target to here
                        let target = self.pc();
                        self.code[split_pc as usize] = Op::Split(target);
                    } else {
                        self.compile(alt);
                    }
                }

                // Patch all jumps to after the alternation
                let after = self.pc();
                for jp in jump_patches {
                    self.code[jp as usize] = Op::Jump(after);
                }
            }

            HirExpr::Repeat(rep) => {
                let min = rep.min;
                let max = rep.max;
                let greedy = rep.greedy;

                // Emit min copies
                for _ in 0..min {
                    self.compile(&rep.expr);
                }

                match max {
                    Some(max_val) if max_val == min => {
                        // Exact count, nothing more to do
                    }
                    Some(max_val) => {
                        // {min, max}: emit (max - min) optional copies
                        for _ in min..max_val {
                            if greedy {
                                let split_pc = self.pc();
                                self.emit(Op::Split(0)); // Try match, on fail skip
                                self.compile(&rep.expr);
                                let target = self.pc();
                                self.code[split_pc as usize] = Op::Split(target);
                            } else {
                                // Non-greedy: try skip first
                                let split_pc = self.pc();
                                self.emit(Op::Split(0));
                                let jump_pc = self.pc();
                                self.emit(Op::Jump(0));
                                let target = self.pc();
                                self.code[split_pc as usize] = Op::Split(target);
                                self.compile(&rep.expr);
                                let after = self.pc();
                                self.code[jump_pc as usize] = Op::Jump(after);
                            }
                        }
                    }
                    None => {
                        // Unbounded: *
                        //
                        // A body that can match the empty string would otherwise
                        // re-enter the loop at a position it can never advance
                        // past, so such a loop carries a progress register: the
                        // body's entry position goes in on the way in, and the
                        // back edge is only taken if the position moved.
                        let guard =
                            crate::hir::matches_empty(&rep.expr).then(|| self.progress_reg());

                        // A leading run of single-byte matches, consumed before
                        // the loop and recorded as one choice point instead of
                        // one per byte. The loop still runs afterwards, so a
                        // body that can also match something wider (a negated
                        // class reaches past ASCII) keeps working.
                        //
                        // The `min` copies were emitted above, so this loop is
                        // always a plain `*` and the run has no minimum to meet.
                        let run_scan = greedy
                            .then(|| crate::literal::single_byte_run_set(&rep.expr))
                            .flatten()
                            .map(|set| {
                                let ranges: Vec<ByteRange> = set
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, member)| **member != 0)
                                    .map(|(byte, _)| ByteRange::new(byte as u8, byte as u8))
                                    .collect();
                                let class = self.byte_class(ranges);
                                let scan_pc = self.pc();
                                self.emit(Op::RunScan { class, retry: 0 });
                                scan_pc
                            });

                        let loop_start = self.pc();
                        if greedy {
                            let split_pc = self.pc();
                            self.emit(Op::Split(0)); // Try match, on fail exit
                            if let Some(reg) = guard {
                                self.emit(Op::MarkPos(reg));
                            }
                            self.compile(&rep.expr);
                            if let Some(reg) = guard {
                                self.emit(Op::AssertProgress(reg));
                            }
                            self.emit(Op::Jump(loop_start));
                            // The retry lands between the loop and its exit, so
                            // the loop's own exit skips over it and the retry
                            // falls straight through once it has given a byte
                            // back.
                            let retry = self.pc();
                            if let Some(scan_pc) = run_scan {
                                self.emit(Op::RunRetry);
                                if let Op::RunScan { retry: slot, .. } =
                                    &mut self.code[scan_pc as usize]
                                {
                                    *slot = retry;
                                }
                            }
                            let exit = self.pc();
                            self.code[split_pc as usize] = Op::Split(exit);
                        } else {
                            // Non-greedy: try exit first
                            let split_pc = self.pc();
                            self.emit(Op::Split(0));
                            let jump_pc = self.pc();
                            self.emit(Op::Jump(0));
                            let loop_body = self.pc();
                            self.code[split_pc as usize] = Op::Split(loop_body);
                            if let Some(reg) = guard {
                                self.emit(Op::MarkPos(reg));
                            }
                            self.compile(&rep.expr);
                            if let Some(reg) = guard {
                                self.emit(Op::AssertProgress(reg));
                            }
                            self.emit(Op::Jump(loop_start));
                            let exit = self.pc();
                            self.code[jump_pc as usize] = Op::Jump(exit);
                        }
                    }
                }
            }

            HirExpr::Capture(cap) => {
                let start_slot = (cap.index as u16) * 2;
                let end_slot = start_slot + 1;

                self.emit(Op::Save(start_slot));
                self.compile(&cap.expr);
                self.emit(Op::Save(end_slot));
            }

            HirExpr::Backref(group) => {
                self.emit(Op::Backref(*group as u16));
            }

            HirExpr::Anchor(anchor) => match anchor {
                HirAnchor::Start => {
                    self.emit(Op::StartAnchor);
                }
                HirAnchor::StartLine => {
                    self.emit(Op::StartLineAnchor);
                }
                HirAnchor::End => {
                    self.emit(Op::EndAnchor);
                }
                HirAnchor::EndLine => {
                    self.emit(Op::EndLineAnchor);
                }
                HirAnchor::WordBoundary => {
                    self.emit(Op::WordBoundary);
                }
                HirAnchor::NotWordBoundary => {
                    self.emit(Op::NotWordBoundary);
                }
            },

            HirExpr::Lookaround(_) => {
                // Not supported in this VM
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::translate;
    use crate::parser::parse;

    fn make_vm(pattern: &str) -> BacktrackingVm {
        let ast = parse(pattern).unwrap();
        let hir = translate(&ast).unwrap();
        BacktrackingVm::new(&hir)
    }

    #[test]
    fn test_simple_literal() {
        let vm = make_vm("hello");
        assert_eq!(vm.find(b"hello world"), Some((0, 5)));
        assert_eq!(vm.find(b"say hello"), Some((4, 9)));
        assert_eq!(vm.find(b"goodbye"), None);
    }

    #[test]
    fn test_alternation() {
        let vm = make_vm("a|b");
        assert_eq!(vm.find(b"a"), Some((0, 1)));
        assert_eq!(vm.find(b"b"), Some((0, 1)));
        assert_eq!(vm.find(b"c"), None);
    }

    #[test]
    fn test_star() {
        let vm = make_vm("a*");
        assert_eq!(vm.find(b"aaa"), Some((0, 3)));
        assert_eq!(vm.find(b"b"), Some((0, 0)));
    }

    #[test]
    fn test_capture_in_star() {
        let vm = make_vm("x(a|b)*y");
        assert_eq!(vm.find(b"xy"), Some((0, 2)));
        assert_eq!(vm.find(b"xay"), Some((0, 3)));
        assert_eq!(vm.find(b"xby"), Some((0, 3)));
        assert_eq!(vm.find(b"xaby"), Some((0, 4)));
        assert_eq!(vm.find(b"xaaby"), Some((0, 5)));
    }

    #[test]
    fn test_json_string() {
        let vm = make_vm(r#""([^"\\]|\\.)*""#);
        assert_eq!(vm.find(br#""""#), Some((0, 2)));
        assert_eq!(vm.find(br#""hello""#), Some((0, 7)));
        assert_eq!(vm.find(br#""hello\"world""#), Some((0, 14)));
        assert_eq!(vm.find(br#""\\""#), Some((0, 4)));
    }

    #[test]
    fn test_captures() {
        let vm = make_vm(r#"(a)(b)(c)"#);
        let caps = vm.captures(b"abc").unwrap();
        assert_eq!(caps[0], Some((0, 3)));
        assert_eq!(caps[1], Some((0, 1)));
        assert_eq!(caps[2], Some((1, 2)));
        assert_eq!(caps[3], Some((2, 3)));
    }
}
