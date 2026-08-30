# Codebase mode, slice B2 — the exec tiers behind `--allow-exec`

Date: 2026-08-30. Builds on slice A (`2026-08-29-codebase-mode-slice-a-design.md`) and
slice B1 (`2026-08-29-codebase-mode-slice-b1-design.md`, PR #49). Slice C (`--judge`)
is separate; nothing here depends on it.

## 1. Why this slice, and what it deliberately leaves out

Tiers 1–5 grade a fill by resemblance: to the gold's text, to its identifiers, to the
repo's symbol set. None of them says whether the fill **compiles** or whether the code
that depended on the masked span still **works**. The umbrella spec's tiers 6 (compile
gate) and 7 (covering test) are the two that do, and it puts both behind one switch,
`--allow-exec`, because they run the user's repository's build. B2 adds that switch and
those two tiers, for Rust, over the task set A and B1 already produce.

Left out, and said in every report:

- **Slice C** — `--judge`.
- Other toolchains (`tsc --noEmit`, `python -m py_compile`): the exec module is written
  behind the same shape, but only Rust is implemented.
- A test *harness* around the fill (generating tests, mutation): tier 7 runs tests the
  repository already has, or reports that there are none.
- Any composite score across tiers.

## 2. Command surface and lifecycle

`capability bench` gains `--allow-exec` (bool, default `false`). It is the **single**
gate on every path that runs repository code, per the umbrella spec §8. Without it the
ladder stops at tier 5 exactly as today and the trailer reads
`tiers 6-7 skipped: --allow-exec not given`. With it:

- **Toolchain probe, once per run.** The worktree root must contain `Cargo.toml` and
  `cargo` must be on `PATH` (`cargo --version` succeeds). Either missing →
  tiers 6 and 7 are `Skipped("no Rust toolchain: <which>")` for every crossing, the
  header says so once, and the run proceeds — a missing toolchain is a capability of
  the machine, never a failing score.
- **Worktree lifetime.** Today `prepare` removes the worktree before the run. With
  `--allow-exec` the `Worktree` handle moves into the run and is removed when the run
  ends; the existing `Drop` still removes it on every early exit. Without the flag the
  lifecycle is unchanged.
- **Registry cache, then offline.** Before the loop, one `cargo fetch` in the worktree
  (with network). Every later cargo invocation carries `--offline`; a command that still
  needs the network is recorded `Skipped("needs network: <cargo's message>")`, never
  retried online.
- **Warm target.** `CARGO_TARGET_DIR` = `<eval>/.scratch/target-<head12>`, created for the
  run and removed with the worktree; nothing is written into the target repository.
  Checks after the first are incremental.
- **Estimate.** `--dry-run` adds `+ exec: cold check unmeasured, then ~6 s per crossing`
  and, once the run has done its first check, the live estimate prints
  `cold check <s>s, ~<s>s per crossing thereafter`.
- **Identity.** The task set is unchanged, so `corpus_id` is unchanged. The stamp gains
  `allow_exec: bool`, `cargo_version: Option<String>` (the `cargo --version` line) and
  `exec_target: "scratch"`; `compare` refuses across a differing `allow_exec` or
  `cargo_version` like any other environment field.

## 3. Apply → check → revert (tier 6)

One **crossing** is one prediction: an `in_file` or `function_body` task has one, a
`cross_file_first` task has two (its arms). For each crossing, in order:

1. **Splice.** Read the worktree's *original* F (test modules intact — elision affects
   only what the model sees). Replace the bytes of the span (`byte_range` on the task)
   with the prediction **trimmed to the gold's line count** — the same text tiers 1–4
   grade — and write F. No other file is touched.
2. **Check.** `cargo check --message-format=json --offline` in the worktree root, with
   `CARGO_TARGET_DIR` set, a 120 s wall-clock timeout, the child in its own process
   group and the group killed on expiry.
3. **Judge.** Parse the JSON stream. Tier 6 **passes** when no diagnostic of level
   `error` exists anywhere in the workspace — a fill can break a caller in another file,
   which is the point. Warnings are ignored. On failure the first error's `message` and
   its primary span `file:line` are stored on the row. Exit status alone is not the
   verdict; cargo exits non-zero on errors it also reports, and the diagnostics are the
   auditable record.
4. **Revert.** After tier 7 (§4) has run — or immediately, when tier 6 failed or was
   skipped — `git checkout -- <F>` in the worktree, then re-read F and compare its bytes
   to the original read in step 1. A revert that does not restore the original
   **aborts the run** with `ExecWorktreeDirty { path, file }` — the loop never continues
   on a worktree it cannot trust.

Non-verdict outcomes are `Skipped(reason)`, never pass and never fail:
`Skipped("check timed out after 120 s")`, `Skipped("cargo check failed to run: <io error>")`,
`Skipped("needs network: <message>")`. Each reason is counted and printed.

## 4. The covering test (tier 7)

Runs only for a crossing whose tier 6 passed; otherwise `Skipped("did not compile")`.

- **Symbol.** The enclosing function's name: for `function_body` the masked fn itself;
  for `in_file` and `cross_file_first` the function whose body contains the span (the
  masker's signature scan already locates it; a span outside any fn — a `const`, a
  `use` — has no enclosing fn and tier 7 is `Skipped("no enclosing function")`). For
  `cross_file_first` the keyed `name` is a second symbol.
- **Crate.** The nearest `Cargo.toml` at or above F in the worktree; its
  `[package] name`. A workspace root without `[package]` above F → `Skipped("no crate")`.
- **Candidates.** `#[test]` functions in that crate's files (the worktree's originals,
  `#[cfg(test)]` modules and `tests/*.rs` alike): an attribute line `#[test]` followed by
  `fn <t>` whose body (to the matching `}`) mentions any symbol as a whole word, outside
  literals and comments. Capped at **5**, in file order; the names are stored on the row.
  Zero → `Skipped("no covering test")`.
- **Run.** For each candidate, `cargo test -p <crate> --offline -- <t> --exact`, 300 s
  timeout, process-group kill, under the same `CARGO_TARGET_DIR`. Tier 7 **passes** when
  every candidate passes; the first failing test's name and cargo's failure text are
  stored on the row. A timeout is `Skipped("test timed out after 300 s")` — a hanging
  test under a bad fill is information, not a fail.
- The splice from §3 is still in place while the tests run (revert happens after tier 7);
  the revert-and-verify rule applies unchanged.

## 5. Scoring and storage

`CodebaseRow` gains, `#[serde(default)]`:

```rust
pub exec: Option<ExecRow>,

pub struct ExecRow {
    pub compile: Score,                 // Value(1.0) | Value(0.0) | Skipped(reason)
    pub compile_error: Option<String>,  // "<file>:<line>: <message>" on failure
    pub tests: Vec<String>,             // the candidates run, in order (≤ 5)
    pub test: Score,
    pub test_failure: Option<String>,   // "<test>: <cargo's text>" on failure
    pub check_secs: f64,
    pub test_secs: f64,
}
```

`exec` is `None` when `--allow-exec` was not given; rows written before B2 load with
`None`. `ladder::score_all` keeps returning tiers 6–7 as `Skipped(...)` — the exec
tiers are scored by the run loop, not the ladder, and are never recomputed on read (a
compile result cannot be re-derived from stored text). The report reads them from the
row, labelled `(measured at run time)` like tier 5.

## 6. Report

Every tier line gains two cells after `symbols`:

```
in_file                 exact 0.50  edit_sim 0.74  ident_f1 0.87  parse 0.92  symbols 0.95 (scored at run time)  compile 0.83 (n=12)  test 1.00 (n=3 of 12 had a covering test)   (n=12)
function_body           ident_f1 0.70  parse 0.83  symbols 0.85 (scored at run time)  compile 0.67 (n=6)  test 0.50 (n=2 of 6 had a covering test)   (n=6)
cross_file_first        …  compile 0.50 (n=6)  test n/a (0 of 6 had a covering test)   (n=6)
cross_file_first+extra  …  compile 0.83 (n=6)  test n/a (0 of 6 had a covering test)   (n=6)
context lift            …  compile +0.33  test n/a   (…)
             tiers 6-7: cold check 84 s, then 6 s median per crossing; 3 skipped (2 check timed out after 120 s, 1 needs network)
```

- `compile`'s `n` counts crossings with a verdict; skips are excluded from the mean and
  counted in the trailer by reason. `test`'s parenthetical always says how many crossings
  had a covering test; `n/a` when none did.
- Without `--allow-exec` the cells are absent and the trailer is
  `tiers 6-7 skipped: --allow-exec not given`. With the flag but no toolchain:
  `tiers 6-7 skipped: no Rust toolchain: <which>`.
- The header's `context:` sentence is unchanged; a new clause after it:
  `exec: cargo <version>, offline, scratch target` when exec ran.

## 7. Safety, stated

`--allow-exec` runs `cargo check` and `cargo test` on a copy of the repository. That
executes its `build.rs` scripts, proc-macros and tests — the same trust as building the
repository yourself. B2 bounds it: worktree only (the user's checkout is never written),
offline after one fetch, a scratch target dir, wall-clock timeouts with process-group
kill, and a run that stops the moment the worktree cannot be restored. It does not
sandbox the filesystem or the network beyond `--offline`; the README says so.

## 8. Errors and edge cases

- `ExecWorktreeDirty { path, file }` — the revert did not restore `file`; remediation:
  the worktree path to inspect and delete, and that the run's rows up to that crossing
  are intact and resumable.
- `--resume` skips crossings whose rows exist; a resumed run re-probes the toolchain and
  re-creates the target dir if it is gone (cold again — said in the estimate).
- A span whose enclosing function cannot be found → tier 7 `Skipped("no enclosing
  function")`; tier 6 still runs.
- A crate whose `cargo test` needs a feature or an env var to compile its tests is a
  `Skipped("did not compile")` at tier 7 with cargo's message — never a fail.
- Non-Rust repositories: `prepare` already refuses with `CodebaseNoTasks` (Rust only);
  the toolchain probe is only reached with Rust tasks in hand.

## 9. Tests

- Splice: the original with the span replaced and everything else byte-identical (test
  modules intact); a span at byte 0 and at EOF; the trimmed prediction is what is spliced.
- Diagnostics: a JSON stream with one warning and one error → fail with the error's
  `file:line: message`; warnings only → pass; malformed lines ignored; `error` in another
  file counts.
- Covering-test discovery: same crate only; `#[test]` adjacency (an attribute two lines
  above with a `#[ignore]` between); whole-word symbol match; a mention inside a string
  or comment does not count; the cap of 5 in file order; `tests/*.rs` found.
- Skip reasons: every string in §3–§4 produced by its condition; a skip never averages.
- Row round-trip; a pre-B2 row loads with `exec: None`; report cells with exact strings;
  the trailer's counts.
- Integration (a temp cargo workspace with one crate, two fns, one covered by a test): a
  correct fill passes 6 and 7 with `tests == ["covers_alpha"]`; a fill with a type error
  fails 6 with the message stored and 7 `Skipped("did not compile")`; a fill for the
  untested fn is `Skipped("no covering test")`; a `build.rs` that sleeps past the timeout
  yields `Skipped("check timed out after 120 s")` (with the timeout lowered through a
  test-only constant) and the file is verified restored.
- Live: `--allow-exec` on a clean clone of this repo and of pushkin with
  `ornith-1.5-35b-a3b`; both blocks and the timing trailer in the PR body.

## 10. Files

`src/core/bench/codebase/exec.rs` (new: probe, target dir, splice, check, diagnostics,
discovery, test run, revert-and-verify), `codebase/{run,mod,tree}.rs` (worktree lifetime,
the loop), `src/core/bench/store.rs` (`ExecRow`, cells, trailer), `src/core/bench/stamp.rs`
(the three fields), `src/commands/capability.rs` (`--allow-exec`, estimate, header
clause), `src/error.rs` (`ExecWorktreeDirty`), `README.md`, `CHANGELOG.md`, `IDEAS.md`,
a pointer from the umbrella spec's §8.
