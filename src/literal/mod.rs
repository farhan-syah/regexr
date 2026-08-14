//! Literal extraction and prefilter module.
//!
//! Extracts literal prefixes/suffixes from patterns for fast prefiltering.
//!
//! # Architecture
//!
//! 1. **Literal Extraction** (`extractor.rs`): Analyzes HIR to find required
//!    literal prefixes/suffixes that must appear in any match.
//!
//! 2. **Prefilter** (`prefilter.rs`): Uses extracted literals to build a
//!    SIMD-accelerated candidate filter (memchr or Teddy).
//!
//! 3. **Reverse suffix** (`reverse_suffix.rs`): Recognises patterns whose match
//!    end is easier to find than its start (`\w+(?=ing\b)`), and walks a class
//!    run backwards from a literal occurrence.

mod extractor;
mod prefilter;
mod reverse_suffix;

pub use extractor::*;
pub use prefilter::*;
pub(crate) use reverse_suffix::ReverseSuffixSearch;
