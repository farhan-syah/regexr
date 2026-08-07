//! One-pass capture engine.
//!
//! Most engines here report only match bounds, so `captures()` recovers the slot
//! positions with a second pass of the PikeVM over the matched text. For a
//! *deterministic* pattern that second pass is pure overhead: if, from any point
//! in the automaton, at most one transition can consume a given byte, then the
//! path through the NFA is forced by the input alone. There is nothing to
//! simulate — the slots can be written during a single left-to-right scan with no
//! thread set, no backtracking and no per-byte allocation.
//!
//! [`OnePass`] compiles exactly those patterns and refuses every other one, so a
//! failed compilation simply leaves the caller on its existing PikeVM path.
//!
//! # Representation
//!
//! Because transitions out of a closure are disjoint, exactly one of them fires
//! per byte and it leads to a single NFA state. A closure is therefore identified
//! by the NFA state that generates it, and the machine is an ordinary DFA over
//! those closures: a 256-entry byte table selects the transition in O(1), and
//! each transition carries the capture actions found on the epsilon path that
//! reaches it. Actions live in one flat arena addressed by `(start, len)` spans,
//! so a transition is two `u32`s plus a target.
//!
//! # Agreement with the PikeVM
//!
//! The scan reproduces [`crate::vm::PikeVm`]'s decisions rather than
//! approximating them:
//!
//! - closures are built by walking `state.epsilon` in order (index 0 = highest
//!   priority, which is what encodes greedy vs non-greedy) with first-arrival
//!   deduplication, matching `add_thread`'s DFS;
//! - the walk stops at the first *unconditional* match state in that order,
//!   mirroring the PikeVM's `limit`: threads at or after the matching one never
//!   advance, so any transition found later is dead. A non-greedy exit therefore
//!   ends the scan exactly where the VM would end it;
//! - capture actions are applied with the same rules as the PikeVM's
//!   `reconstruct_captures` (`pike/shared.rs`) — a start overwrites
//!   the slot with `(pos, pos)` and an end extends an already-started slot — so a
//!   group inside a repetition reports its last iteration.
//!
//! # Assertions
//!
//! An anchor or word boundary cannot be baked into a static transition table,
//! because whether it holds depends on the scan position. It can, however, be
//! *carried* by the transition it gates: every assertion on the epsilon path to
//! an item becomes a [`Guard`] evaluated at the position the item fires. That
//! keeps the table static while letting the run prune the same branches the
//! PikeVM prunes.
//!
//! A guarded match no longer cuts the walk short, so items after it stay
//! reachable and each records its DFS `order`. At run time the first match whose
//! guards hold sets the limit, and a transition after that limit is dead — the
//! same rule the PikeVM applies, resolved per position instead of once.

use crate::nfa::{
    at_end_or_before_final_newline, is_word_boundary, ByteRange, Nfa, NfaInstruction, StateId,
};
use std::collections::HashMap;

/// Upper bound on distinct closures, so compiling a pathological NFA cannot blow
/// up. A pattern past this stays on the PikeVM path.
const MAX_CLOSURES: usize = 512;

/// Byte-table entry meaning "no transition accepts this byte".
const NO_TRANSITION: u8 = u8::MAX;

/// Transitions a closure may hold, bounded by the byte table's index width.
const MAX_TRANSITIONS: usize = NO_TRANSITION as usize;

/// A capture slot write performed while passing through an epsilon path.
#[derive(Debug, Clone, Copy)]
enum Action {
    /// Open capture group `n` at the current position.
    Start(u32),
    /// Close capture group `n` at the current position.
    End(u32),
}

/// A position assertion gating an epsilon path, evaluated during the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guard {
    StartOfText,
    EndOfText,
    StartOfLine,
    EndOfLine,
    WordBoundary,
    NotWordBoundary,
}

impl Guard {
    /// Whether the assertion holds at `pos`. Mirrors `PikeVm::process_instruction`.
    #[inline]
    fn holds(self, input: &[u8], pos: usize) -> bool {
        match self {
            Self::StartOfText => pos == 0,
            Self::EndOfText => at_end_or_before_final_newline(input, pos),
            Self::StartOfLine => pos == 0 || input.get(pos.wrapping_sub(1)) == Some(&b'\n'),
            Self::EndOfLine => pos == input.len() || input.get(pos) == Some(&b'\n'),
            Self::WordBoundary => is_word_boundary(input, pos),
            Self::NotWordBoundary => !is_word_boundary(input, pos),
        }
    }
}

/// A range of [`OnePass::actions`], applied in order.
#[derive(Debug, Clone, Copy)]
struct ActionSpan {
    start: u32,
    len: u32,
}

impl ActionSpan {
    /// The empty span, for an epsilon path that writes no slots.
    const EMPTY: Self = Self { start: 0, len: 0 };
}

/// A range of [`OnePass::guards`], all of which must hold.
#[derive(Debug, Clone, Copy)]
struct GuardSpan {
    start: u32,
    len: u32,
}

impl GuardSpan {
    /// The empty span, for an unconditional epsilon path.
    const EMPTY: Self = Self { start: 0, len: 0 };

    /// Whether this path is unconditional, so the run can skip evaluation.
    #[inline]
    const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// One resolved transition: the closure to enter and the slots to write first.
#[derive(Debug, Clone, Copy)]
struct Transition {
    target: u32,
    actions: ActionSpan,
    guards: GuardSpan,
    /// Position in the closure's DFS order, against which a match's `order`
    /// decides whether this transition is still live.
    order: u32,
}

/// A match reachable from a closure, and the assertions that gate it.
#[derive(Debug, Clone, Copy)]
struct MatchItem {
    actions: ActionSpan,
    guards: GuardSpan,
    order: u32,
}

/// An epsilon-closure, compiled to a deterministic byte dispatch.
#[derive(Debug)]
struct Closure {
    /// Byte value to index into `transitions`, or [`NO_TRANSITION`].
    table: [u8; 256],
    transitions: Vec<Transition>,
    /// Matches reachable from this closure, in DFS order. Empty when no match
    /// can end here; more than one entry only when assertions gate them.
    matches: Vec<MatchItem>,
}

/// A deterministic capture engine for one-pass patterns.
#[derive(Debug)]
pub struct OnePass {
    /// Closure 0 is the start closure: the NFA's start state is interned first.
    closures: Vec<Closure>,
    /// Every capture action, addressed by an `ActionSpan`.
    actions: Vec<Action>,
    /// Every path assertion, addressed by a `GuardSpan`.
    guards: Vec<Guard>,
    /// Number of capture slots, including slot 0 for the whole match.
    slot_count: usize,
}

impl OnePass {
    /// Compiles `nfa` if it is one-pass; returns `None` otherwise.
    ///
    /// The pattern is one-pass when, for every reachable closure, the byte ranges
    /// of the transitions that can still advance are pairwise disjoint — a single
    /// shared byte value is enough to reject. Rejected outright: backreferences,
    /// lookaround, Unicode codepoint classes, a state reachable under two
    /// different sets of assertions (the byte alone would no longer decide the
    /// path), and any NFA needing more than `MAX_CLOSURES` closures.
    pub fn compile(nfa: &Nfa) -> Option<Self> {
        if nfa.has_backrefs || nfa.has_lookaround {
            return None;
        }

        // Closure `i` is generated by `roots[i]`; `ids` maps back the other way so
        // a transition target resolves to an existing closure.
        let mut ids: HashMap<StateId, u32> = HashMap::new();
        let mut roots: Vec<StateId> = Vec::new();
        ids.insert(nfa.start, 0);
        roots.push(nfa.start);

        let mut closures: Vec<Closure> = Vec::new();
        let mut actions: Vec<Action> = Vec::new();
        let mut guards: Vec<Guard> = Vec::new();
        let mut next = 0;

        while next < roots.len() {
            let Some(&root) = roots.get(next) else {
                break;
            };
            next += 1;

            let raw = expand_closure(nfa, root)?;
            if raw.transitions.len() > MAX_TRANSITIONS {
                return None;
            }

            let mut table = [NO_TRANSITION; 256];
            let mut transitions = Vec::with_capacity(raw.transitions.len());
            for (index, raw_transition) in raw.transitions.iter().enumerate() {
                let range = raw_transition.range;
                for byte in range.start..=range.end {
                    let entry = table.get_mut(byte as usize)?;
                    if *entry != NO_TRANSITION {
                        // Two transitions accept this byte: not one-pass. The
                        // check spans the whole closure, including transitions
                        // behind a guard, so the byte decides the path before
                        // any assertion is consulted.
                        return None;
                    }
                    *entry = index as u8;
                }
                transitions.push(Transition {
                    target: intern(&mut ids, &mut roots, raw_transition.target)?,
                    actions: push_actions(&mut actions, &raw_transition.path.actions)?,
                    guards: push_guards(&mut guards, &raw_transition.path.guards)?,
                    order: raw_transition.order,
                });
            }

            let mut matches = Vec::with_capacity(raw.matches.len());
            for raw_match in &raw.matches {
                matches.push(MatchItem {
                    actions: push_actions(&mut actions, &raw_match.path.actions)?,
                    guards: push_guards(&mut guards, &raw_match.path.guards)?,
                    order: raw_match.order,
                });
            }

            closures.push(Closure {
                table,
                transitions,
                matches,
            });
        }

        Some(Self {
            closures,
            actions,
            guards,
            slot_count: nfa.capture_count as usize + 1,
        })
    }

    /// Capture slots for a match that begins exactly at `start`.
    ///
    /// Slot 0 is the whole match. Returns `None` if no match begins there. The
    /// returned vector always has `capture_count + 1` entries, and a group the
    /// match never entered stays `None`.
    pub fn captures_at(&self, input: &[u8], start: usize) -> Option<Vec<Option<(usize, usize)>>> {
        if start > input.len() {
            return None;
        }

        // Slots for the path currently being walked, and a snapshot taken at each
        // position a match could end. The snapshot is what a later, longer match
        // overwrites — the live slots keep advancing past it.
        let mut slots = vec![None; self.slot_count];
        let mut match_slots = vec![None; self.slot_count];
        let mut match_end: Option<usize> = None;

        let mut closure = self.closures.first()?;
        let mut pos = start;
        loop {
            // The first match whose assertions hold both records a candidate end
            // and kills every lower-priority item, transitions included.
            let mut limit = u32::MAX;
            for candidate in &closure.matches {
                if self.guards_hold(candidate.guards, input, pos) {
                    match_slots.copy_from_slice(&slots);
                    self.apply(&mut match_slots, candidate.actions, pos);
                    match_end = Some(pos);
                    limit = candidate.order;
                    break;
                }
            }

            let byte = match input.get(pos) {
                Some(&byte) => byte,
                None => break,
            };
            let index = match closure.table.get(byte as usize) {
                Some(&index) if index != NO_TRANSITION => index as usize,
                _ => break,
            };
            let Some(&transition) = closure.transitions.get(index) else {
                break;
            };
            // Transitions are disjoint on bytes, so a dead or unsatisfied one has
            // no alternative to fall back to.
            if transition.order > limit || !self.guards_hold(transition.guards, input, pos) {
                break;
            }
            let Some(target) = self.closures.get(transition.target as usize) else {
                break;
            };

            self.apply(&mut slots, transition.actions, pos);
            closure = target;
            pos += 1;
        }

        let end = match_end?;
        if let Some(slot) = match_slots.first_mut() {
            *slot = Some((start, end));
        }
        Some(match_slots)
    }

    /// Whether every assertion on a path holds at `pos`.
    #[inline]
    fn guards_hold(&self, span: GuardSpan, input: &[u8], pos: usize) -> bool {
        if span.is_empty() {
            return true;
        }
        let range = span.start as usize..span.start as usize + span.len as usize;
        match self.guards.get(range) {
            Some(guards) => guards.iter().all(|guard| guard.holds(input, pos)),
            // Unreachable for a span this type produced; refusing is the safe
            // reading, since the caller falls back to the PikeVM on no match.
            None => false,
        }
    }

    /// Applies a span of capture actions at `pos`.
    ///
    /// The rules are `reconstruct_captures`': a start overwrites the slot, so the
    /// last iteration of a repetition wins, and an end only extends a slot that
    /// was already started.
    #[inline]
    fn apply(&self, slots: &mut [Option<(usize, usize)>], span: ActionSpan, pos: usize) {
        let range = span.start as usize..span.start as usize + span.len as usize;
        let Some(actions) = self.actions.get(range) else {
            return;
        };
        for action in actions {
            match *action {
                Action::Start(index) => {
                    if let Some(slot) = slots.get_mut(index as usize) {
                        *slot = Some((pos, pos));
                    }
                }
                Action::End(index) => {
                    if let Some(slot) = slots.get_mut(index as usize) {
                        if let Some((slot_start, _)) = *slot {
                            *slot = Some((slot_start, pos));
                        }
                    }
                }
            }
        }
    }
}

/// What an epsilon path accumulates on its way to a transition or a match.
#[derive(Debug, Clone, Default)]
struct Path {
    actions: Vec<Action>,
    guards: Vec<Guard>,
}

/// A transition collected from an epsilon closure, before its target has been
/// resolved to a closure index.
struct RawTransition {
    range: ByteRange,
    target: StateId,
    path: Path,
    order: u32,
}

/// A match state collected from an epsilon closure.
struct RawMatch {
    path: Path,
    order: u32,
}

/// The result of walking one epsilon closure.
struct RawClosure {
    transitions: Vec<RawTransition>,
    matches: Vec<RawMatch>,
}

/// Walks the epsilon closure of `root`, collecting the transitions that can
/// advance and the capture actions on the path to each.
///
/// The walk is the PikeVM's: a depth-first traversal in epsilon order (children
/// pushed in reverse so the highest-priority one pops first) with first-arrival
/// deduplication. It stops at the first *unconditional* match state, because the
/// VM lets only threads before the matching one consume a byte — transitions
/// found after it are unreachable, and stopping there is what makes a non-greedy
/// exit end the scan. A match behind an assertion may not fire, so the walk
/// continues past it and the `order` counter records the priority the run needs
/// to reproduce that cut.
///
/// Returns `None` when the closure contains a construct this engine does not
/// model, or when a state is reachable under two different sets of assertions —
/// then which path a byte takes is no longer decided by the byte.
fn expand_closure(nfa: &Nfa, root: StateId) -> Option<RawClosure> {
    let mut visited: Vec<Option<Vec<Guard>>> = vec![None; nfa.states.len()];
    let mut stack: Vec<(StateId, Path)> = vec![(root, Path::default())];
    let mut transitions = Vec::new();
    let mut matches = Vec::new();
    let mut order = 0u32;

    while let Some((state_id, mut path)) = stack.pop() {
        let seen = visited.get_mut(state_id as usize)?;
        if let Some(previous) = seen {
            // First arrival won, as in the PikeVM. That is only faithful while
            // both arrivals are gated the same way; otherwise the dropped path
            // could be the live one at some position.
            if *previous != path.guards {
                return None;
            }
            continue;
        }
        *seen = Some(path.guards.clone());

        let state = nfa.get(state_id)?;
        match state.instruction {
            None | Some(NfaInstruction::NonGreedyExit) => {}
            Some(NfaInstruction::CaptureStart(index)) => path.actions.push(Action::Start(index)),
            Some(NfaInstruction::CaptureEnd(index)) => path.actions.push(Action::End(index)),
            Some(NfaInstruction::StartOfText) => path.guards.push(Guard::StartOfText),
            Some(NfaInstruction::EndOfText) => path.guards.push(Guard::EndOfText),
            Some(NfaInstruction::StartOfLine) => path.guards.push(Guard::StartOfLine),
            Some(NfaInstruction::EndOfLine) => path.guards.push(Guard::EndOfLine),
            Some(NfaInstruction::WordBoundary) => path.guards.push(Guard::WordBoundary),
            Some(NfaInstruction::NotWordBoundary) => path.guards.push(Guard::NotWordBoundary),
            // Backreferences, lookaround and codepoint classes are not byte
            // transitions at all.
            Some(_) => return None,
        }

        if state.is_match {
            let unconditional = path.guards.is_empty();
            matches.push(RawMatch { path, order });
            order += 1;
            if unconditional {
                break;
            }
            continue;
        }

        for &(range, target) in &state.transitions {
            transitions.push(RawTransition {
                range,
                target,
                path: path.clone(),
                order,
            });
            order += 1;
        }
        for &next in state.epsilon.iter().rev() {
            stack.push((next, path.clone()));
        }
    }

    Some(RawClosure {
        transitions,
        matches,
    })
}

/// Returns the closure index for `state`, queueing it for expansion when it is
/// new. `None` once `MAX_CLOSURES` is reached.
fn intern(
    ids: &mut HashMap<StateId, u32>,
    roots: &mut Vec<StateId>,
    state: StateId,
) -> Option<u32> {
    if let Some(&index) = ids.get(&state) {
        return Some(index);
    }
    if roots.len() >= MAX_CLOSURES {
        return None;
    }
    let index = u32::try_from(roots.len()).ok()?;
    ids.insert(state, index);
    roots.push(state);
    Some(index)
}

/// Appends `path` to the action arena and returns the span addressing it.
fn push_actions(arena: &mut Vec<Action>, path: &[Action]) -> Option<ActionSpan> {
    if path.is_empty() {
        return Some(ActionSpan::EMPTY);
    }
    let start = u32::try_from(arena.len()).ok()?;
    let len = u32::try_from(path.len()).ok()?;
    arena.extend_from_slice(path);
    Some(ActionSpan { start, len })
}

/// Appends `path` to the guard arena and returns the span addressing it.
fn push_guards(arena: &mut Vec<Guard>, path: &[Guard]) -> Option<GuardSpan> {
    if path.is_empty() {
        return Some(GuardSpan::EMPTY);
    }
    let start = u32::try_from(arena.len()).ok()?;
    let len = u32::try_from(path.len()).ok()?;
    arena.extend_from_slice(path);
    Some(GuardSpan { start, len })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{translate, CodepointClass};
    use crate::nfa::{self, NfaState};
    use crate::parser::parse;
    use crate::vm::PikeVm;

    fn build_nfa(pattern: &str) -> Nfa {
        let ast = parse(pattern).unwrap();
        let hir = translate(&ast).unwrap();
        nfa::compile(&hir).unwrap()
    }

    fn compile(pattern: &str) -> Option<OnePass> {
        OnePass::compile(&build_nfa(pattern))
    }

    /// Asserts that the one-pass slots equal the PikeVM's for every input, at the
    /// start position the PikeVM itself reports.
    fn assert_agrees_with_pike(pattern: &str, inputs: &[&str]) {
        let nfa = build_nfa(pattern);
        let one_pass = OnePass::compile(&nfa).expect("pattern should be one-pass");
        let vm = PikeVm::new(nfa);

        for input in inputs {
            let bytes = input.as_bytes();
            let expected = vm.captures(bytes);
            let start = match expected.as_ref().and_then(|caps| caps[0]) {
                Some((start, _)) => start,
                None => {
                    assert_eq!(
                        one_pass.captures_at(bytes, 0),
                        None,
                        "pattern {pattern:?} input {input:?}: expected no match at 0"
                    );
                    continue;
                }
            };
            assert_eq!(
                one_pass.captures_at(bytes, start),
                expected,
                "pattern {pattern:?} input {input:?}"
            );
        }
    }

    #[test]
    fn test_compiles_deterministic_pattern() {
        assert!(compile(r"(\d{4})-(\d{2})").is_some());
        assert!(compile(r"(\w+)@(\w+)\.com").is_some());
        assert!(compile(r"a(b)*c").is_some());
    }

    #[test]
    fn test_rejects_overlapping_alternation() {
        // Both branches accept 'a' from the start closure, so the byte alone does
        // not decide the path.
        assert!(compile("(a|ab)c").is_none());
        assert!(compile("(ab|a)c").is_none());
    }

    #[test]
    fn test_rejects_backreference() {
        assert!(compile(r"(a+)\1").is_none());
    }

    #[test]
    fn test_rejects_lookaround() {
        assert!(compile(r"(a)(?=b)").is_none());
        assert!(compile(r"(?<=a)(b)").is_none());
    }

    #[test]
    fn test_compiles_anchors_and_boundaries() {
        assert!(compile(r"^(a)").is_some());
        assert!(compile(r"(a)$").is_some());
        assert!(compile(r"\b(\w+)\b").is_some());
        assert!(compile(r"\B(a)").is_some());
        assert!(compile(r"^(\w+): *(\d+)$").is_some());
    }

    #[test]
    fn test_slots_match_pike_vm_with_anchors() {
        assert_agrees_with_pike(r"^(\d+)", &["123", "x123", "", "1"]);
        assert_agrees_with_pike(r"(\d+)$", &["123", "123x", "1"]);
        assert_agrees_with_pike(r"^(\w+):(\d+)$", &["ab:12", "ab:12x", ":1"]);
    }

    #[test]
    fn test_slots_match_pike_vm_with_word_boundaries() {
        assert_agrees_with_pike(r"\b(\w+)\b", &["hi there", " hi ", "", "_"]);
        assert_agrees_with_pike(r"\B(a)", &["ba", "a", "xa y"]);
    }

    /// A guarded match must not end the scan when its assertion fails: the
    /// greedy body has to keep consuming and try again further along.
    #[test]
    fn test_guarded_match_does_not_end_scan_early() {
        assert_agrees_with_pike(r"(a+)$", &["aaa", "aaab", "a"]);
        // Non-greedy: the exit is the highest-priority branch, so the loop
        // transition sits *after* the guarded match in DFS order and is only
        // reachable because the failing guard leaves it live.
        assert_agrees_with_pike(r"(a+?)$", &["aaa", "a", "aab"]);
    }

    /// An anchor that never holds must yield no match, not the unanchored one.
    #[test]
    fn test_anchor_that_fails_rejects_the_position() {
        let one_pass = compile(r"^(\d+)").unwrap();
        assert_eq!(one_pass.captures_at(b"x123", 1), None);

        let one_pass = compile(r"(\d+)$").unwrap();
        assert_eq!(one_pass.captures_at(b"123x", 0), None);
    }

    /// Two epsilon paths reaching one state under different assertions leave the
    /// byte unable to decide the path, so compilation must refuse.
    #[test]
    fn test_rejects_state_reachable_under_differing_guards() {
        assert!(compile(r"(?:\b|)(a)").is_none());
    }

    #[test]
    fn test_rejects_codepoint_class() {
        // Built directly: a codepoint class consumes a whole UTF-8 codepoint
        // rather than a byte, so it has no place in a byte transition table.
        let mut nfa = Nfa::new();
        let mut start = NfaState::new();
        start.instruction = Some(NfaInstruction::CodepointClass(
            CodepointClass::new(vec![(0x100, 0x200)], false),
            1,
        ));
        nfa.add_state(start);
        nfa.add_state(NfaState::match_state());
        nfa.start = 0;
        nfa.matches = vec![1];

        assert!(OnePass::compile(&nfa).is_none());
    }

    #[test]
    fn test_rejects_when_closure_cap_exceeded() {
        // A long chain of distinct positions needs one closure each.
        let pattern = "a".repeat(MAX_CLOSURES + 16);
        assert!(compile(&pattern).is_none());
    }

    #[test]
    fn test_slots_match_pike_vm() {
        assert_agrees_with_pike(
            r"(\d{4})-(\d{2})-(\d{2})",
            &["2024-05-17", "x2024-05-17x", "2024-05"],
        );
        assert_agrees_with_pike(r"(\w+)@(\w+)", &["user@host", "@host", "user@"]);
    }

    #[test]
    fn test_slots_match_pike_vm_nested_groups() {
        assert_agrees_with_pike(r"((\d+)-(\d+))", &["12-34", "1-2", "abc"]);
    }

    #[test]
    fn test_slots_match_pike_vm_group_in_repetition() {
        // Group 1 must report its LAST iteration, like the PikeVM's
        // reconstruct_captures.
        assert_agrees_with_pike(r"(?:(\d)x)+", &["1x2x3x", "1x", "x"]);

        let one_pass = compile(r"(?:(\d)x)+").unwrap();
        let slots = one_pass.captures_at(b"1x2x3x", 0).unwrap();
        assert_eq!(slots[0], Some((0, 6)));
        assert_eq!(slots[1], Some((4, 5)));
    }

    #[test]
    fn test_slots_match_pike_vm_optional_group() {
        assert_agrees_with_pike(r"(a)?b", &["ab", "b"]);

        let one_pass = compile(r"(a)?b").unwrap();
        let slots = one_pass.captures_at(b"b", 0).unwrap();
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0], Some((0, 1)));
        assert_eq!(slots[1], None, "unentered group stays None");
    }

    #[test]
    fn test_slots_match_pike_vm_empty_leading_group() {
        assert_agrees_with_pike(r"(a*)b", &["aab", "b"]);

        let one_pass = compile(r"(a*)b").unwrap();
        let slots = one_pass.captures_at(b"b", 0).unwrap();
        assert_eq!(slots[1], Some((0, 0)), "group matched empty at 0");
    }

    #[test]
    fn test_no_match_at_position() {
        let one_pass = compile(r"(\d+)").unwrap();
        assert_eq!(one_pass.captures_at(b"abc", 0), None);
        assert_eq!(one_pass.captures_at(b"abc", 9), None);
    }

    /// Broad sweep: whatever compiles must agree with the PikeVM everywhere.
    ///
    /// The targeted tests above cover the cases reasoning identified; this one
    /// covers the cases it did not, which is where an assertion-ordering mistake
    /// would hide.
    #[test]
    fn test_agrees_with_pike_vm_across_assertion_patterns() {
        const PATTERNS: &[&str] = &[
            r"^(a+)$",
            r"^(a*)(b*)$",
            r"(a+)$",
            r"^(a+)",
            r"\b(\w+)\b",
            r"\b(\w+)",
            r"(\w+)\b",
            r"\B(\w)",
            r"^(\w+)=(\w*)$",
            r"(a+?)$",
            r"^(a+?)",
            r"^(\d)(\d)?$",
            r"\b(\d+)\b",
            r"^$",
            r"^(x?)$",
            r"(?:(a)\b)+",
            r"^(a)|^(b)",
            r"\b(a+)$",
        ];
        const INPUTS: &[&str] = &[
            "", "a", "aa", "aaa", "b", "ab", "ba", "a b", " a ", "x=1", "=", "1", "12", "abc",
            "abc def", "_", "a\n", "\n", "aab", "x", "xy", "a1", "1a",
        ];

        let mut compiled = 0usize;
        for pattern in PATTERNS {
            let nfa = build_nfa(pattern);
            let Some(one_pass) = OnePass::compile(&nfa) else {
                continue;
            };
            compiled += 1;
            let vm = PikeVm::new(nfa);
            let mut ctx = vm.create_context();

            for input in INPUTS {
                let bytes = input.as_bytes();
                // Compare at every position, not just the VM's chosen start: the
                // executor calls `captures_at` with a start the search engine
                // found, which need not be the leftmost one.
                for start in 0..=bytes.len() {
                    let expected = vm.captures_with_context(bytes, &mut ctx, start);
                    assert_eq!(
                        one_pass.captures_at(bytes, start),
                        expected,
                        "pattern {pattern:?} input {input:?} start {start}"
                    );
                }
            }
        }

        // Guards against the sweep quietly becoming vacuous if acceptance narrows.
        assert!(
            compiled >= PATTERNS.len() * 2 / 3,
            "only {compiled} of {} patterns compiled",
            PATTERNS.len()
        );
    }

    #[test]
    fn test_slot_count_matches_capture_count() {
        let one_pass = compile(r"(a)(b)(c)").unwrap();
        let slots = one_pass.captures_at(b"abc", 0).unwrap();
        assert_eq!(slots.len(), 4);
    }
}
