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

fn atom(r: &mut R) -> String {
    format!("{}{}", ATOMS[r.b(ATOMS.len())], QUANT[r.b(QUANT.len())])
}
fn seq(r: &mut R) -> String {
    (0..1 + r.b(3)).map(|_| atom(r)).collect()
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
}
