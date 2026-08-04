//! Executable specification: a deliberately simple, obviously-correct
//! backtracking matcher defining regexr's canonical match semantics.
//!
//! This is the *spec*, not a production engine. It is intentionally written for
//! clarity over speed (recursive backtracking with explicit continuations) so it
//! can serve as the ground-truth oracle in differential tests: every real engine
//! (Shift-Or, DFA, DFA-JIT, PikeVM, TaggedNFA interpreter and JIT, …) must agree
//! with it. When they diverge, the engine is wrong, not the spec.
//!
//! ## Canonical semantics
//! Leftmost-first (Perl/PCRE/Python `regex`), greedy by default:
//! - `find` returns the match at the earliest start position; among matches at
//!   that start, the one preferred by ordered alternation / greedy quantifiers.
//! - Anchors follow PCRE/Python: `^`=start-of-text, `\A`=start; `$` matches at
//!   end of text *or immediately before a trailing `\n`*; `(?m)` switches `^`/`$`
//!   to line semantics; `\b`/`\B` use the ASCII word class `[0-9A-Za-z_]`.
//!
//! Operates on bytes; `UnicodeCpClass` decodes one UTF-8 codepoint.

use crate::hir::{HirAnchor, HirExpr, HirLookaroundKind};

type Caps = Vec<Option<(usize, usize)>>;

/// A continuation: "match the rest of the pattern starting at `pos`", returning
/// the final end position of the overall match (leftmost-first: first success).
type Cont<'c> = dyn FnMut(usize, &mut Caps) -> Option<usize> + 'c;

struct Matcher<'a> {
    input: &'a [u8],
}

impl<'a> Matcher<'a> {
    /// Matches `e` at `pos`; on success calls `k` with the position after `e`.
    fn m(&self, e: &HirExpr, pos: usize, caps: &mut Caps, k: &mut Cont<'_>) -> Option<usize> {
        match e {
            HirExpr::Empty => k(pos, caps),

            HirExpr::Literal(bytes) => {
                if self.input[pos..].starts_with(bytes) {
                    k(pos + bytes.len(), caps)
                } else {
                    None
                }
            }

            HirExpr::Class(class) => {
                let b = *self.input.get(pos)?;
                let in_ranges = class.ranges.iter().any(|&(lo, hi)| lo <= b && b <= hi);
                if in_ranges != class.negated {
                    k(pos + 1, caps)
                } else {
                    None
                }
            }

            HirExpr::UnicodeCpClass(cp) => {
                let (codepoint, len) = decode_utf8(&self.input[pos..])?;
                if cp.contains(codepoint) {
                    k(pos + len, caps)
                } else {
                    None
                }
            }

            HirExpr::Concat(es) => self.concat(es, 0, pos, caps, k),

            HirExpr::Alt(branches) => {
                for b in branches {
                    // Save captures so a failed branch leaves no residue.
                    let saved = caps.clone();
                    if let Some(end) = self.m(b, pos, caps, k) {
                        return Some(end);
                    }
                    *caps = saved;
                }
                None
            }

            HirExpr::Repeat(r) => {
                let max = r.max.map(|m| m as usize).unwrap_or(usize::MAX);
                self.repeat(&r.expr, r.min as usize, max, r.greedy, 0, pos, caps, k)
            }

            HirExpr::Capture(c) => {
                let idx = c.index as usize;
                let saved = if idx < caps.len() { caps[idx] } else { None };
                // Record the span once the inner expr and the rest both match.
                let start = pos;
                let res = self.m(&c.expr, pos, caps, &mut |p, caps| {
                    let prev = if idx < caps.len() { caps[idx] } else { None };
                    if idx < caps.len() {
                        caps[idx] = Some((start, p));
                    }
                    let r = k(p, caps);
                    if r.is_none() && idx < caps.len() {
                        caps[idx] = prev;
                    }
                    r
                });
                if res.is_none() && idx < caps.len() {
                    caps[idx] = saved;
                }
                res
            }

            HirExpr::Anchor(a) => {
                if self.assert(a, pos) {
                    k(pos, caps)
                } else {
                    None
                }
            }

            HirExpr::Lookaround(l) => {
                let matched = match l.kind {
                    HirLookaroundKind::PositiveLookahead | HirLookaroundKind::NegativeLookahead => {
                        let mut probe = caps.clone();
                        self.m(&l.expr, pos, &mut probe, &mut |p, _| Some(p))
                            .is_some()
                    }
                    HirLookaroundKind::PositiveLookbehind
                    | HirLookaroundKind::NegativeLookbehind => {
                        // Some start s <= pos where expr matches exactly s..pos.
                        (0..=pos).any(|s| {
                            let mut probe = caps.clone();
                            self.m(&l.expr, s, &mut probe, &mut |p, _| {
                                if p == pos {
                                    Some(p)
                                } else {
                                    None
                                }
                            })
                            .is_some()
                        })
                    }
                };
                let positive = matches!(
                    l.kind,
                    HirLookaroundKind::PositiveLookahead | HirLookaroundKind::PositiveLookbehind
                );
                if matched == positive {
                    k(pos, caps)
                } else {
                    None
                }
            }

            HirExpr::Backref(idx) => {
                let span = caps.get(*idx as usize).copied().flatten();
                match span {
                    // Unset group: a backref to it matches the empty string.
                    None => k(pos, caps),
                    Some((s, e)) => {
                        let needle = &self.input[s..e];
                        if self.input[pos..].starts_with(needle) {
                            k(pos + needle.len(), caps)
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }

    fn concat(
        &self,
        es: &[HirExpr],
        i: usize,
        pos: usize,
        caps: &mut Caps,
        k: &mut Cont<'_>,
    ) -> Option<usize> {
        if i == es.len() {
            return k(pos, caps);
        }
        self.m(&es[i], pos, caps, &mut |p, caps| {
            self.concat(es, i + 1, p, caps, k)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &self,
        e: &HirExpr,
        min: usize,
        max: usize,
        greedy: bool,
        count: usize,
        pos: usize,
        caps: &mut Caps,
        k: &mut Cont<'_>,
    ) -> Option<usize> {
        let can_more = count < max;
        let can_stop = count >= min;

        if greedy {
            // Prefer matching one more repetition, then fall back to stopping.
            if can_more {
                let saved = caps.clone();
                if let Some(r) = self.m(e, pos, caps, &mut |p, caps| {
                    // zero-width progress once min satisfied: stop to avoid looping
                    if p == pos && count >= min {
                        return None;
                    }
                    self.repeat(e, min, max, greedy, count + 1, p, caps, k)
                }) {
                    return Some(r);
                }
                *caps = saved;
            }
            if can_stop {
                return k(pos, caps);
            }
            None
        } else {
            // Prefer stopping, then fall back to matching one more.
            if can_stop {
                let saved = caps.clone();
                if let Some(r) = k(pos, caps) {
                    return Some(r);
                }
                *caps = saved;
            }
            if can_more {
                return self.m(e, pos, caps, &mut |p, caps| {
                    if p == pos && count >= min {
                        return None;
                    }
                    self.repeat(e, min, max, greedy, count + 1, p, caps, k)
                });
            }
            None
        }
    }

    fn assert(&self, a: &HirAnchor, pos: usize) -> bool {
        let n = self.input.len();
        match a {
            HirAnchor::Start => pos == 0,
            // `$`/`\Z` (`HirAnchor::End`): end of text, or just before a final
            // newline (PCRE/Python). `\z` (strict end) is compiled to `(?![\s\S])`
            // in the HIR builder, so it does not reach here.
            HirAnchor::End => pos == n || (pos + 1 == n && self.input[pos] == b'\n'),
            HirAnchor::StartLine => pos == 0 || self.input[pos - 1] == b'\n',
            HirAnchor::EndLine => pos == n || self.input[pos] == b'\n',
            HirAnchor::WordBoundary => self.is_word(pos.wrapping_sub(1)) != self.is_word(pos),
            HirAnchor::NotWordBoundary => self.is_word(pos.wrapping_sub(1)) == self.is_word(pos),
        }
    }

    fn is_word(&self, idx: usize) -> bool {
        // idx may be usize::MAX (pos==0 -> pos-1 wrapped): out of range -> not word.
        self.input
            .get(idx)
            .map(|&b| b.is_ascii_alphanumeric() || b == b'_')
            .unwrap_or(false)
    }
}

/// Finds the leftmost-first match of `expr` in `input`, returning byte offsets.
///
/// Start positions are restricted to UTF-8 codepoint boundaries (matching the
/// Unicode semantics of Python `regex`/PCRE on valid UTF-8): a zero-width or
/// optional construct must not match in the middle of a multi-byte codepoint.
pub fn find(expr: &HirExpr, num_caps: usize, input: &[u8]) -> Option<(usize, usize)> {
    find_from(expr, num_caps, input, 0)
}

/// Finds the leftmost-first match of `expr` starting at or after byte offset
/// `from`, returning byte offsets into the whole `input`.
///
/// This is [`find`] resumed at an offset — the spec for how iteration continues.
/// The whole input stays visible, so `^`, `\b`/`\B` and lookbehind are evaluated
/// against the real surrounding text and not against a slice that begins at
/// `from`.
pub fn find_from(
    expr: &HirExpr,
    num_caps: usize,
    input: &[u8],
    from: usize,
) -> Option<(usize, usize)> {
    if from > input.len() {
        return None;
    }
    let matcher = Matcher { input };
    for start in from..=input.len() {
        // Skip continuation bytes (0b10xx_xxxx) — not codepoint starts.
        if start < input.len() && (input[start] & 0xC0) == 0x80 {
            continue;
        }
        let mut caps: Caps = vec![None; num_caps + 1];
        if let Some(end) = matcher.m(expr, start, &mut caps, &mut |p, _| Some(p)) {
            return Some((start, end));
        }
    }
    None
}

/// Decodes one UTF-8 codepoint at the start of `bytes`. Returns `(codepoint, len)`.
fn decode_utf8(bytes: &[u8]) -> Option<(u32, usize)> {
    let b0 = *bytes.first()?;
    if b0 < 0x80 {
        return Some((b0 as u32, 1));
    }
    let cont = |i: usize| -> Option<u32> {
        bytes
            .get(i)
            .filter(|&&b| (0x80..0xC0).contains(&b))
            .map(|&b| (b & 0x3F) as u32)
    };
    if (0xC0..0xE0).contains(&b0) {
        let c1 = cont(1)?;
        Some((((b0 & 0x1F) as u32) << 6 | c1, 2))
    } else if (0xE0..0xF0).contains(&b0) {
        let (c1, c2) = (cont(1)?, cont(2)?);
        Some((((b0 & 0x0F) as u32) << 12 | c1 << 6 | c2, 3))
    } else if (0xF0..0xF8).contains(&b0) {
        let (c1, c2, c3) = (cont(1)?, cont(2)?, cont(3)?);
        Some((((b0 & 0x07) as u32) << 18 | c1 << 12 | c2 << 6 | c3, 4))
    } else {
        None
    }
}
