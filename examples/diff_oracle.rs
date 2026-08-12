//! Differential oracle: compares regexr's engines (JIT build and interpreter
//! build) against the reference spec matcher (`regexr::reference`) over random
//! patterns and inputs. Emits divergence records for offline cross-checking
//! against Python `regex`, and a summary.
//!
//! Usage: diff_oracle [iters] [seed]
//! Emits lines: REC <pattern> <input-hex> <ref> <jit> <nojit> <jit-engine>

use regexr::hir::translate;
use regexr::parser::parse;

struct R(u64);
impl R {
    fn n(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn b(&mut self, n: usize) -> usize {
        (self.n() % n as u64) as usize
    }
}

// Codepoint-clean construct set: Unicode codepoint classes plus ASCII-only byte
// classes/literals, where regexr's byte semantics coincide with Python/PCRE's
// codepoint semantics. (`.`/`\d`/`\w`/`\D`/`\W` and non-ASCII byte classes are a
// separate byte-vs-codepoint design question, excluded here.)
const ATOMS: &[&str] = &[
    r"\s",
    r"\S",
    r"\p{L}",
    r"\p{N}",
    r"[^\s]",
    r"[^\r\n\p{L}\p{N}]",
    r"[^\s\p{L}\p{N}]",
    r"[\r\n]",
    r"a",
    r"x",
    r" ",
];
const QUANT: &[&str] = &["", "+", "*", "?", "{1,3}", "+?", "*?"];

/// Zero-width assertions to place *inside* a lookaround, where the linear step
/// extractors have to either represent them or refuse. A trailing one lands on
/// the inner NFA's match state, the position a walk that stops there is most
/// likely to skip.
const INNER_ASSERT: &[&str] = &["", "", "", r"\b", r"\B", "$", "^", r"\Z"];

/// Inner patterns for a lookbehind, spanning the shapes the engines route
/// differently: fixed width, variable width via `?`, alternation of unequal
/// lengths, bounded and unbounded repetition.
const LOOKBEHIND_INNER: &[&str] = &[
    "a",
    "ab",
    r"\p{L}",
    r"[a-z]",
    "a?",
    "..?",
    r"\p{L}\p{L}?",
    r"a|xa",
    r"\p{N}{1,3}",
    "a.*",
    "[α-ω]",
];

fn atom(r: &mut R) -> String {
    format!("{}{}", ATOMS[r.b(ATOMS.len())], QUANT[r.b(QUANT.len())])
}
fn seq(r: &mut R) -> String {
    (0..1 + r.b(3)).map(|_| atom(r)).collect()
}

/// A lookaround wrapping one of the inner shapes above, optionally with a
/// trailing or leading assertion inside it.
fn lookaround(r: &mut R) -> String {
    let inner = LOOKBEHIND_INNER[r.b(LOOKBEHIND_INNER.len())];
    let trailing = INNER_ASSERT[r.b(INNER_ASSERT.len())];
    let leading = INNER_ASSERT[r.b(INNER_ASSERT.len())];
    let body = format!("{leading}{inner}{trailing}");
    match r.b(4) {
        0 => format!("(?<={body})"),
        1 => format!("(?<!{body})"),
        2 => format!("(?={body})"),
        _ => format!("(?!{body})"),
    }
}

fn pat(r: &mut R) -> String {
    let mut p = String::new();
    if r.b(5) == 0 {
        p.push('^');
    }
    let alts: Vec<String> = (0..1 + r.b(3))
        .map(|_| {
            let mut a = seq(r);
            match r.b(7) {
                1 => a.push_str(r"(?!\S)"),
                2 => a.push_str(r"(?=\S)"),
                _ => {}
            }
            // A lookaround before the sequence exercises lookbehind against real
            // left context, and puts an inner assertion where a linear walk ends.
            if r.b(4) == 0 {
                a.insert_str(0, &lookaround(r));
            }
            if r.b(6) == 0 {
                a.push_str(&lookaround(r));
            }
            a
        })
        .collect();
    p.push_str(&alts.join("|"));
    if r.b(5) == 0 {
        p.push('$');
    }
    p
}

const A: &[char] = &[
    'a', 'b', 'x', 'Z', '0', '9', ' ', ' ', '\n', '\r', '\t', '.', ',', '\u{00A0}', '\u{2003}',
    '\u{3000}', 'é', '中', '😀', '½', '²',
];

fn fmt(o: Option<(usize, usize)>) -> String {
    o.map(|(s, e)| format!("{},{}", s, e))
        .unwrap_or_else(|| "none".into())
}

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    let seed: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0xC0FFEE);
    let mut rng = R(seed);
    let mut jit_bad = 0usize;
    let mut nojit_bad = 0usize;
    let mut checked = 0usize;
    let mut emitted = 0usize;

    for _ in 0..iters {
        let p = pat(&mut rng);
        // Reference needs the Hir; skip patterns regexr can't parse/translate.
        let hir = match parse(&p).and_then(|ast| translate(&ast)) {
            Ok(h) => h,
            Err(_) => continue,
        };
        let jit = match regexr::RegexBuilder::new(&p).jit(true).build() {
            Ok(x) => x,
            Err(_) => continue,
        };
        let nojit = match regexr::RegexBuilder::new(&p).jit(false).build() {
            Ok(x) => x,
            Err(_) => continue,
        };
        let ncaps = hir.props.capture_count as usize;

        for _ in 0..5 {
            let len = rng.b(18);
            let s: String = (0..len).map(|_| A[rng.b(A.len())]).collect();
            let bytes = s.as_bytes();
            let rf = regexr::reference::find(&hir.expr, ncaps, bytes);
            let j = jit.find(&s).map(|m| (m.start(), m.end()));
            let nj = nojit.find(&s).map(|m| (m.start(), m.end()));
            checked += 1;
            // Unbiased sample for validating the reference itself against Python.
            if rng.b(40) == 0 {
                let hx: String = bytes.iter().map(|x| format!("{:02x}", x)).collect();
                println!("VAL\t{}\t{}\t{}", p, hx, fmt(rf));
            }
            let jb = j != rf;
            let nb = nj != rf;
            if jb {
                jit_bad += 1;
            }
            if nb {
                nojit_bad += 1;
            }
            if (jb || nb) && emitted < 600 {
                emitted += 1;
                let hx: String = bytes.iter().map(|x| format!("{:02x}", x)).collect();
                println!(
                    "REC\t{}\t{}\t{}\t{}\t{}\t{}",
                    p,
                    hx,
                    fmt(rf),
                    fmt(j),
                    fmt(nj),
                    jit.engine_name()
                );
            }
        }
    }
    eprintln!(
        "checked={checked} jit_disagrees_with_ref={jit_bad} interp_disagrees_with_ref={nojit_bad}"
    );

    // Exit non-zero on any divergence so this is usable as a CI gate rather than
    // something a human has to read the output of.
    if jit_bad > 0 || nojit_bad > 0 {
        std::process::exit(1);
    }
}
