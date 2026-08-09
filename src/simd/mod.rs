//! SIMD acceleration module for high-performance pattern matching.
//!
//! This module provides vectorized string search routines: AVX2 on x86-64,
//! NEON on aarch64, and a scalar fallback everywhere else (and on x86-64 CPUs
//! without AVX2).
//!
//! # Features
//!
//! - **memchr family**: Single-byte and multi-byte search (memchr, memchr2, memchr3, memrchr)
//! - **Teddy**: Multi-literal matcher using SIMD nibble hashing (up to 8 patterns)
//!
//! # Performance
//!
//! AVX2 processes 32 bytes per iteration and NEON 16, against one byte for the
//! scalar fallback. NEON needs no runtime detection: it is mandatory in
//! ARMv8-A, so `#[cfg(target_arch = "aarch64")]` is the whole of the check.
//!
//! # Example
//!
//! ```
//! use regexr::simd::{memchr, Teddy};
//!
//! // Single byte search
//! let pos = memchr(b'x', b"hello world");
//! assert_eq!(pos, None);
//!
//! // Multi-literal search
//! let teddy = Teddy::new(vec![b"hello".to_vec(), b"world".to_vec()]).unwrap();
//! let (pattern_id, pos) = teddy.find(b"say hello there").unwrap();
//! assert_eq!(pattern_id, 0);
//! assert_eq!(pos, 4);
//! ```

mod avx2;
mod fallback;
mod memchr;
mod neon;
mod teddy;

#[cfg(test)]
mod tests;

pub use self::memchr::{memchr, memchr2, memchr3, memchr_range, memrchr};
pub use self::teddy::{Teddy, TeddyIter, MAX_PATTERNS, MAX_PATTERN_LEN};

/// Returns true if AVX2 SIMD instructions are available at runtime.
///
/// This checks the CPU features at runtime and returns true if AVX2 is supported.
///
/// **Not a test for "is there any SIMD".** It answers only for AVX2, and so
/// returns `false` on aarch64, where NEON is always used regardless. Nothing in
/// this crate dispatches on it — each function selects its own path — and new
/// code must not start, or it would route aarch64 to the scalar fallback while
/// a NEON implementation sat unused.
#[inline]
pub fn is_avx2_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
