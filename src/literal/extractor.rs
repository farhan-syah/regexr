//! Literal prefix/suffix extraction.
//!
//! Extracts literal prefixes and suffixes from HIR patterns for prefiltering.
//! Supports both single-literal and multi-literal extraction for Teddy.

use crate::hir::{Hir, HirExpr, HirLookaroundKind};

/// Extracted literals from a pattern.
#[derive(Debug, Clone, Default)]
pub struct Literals {
    /// Required prefix literals.
    /// For alternations like `hello|world`, contains `[b"hello", b"world"]`.
    /// For concatenations like `hello.*`, contains `[b"hello"]`.
    pub prefixes: Vec<Vec<u8>>,
    /// Required suffix literals.
    pub suffixes: Vec<Vec<u8>>,
    /// Whether the prefix set is complete (all match positions start with one of these).
    pub prefix_complete: bool,
    /// True if the pattern starts with a digit class (0-9).
    /// Used to create StartsWithDigit prefilter when no literal prefix exists.
    pub starts_with_digit: bool,
    /// The few distinct bytes a match can start with, when there are few enough
    /// to search for directly. Set only when no literal prefix was found.
    pub leading_bytes: Vec<u8>,
}

impl Literals {
    /// Returns true if there are no literals.
    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty() && self.suffixes.is_empty()
    }

    /// Returns the single prefix if there's exactly one.
    pub fn single_prefix(&self) -> Option<&[u8]> {
        if self.prefixes.len() == 1 {
            Some(&self.prefixes[0])
        } else {
            None
        }
    }

    /// Returns true if there are multiple prefixes (suitable for Teddy).
    pub fn has_multiple_prefixes(&self) -> bool {
        self.prefixes.len() > 1
    }

    /// Returns the number of prefix alternatives.
    pub fn prefix_count(&self) -> usize {
        self.prefixes.len()
    }
}

/// Extracts literals from an HIR.
pub fn extract_literals(hir: &Hir) -> Literals {
    let mut extractor = LiteralExtractor::new();
    let result = extractor.extract(&hir.expr);

    // Patterns with backreferences, lookarounds, word boundaries or anchors
    // cannot be fully matched by literals alone - they require NFA verification.
    // Set prefix_complete = false to prevent TeddyFull from bypassing NFA.
    //
    // Anchors count here because the extractor skips them while collecting
    // prefixes: `^[ab]` yields the complete literals "a"/"b", and a full-match
    // prefilter would then report every "a"/"b" in the haystack as a match,
    // ignoring the `^` — including at every resume position of an iteration.
    // Demoting to a candidate-only prefilter keeps the literal scan (and its
    // SIMD skipping) while letting the engine enforce the anchor.
    let prefix_complete = result.complete
        && !hir.props.has_backrefs
        && !hir.props.has_lookaround
        && !hir.props.has_word_boundary
        && !hir.props.has_anchors;

    // If no prefix literals found, check if pattern starts with digit class
    let starts_with_digit = result.prefixes.is_empty() && starts_with_digit_class(&hir.expr);

    let leading_bytes = if result.prefixes.is_empty() && !starts_with_digit {
        leading_byte_set(&hir.expr).unwrap_or_default()
    } else {
        Vec::new()
    };

    Literals {
        prefixes: result.prefixes,
        suffixes: vec![],
        prefix_complete,
        starts_with_digit,
        leading_bytes,
    }
}

/// Bytes a match can begin with, when a leading character class names at most
/// [`MAX_LEADING_BYTES`] of them.
///
/// `(['\"])[^'\"]*\1` starts with one of two bytes; without this it has no
/// prefilter at all and the engine visits every position.
fn leading_byte_set(expr: &HirExpr) -> Option<Vec<u8>> {
    match expr {
        HirExpr::Class(class) if !class.negated => {
            let mut bytes = Vec::new();
            for &(lo, hi) in &class.ranges {
                if bytes.len() + (hi as usize - lo as usize + 1) > MAX_LEADING_BYTES {
                    return None;
                }
                bytes.extend(lo..=hi);
            }
            (!bytes.is_empty()).then_some(bytes)
        }
        HirExpr::Literal(bytes) => bytes.first().map(|&b| vec![b]),
        HirExpr::Concat(exprs) => exprs
            .iter()
            .find(|e| !is_zero_width(e))
            .and_then(leading_byte_set),
        HirExpr::Capture(capture) => leading_byte_set(&capture.expr),
        HirExpr::Repeat(repeat) if repeat.min >= 1 => leading_byte_set(&repeat.expr),
        _ => None,
    }
}

/// Beyond this many distinct starting bytes a direct search stops paying: the
/// `memchr` family covers up to three, and more candidates than that in ordinary
/// text is no better than scanning.
const MAX_LEADING_BYTES: usize = 3;

/// A set of byte values as a membership table: `set[byte]` is non-zero when the
/// byte belongs. One entry per value rather than one bit, so a lookup is a
/// single indexed load.
pub type ByteSet = [u8; 256];

/// Every byte a match can begin with, or `None` when that cannot be pinned down.
///
/// This is the unbounded form of `leading_byte_set`: a search that tests one
/// byte against a table does not care how many bytes are in it, so `\d+` and
/// `\w+` qualify where a `memchr`-style prefilter does not.
///
/// `None` means "no useful restriction" and every position stays viable. It is
/// the answer whenever the first consuming element is not a single byte class —
/// including a nullable one, since then the element after it decides the byte
/// too, and a set that misses a viable start would skip real matches.
pub fn first_byte_set(hir: &Hir) -> Option<ByteSet> {
    // A set every byte belongs to restricts nothing, and testing it would be
    // pure overhead on the search's hottest loop.
    expr_first_byte_set(&hir.expr).filter(|set| set.contains(&0))
}

/// [`first_byte_set`] for a subexpression.
///
/// Unlike the whole-pattern form this keeps an all-bytes answer, because a
/// caller asking about one branch of an alternation needs to know what that
/// branch can start with even when the answer is "anything".
pub fn expr_first_byte_set(expr: &HirExpr) -> Option<ByteSet> {
    let mut set = [0u8; 256];
    add_first_bytes(expr, &mut set).then_some(set)
}

/// The membership table of a single byte class.
///
/// A negated class admits every byte its ranges do not name, which is how the
/// engines read `negated` — see the class emitters in `vm::backtracking`.
pub fn byte_class_set(ranges: &[(u8, u8)], negated: bool) -> ByteSet {
    let mut set = [u8::from(negated); 256];
    for &(lo, hi) in ranges {
        for byte in lo..=hi {
            if let Some(entry) = set.get_mut(byte as usize) {
                *entry = u8::from(!negated);
            }
        }
    }
    set
}

/// Bytes that `expr` matches as a whole single-byte match, when consuming one
/// such byte can never be the wrong choice.
///
/// A greedy repetition of `expr` can then be run as a byte scan rather than an
/// iteration at a time. Two things have to hold for that to be sound, and both
/// are checked here:
///
/// - a byte in the set is matched by `expr` *entirely*, consuming exactly one
///   byte and writing no capture, so the scan cannot commit to the wrong length
///   or skip a group;
/// - no other branch of `expr` can begin with a byte in the set, so preferring
///   the single-byte reading never overrides a branch the alternation ranks
///   higher.
///
/// The set may be a strict subset of what `expr` matches: a scan that stops
/// early just leaves the rest to the general loop. `[^'"]` lowers to an ASCII
/// class beside a UTF-8 trie, and this returns the ASCII half — which is the
/// whole of it on ASCII text.
pub fn single_byte_run_set(expr: &HirExpr) -> Option<ByteSet> {
    match expr {
        HirExpr::Class(class) => Some(byte_class_set(&class.ranges, class.negated)),
        HirExpr::Alt(branches) => {
            let mut run = [0u8; 256];
            let mut others = [0u8; 256];
            let mut found = false;

            for branch in branches {
                if let HirExpr::Class(class) = branch {
                    // A negated class reaches past ASCII, where the bytes it
                    // admits are lead bytes of characters the trie beside it
                    // spells out — the two would overlap.
                    if !class.negated {
                        for (entry, member) in
                            run.iter_mut().zip(byte_class_set(&class.ranges, false))
                        {
                            *entry |= member;
                        }
                        found = true;
                        continue;
                    }
                }
                // Anything else has to be provably out of the way.
                let first = expr_first_byte_set(branch)?;
                for (entry, member) in others.iter_mut().zip(first) {
                    *entry |= member;
                }
            }

            let disjoint = run
                .iter()
                .zip(others)
                .all(|(member, other)| *member == 0 || other == 0);
            (found && disjoint).then_some(run)
        }
        _ => None,
    }
}

/// Adds every byte `expr` can begin with to `set`. Returns false when the set
/// cannot be determined, in which case `set` is meaningless.
fn add_first_bytes(expr: &HirExpr, set: &mut ByteSet) -> bool {
    match expr {
        HirExpr::Class(class) => {
            let members = byte_class_set(&class.ranges, class.negated);
            for (entry, member) in set.iter_mut().zip(members) {
                *entry |= member;
            }
            true
        }
        HirExpr::Literal(bytes) => match bytes.first() {
            Some(&byte) => {
                if let Some(entry) = set.get_mut(byte as usize) {
                    *entry = 1;
                }
                true
            }
            None => false,
        },
        HirExpr::Concat(exprs) => match exprs.iter().find(|expr| !is_zero_width(expr)) {
            Some(expr) => add_first_bytes(expr, set),
            None => false,
        },
        HirExpr::Alt(branches) => {
            !branches.is_empty() && branches.iter().all(|branch| add_first_bytes(branch, set))
        }
        HirExpr::Capture(capture) => add_first_bytes(&capture.expr, set),
        // A repeat that can match nothing leaves the next element deciding the
        // first byte, so its own set is not the whole answer.
        HirExpr::Repeat(repeat) if repeat.min >= 1 => add_first_bytes(&repeat.expr, set),
        _ => false,
    }
}

/// Checks if an HIR expression starts with a pure digit character class.
/// Returns true only if the class exclusively matches digits (0-9), not if it
/// merely includes digits among other characters (like \w which includes letters).
fn starts_with_digit_class(expr: &HirExpr) -> bool {
    match expr {
        HirExpr::Class(class) => {
            // Only return true if the class is non-negated and ALL ranges are
            // within the digit range 0-9. This ensures we don't match \w
            // (which includes [A-Za-z0-9_]) or \D (negated digit class).
            !class.negated
                && !class.ranges.is_empty()
                && class
                    .ranges
                    .iter()
                    .all(|(lo, hi)| *lo >= b'0' && *hi <= b'9')
        }
        HirExpr::Concat(exprs) => {
            // Skip anchors and find first non-anchor
            for e in exprs {
                if is_zero_width(e) {
                    continue;
                }
                return starts_with_digit_class(e);
            }
            false
        }
        // Repeat of digits still starts with digit
        HirExpr::Repeat(rep) if rep.min > 0 => starts_with_digit_class(&rep.expr),
        HirExpr::Capture(cap) => starts_with_digit_class(&cap.expr),
        _ => false,
    }
}

/// Whether an element consumes no input, so the element after it still decides
/// the first byte. Anchors and lookarounds both qualify.
fn is_zero_width(expr: &HirExpr) -> bool {
    matches!(expr, HirExpr::Anchor(_) | HirExpr::Lookaround(_))
}

/// Result of extracting prefixes from an expression.
#[derive(Debug, Clone, Default)]
struct ExtractionResult {
    /// The extracted prefixes.
    prefixes: Vec<Vec<u8>>,
    /// Whether the extraction is complete (all branches have literals).
    complete: bool,
    /// Whether the expression has a nullable suffix (e.g., ends with `?`, `*`).
    /// If true, we cannot safely extend with subsequent literals.
    has_nullable_suffix: bool,
}

struct LiteralExtractor {
    /// Maximum number of prefixes to extract (for Teddy limit).
    max_prefixes: usize,
    /// Maximum length of each prefix.
    max_prefix_len: usize,
}

impl LiteralExtractor {
    fn new() -> Self {
        Self {
            max_prefixes: 8,   // Teddy limit
            max_prefix_len: 8, // Teddy limit
        }
    }

    fn extract(&mut self, expr: &HirExpr) -> ExtractionResult {
        match expr {
            HirExpr::Literal(bytes) => {
                // Truncate to max length
                let truncated = bytes.len() > self.max_prefix_len;
                let prefix = if truncated {
                    bytes[..self.max_prefix_len].to_vec()
                } else {
                    bytes.clone()
                };
                ExtractionResult {
                    prefixes: vec![prefix],
                    // Only complete if we didn't truncate - truncated prefixes
                    // cannot provide full match bounds
                    complete: !truncated,
                    has_nullable_suffix: false,
                }
            }
            HirExpr::Concat(exprs) => {
                // Extract from the first non-anchor element, extend with subsequent literals.
                // Anchors (including word boundaries) are zero-width and should be skipped
                // during literal extraction. For example, `\bthe\b` should extract "the".
                if exprs.is_empty() {
                    return ExtractionResult::default();
                }

                // Skip leading anchors to find the first literal-producing expression
                let mut start_idx = 0;
                while start_idx < exprs.len() && is_zero_width(&exprs[start_idx]) {
                    start_idx += 1;
                }

                if start_idx >= exprs.len() {
                    // All anchors, no literals
                    return ExtractionResult::default();
                }

                let mut result = self.extract(&exprs[start_idx]);

                // Track whether we've seen all literals so far
                let mut all_literals_so_far = matches!(&exprs[start_idx], HirExpr::Literal(_));

                // Only extend prefixes with subsequent literals if the first
                // element was extracted in full and has no nullable suffix.
                //
                // `complete` is the load-bearing half: an incomplete extraction
                // means text this element can match was NOT captured in the
                // prefix, so a later literal does not follow what we have.
                // `((a)(b))c` extracts "a" from the outer group and stops at the
                // un-extracted `(b)`; splicing "c" on would claim the pattern
                // starts with "ac", which it never does, and the prefilter would
                // then find no candidates and report no match at all.
                // If the extend-loop below bails out on a non-literal element,
                // remember which node it was and what `extract` returned for
                // it. The trailing-element check further down often lands on
                // that exact same node (e.g. a `[Literal, Tail]` concat where
                // `Tail` is both "the element that stopped extension" and
                // "the last element") and would otherwise walk it a second
                // time, which is what makes deeply nested concats like
                // `a(?:a(?:a...)))` exponential.
                //
                // The pointer is only ever compared for identity (`ptr::eq`)
                // below, never dereferenced, and it does not outlive this
                // match arm's borrow of `exprs` - both `expr` and
                // `actual_last` are references into the same `exprs` slice
                // for the lifetime of this call.
                let mut break_node: Option<(*const HirExpr, ExtractionResult)> = None;

                if result.complete && !result.has_nullable_suffix {
                    // Try to extend prefixes with subsequent literals
                    for expr in &exprs[start_idx + 1..] {
                        // Zero-width elements don't affect literals
                        if is_zero_width(expr) {
                            continue;
                        }
                        if let HirExpr::Literal(bytes) = expr {
                            // Extend each prefix (up to max length)
                            for prefix in &mut result.prefixes {
                                let remaining = self.max_prefix_len.saturating_sub(prefix.len());
                                if remaining > 0 {
                                    let extend_len = bytes.len().min(remaining);
                                    prefix.extend_from_slice(&bytes[..extend_len]);
                                    // If we couldn't fit all bytes, mark incomplete
                                    if extend_len < bytes.len() {
                                        result.complete = false;
                                    }
                                } else {
                                    // No room to extend - subsequent literal was skipped
                                    result.complete = false;
                                }
                            }
                        } else {
                            // Stop extending if we hit a non-literal.
                            // The prefix is no longer complete since there's
                            // a non-literal suffix that must also match.
                            all_literals_so_far = false;
                            result.complete = false;
                            // Check if this expression has a nullable suffix
                            let sub = self.extract(expr);
                            if sub.has_nullable_suffix {
                                result.has_nullable_suffix = true;
                            }
                            break_node = Some((expr as *const HirExpr, sub));
                            break;
                        }
                    }
                } else {
                    // If first element has nullable suffix, check if there are
                    // subsequent elements - if so, prefix isn't complete
                    if start_idx + 1 < exprs.len() {
                        result.complete = false;
                    }
                }

                // Check if the concat ends with a nullable expression
                // Also, if the last element is not a literal or anchor, the prefix isn't complete
                // (Anchors are zero-width and don't affect completeness)
                if let Some(last) = exprs.last() {
                    // Skip trailing anchors to find the actual last element
                    let actual_last = exprs
                        .iter()
                        .rev()
                        .find(|e| !matches!(e, HirExpr::Anchor(_)))
                        .unwrap_or(last);

                    let actual_last_ptr: *const HirExpr = actual_last;
                    let last_result = match break_node {
                        // Reusing `sub_result` is sound because `extract` is
                        // a pure function of `expr` (see module-level notes
                        // in the PR/report): `self` carries only the two
                        // `usize` limits set once in `new()`, and `extract`
                        // never mutates the tree or clones nodes, so calling
                        // it twice on the same `&HirExpr` reference is
                        // guaranteed to reproduce the exact same result.
                        // Matched by value (not `&break_node`) since
                        // `break_node` is not read again after this, so the
                        // already-computed result can be moved out instead
                        // of cloned.
                        Some((ptr, sub_result)) if std::ptr::eq(ptr, actual_last_ptr) => sub_result,
                        _ => self.extract(actual_last),
                    };
                    if last_result.has_nullable_suffix {
                        result.has_nullable_suffix = true;
                    }
                    // If the last expression is not a complete literal, mark as incomplete
                    if !last_result.complete || !matches!(actual_last, HirExpr::Literal(_)) {
                        // Only if we haven't already extended through all literals
                        if !all_literals_so_far {
                            result.complete = false;
                        }
                    }
                }

                result
            }
            HirExpr::Alt(exprs) => {
                // Collect prefixes from all branches
                let mut all_prefixes: Vec<Vec<u8>> = Vec::new();
                let mut all_complete = true;
                let mut any_nullable_suffix = false;

                // Branches already extracted by this loop, in order. Passed
                // to `extract_common_prefix` on either bail-out below so it
                // never re-walks a branch this loop already walked - only
                // the not-yet-extracted branches are new work there.
                let mut done: Vec<ExtractionResult> = Vec::with_capacity(exprs.len());

                for expr in exprs {
                    let sub_result = self.extract(expr);

                    if sub_result.prefixes.is_empty() {
                        // One branch has no prefix - can't use multi-prefix
                        // Try to find common prefix instead
                        done.push(sub_result);
                        return self.extract_common_prefix(exprs, done);
                    }

                    all_complete = all_complete && sub_result.complete;
                    any_nullable_suffix = any_nullable_suffix || sub_result.has_nullable_suffix;
                    all_prefixes.extend(sub_result.prefixes.clone());
                    done.push(sub_result);

                    // Check if we've exceeded the limit
                    if all_prefixes.len() > self.max_prefixes {
                        // Too many prefixes - fall back to common prefix
                        return self.extract_common_prefix(exprs, done);
                    }
                }

                // Deduplicate prefixes
                all_prefixes.sort();
                all_prefixes.dedup();

                ExtractionResult {
                    prefixes: all_prefixes,
                    complete: all_complete,
                    has_nullable_suffix: any_nullable_suffix,
                }
            }
            HirExpr::Repeat(rep) => {
                if rep.min > 0 {
                    // Required repetition - extract inner prefix
                    // But the expression has a nullable suffix since repetition
                    // can match more or less than what's required
                    let mut result = self.extract(&rep.expr);
                    // Even with min > 0, repetition can match variable amounts,
                    // which means it has a "nullable suffix" in the sense that
                    // subsequent literals might not directly follow the required part.
                    // For example, a+b can match "ab" or "aab", so we shouldn't
                    // extend the prefix "a" with "b".
                    result.has_nullable_suffix = true;
                    // The literals are a prefix, never the whole match: one more
                    // iteration can always follow. Leaving `complete` set would
                    // license a full-match prefilter to report just the literal
                    // and skip the engine, so `(a|b)+` on "abab" would answer
                    // "a" rather than "abab".
                    result.complete = false;
                    result
                } else {
                    // Zero-or-more means no required prefix
                    ExtractionResult {
                        has_nullable_suffix: true,
                        ..Default::default()
                    }
                }
            }
            HirExpr::Capture(cap) => self.extract(&cap.expr),
            HirExpr::Class(_) => {
                // Can't extract literals from character classes
                ExtractionResult::default()
            }
            _ => ExtractionResult::default(),
        }
    }

    /// Extracts the common prefix from alternation branches.
    ///
    /// `done` holds the `ExtractionResult`s the `Alt` arm's own loop already
    /// computed, in branch order, for `exprs[..done.len()]` - including the
    /// branch that triggered the bail-out into this function. Only the
    /// remaining `exprs[done.len()..]` are walked here, so no branch is ever
    /// extracted twice (walking every branch from scratch here as well as in
    /// the caller was the second exponential-blowup site).
    fn extract_common_prefix(
        &mut self,
        exprs: &[HirExpr],
        done: Vec<ExtractionResult>,
    ) -> ExtractionResult {
        let mut all_prefixes: Vec<Vec<u8>> = Vec::new();
        let done_len = done.len();

        // `done` is owned and not read again after this loop, so its
        // prefixes can be moved into `all_prefixes` instead of cloned.
        for sub_result in done {
            if sub_result.prefixes.is_empty() {
                return ExtractionResult::default();
            }
            all_prefixes.extend(sub_result.prefixes);
        }

        for expr in &exprs[done_len..] {
            let sub_result = self.extract(expr);
            if sub_result.prefixes.is_empty() {
                return ExtractionResult::default();
            }
            all_prefixes.extend(sub_result.prefixes);
        }

        if let Some(common) = find_common_prefix(&all_prefixes) {
            if !common.is_empty() {
                return ExtractionResult {
                    prefixes: vec![common],
                    complete: false, // Common prefix isn't complete
                    has_nullable_suffix: false,
                };
            }
        }

        ExtractionResult::default()
    }
}

/// Finds the common prefix among a set of byte sequences.
fn find_common_prefix(seqs: &[Vec<u8>]) -> Option<Vec<u8>> {
    if seqs.is_empty() {
        return None;
    }

    let first = &seqs[0];
    let mut prefix_len = first.len();

    for seq in &seqs[1..] {
        let common_len = first
            .iter()
            .zip(seq.iter())
            .take_while(|(a, b)| a == b)
            .count();
        prefix_len = prefix_len.min(common_len);
    }

    if prefix_len == 0 {
        None
    } else {
        Some(first[..prefix_len].to_vec())
    }
}

/// A literal that every match must contain.
///
/// `\w+(?=ing\b)` cannot match anywhere in a haystack with no `ing` in it, and
/// neither can `(\w+)@\1` where there is no `@`, so one `memmem` settles the
/// whole search. Neither pattern has a literal *prefix*, so a prefilter — which
/// answers "where could a match start" — has nothing to offer either one.
///
/// Only a top-level concatenation is walked: under an alternation a branch may
/// not require the literal. A repetition qualifies only when it must run at
/// least once, and case-insensitivity is not a concern because the HIR builder
/// turns a folded literal into a class, never a [`HirExpr::Literal`].
///
/// This is a rejection filter and nothing more. It can only turn "no match" into
/// "no match" sooner; it never decides which match is reported.
pub fn required_literal(hir: &Hir) -> Option<Vec<u8>> {
    /// Replaces `best` with `candidate` when the candidate is more selective.
    fn keep_longer(best: &mut Option<Vec<u8>>, candidate: Vec<u8>) {
        if candidate.len() > best.as_ref().map_or(0, Vec::len) {
            *best = Some(candidate);
        }
    }

    fn walk(expr: &HirExpr) -> Option<Vec<u8>> {
        match expr {
            HirExpr::Lookaround(look) => match look.kind {
                HirLookaroundKind::PositiveLookahead | HirLookaroundKind::PositiveLookbehind => {
                    let mut extractor = LiteralExtractor::new();
                    let inner = extractor.extract(&look.expr);
                    inner.prefixes.first().filter(|l| l.len() >= 2).cloned()
                }
                _ => None,
            },
            HirExpr::Literal(bytes) => (!bytes.is_empty()).then(|| bytes.clone()),
            // Every element of a concatenation has to match, so any of them may
            // be chosen and the longest is the most selective. Adjacent literals
            // are joined first: the builder splits `-->` into three of them, and
            // searching for the whole run rejects far more than searching for
            // `>` alone.
            HirExpr::Concat(exprs) => {
                let mut best: Option<Vec<u8>> = None;
                let mut run: Vec<u8> = Vec::new();
                for expr in exprs {
                    if let HirExpr::Literal(bytes) = expr {
                        run.extend_from_slice(bytes);
                        continue;
                    }
                    keep_longer(&mut best, std::mem::take(&mut run));
                    if let Some(found) = walk(expr) {
                        keep_longer(&mut best, found);
                    }
                }
                keep_longer(&mut best, run);
                best
            }
            HirExpr::Capture(capture) => walk(&capture.expr),
            HirExpr::Repeat(repeat) if repeat.min >= 1 => walk(&repeat.expr),
            _ => None,
        }
    }
    walk(&hir.expr)
}

#[cfg(test)]
mod tests {

    /// A leading class of a few bytes must produce a searchable byte set.
    ///
    /// Without it the pattern has no prefilter at all and the engine visits
    /// every position — invisible to any correctness test.
    #[test]
    fn test_small_leading_class_yields_a_byte_set() {
        assert_eq!(literals(r#"(['"])[^'"]*\1"#).leading_bytes, b"\"'".to_vec());
        assert_eq!(literals(r"[abc]xyz").leading_bytes, b"abc".to_vec());
        assert_eq!(literals(r"(?:[ab])+z").leading_bytes, b"ab".to_vec());

        // Too many members to be worth searching for directly.
        assert!(literals(r"[a-z]xyz").leading_bytes.is_empty());
        // A negated class says which bytes CANNOT start a match.
        assert!(literals(r"[^ab]xyz").leading_bytes.is_empty());
        // A literal prefix is a stronger filter and wins.
        assert!(literals(r"abc[de]").leading_bytes.is_empty());
    }

    fn literals(pattern: &str) -> Literals {
        let ast = crate::parser::parse(pattern).unwrap();
        let hir = crate::hir::translate(&ast).unwrap();
        extract_literals(&hir)
    }

    /// A leading lookaround consumes nothing, so the element after it still
    /// decides the first byte. Losing that costs the prefilter and nothing else,
    /// so no match-level test can see it — `(?<!\$)\d+` silently went from a
    /// memchr scan to visiting every position.
    #[test]
    fn test_leading_lookaround_does_not_hide_the_prefilter() {
        assert!(literals(r"(?<!\$)\d+").starts_with_digit);
        assert!(literals(r"(?<=\$)\d+").starts_with_digit);
        assert!(literals(r"(?!x)\d+").starts_with_digit);
        assert_eq!(literals(r"(?<!a)bcd").prefixes, vec![b"bcd".to_vec()]);
        assert_eq!(literals(r"(?=x)abc").prefixes, vec![b"abc".to_vec()]);

        // Still candidate-only: a lookaround means the literal alone cannot
        // decide a match.
        assert!(!literals(r"(?<!a)bcd").prefix_complete);
    }

    fn required(pattern: &str) -> Option<Vec<u8>> {
        let ast = crate::parser::parse(pattern).unwrap();
        let hir = crate::hir::translate(&ast).unwrap();
        required_literal(&hir)
    }

    #[test]
    fn test_required_literal_from_positive_lookahead() {
        assert_eq!(required(r"\w+(?=ing\b)"), Some(b"ing".to_vec()));
        assert_eq!(required(r"(\w+)(?=ing\b)"), Some(b"ing".to_vec()));
        assert_eq!(required(r"a(?=bcd)"), Some(b"bcd".to_vec()));
    }

    /// A literal the pattern must consume is a rejection filter too, and one no
    /// prefilter can supply: `(\w+)@\1` has no literal *prefix*, so without this
    /// a haystack with no `@` in it is searched position by position.
    #[test]
    fn test_required_literal_from_the_pattern_itself() {
        assert_eq!(required(r"(\w+)@\1"), Some(b"@".to_vec()));
        assert_eq!(required(r"\d+-->\d+"), Some(b"-->".to_vec()));
        // The most selective of several is the one worth searching for.
        assert_eq!(required(r"a\d+bcde\d+"), Some(b"bcde".to_vec()));
        // A repetition that must run at least once still requires its literal.
        assert_eq!(required(r"(?:xy)+\d"), Some(b"xy".to_vec()));
    }

    #[test]
    fn test_no_required_literal_when_a_branch_may_not_need_it() {
        // An alternation can match via a branch without the lookahead, so
        // rejecting on the literal's absence would lose real matches.
        assert_eq!(required(r"\w+(?=ing\b)|zzz"), None);
        assert_eq!(required(r"(?:a(?=bcd)|q)"), None);
        assert_eq!(required(r"abc|def"), None);
        // A negative lookahead requires the literal to be ABSENT.
        assert_eq!(required(r"\w+(?!ing)"), None);
        // Optional: neither the lookahead nor the literal need apply at all.
        assert_eq!(required(r"\w(?:(?=bcd))?"), None);
        assert_eq!(required(r"\d+(?:xy)*"), None);
        assert_eq!(required(r"\w+"), None);
    }
    use super::*;
    use crate::hir::translate;
    use crate::parser::parse;

    fn get_literals(pattern: &str) -> Literals {
        let ast = parse(pattern).unwrap();
        let hir = translate(&ast).unwrap();
        extract_literals(&hir)
    }

    #[test]
    fn test_simple_literal() {
        let lits = get_literals("hello");
        assert_eq!(lits.prefixes.len(), 1);
        assert_eq!(lits.prefixes[0], b"hello");
        assert!(lits.prefix_complete);
    }

    #[test]
    fn test_long_literal_truncated() {
        let lits = get_literals("helloworld123");
        assert_eq!(lits.prefixes.len(), 1);
        assert_eq!(lits.prefixes[0], b"hellowor"); // Truncated to 8 bytes
                                                   // Truncated literals cannot be "complete" - they're only prefixes
        assert!(!lits.prefix_complete);
    }

    #[test]
    fn test_no_prefix() {
        let lits = get_literals(".*hello");
        assert!(lits.prefixes.is_empty());
    }

    #[test]
    fn test_alternation_multi_prefix() {
        // Different prefixes - should extract both for Teddy
        let lits = get_literals("hello|world");
        assert_eq!(lits.prefixes.len(), 2);
        assert!(lits.prefixes.contains(&b"hello".to_vec()));
        assert!(lits.prefixes.contains(&b"world".to_vec()));
    }

    #[test]
    fn test_alternation_common_prefix() {
        // Same prefix - should extract common prefix
        let lits = get_literals("hello|help");
        // Both start with "hel", so we get both as separate prefixes
        assert_eq!(lits.prefixes.len(), 2);
        assert!(lits.prefixes.contains(&b"hello".to_vec()));
        assert!(lits.prefixes.contains(&b"help".to_vec()));
    }

    #[test]
    fn test_concat_extends_prefix() {
        let lits = get_literals("ab");
        assert_eq!(lits.prefixes.len(), 1);
        assert_eq!(lits.prefixes[0], b"ab");
    }

    #[test]
    fn test_class_no_prefix() {
        let lits = get_literals("[abc]hello");
        assert!(lits.prefixes.is_empty());
    }

    #[test]
    fn test_repeat_one_or_more() {
        let lits = get_literals("a+b");
        assert_eq!(lits.prefixes.len(), 1);
        // a+ means at least one 'a', but we cannot extend with 'b' because
        // the match could be "aab" or "aaab" - the prefix is just "a"
        assert_eq!(lits.prefixes[0], b"a");
    }

    #[test]
    fn test_repeat_zero_or_more_no_prefix() {
        let lits = get_literals("a*b");
        assert!(lits.prefixes.is_empty());
    }

    #[test]
    fn test_too_many_alternations() {
        // More than 8 alternations - falls back to common prefix (none in this case)
        let lits = get_literals("a|b|c|d|e|f|g|h|i|j");
        // Since there's no common prefix among a,b,c..., should be empty
        assert!(lits.prefixes.is_empty());
    }

    #[test]
    fn test_nested_alternation() {
        let lits = get_literals("(cat|dog)food");
        assert_eq!(lits.prefixes.len(), 2);
        assert!(lits.prefixes.contains(&b"catfood".to_vec()));
        assert!(lits.prefixes.contains(&b"dogfood".to_vec()));
    }

    #[test]
    fn test_literal_then_star() {
        // hello.*world should extract "hello" as prefix
        let lits = get_literals("hello.*world");
        assert_eq!(lits.prefixes.len(), 1);
        assert_eq!(lits.prefixes[0], b"hello");
    }

    #[test]
    fn test_literal_then_class() {
        // hello[0-9]+ should extract "hello" as prefix
        let lits = get_literals("hello[0-9]+");
        assert_eq!(lits.prefixes.len(), 1);
        assert_eq!(lits.prefixes[0], b"hello");
    }

    /// The exact `Concat` doubling shape that used to make `extract` cost
    /// `T(depth) = 2*T(depth-1)`: every level is `[Literal, Tail]`, and
    /// `Tail` is both the element that stops the extend-loop and the
    /// trailing-element check's `actual_last`.
    ///
    /// Hand-traced against the algorithm: `a` chars never merge into one
    /// `Literal` node (each is its own single-char `HirExpr::Literal`, see
    /// the HIR builder's 1:1 `Concat` translation), so nesting one level
    /// gives `Concat[Literal(a), Concat[Literal(a), Concat[Literal(a),
    /// Literal(b)]]]`. The innermost `Concat[Literal(a), Literal(b)]`
    /// extracts and extends to `"ab"`, complete. One level up, the
    /// extend-loop's first (and only) non-anchor sibling is that whole
    /// inner `Concat` — not a `Literal` — so it stops extension immediately
    /// without folding "ab" in; the prefix stays `"a"` and `complete`
    /// becomes `false` because the trailing element is neither complete nor
    /// a literal. That same one-`Literal`-then-stop shape repeats at every
    /// level, so the outermost prefix is `"a"`, incomplete, all the way out.
    #[test]
    fn test_concat_doubling_shape_extracts_leading_literal_only() {
        let lits = get_literals(r"a(?:a(?:ab))");
        assert_eq!(lits.prefixes, vec![b"a".to_vec()]);
        assert!(!lits.prefix_complete);
    }

    /// The exact `Alt` doubling shape that used to make `extract_common_prefix`
    /// re-walk branches its caller had already extracted: a `\d` branch (no
    /// literal prefix) forces the bail-out at every nesting level, and the
    /// other branch is itself a nested alternation with the same shape.
    ///
    /// Hand-traced: `(a|\d)` is `Alt[Literal(a), Class(0x30..=0x39)]` (`a` is
    /// a literal, `\d` in ASCII mode is a digit class — not a literal), and
    /// a non-capturing group adds no wrapper node, but `(a|(a|\d))` uses
    /// *capturing* groups (no `?:`), so it is `Capture(Alt[Literal(a),
    /// Capture(Alt[Literal(a), Class(digit)])])`. `Capture` is transparent
    /// to `extract`. At the inner `Alt`, the `\d` branch's `Class` extracts
    /// to an empty-prefix `ExtractionResult`, which immediately bails the
    /// whole inner `Alt` to `ExtractionResult::default()` (empty prefixes) —
    /// `find_common_prefix` never runs because the fallback path returns
    /// `default()` as soon as any one branch has no prefix at all, before
    /// trying to find a common one. That empty result then makes the outer
    /// `Alt`'s second branch empty too, which bails the outer `Alt` to
    /// `default()` the same way. So the whole pattern extracts no prefix at
    /// any level.
    #[test]
    fn test_alt_doubling_shape_has_no_prefix() {
        let lits = get_literals(r"(a|(a|\d))");
        assert!(lits.prefixes.is_empty());
        assert!(!lits.prefix_complete);
    }

    /// The Concat variant that must NOT double, and must keep behaving
    /// exactly as before the fix: siblings follow the non-literal element
    /// that stops the extend-loop, so the loop's break node (`[0-9]`) and
    /// the trailing-element check's `actual_last` (`c`) are different
    /// nodes — `ptr::eq` returns false and `actual_last` is extracted fresh,
    /// same as pre-fix.
    ///
    /// Hand-traced: `a[0-9]bc` is `Concat[Literal(a), Class(0-9), Literal(b),
    /// Literal(c)]`. The extend-loop extracts `"a"`, then meets `[0-9]` and
    /// stops (not a literal), leaving the prefix at `"a"` with `complete`
    /// already `false`; `Literal(b)` and `Literal(c)` are never visited by
    /// that loop, matching the pre-fix `break` semantics. The
    /// trailing-element check walks back from the end past no anchors,
    /// lands on `Literal(c)` — a different node from the break's `[0-9]` —
    /// extracts it fresh (`"c"`, complete), and since it's a complete
    /// literal the completeness check does not re-flip `complete`, which
    /// was already `false`.
    #[test]
    fn test_concat_break_then_trailing_literal_does_not_double() {
        let lits = get_literals(r"a[0-9]bc");
        assert_eq!(lits.prefixes, vec![b"a".to_vec()]);
        assert!(!lits.prefix_complete);
    }
}
