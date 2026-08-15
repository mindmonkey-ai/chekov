# chekov — project-local standards overrides (§12)

Authoritative baseline: `~/.claude/code-review/coding-standards.md` (§1–§18
core + annex §C Rust + §D tooling). This file records the deviations this
repo is entitled to, per §12.

## Overrides

1. **Committed-red TDD history.** The bootstrap prompt (acceptance criterion 1)
   requires each module's tests to be authored failing and committed before the
   implementation commit. This is not a core-standards rule; §9's
   no-unapproved-commits gate was satisfied by the user approving the bootstrap
   plan on 2026-08-15. Red commits are labeled `test(<module>): red` and are
   expected to fail `make test` (never `make lint`).
2. **Package/binary name split.** crates.io `chekov` is taken; the package is
   `chekov-mac`, the binary and library are `chekov` (bootstrap prompt §2.7).
3. **Completions at install time, not build time.** `build.rs` cannot import
   the crate's own clap tree without a dependency cycle, so `make install` runs
   the hidden `chekov completions zsh` subcommand to write `shell/_chekov`.
4. **Library target addition.** The prompt's layout lists only `src/main.rs`;
   a `src/lib.rs` exists so `tests/` can exercise modules without spawning the
   binary. `main.rs` stays within the prompt's 40-line budget.
5. **§3 limits via clippy.toml.** Annex §C does not inherit §3 mechanically;
   this repo encodes §3.2/§3.4/§3.12 as clippy thresholds (see `clippy.toml`).

## Non-negotiables carried from the standards

- `#![forbid(unsafe_code)]` at every crate root (§C.1).
- No `unwrap()`/`expect()` outside tests (§C.2) — release builds abort on
  panic (§D.7), so a surviving unwrap is a process kill.
- `thiserror` in the library, `anyhow` only in `src/main.rs` (§C.3).
- Every serde struct read from disk or network: `deny_unknown_fields` (§C.7).
- Tests mock the `HttpClient` boundary, never internals (§8.2); no network
  and no real llama.cpp in any test (bootstrap prompt §2.4).
