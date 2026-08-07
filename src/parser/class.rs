//! Character-class parsing.

use super::ast::*;
use super::lexer::{EscapeKind, TokenKind};
use super::state::Parser;
use crate::error::{Error, ErrorKind, Result};
use crate::hir::unicode_data;
use crate::hir::HORIZONTAL_WHITESPACE;

impl Parser<'_> {
    /// Parses a character class: [abc], [^abc], [a-z].
    ///
    /// Also handles the binary set operators `&&` (intersection), `--`
    /// (difference), and `~~` (symmetric difference), e.g.
    /// `[a-z&&[^aeiou]]`. All three share one precedence level and associate
    /// left to right: `[a-z&&[^aeiou]--[xyz]]` is `((a-z) && (^aeiou)) --
    /// (xyz)`. A leading `^` negates the *result* of the whole expression,
    /// which falls out naturally here — `negated` is only ever consumed by
    /// the caller once `ranges` is final, so it is applied last regardless
    /// of how many operators preceded it.
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

        // Handle leading ] or - (but not the leading '-' of a `--` operator,
        // which has no left operand and must be rejected, not read as a
        // literal hyphen).
        if matches!(self.current.kind, TokenKind::CloseBracket) {
            ranges.push(ClassRange::single(']'));
            self.advance()?;
        } else if matches!(self.current.kind, TokenKind::Hyphen) && self.peek_set_op().is_none() {
            ranges.push(ClassRange::single('-'));
            self.advance()?;
        }

        ranges.extend(self.parse_class_term()?);

        while let Some(op) = self.peek_set_op() {
            let op_span = self.current.span;
            if ranges.is_empty() {
                // e.g. `[&&a-z]` or `[--z]`: the operator has no left operand.
                return Err(Error::with_span(
                    ErrorKind::InvalidClassSetOp,
                    self.pattern,
                    op_span,
                ));
            }
            self.advance()?; // first operator character
            self.advance()?; // second operator character

            let rhs = self.parse_class_term()?;
            if rhs.is_empty() {
                // e.g. `[a-z&&]` (operand is the closing bracket), `[a-z&&`
                // (operand is end of pattern), or `[a-z&&&&b]` (operand is
                // another operator).
                return Err(Error::with_span(
                    ErrorKind::InvalidClassSetOp,
                    self.pattern,
                    op_span,
                ));
            }

            ranges = op.apply(&ranges, &rhs);
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

    /// Detects a doubled set-operator (`&&`, `--`, `~~`) starting at the
    /// current token, without consuming anything. Only two of the same
    /// character back to back count — a single `&`, `-`, or `~` keeps its
    /// ordinary meaning (literal member, or range hyphen). Checking the raw
    /// source text (rather than lexing ahead) also means an escaped second
    /// character, e.g. `&\&`, is correctly *not* read as `&&`.
    fn peek_set_op(&self) -> Option<SetOp> {
        let next = self.pattern.get(self.current.span.end..)?.chars().next();
        match (&self.current.kind, next) {
            (TokenKind::Literal('&'), Some('&')) => Some(SetOp::Intersection),
            (TokenKind::Hyphen, Some('-')) => Some(SetOp::Difference),
            (TokenKind::Literal('~'), Some('~')) => Some(SetOp::SymmetricDifference),
            _ => None,
        }
    }

    /// Parses one operand of a bracket expression: a run of class members —
    /// literal characters, ranges, nested classes (`[...]`, unioned in as
    /// today), Perl classes, Unicode properties, POSIX classes — up to
    /// whichever comes first: the closing `]`, end of input, or the start of
    /// a doubled set-operator. Returns whatever ranges were accumulated,
    /// which is empty if the term is empty; the caller treats an empty term
    /// next to an operator as a missing operand.
    fn parse_class_term(&mut self) -> Result<Vec<ClassRange>> {
        let mut ranges = Vec::new();

        while !matches!(self.current.kind, TokenKind::CloseBracket | TokenKind::Eof)
            && self.peek_set_op().is_none()
        {
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
                    // Check for range, but not when the hyphen is actually
                    // the start of a `--` (difference) operator — e.g. in
                    // `[aeiou--a]` the `u` must not try to start a `u-?`
                    // range and swallow half the operator.
                    if matches!(self.current.kind, TokenKind::Hyphen)
                        && self.peek_set_op().is_none()
                    {
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
                            ranges.extend(complement_ranges(&positive_ranges));
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

        Ok(ranges)
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
                    EscapeKind::HorizontalWhitespace => {
                        self.advance()?;
                        Ok(ClassItem::Ranges(horizontal_whitespace_ranges()))
                    }
                    EscapeKind::NotHorizontalWhitespace => {
                        self.advance()?;
                        Ok(ClassItem::Ranges(complement_ranges(
                            &horizontal_whitespace_ranges(),
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
                    // `\b` is the word-boundary assertion outside a class, but
                    // that reading is meaningless inside one; PCRE and Perl
                    // instead give it the C-string sense of "backspace"
                    // (U+0008) here. Outside a class this arm is never
                    // reached, so bare `\b` is untouched.
                    EscapeKind::WordBoundary => {
                        self.advance()?;
                        Ok(ClassItem::Char('\u{8}'))
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
                    // set. `\R` (which falls here too) can match two
                    // characters (`\r\n`), which a single class member can
                    // never be; `\N` is rejected the same way for symmetry
                    // with PCRE, since it is not a fixed set of code points.
                    // Naming the offending escape is what makes the
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
///
/// Built from the generated `PERL_SPACE` table, the same one `\s`/`\S` use
/// outside a class, so `\s` and `[\s]` cannot drift apart.
fn unicode_whitespace_ranges() -> Vec<ClassRange> {
    codepoints_to_ranges(unicode_data::PERL_SPACE)
}

/// The fixed 18-code-point `\h` (horizontal whitespace) set as character
/// ranges, for use inside a character class (`[\h]`). Built from
/// `hir::HORIZONTAL_WHITESPACE`, the single source of truth for the set that
/// `\h`/`\H` also use outside a class.
fn horizontal_whitespace_ranges() -> Vec<ClassRange> {
    codepoints_to_ranges(HORIZONTAL_WHITESPACE)
}

/// Converts a table of inclusive code-point ranges into [`ClassRange`]s.
///
/// The tables are compile-time constants holding only scalar values, so a
/// non-scalar entry would be a bug in the table rather than bad input; such an
/// entry is skipped rather than panicking.
fn codepoints_to_ranges(table: &[(u32, u32)]) -> Vec<ClassRange> {
    table
        .iter()
        .filter_map(|&(s, e)| Some(ClassRange::new(char::from_u32(s)?, char::from_u32(e)?)))
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

/// Pushes `start..=end` as a `ClassRange` if both bounds are valid scalar
/// values (i.e. not surrogates). A non-scalar bound is skipped rather than
/// panicking; callers of this helper only ever pass already-split,
/// surrogate-free bounds, so the skip path is unreachable in practice, but
/// it is here rather than an `unwrap` so a future bug degrades instead of
/// crashing.
fn push_scalar_range_checked(out: &mut Vec<ClassRange>, start: u32, end: u32) {
    if let (Some(s), Some(e)) = (char::from_u32(start), char::from_u32(end)) {
        out.push(ClassRange::new(s, e));
    }
}

/// Push `start..=end` as `ClassRange`(s), splitting around the surrogate gap.
fn push_scalar_range(out: &mut Vec<ClassRange>, start: u32, end: u32) {
    const SUR_LO: u32 = 0xD800;
    const SUR_HI: u32 = 0xDFFF;
    if start > end {
        return;
    }
    if end < SUR_LO || start > SUR_HI {
        push_scalar_range_checked(out, start, end);
    } else {
        if start < SUR_LO {
            push_scalar_range_checked(out, start, SUR_LO - 1);
        }
        if end > SUR_HI {
            push_scalar_range_checked(out, SUR_HI + 1, end);
        }
    }
}

/// A binary set operator inside a bracket expression: `&&` (intersection),
/// `--` (difference), or `~~` (symmetric difference). All three share one
/// precedence level and associate left to right (see `Parser::parse_class`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetOp {
    Intersection,
    Difference,
    SymmetricDifference,
}

impl SetOp {
    /// Applies the operator to a left- and right-hand range set, returning
    /// the normalized (sorted, coalesced) result.
    fn apply(self, lhs: &[ClassRange], rhs: &[ClassRange]) -> Vec<ClassRange> {
        match self {
            SetOp::Intersection => intersect_ranges(lhs, rhs),
            SetOp::Difference => difference_ranges(lhs, rhs),
            SetOp::SymmetricDifference => symmetric_difference_ranges(lhs, rhs),
        }
    }
}

/// An inclusive code-point interval `(start, end)`, both bounds inclusive.
/// The raw-`u32` counterpart of [`ClassRange`], used while combining range
/// sets so that intervals spanning the surrogate gap can be manipulated as
/// ordinary numeric ranges and only split back into valid `char` ranges at
/// the very end, via [`push_scalar_range`].
type Interval = (u32, u32);

/// Sorts a list of intervals and coalesces any that overlap or touch
/// (`b.start <= a.end + 1`) into one. This is the canonical form every set
/// operation below both expects as input and produces as output.
fn sorted_merged(mut intervals: Vec<Interval>) -> Vec<Interval> {
    intervals.sort_by_key(|r| r.0);
    let mut merged: Vec<Interval> = Vec::new();
    for (start, end) in intervals {
        if let Some(last) = merged.last_mut() {
            if start <= last.1.saturating_add(1) {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    merged
}

/// Converts `ClassRange`s into normalized (sorted, coalesced) intervals.
fn to_intervals(ranges: &[ClassRange]) -> Vec<Interval> {
    sorted_merged(
        ranges
            .iter()
            .map(|r| (r.start as u32, r.end as u32))
            .collect(),
    )
}

/// Converts normalized intervals back into `ClassRange`s, splitting any
/// interval that spans the surrogate gap.
fn intervals_to_ranges(intervals: &[Interval]) -> Vec<ClassRange> {
    let mut out = Vec::new();
    for &(start, end) in intervals {
        push_scalar_range(&mut out, start, end);
    }
    out
}

/// Intersection of two normalized interval sets: `[s, e]` for every
/// overlapping pair, where `s = max(a.start, b.start)` and
/// `e = min(a.end, b.end)`.
///
/// Two-pointer merge over both (sorted, disjoint) inputs: at each step only
/// the interval with the smaller end can possibly overlap anything further
/// along the *other* list, so advancing past whichever of `a[i]`/`b[j]` ends
/// first is always safe and each pointer only ever moves forward — O(len(a)
/// + len(b)) total instead of the O(len(a) * len(b)) nested-scan version.
fn intersect_intervals(a: &[Interval], b: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < a.len() && j < b.len() {
        let (a_start, a_end) = a[i];
        let (b_start, b_end) = b[j];
        let start = a_start.max(b_start);
        let end = a_end.min(b_end);
        if start <= end {
            out.push((start, end));
        }
        if a_end < b_end {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Difference (`a - b`) of two normalized interval sets: every part of `a`
/// not covered by any interval of `b`.
///
/// Two-pointer sweep: `j` tracks the first `b` interval that could still
/// matter and only ever advances, since both inputs are sorted and `cur`
/// (the next uncovered position within the current `a` interval) is
/// non-decreasing across the whole sweep — so no `b` interval is ever
/// revisited once passed. Within one `a` interval, a local copy `k` of `j`
/// walks forward over every `b` interval that overlaps it, emitting the gap
/// before each one; a single `a` interval can this way be split into
/// several output pieces when multiple `b` intervals punch holes in it.
fn difference_intervals(a: &[Interval], b: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    let mut j = 0usize;
    for &(a_start, a_end) in a {
        let mut cur = a_start;
        // Intervals of `b` that ended before this `a` interval starts are
        // done for good (later `a` intervals only start later still).
        while j < b.len() && b[j].1 < cur {
            j += 1;
        }
        let mut k = j;
        while cur <= a_end && k < b.len() && b[k].0 <= a_end {
            let (b_start, b_end) = b[k];
            if b_start > cur {
                out.push((cur, b_start - 1));
            }
            if b_end >= a_end {
                cur = a_end + 1;
                break;
            }
            cur = b_end + 1;
            k += 1;
        }
        if cur <= a_end {
            out.push((cur, a_end));
        }
        j = k;
    }
    out
}

/// Intersection of two character-range sets, normalized before and after.
fn intersect_ranges(a: &[ClassRange], b: &[ClassRange]) -> Vec<ClassRange> {
    let a = to_intervals(a);
    let b = to_intervals(b);
    intervals_to_ranges(&sorted_merged(intersect_intervals(&a, &b)))
}

/// Difference (`a - b`) of two character-range sets, normalized before and
/// after.
fn difference_ranges(a: &[ClassRange], b: &[ClassRange]) -> Vec<ClassRange> {
    let a = to_intervals(a);
    let b = to_intervals(b);
    intervals_to_ranges(&sorted_merged(difference_intervals(&a, &b)))
}

/// Symmetric difference of two character-range sets: `(a - b) ∪ (b - a)`,
/// normalized before and after.
fn symmetric_difference_ranges(a: &[ClassRange], b: &[ClassRange]) -> Vec<ClassRange> {
    let a = to_intervals(a);
    let b = to_intervals(b);
    let mut sym = difference_intervals(&a, &b);
    sym.extend(difference_intervals(&b, &a));
    intervals_to_ranges(&sorted_merged(sym))
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

    /// Whether `ranges` (not assumed normalized) contains code point `cp`,
    /// checked directly with `ClassRange::contains` rather than via any of
    /// the interval machinery under test.
    fn ranges_contain_cp(ranges: &[ClassRange], cp: u32) -> bool {
        match char::from_u32(cp) {
            Some(c) => ranges.iter().any(|r| r.contains(c)),
            None => false,
        }
    }

    /// Brute-force equivalence check: for every code point in `0..=0x2FF`
    /// (ASCII, Latin-1 and the surrogate-free low range beyond it),
    /// `intersect_ranges`, `difference_ranges`, and `symmetric_difference_ranges`
    /// must each produce a set whose membership matches the naive boolean
    /// combination of `a`'s and `b`'s membership directly. This is the real
    /// gate on the two-pointer `intersect_intervals`/`difference_intervals`
    /// rewrite: any off-by-one in the sweep shows up as a membership
    /// mismatch somewhere in the window.
    #[test]
    fn set_ops_match_naive_membership_oracle() {
        const WINDOW: std::ops::RangeInclusive<u32> = 0u32..=0x2FF;

        let cases: &[(&str, Vec<ClassRange>, Vec<ClassRange>)] = &[
            (
                "disjoint",
                vec![ClassRange::new('a', 'f')],
                vec![ClassRange::new('x', 'z')],
            ),
            (
                "b fully contains a",
                vec![ClassRange::new('m', 'p')],
                vec![ClassRange::new('a', 'z')],
            ),
            (
                "a fully contains b",
                vec![ClassRange::new('a', 'z')],
                vec![ClassRange::new('m', 'p')],
            ),
            (
                "partial overlap, a starts first",
                vec![ClassRange::new('a', 'm')],
                vec![ClassRange::new('g', 'z')],
            ),
            (
                "partial overlap, b starts first",
                vec![ClassRange::new('g', 'z')],
                vec![ClassRange::new('a', 'm')],
            ),
            (
                "identical sets",
                vec![ClassRange::new('a', 'z'), ClassRange::new('0', '9')],
                vec![ClassRange::new('a', 'z'), ClassRange::new('0', '9')],
            ),
            ("a empty", vec![], vec![ClassRange::new('a', 'z')]),
            ("b empty", vec![ClassRange::new('a', 'z')], vec![]),
            ("both empty", vec![], vec![]),
            (
                "adjacent but not overlapping",
                vec![ClassRange::new('a', 'c')],
                vec![ClassRange::new('d', 'f')],
            ),
            (
                "multiple holes punched in one a interval",
                vec![ClassRange::new('\0', '\u{FF}')],
                vec![
                    ClassRange::new('\u{10}', '\u{1F}'),
                    ClassRange::new('\u{40}', '\u{5F}'),
                    ClassRange::new('\u{90}', '\u{9F}'),
                ],
            ),
        ];

        for (label, a, b) in cases {
            let inter = intersect_ranges(a, b);
            let diff = difference_ranges(a, b);
            let sym = symmetric_difference_ranges(a, b);

            for cp in WINDOW {
                let in_a = ranges_contain_cp(a, cp);
                let in_b = ranges_contain_cp(b, cp);

                let expect_inter = in_a && in_b;
                let expect_diff = in_a && !in_b;
                let expect_sym = in_a != in_b;

                assert_eq!(
                    ranges_contain_cp(&inter, cp),
                    expect_inter,
                    "{label}: intersect mismatch at U+{cp:04X}"
                );
                assert_eq!(
                    ranges_contain_cp(&diff, cp),
                    expect_diff,
                    "{label}: difference mismatch at U+{cp:04X}"
                );
                assert_eq!(
                    ranges_contain_cp(&sym, cp),
                    expect_sym,
                    "{label}: symmetric_difference mismatch at U+{cp:04X}"
                );
            }
        }
    }
}
