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

    Literals {
        prefixes: result.prefixes,
        suffixes: vec![],
        prefix_complete,
        starts_with_digit,
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
                if matches!(e, HirExpr::Anchor(_)) {
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
                while start_idx < exprs.len() && matches!(&exprs[start_idx], HirExpr::Anchor(_)) {
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
                if result.complete && !result.has_nullable_suffix {
                    // Try to extend prefixes with subsequent literals
                    for expr in &exprs[start_idx + 1..] {
                        // Skip anchors (zero-width, don't affect literals)
                        if matches!(expr, HirExpr::Anchor(_)) {
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

                    let last_result = self.extract(actual_last);
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

                for expr in exprs {
                    let sub_result = self.extract(expr);

                    if sub_result.prefixes.is_empty() {
                        // One branch has no prefix - can't use multi-prefix
                        // Try to find common prefix instead
                        return self.extract_common_prefix(exprs);
                    }

                    all_complete = all_complete && sub_result.complete;
                    any_nullable_suffix = any_nullable_suffix || sub_result.has_nullable_suffix;
                    all_prefixes.extend(sub_result.prefixes);

                    // Check if we've exceeded the limit
                    if all_prefixes.len() > self.max_prefixes {
                        // Too many prefixes - fall back to common prefix
                        return self.extract_common_prefix(exprs);
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
    fn extract_common_prefix(&mut self, exprs: &[HirExpr]) -> ExtractionResult {
        let mut all_prefixes: Vec<Vec<u8>> = Vec::new();

        for expr in exprs {
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

/// A literal that every match must contain, drawn from a positive lookahead.
///
/// `\w+(?=ing\b)` cannot match anywhere in a haystack with no `ing` in it, so
/// one `memmem` settles the whole search. Only a top-level concatenation is
/// walked: under an alternation a branch may not require the literal.
pub fn required_literal(hir: &Hir) -> Option<Vec<u8>> {
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
            HirExpr::Concat(exprs) => exprs.iter().find_map(walk),
            HirExpr::Capture(capture) => walk(&capture.expr),
            HirExpr::Repeat(repeat) if repeat.min >= 1 => walk(&repeat.expr),
            _ => None,
        }
    }
    walk(&hir.expr)
}

#[cfg(test)]
mod tests {

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

    #[test]
    fn test_no_required_literal_when_a_branch_may_not_need_it() {
        // An alternation can match via a branch without the lookahead, so
        // rejecting on the literal's absence would lose real matches.
        assert_eq!(required(r"\w+(?=ing\b)|zzz"), None);
        assert_eq!(required(r"(?:a(?=bcd)|q)"), None);
        // A negative lookahead requires the literal to be ABSENT.
        assert_eq!(required(r"\w+(?!ing)"), None);
        // Optional: the lookahead need not apply at all.
        assert_eq!(required(r"a(?:(?=bcd))?"), None);
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
}
