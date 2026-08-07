//! regexr - A high-performance regex engine built from scratch
//!
//! This crate provides a regex engine with multiple execution backends:
//! - PikeVM: Thread-based NFA simulation (supports backreferences, lookaround)
//! - Shift-Or: Bit-parallel NFA for patterns with ≤64 states
//! - Lazy DFA: On-demand determinization with caching
//! - JIT: Native x86-64 code generation (optional, requires `jit` feature)
//! - SIMD: AVX2-accelerated literal search (optional, requires `simd` feature)
//!
//! The [`mod@reference`] module is an executable specification: a simple,
//! obviously-correct backtracking matcher defining regexr's canonical match
//! semantics, used as the ground-truth oracle in conformance/differential tests.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

pub mod dfa;
pub mod engine;
pub mod error;
pub mod hir;
pub mod literal;
pub mod nfa;
pub mod parser;
pub mod reference;
pub mod vm;

#[cfg(feature = "jit")]
pub mod jit;

#[cfg(feature = "simd")]
pub mod simd;

pub use error::{Error, Result};

use engine::CompiledRegex;
use std::collections::HashMap;
use std::sync::Arc;

/// Configuration options for regex compilation.
#[derive(Debug, Clone, Default)]
pub struct RegexBuilder {
    pattern: String,
    /// Whether to enable JIT compilation.
    jit: bool,
    /// Whether to enable prefix optimization for large alternations.
    /// This is critical for tokenizer-style patterns with many literal alternatives.
    optimize_prefixes: bool,
    /// Steps a backreference search may take; see [`RegexBuilder::backtrack_limit`].
    backtrack_limit: u64,
    /// Elements the pattern may expand to; see [`RegexBuilder::size_limit`].
    size_limit: u32,
}

impl RegexBuilder {
    /// Creates a new RegexBuilder with the given pattern.
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
            jit: false,
            optimize_prefixes: false,
            backtrack_limit: vm::backtracking::DEFAULT_BACKTRACK_LIMIT,
            size_limit: hir::builder::DEFAULT_EXPANDED_SIZE,
        }
    }

    /// Sets how large a pattern may expand to before it is refused.
    ///
    /// Every engine here compiles `{n,m}` by emitting the subexpression `m`
    /// times, so what a pattern costs to build is its *expanded* size, not its
    /// text length: `\w{200000,}` is eleven characters and minutes of work.
    /// Anything compiling a pattern it did not write needs that bounded, so the
    /// default refuses a pattern past [`hir::builder::DEFAULT_EXPANDED_SIZE`]
    /// elements — roughly a tenth of a second of compilation.
    ///
    /// Raise it when you own the pattern and the cost is acceptable. The error
    /// reports the size the pattern reached, so it also tells you what to raise
    /// it to.
    ///
    /// The count is taken *after* classes are lowered, which is worth knowing
    /// for a pattern that looks small: a large Unicode property compiles to a
    /// single node, but a small multi-byte class becomes a UTF-8 trie of up to
    /// 64 branches, and a bounded repetition multiplies that.
    ///
    /// # Example
    ///
    /// ```
    /// use regexr::RegexBuilder;
    ///
    /// assert!(RegexBuilder::new(r"a{50000}").build().is_err());
    /// assert!(RegexBuilder::new(r"a{50000}")
    ///     .size_limit(100_000)
    ///     .build()
    ///     .is_ok());
    /// ```
    pub fn size_limit(mut self, limit: u32) -> Self {
        self.size_limit = limit;
        self
    }

    /// Sets how many steps a backreference search may take before giving up.
    ///
    /// Every engine in this crate is linear in the input except the backtracking
    /// one, and only patterns with backreferences reach it. Matching a
    /// backreference is NP-hard in general — there is no polynomial bound to
    /// fall back on — so a step budget is what guarantees the search ends.
    ///
    /// The default is high enough that ordinary patterns never approach it. A
    /// search that does exhaust it makes [`Regex::try_find`],
    /// [`Regex::try_captures`] and [`Regex::try_is_match`] return
    /// [`error::ErrorKind::MatchLimitExceeded`]; the infallible [`Regex::find`],
    /// [`Regex::captures`] and [`Regex::is_match`] report it as "no match",
    /// which is why the fallible forms exist.
    pub fn backtrack_limit(mut self, limit: u64) -> Self {
        self.backtrack_limit = limit;
        self
    }

    /// Enables or disables JIT compilation.
    ///
    /// When enabled, the regex will be compiled to native machine code
    /// for maximum performance. This is ideal for patterns that will be
    /// matched many times (e.g., tokenization).
    ///
    /// JIT compilation has higher upfront cost but faster matching.
    /// Only available on x86-64 with the `jit` feature enabled.
    ///
    /// # Example
    ///
    /// ```
    /// use regexr::RegexBuilder;
    ///
    /// let re = RegexBuilder::new(r"\w+")
    ///     .jit(true)
    ///     .build()
    ///     .unwrap();
    /// assert!(re.is_match("hello"));
    /// ```
    pub fn jit(mut self, enabled: bool) -> Self {
        self.jit = enabled;
        self
    }

    /// Enables or disables prefix optimization for large alternations.
    ///
    /// When enabled, large alternations of literals (like `(token1|token2|...|tokenN)`)
    /// will be optimized by merging common prefixes into a trie structure.
    /// This reduces the number of active NFA threads from O(vocabulary_size) to O(token_length).
    ///
    /// This is critical for tokenizer-style patterns with many literal alternatives.
    ///
    /// # Example
    ///
    /// ```
    /// use regexr::RegexBuilder;
    ///
    /// // Pattern with many tokens sharing common prefixes
    /// let re = RegexBuilder::new(r"(the|that|them|they|this)")
    ///     .optimize_prefixes(true)
    ///     .build()
    ///     .unwrap();
    /// assert!(re.is_match("the"));
    /// ```
    pub fn optimize_prefixes(mut self, enabled: bool) -> Self {
        self.optimize_prefixes = enabled;
        self
    }

    /// Builds the regex with the configured options.
    pub fn build(self) -> Result<Regex> {
        let ast = parser::parse(&self.pattern)?;
        let mut hir_result = hir::translate_with_limit(&ast, self.size_limit)?;

        // Apply prefix optimization if enabled
        if self.optimize_prefixes {
            hir_result = hir::optimize_prefixes(hir_result);
        }

        let named_groups = Arc::new(hir_result.props.named_groups.clone());

        let inner = if self.jit {
            engine::compile_with_jit(&hir_result)?
        } else {
            // Use compile_from_hir for optimal engine selection (ShiftOr, LazyDfa, etc.)
            engine::compile_from_hir(&hir_result)?
        };

        Ok(Regex {
            inner,
            required_literal: required_literal_finder(&hir_result),
            pattern: self.pattern,
            named_groups,
            backtrack_limit: self.backtrack_limit,
        })
    }
}

fn required_literal_finder(hir: &hir::Hir) -> Option<memchr::memmem::Finder<'static>> {
    literal::required_literal(hir).map(|l| memchr::memmem::Finder::new(&l).into_owned())
}

/// Escapes all regex metacharacters in `text` so that the returned pattern
/// matches `text` literally.
///
/// This is regexr's counterpart to `regex::escape` from the `regex` crate:
/// pass the result to [`Regex::new`] or [`RegexBuilder::new`] when you have a
/// plain string (e.g. a user-supplied delimiter) that must be matched
/// character-for-character rather than interpreted as a pattern.
///
/// # Which characters are escaped
///
/// Escaping covers the characters regexr's parser treats specially at the top
/// level (outside a character class) - `\ . * + ? | ^ $ ( ) [ ] { }` - plus
/// `#` and ASCII whitespace, which extended (`x`) mode strips. Every
/// other character - including `-`, `:`, `<`, `>`, `=`, `!`, `,`, `&`, `~`,
/// digits, and non-ASCII text - already parses as a literal on its own and is
/// passed through unchanged.
///
/// This differs from `regex::escape` only in leaving `&`, `~` and `-` alone:
/// those matter inside a character class in engines with class-set operators,
/// and regexr has none. The result of `escape` is therefore safe as a
/// standalone pattern, concatenated with other *escaped* text, or spliced into
/// an extended-mode pattern - but not when spliced directly inside a
/// hand-written character class.
///
/// # Example
///
/// ```
/// use regexr::{escape, Regex};
///
/// let delimiter = "a.b|c";
/// let pattern = escape(delimiter);
/// let re = Regex::new(&pattern).unwrap();
/// assert!(re.is_match(delimiter));
/// assert!(!re.is_match("axb c"));
/// ```
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            // Line-oriented whitespace gets its symbolic escape so the output
            // stays readable when logged or embedded in source.
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            '\\' | '.' | '*' | '+' | '?' | '|' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}'
            // `#` starts a comment and whitespace separates nothing under
            // extended mode, so both must survive being spliced into `(?x)`.
            | '#' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii_whitespace() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// A compiled regular expression.
#[derive(Debug)]
pub struct Regex {
    inner: CompiledRegex,
    /// A literal every match must contain; absent from the haystack means no
    /// match exists. Rejection only — never decides which match is reported.
    required_literal: Option<memchr::memmem::Finder<'static>>,
    pattern: String,
    /// Named capture groups: maps name to index.
    named_groups: Arc<HashMap<String, u32>>,
    /// Steps a backreference search may take; see [`RegexBuilder::backtrack_limit`].
    backtrack_limit: u64,
}

impl Regex {
    /// Compiles a regular expression pattern.
    ///
    /// # Errors
    /// Returns an error if the pattern is invalid.
    pub fn new(pattern: &str) -> Result<Regex> {
        let ast = parser::parse(pattern)?;
        let hir = hir::translate(&ast)?;
        let named_groups = Arc::new(hir.props.named_groups.clone());
        // Use HIR-based compilation to enable Shift-Or and prefilters
        let inner = engine::compile_from_hir(&hir)?;

        Ok(Regex {
            inner,
            required_literal: required_literal_finder(&hir),
            pattern: pattern.to_string(),
            named_groups,
            backtrack_limit: vm::backtracking::DEFAULT_BACKTRACK_LIMIT,
        })
    }

    /// Returns the names of all named capture groups.
    pub fn capture_names(&self) -> impl Iterator<Item = &str> {
        self.named_groups.keys().map(|s| s.as_str())
    }

    /// Returns the original pattern string.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Returns true if the regex matches anywhere in the text.
    pub fn is_match(&self, text: &str) -> bool {
        self.can_match(text) && self.inner.is_match(text.as_bytes())
    }

    /// Returns the first match in the text.
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        if !self.can_match(text) {
            return None;
        }
        self.inner
            .find(text.as_bytes())
            .map(|(start, end)| Match { text, start, end })
    }

    /// Whether a required literal (if any) is present at all.
    pub(crate) fn can_match(&self, text: &str) -> bool {
        match self.required_literal {
            Some(ref finder) => finder.find(text.as_bytes()).is_some(),
            None => true,
        }
    }

    /// Returns an iterator over all non-overlapping matches.
    ///
    /// `.`, character classes, and the Perl shorthand classes (including the
    /// ASCII-mode negated forms `\W`, `\D`) all match whole codepoints, so
    /// every span covers complete characters — no match can start or end
    /// inside one.
    pub fn find_iter<'a>(&'a self, text: &'a str) -> Matches<'a> {
        Matches::new(self, text)
    }

    /// Returns the capture groups for the first match.
    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        if !self.can_match(text) {
            return None;
        }
        self.inner.captures(text.as_bytes()).map(|slots| Captures {
            text,
            slots,
            named_groups: Arc::clone(&self.named_groups),
        })
    }

    /// [`Self::is_match`], reporting an exhausted backtracking budget instead of
    /// answering "no match".
    ///
    /// # Errors
    /// [`error::ErrorKind::MatchLimitExceeded`] if a backreference search ran
    /// past [`RegexBuilder::backtrack_limit`]. No other pattern can fail here.
    pub fn try_is_match(&self, text: &str) -> Result<bool> {
        Ok(self.try_find(text)?.is_some())
    }

    /// [`Self::find`], reporting an exhausted backtracking budget instead of
    /// answering "no match".
    ///
    /// # Errors
    /// [`error::ErrorKind::MatchLimitExceeded`] if a backreference search ran
    /// past [`RegexBuilder::backtrack_limit`]. No other pattern can fail here.
    pub fn try_find<'t>(&self, text: &'t str) -> Result<Option<Match<'t>>> {
        if !self.can_match(text) {
            return Ok(None);
        }
        self.inner
            .try_find_from(text.as_bytes(), 0, self.backtrack_limit)
            .map(|found| found.map(|(start, end)| Match { text, start, end }))
            .map_err(|_| self.match_limit_error())
    }

    /// [`Self::captures`], reporting an exhausted backtracking budget instead of
    /// answering "no match".
    ///
    /// # Errors
    /// [`error::ErrorKind::MatchLimitExceeded`] if a backreference search ran
    /// past [`RegexBuilder::backtrack_limit`]. No other pattern can fail here.
    pub fn try_captures<'t>(&self, text: &'t str) -> Result<Option<Captures<'t>>> {
        if !self.can_match(text) {
            return Ok(None);
        }
        self.inner
            .try_captures_from(text.as_bytes(), 0, self.backtrack_limit)
            .map(|found| {
                found.map(|slots| Captures {
                    text,
                    slots,
                    named_groups: Arc::clone(&self.named_groups),
                })
            })
            .map_err(|_| self.match_limit_error())
    }

    fn match_limit_error(&self) -> Error {
        Error::new(error::ErrorKind::MatchLimitExceeded, &self.pattern)
    }

    /// Returns an iterator over all non-overlapping captures.
    pub fn captures_iter<'r, 't>(&'r self, text: &'t str) -> CapturesIter<'r, 't> {
        CapturesIter {
            regex: self,
            text,
            // Past the end, so a required literal that is absent yields nothing.
            last_end: if self.can_match(text) {
                0
            } else {
                text.len() + 1
            },
            skip_empty_at: None,
        }
    }

    /// Replaces the first match with the replacement string.
    pub fn replace<'t>(&self, text: &'t str, rep: &str) -> std::borrow::Cow<'t, str> {
        match self.find(text) {
            None => std::borrow::Cow::Borrowed(text),
            Some(m) => {
                // Assembled as bytes for simplicity; every match span covers
                // whole codepoints (see `Match::as_str`), so this is always
                // valid UTF-8.
                let bytes = text.as_bytes();
                let mut result = Vec::with_capacity(text.len() + rep.len());
                result.extend_from_slice(&bytes[..m.start()]);
                result.extend_from_slice(rep.as_bytes());
                result.extend_from_slice(&bytes[m.end()..]);
                std::borrow::Cow::Owned(into_string_lossy(result))
            }
        }
    }

    /// Returns the name of the engine being used (for debugging).
    pub fn engine_name(&self) -> &'static str {
        self.inner.engine_name()
    }

    /// Replaces all matches with the replacement string.
    pub fn replace_all<'t>(&self, text: &'t str, rep: &str) -> std::borrow::Cow<'t, str> {
        let bytes = text.as_bytes();
        let mut last_end = 0;
        // Assembled as bytes: see `replace`.
        let mut result = Vec::new();
        let mut had_match = false;

        for m in self.find_iter(text) {
            had_match = true;
            result.extend_from_slice(&bytes[last_end..m.start()]);
            result.extend_from_slice(rep.as_bytes());
            last_end = m.end();
        }

        if !had_match {
            std::borrow::Cow::Borrowed(text)
        } else {
            result.extend_from_slice(&bytes[last_end..]);
            std::borrow::Cow::Owned(into_string_lossy(result))
        }
    }
}

/// A single match in the text.
#[derive(Debug, Clone, Copy)]
pub struct Match<'t> {
    text: &'t str,
    start: usize,
    end: usize,
}

impl<'t> Match<'t> {
    /// Returns the start byte offset of the match.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Returns the end byte offset of the match.
    pub fn end(&self) -> usize {
        self.end
    }

    /// Returns the matched text.
    ///
    /// Every match-producing construct — `.`, character classes, and the
    /// Perl shorthand classes including the ASCII-mode negated forms `\W`
    /// and `\D` — consumes a whole codepoint, so a match span is always a
    /// valid `&str` slice; this is total, never `""` for a non-empty span.
    /// (`.get` is still used defensively rather than an indexing panic.)
    pub fn as_str(&self) -> &'t str {
        self.text.get(self.start..self.end).unwrap_or("")
    }

    /// Returns the matched bytes.
    ///
    /// Byte-identical to slicing [`Match::as_str`]'s underlying text at
    /// [`Match::range`]; provided for callers that want raw bytes without
    /// the `&str` conversion.
    pub fn as_bytes(&self) -> &'t [u8] {
        self.text
            .as_bytes()
            .get(self.start..self.end)
            .unwrap_or(&[])
    }

    /// Returns the byte range of the match.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start..self.end
    }

    /// Returns the length of the match in bytes.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Returns true if the match is empty.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Returns the smallest index `>= i` that is a UTF-8 codepoint boundary of
/// `text`. Indices at or past the end of `text` are returned unchanged, so
/// callers can use `i + 1` to force forward progress past the last byte.
///
/// `std`'s `is_char_boundary` is the sole authority on what a boundary is.
fn ceil_char_boundary(text: &str, i: usize) -> usize {
    let mut j = i;
    while j < text.len() && !text.is_char_boundary(j) {
        j += 1;
    }
    j
}

/// Converts assembled replacement output into a `String`, substituting U+FFFD
/// for any invalid UTF-8. Every match span covers whole codepoints (see
/// `Match::as_str`), and the surrounding bytes are unmodified slices of the
/// original `&str`, so this never actually falls back to the lossy path
/// today; it is kept as a defensive guard rather than an unwrap.
fn into_string_lossy(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    }
}

/// An iterator over all non-overlapping matches.
pub struct Matches<'a> {
    inner: MatchesInner<'a>,
    text: &'a str,
}

impl<'a> std::fmt::Debug for Matches<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Matches")
            .field("text_len", &self.text.len())
            .finish_non_exhaustive()
    }
}

/// Internal iterator state - either uses TeddyFull fast path or generic find().
enum MatchesInner<'a> {
    /// Fast path: use TeddyFull prefilter iterator directly.
    TeddyFull(literal::FullMatchIter<'a, 'a>),
    /// Generic path: call find() repeatedly.
    Generic {
        regex: &'a Regex,
        last_end: usize,
        /// End of the previous match when it was non-empty. An empty match at
        /// that exact position is the same position reported twice, and every
        /// other engine drops it.
        skip_empty_at: Option<usize>,
    },
    /// A required literal is absent, so no match exists anywhere.
    Empty,
}

impl<'a> Matches<'a> {
    /// Creates a new matches iterator.
    fn new(regex: &'a Regex, text: &'a str) -> Self {
        let inner = if !regex.can_match(text) {
            MatchesInner::Empty
        } else if regex.inner.is_full_match_prefilter() {
            // Fast path: use Teddy iterator directly
            MatchesInner::TeddyFull(regex.inner.find_full_matches(text.as_bytes()))
        } else {
            // Generic path
            MatchesInner::Generic {
                regex,
                last_end: 0,
                skip_empty_at: None,
            }
        };
        Matches { inner, text }
    }
}

impl<'a> Iterator for Matches<'a> {
    type Item = Match<'a>;

    fn next(&mut self) -> Option<Match<'a>> {
        match &mut self.inner {
            MatchesInner::Empty => None,
            MatchesInner::TeddyFull(iter) => {
                // Fast path: get match directly from Teddy iterator
                iter.next().map(|(start, end)| Match {
                    text: self.text,
                    start,
                    end,
                })
            }
            MatchesInner::Generic {
                regex,
                last_end,
                skip_empty_at,
            } => {
                loop {
                    if *last_end > self.text.len() {
                        return None;
                    }

                    // The search is resumed at an offset into the *original* text
                    // rather than run on a slice starting there, so `^`, `\b`/`\B`
                    // and lookbehind still see the real text to the left of the
                    // resume point.
                    let (abs_start, abs_end) =
                        regex.inner.find_from(self.text.as_bytes(), *last_end)?;

                    // Every match already ends on a codepoint boundary (the
                    // engine-wide rule, see `nfa::is_utf8_boundary`), so
                    // `ceil_char_boundary` is a no-op here in practice; it is
                    // kept as a defensive snap-forward rather than relying on
                    // that invariant unchecked. For empty matches, step one
                    // byte first so the iterator always makes forward progress.
                    let empty = abs_start == abs_end;
                    *last_end = if empty {
                        ceil_char_boundary(self.text, abs_end + 1)
                    } else {
                        ceil_char_boundary(self.text, abs_end)
                    };

                    // An empty match where the previous, non-empty one ended is
                    // that position reported a second time. `a*` on "aa" is one
                    // match of "aa", not that plus an empty match at 2.
                    if empty && *skip_empty_at == Some(abs_start) {
                        *skip_empty_at = None;
                        continue;
                    }
                    *skip_empty_at = (!empty).then_some(abs_end);

                    return Some(Match {
                        text: self.text,
                        start: abs_start,
                        end: abs_end,
                    });
                }
            }
        }
    }
}

/// An iterator over all non-overlapping captures.
#[derive(Debug)]
pub struct CapturesIter<'r, 't> {
    regex: &'r Regex,
    text: &'t str,
    last_end: usize,
    /// See `MatchesInner::Generic::skip_empty_at`.
    skip_empty_at: Option<usize>,
}

impl<'r, 't> Iterator for CapturesIter<'r, 't> {
    type Item = Captures<'t>;

    fn next(&mut self) -> Option<Captures<'t>> {
        loop {
            if self.last_end > self.text.len() {
                return None;
            }

            // Resumed at an offset into the original text, for the same reason as
            // `Matches::next`; the slots come back as absolute offsets.
            let slots = self
                .regex
                .inner
                .captures_from(self.text.as_bytes(), self.last_end)?;
            let (start, end) = slots.first().and_then(|s| *s)?;

            // Resume at the next UTF-8 character boundary, ensuring progress on
            // empty matches by stepping one byte first (see `Matches::next`).
            let empty = start == end;
            self.last_end = if empty {
                ceil_char_boundary(self.text, end + 1)
            } else {
                ceil_char_boundary(self.text, end)
            };

            // Same suppression as `Matches::next`, so the two iterators report
            // the same match sequence.
            if empty && self.skip_empty_at == Some(start) {
                self.skip_empty_at = None;
                continue;
            }
            self.skip_empty_at = (!empty).then_some(end);

            return Some(Captures {
                text: self.text,
                slots,
                named_groups: Arc::clone(&self.regex.named_groups),
            });
        }
    }
}

/// Captured groups from a regex match.
#[derive(Debug, Clone)]
pub struct Captures<'t> {
    text: &'t str,
    slots: Vec<Option<(usize, usize)>>,
    named_groups: Arc<HashMap<String, u32>>,
}

impl<'t> Captures<'t> {
    /// Returns the number of capture groups (including group 0 for the full match).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Returns true if there are no captures.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Returns the capture group at the given index.
    pub fn get(&self, i: usize) -> Option<Match<'t>> {
        self.slots.get(i).and_then(|slot| {
            slot.map(|(start, end)| Match {
                text: self.text,
                start,
                end,
            })
        })
    }

    /// Returns the capture group with the given name.
    pub fn name(&self, name: &str) -> Option<Match<'t>> {
        self.named_groups
            .get(name)
            .and_then(|&idx| self.get(idx as usize))
    }
}

impl<'t> std::ops::Index<usize> for Captures<'t> {
    type Output = str;

    fn index(&self, i: usize) -> &str {
        self.get(i)
            .map(|m| m.as_str())
            .unwrap_or_else(|| panic!("no capture group at index {}", i))
    }
}

impl<'t> std::ops::Index<&str> for Captures<'t> {
    type Output = str;

    fn index(&self, name: &str) -> &str {
        self.name(name)
            .map(|m| m.as_str())
            .unwrap_or_else(|| panic!("no capture group named '{}'", name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check on the escaped output shape: every metacharacter this
    /// function targets gets a leading backslash, and nothing else does.
    /// Behavioral round-trip coverage (building a real `Regex` from the
    /// escaped output and matching against it) lives in `tests/api/`.
    #[test]
    fn test_escape_shape() {
        assert_eq!(escape(r"\.*+?|^$(){}[]"), r"\\\.\*\+\?\|\^\$\(\)\{\}\[\]");
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape(""), "");
        // Extended mode would otherwise drop these.
        assert_eq!(escape("plain text"), r"plain\ text");
        assert_eq!(escape("a#b"), r"a\#b");
        assert_eq!(escape("a\nb"), r"a\nb");
    }

    /// `ceil_char_boundary` is the resume-position rule for `Matches` /
    /// `CapturesIter`: it must never move backwards, must land on a boundary
    /// inside the text, and must pass indices at/after the end through so an
    /// empty match at the end terminates the iterators.
    #[test]
    fn test_ceil_char_boundary() {
        let text = "aé世🎉";
        // Boundaries: 0 (a), 1 (é), 3 (世), 6 (🎉), 10 (end).
        assert_eq!(ceil_char_boundary(text, 0), 0);
        assert_eq!(ceil_char_boundary(text, 1), 1);
        assert_eq!(ceil_char_boundary(text, 2), 3);
        assert_eq!(ceil_char_boundary(text, 3), 3);
        assert_eq!(ceil_char_boundary(text, 4), 6);
        assert_eq!(ceil_char_boundary(text, 5), 6);
        assert_eq!(ceil_char_boundary(text, 7), 10);
        assert_eq!(ceil_char_boundary(text, 10), 10);
        // Past the end: passed through, which is what stops the iterators.
        assert_eq!(ceil_char_boundary(text, 11), 11);

        // Every ASCII index is already a boundary, so nothing moves.
        let ascii = "abc";
        for i in 0..=ascii.len() {
            assert_eq!(ceil_char_boundary(ascii, i), i);
        }

        // Result is always a boundary (or past the end) and never regresses.
        for i in 0..=text.len() {
            let j = ceil_char_boundary(text, i);
            assert!(j >= i);
            assert!(text.is_char_boundary(j));
        }
    }

    /// `into_string_lossy` must be an exact round-trip for valid UTF-8 (the
    /// only case existing replacements produce) and lossy otherwise.
    #[test]
    fn test_into_string_lossy() {
        assert_eq!(into_string_lossy("héllo".as_bytes().to_vec()), "héllo");
        assert_eq!(into_string_lossy(Vec::new()), "");
        // Orphaned continuation bytes from a split codepoint.
        assert_eq!(
            into_string_lossy(vec![b'-', 0xB8, 0x96]),
            "-\u{FFFD}\u{FFFD}"
        );
    }
}
