//! NEON-accelerated byte search for aarch64.
//!
//! The counterpart to [`super::avx2`]. Until this existed, aarch64 ran the
//! scalar fallback — one byte per step, where x86-64 does 32 — so every literal
//! search on Apple Silicon and on ARM servers was an order of magnitude behind
//! the same code on a desktop.
//!
//! # Why there is no runtime check
//!
//! NEON ("Advanced SIMD") is **mandatory** in ARMv8-A, so every `aarch64`
//! target has it. That is the difference from AVX2, which is an extension and
//! must be detected with `is_x86_feature_detected!`. Here `#[cfg(target_arch =
//! "aarch64")]` is the whole of the check, and the intrinsics need no
//! `#[target_feature]` gate — the compiler already assumes NEON for the target.
//!
//! # Finding the matching lane without `movemask`
//!
//! x86 gets a lane bitmap for free from `_mm256_movemask_epi8`. NEON has no such
//! instruction, and emulating it (the usual "multiply by a bit pattern and
//! horizontally add" trick) costs several instructions.
//!
//! The cheaper idiom, used by every function here: a NEON comparison sets a lane
//! to `0xFF` or `0x00`, so reinterpreting the 16×`u8` result as 8×`u16` and
//! narrowing each of those to a nibble with `vshrn_n_u16(_, 4)` produces a
//! `u64` holding **four bits per input byte** — `0xF` for a match, `0x0`
//! otherwise. One `trailing_zeros() / 4` then gives the first matching lane, and
//! `leading_zeros() / 4` the last, which is what [`memrchr_neon`] needs.
//!
//! # Safety
//!
//! Every function is `unsafe` only because the intrinsics are. Loads are
//! `vld1q_u8` on a pointer proven in range by the loop bound (`offset + 16 <=
//! len`), and the trailing bytes go through a scalar loop rather than a masked
//! load, so nothing reads past the end of the haystack.

#![cfg(target_arch = "aarch64")]

use std::arch::aarch64::*;

/// Byte offset of the first `0xFF` lane in a comparison result, or `None`.
///
/// See the module docs for why this is a narrowing shift rather than a
/// movemask.
#[inline]
unsafe fn first_match_lane(cmp: uint8x16_t) -> Option<usize> {
    let nibbles = vget_lane_u64(
        vreinterpret_u64_u8(vshrn_n_u16(vreinterpretq_u16_u8(cmp), 4)),
        0,
    );
    (nibbles != 0).then(|| (nibbles.trailing_zeros() / 4) as usize)
}

/// Byte offset of the **last** `0xFF` lane in a comparison result, or `None`.
#[inline]
unsafe fn last_match_lane(cmp: uint8x16_t) -> Option<usize> {
    let nibbles = vget_lane_u64(
        vreinterpret_u64_u8(vshrn_n_u16(vreinterpretq_u16_u8(cmp), 4)),
        0,
    );
    (nibbles != 0).then(|| 15 - (nibbles.leading_zeros() / 4) as usize)
}

/// NEON `memchr`: first occurrence of `needle`.
#[inline]
pub unsafe fn memchr_neon(needle: u8, haystack: &[u8]) -> Option<usize> {
    let len = haystack.len();
    let ptr = haystack.as_ptr();
    let needle_vec = vdupq_n_u8(needle);

    let mut offset = 0;
    while offset + 16 <= len {
        let data = vld1q_u8(ptr.add(offset));
        if let Some(lane) = first_match_lane(vceqq_u8(data, needle_vec)) {
            return Some(offset + lane);
        }
        offset += 16;
    }
    (offset..len).find(|&i| *haystack.get_unchecked(i) == needle)
}

/// NEON `memchr2`: first occurrence of either byte.
#[inline]
pub unsafe fn memchr2_neon(needle1: u8, needle2: u8, haystack: &[u8]) -> Option<usize> {
    let len = haystack.len();
    let ptr = haystack.as_ptr();
    let (v1, v2) = (vdupq_n_u8(needle1), vdupq_n_u8(needle2));

    let mut offset = 0;
    while offset + 16 <= len {
        let data = vld1q_u8(ptr.add(offset));
        let cmp = vorrq_u8(vceqq_u8(data, v1), vceqq_u8(data, v2));
        if let Some(lane) = first_match_lane(cmp) {
            return Some(offset + lane);
        }
        offset += 16;
    }
    (offset..len).find(|&i| {
        let b = *haystack.get_unchecked(i);
        b == needle1 || b == needle2
    })
}

/// NEON `memchr3`: first occurrence of any of three bytes.
#[inline]
pub unsafe fn memchr3_neon(
    needle1: u8,
    needle2: u8,
    needle3: u8,
    haystack: &[u8],
) -> Option<usize> {
    let len = haystack.len();
    let ptr = haystack.as_ptr();
    let (v1, v2, v3) = (
        vdupq_n_u8(needle1),
        vdupq_n_u8(needle2),
        vdupq_n_u8(needle3),
    );

    let mut offset = 0;
    while offset + 16 <= len {
        let data = vld1q_u8(ptr.add(offset));
        let cmp = vorrq_u8(
            vorrq_u8(vceqq_u8(data, v1), vceqq_u8(data, v2)),
            vceqq_u8(data, v3),
        );
        if let Some(lane) = first_match_lane(cmp) {
            return Some(offset + lane);
        }
        offset += 16;
    }
    (offset..len).find(|&i| {
        let b = *haystack.get_unchecked(i);
        b == needle1 || b == needle2 || b == needle3
    })
}

/// NEON inclusive range search: first byte in `lo..=hi`.
///
/// Uses unsigned min/max rather than a signed comparison: NEON's `vcgtq_u8` is
/// unsigned, but expressing "within a range" as two compares and an AND needs
/// both endpoints anyway, and `vcgeq_u8`/`vcleq_u8` say it directly without the
/// bias trick AVX2 needs for its signed `_mm256_cmpgt_epi8`.
#[inline]
pub unsafe fn memchr_range_neon(lo: u8, hi: u8, haystack: &[u8]) -> Option<usize> {
    let len = haystack.len();
    let ptr = haystack.as_ptr();
    let (lo_vec, hi_vec) = (vdupq_n_u8(lo), vdupq_n_u8(hi));

    let mut offset = 0;
    while offset + 16 <= len {
        let data = vld1q_u8(ptr.add(offset));
        let cmp = vandq_u8(vcgeq_u8(data, lo_vec), vcleq_u8(data, hi_vec));
        if let Some(lane) = first_match_lane(cmp) {
            return Some(offset + lane);
        }
        offset += 16;
    }
    (offset..len).find(|&i| {
        let b = *haystack.get_unchecked(i);
        b >= lo && b <= hi
    })
}

/// NEON `memrchr`: **last** occurrence of `needle`.
///
/// Walks backwards from the end, so the trailing partial vector is at the
/// *front* of the haystack and is handled after the vector loop, not before.
#[inline]
pub unsafe fn memrchr_neon(needle: u8, haystack: &[u8]) -> Option<usize> {
    let len = haystack.len();
    let ptr = haystack.as_ptr();
    let needle_vec = vdupq_n_u8(needle);

    let mut end = len;
    while end >= 16 {
        let offset = end - 16;
        let data = vld1q_u8(ptr.add(offset));
        if let Some(lane) = last_match_lane(vceqq_u8(data, needle_vec)) {
            return Some(offset + lane);
        }
        end = offset;
    }
    (0..end)
        .rev()
        .find(|&i| *haystack.get_unchecked(i) == needle)
}
