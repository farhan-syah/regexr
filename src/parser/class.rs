//! Character-class parsing.

use super::ast::*;
use super::lexer::{EscapeKind, TokenKind};
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result};
use crate::hir::unicode_data;

impl Parser<'_> {
    /// Parses a character class: [abc], [^abc], [a-z].
    pub(super) fn parse_class(&mut self) -> Result<Expr> {
        let start_span = self.current.span;
        // Set in_class BEFORE advancing so the next token is lexed correctly
        self.lexer.set_in_class(true);
        self.advance()?; // consume '['

        // Check for negation
        let negated = if matches!(self.current.kind, TokenKind::Caret) {
            self.advance()?;
            true
        } else {
            false
        };

        let mut ranges = Vec::new();

        // Handle leading ] or -
        if matches!(self.current.kind, TokenKind::CloseBracket) {
            ranges.push(ClassRange::single(']'));
            self.advance()?;
        } else if matches!(self.current.kind, TokenKind::Hyphen) {
            ranges.push(ClassRange::single('-'));
            self.advance()?;
        }

        while !matches!(self.current.kind, TokenKind::CloseBracket | TokenKind::Eof) {
            // Nested character class (set union), e.g. `[a[b-c]]` or the bracketed
            // alternation `[^(\s|[.,!])]`. Parse it recursively and union its
            // members into the parent (complementing a negated nested class).
            if matches!(self.current.kind, TokenKind::OpenBracket) {
                if let Expr::Class(c) = self.parse_class()? {
                    if c.negated {
                        ranges.extend(complement_ranges(&c.ranges));
                    } else {
                        ranges.extend(c.ranges);
                    }
                }
                continue;
            }

            let item = self.parse_class_item()?;

            match item {
                ClassItem::Char(start_char) => {
                    // Check for range
                    if matches!(self.current.kind, TokenKind::Hyphen) {
                        self.advance()?;

                        // Trailing hyphen
                        if matches!(self.current.kind, TokenKind::CloseBracket) {
                            ranges.push(ClassRange::single(start_char));
                            ranges.push(ClassRange::single('-'));
                            break;
                        }

                        let end_item = self.parse_class_item()?;
                        match end_item {
                            ClassItem::Char(end_char) => {
                                if start_char > end_char {
                                    return Err(Error::with_span(
                                        ErrorKind::InvalidClassRange {
                                            start: start_char,
                                            end: end_char,
                                        },
                                        self.pattern,
                                        self.current.span,
                                    ));
                                }
                                ranges.push(ClassRange::new(start_char, end_char));
                            }
                            ClassItem::Ranges(r) => {
                                // Can't have a range ending with a Perl class like [a-\d]
                                // Just add start_char, hyphen, and the ranges
                                ranges.push(ClassRange::single(start_char));
                                ranges.push(ClassRange::single('-'));
                                ranges.extend(r);
                            }
                            ClassItem::UnicodeProperty { name, negated: _ } => {
                                // Can't have a range ending with a Unicode property like [a-\p{P}]
                                // Just add start_char, hyphen, and expand the property
                                ranges.push(ClassRange::single(start_char));
                                ranges.push(ClassRange::single('-'));
                                if let Some(code_point_ranges) = unicode_data::get_property(&name) {
                                    for &(start, end) in code_point_ranges {
                                        if (0xD800..=0xDFFF).contains(&start) {
                                            continue;
                                        }
                                        let start = start.min(0x10FFFF);
                                        let end = end.min(0x10FFFF);
                                        if let (Some(s), Some(e)) =
                                            (char::from_u32(start), char::from_u32(end))
                                        {
                                            ranges.push(ClassRange::new(s, e));
                                        }
                                    }
                                } else {
                                    return Err(Error::with_span(
                                        ErrorKind::UnknownUnicodeProperty(name.clone()),
                                        self.pattern,
                                        self.current.span,
                                    ));
                                }
                            }
                        }
                    } else {
                        ranges.push(ClassRange::single(start_char));
                    }
                }
                ClassItem::Ranges(r) => {
                    // Perl class like \d, \w, \s - just add all ranges
                    ranges.extend(r);
                }
                ClassItem::UnicodeProperty { name, negated } => {
                    // Look up Unicode property and expand to ranges
                    if let Some(code_point_ranges) = unicode_data::get_property(&name) {
                        // Convert (u32, u32) code point ranges to ClassRange (char-based)
                        for &(start, end) in code_point_ranges {
                            // Skip surrogate range (U+D800-U+DFFF) since they're not valid chars
                            if (0xD800..=0xDFFF).contains(&start) {
                                continue;
                            }
                            // Clamp end to valid char range
                            let start = start.min(0x10FFFF);
                            let end = end.min(0x10FFFF);

                            // Handle ranges that span across surrogates
                            if start < 0xD800 && end > 0xDFFF {
                                // Split into two ranges: before and after surrogates
                                if let (Some(s), Some(e)) =
                                    (char::from_u32(start), char::from_u32(0xD7FF))
                                {
                                    ranges.push(ClassRange::new(s, e));
                                }
                                if let (Some(s), Some(e)) =
                                    (char::from_u32(0xE000), char::from_u32(end))
                                {
                                    ranges.push(ClassRange::new(s, e));
                                }
                            } else if start <= 0xD7FF && (0xD800..=0xDFFF).contains(&end) {
                                // Range ends in surrogates, truncate
                                if let (Some(s), Some(e)) =
                                    (char::from_u32(start), char::from_u32(0xD7FF))
                                {
                                    ranges.push(ClassRange::new(s, e));
                                }
                            } else if (0xD800..=0xDFFF).contains(&start) && end > 0xDFFF {
                                // Range starts in surrogates, start from after
                                if let (Some(s), Some(e)) =
                                    (char::from_u32(0xE000), char::from_u32(end))
                                {
                                    ranges.push(ClassRange::new(s, e));
                                }
                            } else if let (Some(s), Some(e)) =
                                (char::from_u32(start), char::from_u32(end))
                            {
                                ranges.push(ClassRange::new(s, e));
                            }
                        }

                        // Handle negation: compute complement for \P{} inside class
                        if negated {
                            // Compute complement: all characters NOT in the property ranges
                            // The ranges we just added are the positive ranges, we need their complement
                            let count_to_drain = code_point_ranges
                                .iter()
                                .filter(|&&(start, _)| {
                                    // Count how many ranges we actually added
                                    !(0xD800..=0xDFFF).contains(&start)
                                })
                                .map(|&(start, end)| {
                                    let start = start.min(0x10FFFF);
                                    let end = end.min(0x10FFFF);
                                    if start < 0xD800 && end > 0xDFFF {
                                        2
                                    } else {
                                        1
                                    }
                                })
                                .sum::<usize>();
                            let drain_start = ranges.len().saturating_sub(count_to_drain);
                            let positive_ranges: Vec<ClassRange> =
                                ranges.drain(drain_start..).collect();

                            // Compute complement of positive_ranges
                            // Sort and merge positive ranges first
                            let mut sorted: Vec<(u32, u32)> = positive_ranges
                                .iter()
                                .map(|r| (r.start as u32, r.end as u32))
                                .collect();
                            sorted.sort_by_key(|r| r.0);

                            // Merge overlapping ranges
                            let mut merged: Vec<(u32, u32)> = Vec::new();
                            for (start, end) in sorted {
                                if let Some(last) = merged.last_mut() {
                                    if start <= last.1 + 1 {
                                        last.1 = last.1.max(end);
                                        continue;
                                    }
                                }
                                merged.push((start, end));
                            }

                            // Compute complement (gaps between merged ranges)
                            let mut prev_end: u32 = 0;
                            for (start, end) in merged {
                                if prev_end < start {
                                    // Gap from prev_end to start-1
                                    let gap_start = prev_end;
                                    let gap_end = start - 1;
                                    // Add gap ranges, avoiding surrogates
                                    if gap_start < 0xD800 {
                                        let s = gap_start;
                                        let e = gap_end.min(0xD7FF);
                                        if s <= e {
                                            if let (Some(cs), Some(ce)) =
                                                (char::from_u32(s), char::from_u32(e))
                                            {
                                                ranges.push(ClassRange::new(cs, ce));
                                            }
                                        }
                                    }
                                    if gap_end > 0xDFFF {
                                        let s = gap_start.max(0xE000);
                                        let e = gap_end;
                                        if s <= e {
                                            if let (Some(cs), Some(ce)) =
                                                (char::from_u32(s), char::from_u32(e))
                                            {
                                                ranges.push(ClassRange::new(cs, ce));
                                            }
                                        }
                                    }
                                }
                                prev_end = end + 1;
                            }
                            // Add final gap from last range to max codepoint
                            if prev_end <= 0x10FFFF {
                                let gap_start = prev_end;
                                let gap_end = 0x10FFFF;
                                if gap_start < 0xD800 {
                                    let s = gap_start;
                                    let e = gap_end.min(0xD7FF);
                                    if s <= e {
                                        if let (Some(cs), Some(ce)) =
                                            (char::from_u32(s), char::from_u32(e))
                                        {
                                            ranges.push(ClassRange::new(cs, ce));
                                        }
                                    }
                                }
                                if gap_end > 0xDFFF {
                                    let s = gap_start.max(0xE000);
                                    let e = gap_end;
                                    if s <= e {
                                        if let (Some(cs), Some(ce)) =
                                            (char::from_u32(s), char::from_u32(e))
                                        {
                                            ranges.push(ClassRange::new(cs, ce));
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        return Err(Error::with_span(
                            ErrorKind::UnknownUnicodeProperty(name.clone()),
                            self.pattern,
                            self.current.span,
                        ));
                    }
                }
            }
        }

        self.lexer.set_in_class(false);

        if matches!(self.current.kind, TokenKind::Eof) {
            return Err(Error::with_span(
                ErrorKind::UnmatchedOpenBracket,
                self.pattern,
                start_span,
            ));
        }

        self.advance()?; // consume ']'

        if ranges.is_empty() {
            return Err(Error::with_span(
                ErrorKind::EmptyClass,
                self.pattern,
                start_span,
            ));
        }

        Ok(Expr::Class(Box::new(Class::new(ranges, negated))))
    }

    /// Parses a class item which can be a single char, a range, or a Perl class.
    /// Returns either a list of ranges (for Perl classes) or a single character.
    fn parse_class_item(&mut self) -> Result<ClassItem> {
        match &self.current.kind {
            TokenKind::Escape(esc) => {
                match esc {
                    // Perl character classes - expand to ranges
                    EscapeKind::Digit => {
                        self.advance()?;
                        Ok(ClassItem::Ranges(vec![ClassRange::new('0', '9')]))
                    }
                    EscapeKind::NotDigit => {
                        self.advance()?;
                        // [^\d] = everything except 0-9
                        Ok(ClassItem::Ranges(vec![
                            ClassRange::new('\x00', '/'),       // 0x00-0x2F
                            ClassRange::new(':', '\u{10FFFF}'), // 0x3A onwards
                        ]))
                    }
                    EscapeKind::Word => {
                        self.advance()?;
                        Ok(ClassItem::Ranges(vec![
                            ClassRange::new('a', 'z'),
                            ClassRange::new('A', 'Z'),
                            ClassRange::new('0', '9'),
                            ClassRange::single('_'),
                        ]))
                    }
                    EscapeKind::NotWord => {
                        self.advance()?;
                        // [^\w] = everything except [a-zA-Z0-9_]
                        Ok(ClassItem::Ranges(vec![
                            ClassRange::new('\x00', '/'),       // before '0'
                            ClassRange::new(':', '@'),          // between '9' and 'A'
                            ClassRange::new('[', '^'),          // between 'Z' and '_'
                            ClassRange::single('`'),            // between '_' and 'a'
                            ClassRange::new('{', '\u{10FFFF}'), // after 'z'
                        ]))
                    }
                    EscapeKind::Whitespace => {
                        self.advance()?;
                        // Full Unicode `White_Space` set (see `translate_perl_class`).
                        Ok(ClassItem::Ranges(unicode_whitespace_ranges()))
                    }
                    EscapeKind::NotWhitespace => {
                        self.advance()?;
                        Ok(ClassItem::Ranges(complement_ranges(
                            &unicode_whitespace_ranges(),
                        )))
                    }
                    // Single character escapes
                    EscapeKind::Literal(c) => {
                        let c = *c;
                        self.advance()?;
                        Ok(ClassItem::Char(c))
                    }
                    EscapeKind::Newline => {
                        self.advance()?;
                        Ok(ClassItem::Char('\n'))
                    }
                    EscapeKind::CarriageReturn => {
                        self.advance()?;
                        Ok(ClassItem::Char('\r'))
                    }
                    EscapeKind::Tab => {
                        self.advance()?;
                        Ok(ClassItem::Char('\t'))
                    }
                    EscapeKind::FormFeed => {
                        self.advance()?;
                        Ok(ClassItem::Char('\x0C'))
                    }
                    EscapeKind::VerticalTab => {
                        self.advance()?;
                        Ok(ClassItem::Char('\x0B'))
                    }
                    EscapeKind::Null => {
                        self.advance()?;
                        Ok(ClassItem::Char('\0'))
                    }
                    EscapeKind::Hex(c) | EscapeKind::Unicode(c) => {
                        let c = *c;
                        self.advance()?;
                        Ok(ClassItem::Char(c))
                    }
                    // Unicode properties \p{...} and \P{...}
                    EscapeKind::UnicodeProperty(name) => {
                        let name = name.clone();
                        self.advance()?;
                        Ok(ClassItem::UnicodeProperty {
                            name,
                            negated: false,
                        })
                    }
                    EscapeKind::NotUnicodeProperty(name) => {
                        let name = name.clone();
                        self.advance()?;
                        Ok(ClassItem::UnicodeProperty {
                            name,
                            negated: true,
                        })
                    }
                    // Anchors and boundaries are assertions, not members of a
                    // set. Naming the offending escape is what makes the
                    // rejection actionable, so recover its text from the span.
                    _ => Err(Error::with_span(
                        ErrorKind::EscapeNotAllowedInClass(self.current_text().to_string()),
                        self.pattern,
                        self.current.span,
                    )),
                }
            }
            // POSIX bracket-expression class, e.g. `[:alpha:]` or `[:^alpha:]`.
            // The lexer has already validated the syntax; only the name can
            // still be unrecognized here.
            TokenKind::PosixClass { name, negated } => {
                let name = name.clone();
                let negated = *negated;
                let ranges = posix_class_ranges(&name).ok_or_else(|| {
                    Error::with_span(
                        ErrorKind::UnknownPosixClass(name.clone()),
                        self.pattern,
                        self.current.span,
                    )
                })?;
                let ranges = if negated {
                    complement_ranges(ranges)
                } else {
                    ranges.to_vec()
                };
                self.advance()?;
                Ok(ClassItem::Ranges(ranges))
            }
            TokenKind::Literal(c) => {
                let c = *c;
                self.advance()?;
                Ok(ClassItem::Char(c))
            }
            // Inside a character class (not at start), caret is a literal character
            TokenKind::Caret => {
                self.advance()?;
                Ok(ClassItem::Char('^'))
            }
            _ => Err(Error::with_span(
                ErrorKind::UnexpectedChar(self.current_char().unwrap_or('?')),
                self.pattern,
                self.current.span,
            )),
        }
    }
}

/// Item parsed from a character class.
enum ClassItem {
    /// A single character
    Char(char),
    /// Multiple ranges (from Perl classes like \d, \w, \s)
    Ranges(Vec<ClassRange>),
    /// Unicode property (will be expanded to ranges later)
    UnicodeProperty { name: String, negated: bool },
}

/// The Unicode `White_Space` property as character ranges (for `\s`/`\S`).
fn unicode_whitespace_ranges() -> Vec<ClassRange> {
    const WS: &[(u32, u32)] = &[
        (0x0009, 0x000D),
        (0x0020, 0x0020),
        (0x0085, 0x0085),
        (0x00A0, 0x00A0),
        (0x1680, 0x1680),
        (0x2000, 0x200A),
        (0x2028, 0x2029),
        (0x202F, 0x202F),
        (0x205F, 0x205F),
        (0x3000, 0x3000),
    ];
    WS.iter()
        .map(|&(s, e)| ClassRange::new(char::from_u32(s).unwrap(), char::from_u32(e).unwrap()))
        .collect()
}

/// ASCII ranges for the 14 POSIX bracket-expression class names, e.g.
/// `[:alpha:]`. Returns `None` for an unrecognized name.
///
/// Deliberately ASCII-only — `[:alpha:]` is `[A-Za-z]`, not Unicode
/// `Alphabetic`. This matches PCRE, grep and the `regex` crate, and matches
/// this codebase's own precedent that `\d`/`\w` are ASCII-only rather than
/// wired to `crate::hir::unicode_data`.
fn posix_class_ranges(name: &str) -> Option<&'static [ClassRange]> {
    const ALPHA: &[ClassRange] = &[
        ClassRange {
            start: 'A',
            end: 'Z',
        },
        ClassRange {
            start: 'a',
            end: 'z',
        },
    ];
    const DIGIT: &[ClassRange] = &[ClassRange {
        start: '0',
        end: '9',
    }];
    const ALNUM: &[ClassRange] = &[
        ClassRange {
            start: 'A',
            end: 'Z',
        },
        ClassRange {
            start: 'a',
            end: 'z',
        },
        ClassRange {
            start: '0',
            end: '9',
        },
    ];
    const UPPER: &[ClassRange] = &[ClassRange {
        start: 'A',
        end: 'Z',
    }];
    const LOWER: &[ClassRange] = &[ClassRange {
        start: 'a',
        end: 'z',
    }];
    const SPACE: &[ClassRange] = &[
        ClassRange {
            start: '\t',
            end: '\r',
        },
        ClassRange {
            start: ' ',
            end: ' ',
        },
    ];
    const BLANK: &[ClassRange] = &[
        ClassRange {
            start: '\t',
            end: '\t',
        },
        ClassRange {
            start: ' ',
            end: ' ',
        },
    ];
    const CNTRL: &[ClassRange] = &[
        ClassRange {
            start: '\x00',
            end: '\x1F',
        },
        ClassRange {
            start: '\x7F',
            end: '\x7F',
        },
    ];
    const PRINT: &[ClassRange] = &[ClassRange {
        start: '\x20',
        end: '\x7E',
    }];
    const GRAPH: &[ClassRange] = &[ClassRange {
        start: '\x21',
        end: '\x7E',
    }];
    const PUNCT: &[ClassRange] = &[
        ClassRange {
            start: '\x21',
            end: '\x2F',
        },
        ClassRange {
            start: '\x3A',
            end: '\x40',
        },
        ClassRange {
            start: '\x5B',
            end: '\x60',
        },
        ClassRange {
            start: '\x7B',
            end: '\x7E',
        },
    ];
    const XDIGIT: &[ClassRange] = &[
        ClassRange {
            start: '0',
            end: '9',
        },
        ClassRange {
            start: 'A',
            end: 'F',
        },
        ClassRange {
            start: 'a',
            end: 'f',
        },
    ];
    const WORD: &[ClassRange] = &[
        ClassRange {
            start: 'A',
            end: 'Z',
        },
        ClassRange {
            start: 'a',
            end: 'z',
        },
        ClassRange {
            start: '0',
            end: '9',
        },
        ClassRange {
            start: '_',
            end: '_',
        },
    ];
    const ASCII: &[ClassRange] = &[ClassRange {
        start: '\x00',
        end: '\x7F',
    }];

    match name {
        "alpha" => Some(ALPHA),
        "digit" => Some(DIGIT),
        "alnum" => Some(ALNUM),
        "upper" => Some(UPPER),
        "lower" => Some(LOWER),
        "space" => Some(SPACE),
        "blank" => Some(BLANK),
        "cntrl" => Some(CNTRL),
        "print" => Some(PRINT),
        "graph" => Some(GRAPH),
        "punct" => Some(PUNCT),
        "xdigit" => Some(XDIGIT),
        "word" => Some(WORD),
        "ascii" => Some(ASCII),
        _ => None,
    }
}

/// Complement of a sorted-or-unsorted set of character ranges over the Unicode
/// scalar value space (skipping the surrogate gap `U+D800..=U+DFFF`).
fn complement_ranges(ranges: &[ClassRange]) -> Vec<ClassRange> {
    let mut pts: Vec<(u32, u32)> = ranges
        .iter()
        .map(|r| (r.start as u32, r.end as u32))
        .collect();
    pts.sort_by_key(|r| r.0);
    let mut out = Vec::new();
    let mut next = 0u32;
    for (s, e) in pts {
        if s > next {
            push_scalar_range(&mut out, next, s - 1);
        }
        if e + 1 > next {
            next = e + 1;
        }
    }
    if next <= 0x10FFFF {
        push_scalar_range(&mut out, next, 0x10FFFF);
    }
    out
}

/// Push `start..=end` as `ClassRange`(s), splitting around the surrogate gap.
fn push_scalar_range(out: &mut Vec<ClassRange>, start: u32, end: u32) {
    const SUR_LO: u32 = 0xD800;
    const SUR_HI: u32 = 0xDFFF;
    if start > end {
        return;
    }
    if end < SUR_LO || start > SUR_HI {
        out.push(ClassRange::new(
            char::from_u32(start).unwrap(),
            char::from_u32(end).unwrap(),
        ));
    } else {
        if start < SUR_LO {
            out.push(ClassRange::new(
                char::from_u32(start).unwrap(),
                char::from_u32(SUR_LO - 1).unwrap(),
            ));
        }
        if end > SUR_HI {
            out.push(ClassRange::new(
                char::from_u32(SUR_HI + 1).unwrap(),
                char::from_u32(end).unwrap(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_all_14_posix_class_names() {
        for name in [
            "alpha", "digit", "alnum", "upper", "lower", "space", "blank", "cntrl", "print",
            "graph", "punct", "xdigit", "word", "ascii",
        ] {
            assert!(
                posix_class_ranges(name).is_some(),
                "{name} should be a recognized POSIX class"
            );
        }
    }

    #[test]
    fn rejects_unknown_names() {
        assert!(posix_class_ranges("bogus").is_none());
        assert!(posix_class_ranges("").is_none());
        assert!(posix_class_ranges("Alpha").is_none());
    }

    #[test]
    fn alpha_is_ascii_letters_only() {
        let ranges = posix_class_ranges("alpha").unwrap();
        assert!(ranges.iter().any(|r| r.contains('a')));
        assert!(ranges.iter().any(|r| r.contains('X')));
        assert!(!ranges.iter().any(|r| r.contains('9')));
        assert!(!ranges.iter().any(|r| r.contains('_')));
        // Not the Unicode `Alphabetic` property.
        assert!(!ranges.iter().any(|r| r.contains('é')));
    }

    #[test]
    fn digit_is_0_to_9() {
        let ranges = posix_class_ranges("digit").unwrap();
        assert!(ranges.iter().any(|r| r.contains('5')));
        assert!(!ranges.iter().any(|r| r.contains('a')));
    }

    #[test]
    fn space_matches_space_and_tab_but_not_a() {
        let ranges = posix_class_ranges("space").unwrap();
        assert!(ranges.iter().any(|r| r.contains(' ')));
        assert!(ranges.iter().any(|r| r.contains('\t')));
        assert!(!ranges.iter().any(|r| r.contains('a')));
    }

    #[test]
    fn word_matches_underscore() {
        let ranges = posix_class_ranges("word").unwrap();
        assert!(ranges.iter().any(|r| r.contains('_')));
    }
}
