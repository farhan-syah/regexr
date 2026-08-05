# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each release must have a non-empty section here before it can be tagged — `.github/workflows/release-validate.yml` refuses a tag whose version has no entry, and the GitHub Release body is this file's section for that version.

## [0.1.5] - Unreleased

### Added

- `escape()` — quote a string so it matches literally, for building patterns from untrusted or user-supplied text.

### Fixed

- Matching no longer panics when a match boundary falls inside a multi-byte codepoint. The public API now reports byte offsets that are always valid UTF-8 boundaries.
- Left context is preserved across a resumed search, so look-behind assertions and `^`/`\b` decide against the real preceding text rather than the start of the resumed slice. This affected both `find_at`-style resumption and iteration.

### Performance

- Unanchored capture extraction in the PikeVM runs as a single pass instead of restarting the VM at every start offset.
- Unanchored Shift-Or scans are bounded by a single-pass existence check, so a non-matching haystack is rejected once rather than re-scanned per candidate start.

### Changed

- Minimum supported Rust version is declared as 1.83 in `Cargo.toml`. It was already the version CI checked; it is now a resolver constraint and a build-time error rather than a CI-only convention.
- Dependencies moved to their current releases: `dynasm`/`dynasmrt` 5.1, `memchr` 2.8, `aho-corasick` 1.1, and — for benchmarks only — `regex` 1.13 and `fancy-regex` 0.19.
- The published crate no longer carries repository scaffolding — README screenshots, the raw UCD input files, CI config, `docs/` — that nothing in the build reads. The packaged archive drops from 1.2 MiB to 410 KiB compressed. The README now references its images and its `docs/` pages by absolute GitHub URL, so they render on crates.io and docs.rs from the repository rather than from a copy inside every downloaded crate.

## [0.1.4] - 2026-06-06

### Fixed

- Leftmost-first branch priority is enforced across engines, so an alternation picks the first branch that can match rather than whichever engine-internal order happened to win.
- `$` follows PCRE/Python semantics consistently, and end-anchor handling agrees between the interpreted engines and the JIT.
- aarch64 JIT: end-of-text validation, unanchored retry, and greedy backtracking corrected.
- `TaggedNfa` gained a per-kind assertion-tally soundness guard, closing a case where an assertion count could be trusted after it went stale.
- Parser: scoped inline flags, nested character classes, and Unicode `\s` are handled.

### Added

- A reference matcher and a differential conformance suite: every engine is now checked against one independent implementation on the same inputs, so an engine-specific divergence fails the suite instead of hiding behind engine selection.

## [0.1.3] - 2026-06-04

### Fixed

- aarch64 JIT clippy warnings across the JIT backends.

### Changed

- Dependency bumps: `dynasm`/`dynasmrt` 5.0, `criterion` 0.8, `fancy-regex` 0.18.

## [0.1.2] - 2026-03-15

### Fixed

- Missing `.arch x64` directives in the dynasm blocks that implement the calling convention, which made x86-64 codegen depend on the host's default architecture rather than the target's.

## [0.1.1] - 2026-03-15

### Fixed

- Missing `.arch aarch64` directives in every dynasm block, so ARM64 JIT compilation no longer depends on the assembler's default architecture.

## [0.1.0] - 2026-03-12

The first release.

### Added

- **Multiple execution backends, selected per pattern** — Shift-Or (bit-parallel, small patterns), Wide Shift-Or (65–256 positions), eager DFA (anchors and word boundaries), lazy DFA (general), backtracking VM (backreferences), and PikeVM (lookaround, non-greedy).
- **JIT compilation** for x86-64 and aarch64 behind the `jit` feature: a backtracking JIT for backreferences, a tagged-NFA JIT at parity with the interpreter for lookaround and non-greedy quantifiers, a Shift-Or JIT for alternation-led patterns, and a DFA JIT with SIMD prefiltering. Windows x64 ABI supported.
- **SIMD literal search** (`simd`, on by default): AVX2 `memchr` family and Teddy multi-literal matching.
- **Unicode support** generated from the UCD — general categories, scripts, and properties, with a `CodepointClass` instruction so large classes do not explode the DFA.
- **Lookaround and backreferences**, which the `regex` crate does not offer.
- Dual MIT / Apache-2.0 licensing.
