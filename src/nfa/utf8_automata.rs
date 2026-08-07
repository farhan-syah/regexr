//! UTF-8 automata compiler.
//!
//! This module converts Unicode code point ranges into sequences of byte ranges
//! that represent valid UTF-8 encodings. This is essential for matching Unicode
//! character classes like `[α-ω]` or `[\u{100}-\u{1FF}]` in a byte-oriented regex engine.
//!
//! # UTF-8 Encoding Structure
//!
//! | Code Point Range     | Byte 1     | Byte 2     | Byte 3     | Byte 4     |
//! |----------------------|------------|------------|------------|------------|
//! | U+0000..U+007F       | 0xxxxxxx   |            |            |            |
//! | U+0080..U+07FF       | 110xxxxx   | 10xxxxxx   |            |            |
//! | U+0800..U+FFFF       | 1110xxxx   | 10xxxxxx   | 10xxxxxx   |            |
//! | U+10000..U+10FFFF    | 11110xxx   | 10xxxxxx   | 10xxxxxx   | 10xxxxxx   |
//!
//! # Algorithm
//!
//! To match a range of code points, we split the range at UTF-8 encoding boundaries
//! and generate byte range sequences for each sub-range. For example, `[a-ÿ]` (U+0061-U+00FF)
//! becomes:
//! - 1-byte: `[0x61-0x7F]` (U+0061-U+007F)
//! - 2-byte: `[0xC2-0xC3][0x80-0xBF]` (U+0080-U+00FF)

/// A sequence of byte ranges representing a UTF-8 encoded character range.
///
/// Each element is a (min, max) byte range. The sequence length corresponds
/// to the UTF-8 encoding length (1-4 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utf8Sequence {
    /// The byte ranges, one per UTF-8 byte position.
    pub ranges: Vec<(u8, u8)>,
}

impl Utf8Sequence {
    /// Creates a new UTF-8 sequence with the given byte ranges.
    pub fn new(ranges: Vec<(u8, u8)>) -> Self {
        Self { ranges }
    }

    /// Returns the number of bytes in this sequence.
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Returns true if this sequence is empty.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// Compiles a Unicode code point range into a set of UTF-8 byte sequences.
///
/// This function takes a range of Unicode code points and returns all the
/// UTF-8 byte sequences needed to match that range. The sequences are
/// returned in order and cover the entire input range without overlap.
///
/// # Example
///
/// ```ignore
/// // Range [a-ÿ] (U+0061-U+00FF)
/// let sequences = compile_utf8_range('a' as u32, 'ÿ' as u32);
/// // Returns:
/// // - 1-byte sequence: [0x61, 0x7F]
/// // - 2-byte sequence: [0xC2, 0xC3] + [0x80, 0xBF]
/// ```
pub fn compile_utf8_range(start: u32, end: u32) -> Vec<Utf8Sequence> {
    if start > end {
        return vec![];
    }

    // Clamp to valid Unicode range
    let start = start.min(0x10FFFF);
    let end = end.min(0x10FFFF);

    let mut sequences = Vec::new();

    // Split the range at UTF-8 encoding boundaries
    let boundaries = [
        0x00,     // 1-byte start
        0x80,     // 2-byte start
        0x800,    // 3-byte start
        0x10000,  // 4-byte start
        0x110000, // End (one past max Unicode)
    ];

    let mut current = start;

    for i in 0..4 {
        let boundary_start = boundaries[i];
        let boundary_end = boundaries[i + 1] - 1;

        if current > boundary_end {
            continue;
        }
        if current > end {
            break;
        }

        let range_start = current.max(boundary_start);
        let range_end = end.min(boundary_end);

        if range_start <= range_end {
            // Generate sequences for this encoding class
            let class_sequences = compile_utf8_class(range_start, range_end, i + 1);
            sequences.extend(class_sequences);
            current = range_end + 1;
        }
    }

    sequences
}

/// Compiles a code point range within a single UTF-8 encoding class.
///
/// `encoding_len` is 1, 2, 3, or 4.
fn compile_utf8_class(start: u32, end: u32, encoding_len: usize) -> Vec<Utf8Sequence> {
    match encoding_len {
        1 => compile_1byte(start, end),
        2 => compile_2byte(start, end),
        3 => compile_3byte(start, end),
        4 => compile_4byte(start, end),
        _ => vec![],
    }
}

/// Compiles 1-byte UTF-8 sequences (U+0000..U+007F).
fn compile_1byte(start: u32, end: u32) -> Vec<Utf8Sequence> {
    debug_assert!(start <= 0x7F && end <= 0x7F);
    vec![Utf8Sequence::new(vec![(start as u8, end as u8)])]
}

/// Compiles 2-byte UTF-8 sequences (U+0080..U+07FF).
///
/// 2-byte encoding: 110xxxxx 10xxxxxx
/// - Byte 1: 0xC0 | (cp >> 6)
/// - Byte 2: 0x80 | (cp & 0x3F)
fn compile_2byte(start: u32, end: u32) -> Vec<Utf8Sequence> {
    debug_assert!(start >= 0x80 && end <= 0x7FF);

    let mut sequences = Vec::new();
    let mut current = start;

    while current <= end {
        // Find the range where byte 1 stays constant
        let byte1 = (0xC0 | (current >> 6)) as u8;

        // Calculate the max code point with the same first byte
        let max_with_same_byte1 = ((current >> 6) << 6) | 0x3F;
        let range_end = end.min(max_with_same_byte1);

        // Byte 2 range
        let byte2_start = (0x80 | (current & 0x3F)) as u8;
        let byte2_end = (0x80 | (range_end & 0x3F)) as u8;

        sequences.push(Utf8Sequence::new(vec![
            (byte1, byte1),
            (byte2_start, byte2_end),
        ]));

        current = range_end + 1;
    }

    sequences
}

/// Compiles 3-byte UTF-8 sequences (U+0800..U+FFFF).
///
/// 3-byte encoding: 1110xxxx 10xxxxxx 10xxxxxx
/// - Byte 1: 0xE0 | (cp >> 12)
/// - Byte 2: 0x80 | ((cp >> 6) & 0x3F)
/// - Byte 3: 0x80 | (cp & 0x3F)
fn compile_3byte(start: u32, end: u32) -> Vec<Utf8Sequence> {
    debug_assert!(start >= 0x800 && end <= 0xFFFF);

    // Skip surrogate range (U+D800..U+DFFF) - these are not valid Unicode scalar values
    let mut sequences = Vec::new();
    let mut current = start;

    while current <= end {
        // Skip surrogates
        if (0xD800..=0xDFFF).contains(&current) {
            current = 0xE000;
            if current > end {
                break;
            }
        }

        let byte1 = (0xE0 | (current >> 12)) as u8;

        // Find max with same byte1
        let max_with_same_byte1 = ((current >> 12) << 12) | 0xFFF;
        let range_end = end.min(max_with_same_byte1);

        // Skip if we'd land in surrogates
        let range_end = if range_end >= 0xD800 && current < 0xD800 {
            0xD7FF
        } else {
            range_end
        };

        if current <= range_end {
            // Now split by byte2
            let sub_sequences = compile_3byte_with_fixed_byte1(current, range_end, byte1);
            sequences.extend(sub_sequences);
        }

        current = range_end + 1;

        // Skip surrogates after the range
        if (0xD800..=0xDFFF).contains(&current) {
            current = 0xE000;
        }
    }

    sequences
}

/// Compiles 3-byte sequences with a fixed first byte.
fn compile_3byte_with_fixed_byte1(start: u32, end: u32, byte1: u8) -> Vec<Utf8Sequence> {
    let mut sequences = Vec::new();
    let mut current = start;

    while current <= end {
        let byte2 = (0x80 | ((current >> 6) & 0x3F)) as u8;

        // Find max with same byte2
        let max_with_same_byte2 = ((current >> 6) << 6) | 0x3F;
        let range_end = end.min(max_with_same_byte2);

        let byte3_start = (0x80 | (current & 0x3F)) as u8;
        let byte3_end = (0x80 | (range_end & 0x3F)) as u8;

        sequences.push(Utf8Sequence::new(vec![
            (byte1, byte1),
            (byte2, byte2),
            (byte3_start, byte3_end),
        ]));

        current = range_end + 1;
    }

    sequences
}

/// Compiles 4-byte UTF-8 sequences (U+10000..U+10FFFF).
///
/// 4-byte encoding: 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx
/// - Byte 1: 0xF0 | (cp >> 18)
/// - Byte 2: 0x80 | ((cp >> 12) & 0x3F)
/// - Byte 3: 0x80 | ((cp >> 6) & 0x3F)
/// - Byte 4: 0x80 | (cp & 0x3F)
fn compile_4byte(start: u32, end: u32) -> Vec<Utf8Sequence> {
    debug_assert!(start >= 0x10000 && end <= 0x10FFFF);

    let mut sequences = Vec::new();
    let mut current = start;

    while current <= end {
        let byte1 = (0xF0 | (current >> 18)) as u8;

        // Find max with same byte1
        let max_with_same_byte1 = ((current >> 18) << 18) | 0x3FFFF;
        let range_end = end.min(max_with_same_byte1);

        let sub_sequences = compile_4byte_with_fixed_byte1(current, range_end, byte1);
        sequences.extend(sub_sequences);

        current = range_end + 1;
    }

    sequences
}

/// Compiles 4-byte sequences with a fixed first byte.
fn compile_4byte_with_fixed_byte1(start: u32, end: u32, byte1: u8) -> Vec<Utf8Sequence> {
    let mut sequences = Vec::new();
    let mut current = start;

    while current <= end {
        let byte2 = (0x80 | ((current >> 12) & 0x3F)) as u8;

        // Find max with same byte2
        let max_with_same_byte2 = ((current >> 12) << 12) | 0xFFF;
        let range_end = end.min(max_with_same_byte2);

        let sub_sequences = compile_4byte_with_fixed_byte12(current, range_end, byte1, byte2);
        sequences.extend(sub_sequences);

        current = range_end + 1;
    }

    sequences
}

/// Compiles 4-byte sequences with fixed first and second bytes.
fn compile_4byte_with_fixed_byte12(
    start: u32,
    end: u32,
    byte1: u8,
    byte2: u8,
) -> Vec<Utf8Sequence> {
    let mut sequences = Vec::new();
    let mut current = start;

    while current <= end {
        let byte3 = (0x80 | ((current >> 6) & 0x3F)) as u8;

        // Find max with same byte3
        let max_with_same_byte3 = ((current >> 6) << 6) | 0x3F;
        let range_end = end.min(max_with_same_byte3);

        let byte4_start = (0x80 | (current & 0x3F)) as u8;
        let byte4_end = (0x80 | (range_end & 0x3F)) as u8;

        sequences.push(Utf8Sequence::new(vec![
            (byte1, byte1),
            (byte2, byte2),
            (byte3, byte3),
            (byte4_start, byte4_end),
        ]));

        current = range_end + 1;
    }

    sequences
}

/// Encodes a single code point to its UTF-8 byte sequence.
///
/// Returns `None` for invalid code points (surrogates or out of range).
pub fn encode_code_point(cp: u32) -> Option<Vec<u8>> {
    if cp > 0x10FFFF || (0xD800..=0xDFFF).contains(&cp) {
        return None;
    }

    let c = char::from_u32(cp)?;
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    Some(s.as_bytes().to_vec())
}

/// Optimizes a list of UTF-8 sequences by merging adjacent ranges where possible.
///
/// This can significantly reduce the number of NFA states needed.
pub fn optimize_sequences(sequences: Vec<Utf8Sequence>) -> Vec<Utf8Sequence> {
    if sequences.len() <= 1 {
        return sequences;
    }

    // Repeat until nothing more merges. A single pass only ever collapses the
    // last byte, which leaves the byte *before* it split into one sequence per
    // value — a full 3-byte range would stay 1024 sequences instead of one.
    // Collapsing the last byte first is what makes the next one mergeable, so
    // the passes have to run to a fixed point.
    let mut current = sequences;
    loop {
        let mut merged = Vec::with_capacity(current.len());
        let mut changed = false;
        let mut index = 0;

        while index < current.len() {
            let Some(mut sequence) = current.get(index).cloned() else {
                break;
            };
            while let Some(next) = current.get(index + 1) {
                match try_merge_sequences(&sequence, next) {
                    Some(combined) => {
                        sequence = combined;
                        index += 1;
                        changed = true;
                    }
                    None => break,
                }
            }
            merged.push(sequence);
            index += 1;
        }

        current = merged;
        if !changed {
            return current;
        }
    }
}

/// Computes the complement of sorted, non-overlapping code point ranges.
///
/// The complement includes all valid Unicode scalar values NOT in the input ranges.
/// Invalid code points (surrogates U+D800-U+DFFF) are automatically excluded.
///
/// # Arguments
///
/// * `ranges` - Sorted, non-overlapping code point ranges to exclude
///
/// # Returns
///
/// A vector of code point ranges representing the complement.
fn complement_code_point_ranges(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    if ranges.is_empty() {
        // If no ranges to exclude, return full Unicode range minus surrogates
        return vec![(0x0000, 0xD7FF), (0xE000, 0x10FFFF)];
    }

    let mut complement = Vec::new();
    let mut current = 0u32;

    for &(start, end) in ranges {
        // Clamp to valid Unicode range
        let start = start.min(0x10FFFF);
        let end = end.min(0x10FFFF);

        // Skip invalid ranges
        if start > end || start > 0x10FFFF {
            continue;
        }

        // Add gap before this range
        if current < start {
            // Need to handle surrogate split
            if current <= 0xD7FF && start > 0xD7FF {
                // Gap crosses surrogate boundary
                if current <= 0xD7FF {
                    complement.push((current, 0xD7FF.min(start.saturating_sub(1))));
                }
                if start > 0xDFFF {
                    complement.push((0xE000.max(current), start.saturating_sub(1)));
                } else if (0xD800..=0xDFFF).contains(&start) {
                    // start is in surrogate range, skip to after
                    current = 0xE000;
                    if current < start {
                        complement.push((current, start.saturating_sub(1)));
                    }
                }
            } else if (0xD800..=0xDFFF).contains(&current) {
                // Current is in surrogate range, skip to after
                current = 0xE000;
                if current < start {
                    complement.push((current, start.saturating_sub(1)));
                }
            } else if (0xD800..=0xDFFF).contains(&start) {
                // Start is in surrogate range
                if current < 0xD800 {
                    complement.push((current, 0xD7FF));
                }
                // current will be set to end+1 below, no need to set here
            } else {
                // Simple gap, no surrogate issues
                complement.push((current, start.saturating_sub(1)));
            }
        }

        // Move past this range
        current = end.saturating_add(1);

        // Skip surrogates if we land in them
        if (0xD800..=0xDFFF).contains(&current) {
            current = 0xE000;
        }
    }

    // Add final gap to end of Unicode
    if current <= 0x10FFFF {
        if current <= 0xD7FF {
            complement.push((current, 0xD7FF));
            if 0xE000 <= 0x10FFFF {
                complement.push((0xE000, 0x10FFFF));
            }
        } else if current >= 0xE000 {
            complement.push((current, 0x10FFFF));
        }
    }

    complement
}

/// Computes the UTF-8 sequences that match the complement of the given code point ranges.
///
/// The complement includes all valid Unicode scalar values NOT in the input ranges.
/// Invalid code points (surrogates) are automatically excluded.
///
/// # Arguments
///
/// * `ranges` - Code point ranges to exclude (does not need to be sorted)
///
/// # Returns
///
/// A vector of UTF-8 sequences representing all valid characters NOT in the input ranges.
///
/// # Example
///
/// ```ignore
/// // Complement of Greek lowercase [α-ω] (U+03B1-U+03C9)
/// let complement = compile_utf8_complement(&[(0x03B1, 0x03C9)]);
/// // Returns sequences matching everything except Greek lowercase
/// ```
pub fn compile_utf8_complement(ranges: &[(u32, u32)]) -> Vec<Utf8Sequence> {
    // Sort and merge input ranges
    let mut sorted_ranges: Vec<(u32, u32)> = ranges.to_vec();
    sorted_ranges.sort_by_key(|r| r.0);

    // Merge overlapping ranges
    let mut merged = Vec::new();
    for range in sorted_ranges {
        if merged.is_empty() {
            merged.push(range);
        } else {
            let last = merged.last_mut().unwrap();
            if range.0 <= last.1.saturating_add(1) {
                // Overlapping or adjacent, merge
                last.1 = last.1.max(range.1);
            } else {
                merged.push(range);
            }
        }
    }

    // Compute complement ranges
    let complement_ranges = complement_code_point_ranges(&merged);

    // Convert each complement range to UTF-8 sequences
    let mut sequences = Vec::new();
    for (start, end) in complement_ranges {
        sequences.extend(compile_utf8_range(start, end));
    }

    // Optimize the result
    optimize_sequences(sequences)
}

/// Tries to merge two UTF-8 sequences that agree everywhere but one byte
/// position, where their ranges are adjacent.
///
/// `[A][B1][C]` and `[A][B2][C]` cover exactly `[A][B1∪B2][C]` when `B1` and
/// `B2` abut, whichever position they sit at — restricting this to the last byte
/// would leave a fully covered range spread across one sequence per value of the
/// second-to-last byte.
fn try_merge_sequences(a: &Utf8Sequence, b: &Utf8Sequence) -> Option<Utf8Sequence> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    let mut differing = None;
    for (index, (left, right)) in a.ranges.iter().zip(b.ranges.iter()).enumerate() {
        if left != right {
            if differing.is_some() {
                return None;
            }
            differing = Some(index);
        }
    }

    let index = differing?;
    let &(a_start, a_end) = a.ranges.get(index)?;
    let &(b_start, b_end) = b.ranges.get(index)?;
    if a_end.checked_add(1) != Some(b_start) {
        return None;
    }

    let mut ranges = a.ranges.clone();
    *ranges.get_mut(index)? = (a_start, b_end);
    Some(Utf8Sequence::new(ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The complement of a small class must stay a handful of sequences.
    ///
    /// This is a size property, not a behaviour one: nothing *matches* wrong when
    /// it regresses. The class simply exceeds the trie budget and falls back to a
    /// code-point node, which forces every pattern containing it onto the PikeVM
    /// — a large, silent slowdown that no correctness test would catch.
    #[test]
    fn test_complement_of_small_class_stays_compact() {
        // Roughly `[^\s<>]`: ASCII whitespace, the two brackets, and the
        // non-ASCII White_Space code points.
        let excluded = [
            (0x09, 0x0D),
            (0x20, 0x20),
            (0x3C, 0x3C),
            (0x3E, 0x3E),
            (0x85, 0x85),
            (0xA0, 0xA0),
            (0x1680, 0x1680),
            (0x2000, 0x200A),
            (0x2028, 0x2029),
            (0x202F, 0x202F),
            (0x205F, 0x205F),
            (0x3000, 0x3000),
        ];
        let sequences = compile_utf8_complement(&excluded);
        assert!(
            sequences.len() <= 64,
            "complement expanded to {} sequences; merging must reach a fixed \
             point across every byte position, not just the last",
            sequences.len()
        );
    }

    /// A fully covered encoding class collapses to one sequence per byte width.
    #[test]
    fn test_full_range_collapses_to_one_sequence_per_width() {
        let three_byte = optimize_sequences(compile_utf8_range(0xE000, 0xFFFF));
        assert_eq!(
            three_byte.len(),
            1,
            "U+E000..U+FFFF is one contiguous 3-byte block: {three_byte:?}"
        );

        let four_byte = optimize_sequences(compile_utf8_range(0x10000, 0x10FFFF));
        assert!(
            four_byte.len() <= 4,
            "the 4-byte plane should not stay split per byte-2 value: {} sequences",
            four_byte.len()
        );
    }

    #[test]
    fn test_encode_code_point_ascii() {
        assert_eq!(encode_code_point(0x41), Some(vec![0x41])); // 'A'
        assert_eq!(encode_code_point(0x7F), Some(vec![0x7F]));
    }

    #[test]
    fn test_encode_code_point_2byte() {
        // U+00E9 = 'é' = 0xC3 0xA9
        assert_eq!(encode_code_point(0xE9), Some(vec![0xC3, 0xA9]));
        // U+00FF = 'ÿ' = 0xC3 0xBF
        assert_eq!(encode_code_point(0xFF), Some(vec![0xC3, 0xBF]));
    }

    #[test]
    fn test_encode_code_point_3byte() {
        // U+3042 = 'あ' = 0xE3 0x81 0x82
        assert_eq!(encode_code_point(0x3042), Some(vec![0xE3, 0x81, 0x82]));
    }

    #[test]
    fn test_encode_code_point_4byte() {
        // U+1F600 = '😀' = 0xF0 0x9F 0x98 0x80
        assert_eq!(
            encode_code_point(0x1F600),
            Some(vec![0xF0, 0x9F, 0x98, 0x80])
        );
    }

    #[test]
    fn test_encode_code_point_invalid() {
        // Surrogates are invalid
        assert_eq!(encode_code_point(0xD800), None);
        assert_eq!(encode_code_point(0xDFFF), None);
        // Out of range
        assert_eq!(encode_code_point(0x110000), None);
    }

    #[test]
    fn test_compile_1byte_range() {
        let seqs = compile_utf8_range(0x41, 0x5A); // A-Z
        assert_eq!(seqs.len(), 1);
        assert_eq!(seqs[0].ranges, vec![(0x41, 0x5A)]);
    }

    #[test]
    fn test_compile_2byte_range() {
        // U+0080 to U+00BF (first 64 2-byte characters)
        let seqs = compile_utf8_range(0x80, 0xBF);
        // All have first byte 0xC2
        assert_eq!(seqs.len(), 1);
        assert_eq!(seqs[0].ranges[0], (0xC2, 0xC2));
        assert_eq!(seqs[0].ranges[1], (0x80, 0xBF));
    }

    #[test]
    fn test_compile_cross_boundary_1_2() {
        // Range crossing 1-byte and 2-byte boundary
        let seqs = compile_utf8_range(0x61, 0xFF); // 'a' to 'ÿ'

        // Should have 1-byte part and 2-byte part
        assert!(seqs.len() >= 2);

        // First sequence should be 1-byte (a-DEL area)
        assert_eq!(seqs[0].len(), 1);
        assert_eq!(seqs[0].ranges[0], (0x61, 0x7F));

        // Remaining sequences should be 2-byte
        for seq in &seqs[1..] {
            assert_eq!(seq.len(), 2);
        }
    }

    #[test]
    fn test_compile_greek_letters() {
        // Greek lowercase: α (U+03B1) to ω (U+03C9)
        let seqs = compile_utf8_range(0x03B1, 0x03C9);

        // All should be 2-byte sequences
        for seq in &seqs {
            assert_eq!(seq.len(), 2);
            // First byte should be 0xCE or 0xCF
            assert!(seq.ranges[0].0 >= 0xCE && seq.ranges[0].1 <= 0xCF);
        }
    }

    #[test]
    fn test_compile_cjk() {
        // CJK Unified Ideographs: U+4E00 to U+4E0F (first 16)
        let seqs = compile_utf8_range(0x4E00, 0x4E0F);

        // All should be 3-byte sequences
        for seq in &seqs {
            assert_eq!(seq.len(), 3);
        }
    }

    #[test]
    fn test_compile_emoji() {
        // Emoji range: U+1F600 to U+1F60F (first 16 emoji faces)
        let seqs = compile_utf8_range(0x1F600, 0x1F60F);

        // All should be 4-byte sequences
        for seq in &seqs {
            assert_eq!(seq.len(), 4);
        }
    }

    #[test]
    fn test_surrogate_skip() {
        // Range that includes surrogates should skip them
        let seqs = compile_utf8_range(0xD700, 0xE000);

        // Should have sequences for U+D700-U+D7FF and U+E000
        // but nothing in U+D800-U+DFFF
        for seq in &seqs {
            // Verify no sequence would match surrogate-like bytes
            // (This is implicit in the algorithm)
            assert!(seq.len() >= 2);
        }
    }

    #[test]
    fn test_optimize_sequences() {
        let seqs = vec![
            Utf8Sequence::new(vec![(0xC2, 0xC2), (0x80, 0x8F)]),
            Utf8Sequence::new(vec![(0xC2, 0xC2), (0x90, 0x9F)]),
            Utf8Sequence::new(vec![(0xC2, 0xC2), (0xA0, 0xAF)]),
        ];

        let optimized = optimize_sequences(seqs);

        // Should merge into one sequence
        assert_eq!(optimized.len(), 1);
        assert_eq!(optimized[0].ranges, vec![(0xC2, 0xC2), (0x80, 0xAF)]);
    }

    #[test]
    fn test_full_unicode_range() {
        // Test the entire valid Unicode range
        let seqs = compile_utf8_range(0, 0x10FFFF);

        // Should have sequences covering all 4 encoding lengths
        let has_1byte = seqs.iter().any(|s| s.len() == 1);
        let has_2byte = seqs.iter().any(|s| s.len() == 2);
        let has_3byte = seqs.iter().any(|s| s.len() == 3);
        let has_4byte = seqs.iter().any(|s| s.len() == 4);

        assert!(has_1byte);
        assert!(has_2byte);
        assert!(has_3byte);
        assert!(has_4byte);
    }

    #[test]
    fn test_single_code_point() {
        // Single code point should produce exactly one sequence
        let seqs = compile_utf8_range(0x03B1, 0x03B1); // Just 'α'
        assert_eq!(seqs.len(), 1);
        assert_eq!(seqs[0].len(), 2);
    }

    #[test]
    fn test_complement_ascii() {
        // [^a-z] should match everything except a-z
        let complement = compile_utf8_complement(&[(0x61, 0x7A)]);

        // Should have sequences for:
        // 1. 0x00-0x60 (single byte)
        // 2. 0x7B-0x7F (single byte)
        // 3. All 2-byte UTF-8 (U+0080-U+07FF)
        // 4. All 3-byte UTF-8 (U+0800-U+FFFF, minus surrogates)
        // 5. All 4-byte UTF-8 (U+10000-U+10FFFF)

        // Verify we have different sequence lengths
        let has_1byte = complement.iter().any(|s| s.len() == 1);
        let has_2byte = complement.iter().any(|s| s.len() == 2);
        let has_3byte = complement.iter().any(|s| s.len() == 3);
        let has_4byte = complement.iter().any(|s| s.len() == 4);

        assert!(has_1byte, "Should have 1-byte sequences");
        assert!(has_2byte, "Should have 2-byte sequences");
        assert!(has_3byte, "Should have 3-byte sequences");
        assert!(has_4byte, "Should have 4-byte sequences");
    }

    #[test]
    fn test_complement_greek() {
        // [^α-ω] should match everything except Greek lowercase (U+03B1-U+03C9)
        let complement = compile_utf8_complement(&[(0x03B1, 0x03C9)]);

        // Should include ASCII, other 2-byte, all 3-byte, all 4-byte
        let has_1byte = complement.iter().any(|s| s.len() == 1);
        let has_2byte = complement.iter().any(|s| s.len() == 2);
        let has_3byte = complement.iter().any(|s| s.len() == 3);
        let has_4byte = complement.iter().any(|s| s.len() == 4);

        assert!(has_1byte, "Should have ASCII");
        assert!(has_2byte, "Should have other 2-byte sequences");
        assert!(has_3byte, "Should have 3-byte sequences");
        assert!(has_4byte, "Should have 4-byte sequences");
    }

    #[test]
    fn test_complement_excludes_surrogates() {
        // Complement of empty set should give full Unicode minus surrogates
        let complement = compile_utf8_complement(&[]);

        // Full range is 0x0000-0xD7FF and 0xE000-0x10FFFF
        // The complement should cover all valid Unicode

        // Verify we never generate sequences for surrogates (U+D800-U+DFFF)
        // Surrogates would be 3-byte: 0xED 0xA0-0xBF 0x80-0xBF
        for seq in &complement {
            if seq.len() == 3 {
                let (b1_start, b1_end) = seq.ranges[0];
                if b1_start == 0xED && b1_end == 0xED {
                    let (b2_start, _) = seq.ranges[1];
                    // If first byte is 0xED, second byte should not start at 0xA0 or higher
                    // (which would enter surrogate territory)
                    assert!(b2_start < 0xA0, "Should not generate surrogate sequences");
                }
            }
        }
    }

    #[test]
    fn test_complement_single_char() {
        // [^α] should match everything except α (U+03B1)
        let complement = compile_utf8_complement(&[(0x03B1, 0x03B1)]);

        // Should have many sequences
        assert!(!complement.is_empty());

        // Should have all encoding lengths
        let has_1byte = complement.iter().any(|s| s.len() == 1);
        let has_2byte = complement.iter().any(|s| s.len() == 2);
        let has_3byte = complement.iter().any(|s| s.len() == 3);
        let has_4byte = complement.iter().any(|s| s.len() == 4);

        assert!(has_1byte);
        assert!(has_2byte);
        assert!(has_3byte);
        assert!(has_4byte);
    }

    #[test]
    fn test_complement_multiple_ranges() {
        // [^a-zA-Z] should match everything except ASCII letters
        let complement = compile_utf8_complement(&[(0x41, 0x5A), (0x61, 0x7A)]);

        // Should have single-byte sequences for other ASCII
        let has_1byte = complement.iter().any(|s| s.len() == 1);
        assert!(has_1byte);

        // Should also have multi-byte for all Unicode
        let has_2byte = complement.iter().any(|s| s.len() == 2);
        let has_3byte = complement.iter().any(|s| s.len() == 3);
        let has_4byte = complement.iter().any(|s| s.len() == 4);

        assert!(has_2byte);
        assert!(has_3byte);
        assert!(has_4byte);
    }

    #[test]
    fn test_complement_cjk() {
        // [^一-龥] should match everything except CJK Unified Ideographs (U+4E00-U+9FFF)
        let complement = compile_utf8_complement(&[(0x4E00, 0x9FFF)]);

        // Should include ASCII and other ranges
        let has_1byte = complement.iter().any(|s| s.len() == 1);
        let has_2byte = complement.iter().any(|s| s.len() == 2);
        let has_3byte = complement.iter().any(|s| s.len() == 3);
        let has_4byte = complement.iter().any(|s| s.len() == 4);

        assert!(has_1byte);
        assert!(has_2byte);
        assert!(has_3byte);
        assert!(has_4byte);
    }

    #[test]
    fn test_complement_emoji() {
        // [^😀-😂] should match everything except a small emoji range
        let complement = compile_utf8_complement(&[(0x1F600, 0x1F602)]);

        // Should have all encoding lengths
        let has_1byte = complement.iter().any(|s| s.len() == 1);
        let has_2byte = complement.iter().any(|s| s.len() == 2);
        let has_3byte = complement.iter().any(|s| s.len() == 3);
        let has_4byte = complement.iter().any(|s| s.len() == 4);

        assert!(has_1byte);
        assert!(has_2byte);
        assert!(has_3byte);
        assert!(has_4byte);
    }

    #[test]
    fn test_complement_boundary_cases() {
        // Test complement at encoding boundaries
        // Range crossing 1-byte/2-byte boundary
        let complement = compile_utf8_complement(&[(0x7F, 0x80)]);
        assert!(!complement.is_empty());

        // Range crossing 2-byte/3-byte boundary
        let complement = compile_utf8_complement(&[(0x7FF, 0x800)]);
        assert!(!complement.is_empty());

        // Range crossing 3-byte/4-byte boundary
        let complement = compile_utf8_complement(&[(0xFFFF, 0x10000)]);
        assert!(!complement.is_empty());
    }
}
