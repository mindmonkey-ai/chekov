# chekov — pushkin-adopted governance (customized)

Repo-level working rules. Precedence: (1) current human instruction, (2) this
file (pushkin-adopted governance), (3) the standards overrides recorded in the
`## §12 overrides` section below, (4) your defaults. This file carries
pushkin's conduct rules and adapts them to chekov's shape (a Rust-only,
single-crate macOS CLI — no schema/contract system, no TS, no SQLite core).

The authoritative standards baseline is
`~/.claude/code-review/coding-standards.md` (§1–§18 core + annex §C Rust + §D
tooling); this file records the deviations this repo is entitled to, per §12.

Rules here are **instruction-layer only**. Everything mechanically enforceable
lives in configs that are law: `clippy.toml` (size gates), `deny.toml`
(supply-chain), crate-root lints (`forbid(unsafe_code)`), `rustfmt.toml`, and
the hooks in `.claude/settings.json`. When a gate blocks you, the gate is right
until a human says otherwise — fix the code, not the gate.

## §12 overrides (carried from chekov's prior AGENTS.md)

Authoritative baseline: `~/.claude/code-review/coding-standards.md`. This
section records the deviations this repo is entitled to, per §12.

1. **Committed-red TDD history.** The bootstrap prompt (acceptance criterion 1)
   requires each module's tests to be authored failing and committed before the
   implementation commit. This is not a core-standards rule; the
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

## Before editing

- Read the actual file before changing it. Never edit from a filename guess.
- Run `git status` first; don't clobber in-progress work.
- New capability idea → `IDEAS.md`, not code (charter N13).
- Run `make lint && make test` before claiming done.

## Scope discipline

- One concern per commit; minimal diff; no drive-by renames, reformats, or
  cleanups outside the task.
- No moving files or restructuring modules unless the task says so. Flag it.
- Ask before any change touching >5 files.
- Never hand-edit generated files (lockfiles, `target/`, generated
  completions). Edit the source; regenerate; commit generated output
  separately.
- Every function/type you add must have a caller/user in the same change — no
  orphaned code, no speculative abstractions. An interface (trait/base)
  requires 2+ concrete implementations existing now.
- Keep functions ≤40 LOC, ≤3 args (bundle into a struct beyond that, e.g.
  `Session`, `Banner`), no boolean flag params — split the function.

## Approvals required (state what and why, then wait)

- Adding any dependency (exact pins; must pass `cargo deny`; no postinstall).
- `git push`, tags, releases, publishing to crates.io.
- New config files, new languages, new top-level directories.
- Commits themselves are expected — follow the TDD protocol (tests committed
  first, phase-tagged messages).

## Rust (non-negotiables)

- `Result<T, E>` for all fallible paths. `unwrap()`/`expect()` only in tests,
  `build.rs`, or compile-time-proven invariants. Release builds abort on panic
  (`profile.release panic = "abort"`), so a surviving unwrap is a process kill.
- `#![forbid(unsafe_code)]` at every crate root.
- `thiserror` enums in the library (errors are API); `anyhow` + `.context()`
  only in `src/main.rs`.
- Newtypes for values with invariants; secrets: `Zeroize`, manual `Debug`
  printing `<redacted>`.
- Every externally-deserialized struct: `#[serde(deny_unknown_fields)]`.
- State machines are `enum` + exhaustive `match`, never `if let` chains.

### Rust — DO / NOT (the shapes reviews reject)

Exhaustive enum dispatch — never a wildcard arm on our own enums, never
stringly-typed:

```rust
// DO — a new variant breaks this at compile time, which is the feature
match verdict {
    Verdict::Allow => 0,
    Verdict::Deny { rule } => render_deny(&rule),
    Verdict::Escalate { attempt } => render_escalation(attempt),
}
// NOT: `_ => 0` (silently absorbs the next variant) · NOT: if x == "deny" { ... }
```

Newtype over bare primitives — arguments can't transpose:

```rust
// DO
fn attempts(session: &SessionId, rule: &RuleId) -> Result<u64, EventError> { /* ... */ }
// NOT: fn attempts(session: &str, rule: &str) -> ...   // call sites swap them silently
```

`?` propagation — the binary layer renders the error once:

```rust
// DO
let manifest = Manifest::parse(&fs::read_to_string(&path)?)?;
// NOT: let manifest = Manifest::parse(&fs::read_to_string(&path).unwrap()).unwrap();
```

Option combinators, not `is_some()` + `unwrap()`:

```rust
// DO
let root = repo_root()?.unwrap_or_else(|| PathBuf::from("."));
// NOT: if x.is_some() { let x = x.unwrap(); ... }
```

Iterator chains, not index loops:

```rust
// DO
let denied: Vec<&Violation> = result.violations.iter().filter(|v| v.blocking).collect();
// NOT: for i in 0..result.violations.len() { if result.violations[i].blocking { ... } }
```

Verb handlers: every verb module exposes `run(args) -> Result<ExitCode>`,
called from exactly one `dispatch()` arm. Bundle past a scalar or two into an
args struct rather than positional params — and never a multi-`bool`
signature. Keep `cli.rs` a dispatch table: arm-repacking logic belongs in a
`From` impl beside the type, not inline in the `match`.

## CLI (clap derive)

- `Cli` struct → `Cmd` subcommand enum, one exhaustive `dispatch(&Cmd, &Ctx)`
  match. Adding/removing a subcommand touches ALL of:
  `src/commands/<name>.rs` → `pub mod <name>;` in `commands/mod.rs` → variant on
  `Cmd` in `cli.rs` → arm in `dispatch()` → doc-comment help text on the
  variant.
- On removal, also sweep `error.rs` remediation strings and `README.md`
  (command table + runbook + troubleshooting rows), then regenerate completions.
- `chekov launch` is Claude Code-only: `AgentKind` is a closed enum with only
  the `Claude` variant. Do not assume chekov can run an arbitrary agent.

## Errors

- `ChekovError` (thiserror) in the library. `#[error("...")]` strings carry
  user-facing remediation that names chekov commands — **update these when you
  rename/remove a command** (grep the error strings).

## Tests

- Small tests, one behavior each, named for the behavior. Integration tests in
  `tests/`; unit tests live inline in `#[cfg(test)] mod tests` per module.
- Mock at the `HttpClient` boundary only — never chekov's own internals. No
  network and no real llama.cpp in any test.
- Committed-red `test(<module>): red` commits are expected to fail `make test`;
  do not "fix" a red commit's failure.

## Comments & docs

- Comment only what code can't say: why, footguns, intentional oddities. No
  restating code, no commented-out code.
- Don't create documentation files beyond those the task names unless asked.

## Decisions (how to ask the human — binding)

- A question only the human can answer is filed as a proposed decision record.
  Plain language first: the question, options, and recommendation must be
  readable with zero repo context. Ledger ids (F-numbers, §refs) go in tags and
  More Information only — never cite a bare number to the human.
- Every record offers the resolution paths honestly: rule now / research first /
  spike first (time-boxed, `spike/` branch, never merged).
- **No ruling exists until it is written.** Changing course means a superseding
  record.

## When stuck

- Two failures of the same approach = stop retrying variants. Consult the
  official docs for the tool/API, then either fix with new information or pivot
  strategy. Five failures = escalate with the log. Surface assumptions you
  couldn't verify instead of building on them.

## The rule that outranks the rest

chekov exists to keep a local inference stack honest and safe, and it enforces
its own standards on the agents that develop it. You will at some point be
blocked by a clippy gate, a denied unwrap, or a strictness setting that feels
excessive. That moment is the product working. **Never add a `#[allow(...)]` or
suppression to make a check pass, never loosen `clippy.toml`/`deny.toml`/
`-D warnings`, never edit a test to make failing work pass, never weaken
`forbid(unsafe_code)`/`deny_unknown_fields`.** If a gate is genuinely wrong,
stop and escalate with evidence. A blocked task is recoverable; a silently
weakened gate is the one failure this project exists to prevent.

## Adopted-from-pushkin manifest

Gates and the mechanical floor are declared in `pushkin.toml` (Rust-only
profile: fmt, clippy, test, deny). Path-gating is off-for-now with an explicit
re-arm path; see that file. The harness's own hooks are already wired into
`.claude/settings.json` (`pushkin-v1`).
