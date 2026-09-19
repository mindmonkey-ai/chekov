# fixture-v1 compiled in — the manifest-driven grader

Date: 2026-09-18. Builds on capability-spec §9 (fixture mode) and the codebase
mode slices (`2026-08-29-codebase-mode-slice-a-design.md` and later). Approved
in chat on 2026-09-18 after the option "compiled in" was chosen over "read from
the repo path" and "gate to codebase mode".

## 1. Problem

`fixtures/fixture-v1/` exists (commits a8f9c74, f55ce68): a materialized ledger
crate, four graded devices, four held-out assertions under `hidden/`, and a
grading `manifest.toml`. Nothing in `src/` reads any of it. `--fixture <path>`
still means an external probe-set TOML, and the flag's help says there is no
compiled-in fixture. The release gate in §9 (three models, published spread)
cannot be measured until chekov can run the fixture.

## 2. Scope

In: embedding the fixture in the binary, the manifest contract, materializing
the visible tree, naming the four tasks, withholding and injecting the hidden
tests, the fixture identity in the run head, and the bare `--fixture` flag.

Out: the runtime flat-band detector (§9 Angle B) — report-side, its own
follow-up; the three-model campaign itself; any change to how codebase mode
samples a user's repo. The fixture stays documented as release-gated until the
campaign is published.

## 3. Approach

A manifest-driven variant of codebase mode. The compiled-in files are
materialized into a scratch git repository, and the existing checkout, mask,
leakage-filter and cargo-tier pipeline runs over it with tasks named by the
manifest instead of sampled by seed. The rejected alternative — a standalone
fixture pipeline calling the cargo runner directly — would duplicate the
masking, filtering and prompt assembly codebase mode already owns.

## 4. Components

### 4.1 Embedding — `build.rs` (new, crate root)

Walks `fixtures/fixture-v1/` at build time and writes
`$OUT_DIR/fixture_v1_files.rs` containing one table:

```rust
pub const FILES: &[(&str, &str)] = &[
    ("Cargo.toml", include_str!("/abs/fixtures/fixture-v1/Cargo.toml")),
    ("hidden/near_miss_api.rs", include_str!("...")),
    ...
];
```

Rules: paths are relative to the fixture root with `/` separators, sorted
bytewise; `target/`, `README.md` and `.gitignore` are skipped; `Cargo.lock` is
kept so `cargo --offline` is deterministic; every file is UTF-8 (`include_str!`
refuses otherwise, which is the right failure). `cargo:rerun-if-changed` is
emitted for the fixture directory. No build dependency is added; the hash is
computed at runtime (§4.2).

`build.rs` does not touch the crate's own CLI (AGENTS.md §12 override 3 still
holds — completions stay an install-time step).

### 4.2 Manifest contract — `src/core/bench/fixture/manifest.rs` (new)

A strict (`deny_unknown_fields`) `Manifest` for `manifest.toml`:

| key | type | meaning |
|-----|------|---------|
| `version` | u32 | must equal the supported manifest version (1) |
| `id` | String | `fixture-v1` |
| `content_hash` | String | sha256 hex over the content set (below) |
| `task_slots` | u32 | reserve count, reported only |
| `[[tasks]].id` | String | task id, unique |
| `[[tasks]].device` | String | human label |
| `[[tasks]].source` | String | file holding the masked body |
| `[[tasks]].hidden` | String | held-out test file, must be under `hidden/` |
| `[[tasks]].tier` | u32 | 6 or 7 |
| `[[tasks]].symbol` | String | **new** — the masked fn, `name` or `Owner::name` |

The content set is every embedded file except `manifest.toml` itself, hashed
as `path\0bytes\0` in sorted order with the crate's existing `hash::sha256_hex`.
`manifest.toml`'s `content_hash` is replaced with the real value in this
slice; `fixture::builtin()` recomputes it from `FILES` and refuses with
`FixtureInvalid { reason: "content hash <computed> does not match manifest
<declared>" }` when they differ. An edit to any fixture file without a manifest
bump is therefore a refused run, not a silently different corpus.

Validation beyond serde: every `source` and `hidden` must name an embedded
file; every `hidden` must start with `hidden/`; `symbol` must be non-empty;
task ids unique; tier ∈ {6, 7}.

### 4.3 Materialize — `src/core/bench/fixture/materialize.rs` (new)

`materialize(scratch_root) -> Result<Materialized, ChekovError>` writes every
`FILES` entry whose path does not start with `hidden/` under
`<scratch_root>/fixture-v1-<hash12>/`, then `git init`, `git add -A`,
`git commit` (identity `chekov <chekov@localhost>`) so `tree::assert_clean`
and `tree::head_sha` work unchanged. Re-running on an existing directory that
already has a HEAD reuses it. Returns the repo path and the hidden files as
`Vec<(String, &'static str)>` held in memory. `hidden/` is never on disk in
the visible tree — the property the manifest promises holds by construction.

### 4.4 Named tasks — `codebase::prepare_named` (new fn in `codebase/mod.rs`)

`prepare_named(repo, &NamedTasks, &PrepareInputs) -> Result<Prepared, _>`
runs the same steps as `prepare` (gate, worktree, walk, elide, index,
candidates, symbols, exec state) but replaces `sample::sample` with
`named::pick`: for each manifest task, the `function_body` candidate in
`source` whose `masker::enclosing_fn` equals the symbol's `name` part and,
when an `Owner::` is given, whose span lies inside an `impl` block whose header
names `Owner`. Zero or several matches refuse with
`CodebaseNoTasks { reason: "fixture task <id>: <n> candidates named <symbol> in
<source>" }`. Task ids are the manifest ids, `TaskTier::FunctionBody`, suite
`fixture`. The shared body of `prepare` is extracted into a private
`walk_and_index` so both entry points stay under 40 lines.

The leakage filter runs unchanged and its exclusion counts are printed and
recorded exactly as for a user repo (§9: "auditable by the same mechanism").

### 4.5 Grading — exec tier 7 with an injected covering test

`CodebaseTask` gains `covering_override: Option<HiddenTest>` (`{ file:
String, text: String }`, serde default, absent on every codebase-mode row).
When present, `exec::test_tier` writes `text` to `<worktree>/tests/<stem>.rs`,
runs `cargo test --test <stem> --offline` with the existing timeout and target
directory, records `tests = [<stem>]`, and removes the file afterwards —
including on the failure and timeout paths. When absent, behaviour is
unchanged. Tier 6 is unchanged; a tier-6-only manifest task still runs tier 7
when it compiles (the manifest's `tier` is the *scoring* tier, reported per
task, not a switch).

The fixture crate's `[lib] path = "src/lib.rs"` with package name `fixture-v1`
yields the `fixture_v1` crate the hidden files already `use`.

### 4.6 Identity — corpus id

`corpus_id` already folds a fixture digest into the head. The compiled-in
fixture contributes `fixture-v1:<content_hash12>` (replacing the `+fixture:`
suffix path for the external TOML, which is unchanged). `compare` already
refuses differing corpus ids, so a v2 fixture can never compare against a v1
run and no `Stamp` field is added (the stamp is `deny_unknown_fields` and
built from a struct literal in `capability.rs`; leaving it alone is the safer
choice).

### 4.7 CLI — `--fixture` with an optional value

`BenchOpts::fixture` becomes `Option<FixtureArg>` parsed from
`#[arg(long, num_args = 0..=1, default_missing_value = "builtin")]` where
`FixtureArg::Builtin | FixtureArg::External(PathBuf)`; the literal `builtin`
is the sentinel and is not a legal external path. Bare `--fixture` runs the
compiled-in fixture-v1 through `materialize` → `prepare_named` → the codebase
task lane, requiring `--allow-exec` like any exec-tier run and refusing
without it. `--fixture <path>` keeps today's external probe-set behaviour.
`--fixture` and `--codebase` remain mutually exclusive. The flag's help text
and the README "there is no compiled-in fixture yet" wording are replaced
with the gate statement: shipped, release-gated on the three-model campaign.

## 5. Data flow

```
build.rs ──▶ FILES table (compiled in)
                 │
   bare --fixture│
                 ▼
  fixture::builtin()  ── parse manifest, verify content_hash ──▶ Manifest
                 │
                 ▼
  materialize(scratch) ── visible files only, git init+commit ──▶ repo path
                 │                                           hidden in memory
                 ▼
  codebase::prepare_named(repo, tasks, inputs) ── mask the four named bodies,
                 │      leakage filter (counts printed), symbol ladder
                 ▼
  codebase task lane (unchanged) ── prompt, reply, tiers 1–5
                 │
                 ▼
  exec::tiers ── tier 6 cargo check ── tier 7 with injected hidden test
                 │
                 ▼
  RunWriter rows, suite "fixture", corpus id fixture-v1:<hash12>
```

## 6. Error handling

Every refusal names the fixture and the rule: hash mismatch, unknown manifest
key, missing source or hidden file, ambiguous or absent symbol, `--fixture`
without `--allow-exec`, git init failure in scratch. All are `ChekovError`
variants that already exist (`FixtureInvalid`, `CodebaseNoTasks`, `io`) —
no new variant. A hidden test that fails to write or remove is an `io` error
attributed to its path, never a silent skip.

## 7. Testing

All new tests are inline `#[cfg(test)]` modules (`tests/**` is
write-protected). Real-cargo assertions follow `tests/codebase_exec.rs`'s
pattern and are gated on `CHEKOV_TEST_EXEC=1`; everything else uses the
existing fake-cargo helpers.

- `build.rs` table: contains `manifest.toml`, `Cargo.toml`, `Cargo.lock`, all
  four `hidden/*.rs`, every `src/**/*.rs`; nothing under `target/`; no
  `README.md`; sorted.
- Manifest: parses the embedded manifest; the computed hash equals the
  declared one; an unknown key, a `hidden` outside `hidden/`, a duplicate id,
  a tier of 5 each refuse with the reason named.
- Materialize: the scratch tree has no `hidden/` directory, has a HEAD, and a
  second call reuses it; the returned hidden set has four entries.
- Named tasks: the four ids resolve to four `function_body` spans whose
  enclosing fn names match; a symbol with two matches refuses; no assembled
  prompt context contains any hidden file's text (asserted by substring over
  every task's context).
- Tier 7 injection (fake cargo): the hidden file exists inside the worktree
  during the `cargo test` invocation (the fake echoes the directory listing)
  and is gone after; the test name recorded is the stem; a timeout still
  removes the file.
- Corpus id: bare `--fixture` yields `fixture-v1:<12 hex>`; `compare`
  refuses two heads differing only in that id.
- CLI: `--fixture` alone parses to `Builtin`; `--fixture x.toml` to
  `External`; `--fixture` without `--allow-exec` refuses before any launch.

`make lint && make test` green is the exit criterion, plus one manual
`CHEKOV_TEST_EXEC=1` run of the real-cargo tests.

## 8. Files touched

New: `build.rs`, `src/core/bench/fixture/manifest.rs`,
`src/core/bench/fixture/materialize.rs`, `src/core/bench/codebase/named.rs`.
Modified: `src/core/bench/fixture.rs` (becomes `fixture/mod.rs`),
`src/core/bench/codebase/mod.rs`, `src/core/bench/codebase/exec.rs`,
`src/commands/capability.rs`, `fixtures/fixture-v1/manifest.toml` (`symbol`
keys, real `content_hash`), `README.md`, `CHANGELOG.md`, `Cargo.toml`
(`build = "build.rs"`). Approved as a >5-file change and a new root file on
2026-09-18.

## 9. Open decisions carried, not settled here

Which three models define the campaign spread now that `gpt-oss-120b` is
gone, and what spread counts as discriminating (IDEAS.md 2026-09-16, decisions
2 and 3). License exposure is settled by this design's choice to compile the
fixture in: it ships in every binary and is already public in the repo.

## 10. Amendments (2026-09-18, at planning)

Recorded when the implementation plan was written against the real seams
(`docs/superpowers/plans/2026-09-18-fixture-v1-compiled-in.md`):

- **Rows keep suite `codebase`.** The fixture's rows are codebase rows with
  exec halves, and the report, `compare` and `--resume` key on that suite.
  The corpus id `fixture-v1:<hash12>` is what distinguishes a fixture run.
  (§4.4 said suite `fixture`; that name stays with the external probe-set
  rows `run_fixture` writes.)
- **No `CodebaseTask` field.** `tests/codebase_exec.rs` is write-protected
  and builds `CodebaseTask`, `Env` and calls `exec_crossing(env, task, fill)`
  by literal. The hidden test therefore rides `Prepared.hidden`
  (`Vec<HiddenTest>` keyed by task id) and reaches tier 7 through a new
  `exec::Crossing { task, fill, hidden }` bundle and
  `exec::exec_crossing_with`; `exec_crossing` becomes a wrapper with its
  signature unchanged. (§4.5 said `CodebaseTask.covering_override`.)
- **Corpus override on `Prepared`.** `Prepared.corpus: Option<String>` is
  `Some("fixture-v1:<hash12>")` from `prepare_named` and `None` from
  `prepare`; `CodebaseHead` carries it and `head_corpus` prefers it. (§4.6.)
- **Git plumbing in `tree.rs`.** `materialize` calls a new
  `tree::init_and_commit(repo, message)` rather than spawning git itself,
  so the module's error contract has one spelling. (§4.3.)
- **The fixture sources leaked their answers.** Stray `// MASKED — TASK n`
  blocks sat above (not inside) two masked functions, and two doc comments
  named the correct API. The plan's first task scrubs them; the content hash
  is computed after that scrub.
- **The manifest is inside the content hash.** `content_hash` covers
  `manifest.toml` as well as the fixture's files, normalised by dropping every
  line whose trimmed form starts with `content_hash` — so the grading contract
  (`symbol`, `source`, `hidden`, `tier`, the task ids) binds the corpus id,
  while writing the computed value back into the manifest stays a fixed point.
  (§4.2 said the manifest was excluded outright.)
- **The fixture tree carries `Cargo.toml.in`, not `Cargo.toml`.** A nested
  `Cargo.toml` makes `fixtures/fixture-v1/` a package cargo skips, so the
  published `.crate` contained none of it and `build.rs` could not build inside
  it. `build.rs` strips a trailing `.in` when it builds the table key, and the
  materializer writes `Cargo.toml` as before. The materializer also keeps
  `manifest.toml` out of the graded tree: its comments narrate every device.
