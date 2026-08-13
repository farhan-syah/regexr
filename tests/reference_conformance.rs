//! Conformance tests against the executable spec (`regexr::reference`).
//!
//! The reference matcher defines regexr's canonical leftmost-first/PCRE
//! semantics and is validated against Python `regex` separately. Here we assert
//! that the real engines (both the JIT build and the interpreter build) agree
//! with the reference on a curated set of patterns — in particular the codepoint
//! / alternation / backtracking constructs whose bugs were fixed, plus the real
//! tiktoken split patterns. This locks those fixes in against regression.
//!
//! NOTE: regexr's engines are not yet fully conformant across *all* constructs
//! (lazy quantifiers combined with bounded repeats, `$`-before-newline, and DFA
//! alternation priority are known-divergent and tracked separately). This test
//! deliberately covers the conformant subset; `examples/diff_oracle.rs` is the
//! broad differential fuzzer used to drive the remaining work to green.

use regexr::hir::translate;
use regexr::parser::parse;

const CL100K: &str = r"'(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s+$|\s*[\r\n]|\s+(?!\S)|\s";
const O200K: &str = r"[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]*[\p{Ll}\p{Lm}\p{Lo}\p{M}]+(?i:'s|'t|'re|'ve|'m|'ll|'d)?|[^\r\n\p{L}\p{N}]?[\p{Lu}\p{Lt}\p{Lm}\p{Lo}\p{M}]+[\p{Ll}\p{Lm}\p{Lo}\p{M}]*(?i:'s|'t|'re|'ve|'m|'ll|'d)?|\p{N}{1,3}| ?[^\s\p{L}\p{N}]+[\r\n/]*|\s*[\r\n]+|\s+(?!\S)|\s+";

/// Patterns whose engine output must equal the reference (fixed/known-good).
const PATTERNS: &[&str] = &[
    // Codepoint greedy backtracking in alternation arms (fixed).
    r"\s+\s+",
    r"\s+(?!\S)",
    r"\s+(?!\S)|\s",
    r"\s*[\r\n]",
    r"\s+$",
    // `$`/`\Z` before-newline; `\z` strict. (anchor-eval across engines)
    r"world$",
    r"^[a-z]+$",
    r"\d{1,3}$",
    r"x\Z",
    r"x\z",
    r"[a-z]+\n$",
    r"\p{L}+$",
    r"[^\s]+$",
    r"\p{N}+$",
    // Codepoint-boundary match starts: a nullable construct must not match inside
    // a multi-byte codepoint.
    r"\p{L}*$",
    r"[^\s]*$",
    r"\p{L}+\p{N}",
    r"[^\s]+(?!a)",
    r"\s+(?=\S)",
    // Byte greedy + codepoint lookahead (fixed).
    r"a+(?!\S)",
    r"\w+\s+error\s+\w+",
    // Nullable routing (fixed): a nullable pattern matches empty at pos 0.
    r"\s*",
    r" *",
    // Non-greedy / lazy quantifiers, now correct via the unified-priority PikeVm.
    r"\S+?x",
    r"\s+?\S",
    r"\p{L}+?\p{N}",
    r"\s*?[\r\n]",
    r"[^\s]*?\S",
    r"\p{L}*?\p{L}",
    // Bounded repeat followed by a lazy quantifier (was the dominant lazy bug).
    r"\p{L}{1,3}\s+?\S",
    // Backreferences (NFA continuation-state fix).
    r"(\p{L}+)\s\1",
    r"(\S)\1",
    // Lookbehind whose inner pattern can end at more than one position: the
    // assertion holds if *any* of those ends lands on the current position.
    r"(?<=..?)x",
    r"(?<!..?)x",
    r"(?<=\w\w?)x",
    r"(?<!\w\w?)x",
    r"(?<=a|\sa)x",
    r"(?<!a|\sa)x",
    r"(?<=\p{N}{1,3})x",
    r"(?<!\p{N}{1,3})x",
    r"(?<=a.*)x",
    r"(?<!a.*)x",
    // Zero-width assertions inside a lookaround, in every position — leading,
    // interior and trailing.
    r"(?<=a\b)x",
    r"(?<=a\B)x",
    r"(?<!a\b)x",
    r"(?<=a$)x",
    r"(?<!a$)x",
    r"(?<=^a)x",
    r"(?<=\ba)x",
    r"(?<=a\bb)x",
    r"(?<=\w\b)x",
    r"x(?=a\b)",
    r"x(?=a\B)",
    r"x(?!a\b)",
    r"x(?=a$)",
    r"x(?!a$)",
    r"x(?=\ba)",
    r"\w(?=\w\b)",
    // A quantifier splits into "took some"/"took none"; every assertion after
    // it belongs to both arms.
    r"\p{L}*(?=\S)(?=\p{L})",
    r"[^\s]*(?!\S)(?=a.*)",
    r"\p{N}*(?=a)(?=\S)",
    r"\p{L}*\b\b",
    // Anchors and boundaries inside a lookaround read absolute position, which
    // a search resuming at a later candidate must not hide.
    r"(?=^)x",
    r"(?!^)x",
    r"(?=\b)x",
    r"(?=\B)x",
    r"(?=^\p{L})x",
    r"(?!^\p{L})x",
    // A byte-class quantifier walking the same span a codepoint class crosses
    // in one stride: both threads must stay comparable on priority.
    r"[^\s]*\p{N}",
    r"[^\s]*\p{L}",
    r"\S*\p{N}",
    r"\S+\p{N}+",
    r"[^\s]+[^\r\n\p{L}\p{N}]*",
    r"\S{1,3}\p{L}",
    r"[^\s]{1,3}\p{N}?",
    // `EagerDfa`'s metered word-boundary loop hands off to a fresh `LazyDfa`
    // once its scan budget is exceeded (see `tests/bounded_execution.rs` for
    // the long-input case that actually exercises the give-up); this keeps
    // the handoff's output checked against the reference on ordinary-sized
    // inputs too, matching and non-matching alike.
    r"\b[a-y ]+\d",
    // The real tiktoken split patterns.
    CL100K,
    O200K,
];

/// Inputs exercising ASCII, Unicode whitespace, CJK, combining marks, emoji,
/// digits, punctuation and newlines.
fn inputs() -> Vec<String> {
    let alphabet: &[char] = &[
        'a',
        'Z',
        '0',
        '9',
        ' ',
        '\n',
        '\r',
        '\t',
        '.',
        ',',
        '!',
        '\'',
        '/',
        '\u{00A0}',
        '\u{2003}',
        '\u{3000}',
        '\u{00E9}',
        '\u{4E2D}',
        '\u{1F600}',
        '\u{00B2}',
        'x',
        'e',
        'r',
    ];
    // Deterministic xorshift so the test is reproducible.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = vec![
        String::new(),
        " ".into(),
        "  \n".into(),
        "   x".into(),
        "hello   world".into(),
        "中文 \n  test".into(),
    ];
    for _ in 0..4000 {
        let len = (next() % 24) as usize;
        let s: String = (0..len)
            .map(|_| alphabet[(next() as usize) % alphabet.len()])
            .collect();
        out.push(s);
    }
    out
}

#[test]
fn engines_match_reference() {
    let inputs = inputs();
    let mut failures = Vec::new();

    for &pat in PATTERNS {
        let hir = parse(pat)
            .and_then(|ast| translate(&ast))
            .expect("pattern should compile");
        let ncaps = hir.props.capture_count as usize;
        let jit = regexr::RegexBuilder::new(pat).jit(true).build().unwrap();
        let interp = regexr::RegexBuilder::new(pat).jit(false).build().unwrap();

        for s in &inputs {
            let bytes = s.as_bytes();
            let expected = regexr::reference::find(&hir.expr, ncaps, bytes);
            let j = jit.find(s).map(|m| (m.start(), m.end()));
            let n = interp.find(s).map(|m| (m.start(), m.end()));
            if j != expected || n != expected {
                failures.push(format!(
                    "pat={pat:?} input={s:?}: ref={expected:?} jit={j:?} interp={n:?}"
                ));
                if failures.len() > 20 {
                    break;
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "engine/reference divergences ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
