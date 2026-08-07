//! Escape-sequence lexing: `\d`, `\n`, `\xHH`, `\u{…}`, `\p{…}`, backrefs, and
//! escaped literals.

use super::scanner::Lexer;
use super::token::{EscapeKind, TokenKind};
use crate::error::{Error, ErrorKind, Result, Span};

impl Lexer<'_> {
    /// Lexes an escape sequence.
    pub(super) fn lex_escape(&mut self, start: usize) -> Result<TokenKind> {
        let c = match self.next_char() {
            Some((_, c)) => c,
            None => {
                return Err(Error::with_span(
                    ErrorKind::UnexpectedEof,
                    self.src,
                    Span::point(start),
                ));
            }
        };

        let escape = match c {
            // Character classes
            'd' => EscapeKind::Digit,
            'D' => EscapeKind::NotDigit,
            'w' => EscapeKind::Word,
            'W' => EscapeKind::NotWord,
            's' => EscapeKind::Whitespace,
            'S' => EscapeKind::NotWhitespace,
            // \h/\H (horizontal whitespace) are always Unicode-correct, the
            // same way \s/\S are — see `HirTranslator::translate_perl_class`.
            'h' => EscapeKind::HorizontalWhitespace,
            'H' => EscapeKind::NotHorizontalWhitespace,

            // Anchors
            'b' => EscapeKind::WordBoundary,
            'B' => EscapeKind::NotWordBoundary,
            'A' => EscapeKind::StartOfInput,
            'z' => EscapeKind::EndOfInput,
            'Z' => EscapeKind::EndOfInputBeforeNewline,

            // Special characters
            'n' => EscapeKind::Newline,
            'r' => EscapeKind::CarriageReturn,
            't' => EscapeKind::Tab,
            'f' => EscapeKind::FormFeed,
            'v' => EscapeKind::VerticalTab,
            '0' => EscapeKind::Null,
            // Alert/bell and escape, as in PCRE, Perl, Java and the `regex`
            // crate. Both are plain characters, so they are also valid inside a
            // character class.
            'a' => EscapeKind::Literal('\u{07}'),
            'e' => EscapeKind::Literal('\u{1b}'),

            // One extended grapheme cluster. Expanded by the HIR builder into
            // the UAX #29 boundary rules.
            'X' => EscapeKind::GraphemeCluster,

            // Any Unicode line-break sequence, matched as a single unit
            // (`\r\n`, or one of LF/VT/FF/CR/NEL/LS/PS). Bare only; rejected
            // inside a character class in `Parser::parse_class_item` since a
            // multi-character escape cannot be a class member.
            'R' => EscapeKind::LineBreak,

            // Any code point except line feed — like `.` with dot-all off,
            // but unaffected by the `s` flag. Bare only, same class
            // restriction as `\R`. The named-character form `\N{NAME}` is not
            // supported (it needs the full Unicode character-name database),
            // so it is rejected here rather than being misparsed as bare `\N`
            // followed by a literal `{`.
            'N' => {
                if self.peek_char() == Some('{') {
                    return Err(Error::with_span(
                        ErrorKind::NamedUnicodeCharacterNotSupported,
                        self.src,
                        Span::new(start, self.pos),
                    ));
                }
                EscapeKind::AnyExceptNewline
            }

            // Hex escape: \xHH (exactly two digits) or \x{H..HHHHHH} (braced,
            // 1-6 digits, same syntax and range as \u{...}).
            'x' => {
                let ch = self.lex_hex_escape(start)?;
                EscapeKind::Hex(ch)
            }

            // Unicode escape
            'u' => {
                let ch = self.lex_unicode_escape(start)?;
                EscapeKind::Unicode(ch)
            }

            // Control escape: \cX is the ASCII code of X with bit 0x40
            // cleared (\cA is U+0001, \cZ is U+001A). A plain character
            // escape, so it is also valid inside a character class.
            'c' => {
                let ch = self.lex_control_escape(start)?;
                EscapeKind::Literal(ch)
            }

            // Unicode property
            'p' => {
                let name = self.lex_unicode_property(start)?;
                EscapeKind::UnicodeProperty(name)
            }

            // Negated Unicode property
            'P' => {
                let name = self.lex_unicode_property(start)?;
                EscapeKind::NotUnicodeProperty(name)
            }

            // Backreference
            c if c.is_ascii_digit() && c != '0' => {
                let n = self.lex_backref(c)?;
                EscapeKind::Backref(n)
            }

            // Named backreference: \k<name>, \k{name}, or \k'name'.
            'k' => {
                let name = self.lex_named_backref(start)?;
                EscapeKind::NamedBackref(name)
            }

            // Escaping any non-alphanumeric ASCII character yields that
            // character. Patterns written for other engines rely on this far
            // beyond the metacharacters that strictly need it — `\ `, `\"`,
            // `\@`, `\#` and friends are all common in the wild.
            c if c.is_ascii() && !c.is_ascii_alphanumeric() => EscapeKind::Literal(c),

            // An unassigned ASCII letter stays an error rather than decaying to
            // the letter itself, so that giving it a meaning later is not a
            // silent behavior change. Non-ASCII needs no escaping to begin with.
            _ => {
                return Err(Error::with_span(
                    ErrorKind::InvalidEscape(c),
                    self.src,
                    Span::new(start, self.pos),
                ));
            }
        };

        Ok(TokenKind::Escape(escape))
    }

    /// Lexes a hex escape: `\xHH` (exactly two digits) or the braced form
    /// `\x{H..HHHHHH}` (1-6 digits), which shares its scanning with `\u{...}`
    /// via [`Lexer::lex_braced_hex`] — same digit count, same rejection of
    /// surrogates and out-of-range values through `char::from_u32`.
    fn lex_hex_escape(&mut self, start: usize) -> Result<char> {
        if self.peek_char() == Some('{') {
            return self.lex_braced_hex(start, ErrorKind::InvalidHexEscape);
        }

        let mut value = 0u32;

        for _ in 0..2 {
            let (_, c) = self.next_char().ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidHexEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })?;

            let digit = c.to_digit(16).ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidHexEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })?;

            value = value * 16 + digit;
        }

        char::from_u32(value).ok_or_else(|| {
            Error::with_span(
                ErrorKind::InvalidHexEscape,
                self.src,
                Span::new(start, self.pos),
            )
        })
    }

    /// Lexes a unicode escape (\u{HHHH} or \uHHHH).
    fn lex_unicode_escape(&mut self, start: usize) -> Result<char> {
        if self.peek_char() == Some('{') {
            self.lex_braced_hex(start, ErrorKind::InvalidUnicodeEscape)
        } else {
            // \uHHHH format
            let mut value = 0u32;

            for _ in 0..4 {
                let (_, c) = self.next_char().ok_or_else(|| {
                    Error::with_span(
                        ErrorKind::InvalidUnicodeEscape,
                        self.src,
                        Span::new(start, self.pos),
                    )
                })?;

                let digit = c.to_digit(16).ok_or_else(|| {
                    Error::with_span(
                        ErrorKind::InvalidUnicodeEscape,
                        self.src,
                        Span::new(start, self.pos),
                    )
                })?;

                value = value * 16 + digit;
            }

            char::from_u32(value).ok_or_else(|| {
                Error::with_span(
                    ErrorKind::InvalidUnicodeEscape,
                    self.src,
                    Span::new(start, self.pos),
                )
            })
        }
    }

    /// Lexes a braced hex payload `{H..HHHHHH}` (1-6 hex digits), already
    /// positioned just before the `{`. Shared by `\u{...}` and `\x{...}` — the
    /// only difference between the two escapes is which `ErrorKind` a
    /// malformed payload reports.
    ///
    /// Rejects an empty brace, a non-hex character before the closing brace,
    /// more than 6 digits, an unterminated brace (EOF before `}`), and any
    /// value that is not a valid Unicode scalar value — `char::from_u32`
    /// rejects both values above U+10FFFF and the surrogate range
    /// U+D800..=U+DFFF, so both escapes reject surrogates identically.
    fn lex_braced_hex(&mut self, start: usize, err: ErrorKind) -> Result<char> {
        self.next_char(); // consume '{'

        let mut value = 0u32;
        let mut count = 0;

        loop {
            match self.peek_char() {
                Some('}') => {
                    self.next_char();
                    break;
                }
                Some(c) if c.is_ascii_hexdigit() => {
                    self.next_char();
                    let digit = c.to_digit(16).unwrap();
                    value = value * 16 + digit;
                    count += 1;
                    if count > 6 {
                        return Err(Error::with_span(err, self.src, Span::new(start, self.pos)));
                    }
                }
                _ => {
                    return Err(Error::with_span(err, self.src, Span::new(start, self.pos)));
                }
            }
        }

        if count == 0 {
            return Err(Error::with_span(err, self.src, Span::new(start, self.pos)));
        }

        char::from_u32(value)
            .ok_or_else(|| Error::with_span(err, self.src, Span::new(start, self.pos)))
    }

    /// Lexes a control escape (\cX): the ASCII code of `X` with bit 0x40
    /// cleared, accepting either case (`\ca` == `\cA` == U+0001). Rejects `\c`
    /// at end of pattern and `\c` followed by a non-letter.
    fn lex_control_escape(&mut self, start: usize) -> Result<char> {
        let (_, c) = self.next_char().ok_or_else(|| {
            Error::with_span(
                ErrorKind::InvalidControlEscape,
                self.src,
                Span::new(start, self.pos),
            )
        })?;

        if !c.is_ascii_alphabetic() {
            return Err(Error::with_span(
                ErrorKind::InvalidControlEscape,
                self.src,
                Span::new(start, self.pos),
            ));
        }

        let code = (c.to_ascii_uppercase() as u8) & 0x1F;
        Ok(code as char)
    }

    /// Lexes a Unicode property escape.
    ///
    /// Supports two syntaxes:
    /// - `\p{Name}` or `\P{Name}` - full property name in braces
    /// - `\pL` or `\PL` - single-letter shorthand for general categories
    fn lex_unicode_property(&mut self, start: usize) -> Result<String> {
        match self.peek_char() {
            Some('{') => {
                // Brace syntax: \p{Name}
                self.next_char(); // consume '{'

                let mut name = String::new();

                // Read property name until closing brace
                loop {
                    match self.next_char() {
                        Some((_, '}')) => break,
                        Some((_, c)) if c.is_alphanumeric() || c == '_' || c == '-' => {
                            name.push(c);
                        }
                        _ => {
                            return Err(Error::with_span(
                                ErrorKind::InvalidUnicodeProperty,
                                self.src,
                                Span::new(start, self.pos),
                            ));
                        }
                    }
                }

                if name.is_empty() {
                    return Err(Error::with_span(
                        ErrorKind::InvalidUnicodeProperty,
                        self.src,
                        Span::new(start, self.pos),
                    ));
                }

                Ok(name)
            }
            Some(c) if c.is_ascii_alphabetic() => {
                // Shorthand syntax: \pL (single letter)
                self.next_char(); // consume the letter
                Ok(c.to_string())
            }
            _ => Err(Error::with_span(
                ErrorKind::InvalidUnicodeProperty,
                self.src,
                Span::new(start, self.pos),
            )),
        }
    }

    /// Lexes a POSIX bracket-expression class (`[:alpha:]`, `[:^alpha:]`),
    /// already past the opening `[` — the caller peeked the following `:`
    /// before dispatching here. Once `[:` has been seen the syntax is
    /// committed: anything short of a well-formed `[:name:]` (or `[:^name:]`)
    /// is an error, never a silent fallback to a literal `[`.
    pub(super) fn lex_posix_class(&mut self, start: usize) -> Result<TokenKind> {
        self.next_char(); // consume ':'

        let negated = if self.peek_char() == Some('^') {
            self.next_char();
            true
        } else {
            false
        };

        let mut name = String::new();
        while let Some(c) = self.peek_char() {
            if c.is_ascii_alphabetic() {
                self.next_char();
                name.push(c);
            } else {
                break;
            }
        }

        if name.is_empty() {
            return Err(Error::with_span(
                ErrorKind::InvalidPosixClass,
                self.src,
                Span::new(start, self.pos),
            ));
        }

        match (self.next_char(), self.next_char()) {
            (Some((_, ':')), Some((_, ']'))) => Ok(TokenKind::PosixClass { name, negated }),
            _ => Err(Error::with_span(
                ErrorKind::InvalidPosixClass,
                self.src,
                Span::new(start, self.pos),
            )),
        }
    }

    /// Lexes a named backreference: `\k<name>`, `\k{name}`, or `\k'name'`,
    /// already positioned just past the `k`. Returns the raw name; the
    /// parser resolves it to a capture index, since only the parser tracks
    /// which names have been defined.
    ///
    /// The name itself uses the same shape as a `(?<name>...)` group name —
    /// an arbitrary first character followed by `read_ident_rest`'s
    /// alphanumeric/underscore run, shared with every named-group spelling
    /// (see `Parser::parse_group_name`) — except the first character may not
    /// be a digit, matching how a leading digit lexes to `Digit` rather than
    /// `Literal` for the group-name spellings and so is rejected there too.
    ///
    /// Rejects: no delimiter or an unrecognized one, an empty name, an
    /// unterminated form (EOF before the closing delimiter), and a
    /// mismatched delimiter pair (e.g. `\k<name}`).
    fn lex_named_backref(&mut self, start: usize) -> Result<String> {
        let close = match self.next_char() {
            Some((_, '<')) => '>',
            Some((_, '{')) => '}',
            Some((_, '\'')) => '\'',
            _ => {
                return Err(Error::with_span(
                    ErrorKind::InvalidNamedBackref,
                    self.src,
                    Span::new(start, self.pos),
                ));
            }
        };

        let first = match self.next_char() {
            Some((_, c)) if c != close && !c.is_ascii_digit() => c,
            _ => {
                return Err(Error::with_span(
                    ErrorKind::InvalidNamedBackref,
                    self.src,
                    Span::new(start, self.pos),
                ));
            }
        };
        let rest = self.read_ident_rest();
        let name = format!("{first}{rest}");

        match self.next_char() {
            Some((_, c)) if c == close => Ok(name),
            _ => Err(Error::with_span(
                ErrorKind::InvalidNamedBackref,
                self.src,
                Span::new(start, self.pos),
            )),
        }
    }

    /// Lexes a backreference (\1, \12, etc.).
    fn lex_backref(&mut self, first: char) -> Result<u32> {
        let mut n = first.to_digit(10).unwrap();

        // Consume additional digits
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                self.next_char();
                n = n * 10 + c.to_digit(10).unwrap();
            } else {
                break;
            }
        }

        Ok(n)
    }
}
